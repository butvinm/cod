//! A workspace item that shows a web page through the operating system's web view.
//!
//! This is the Cod spike for issue #79: the page is a native child view laid over the GPUI window, kept on the bounds GPUI gives the item and hidden while its tab is not the active one.
//! macOS uses WKWebView; Linux uses WebKitGTK in an X11 child window (GPUI on X11 only).
//!
//! Issue #16 adds a second Linux backend behind the `offscreen` feature and `COD_WEBVIEW_BACKEND=offscreen`: an off-screen browser (CEF) whose frames GPUI paints as images, which works on native Wayland.

#[cfg(all(target_os = "linux", feature = "offscreen"))]
mod cef_engine;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(all(target_os = "linux", feature = "offscreen"))]
mod offscreen;
#[cfg(all(target_os = "linux", feature = "offscreen"))]
mod offscreen_driver;
#[cfg(target_os = "macos")]
mod spike_driver;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_view_item;

#[cfg(all(target_os = "linux", feature = "offscreen"))]
pub use cef_engine::run_subprocess_if_requested;
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use web_view_item::*;

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
pub fn init(cx: &mut gpui::App) {
    #[cfg(target_os = "linux")]
    if std::env::var("COD_WEBVIEW_BACKEND").is_ok_and(|value| value == "offscreen") {
        #[cfg(feature = "offscreen")]
        {
            offscreen::init(cx);
            return;
        }
        #[cfg(not(feature = "offscreen"))]
        log::error!(
            "web_view: COD_WEBVIEW_BACKEND=offscreen needs a build with the offscreen feature, using the X11 backend"
        );
    }
    #[cfg(target_os = "linux")]
    if !linux::init_gtk(cx) {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    web_view_item::init(cx);
}
