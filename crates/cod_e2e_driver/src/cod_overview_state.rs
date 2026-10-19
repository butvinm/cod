//! The driver's part of story #26: the workspace overview while it is open, read only.
//!
//! `overview` in the state is null while no overview is open, else `{"placeholder", "query",
//! "selected_index", "focused", "rows": [{"kind", "title", "detail", "dot", "selected"}]}`.
//! `rows` are the rows the list shows, in order; `kind` is "workspace", "folder", "session" or
//! "page"; `dot` is the status dot ("yellow", "red", "blue", "green") or null; `focused` tells
//! whether the overview holds the window's focus, so typed keys reach its query.

use gpui::{App, Focusable as _, Window};
use picker::PickerDelegate as _;
use serde_json::{Value, json};
use workspace::Workspace;
use workspace_overview::{PLACEHOLDER, WorkspaceOverview};

/// The open overview of the workspace, or null.
pub(crate) fn overview_state(workspace: &Workspace, window: &Window, cx: &App) -> Value {
    let Some(overview) = workspace.active_modal::<WorkspaceOverview>(cx) else {
        return Value::Null;
    };
    let overview = overview.read(cx);
    let focused = overview.focus_handle(cx).contains_focused(window, cx);
    let picker = overview.picker().read(cx);
    let delegate = &picker.delegate;
    let selected = delegate.selected_index();
    let rows: Vec<Value> = delegate
        .listed_rows()
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            json!({
                "kind": row.kind.name(),
                "title": row.title,
                "detail": row.detail,
                "dot": row.status.dot_name(),
                "selected": index == selected,
            })
        })
        .collect();
    json!({
        "placeholder": PLACEHOLDER,
        "query": picker.query(cx),
        "selected_index": selected,
        "focused": focused,
        "rows": rows,
    })
}
