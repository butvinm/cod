//! A workspace item that shows a web page through the operating system's web view.
//!
//! This is the Cod spike for issue #79: the page is a native child view laid over the GPUI window, kept on the bounds GPUI gives the item and hidden while its tab is not the active one.
//! macOS uses WKWebView for this spike; Linux uses WebKitGTK in an X11 child window (GPUI on X11 only).
//!
//! Issue #16 adds the real page-tab backend, chosen on Linux with `COD_WEBVIEW_BACKEND=offscreen`
//! and always on macOS (issue #23 round 1): an off-screen browser (CEF, in the `cod-browser`
//! helper process) whose frames GPUI paints as images. On Linux this also works on native
//! Wayland; on macOS it replaces the WKWebView spike for page tabs (the spike stays only for the
//! `COD_SPIKE_URL`/`OpenSpikeUrl` developer comparison tool, which `offscreen::init` itself still
//! drives on both platforms; see `offscreen::init`'s own `COD_SPIKE_URL` handling). The off-screen
//! modules have almost no OS-specific code themselves (verified: no internal `target_os` cfgs
//! beyond this file, `cod_browser` and `linux.rs`/`spike_driver.rs`), so widening their module
//! gates to macOS does not change Linux: every one of them keeps compiling, and running, exactly
//! as before there.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod agent_bridge;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod browser_client;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod offscreen;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod offscreen_driver;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod open_page_modal;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod page_db;
pub mod page_control;
pub mod page_url;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod pages;
#[cfg(target_os = "macos")]
mod spike_driver;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_view_item;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use web_view_item::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use offscreen::{OffscreenView, PageItemEvent, PageState, open_or_focus_page};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use open_page_modal::OpenPageModal;

gpui::actions!(
    web,
    [
        /// Asks for a URL and opens it as a page tab in the active pane.
        OpenUrl,
    ]
);

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
pub fn init(cx: &mut gpui::App) {
    // macOS has only the off-screen CEF backend for page tabs (issue #23 round 1): the decision
    // is to port the same backend Linux uses, not to build a second page system on WKWebView.
    // `web_view_item::init` (the WKWebView spike) is therefore never called on macOS any more;
    // `offscreen::init` keeps driving the `COD_SPIKE_URL`/`OpenSpikeUrl` developer comparison tool
    // itself, the same way it already does for Linux's off-screen backend.
    #[cfg(target_os = "macos")]
    {
        offscreen::init(cx);
        return;
    }
    #[cfg(target_os = "linux")]
    if offscreen::backend_requested() {
        offscreen::init(cx);
        return;
    }
    #[cfg(target_os = "linux")]
    if !linux::init_gtk(cx) {
        return;
    }
    #[cfg(target_os = "linux")]
    web_view_item::init(cx);
}
