//! The codctl command server (story #37).
//!
//! Every running Cod listens on one private Unix socket, at the path `codctl::default_socket_path` names (`$COD_SOCKET`, else `$XDG_RUNTIME_DIR/cod/codctl.sock`). The socket's folder is made owner-only when it is missing, and the server refuses a folder that is a link, belongs to someone else or is open to group or others. A live socket of another Cod at the path is left alone (the first Cod keeps it); a stale one is replaced.
//! The protocol is the one of the `codctl` library: one JSON request per line, one JSON reply per line, versioned.
//!
//! The socket is served on background threads. Every request is handed to the foreground through one channel, and each runs as its own foreground task, so a command that waits (a save prompt of `workspace close`) never holds up the others and the GPUI thread never waits for a client.
//! Workspace commands use the operations of story #24 (`MultiWorkspace::activate`, `cod_show_pending`, `Workspace::new_local`, `Workspace::open_paths`), so the window shows each change as the matching UI action does.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use codctl::Request;
use editor::Editor;
use git_ui::commit_view::CommitView;
use git_ui_core::file_diff_view::FileDiffView;
use gpui::{App, AsyncApp, Axis, Bounds, Entity, EntityId, Focusable as _, Pixels, WindowHandle};
use serde_json::{Value, json};
use workspace::{
    CodMember, MultiWorkspace, OpenMode, OpenOptions, OpenVisible, Pane, RemovalIntent,
    SaveIntent, SplitDirection, Workspace, WorkspaceId,
};

/// The rolled-up status of a workspace until workspace statuses exist (#25, #26).
const IDLE: &str = "idle";

/// Starts the server.
#[cfg(unix)]
pub fn init(cx: &mut App) {
    let path = codctl::default_socket_path();
    cx.spawn(async move |cx| serve(path, cx).await).detach();
}

/// The server uses Unix sockets, so other platforms never start it.
#[cfg(not(unix))]
pub fn init(_cx: &mut App) {}

/// A request from a client thread, with the way back to it.
#[cfg(unix)]
struct Job {
    request: Request,
    reply: std::sync::mpsc::Sender<Result<Value, String>>,
}

/// Opens the socket and runs every request on the foreground, each as its own task.
#[cfg(unix)]
async fn serve(path: PathBuf, cx: &mut AsyncApp) {
    use futures::StreamExt as _;

    let (jobs, mut incoming) = futures::channel::mpsc::unbounded::<Job>();
    // Every file system and socket call of the setup runs on the background executor.
    let listening = cx.background_executor().spawn({
        let path = path.clone();
        async move { socket::listen(&path, jobs) }
    });
    match listening.await {
        Ok(()) => log::info!("codctl: listening on {}", path.display()),
        Err(error) => {
            log::error!("codctl: not started: {error:#}");
            return;
        }
    }
    while let Some(job) = incoming.next().await {
        cx.spawn(async move |cx| {
            let reply = run(job.request, cx)
                .await
                .map_err(|error| format!("{error:#}"));
            job.reply.send(reply).ok();
        })
        .detach();
    }
}

/// Runs one request on the foreground.
async fn run(request: Request, cx: &mut AsyncApp) -> Result<Value> {
    match request {
        Request::WorkspaceList => Ok(cx.update(|cx| Value::Array(list(cx)))),
        Request::WorkspaceNew {
            name,
            folders,
            focus,
        } => new_workspace(name, folders, focus, cx).await,
        Request::WorkspaceFocus { workspace } => focus_workspace(&workspace, cx).await,
        Request::WorkspaceAddFolder { workspace, folders } => {
            add_folders(&workspace, folders, cx).await
        }
        Request::WorkspaceRename { workspace, name } => {
            rename_workspace(&workspace, name, cx).await
        }
        Request::WorkspaceClose { workspace, force } => {
            close_workspace(&workspace, force, cx).await
        }
        Request::PaneList { workspace } => {
            let (_, workspace) = cx.update(|cx| target_workspace(workspace.as_deref(), cx))?;
            Ok(cx.update(|cx| list_panes(&workspace, cx)))
        }
        Request::PaneSplit {
            workspace,
            pane,
            direction,
        } => split_pane(workspace.as_deref(), pane, &direction, cx).await,
        Request::PaneFocus { workspace, pane } => focus_pane(workspace.as_deref(), pane, cx).await,
        Request::PaneClose { workspace, pane } => close_pane(workspace.as_deref(), pane, cx).await,
        Request::PaneResize {
            workspace,
            pane,
            size,
        } => resize_pane(workspace.as_deref(), pane, size, cx).await,
        Request::Open {
            workspace,
            path,
            line,
            column,
            pane,
        } => open_file(workspace.as_deref(), path, line, column, pane, cx).await,
        Request::OpenPage { workspace, url } => open_page(workspace.as_deref(), url, cx).await,
        Request::Diff {
            workspace,
            old,
            new,
        } => open_diff(workspace.as_deref(), old, new, cx).await,
        Request::DiffRevisions {
            workspace,
            old,
            new,
        } => open_diff_revisions(workspace.as_deref(), old, new, cx).await,
        Request::PageGet { page } => get_page(page, cx).await,
        Request::PageReload { page } => reload_page(page, cx).await,
        Request::PageBack { page } => back_page(page, cx).await,
        Request::PageForward { page } => forward_page(page, cx).await,
        Request::PageClose { page } => close_page(page, cx).await,
    }
}

/// Every open workspace window.
fn windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

/// The window a new workspace goes to: the active workspace window, else the first one.
fn target_window(cx: &App) -> Option<WindowHandle<MultiWorkspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
        .or_else(|| windows(cx).into_iter().next())
}

/// One workspace of a window: loaded, or pending (saved, loads when shown).
struct Member {
    window: WindowHandle<MultiWorkspace>,
    id: Option<WorkspaceId>,
    live: Option<Entity<Workspace>>,
    name: Option<String>,
    folders: Vec<String>,
    active: bool,
}

/// The members of every window in window order, as the window lists them: loaded workspaces, and pending ones that have saved data.
fn members(cx: &App) -> Vec<Member> {
    let mut found = Vec::new();
    for window in windows(cx) {
        let Ok(multi_workspace) = window.read(cx) else {
            continue;
        };
        let active = multi_workspace.workspace().clone();
        for member in multi_workspace.cod_members(cx) {
            match member {
                CodMember::Live(workspace) => {
                    let read = workspace.read(cx);
                    found.push(Member {
                        window,
                        id: read.database_id(),
                        name: workspace::cod_workspace_name(read, cx),
                        folders: read
                            .visible_worktrees(cx)
                            .map(|worktree| {
                                worktree.read(cx).abs_path().to_string_lossy().into_owned()
                            })
                            .collect(),
                        active: workspace == active,
                        live: Some(workspace),
                    });
                }
                CodMember::Pending(id) => {
                    let Some(paths) = multi_workspace.cod_pending_paths(id) else {
                        continue;
                    };
                    found.push(Member {
                        window,
                        id: Some(id),
                        live: None,
                        name: workspace::cod_workspace_name_by_id(id, cx),
                        folders: paths
                            .iter()
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect(),
                        active: false,
                    });
                }
            }
        }
    }
    found
}

/// What `workspace list` reports for each workspace.
fn list(cx: &App) -> Vec<Value> {
    members(cx)
        .into_iter()
        .map(|member| {
            json!({
                "id": member.id.map(i64::from),
                "name": member.name,
                "status": IDLE,
                "folders": member.folders,
                "active": member.active,
                "loaded": member.live.is_some(),
            })
        })
        .collect()
}

/// Finds the workspace a command names: by its ID first, else by its exact name. Several workspaces with that name are an error.
fn resolve(reference: &str, cx: &App) -> Result<Member> {
    let wanted_id = reference.parse::<i64>().ok();
    let mut named = Vec::new();
    for member in members(cx) {
        if member.id.is_some_and(|id| Some(i64::from(id)) == wanted_id) {
            return Ok(member);
        }
        if member.id.is_some() && member.name.as_deref() == Some(reference) {
            named.push(member);
        }
    }
    match named.len() {
        0 => Err(anyhow!("unknown workspace {reference:?}")),
        1 => Ok(named.remove(0)),
        count => Err(anyhow!(
            "{count} workspaces are named {reference:?}: use the workspace ID"
        )),
    }
}

fn id_of(member: &Member) -> Result<WorkspaceId> {
    member.id.context("the workspace has no ID yet")
}

async fn new_workspace(
    name: Option<String>,
    folders: Vec<PathBuf>,
    focus: bool,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let window = cx
        .update(|cx| target_window(cx))
        .context("Cod has no workspace window")?;
    let app_state = cx.update(|cx| {
        window
            .read(cx)
            .map(|multi_workspace| multi_workspace.workspace().read(cx).app_state().clone())
    })?;
    let mode = if focus {
        OpenMode::Activate
    } else {
        OpenMode::Add
    };
    let opened = cx
        .update(|cx| Workspace::new_local(folders, app_state, Some(window), None, None, mode, cx))
        .await?;
    let id = cx
        .update(|cx| opened.workspace.read(cx).database_id())
        .context("the new workspace has no ID")?;
    if let Some(name) = name {
        opened.window.update(cx, |_, window, cx| {
            workspace::cod_set_workspace_name(&opened.workspace, Some(name), window, cx)
        })?;
    }
    Ok(json!({ "id": i64::from(id) }))
}

/// Names the workspace like "workspace: rename workspace" does; the window title follows when it is the one shown.
async fn rename_workspace(reference: &str, name: String, cx: &mut AsyncApp) -> Result<Value> {
    if name.is_empty() {
        bail!("the name must not be empty");
    }
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = i64::from(id_of(&member)?);
    let Some(workspace) = member.live else {
        bail!("workspace {id} is not loaded yet: show it first with codctl workspace focus {id}");
    };
    member.window.update(cx, |_, window, cx| {
        workspace::cod_set_workspace_name(&workspace, Some(name), window, cx)
    })?;
    Ok(json!({ "id": id }))
}

async fn focus_workspace(reference: &str, cx: &mut AsyncApp) -> Result<Value> {
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = id_of(&member)?;
    match member.live {
        Some(workspace) => member
            .window
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.activate(workspace, None, window, cx)
            })?,
        None => {
            member
                .window
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.cod_show_pending(id, window, cx)
                })?
                .await?
        }
    }
    Ok(json!({ "id": i64::from(id) }))
}

async fn add_folders(reference: &str, folders: Vec<PathBuf>, cx: &mut AsyncApp) -> Result<Value> {
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = i64::from(id_of(&member)?);
    let Some(workspace) = member.live else {
        bail!("workspace {id} is not loaded yet: show it first with codctl workspace focus {id}");
    };
    let opened = member
        .window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_paths(
                    folders,
                    OpenOptions {
                        visible: Some(OpenVisible::All),
                        ..Default::default()
                    },
                    None,
                    window,
                    cx,
                )
            })
        })?
        .await;
    for result in opened.into_iter().flatten() {
        result?;
    }
    Ok(json!({ "id": id }))
}

/// How many items of the workspace have unsaved changes.
fn unsaved_count(workspace: &Entity<Workspace>, cx: &App) -> usize {
    workspace
        .read(cx)
        .panes()
        .iter()
        .flat_map(|pane| pane.read(cx).items())
        .filter(|item| item.is_dirty(cx))
        .count()
}

/// Closes the workspace. Its unsaved files are closed first the way Zed closes unsaved tabs (`Pane::close_items` with `SaveIntent::Close`: the same prompt, Save, Don't Save or Cancel), in the workspace shown so the user sees what is asked; `force` drops them without asking (`SaveIntent::Skip`).
/// When a file is still unsaved after that, the user kept the workspace: `{"closed": false}`. Otherwise the workspace leaves the window and its session like the palette close (`MultiWorkspace::remove`, which also drops its name), and a closed only workspace leaves an empty one.
async fn close_workspace(reference: &str, force: bool, cx: &mut AsyncApp) -> Result<Value> {
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = i64::from(id_of(&member)?);
    let window = member.window;
    let Some(workspace) = member.live else {
        bail!("workspace {id} is not loaded yet: show it first with codctl workspace focus {id}");
    };
    if cx.update(|cx| unsaved_count(&workspace, cx)) > 0 {
        if !force {
            window.update(cx, |multi_workspace, window, cx| {
                if *multi_workspace.workspace() != workspace {
                    multi_workspace.activate(workspace.clone(), None, window, cx);
                }
            })?;
        }
        let intent = if force {
            SaveIntent::Skip
        } else {
            SaveIntent::Close
        };
        let panes = cx.update(|cx| workspace.read(cx).panes().to_vec());
        for pane in panes {
            let closing = window.update(cx, |_, window, cx| {
                pane.update(cx, |pane, cx| {
                    let unsaved: Vec<EntityId> = pane
                        .items()
                        .filter(|item| item.is_dirty(cx))
                        .map(|item| item.item_id())
                        .collect();
                    pane.close_items(window, cx, intent, &move |item| unsaved.contains(&item))
                })
            })?;
            closing.await?;
        }
        if cx.update(|cx| unsaved_count(&workspace, cx)) > 0 {
            return Ok(json!({ "closed": false }));
        }
    }
    let removed = window
        .update(cx, |multi_workspace, window, cx| {
            multi_workspace.remove([workspace], RemovalIntent::CloseProject, window, cx)
        })?
        .await?;
    Ok(json!({ "closed": removed }))
}

/// The workspace a pane, open or diff command acts on (#38): the named one, else the workspace shown in the active window. It must be loaded.
fn target_workspace(
    reference: Option<&str>,
    cx: &App,
) -> Result<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> {
    match reference {
        Some(reference) => {
            let member = resolve(reference, cx)?;
            let id = i64::from(id_of(&member)?);
            let workspace = member.live.with_context(|| {
                format!(
                    "workspace {id} is not loaded yet: show it first with codctl workspace focus {id}"
                )
            })?;
            Ok((member.window, workspace))
        }
        None => {
            let window = target_window(cx).context("Cod has no workspace window")?;
            let workspace = window.read(cx)?.workspace().clone();
            Ok((window, workspace))
        }
    }
}

/// The ID codctl shows for a pane: its entity ID, stable while the pane lives.
fn pane_id(pane: &Entity<Pane>) -> u64 {
    pane.entity_id().as_u64()
}

/// The center pane of the workspace with this ID.
fn find_pane(workspace: &Entity<Workspace>, id: u64, cx: &App) -> Result<Entity<Pane>> {
    workspace
        .read(cx)
        .panes()
        .iter()
        .find(|pane| pane_id(pane) == id)
        .cloned()
        .with_context(|| format!("unknown pane {id}: see codctl pane list"))
}

/// What `pane list` reports: every center pane with its ID, whether it is the active one, its
/// tab titles and its pixel bounds (null until the window has painted it once), so `codctl pane
/// resize`'s effect on the split can be checked precisely (#38 AC1).
fn list_panes(workspace: &Entity<Workspace>, cx: &App) -> Value {
    let read = workspace.read(cx);
    let active = read.active_pane().clone();
    Value::Array(
        read.panes()
            .iter()
            .map(|pane| {
                let tabs: Vec<String> = pane
                    .read(cx)
                    .items()
                    .map(|item| item.tab_content_text(0, cx).to_string())
                    .collect();
                let bounds = read.bounding_box_for_pane(pane).map(|bounds| {
                    json!({ "width": f32::from(bounds.size.width), "height": f32::from(bounds.size.height) })
                });
                json!({ "id": pane_id(pane), "active": *pane == active, "tabs": tabs, "bounds": bounds })
            })
            .collect(),
    )
}

/// Splits the pane like Zed's split does (`Workspace::split_pane`), making a new empty pane.
async fn split_pane(
    reference: Option<&str>,
    pane: u64,
    direction: &str,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let pane = cx.update(|cx| find_pane(&workspace, pane, cx))?;
    let direction = if direction == "down" {
        SplitDirection::Down
    } else {
        SplitDirection::Right
    };
    let new_pane = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            workspace.split_pane(pane, direction, window, cx)
        })
    })?;
    Ok(json!({ "id": pane_id(&new_pane) }))
}

/// Focuses the pane, which makes it the active pane of its workspace.
///
/// `window.focus` alone only queues the raw GPUI focus change: `Pane::focus_in` (and so
/// `Workspace::active_pane`) only runs once a frame draws with the window counted as active
/// ("activated" by the platform, which tracks real keyboard focus and stays false for a window
/// nothing has put real input focus on, as in a driven end-to-end run). Emitting the pane's own
/// `Focus` event tells the workspace directly, the same way `close_pane` below already emits
/// `Remove` for its own bookkeeping, so `pane list` reports the new active pane right away.
async fn focus_pane(reference: Option<&str>, pane: u64, cx: &mut AsyncApp) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let pane = cx.update(|cx| find_pane(&workspace, pane, cx))?;
    window.update(cx, |_, window, cx| {
        let handle = pane.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        pane.update(cx, |_, cx| cx.emit(workspace::pane::Event::Focus));
    })?;
    Ok(json!({ "id": pane_id(&pane) }))
}

/// Closes the tabs of the pane the way Zed's "pane: close all items" does (the same save prompt), then the pane itself. The only pane of a workspace stays.
/// When a tab is still open after that, the user kept it: `{"closed": false}`.
async fn close_pane(reference: Option<&str>, pane: u64, cx: &mut AsyncApp) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let id = pane;
    let pane = cx.update(|cx| find_pane(&workspace, id, cx))?;
    if cx.update(|cx| workspace.read(cx).panes().len()) < 2 {
        bail!("pane {id} is the only pane of the workspace");
    }
    let closing = window.update(cx, |_, window, cx| {
        pane.update(cx, |pane, cx| {
            pane.close_all_items(
                &workspace::pane::CloseAllItems {
                    save_intent: Some(SaveIntent::Close),
                    close_pinned: true,
                },
                window,
                cx,
            )
        })
    })?;
    closing.await?;
    let (still_listed, empty) = cx.update(|cx| {
        (
            workspace.read(cx).panes().contains(&pane),
            pane.read(cx).items().next().is_none(),
        )
    });
    if still_listed {
        if !empty {
            return Ok(json!({ "closed": false }));
        }
        // An empty pane leaves the split the way a pane whose last tab closed does.
        window.update(cx, |_, _, cx| {
            pane.update(cx, |_, cx| {
                cx.emit(workspace::pane::Event::Remove {
                    focus_on_pane: None,
                })
            })
        })?;
    }
    Ok(json!({ "closed": true }))
}

/// Sets the pane's share of its split to `size` percent (#38 AC1), the way dragging the split's
/// handle to that position would. `Workspace::resize_pane` only ever resizes the active pane, so
/// the pane is focused first (as `focus_pane` above does) and, when it was not the active pane
/// already, the previous active pane is re-focused afterward: only the size changes.
///
/// The focus and the resize each run in their own `window.update` call. GPUI only flushes the
/// effects of an update (here, the `pane::Event::Focus` that `focus_pane_now` emits) once its
/// outermost `window.update`/`App::update` call returns; doing the focus, the resize and the
/// re-focus inside one such call left `Workspace::active_pane` still pointing at the pane that
/// was active before this command ran when `resize_pane` read it, so the wrong pane (and the
/// wrong sign of the share) was resized. Splitting the calls lets the focus take effect, as
/// `focus_pane` above relies on too, before the resize reads the active pane.
async fn resize_pane(
    reference: Option<&str>,
    pane: u64,
    size: u8,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let pane = cx.update(|cx| find_pane(&workspace, pane, cx))?;
    let (axis, amount) = cx
        .update(|cx| resize_amount(&workspace, &pane, size, cx))
        .with_context(|| format!("pane {} has not been shown yet", pane_id(&pane)))?;
    let previous_active = cx.update(|cx| workspace.read(cx).active_pane().clone());

    fn focus_pane_now(target: &Entity<Pane>, window: &mut gpui::Window, cx: &mut App) {
        let handle = target.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        target.update(cx, |_, cx| cx.emit(workspace::pane::Event::Focus));
    }

    window.update(cx, |_, window, cx| focus_pane_now(&pane, window, cx))?;
    window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| workspace.resize_pane(axis, amount, window, cx))
    })?;
    if previous_active != pane {
        window.update(cx, |_, window, cx| focus_pane_now(&previous_active, window, cx))?;
    }
    Ok(json!({ "id": pane_id(&pane) }))
}

/// The axis and pixel delta that would set `pane`'s share of its split to `size` percent, from
/// the current bounds of `pane` and of the other panes of the workspace. Exact for a simple
/// split of two panes (what `codctl pane split` makes); with more panes in the same split the
/// total is still the sum of every other pane's bounds, which only approximates that group's
/// true span when panes of more than one split are mixed together.
fn resize_amount(
    workspace: &Entity<Workspace>,
    pane: &Entity<Pane>,
    size: u8,
    cx: &App,
) -> Option<(Axis, Pixels)> {
    let read = workspace.read(cx);
    let bounds = read.bounding_box_for_pane(pane)?;
    let others: Vec<Bounds<Pixels>> = read
        .panes()
        .iter()
        .filter(|other| *other != pane)
        .filter_map(|other| read.bounding_box_for_pane(other))
        .collect();
    let sibling = others.first()?;
    let close = |a: Pixels, b: Pixels| (a - b).abs() < gpui::px(1.);
    let axis = if close(sibling.origin.y, bounds.origin.y) && close(sibling.size.height, bounds.size.height) {
        Axis::Horizontal
    } else {
        Axis::Vertical
    };
    let span = |bounds: &Bounds<Pixels>| if axis == Axis::Horizontal { bounds.size.width } else { bounds.size.height };
    let current = span(&bounds);
    let total = others.iter().fold(current, |total, other| total + span(other));
    let target = total * (f32::from(size) / 100.);
    Some((axis, target - current))
}

/// Opens the file in the pane (else the active pane) like Zed's file finder does, then puts the cursor at the 1-based line and column like `zed <file>:<line>:<column>`.
async fn open_file(
    reference: Option<&str>,
    path: PathBuf,
    line: Option<u32>,
    column: Option<u32>,
    pane: Option<u64>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let pane = match pane {
        Some(id) => Some(cx.update(|cx| find_pane(&workspace, id, cx))?),
        None => None,
    };
    let project = cx.update(|cx| workspace.read(cx).project().clone());
    let (_, project_path) = cx
        .update(|cx| Workspace::project_path_for_path(project, &path, false, cx))
        .await?;
    let item = window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_path(
                    project_path,
                    pane.as_ref().map(|pane| pane.downgrade()),
                    true,
                    window,
                    cx,
                )
            })
        })?
        .await?;
    if let Some(line) = line
        && let Some(editor) = item.downcast::<Editor>()
    {
        window.update(cx, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                    return;
                };
                let snapshot = buffer.read(cx).snapshot();
                let point = snapshot.point_from_external_input(
                    line.saturating_sub(1),
                    column.unwrap_or(1).saturating_sub(1),
                );
                editor.go_to_singleton_buffer_point(point, window, cx);
            })
        })?;
    }
    let pane = cx.update(|cx| workspace.read(cx).active_pane().clone());
    Ok(json!({ "pane": pane_id(&pane) }))
}

/// Opens the URL as a page tab (#31) in the active pane and returns its page ID (#38 AC3).
async fn open_page(reference: Option<&str>, url: String, cx: &mut AsyncApp) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let id = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| web_view::page_control::open(workspace, url, window, cx))
    })??;
    Ok(json!({ "id": id }))
}

/// Opens Zed's diff view of two files (`FileDiffView`, as `zed --diff <old> <new>` does) in the active pane.
async fn open_diff(
    reference: Option<&str>,
    old: PathBuf,
    new: PathBuf,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let weak = workspace.downgrade();
    window
        .update(cx, |_, window, cx| {
            FileDiffView::open(old, new, None, weak, window, cx)
        })?
        .await?;
    Ok(json!({}))
}

/// Resolves `revision` (for example `HEAD`, `HEAD~1`, a branch or a commit SHA) to a commit SHA
/// in the repo at `repo_dir` by asking git directly: Cod's own git plumbing diffs a commit only
/// against its immediate parent, not two arbitrary revisions, so the two revisions `codctl diff`
/// is given are checked here before the parent's commit is shown.
fn resolve_revision(repo_dir: &Path, revision: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_dir)
        .args(["rev-parse", "--verify"])
        .arg(format!("{revision}^{{commit}}"))
        .output()
        .with_context(|| format!("cannot run git to resolve {revision:?}"))?;
    if !output.status.success() {
        bail!("unknown revision {revision:?}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Opens Zed's diff view of the commit `new` against its direct parent (`CommitView`, the same
/// view a commit's row in `git: open modified files`/the commit graph opens), checked first to
/// be `old` (#38 AC6). A repo has one diff view per call, matching the story's own default.
async fn open_diff_revisions(
    reference: Option<&str>,
    old: String,
    new: String,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let project = cx.update(|cx| workspace.read(cx).project().clone());
    let repo = cx
        .update(|cx| project.read(cx).active_repository(cx))
        .context("the workspace has no git repository")?;
    let repo_dir = cx.update(|cx| repo.read(cx).work_directory_abs_path.to_path_buf());
    let (old_for_check, new_for_check) = (old.clone(), new.clone());
    let (old_sha, parent_sha) = cx
        .background_executor()
        .spawn(async move {
            let old_sha = resolve_revision(&repo_dir, &old_for_check)?;
            let parent_sha = resolve_revision(&repo_dir, &format!("{new_for_check}~1"))?;
            Ok::<_, anyhow::Error>((old_sha, parent_sha))
        })
        .await?;
    if old_sha != parent_sha {
        bail!("{old} is not the direct parent of {new}: codctl diff only shows a commit against its parent");
    }
    let weak_repo = repo.downgrade();
    let weak_workspace = workspace.downgrade();
    window.update(cx, |_, window, cx| {
        CommitView::open(new, weak_repo, weak_workspace, None, None, window, cx);
    })?;
    Ok(json!({}))
}

/// Prints the page's current URL, title and state (#38 AC4).
async fn get_page(page: String, cx: &mut AsyncApp) -> Result<Value> {
    cx.update(|cx| web_view::page_control::get(&page, cx))
}

/// Reloads the page tab (#38 AC5).
async fn reload_page(page: String, cx: &mut AsyncApp) -> Result<Value> {
    cx.update(|cx| web_view::page_control::reload(&page, cx))?;
    Ok(json!({}))
}

/// Goes back in the page tab's history (#38 AC5).
async fn back_page(page: String, cx: &mut AsyncApp) -> Result<Value> {
    cx.update(|cx| web_view::page_control::back(&page, cx))?;
    Ok(json!({}))
}

/// Goes forward in the page tab's history (#38 AC5).
async fn forward_page(page: String, cx: &mut AsyncApp) -> Result<Value> {
    cx.update(|cx| web_view::page_control::forward(&page, cx))?;
    Ok(json!({}))
}

/// The window that shows this workspace live, if any (for `codctl page close`, which must act
/// on the window of the page's own workspace, not necessarily the active one).
fn window_of(workspace: &Entity<Workspace>, cx: &App) -> Option<WindowHandle<MultiWorkspace>> {
    members(cx)
        .into_iter()
        .find(|member| member.live.as_ref() == Some(workspace))
        .map(|member| member.window)
}

/// Closes the page tab (#38 AC5).
async fn close_page(page: String, cx: &mut AsyncApp) -> Result<Value> {
    let workspace = cx
        .update(|cx| web_view::page_control::workspace_of(&page, cx))
        .with_context(|| format!("unknown page {page}"))?
        .upgrade()
        .with_context(|| format!("the workspace of page {page} is gone"))?;
    let window = cx
        .update(|cx| window_of(&workspace, cx))
        .with_context(|| format!("the window of page {page} is gone"))?;
    window.update(cx, |_, window, cx| web_view::page_control::close(&page, window, cx))??;
    Ok(json!({}))
}

/// The path of the request that must be a file, if any.
fn files_of(request: &Request) -> Vec<&PathBuf> {
    match request {
        Request::Open { path, .. } => vec![path],
        Request::Diff { old, new, .. } => vec![old, new],
        _ => Vec::new(),
    }
}

/// Checks on the client thread, before anything reaches the window, that every folder of the request is a folder and every file a file.
pub fn check_folders(request: &Request) -> Result<(), String> {
    for file in files_of(request) {
        if !file.is_file() {
            return Err(format!("not a file: {}", file.display()));
        }
    }
    let folders = match request {
        Request::WorkspaceNew { folders, .. } | Request::WorkspaceAddFolder { folders, .. } => {
            folders
        }
        _ => return Ok(()),
    };
    for folder in folders {
        if !folder.is_dir() {
            return Err(format!("not a folder: {}", folder.display()));
        }
    }
    Ok(())
}

#[cfg(unix)]
mod socket {
    use std::{
        fs,
        io::{BufRead as _, BufReader, Write as _},
        os::unix::{
            fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
            net::{UnixListener, UnixStream},
        },
        path::Path,
        sync::mpsc,
    };

    use anyhow::{Context as _, Result, bail};
    use futures::channel::mpsc::UnboundedSender;

    use super::{Job, check_folders};

    /// What the server needs to know about the folder that holds the socket.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct FolderFacts {
        pub is_symlink: bool,
        pub is_dir: bool,
        pub owner: u32,
        pub mode: u32,
    }

    /// Why the folder may not hold the socket, or None when it is a real folder of this user that nobody else can enter.
    pub(super) fn folder_problem(facts: FolderFacts, user: u32) -> Option<String> {
        if facts.is_symlink {
            Some("is a symbolic link".to_owned())
        } else if !facts.is_dir {
            Some("is not a folder".to_owned())
        } else if facts.owner != user {
            Some(format!(
                "belongs to user {} instead of this user ({user})",
                facts.owner
            ))
        } else if facts.mode & 0o077 != 0 {
            Some(format!(
                "has mode {:o}: group and others must have no access (for example mode 700)",
                facts.mode & 0o777
            ))
        } else {
            None
        }
    }

    fn current_user() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Makes the socket folder (owner-only) when it is missing, then refuses it when it is a link, someone else's or open to others.
    fn prepare_folder(path: &Path) -> Result<()> {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        if fs::symlink_metadata(folder).is_err() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)
                .with_context(|| format!("cannot make the socket folder {}", folder.display()))?;
        }
        let metadata = fs::symlink_metadata(folder).with_context(|| {
            format!("the socket folder {} cannot be inspected", folder.display())
        })?;
        let facts = FolderFacts {
            is_symlink: metadata.file_type().is_symlink(),
            is_dir: metadata.file_type().is_dir(),
            owner: metadata.uid(),
            mode: metadata.mode(),
        };
        if let Some(problem) = folder_problem(facts, current_user()) {
            bail!("the socket folder {} {problem}", folder.display());
        }
        Ok(())
    }

    /// Removes a leftover socket of an earlier Cod at the path. Refuses a live socket, a socket of another user and any other kind of file.
    fn remove_stale_socket(path: &Path) -> Result<()> {
        let first = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
            }
        };
        let user = current_user();
        if !first.file_type().is_socket() || first.uid() != user {
            bail!("{} exists and is not a socket of this user", path.display());
        }
        match UnixStream::connect(path) {
            Ok(_) => bail!(
                "{} is the socket of another running Cod: it is left alone",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot tell whether {} is still in use", path.display())
                });
            }
        }
        let again = fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect {} again", path.display()))?;
        if !again.file_type().is_socket()
            || again.uid() != user
            || again.dev() != first.dev()
            || again.ino() != first.ino()
        {
            bail!("{} changed while it was checked", path.display());
        }
        fs::remove_file(path)
            .with_context(|| format!("cannot remove the old socket {}", path.display()))
    }

    /// Binds the socket with owner-only permissions and serves it on a background thread. Call it off the GPUI thread: it touches the file system.
    pub(super) fn listen(path: &Path, jobs: UnboundedSender<Job>) -> Result<()> {
        prepare_folder(path)?;
        remove_stale_socket(path)?;
        let listener = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            fs::remove_file(path).ok();
            return Err(error).with_context(|| format!("cannot make {} private", path.display()));
        }
        std::thread::Builder::new()
            .name("codctl-server".into())
            .spawn(move || accept(listener, jobs))
            .context("cannot start the codctl server thread")?;
        Ok(())
    }

    fn accept(listener: UnixListener, jobs: UnboundedSender<Job>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let jobs = jobs.clone();
                    let started = std::thread::Builder::new()
                        .name("codctl-client".into())
                        .spawn(move || {
                            if let Err(error) = serve_client(stream, jobs) {
                                log::warn!("codctl: client connection ended: {error:#}");
                            }
                        });
                    if let Err(error) = started {
                        log::error!("codctl: cannot start a client thread: {error}");
                    }
                }
                Err(error) => log::warn!("codctl: accept failed: {error}"),
            }
            if jobs.is_closed() {
                return;
            }
        }
    }

    fn serve_client(stream: UnixStream, jobs: UnboundedSender<Job>) -> Result<()> {
        let mut writer = stream.try_clone().context("cannot clone the client stream")?;
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line.context("cannot read a request")?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match codctl::parse_request_line(&line) {
                Ok(envelope) => {
                    let result = match check_folders(&envelope.request) {
                        Ok(()) => {
                            let (reply, answer) = mpsc::channel();
                            if jobs
                                .unbounded_send(Job {
                                    request: envelope.request,
                                    reply,
                                })
                                .is_err()
                            {
                                bail!("the codctl server stopped");
                            }
                            answer.recv().context("the codctl server stopped")?
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(value) => codctl::reply_ok(&envelope.id, value),
                        Err(error) => codctl::reply_error(&envelope.id, &error),
                    }
                }
                Err((id, error)) => codctl::reply_error(&id, &error),
            };
            writer
                .write_all(format!("{reply}\n").as_bytes())
                .and_then(|()| writer.flush())
                .context("cannot write a reply")?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{FolderFacts, folder_problem};

        const USER: u32 = 1000;
        const DIR: u32 = 0o040000;

        fn folder(mode: u32) -> FolderFacts {
            FolderFacts {
                is_symlink: false,
                is_dir: true,
                owner: USER,
                mode: DIR | mode,
            }
        }

        #[test]
        fn accepts_only_a_private_folder_of_this_user() {
            assert_eq!(folder_problem(folder(0o700), USER), None);
            assert!(folder_problem(folder(0o750), USER).is_some());
            assert!(folder_problem(folder(0o707), USER).is_some());
            assert!(folder_problem(folder(0o700), USER + 1).is_some());
            let link = FolderFacts {
                is_symlink: true,
                ..folder(0o700)
            };
            assert!(folder_problem(link, USER).is_some());
            let file = FolderFacts {
                is_dir: false,
                ..folder(0o700)
            };
            assert!(folder_problem(file, USER).is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `git` in `repo`, with no user or system config, an invented author (so the test
    /// never touches the real user's git identity), and panics on failure.
    fn git(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Cod Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Cod Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .status()
            .expect("cannot run git");
        assert!(status.success(), "git {args:?} failed in {repo:?}");
    }

    #[test]
    fn resolve_revision_reads_a_real_git_repo() {
        let repo = tempfile::tempdir().unwrap();
        let repo = repo.path();
        git(repo, &["init", "--quiet", "--initial-branch=main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(repo, &["add", "a.txt"]);
        git(repo, &["commit", "--quiet", "--no-gpg-sign", "-m", "first"]);
        let first = resolve_revision(repo, "HEAD").unwrap();
        assert_eq!(resolve_revision(repo, "main").unwrap(), first);

        std::fs::write(repo.join("a.txt"), "two\n").unwrap();
        git(repo, &["commit", "--quiet", "--no-gpg-sign", "-a", "-m", "second"]);
        let second = resolve_revision(repo, "HEAD").unwrap();

        assert_ne!(first, second);
        assert_eq!(resolve_revision(repo, "HEAD~1").unwrap(), first);
        assert!(resolve_revision(repo, "no-such-revision").is_err());
        assert!(resolve_revision(repo, "HEAD~2").is_err());
    }

    #[test]
    fn folders_must_exist() {
        let missing = Request::WorkspaceAddFolder {
            workspace: "1".to_owned(),
            folders: vec![PathBuf::from("/no/such/folder/for/codctl")],
        };
        assert!(check_folders(&missing).is_err());
        let present = Request::WorkspaceNew {
            name: None,
            folders: vec![std::env::temp_dir()],
            focus: false,
        };
        assert_eq!(check_folders(&present), Ok(()));
        assert_eq!(check_folders(&Request::WorkspaceList), Ok(()));
    }
}
