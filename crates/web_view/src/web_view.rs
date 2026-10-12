//! A workspace item that shows a web page through the operating system's web view.
//!
//! This is the Cod spike for issue #79: the page is a native child view (WKWebView via wry) laid over the GPUI window, kept on the bounds GPUI gives the item and hidden while its tab is not the active one.
//! Only macOS is wired up for now.

#[cfg(target_os = "macos")]
mod spike_driver;
#[cfg(target_os = "macos")]
mod web_view_item;

#[cfg(target_os = "macos")]
pub use web_view_item::*;

#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn init(cx: &mut gpui::App) {
    #[cfg(target_os = "macos")]
    web_view_item::init(cx);
}
