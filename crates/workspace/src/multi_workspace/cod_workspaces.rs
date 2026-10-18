//! Cod's workspaces (story #24): every workspace of a window is one Zed `Workspace` held by the
//! window's `MultiWorkspace`. This module adds the palette commands that create, name, switch and
//! close them, the rename field, and the names, which are stored per workspace id in a key-value
//! scope.

use std::{cell::RefCell, collections::HashMap};

use db::kvp::KeyValueStore;
use gpui::{
    App, AppContext as _, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    Global, TaskExt as _, WeakEntity, Window, actions,
};
use ui::prelude::*;
use ui_input::InputField;
use util::ResultExt as _;

use super::{HeldWorkspace, MultiWorkspace};
use crate::{ModalView, OpenMode, SaveIntent, Workspace, WorkspaceId};

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

/// The key-value scope that holds the workspace names, keyed by workspace id.
const NAMES_SCOPE: &str = "cod_workspace_names";

/// The names read or written in this run, so the window title does not read the database each time.
#[derive(Default)]
struct WorkspaceNames(RefCell<HashMap<WorkspaceId, Option<String>>>);

impl Global for WorkspaceNames {}

fn name_key(id: WorkspaceId) -> String {
    i64::from(id).to_string()
}

/// The name the user gave the workspace, or None while it has none.
pub fn cod_workspace_name(workspace: &Workspace, cx: &App) -> Option<String> {
    let id = workspace.database_id()?;
    let names = cx.try_global::<WorkspaceNames>();
    if let Some(names) = names
        && let Some(name) = names.0.borrow().get(&id)
    {
        return name.clone();
    }
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
        let count = self.held.len();
        if count < 2 {
            return;
        }
        let index = self.displayed_index();
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        let workspace = self.held[next].workspace.clone();
        self.activate(workspace, None, window, cx);
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

            let replacement = this.update(cx, |this, _cx| {
                let index = this.held_index(&doomed)?;
                let others: Vec<Entity<Workspace>> = this
                    .held
                    .iter()
                    .map(|held| held.workspace.clone())
                    .filter(|workspace| *workspace != doomed)
                    .collect();
                others.get(index.saturating_sub(1)).cloned()
            })?;
            match replacement {
                Some(replacement) => {
                    this.update_in(cx, |this, window, cx| {
                        this.activate(replacement, None, window, cx);
                    })?;
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
                    let id = doomed.read(cx).database_id();
                    this.detach_workspace(&doomed, cx);
                    set_workspace_name(id, None, cx);
                    this.serialize(cx);
                    cx.notify();
                }
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
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
