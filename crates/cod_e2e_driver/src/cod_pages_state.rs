//! Read-only state of page tabs for the end-to-end tests of #31 and #32, under the `pages` key of the driver's state.
//!
//! `open` lists every open page tab of the window: its page ID, URL, title, whether it is loading, its state (`loading`, `loaded` or `failed`), its kind, the index of its workspace (as in `workspaces`), the index of its pane in layout order (as in `panes`), whether it is its pane's active tab, whether it is hidden, whether its browser was started, whether it holds the keyboard focus, and its drawn pixel bounds in the window (`bounds`, #43: for a test that clicks inside the page's own HTML content).
//! `saved` lists the saved page rows (page ID and URL), including those of workspaces not loaded yet.
//! `prompt` is the "web: open url" prompt: its text and its error, or null while it is closed.

#[cfg(target_os = "linux")]
use gpui::Entity;
use gpui::{App, Window};
use serde_json::Value;
use workspace::MultiWorkspace;

#[cfg(target_os = "linux")]
pub(crate) fn state(multi: &MultiWorkspace, window: &Window, cx: &App) -> Value {
    use gpui::Focusable as _;
    use serde_json::json;
    use workspace::{CodMember, Workspace};

    // The index of a loaded workspace among the members the driver lists in `workspaces` (pending ones with saved data count too).
    let live_index = |workspace: &Entity<Workspace>| -> Option<usize> {
        multi
            .cod_members(cx)
            .into_iter()
            .filter(|member| match member {
                CodMember::Live(_) => true,
                CodMember::Pending(id) => multi.cod_pending_paths(*id).is_some(),
            })
            .position(|member| matches!(member, CodMember::Live(live) if live == *workspace))
    };

    let open: Vec<Value> = web_view::pages::all(cx)
        .into_iter()
        .map(|view| {
            let item_id = view.entity_id();
            let page = view.read(cx);
            let workspace = page.workspace().and_then(|workspace| workspace.upgrade());
            let (workspace_index, pane_index, active) = match &workspace {
                Some(workspace) => {
                    let ordered = ordered_panes(workspace.read(cx));
                    let pane = ordered.iter().position(|pane| {
                        pane.read(cx).items().any(|item| item.item_id() == item_id)
                    });
                    let active = pane.is_some_and(|index| {
                        ordered[index]
                            .read(cx)
                            .active_item()
                            .is_some_and(|item| item.item_id() == item_id)
                    });
                    (live_index(workspace), pane, active)
                }
                None => (None, None, false),
            };
            let bounds = page.pane_bounds();
            json!({
                "id": page.page_id().to_string(),
                "url": page.current_url(),
                "title": page.title(),
                "loading": page.is_loading(),
                "state": page.state().as_str(),
                "kind": page.kind().as_str(),
                "workspace": workspace_index,
                "pane": pane_index,
                "active": active,
                "hidden": page.is_hidden(),
                "browser_started": page.browser_started(),
                "focused": page.focus_handle(cx).contains_focused(window, cx),
                // The page's drawn pixel bounds in the window (#43): lets a test compute a real
                // mouse click point inside its own, deterministically laid out HTML content.
                "bounds": {
                    "left": f32::from(bounds.origin.x),
                    "top": f32::from(bounds.origin.y),
                    "width": f32::from(bounds.size.width),
                    "height": f32::from(bounds.size.height),
                },
            })
        })
        .collect();

    let saved: Vec<Value> = web_view::pages::saved_pages(cx)
        .into_iter()
        .map(|(_, _, page_id, url)| json!({ "id": page_id, "url": url }))
        .collect();

    let prompt = multi
        .workspace()
        .read(cx)
        .active_modal::<web_view::OpenPageModal>(cx)
        .map(|modal| {
            let modal = modal.read(cx);
            json!({ "query": modal.query(cx), "error": modal.error() })
        });

    json!({ "open": open, "saved": saved, "prompt": prompt })
}

/// The workspace's center panes in layout order, the same order as the driver's `panes`.
#[cfg(target_os = "linux")]
fn ordered_panes(workspace: &workspace::Workspace) -> Vec<Entity<workspace::Pane>> {
    let panes = workspace.panes();
    let rects: Option<Vec<crate::Rect>> = panes
        .iter()
        .map(|pane| {
            workspace.bounding_box_for_pane(pane).map(|bounds| crate::Rect {
                left: f32::from(bounds.origin.x),
                top: f32::from(bounds.origin.y),
                right: f32::from(bounds.origin.x + bounds.size.width),
                bottom: f32::from(bounds.origin.y + bounds.size.height),
            })
        })
        .collect();
    let order: Vec<usize> = match rects {
        Some(rects) if panes.len() > 1 => crate::pane_layout(&rects).1,
        _ => (0..panes.len()).collect(),
    };
    order.into_iter().map(|index| panes[index].clone()).collect()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn state(
    _multi: &MultiWorkspace,
    _window: &Window,
    _cx: &App,
) -> Value {
    Value::Null
}
