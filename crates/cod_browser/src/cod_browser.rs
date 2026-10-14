//! `cod-browser`: the off-screen browser helper for Cod (issue #16).
//!
//! Cod starts this program, listens on a Unix socket and sends it commands (open a page, resize, mouse and key events).
//! The helper renders the pages with CEF without any window and sends every frame back as BGRA pixels, which Cod draws as GPUI images.
//! It lives in its own process so that the browser's libraries stay out of the editor and a browser crash does not take the editor with it.
//! It exits when the socket closes. CEF also starts its renderer, GPU and utility processes from this same executable.

#[cfg(target_os = "linux")]
mod engine;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
mod protocol;

fn main() {
    #[cfg(target_os = "linux")]
    engine::main();
    #[cfg(not(target_os = "linux"))]
    eprintln!("cod-browser only runs on Linux");
}
