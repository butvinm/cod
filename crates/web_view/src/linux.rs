//! Linux glue for the web view: GTK setup and its event loop, the X11 parent handle, key routing between WebKitGTK and GPUI, and a page probe for the spike.

use std::{
    os::raw::c_ulong,
    rc::Weak,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, bail};
use gpui::{App, AsyncWindowContext, FocusHandle, Keystroke, Window};
use gtk::{gdk, glib, prelude::*};
use webkit2gtk::WebViewExt as _;
use wry::{
    WebViewExtUnix as _,
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
};

use crate::web_view_item::{EditCommand, NativeWebView};

/// How often GTK's own event queue is drained.
/// GTK has its own X connection that GPUI's event loop does not watch, so the spike polls it from a GPUI task.
const GTK_PUMP_INTERVAL: Duration = Duration::from_millis(8);

/// Starts GTK on the main thread and keeps its event loop running from GPUI's foreground executor.
/// Returns false when GTK cannot start (no X display), and the web view is then not registered.
pub(crate) fn init_gtk(cx: &mut App) -> bool {
    // The page window is reparented into GPUI's X11 window, so GDK must use X11.
    gdk::set_allowed_backends("x11");
    if let Err(error) = gtk::init() {
        log::error!("web_view: GTK did not start, web views are disabled: {error}");
        return false;
    }
    log::info!("web_view: GTK started, pumping its events every {GTK_PUMP_INTERVAL:?}");
    cx.spawn(async move |cx| {
        loop {
            // No GPUI entity is borrowed here, so GTK callbacks may update GPUI synchronously.
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            cx.background_executor().timer(GTK_PUMP_INTERVAL).await;
        }
    })
    .detach();
    true
}

/// The GTK window that holds one page, reparented into GPUI's X11 window.
///
/// wry's own `build_as_child` on X11 wraps a plain X child window as a foreign GDK window.
/// GTK gives foreign windows no frame clock ("_gdk_frame_clock_freeze: assertion 'GDK_IS_FRAME_CLOCK (clock)' failed"), so the page never gets laid out or painted: the child stays black and WebKit's own window stays 1x1.
/// Instead the spike makes a normal GTK popup (override-redirect, its own frame clock), lets wry put WebKitGTK in it with `build_gtk`, and moves its X window under GPUI's window with XReparentWindow.
/// From then on GTK's move and resize calls are relative to GPUI's window.
pub(crate) struct Host {
    pub(crate) window: gtk::Window,
    pub(crate) container: gtk::Box,
    pub(crate) gpui_window: c_ulong,
}

impl Host {
    pub(crate) fn new(gpui: &Window) -> anyhow::Result<Self> {
        let gpui_window = gpui_xid(gpui)?;
        let window = gtk::Window::new(gtk::WindowType::Popup);
        window.set_default_size(1, 1);
        let container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&container);
        Ok(Self {
            window,
            container,
            gpui_window,
        })
    }

    /// Realizes the GTK window and moves it under GPUI's window. Call after wry has added the web view.
    pub(crate) fn attach(&self) -> anyhow::Result<()> {
        let display = gdk::Display::default().context("no GDK display")?;
        let x11_display = display
            .downcast_ref::<gdkx11::X11Display>()
            .context("GDK is not on X11")?;
        let xlib = x11_dl::xlib::Xlib::open().context("loading libX11")?;
        // SAFETY: the display is GDK's open X11 display and GPUI's window is live on it.
        let xdisplay = unsafe {
            gdkx11::ffi::gdk_x11_display_get_xdisplay(x11_display.as_ptr())
                as *mut x11_dl::xlib::Display
        };
        self.match_gpui_visual(&xlib, xdisplay);
        self.window.realize();
        let xid = self
            .window
            .window()
            .and_then(|window| window.downcast::<gdkx11::X11Window>().ok())
            .context("the GTK window has no X11 window")?
            .xid();
        // SAFETY: both windows are live on GDK's open X11 display.
        unsafe {
            (xlib.XReparentWindow)(xdisplay, xid, self.gpui_window, 0, 0);
            (xlib.XFlush)(xdisplay);
        }
        log::info!(
            "web_view: GTK window {xid:#x} reparented into GPUI window {:#x}",
            self.gpui_window
        );
        Ok(())
    }

    /// Gives the GTK window the visual of GPUI's window (a 32-bit ARGB one) before it is realized.
    /// GPUI picks a transparent visual, GTK's popup would take the screen default (24 bit).
    /// A child of another depth inside an ARGB parent can lose its content when the X server hands the parent to a compositor, as XWayland does (issue #5).
    /// Falls back to GDK's RGBA visual when the id cannot be looked up.
    fn match_gpui_visual(
        &self,
        xlib: &x11_dl::xlib::Xlib,
        xdisplay: *mut x11_dl::xlib::Display,
    ) {
        let Some(screen) = gdk::Screen::default()
            .and_then(|screen| screen.downcast::<gdkx11::X11Screen>().ok())
        else {
            return;
        };
        // SAFETY: zeroed attributes are filled in by the call; the window is GPUI's live one.
        let (visual_id, depth) = unsafe {
            let mut attributes: x11_dl::xlib::XWindowAttributes = std::mem::zeroed();
            if (xlib.XGetWindowAttributes)(xdisplay, self.gpui_window, &mut attributes) == 0 {
                (0, 0)
            } else {
                ((xlib.XVisualIDFromVisual)(attributes.visual), attributes.depth)
            }
        };
        let visual = (visual_id != 0)
            .then(|| screen.lookup_visual(visual_id))
            .flatten()
            .map(|visual| visual.upcast::<gdk::Visual>())
            .or_else(|| screen.rgba_visual());
        match visual {
            Some(visual) => {
                log::info!(
                    "web_view: GPUI window visual {visual_id:#x} depth {depth}; GTK window gets a visual of depth {}",
                    visual.depth()
                );
                self.window.set_visual(Some(&visual));
            }
            None => log::warn!(
                "web_view: GPUI window visual {visual_id:#x} depth {depth}; no matching GTK visual, the page window keeps the default"
            ),
        }
    }

    pub(crate) fn set_bounds(&self, x: i32, y: i32, width: i32, height: i32) {
        self.window.move_(x, y);
        self.window.resize(width.max(1), height.max(1));
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if visible {
            self.window.show_all();
        } else {
            self.window.hide();
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // SAFETY: GTK keeps toplevels alive on its own; the page is gone, so nothing else uses this window.
        unsafe { self.window.destroy() };
    }
}

/// GPUI's X11 window gives an XCB handle; the X window id is all the spike needs.
fn gpui_xid(window: &Window) -> anyhow::Result<c_ulong> {
    let handle = HasWindowHandle::window_handle(window)?;
    match handle.as_raw() {
        RawWindowHandle::Xcb(handle) => Ok(c_ulong::from(handle.window.get())),
        RawWindowHandle::Xlib(handle) => Ok(handle.window),
        other => bail!(
            "the web view needs an X11 window, GPUI gave {other:?} (GPUI on Wayland is not supported)"
        ),
    }
}

/// Gives the page X keyboard focus, so keys reach it without the pointer over it.
/// Without a window manager GDK does this with XSetInputFocus on the host window.
pub(crate) fn focus_page(native: &NativeWebView) {
    let webview = native.webview.webview();
    webview.grab_focus();
    if let Some(container) = webview.toplevel().and_then(|toplevel| toplevel.window()) {
        container.focus(0);
    }
}

/// Hands X keyboard focus back to GPUI's window.
/// wry's `focus_parent` would focus the host window instead, which keeps every key in the page.
pub(crate) fn focus_gpui(gpui_window: c_ulong) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let Some(x11_display) = display.downcast_ref::<gdkx11::X11Display>() else {
        return;
    };
    // SAFETY: the display is GDK's open X11 display and the id is GPUI's live top-level window; GDK takes ownership of the returned wrapper.
    let window: gdk::Window = unsafe {
        glib::translate::from_glib_full(gdkx11::ffi::gdk_x11_window_foreign_new_for_display(
            x11_display.as_ptr(),
            gpui_window,
        ))
    };
    window.focus(0);
}

/// Sends a WebKit editing command, as GTK's own Ctrl-C/V handling would.
pub(crate) fn edit_command(native: &NativeWebView, command: EditCommand) {
    let name = match command {
        EditCommand::Cut => "Cut",
        EditCommand::Copy => "Copy",
        EditCommand::Paste => "Paste",
        EditCommand::SelectAll => "SelectAll",
    };
    log::info!("web_view: {command:?} sent to WebKitGTK as {name}");
    native.webview.webview().execute_editing_command(name);
}

/// Key presses in the page go to GTK, not GPUI.
/// While the page has GPUI focus, every key with Ctrl, Alt or Super is offered to GPUI first, so Zed's keymap (Ctrl-W, the palette, and the `WebView` context's Ctrl-C/V) works over the page; keys GPUI does not handle, and all plain typing, stay with the page.
/// While GPUI focus is elsewhere, X can still deliver keys to the page: the X rule sends keys to the window under the pointer when it is inside the focus window. Those keys belong to GPUI, so all of them are passed on and the page gets none.
pub(crate) fn route_keys(native: &NativeWebView, focus_handle: FocusHandle, cx: AsyncWindowContext) {
    native
        .webview
        .webview()
        .connect_key_press_event(move |_, event| {
            let mut cx = cx.clone();
            let page_focused = cx
                .update(|window, _| focus_handle.is_focused(window))
                .unwrap_or(false);
            let Some(keystroke) = gpui_keystroke(event, !page_focused) else {
                return if page_focused {
                    glib::Propagation::Proceed
                } else {
                    glib::Propagation::Stop
                };
            };
            let handled = cx
                .update(|window, cx| window.dispatch_keystroke(keystroke.clone(), cx))
                .unwrap_or(false);
            log::info!(
                "web_view: page key {keystroke} passed to GPUI (page has GPUI focus: {page_focused}), handled={handled}"
            );
            if handled || !page_focused {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
}

/// Converts a GDK key press to a GPUI keystroke; plain keys only when `all_keys` is set.
fn gpui_keystroke(event: &gdk::EventKey, all_keys: bool) -> Option<Keystroke> {
    let state = event.state();
    let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = state.contains(gdk::ModifierType::MOD1_MASK);
    let platform = state.contains(gdk::ModifierType::SUPER_MASK);
    if !(ctrl || alt || platform || all_keys) {
        return None;
    }
    let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
    let keyval = event.keyval();
    let name = keyval.name()?.to_string();
    let key = match name.as_str() {
        "Return" | "KP_Enter" => "enter".to_string(),
        "Tab" | "ISO_Left_Tab" => "tab".to_string(),
        "Escape" => "escape".to_string(),
        "BackSpace" => "backspace".to_string(),
        "Delete" => "delete".to_string(),
        "Left" => "left".to_string(),
        "Right" => "right".to_string(),
        "Up" => "up".to_string(),
        "Down" => "down".to_string(),
        "Page_Up" => "pageup".to_string(),
        "Page_Down" => "pagedown".to_string(),
        "Home" => "home".to_string(),
        "End" => "end".to_string(),
        "space" => "space".to_string(),
        _ => {
            let character = keyval.to_lower().to_unicode()?;
            if character.is_control() {
                return None;
            }
            character.to_string()
        }
    };
    let mut source = String::new();
    if ctrl {
        source.push_str("ctrl-");
    }
    if alt {
        source.push_str("alt-");
    }
    if platform {
        source.push_str("super-");
    }
    if shift {
        source.push_str("shift-");
    }
    source.push_str(&key);
    Keystroke::parse(&source).ok()
}

/// Spike evidence on Linux: logs the page state (URL, scroll, focused field and its value, selection) whenever it changes.
/// Enabled with COD_SPIKE_PROBE=1.
pub(crate) fn start_probe(native: Weak<NativeWebView>, cx: &mut App) {
    const PROBE: &str = r#"JSON.stringify({href: location.href, scrollY: Math.round(scrollY), title: document.title.slice(0, 60), active: document.activeElement ? document.activeElement.tagName + (document.activeElement.id ? '#' + document.activeElement.id : '') + (document.activeElement.getAttribute('aria-label') ? '[' + document.activeElement.getAttribute('aria-label') + ']' : '') : null, value: document.activeElement && 'value' in document.activeElement ? document.activeElement.value : null, selection: String(getSelection()).slice(0, 60)})"#;
    let last = Arc::new(Mutex::new(String::new()));
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let Some(native) = native.upgrade() else {
                log::info!("web_view: probe stopped, the page is gone");
                break;
            };
            let last = last.clone();
            let result = native.webview.evaluate_script_with_callback(PROBE, move |state| {
                let mut last = last.lock().unwrap_or_else(|error| error.into_inner());
                if *last != state {
                    log::info!("web_view: probe {state}");
                    *last = state;
                }
            });
            if result.is_err() {
                break;
            }
        }
    })
    .detach();
}
