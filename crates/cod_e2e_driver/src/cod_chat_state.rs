//! The `chat` field of the driver state (story #28): what the active chat of the active workspace shows, read only.
//!
//! It reads `AgentPanel::cod_chat_snapshot`, a read-only getter added to agent_ui for this driver.

use agent_ui::AgentPanel;
use gpui::{App, Window};
use serde_json::{Value, json};
use workspace::Workspace;

/// `{"agent", "title", "transcript", "reply_focused", "focused_link"}` for the active chat of the workspace's agent panel, or null when the panel shows no chat.
/// `focused_link` is `{"text", "url"}` for the link of an agent reply that has the keyboard focus, else null.
pub(crate) fn chat_state(workspace: &Workspace, window: &Window, cx: &App) -> Value {
    let Some(panel) = workspace.panel::<AgentPanel>(cx) else {
        return Value::Null;
    };
    let Some(snapshot) = panel.read(cx).cod_chat_snapshot(window, cx) else {
        return Value::Null;
    };
    json!({
        "agent": snapshot.agent,
        "title": snapshot.title,
        "transcript": snapshot.transcript,
        "reply_focused": snapshot.reply_focused,
        "focused_link": snapshot
            .focused_link
            .map(|link| json!({ "text": link.text, "url": link.url })),
    })
}
