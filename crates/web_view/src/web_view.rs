//! A workspace item that shows a web page through the operating system's web view.
//!
//! This is the Cod spike for issue #79: the page is a native child view laid over the GPUI window, kept on the bounds GPUI gives the item and hidden while its tab is not the active one.
//! macOS uses WKWebView; Linux uses WebKitGTK in an X11 child window (GPUI on X11 only).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod spike_driver;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod web_view_item;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use web_view_item::*;

#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    allow(unused_variables)
)]
pub fn init(cx: &mut gpui::App) {
    #[cfg(target_os = "linux")]
    if !linux::init_gtk(cx) {
        return;
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    web_view_item::init(cx);
}
