//! Read-only greetings of Zed's welcome and onboarding pages (issue #77 AC6), under the
//! `welcome` and `onboarding_header` keys of the driver's state: the driver has no other way to
//! read them. Narrow additions, the same shape as `cod_markdown_preview_state`.

use gpui::App;
use onboarding::Onboarding;
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

/// The header text of the active tab's first-run onboarding page (issue #77 F5), or null when
/// the active tab is not one. A brand new profile shows this page before the welcome page.
pub(crate) fn onboarding_header(workspace: &Workspace, cx: &App) -> Value {
    workspace
        .active_item_as::<Onboarding>(cx)
        .map(|onboarding| Value::String(onboarding.read(cx).cod_header_label().to_owned()))
        .unwrap_or(Value::Null)
}
