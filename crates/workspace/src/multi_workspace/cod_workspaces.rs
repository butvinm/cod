//! Cod's workspaces (story #24): every workspace of a window is one Zed `Workspace` held by the
//! window's `MultiWorkspace`. This module adds the palette commands that create, name, switch and
//! close them, the rename field, and the names, which are stored per workspace id in a key-value
//! scope.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
};

use db::kvp::KeyValueStore;
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Global, Task, TaskExt as _, WeakEntity, Window, actions,
};
use ui::prelude::*;
use ui_input::InputField;
use util::ResultExt as _;

use super::{HeldWorkspace, MultiWorkspace};
use crate::{ModalView, OpenMode, SaveIntent, Workspace, WorkspaceId};

/// Cod (#24): opens the saved remote workspace with this id (exact row) over its saved
/// connection into the given window, where it becomes a member. Remote connections live in
/// `recent_projects`, which registers this with `cod_set_remote_member_opener`.
pub type CodRemoteMemberOpen = std::sync::Arc<
    dyn Fn(
        WorkspaceId,
        remote::RemoteConnectionOptions,
        Vec<std::path::PathBuf>,
        std::sync::Arc<crate::AppState>,
        gpui::WindowHandle<MultiWorkspace>,
        &mut App,
    ) -> Task<anyhow::Result<()>>,
>;

struct CodRemoteMemberOpener(CodRemoteMemberOpen);

impl Global for CodRemoteMemberOpener {}

/// Cod (#24): registers how a pending remote member is opened (see `CodRemoteMemberOpen`).
pub fn cod_set_remote_member_opener(open: CodRemoteMemberOpen, cx: &mut App) {
    cx.set_global(CodRemoteMemberOpener(open));
}

/// Cod (#24): the window that holds a pending remote member on the same connection identity as
/// `connection` with exactly the folders `paths`, and that member's saved id. An ordinary
/// remote open of the same workspace shows this member (it loads its saved row by id) instead
/// of selecting the same row by its folders a second time.
pub fn cod_pending_remote_member(
    connection: &remote::RemoteConnectionOptions,
    paths: &[std::path::PathBuf],
    cx: &App,
) -> Option<(gpui::WindowHandle<MultiWorkspace>, WorkspaceId)> {
    cx.windows().into_iter().find_map(|window| {
        let window = window.downcast::<MultiWorkspace>()?;
        let id = window
            .read(cx)
            .ok()?
            .cod_pending_remote_id_for(connection, paths)?;
        Some((window, id))
    })
}

fn cod_open_remote_member(
    id: WorkspaceId,
    connection: remote::RemoteConnectionOptions,
    paths: Vec<std::path::PathBuf>,
    app_state: std::sync::Arc<crate::AppState>,
    window: gpui::WindowHandle<MultiWorkspace>,
    cx: &mut App,
) -> Task<anyhow::Result<()>> {
    let Some(open) = cx
        .try_global::<CodRemoteMemberOpener>()
        .map(|opener| opener.0.clone())
    else {
        return Task::ready(Err(anyhow::anyhow!(
            "remote workspace {id:?} cannot be opened: no remote opener is registered"
        )));
    };
    open(id, connection, paths, app_state, window, cx)
}

actions!(
    workspace,
    [
        /// Opens a new, empty workspace in this window.
        NewWorkspace,
        /// Names the active workspace.
        RenameWorkspace,
        /// Shows the next workspace of this window.
        NextWorkspace,
        /// Shows the previous workspace of this window.
        PreviousWorkspace,
        /// Closes the active workspace after asking to save its unsaved files.
        CloseWorkspace,
    ]
);

/// One member of a window's workspace list: loaded, or pending (saved but not loaded yet since
/// the start, see lazy restore in `restore_multiworkspace`).
#[derive(Clone)]
pub enum CodMember {
    Live(Entity<Workspace>),
    Pending(WorkspaceId),
}

/// The key-value scope that holds the workspace names, keyed by workspace id.
const NAMES_SCOPE: &str = "cod_workspace_names";

/// The names read or written in this run, so the window title does not read the database each
/// time. Absent names are cached as None. `loaded` turns true once every stored name is in the
/// map; from then on a missing id means the workspace has no name.
#[derive(Default)]
struct WorkspaceNames(RefCell<HashMap<WorkspaceId, Option<String>>>, Cell<bool>);

impl Global for WorkspaceNames {}

fn name_key(id: WorkspaceId) -> String {
    i64::from(id).to_string()
}

/// Sets up the name cache at startup and loads the stored names off the foreground thread.
pub(crate) fn init_workspace_names(cx: &mut App) {
    cx.set_global(WorkspaceNames::default());
    let kvp = KeyValueStore::global(cx);
    cx.spawn(async move |cx| {
        let stored: Vec<(String, String)> = cx
            .background_executor()
            .spawn(async move {
                kvp.select_bound::<&str, (String, String)>(
                    "SELECT key, value FROM scoped_kv_store WHERE namespace = (?)",
                )
                .and_then(|mut select| select(NAMES_SCOPE))
            })
            .await
            .log_err()
            .unwrap_or_default();
        cx.update(|cx| {
            if let Some(names) = cx.try_global::<WorkspaceNames>() {
                let mut map = names.0.borrow_mut();
                for (key, name) in stored {
                    if let Ok(id) = key.parse::<i64>() {
                        // A name set or read in this run is newer than the stored list.
                        map.entry(WorkspaceId::from_i64(id)).or_insert(Some(name));
                    }
                }
                names.1.set(true);
            }
        });
    })
    .detach();
}

/// The name the user gave the workspace, or None while it has none.
pub fn cod_workspace_name(workspace: &Workspace, cx: &App) -> Option<String> {
    let id = workspace.database_id()?;
    cod_workspace_name_by_id(id, cx)
}

/// The name the user gave the workspace with this id (loaded or pending), or None.
pub fn cod_workspace_name_by_id(id: WorkspaceId, cx: &App) -> Option<String> {
    let names = cx.try_global::<WorkspaceNames>();
    if let Some(names) = names {
        if let Some(name) = names.0.borrow().get(&id) {
            return name.clone();
        }
        if names.1.get() {
            return None;
        }
    }
    // Only before the startup load finished: read this one name and cache it, absent or not.
    let name = KeyValueStore::global(cx)
        .scoped(NAMES_SCOPE)
        .read(&name_key(id))
        .log_err()
        .flatten();
    if let Some(names) = names {
        names.0.borrow_mut().insert(id, name.clone());
    }
    name
}

/// Stores the name of the workspace with this id; None removes it.
fn set_workspace_name(id: Option<WorkspaceId>, name: Option<String>, cx: &mut App) {
    let Some(id) = id else {
        return;
    };
    cx.default_global::<WorkspaceNames>()
        .0
        .borrow_mut()
        .insert(id, name.clone());
    let kvp = KeyValueStore::global(cx);
    cx.spawn(async move |_cx| {
        let scope = kvp.scoped(NAMES_SCOPE);
        let result = match name {
            Some(name) => scope.write(name_key(id), name).await,
            None => scope.delete(name_key(id)).await,
        };
        result.log_err();
    })
    .detach();
}

/// Forgets the name of a workspace that leaves its window for good, in this run's cache. The
/// stored name goes with `delete_stored_workspace_name`, after the workspace left its session.
pub(super) fn forget_workspace_name(id: WorkspaceId, cx: &mut App) {
    cx.default_global::<WorkspaceNames>()
        .0
        .borrow_mut()
        .insert(id, None);
}

/// Deletes the stored name of a workspace that was removed from its window.
pub(super) async fn delete_stored_workspace_name(
    kvp: &KeyValueStore,
    id: WorkspaceId,
) -> anyhow::Result<()> {
    kvp.scoped(NAMES_SCOPE).delete(name_key(id)).await
}

/// The field of "workspace: rename workspace": it proposes the current name, enter saves the
/// typed text, escape keeps the old name.
pub struct RenameWorkspaceModal {
    workspace: WeakEntity<Workspace>,
    input: Entity<InputField>,
}

impl RenameWorkspaceModal {
    fn new(
        workspace: WeakEntity<Workspace>,
        current: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputField::new(window, cx, "Workspace name"));
        if let Some(current) = current {
            let editor = input.read(cx).editor().clone();
            editor.set_text(&current, window, cx);
            editor.select_all(window, cx);
        }
        Self { workspace, input }
    }

    /// The text the field holds now.
    pub fn text(&self, cx: &App) -> String {
        self.input.read(cx).text(cx)
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        let name = if text.is_empty() { None } else { Some(text) };
        if let Some(workspace) = self.workspace.upgrade() {
            let id = workspace.read(cx).database_id();
            set_workspace_name(id, name, cx);
            workspace.update(cx, |workspace, cx| workspace.refresh_window_state(window, cx));
        }
        cx.emit(DismissEvent);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for RenameWorkspaceModal {}

impl Focusable for RenameWorkspaceModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).editor().focus_handle(cx)
    }
}

impl ModalView for RenameWorkspaceModal {}

impl Render for RenameWorkspaceModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("RenameWorkspaceModal")
            .elevation_3(cx)
            .w(rems(34.))
            .p_2()
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .child(self.input.clone())
    }
}

impl MultiWorkspace {
    /// Opens a new workspace with no folder and no tab in this window and shows it.
    pub(super) fn new_workspace(
        &mut self,
        _: &NewWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };
        let app_state = self.workspace().read(cx).app_state().clone();
        Workspace::new_local(
            Vec::new(),
            app_state,
            Some(window_handle),
            None,
            None,
            OpenMode::Activate,
            cx,
        )
        .detach_and_log_err(cx);
    }

    pub(super) fn rename_workspace(
        &mut self,
        _: &RenameWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace().clone();
        let current = cod_workspace_name(workspace.read(cx), cx);
        let weak = workspace.downgrade();
        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, |window, cx| {
                RenameWorkspaceModal::new(weak, current, window, cx)
            });
        });
    }

    pub(super) fn next_workspace(
        &mut self,
        _: &NextWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_workspace(true, window, cx);
    }

    pub(super) fn previous_workspace(
        &mut self,
        _: &PreviousWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cycle_workspace(false, window, cx);
    }

    /// Shows the workspace after (or before) the active one in window order, wrapping around.
    fn cycle_workspace(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        // A pending member is still loading: the switch runs once it is shown, in the order
        // the switches were asked for.
        if self.cod_materializing {
            self.cod_queued_cycles.push(forward);
            return;
        }
        let members = self.cod_members(cx);
        let count = members.len();
        if count < 2 {
            return;
        }
        let active = self.workspace().clone();
        let Some(index) = members
            .iter()
            .position(|member| matches!(member, CodMember::Live(workspace) if *workspace == active))
        else {
            return;
        };
        for step in 1..count {
            let next = if forward {
                (index + step) % count
            } else {
                (index + count - step) % count
            };
            match &members[next] {
                CodMember::Live(workspace) => {
                    self.activate(workspace.clone(), None, window, cx);
                    return;
                }
                CodMember::Pending(id) if self.cod_pending.contains_key(id) => {
                    self.cod_activate_pending(*id, window, cx)
                        .detach_and_log_err(cx);
                    return;
                }
                // A listed member whose saved data was not found cannot be shown.
                CodMember::Pending(_) => {}
            }
        }
    }

    /// The members of this window in window order: the loaded workspaces, and the pending ones
    /// at the places they had in the saved list.
    pub fn cod_members(&self, cx: &App) -> Vec<CodMember> {
        let mut members: Vec<(Option<WorkspaceId>, CodMember)> = self
            .held
            .iter()
            .map(|held| {
                (
                    held.workspace.read(cx).database_id(),
                    CodMember::Live(held.workspace.clone()),
                )
            })
            .collect();
        for id in &self.cod_unrestored_workspace_ids {
            if members.iter().any(|(member_id, _)| *member_id == Some(*id)) {
                continue;
            }
            // Put it right after the nearest member that came before it in the saved list.
            let saved_index = self
                .cod_saved_order
                .iter()
                .position(|saved| saved == id)
                .unwrap_or(self.cod_saved_order.len());
            let insert_at = self.cod_saved_order[..saved_index]
                .iter()
                .rev()
                .find_map(|before| {
                    members
                        .iter()
                        .position(|(member_id, _)| *member_id == Some(*before))
                })
                .map(|position| position + 1)
                .unwrap_or(if saved_index == self.cod_saved_order.len() {
                    members.len()
                } else {
                    0
                });
            members.insert(insert_at, (Some(*id), CodMember::Pending(*id)));
        }
        members.into_iter().map(|(_, member)| member).collect()
    }

    /// The pending member whose folders are exactly `path_list`, if any.
    pub fn cod_pending_id_for_paths(
        &self,
        path_list: &util::path_list::PathList,
    ) -> Option<WorkspaceId> {
        // Local routing only: a remote pending member is not a local folder match.
        self.cod_pending.iter().find_map(|(id, paths)| {
            (!self.cod_pending_remote.contains_key(id)
                && util::path_list::PathList::new(paths) == *path_list)
                .then_some(*id)
        })
    }

    /// The project group key paths of a pending local member: each folder that is a linked git
    /// worktree stands for its main repository, as for a loaded workspace's group key. Other
    /// folders (plain checkouts, submodules, non-git folders) stand for themselves.
    pub(super) fn cod_pending_group_paths(
        &self,
        id: WorkspaceId,
    ) -> Option<util::path_list::PathList> {
        if self.cod_pending_remote.contains_key(&id) {
            return None;
        }
        let paths = self.cod_pending.get(&id)?;
        let mains: Vec<std::path::PathBuf> = paths
            .iter()
            .map(|path| cod_linked_worktree_main_path(path).unwrap_or_else(|| path.clone()))
            .collect();
        Some(util::path_list::PathList::new(&mains))
    }

    /// The local pending members of this window with the folders of each, in saved order.
    /// Remote pending members are left out: local routing never matches them.
    pub fn cod_pending_members(&self) -> Vec<(WorkspaceId, Vec<std::path::PathBuf>)> {
        let mut members: Vec<(WorkspaceId, Vec<std::path::PathBuf>)> = self
            .cod_pending
            .iter()
            .filter(|(id, _)| !self.cod_pending_remote.contains_key(id))
            .map(|(id, paths)| (*id, paths.clone()))
            .collect();
        members.sort_by_key(|(id, _)| {
            self.cod_saved_order
                .iter()
                .position(|saved| saved == id)
                .unwrap_or(usize::MAX)
        });
        members
    }

    /// Loads and shows a pending member (see `cod_activate_pending`).
    pub fn cod_show_pending(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        self.cod_activate_pending(id, window, cx)
    }

    /// The folders of a pending member, None when the id is not a pending member.
    pub fn cod_pending_paths(&self, id: WorkspaceId) -> Option<&[std::path::PathBuf]> {
        self.cod_pending.get(&id).map(|paths| paths.as_slice())
    }

    /// Whether the pending member `id` is a remote workspace (it loads over its saved
    /// connection).
    pub fn cod_pending_is_remote(&self, id: WorkspaceId) -> bool {
        self.cod_pending_remote.contains_key(&id)
    }

    /// The pending remote member on the same connection identity as `connection` with exactly
    /// the folders `paths`, if any.
    pub fn cod_pending_remote_id_for(
        &self,
        connection: &remote::RemoteConnectionOptions,
        paths: &[std::path::PathBuf],
    ) -> Option<WorkspaceId> {
        let identity = remote::remote_connection_identity(connection);
        let wanted = util::path_list::PathList::new(paths);
        let mut matches: Vec<WorkspaceId> = self
            .cod_pending_remote
            .iter()
            .filter(|(id, options)| {
                remote::remote_connection_identity(options) == identity
                    && self.cod_pending.get(*id).is_some_and(|saved| {
                        util::path_list::PathList::new(saved.as_slice()) == wanted
                    })
            })
            .map(|(id, _)| *id)
            .collect();
        // The first one in window order, so the choice does not depend on map order.
        matches.sort_by_key(|id| {
            self.cod_saved_order
                .iter()
                .position(|saved| saved == id)
                .unwrap_or(usize::MAX)
        });
        matches.first().copied()
    }

    /// Adds a remote member that is saved but not loaded: the first time it is shown it loads
    /// its exact saved row over its saved connection.
    pub fn cod_add_pending_remote_workspace(
        &mut self,
        id: WorkspaceId,
        connection: remote::RemoteConnectionOptions,
        paths: Vec<std::path::PathBuf>,
    ) {
        self.cod_add_pending_workspace(id, paths);
        self.cod_pending_remote.insert(id, connection);
    }

    /// Adds a member that is saved but not loaded: it loads the first time it is shown.
    pub fn cod_add_pending_workspace(&mut self, id: WorkspaceId, paths: Vec<std::path::PathBuf>) {
        if !self.cod_unrestored_workspace_ids.contains(&id) {
            self.cod_unrestored_workspace_ids.push(id);
        }
        if !self.cod_saved_order.contains(&id) {
            self.cod_saved_order.push(id);
        }
        self.cod_pending.insert(id, paths);
    }

    /// Loads a pending member in the foreground and shows it, at its place in the window
    /// order and with its id. On failure it stays pending, so a later switch tries again.
    pub(super) fn cod_activate_pending(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        if self.cod_materializing {
            // Another member is loading: wait for it, then load or show this one by its id.
            // Success is never reported for a member that was not shown.
            let (done_tx, done_rx) = futures::channel::oneshot::channel();
            self.cod_materialize_waiters.push(done_tx);
            return cx.spawn_in(window, async move |this, cx| {
                done_rx.await.ok();
                this.update_in(cx, |this, window, cx| {
                    if this.cod_pending.contains_key(&id) {
                        return this.cod_activate_pending(id, window, cx);
                    }
                    let loaded = this
                        .held
                        .iter()
                        .map(|held| held.workspace.clone())
                        .find(|workspace| workspace.read(cx).database_id() == Some(id));
                    match loaded {
                        Some(workspace) => {
                            if *this.workspace() != workspace {
                                this.activate(workspace, None, window, cx);
                            }
                            Task::ready(Ok(()))
                        }
                        None => Task::ready(Err(anyhow::anyhow!("workspace {id:?} is gone"))),
                    }
                })?
                .await
            });
        }
        let Some(paths) = self.cod_pending.get(&id).cloned() else {
            return Task::ready(Err(anyhow::anyhow!("workspace {id:?} is not pending")));
        };
        let remote = self.cod_pending_remote.get(&id).cloned();
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return Task::ready(Ok(()));
        };
        let order: Vec<WorkspaceId> = self.cod_saved_workspace_ids(cx);
        let app_state = self.workspace().read(cx).app_state().clone();
        self.cod_materializing = true;
        cx.spawn_in(window, async move |this, cx| {
            let result: anyhow::Result<()> = if let Some(connection) = remote {
                // A remote member loads its exact saved row over its saved connection.
                match cx.update(|_window, cx| {
                    cod_open_remote_member(
                        id,
                        connection,
                        paths.clone(),
                        app_state.clone(),
                        window_handle,
                        cx,
                    )
                }) {
                    Ok(task) => task.await,
                    Err(err) => Err(err),
                }
            } else if paths.is_empty() {
                match cx.update(|_window, cx| {
                    crate::open_workspace_by_id(id, app_state.clone(), Some(window_handle), cx)
                }) {
                    Ok(task) => task.await.map(|_| ()),
                    Err(err) => Err(err),
                }
            } else {
                match cx.update(|_window, cx| {
                    Workspace::restore_local_by_id(
                        id,
                        paths.clone(),
                        app_state.clone(),
                        Some(window_handle),
                        OpenMode::Activate,
                        cx,
                    )
                }) {
                    Ok(task) => task.await.map(|_| ()),
                    Err(err) => Err(err),
                }
            };
            this.update_in(cx, |this, window, cx| {
                this.cod_materializing = false;
                let loaded = this
                    .held
                    .iter()
                    .map(|held| held.workspace.clone())
                    .find(|workspace| workspace.read(cx).database_id() == Some(id));
                match (&result, loaded) {
                    (Ok(()), Some(workspace)) => {
                        this.cod_pending.remove(&id);
                        this.cod_pending_remote.remove(&id);
                        this.cod_unrestored_workspace_ids
                            .retain(|other| *other != id);
                        this.cod_order_workspaces(&order, cx);
                        if *this.workspace() != workspace {
                            this.activate(workspace, None, window, cx);
                        }
                        // The items are loaded now: the shown workspace's active item takes the
                        // keys, unless the user opened a modal in it meanwhile. Focusing only the
                        // pane is not enough while the platform window is inactive.
                        let shown = this.workspace().clone();
                        shown.update(cx, |shown, cx| shown.cod_focus_restored(window, cx));
                    }
                    (Ok(()), None) => {
                        log::error!("Workspace {id:?} loaded but is not in its window; it stays pending");
                        this.serialize(cx);
                    }
                    (Err(err), _) => {
                        log::error!("Failed to load workspace {id:?}; it stays pending: {err:#}");
                        this.serialize(cx);
                    }
                }
                // Run the switches asked for while the member loaded.
                for forward in std::mem::take(&mut this.cod_queued_cycles) {
                    this.cycle_workspace(forward, window, cx);
                }
                // Then the activations by id asked for meanwhile, in the order they came.
                for waiter in std::mem::take(&mut this.cod_materialize_waiters) {
                    waiter.send(()).ok();
                }
            })?;
            result
        })
    }

    /// Asks to save the unsaved files of the active workspace like Zed does when a window closes,
    /// then removes the workspace from the window and its session and shows the one before it
    /// (else the one after it). Closing the only workspace leaves a new empty one.
    pub(super) fn close_workspace(
        &mut self,
        _: &CloseWorkspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(window_handle) = window.window_handle().downcast::<MultiWorkspace>() else {
            return;
        };
        let doomed = self.workspace().clone();
        let app_state = doomed.read(cx).app_state().clone();
        cx.spawn_in(window, async move |this, cx| {
            let consent = doomed
                .update_in(cx, |workspace, window, cx| {
                    workspace.save_all_internal(SaveIntent::Close, false, window, cx)
                })?
                .await?;
            if !consent {
                return anyhow::Ok(());
            }
            // Write the closing workspace's final state (it stays in recent projects) before it
            // leaves the session; detaching cancels its scheduled saves.
            doomed
                .update_in(cx, |workspace, window, cx| {
                    workspace.flush_serialization(window, cx)
                })?
                .await;

            // The replacement is the member before the closed one (else the one after it), loaded
            // or pending; a listed member whose saved data was not found is skipped.
            let replacement = this.update(cx, |this, cx| {
                let members = this.cod_members(cx);
                let index = members.iter().position(|member| match member {
                    CodMember::Live(workspace) => *workspace == doomed,
                    CodMember::Pending(_) => false,
                })?;
                let candidates: Vec<(usize, CodMember)> = members
                    .into_iter()
                    .enumerate()
                    .filter(|(position, member)| {
                        *position != index
                            && match member {
                                CodMember::Live(_) => true,
                                CodMember::Pending(id) => this.cod_pending.contains_key(id),
                            }
                    })
                    .collect();
                candidates
                    .iter()
                    .rev()
                    .find(|(position, _)| *position < index)
                    .or_else(|| candidates.iter().find(|(position, _)| *position > index))
                    .map(|(_, member)| member.clone())
            })?;
            match replacement {
                Some(CodMember::Live(replacement)) => {
                    this.update_in(cx, |this, window, cx| {
                        this.activate(replacement, None, window, cx);
                    })?;
                }
                Some(CodMember::Pending(id)) => {
                    this.update_in(cx, |this, window, cx| {
                        this.cod_activate_pending(id, window, cx)
                    })?
                    .await?;
                }
                None => {
                    cx.update(|_window, cx| {
                        Workspace::new_local(
                            Vec::new(),
                            app_state,
                            Some(window_handle),
                            None,
                            None,
                            OpenMode::Activate,
                            cx,
                        )
                    })?
                    .await?;
                }
            }

            this.update(cx, |this, cx| {
                if *this.workspace() != doomed && this.held_index(&doomed).is_some() {
                    // Detaching also deletes the workspace's name.
                    this.detach_workspace(&doomed, cx);
                    this.serialize(cx);
                    cx.notify();
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    /// The ids to save as the window's workspace list: the held workspaces in window order, then
    /// the ones from the last saved list that are not back yet, so a failed or interrupted
    /// restore never drops them.
    pub(super) fn cod_saved_workspace_ids(&self, cx: &App) -> Vec<WorkspaceId> {
        // Pending members keep their places among the loaded ones.
        let mut ids: Vec<WorkspaceId> = self
            .cod_members(cx)
            .into_iter()
            .filter_map(|member| match member {
                CodMember::Live(workspace) => workspace.read(cx).database_id(),
                CodMember::Pending(id) => Some(id),
            })
            .collect();
        // Members of the saved list that are not back yet are pending members above, so a
        // registry edit made while the start still restores this window keeps them.
        for id in &self.cod_removing_workspace_ids {
            if !ids.contains(id) {
                ids.push(*id);
            }
        }
        // Members that left with a group move belong to their new window's entry already.
        ids.retain(|id| !self.cod_moving_ids.contains(id));
        ids
    }

    /// Starts a restore: every id of the saved list counts as not back until its workspace is
    /// held by this window.
    pub fn cod_begin_restore(&mut self, saved: &[WorkspaceId]) {
        self.cod_unrestored_workspace_ids = saved.to_vec();
        self.cod_saved_order = saved.to_vec();
    }

    /// Saves the window's state with its workspace list now; the future ends once it is written
    /// and gives the result of the registry write.
    pub fn cod_save_workspace_list_now(
        &mut self,
        cx: &mut Context<Self>,
    ) -> impl std::future::Future<Output = anyhow::Result<()>> + use<> {
        self.serialize_now(cx)
    }

    /// Drops from the saved list the ids of workspaces that were closed (no session binding)
    /// but were still listed when the app stopped.
    pub fn cod_forget_closed_workspaces(&mut self, closed: &[WorkspaceId]) {
        self.cod_unrestored_workspace_ids
            .retain(|id| !closed.contains(id));
    }

    /// Ends a restore. The ids that are still not held failed to come back; they stay in the
    /// saved list so the next start tries them again.
    pub fn cod_finish_restore(&mut self, cx: &mut Context<Self>) {
        let held: Vec<WorkspaceId> = self
            .held
            .iter()
            .filter_map(|held| held.workspace.read(cx).database_id())
            .collect();
        self.cod_unrestored_workspace_ids
            .retain(|id| !held.contains(id));
        let lost: Vec<WorkspaceId> = self
            .cod_unrestored_workspace_ids
            .iter()
            .copied()
            .filter(|id| !self.cod_pending.contains_key(id))
            .collect();
        if !lost.is_empty() {
            log::error!(
                "Workspaces {:?} were not found; they stay in the window's list for the next start",
                lost
            );
        }
    }

    /// Puts the held workspaces in the given order of database ids (the order the window had
    /// when it was saved). Workspaces not in the list keep their order after the listed ones.
    pub fn cod_order_workspaces(&mut self, order: &[WorkspaceId], cx: &mut Context<Self>) {
        if order.is_empty() {
            return;
        }
        let keys: Vec<usize> = self
            .held
            .iter()
            .map(|held| {
                held.workspace
                    .read(cx)
                    .database_id()
                    .and_then(|id| order.iter().position(|ordered| *ordered == id))
                    .unwrap_or(usize::MAX)
            })
            .collect();
        let mut keyed: Vec<(usize, HeldWorkspace)> =
            keys.into_iter().zip(self.held.drain(..)).collect();
        keyed.sort_by_key(|(key, _)| *key);
        self.held = keyed.into_iter().map(|(_, held)| held).collect();
        self.serialize(cx);
        cx.notify();
    }
}

/// Cod (#24): the main repository folder of a linked git worktree, read from its `.git` file
/// (`gitdir: <dir>`) and that directory's `commondir` file, under the stock repository identity
/// rules (`repo_identity_path`): `.git` and `.bare` give their parent, a named bare repository
/// such as `project.git` is itself the identity. None for anything else.
fn cod_linked_worktree_main_path(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    let dot_git = std::fs::read_to_string(folder.join(".git")).ok()?;
    let gitdir = dot_git.trim().strip_prefix("gitdir:")?.trim();
    let gitdir = folder.join(gitdir);
    let commondir = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = std::fs::canonicalize(gitdir.join(commondir.trim())).ok()?;
    let identity =
        project::repo_identity_path(&common, util::paths::PathStyle::local()).to_path_buf();
    // A folder whose identity is the folder itself is not a linked worktree of another one.
    (identity != folder).then_some(identity)
}

/// Cod (#37): names the workspace like "workspace: rename workspace" does (None removes the
/// name) and shows the new name in the window title when the workspace is the one shown.
pub fn cod_set_workspace_name(
    workspace: &Entity<Workspace>,
    name: Option<String>,
    window: &mut Window,
    cx: &mut App,
) {
    let id = workspace.read(cx).database_id();
    set_workspace_name(id, name, cx);
    workspace.update(cx, |workspace, cx| {
        if workspace.owns_window_chrome() {
            workspace.refresh_window_state(window, cx);
        }
        cx.notify();
    });
}
