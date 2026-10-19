//! The `chat` field of the driver state (story #28): what the active chat of the active workspace shows, read only.
//!
//! It reads `AgentPanel::cod_chat_snapshot`, a read-only getter added to agent_ui for this driver.

use agent_ui::AgentPanel;
use gpui::{App, Focusable as _, Window};
use serde_json::{Value, json};
use workspace::Workspace;

/// `{"x", "y", "width", "height"}` of a window rectangle, or null before it was drawn.
fn rect_json(bounds: Option<(f32, f32, f32, f32)>) -> Value {
    match bounds {
        Some((x, y, width, height)) => json!({ "x": x, "y": y, "width": width, "height": height }),
        None => Value::Null,
    }
}

/// Whether the agent panel's commands are in the command palette: Zed registers `agent::ToggleFocus` only once the panel has loaded, a moment after the window, and the palette lists only the commands of the frame it opens on.
pub(crate) fn agent_panel_ready(window: &Window, cx: &App) -> bool {
    cx.build_action("agent::ToggleFocus", None)
        .is_ok_and(|action| window.is_action_available(action.as_ref(), cx))
}

/// Whether the keyboard focus is inside the agent panel of the workspace.
pub(crate) fn agent_panel_focused(workspace: &Workspace, window: &Window, cx: &App) -> bool {
    workspace
        .panel::<AgentPanel>(cx)
        .is_some_and(|panel| panel.read(cx).focus_handle(cx).contains_focused(window, cx))
}

/// `{"agent", "session_id", "status", "title", "transcript", "reply_focused", "focused_link", "link_bounds"}` for the active chat of the workspace's agent panel, or null when the panel shows no chat.
/// `focused_link` is `{"text", "url"}` for the link of an agent reply that has the keyboard focus, else null.
/// `link_bounds` is `[{"url", "bounds"}]` for every link across the chat's replies, the real mouse
/// click target of #32 AC1 (a chat reply link cannot otherwise be clicked: the driver has no other
/// way to find where it was drawn).
pub(crate) fn chat_state(workspace: &Workspace, window: &Window, cx: &App) -> Value {
    let Some(panel) = workspace.panel::<AgentPanel>(cx) else {
        return Value::Null;
    };
    let Some(snapshot) = panel.read(cx).cod_chat_snapshot(window, cx) else {
        return Value::Null;
    };
    json!({
        "agent": snapshot.agent,
        "session_id": snapshot.session_id,
        "status": snapshot.status.as_str(),
        "title": snapshot.title,
        "transcript": snapshot.transcript,
        "reply_focused": snapshot.reply_focused,
        "focused_link": snapshot
            .focused_link
            .map(|link| json!({ "text": link.text, "url": link.url })),
        "link_bounds": snapshot
            .link_bounds
            .into_iter()
            .map(|link| json!({ "url": link.url, "bounds": rect_json(link.bounds) }))
            .collect::<Vec<_>>(),
    })
}

/// The `chat_tabs` field of the driver state (story #28 AC1): the panel header's chat tabs of
/// the active workspace, in the order they are drawn, as `[{"session_id", "title", "status",
/// "dot", "active"}]`. Empty (not null) when the panel holds no chat yet.
pub(crate) fn chat_tabs_state(workspace: &Workspace, cx: &App) -> Value {
    let Some(panel) = workspace.panel::<AgentPanel>(cx) else {
        return json!([]);
    };
    let tabs: Vec<Value> = panel
        .read(cx)
        .cod_chat_tabs(cx)
        .into_iter()
        .map(|tab| {
            json!({
                "session_id": tab.session_id,
                "title": tab.title,
                "status": tab.status.as_str(),
                "dot": tab.status.cod_status().dot_name(),
                "active": tab.active,
            })
        })
        .collect();
    json!(tabs)
}
