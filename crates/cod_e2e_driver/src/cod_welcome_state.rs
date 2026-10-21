//! Read-only greeting of Zed's welcome page (issue #77 AC6), under the `welcome` key of the
//! driver's state: the driver has no other way to read it. Narrow addition, the same shape as
//! `cod_markdown_preview_state`.

use gpui::App;
use serde_json::Value;
use workspace::Workspace;
use workspace::welcome::WelcomePage;

/// The greeting text of the active tab's welcome page, or null when the active tab is not one.
pub(crate) fn state(workspace: &Workspace, cx: &App) -> Value {
    workspace
        .active_item_as::<WelcomePage>(cx)
        .map(|welcome| Value::String(welcome.read(cx).cod_welcome_label().to_owned()))
        .unwrap_or(Value::Null)
}
