//! `cod-browser`: the off-screen browser helper for Cod (issue #16).
//!
//! Cod starts this program, listens on a Unix socket and sends it commands (open a page, resize, mouse and key events).
//! The helper renders the pages with CEF without any window and sends every frame back as BGRA pixels, which Cod draws as GPUI images.
//! It lives in its own process so that the browser's libraries stay out of the editor and a browser crash does not take the editor with it.
//! It exits when the socket closes. CEF also starts its renderer, GPU and utility processes from
//! this same executable on Linux; on macOS it starts them from a packaged copy of this same
//! binary instead (`Cod Helper.app`, see `engine::helper_subprocess_path`), because macOS app
//! bundles need a separate, nested bundle for a CEF subprocess (issue #23 round 1).

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod engine;
#[cfg(target_os = "macos")]
mod mac_app;
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[allow(dead_code)]
mod protocol;

fn main() {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    engine::main();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    eprintln!("cod-browser only runs on Linux and macOS");
}
