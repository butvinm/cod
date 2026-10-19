//! The driver of Cod's end-to-end tests (issue #22).
//!
//! Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket once its first workspace window is open. Without the variable nothing here runs.
//! The socket's folder must be a private folder of the same user (no access for group and others); the driver refuses to start otherwise, and also when a live socket or any other file is at the path.
//! The protocol is one JSON object per line, answered by one JSON line (see `tests/e2e/cod_e2e.py`):
//!
//! - `{"cmd":"keys","keys":"ctrl-p enter"}` presses each space-separated keystroke (Zed keystroke syntax). Reply `{"ok":true}`.
//! - `{"cmd":"type","text":"invoice"}` presses one key per character, with the character as the typed text. Reply `{"ok":true}`.
//! - `{"cmd":"state"}` replies `{"ok":true,"state":{...}}` with the fields of `read_state` (listed in `tests/e2e/README.md`).
//! - A failed request replies `{"ok":false,"error":"..."}`.
//!
//! Keys enter the window the way the Linux platform layer delivers them: `Window::dispatch_event` with `PlatformInput::KeyDown` and `KeyUp`, and when the key down is not handled and carries text, the window's text input receives that text (`Window::dispatch_keystroke` does both).
//! A keystroke with modifiers is wrapped in `PlatformInput::ModifiersChanged`: the modifiers go down before the key and up after it, so views that act when the modifiers are released (the ctrl-tab switcher) finish as they do for a user.
//! So the keymap, the focus and the text input behave as they do for a user. The driver never dispatches actions and never calls into views; `state` only reads what the window shows.
//! Keys go to the window the platform names active. Without a keyboard (the headless sway of the tests has none) the platform names none; then keys go to the newest open workspace window, the one the compositor focuses.
//! The socket is served on background threads. Every request is handed to one foreground task through a channel, so the GPUI thread never waits for a client.
//!
//! Some views keep what they show private. Read-only getters added for this driver (test hooks, no behavior change): `CommandPaletteDelegate::highlighted_command_name` and `listed_query` (command_palette), `ZedPromptRenderer::message`, `detail`, `answers` and `highlighted_answer` (ui_prompt), `ProjectPanel::listed_entries` (project_panel).

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

mod cod_chat_state;
mod cod_markdown_preview_state;
mod cod_overview_state;
mod cod_tabs_state;

use std::{cell::RefCell, rc::Rc, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail};
use command_palette::CommandPaletteDelegate;
use editor::{Editor, ToPoint as _};
use gpui::{
    AnyWindowHandle, App, AppContext as _, AsyncApp, Entity, Focusable as _, Keystroke, Modifiers,
    WeakEntity, Window, WindowHandle,
};
use open_path_prompt::OpenPathDelegate;
use picker::{Picker, PickerDelegate as _};
use project_panel::{ListedEntry, ProjectPanel};
use serde::Deserialize;
use serde_json::{Value, json};
use settings::Settings as _;
use ui_prompt::ZedPromptRenderer;
use workspace::{
    CodMember, ItemHandle, MultiWorkspace, Pane, Workspace, WorkspaceSettings, dock::PanelHandle as _,
};

mod cod_pages_state;

/// The environment variable that names the driver socket.
pub const SOCKET_ENV: &str = "COD_E2E_SOCKET";

/// How often the driver looks for the first workspace window before it opens the socket.
const WINDOW_POLL: Duration = Duration::from_millis(50);

/// The pause between two key presses of one request, so the app runs its event loop in between as it does for a person typing.
const KEY_PAUSE: Duration = Duration::from_millis(10);

/// Starts the driver when `COD_E2E_SOCKET` is set. Does nothing otherwise.
#[cfg(unix)]
pub fn init(cx: &mut App) {
    let Some(path) = std::env::var_os(SOCKET_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let memory = Rc::new(RefCell::new(Memory::default()));
    // The command palette keeps its picker private; the driver remembers each new palette picker to read its query and its rows.
    cx.observe_new({
        let memory = memory.clone();
        move |_: &mut Picker<CommandPaletteDelegate>, _, cx| {
            memory.borrow_mut().palette = Some(cx.weak_entity());
        }
    })
    .detach();
    // The window keeps its prompt private; the driver remembers each new prompt drawn in a window (Zed's ui_prompt) and the window it is in.
    cx.observe_new({
        let memory = memory.clone();
        move |_: &mut ZedPromptRenderer, window: Option<&mut Window>, cx| {
            if let Some(window) = window {
                memory.borrow_mut().prompt = Some((window.window_handle(), cx.weak_entity()));
            }
        }
    })
    .detach();
    // The workspace windows in the order they opened, so keys can go to the newest one when the platform names no active window.
    cx.observe_new({
        let memory = memory.clone();
        move |_: &mut MultiWorkspace, window: Option<&mut Window>, _| {
            if let Some(window) = window {
                memory.borrow_mut().windows.push(window.window_handle());
            }
        }
    })
    .detach();
    cx.spawn(async move |cx| serve(path, memory, cx).await).detach();
}

/// What the driver remembers between requests. It only ever holds observations, never anything the app reads.
#[derive(Default)]
struct Memory {
    /// The picker of the newest command palette.
    palette: Option<WeakEntity<Picker<CommandPaletteDelegate>>>,
    /// The newest prompt drawn in a window, with that window.
    prompt: Option<(AnyWindowHandle, WeakEntity<ZedPromptRenderer>)>,
    /// Every workspace window that opened, oldest first (closed ones included).
    windows: Vec<AnyWindowHandle>,
}

/// The driver uses Unix sockets, so other platforms never start it.
#[cfg(not(unix))]
pub fn init(_cx: &mut App) {}

/// One parsed request.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Key presses, in order.
    Keys(Vec<Keystroke>),
    /// Key presses for typed text, one per character.
    Type(Vec<Keystroke>),
    /// Read what the window shows.
    State,
    /// A left click at a window position, with the control key held when `ctrl` is set (#25, see
    /// `cod_tabs_state`; the control modifier was added for #32 AC3's Ctrl-click on a URL).
    Click { x: f32, y: f32, ctrl: bool },
    /// The test fixture that sets a session or page status (#25, see `cod_tabs_state`).
    SetEntry(cod_tabs_state::SetEntry),
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum WireRequest {
    Keys { keys: String },
    Type { text: String },
    State,
    Click {
        x: f32,
        y: f32,
        #[serde(default)]
        ctrl: bool,
    },
    SetEntry(cod_tabs_state::SetEntry),
}

/// Parses one request line.
pub fn parse_request(line: &str) -> Result<Request> {
    let request: WireRequest =
        serde_json::from_str(line).map_err(|error| anyhow!("invalid request: {error}"))?;
    Ok(match request {
        WireRequest::Keys { keys } => Request::Keys(parse_keys(&keys)?),
        WireRequest::Type { text } => Request::Type(typed_keystrokes(&text)?),
        WireRequest::State => Request::State,
        WireRequest::Click { x, y, ctrl } => {
            cod_tabs_state::check_click(x, y)?;
            Request::Click { x, y, ctrl }
        }
        WireRequest::SetEntry(entry) => {
            cod_tabs_state::check_set_entry(&entry)?;
            Request::SetEntry(entry)
        }
    })
}

/// Parses space-separated keystrokes in Zed's syntax, for example "ctrl-p" or "escape ctrl-shift-p".
pub fn parse_keys(keys: &str) -> Result<Vec<Keystroke>> {
    let keystrokes = keys
        .split_whitespace()
        .map(|source| {
            Keystroke::parse(source)
                .map(platform_keystroke)
                .map_err(|_| anyhow!("invalid keystroke {source:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if keystrokes.is_empty() {
        bail!("keys must name at least one keystroke");
    }
    Ok(keystrokes)
}

/// Fills in the typed text of a parsed keystroke the way the Linux platform layer does: a printable key types its character (upper case with shift), space types a space, and keys with ctrl, super or fn type nothing. Enter, tab, escape and other named keys type nothing.
pub fn platform_keystroke(mut keystroke: Keystroke) -> Keystroke {
    if keystroke.key_char.is_some() {
        return keystroke;
    }
    let modifiers = keystroke.modifiers;
    if modifiers.control || modifiers.platform || modifiers.function {
        return keystroke;
    }
    let mut chars = keystroke.key.chars();
    keystroke.key_char = match (chars.next(), chars.next()) {
        _ if keystroke.key == "space" => Some(" ".to_owned()),
        (Some(character), None) if !character.is_control() => Some(if modifiers.shift {
            character.to_uppercase().collect()
        } else {
            character.to_string()
        }),
        _ => None,
    };
    keystroke
}

/// The key presses that type the text: one per character, carrying the character as the typed text.
pub fn typed_keystrokes(text: &str) -> Result<Vec<Keystroke>> {
    if text.is_empty() {
        bail!("type needs a non-empty text");
    }
    text.chars().map(typed_keystroke).collect()
}

fn typed_keystroke(character: char) -> Result<Keystroke> {
    let (key, key_char, shift) = match character {
        ' ' => ("space".to_owned(), Some(" ".to_owned()), false),
        '\n' => ("enter".to_owned(), None, false),
        '\t' => ("tab".to_owned(), None, false),
        character if character.is_control() => {
            bail!("cannot type the control character {character:?}: use keys instead")
        }
        character if character.is_uppercase() => (
            character.to_lowercase().collect(),
            Some(character.to_string()),
            true,
        ),
        character => (character.to_string(), Some(character.to_string()), false),
    };
    Ok(Keystroke {
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        key,
        key_char,
    })
}

/// How the root of the center pane layout is split, named by the divider the user sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// A vertical divider: panes side by side, left to right (Zed's `Axis::Horizontal`, made by "pane: split right").
    Vertical,
    /// A horizontal divider: panes stacked, top to bottom (Zed's `Axis::Vertical`, made by "pane: split down").
    Horizontal,
}

impl Split {
    pub fn name(self) -> &'static str {
        match self {
            Split::Vertical => "vertical",
            Split::Horizontal => "horizontal",
        }
    }
}

/// The screen rectangle of one pane, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// Panes closer than this many pixels count as touching: the divider between two panes may overlap them a little.
const EDGE_TOLERANCE: f32 = 2.0;

/// Orders panes the way the layout tree holds them and names the split at its root, from the rectangles the panes cover on screen.
/// The root split is the line, crossing no pane, that cuts all panes into groups; each group is ordered the same way, recursively.
/// Returns the indices into `rects` in layout order. One pane, or none, has no split.
pub fn pane_layout(rects: &[Rect]) -> (Option<Split>, Vec<usize>) {
    order_group(rects, (0..rects.len()).collect())
}

fn order_group(rects: &[Rect], group: Vec<usize>) -> (Option<Split>, Vec<usize>) {
    if group.len() < 2 {
        return (None, group);
    }
    for split in [Split::Vertical, Split::Horizontal] {
        if let Some(parts) = cut(rects, &group, split) {
            let order = parts
                .into_iter()
                .flat_map(|part| order_group(rects, part).1)
                .collect();
            return (Some(split), order);
        }
    }
    // No straight cut exists (Zed's layouts always have one): fall back to reading order.
    let mut order = group;
    order.sort_by(|&a, &b| {
        rects[a]
            .top
            .total_cmp(&rects[b].top)
            .then(rects[a].left.total_cmp(&rects[b].left))
    });
    (None, order)
}

/// Cuts the group at every line across `split` that no pane crosses. None when there is no such line.
fn cut(rects: &[Rect], group: &[usize], split: Split) -> Option<Vec<Vec<usize>>> {
    let span = |index: usize| {
        let rect = rects[index];
        match split {
            Split::Vertical => (rect.left, rect.right),
            Split::Horizontal => (rect.top, rect.bottom),
        }
    };
    let mut sorted = group.to_vec();
    sorted.sort_by(|&a, &b| {
        let (a_start, a_end) = span(a);
        let (b_start, b_end) = span(b);
        a_start.total_cmp(&b_start).then(a_end.total_cmp(&b_end))
    });
    let mut parts = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut reach = f32::NEG_INFINITY;
    for index in sorted {
        let (start, end) = span(index);
        if !current.is_empty() && start >= reach - EDGE_TOLERANCE {
            parts.push(std::mem::take(&mut current));
        }
        current.push(index);
        reach = reach.max(end);
    }
    parts.push(current);
    (parts.len() > 1).then_some(parts)
}

/// The name of the command the palette highlights, or None while that row does not belong to the typed query.
/// `listed_query` is the query the palette's rows were matched against (None while it is still matching); `resolved_query` is the typed query after command aliases.
pub fn palette_selection(
    listed_query: Option<&str>,
    resolved_query: &str,
    highlighted: Option<&str>,
) -> Option<String> {
    if listed_query != Some(resolved_query) {
        return None;
    }
    highlighted.map(str::to_owned)
}

/// The name a project panel row has in the driver state: the root name, then the path below the root, joined by `/`.
pub fn panel_row_name(row: &ListedEntry) -> String {
    if row.path.is_empty() {
        row.root_name.clone()
    } else {
        format!("{}/{}", row.root_name, row.path)
    }
}

/// The reply line, without the final newline.
pub fn encode_reply(reply: &Result<Option<Value>>) -> String {
    let value = match reply {
        Ok(None) => json!({ "ok": true }),
        Ok(Some(state)) => json!({ "ok": true, "state": state }),
        Err(error) => json!({ "ok": false, "error": format!("{error:#}") }),
    };
    value.to_string()
}

/// A request from a client thread, with the way back to it.
#[cfg(unix)]
struct Job {
    request: Request,
    reply: std::sync::mpsc::Sender<Result<Option<Value>>>,
}

/// Waits for the first workspace window, opens the socket and runs every request on the foreground.
#[cfg(unix)]
async fn serve(path: std::path::PathBuf, memory: Rc<RefCell<Memory>>, cx: &mut AsyncApp) {
    use futures::StreamExt as _;

    while cx
        .update(|cx| workspace_window(cx, &memory.borrow()))
        .is_none()
    {
        cx.background_executor().timer(WINDOW_POLL).await;
    }
    let (jobs, mut incoming) = futures::channel::mpsc::unbounded::<Job>();
    // Every file system and socket call of the setup runs on the background executor; the foreground only awaits the result.
    let listening = cx.background_executor().spawn({
        let path = path.clone();
        async move { socket::listen(&path, jobs) }
    });
    match listening.await {
        Ok(()) => log::info!("e2e driver: listening on {}", path.display()),
        Err(error) => {
            log::error!("e2e driver: not started: {error:#}");
            return;
        }
    }
    while let Some(job) = incoming.next().await {
        let reply = run(job.request, &memory, cx).await;
        job.reply.send(reply).ok();
    }
}

#[cfg(unix)]
async fn run(
    request: Request,
    memory: &Rc<RefCell<Memory>>,
    cx: &mut AsyncApp,
) -> Result<Option<Value>> {
    match request {
        Request::Keys(keystrokes) | Request::Type(keystrokes) => {
            for (index, keystroke) in keystrokes.into_iter().enumerate() {
                if index > 0 {
                    cx.background_executor().timer(KEY_PAUSE).await;
                }
                press(keystroke, memory, cx)?;
            }
            Ok(None)
        }
        Request::Click { x, y, ctrl } => {
            cod_tabs_state::click(x, y, ctrl, memory, cx).await.map(|()| None)
        }
        Request::SetEntry(entry) => cod_tabs_state::set_entry(entry, memory, cx).map(|()| None),
        Request::State => {
            // Counted outside update_window: while a window is being updated, App::windows leaves it out.
            let window_count = cx.update(|cx| cx.windows().len());
            let window = cx
                .update(|cx| workspace_window(cx, &memory.borrow()))
                .context("Cod has no workspace window")?;
            cx.update_window(window.into(), |root, window, cx| {
                let multi_workspace = root
                    .downcast::<MultiWorkspace>()
                    .map_err(|_| anyhow!("the window does not show a workspace"))?;
                read_state(&multi_workspace, window, window_count, &memory.borrow(), cx)
            })
            .context("the workspace window closed")?
            .map(Some)
        }
    }
}

/// The window the driver works on: the active window when it shows a workspace, otherwise the newest open workspace window, otherwise the first one.
fn workspace_window(cx: &App, memory: &Memory) -> Option<WindowHandle<MultiWorkspace>> {
    if let Some(active) = cx
        .active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
    {
        return Some(active);
    }
    let open: Vec<AnyWindowHandle> = cx
        .windows()
        .into_iter()
        .filter(|window| window.downcast::<MultiWorkspace>().is_some())
        .collect();
    memory
        .windows
        .iter()
        .rev()
        .find(|window| open.contains(*window))
        .or_else(|| open.first())
        .and_then(|window| window.downcast::<MultiWorkspace>())
}

/// Presses one key: key down, then key up, as the platform layer delivers them.
#[cfg(unix)]
fn press(keystroke: Keystroke, memory: &Rc<RefCell<Memory>>, cx: &mut AsyncApp) -> Result<()> {
    use gpui::{Capslock, KeyDownEvent, KeyUpEvent, ModifiersChangedEvent, PlatformInput};

    let window = cx
        .update(|cx| workspace_window(cx, &memory.borrow()))
        .context("Cod has no workspace window")?;
    cx.update_window(window.into(), |_, window, cx| {
        let modified = keystroke.modifiers.modified();
        if modified {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: keystroke.modifiers,
                    capslock: Capslock::default(),
                }),
                cx,
            );
        }
        // The Linux platform layer gives unhandled text keys without modifiers other than shift to the text input.
        let types_text = keystroke.key_char.is_some()
            && keystroke.modifiers.is_subset_of(&Modifiers::shift());
        if types_text {
            window.dispatch_keystroke(keystroke.clone(), cx);
        } else {
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        }
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        if modified {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: Modifiers::none(),
                    capslock: Capslock::default(),
                }),
                cx,
            );
        }
    })
    .context("the workspace window closed")
}

/// The name of a workspace as the user gave it, or None while it has none (story #24, `workspace::cod_workspace_name`).
fn workspace_name(workspace: &Entity<Workspace>, cx: &App) -> Option<String> {
    workspace::cod_workspace_name(workspace.read(cx), cx)
}

/// The text field of "workspace: rename workspace" while it is shown in the window: `{"text": <its current text>}`, else None.
fn rename_input(multi_workspace: &MultiWorkspace, _window: &Window, cx: &App) -> Option<Value> {
    multi_workspace
        .workspace()
        .read(cx)
        .active_modal::<workspace::RenameWorkspaceModal>(cx)
        .map(|modal| json!({ "text": modal.read(cx).text(cx) }))
}

/// Whether the pane is part of the frame the window drew last, read from that frame: the pane's own element (the one that tracks its focus) is in the frame's dispatch tree, with the listener of a pane-only action.
/// A workspace the window does not draw has none of its panes in the frame.
fn is_drawn(pane: &Entity<Pane>, window: &Window, cx: &App) -> bool {
    window.is_action_available_in(
        &workspace::pane::JoinIntoNext,
        &pane.read(cx).focus_handle(cx),
    )
}

/// The project panel of the workspace as the panel lists it: whether it is open in its dock, whether it has the focus, its root rows, its expanded folders below the roots, its selected row and all its rows. Null when the workspace has no project panel.
fn project_panel_state(workspace: &Workspace, window: &Window, cx: &App) -> Value {
    let Some(panel) = workspace.panel::<ProjectPanel>(cx) else {
        return Value::Null;
    };
    let open = workspace.all_docks().iter().any(|dock| {
        let dock = dock.read(cx);
        dock.is_open()
            && dock
                .visible_panel()
                .is_some_and(|visible| visible.panel_id() == panel.entity_id())
    });
    let focused = panel.read(cx).focus_handle(cx).contains_focused(window, cx);
    let rows = panel.read(cx).listed_entries(cx);
    let roots: Vec<String> = rows
        .iter()
        .filter(|row| row.path.is_empty())
        .map(|row| row.root_name.clone())
        .collect();
    let expanded: Vec<String> = rows
        .iter()
        .filter(|row| row.is_dir && row.is_expanded && !row.path.is_empty())
        .map(panel_row_name)
        .collect();
    let selected = rows.iter().find(|row| row.is_selected).map(panel_row_name);
    let names: Vec<String> = rows.iter().map(panel_row_name).collect();
    json!({
        "open": open,
        "focused": focused,
        "roots": roots,
        "expanded": expanded,
        "selected": selected,
        "rows": names,
    })
}

/// The prompt drawn in the window: its message and detail (Markdown source, as Zed passes them), its answers and the highlighted answer. Null without a prompt.
fn prompt_state(window: &Window, memory: &Memory, cx: &App) -> Value {
    if !window.has_active_prompt() {
        return Value::Null;
    }
    let renderer = memory
        .prompt
        .as_ref()
        .filter(|(prompt_window, _)| *prompt_window == window.window_handle())
        .and_then(|(_, renderer)| renderer.upgrade());
    match renderer {
        Some(renderer) => {
            let renderer = renderer.read(cx);
            json!({
                "message": renderer.message(cx),
                "detail": renderer.detail(cx),
                "answers": renderer.answers(),
                "highlighted": renderer.highlighted_answer(),
            })
        }
        // A prompt of another kind: shown, but its text cannot be read.
        None => json!({ "message": null, "detail": null, "answers": null, "highlighted": null }),
    }
}

fn worktrees_of(workspace: &Workspace, cx: &App) -> Vec<String> {
    workspace
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect()
}

fn tab_title(item: &dyn ItemHandle, cx: &App) -> String {
    item.tab_content_text(0, cx).to_string()
}

/// What the window shows. The fields are described in `tests/e2e/README.md`; the first four are the ones of story #22.
fn read_state(
    multi_workspace: &Entity<MultiWorkspace>,
    window: &Window,
    window_count: usize,
    memory: &Memory,
    cx: &App,
) -> Result<Value> {
    let multi_workspace = multi_workspace.read(cx);
    let active = multi_workspace.workspace().clone();
    // The members in window order: loaded workspaces, and pending ones (lazy restore) that have saved data.
    let held: Vec<CodMember> = multi_workspace
        .cod_members(cx)
        .into_iter()
        .filter(|member| match member {
            CodMember::Live(_) => true,
            CodMember::Pending(id) => multi_workspace.cod_pending_paths(*id).is_some(),
        })
        .collect();
    let active_workspace = held
        .iter()
        .position(|member| matches!(member, CodMember::Live(workspace) if *workspace == active));
    // What the last drawn frame holds of each workspace: whether any of its center panes is in it, and the tabs of those panes.
    let mut drawn_tabs: Vec<String> = Vec::new();
    let workspaces: Vec<Value> = held
        .iter()
        .map(|member| {
            let workspace = match member {
                CodMember::Live(workspace) => workspace,
                CodMember::Pending(id) => {
                    // A pending member is not loaded: its name and folders come from its saved data.
                    let worktrees: Vec<String> = multi_workspace
                        .cod_pending_paths(*id)
                        .unwrap_or_default()
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect();
                    return json!({
                        "name": workspace::cod_workspace_name_by_id(*id, cx),
                        "worktrees": worktrees,
                        "drawn": false,
                        "remote": multi_workspace.cod_pending_is_remote(*id),
                        "status": cod_tabs_state::member_status(member, cx),
                    });
                }
            };
            let mut drawn = false;
            for pane in workspace.read(cx).panes() {
                if is_drawn(pane, window, cx) {
                    drawn = true;
                    drawn_tabs.extend(pane.read(cx).items().map(|item| tab_title(&**item, cx)));
                }
            }
            json!({
                "name": workspace_name(workspace, cx),
                "worktrees": worktrees_of(workspace.read(cx), cx),
                "drawn": drawn,
                "remote": workspace.read(cx).project().read(cx).is_via_remote_server(),
                "status": cod_tabs_state::member_status(member, cx),
            })
        })
        .collect();
    let workspace = active.read(cx);
    let active_tab = workspace
        .active_item(cx)
        .map(|item| tab_title(&*item, cx));
    let tabs: Vec<String> = workspace
        .active_pane()
        .read(cx)
        .items()
        .map(|item| tab_title(&**item, cx))
        .collect();
    let worktrees = worktrees_of(workspace, cx);

    // The center panes in layout order, read from where they are on screen: the pane tree itself is private to the workspace.
    let panes = workspace.panes();
    let rects: Option<Vec<Rect>> = panes
        .iter()
        .map(|pane| {
            workspace.bounding_box_for_pane(pane).map(|bounds| Rect {
                left: f32::from(bounds.origin.x),
                top: f32::from(bounds.origin.y),
                right: f32::from(bounds.origin.x + bounds.size.width),
                bottom: f32::from(bounds.origin.y + bounds.size.height),
            })
        })
        .collect();
    let (split, order): (Value, Vec<usize>) = match rects {
        _ if panes.len() < 2 => (Value::Null, (0..panes.len()).collect()),
        Some(rects) => {
            let (split, order) = pane_layout(&rects);
            (json!(split.map(Split::name)), order)
        }
        // Several panes that are not laid out yet: the split cannot be told.
        None => (json!("unknown"), (0..panes.len()).collect()),
    };
    let ordered_panes: Vec<&Entity<Pane>> =
        order.iter().map(|&index| &panes[index]).collect();
    let panes_json: Vec<Value> = ordered_panes
        .iter()
        .map(|pane| {
            let pane = pane.read(cx);
            json!({
                "tabs": pane.items().map(|item| tab_title(&**item, cx)).collect::<Vec<_>>(),
                "active_tab": pane.active_item().map(|item| tab_title(&*item, cx)),
            })
        })
        .collect();
    let dirty_tabs: Vec<String> = ordered_panes
        .iter()
        .flat_map(|pane| pane.read(cx).items())
        .filter(|item| item.is_dirty(cx))
        .map(|item| tab_title(&**item, cx))
        .collect();
    // The text each unsaved editor tab holds now, so a test can tell that an edit survived exactly.
    let unsaved_texts: Vec<Value> = ordered_panes
        .iter()
        .flat_map(|pane| pane.read(cx).items())
        .filter(|item| item.is_dirty(cx))
        .map(|item| {
            json!({
                "tab": tab_title(&**item, cx),
                "text": (**item).act_as::<Editor>(cx).map(|editor| editor.read(cx).text(cx)),
            })
        })
        .collect();

    // The cursor of the active editor as the status bar shows it: 1-based line, 1-based column (in bytes).
    let cursor = workspace.active_item_as::<Editor>(cx).map(|editor| {
        let editor = editor.read(cx);
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let point = editor.selections.newest_anchor().head().to_point(&snapshot);
        json!({ "row": point.row + 1, "column": point.column + 1 })
    });
    // Where the active pane's cursor was last drawn, in window pixels (#32 AC3): the end-to-end
    // driver's Ctrl-click target for a URL in a code comment. The test moves the cursor there with
    // the keyboard first (as "editor: open url" already does), then reads this field instead of
    // computing text layout itself; `Pane::pixel_position_of_cursor` already exists for pane-split
    // navigation (`pane_group.rs`), so this adds no new editor code.
    let cursor_pixel_position = workspace
        .active_pane()
        .read(cx)
        .pixel_position_of_cursor(cx)
        .map(|point| json!({ "x": f32::from(point.x), "y": f32::from(point.y) }));

    // The command palette: its query, how many rows it lists, which row is highlighted, the query those rows belong to, and the command on the highlighted row.
    let palette_open = workspace
        .active_modal::<command_palette::CommandPalette>(cx)
        .is_some();
    let palette_picker = memory
        .palette
        .as_ref()
        .and_then(|picker| picker.upgrade())
        .filter(|_| palette_open);
    let (palette, selection) = match palette_picker {
        Some(picker) => {
            let picker = picker.read(cx);
            let delegate = &picker.delegate;
            let query = picker.query(cx);
            // The palette matches an aliased query as its alias (CommandPaletteDelegate::update_matches).
            let resolved_query: String = WorkspaceSettings::get_global(cx)
                .command_aliases
                .get(&query)
                .map(|alias| AsRef::<str>::as_ref(alias).to_owned())
                .unwrap_or_else(|| query.clone());
            let listed_query = delegate.listed_query();
            let highlighted = delegate
                .highlighted_command_name()
                .map(|name| name.to_string());
            let selection =
                palette_selection(listed_query, &resolved_query, highlighted.as_deref());
            (
                json!({
                    "query": query,
                    "match_count": delegate.match_count(),
                    "selected_index": delegate.selected_index(),
                    "listed_query": listed_query,
                    "highlighted": highlighted,
                }),
                json!(selection),
            )
        }
        None => (Value::Null, Value::Null),
    };

    // Zed's own path picker (with use_system_path_prompts off): its query and how many entries match it.
    let path_prompt = workspace
        .active_modal::<Picker<OpenPathDelegate>>(cx)
        .map(|picker| {
            let picker = picker.read(cx);
            json!({
                "query": picker.query(cx),
                "match_count": picker.delegate.match_count(),
            })
        });

    // A prompt drawn in the window (Zed's ui_prompt, always used on Linux).
    let prompt = prompt_state(window, memory, cx);
    let project_panel = project_panel_state(workspace, window, cx);
    let rename = rename_input(multi_workspace, window, cx);
    // Whether the file finder is in the frame the window drew last and holds the focus there, so
    // typed keys reach its query: an action only the finder's element handles is available from
    // its focus handle in that frame.
    let file_finder_ready = workspace
        .active_modal::<file_finder::FileFinder>(cx)
        .is_some_and(|finder| {
            let focus_handle = finder.read(cx).focus_handle(cx);
            focus_handle.contains_focused(window, cx)
                && window.is_action_available_in(&file_finder::OpenWithoutDismiss, &focus_handle)
        });
    // Whether the command palette holds the focus (null while no palette is open): a palette
    // that lost the focus keeps an empty query whatever is typed.
    let palette_focused = workspace
        .active_modal::<command_palette::CommandPalette>(cx)
        .map(|palette| palette.read(cx).focus_handle(cx).contains_focused(window, cx));
    // Whether the platform reports Cod's window as the active (keyboard) window.
    let window_active = window.is_window_active();
    let focus = focus_state(workspace, window, cx);
    let modal = if workspace
        .active_modal::<file_finder::FileFinder>(cx)
        .is_some()
    {
        Some("file_finder")
    } else if workspace
        .active_modal::<command_palette::CommandPalette>(cx)
        .is_some()
    {
        Some("command_palette")
    } else if workspace.modal_layer().read(cx).has_active_modal() {
        Some("other")
    } else {
        None
    };
    let mut state = json!({
        "active_tab": active_tab,
        "tabs": tabs,
        "worktrees": worktrees,
        "modal": modal,
        "window_count": window_count,
        "workspace_count": held.len(),
        "active_workspace": active_workspace,
        "workspaces": workspaces,
        "panes": panes_json,
        "split": split,
        "cursor": cursor,
        "cursor_pixel_position": cursor_pixel_position,
        "dirty_tabs": dirty_tabs,
        "unsaved_texts": unsaved_texts,
        "drawn_tabs": drawn_tabs,
        "project_panel": project_panel,
        "rename_input": rename,
        "palette": palette,
        "palette_selection": selection,
        "path_prompt": path_prompt,
        "file_finder_ready": file_finder_ready,
        "palette_focused": palette_focused,
        "window_active": window_active,
        "focus": focus,
        "prompt": prompt,
        "title_bar": cod_tabs_state::title_bar_state(&active, cx),
    });
    // Set outside `json!` to keep the macro's recursion short.
    state["overview"] = cod_overview_state::overview_state(workspace, window, cx);
    state["pages"] = cod_pages_state::state(multi_workspace, window, cx);
    state["chat"] = cod_chat_state::chat_state(workspace, window, cx);
    state["markdown_preview"] = cod_markdown_preview_state::state(workspace, cx);
    state["agent_panel_ready"] = json!(cod_chat_state::agent_panel_ready(window, cx));
    state["agent_panel_focused"] = json!(cod_chat_state::agent_panel_focused(workspace, window, cx));
    Ok(state)
}

/// What holds the window's focus, read only: null when nothing is focused, else `kind` is
/// "item" (an item of the active workspace is focused itself, so an editor takes typed text),
/// "in_item" (an element inside such an item), "pane" (a pane of the active workspace),
/// "modal" (anything while a modal is open) or "other"; `tab` names the item for the first two.
fn focus_state(workspace: &Workspace, window: &Window, cx: &App) -> Value {
    if window.focused(cx).is_none() {
        return Value::Null;
    }
    let mut inside: Option<String> = None;
    for pane in workspace.panes() {
        let pane = pane.read(cx);
        for item in pane.items() {
            let handle = item.item_focus_handle(cx);
            if handle.is_focused(window) {
                return json!({ "kind": "item", "tab": tab_title(&**item, cx) });
            }
            if inside.is_none() && handle.contains_focused(window, cx) {
                inside = Some(tab_title(&**item, cx));
            }
        }
    }
    if let Some(tab) = inside {
        return json!({ "kind": "in_item", "tab": tab });
    }
    if workspace
        .panes()
        .iter()
        .any(|pane| pane.read(cx).focus_handle(cx).is_focused(window))
    {
        return json!({ "kind": "pane", "tab": Value::Null });
    }
    if workspace.modal_layer().read(cx).has_active_modal() {
        return json!({ "kind": "modal", "tab": Value::Null });
    }
    json!({ "kind": "other", "tab": Value::Null })
}

/// The socket side: runs on background threads only.
#[cfg(unix)]
mod socket {
    use std::{
        fs,
        io::{BufRead as _, BufReader, Write as _},
        os::unix::{
            fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
            net::{UnixListener, UnixStream},
        },
        path::Path,
        sync::mpsc,
    };

    use anyhow::{Context as _, Result, bail};
    use futures::channel::mpsc::UnboundedSender;

    use super::{Job, encode_reply, parse_request};

    /// What the driver needs to know about the folder that holds the socket.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct FolderFacts {
        /// The path names a symbolic link (not followed).
        pub is_symlink: bool,
        /// The path names a real folder.
        pub is_dir: bool,
        /// The owner's user id.
        pub owner: u32,
        /// The mode bits, as in `st_mode`.
        pub mode: u32,
    }

    /// Why the folder may not hold the socket, or None when it is a real folder of this user that nobody else can enter.
    /// Only such a folder keeps other users away from the socket between bind and chmod.
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

    /// Refuses a socket path whose folder is missing, a link, someone else's or open to others.
    fn check_folder(path: &Path) -> Result<()> {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
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
                "{} is the socket of a running Cod: it is left alone",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot tell whether {} is still in use", path.display())
                });
            }
        }
        // Nobody listens. Remove it only when it is still the very socket that was checked.
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
    /// The folder must be a private folder of this user. A stale socket of this user is replaced; a live socket or any other file at the path is left alone and the driver does not start.
    pub(super) fn listen(path: &Path, jobs: UnboundedSender<Job>) -> Result<()> {
        check_folder(path)?;
        remove_stale_socket(path)?;
        let listener = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        // The private folder already keeps others out; the socket mode is a second fence.
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            fs::remove_file(path).ok();
            return Err(error)
                .with_context(|| format!("cannot make {} private", path.display()));
        }
        std::thread::Builder::new()
            .name("cod-e2e-driver".into())
            .spawn(move || accept(listener, jobs))
            .context("cannot start the driver thread")?;
        Ok(())
    }

    fn accept(listener: UnixListener, jobs: UnboundedSender<Job>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let jobs = jobs.clone();
                    let started = std::thread::Builder::new()
                        .name("cod-e2e-client".into())
                        .spawn(move || {
                            if let Err(error) = serve_client(stream, jobs) {
                                log::warn!("e2e driver: client connection ended: {error:#}");
                            }
                        });
                    if let Err(error) = started {
                        log::error!("e2e driver: cannot start a client thread: {error}");
                    }
                }
                Err(error) => log::warn!("e2e driver: accept failed: {error}"),
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
            let reply = match parse_request(&line) {
                Ok(request) => {
                    let (reply, answer) = mpsc::channel();
                    if jobs.unbounded_send(Job { request, reply }).is_err() {
                        bail!("the driver stopped");
                    }
                    answer.recv().context("the driver stopped")?
                }
                Err(error) => Err(error),
            };
            let mut line = encode_reply(&reply);
            line.push('\n');
            writer
                .write_all(line.as_bytes())
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
        fn accepts_a_private_folder_of_this_user() {
            assert_eq!(folder_problem(folder(0o700), USER), None);
            assert_eq!(folder_problem(folder(0o500), USER), None);
        }

        #[test]
        fn refuses_folders_others_can_reach() {
            for mode in [0o750, 0o705, 0o710, 0o701, 0o770, 0o755, 0o777] {
                let problem = folder_problem(folder(mode), USER);
                assert!(
                    problem.as_deref().is_some_and(|problem| problem.contains("mode")),
                    "mode {mode:o} was accepted: {problem:?}"
                );
            }
        }

        #[test]
        fn refuses_links_files_and_other_owners() {
            let link = FolderFacts {
                is_symlink: true,
                is_dir: false,
                ..folder(0o700)
            };
            assert_eq!(
                folder_problem(link, USER).as_deref(),
                Some("is a symbolic link")
            );
            let file = FolderFacts {
                is_dir: false,
                ..folder(0o600)
            };
            assert_eq!(folder_problem(file, USER).as_deref(), Some("is not a folder"));
            let foreign = FolderFacts {
                owner: 0,
                ..folder(0o700)
            };
            assert!(
                folder_problem(foreign, USER)
                    .as_deref()
                    .is_some_and(|problem| problem.contains("belongs to user 0"))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    #[test]
    fn parses_the_three_commands() {
        assert_eq!(
            parse_request(r#"{"cmd":"keys","keys":"ctrl-p enter"}"#).unwrap(),
            Request::Keys(vec![
                keystroke("p", None, Modifiers::control()),
                keystroke("enter", None, Modifiers::none()),
            ])
        );
        assert_eq!(
            parse_request(r#"{"cmd":"type","text":"aB c"}"#).unwrap(),
            Request::Type(vec![
                keystroke("a", Some("a"), Modifiers::none()),
                keystroke("b", Some("B"), Modifiers::shift()),
                keystroke("space", Some(" "), Modifiers::none()),
                keystroke("c", Some("c"), Modifiers::none()),
            ])
        );
        assert_eq!(parse_request(r#"{"cmd":"state"}"#).unwrap(), Request::State);
    }

    #[test]
    fn click_defaults_ctrl_to_false() {
        assert_eq!(
            parse_request(r#"{"cmd":"click","x":12.5,"y":4}"#).unwrap(),
            Request::Click { x: 12.5, y: 4., ctrl: false }
        );
        assert_eq!(
            parse_request(r#"{"cmd":"click","x":12.5,"y":4,"ctrl":true}"#).unwrap(),
            Request::Click { x: 12.5, y: 4., ctrl: true }
        );
    }

    #[test]
    fn refuses_bad_requests() {
        for line in [
            "not json",
            r#"{"keys":"a"}"#,
            r#"{"cmd":"click"}"#,
            r#"{"cmd":"keys"}"#,
            r#"{"cmd":"keys","keys":"   "}"#,
            r#"{"cmd":"keys","keys":"ctrl-p-q"}"#,
            r#"{"cmd":"type","text":""}"#,
            r#"{"cmd":"type","text":"a\u0007"}"#,
        ] {
            assert!(parse_request(line).is_err(), "{line} was accepted");
        }
    }

    #[test]
    fn keys_type_text_like_the_linux_platform() {
        assert_eq!(
            parse_keys("a shift-a space ctrl-a enter escape alt-x").unwrap(),
            vec![
                keystroke("a", Some("a"), Modifiers::none()),
                keystroke("a", Some("A"), Modifiers::shift()),
                keystroke("space", Some(" "), Modifiers::none()),
                keystroke("a", None, Modifiers::control()),
                keystroke("enter", None, Modifiers::none()),
                keystroke("escape", None, Modifiers::none()),
                keystroke("x", Some("x"), Modifiers::alt()),
            ]
        );
    }

    #[test]
    fn typing_names_enter_and_tab() {
        assert_eq!(
            typed_keystrokes("_\n\t").unwrap(),
            vec![
                keystroke("_", Some("_"), Modifiers::none()),
                keystroke("enter", None, Modifiers::none()),
                keystroke("tab", None, Modifiers::none()),
            ]
        );
    }

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn one_pane_has_no_split() {
        assert_eq!(pane_layout(&[]), (None, vec![]));
        assert_eq!(
            pane_layout(&[rect(0., 0., 800., 600.)]),
            (None, vec![0])
        );
    }

    #[test]
    fn side_by_side_panes_are_a_vertical_split_in_left_to_right_order() {
        let right = rect(400., 0., 800., 600.);
        let left = rect(0., 0., 401., 600.);
        assert_eq!(
            pane_layout(&[right, left]),
            (Some(Split::Vertical), vec![1, 0])
        );
    }

    #[test]
    fn stacked_panes_are_a_horizontal_split_in_top_to_bottom_order() {
        let bottom = rect(0., 300., 800., 600.);
        let top = rect(0., 0., 800., 300.);
        assert_eq!(
            pane_layout(&[bottom, top]),
            (Some(Split::Horizontal), vec![1, 0])
        );
    }

    #[test]
    fn nested_layouts_name_the_root_split_and_keep_tree_order() {
        // Left column split into top and bottom, one full-height pane on the right.
        let right = rect(400., 0., 800., 600.);
        let left_bottom = rect(0., 300., 400., 600.);
        let left_top = rect(0., 0., 400., 300.);
        assert_eq!(
            pane_layout(&[right, left_bottom, left_top]),
            (Some(Split::Vertical), vec![2, 1, 0])
        );
        // A top row split into left and right, one full-width pane below.
        let bottom = rect(0., 300., 800., 600.);
        let top_right = rect(400., 0., 800., 300.);
        let top_left = rect(0., 0., 400., 300.);
        assert_eq!(
            pane_layout(&[bottom, top_right, top_left]),
            (Some(Split::Horizontal), vec![2, 1, 0])
        );
    }

    #[test]
    fn the_highlighted_row_of_the_typed_query_is_the_selection() {
        assert_eq!(
            palette_selection(
                Some("workspace: close workspace"),
                "workspace: close workspace",
                Some("workspace: close workspace"),
            )
            .as_deref(),
            Some("workspace: close workspace")
        );
        // The highlighted row is whatever the palette highlights, even a near match.
        assert_eq!(
            palette_selection(
                Some("workspace: new workspace"),
                "workspace: new workspace",
                Some("workspace: new window"),
            )
            .as_deref(),
            Some("workspace: new window")
        );
    }

    #[test]
    fn no_selection_while_the_rows_belong_to_another_query() {
        // Still matching the typed query.
        assert_eq!(
            palette_selection(None, "workspace: new workspace", Some("workspace: new window")),
            None
        );
        // The rows are those of an earlier, shorter query.
        assert_eq!(
            palette_selection(
                Some("workspace: new"),
                "workspace: new workspace",
                Some("workspace: new window"),
            ),
            None
        );
        // Nothing listed.
        assert_eq!(
            palette_selection(Some("workspace: new workspace"), "workspace: new workspace", None),
            None
        );
    }

    #[test]
    fn panel_rows_are_named_by_root_and_path() {
        let row = |root_name: &str, path: &str| ListedEntry {
            root_name: root_name.to_owned(),
            path: path.to_owned(),
            is_dir: false,
            is_expanded: false,
            is_selected: false,
        };
        assert_eq!(panel_row_name(&row("billing-api", "")), "billing-api");
        assert_eq!(
            panel_row_name(&row("billing-api", "src/invoice.rs")),
            "billing-api/src/invoice.rs"
        );
    }

    #[test]
    fn encodes_replies() {
        assert_eq!(encode_reply(&Ok(None)), r#"{"ok":true}"#);
        assert_eq!(
            encode_reply(&Ok(Some(json!({ "modal": null })))),
            r#"{"ok":true,"state":{"modal":null}}"#
        );
        assert_eq!(
            encode_reply(&Err(anyhow!("Cod has no workspace window"))),
            r#"{"ok":false,"error":"Cod has no workspace window"}"#
        );
    }
}
