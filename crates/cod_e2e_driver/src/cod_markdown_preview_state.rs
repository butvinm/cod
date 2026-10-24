//! Read-only link bounds of Zed's markdown preview (#32 AC2), under the `markdown_preview` key of
//! the driver's state: the real mouse click target for a link the preview draws, since the driver
//! has no other way to find where it was drawn (narrow addition justified by #32 AC2).

use gpui::App;
use markdown_preview::markdown_preview_view::MarkdownPreviewView;
use serde_json::{Value, json};
use workspace::Workspace;

/// `{"links": [{"url", "bounds"}]}` for the active markdown preview tab's last painted frame, or
/// null when the active tab is not a markdown preview.
pub(crate) fn state(workspace: &Workspace, cx: &App) -> Value {
    let Some(preview) = workspace.active_item_as::<MarkdownPreviewView>(cx) else {
        return Value::Null;
    };
    let links: Vec<Value> = preview
        .read(cx)
        .cod_link_bounds(cx)
        .into_iter()
        .map(|link| json!({ "url": link.url, "bounds": crate::cod_tabs_state::bounds_json(link.bounds) }))
        .collect();
    json!({ "links": links })
}
