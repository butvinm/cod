//! A workspace item that shows a web page through the operating system's web view.
//!
//! This is the Cod spike for issue #79: the page is a native child view laid over the GPUI window, kept on the bounds GPUI gives the item and hidden while its tab is not the active one.
//! macOS uses WKWebView; Linux uses WebKitGTK in an X11 child window (GPUI on X11 only).
//!
//! Issue #16 adds a second Linux backend, chosen with `COD_WEBVIEW_BACKEND=offscreen`: an off-screen browser (CEF, in the `cod-browser` helper process) whose frames GPUI paints as images, which works on native Wayland.

#[cfg(target_os = "linux")]
mod browser_client;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod offscreen;
#[cfg(target_os = "linux")]
mod offscreen_driver;
#[cfg(target_os = "linux")]
mod open_page_modal;
#[cfg(target_os = "linux")]
mod page_db;
pub mod page_url;
#[cfg(target_os = "linux")]
pub mod pages;
#[cfg(target_os = "macos")]
mod spike_driver;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_view_item;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use web_view_item::*;

#[cfg(target_os = "linux")]
pub use offscreen::{OffscreenView, PageItemEvent, PageState, open_or_focus_page};
#[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    if offscreen::backend_requested() {
        offscreen::init(cx);
        return;
    }
    #[cfg(target_os = "linux")]
    if !linux::init_gtk(cx) {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    web_view_item::init(cx);
}
