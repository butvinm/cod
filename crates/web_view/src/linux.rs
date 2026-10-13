//! Linux glue for the web view: GTK setup and its event loop, the X11 parent handle, key routing between WebKitGTK and GPUI, and a page probe for the spike.

use std::{
    cell::{Cell, RefCell},
    os::raw::c_ulong,
    rc::{Rc, Weak},
    sync::{Arc, Mutex, OnceLock},
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
    log::info!(
        "web_view: placement {:?} (set COD_WEBVIEW_PLACEMENT=overlay or child)",
        placement()
    );
    log_xwayland();
    cx.spawn(async move |cx| {
        let mut tick = 0u32;
        loop {
            tick = tick.wrapping_add(1);
            sync_overlays(tick);
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

/// Logs whether the X server is XWayland (it has the XWAYLAND extension), to help read logs from other desktops.
fn log_xwayland() {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let Some(x11_display) = display.downcast_ref::<gdkx11::X11Display>() else {
        log::info!("web_view: GDK is not on X11");
        return;
    };
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else {
        return;
    };
    // SAFETY: the display is GDK's open X11 display; the vendor string is owned by Xlib and read at once.
    unsafe {
        let xdisplay = gdkx11::ffi::gdk_x11_display_get_xdisplay(x11_display.as_ptr())
            as *mut x11_dl::xlib::Display;
        let (mut opcode, mut event, mut error) = (0, 0, 0);
        let extension = (xlib.XQueryExtension)(
            xdisplay,
            c"XWAYLAND".as_ptr(),
            &mut opcode,
            &mut event,
            &mut error,
        ) != 0;
        let vendor = std::ffi::CStr::from_ptr((xlib.XServerVendor)(xdisplay)).to_string_lossy();
        let xwayland = extension || vendor.to_uppercase().contains("XWAYLAND");
        log::info!(
            "web_view: XWayland detected={xwayland} (XWAYLAND extension={extension}, server vendor {vendor:?})"
        );
    }
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
    /// Set in overlay placement: the window is not reparented and follows the pane instead.
    overlay: RefCell<Option<Rc<Overlay>>>,
}

/// Where the page window lives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Placement {
    /// A child X window of GPUI's window (the default).
    Child,
    /// A separate override-redirect top-level window kept over the pane.
    /// Meant for desktops where the child window is not composited into GPUI's window (XWayland, issue #5).
    Overlay,
}

/// Read once from COD_WEBVIEW_PLACEMENT (`child` or `overlay`).
pub(crate) fn placement() -> Placement {
    static PLACEMENT: OnceLock<Placement> = OnceLock::new();
    *PLACEMENT.get_or_init(|| match std::env::var("COD_WEBVIEW_PLACEMENT").as_deref() {
        Ok("overlay") => Placement::Overlay,
        _ => Placement::Child,
    })
}

thread_local! {
    static OVERLAYS: RefCell<Vec<Weak<Overlay>>> = const { RefCell::new(Vec::new()) };
}

/// Keeps every overlay window over its pane. Called from the GTK pump; `tick` counts pump rounds.
fn sync_overlays(tick: u32) {
    // The stacking order is a round trip with a list in it, so it is read every few rounds only.
    let query_stack = tick % 6 == 0;
    OVERLAYS.with(|overlays| {
        let mut overlays = overlays.borrow_mut();
        overlays.retain(|overlay| overlay.strong_count() > 0);
        for overlay in overlays.iter().filter_map(Weak::upgrade) {
            overlay.sync(query_stack);
        }
    });
}

/// A page window that is a top-level override-redirect window placed in root coordinates over the pane.
/// Its position is GPUI's window origin on the screen (XTranslateCoordinates) plus the pane's offset.
/// It is hidden while GPUI's window is not viewable or while another top-level window above GPUI's overlaps the pane, and it is restacked right above GPUI's top-level window.
struct Overlay {
    window: gtk::Window,
    xid: c_ulong,
    gpui_window: c_ulong,
    xlib: Rc<x11_dl::xlib::Xlib>,
    xdisplay: *mut x11_dl::xlib::Display,
    /// Pane bounds in GPUI window coordinates: x, y, width, height.
    frame: Cell<(i32, i32, i32, i32)>,
    want_visible: Cell<bool>,
    shown: Cell<bool>,
    /// The window that covers the pane, so it is logged once per change.
    covering: Cell<Option<c_ulong>>,
    gdk_display: *mut gdkx11::ffi::GdkX11Display,
    last_position: Cell<Option<(i32, i32, i32, i32)>>,
}

impl Overlay {
    fn sync(&self, query_stack: bool) {
        let want = self.want_visible.get();
        // Reads GPUI's window state and, now and then, the stacking order. A failed or trapped request counts as "GPUI's window is not viewable", which hides the overlay until the next good read.
        let probe = if want {
            self.trapped(|| self.probe(query_stack))
        } else {
            None
        };
        let viewable = probe.is_some();
        let mut raise_above = None;
        let mut position = (0, 0, 1, 1);
        if let Some(probe) = &probe {
            let (frame_x, frame_y, width, height) = self.frame.get();
            position = (probe.root_x + frame_x, probe.root_y + frame_y, width, height);
            if let Some(stacking) = &probe.stacking {
                if stacking.covering != self.covering.get() {
                    if let Some(window) = stacking.covering {
                        log::info!("web_view: overlay covered by window {window:#x}");
                    } else {
                        log::info!("web_view: overlay no longer covered");
                    }
                    self.covering.set(stacking.covering);
                }
                raise_above = stacking.raise_above;
            }
        }
        let show = want && viewable && self.covering.get().is_none();
        if show != self.shown.get() {
            log::info!("web_view: overlay shown={show} (gpui viewable={viewable})");
            if show {
                self.window.show_all();
            } else {
                self.window.hide();
            }
            self.shown.set(show);
            self.last_position.set(None);
        }
        if !show {
            return;
        }
        if self.last_position.get() != Some(position) {
            self.window.move_(position.0, position.1);
            self.window.resize(position.2, position.3);
            self.last_position.set(Some(position));
        }
        if let Some(sibling) = raise_above {
            self.trapped(|| {
                use x11_dl::xlib;
                // SAFETY: the display is GDK's open X11 display; a window that vanished makes a trapped X error, not an abort.
                unsafe {
                    let mut changes: xlib::XWindowChanges = std::mem::zeroed();
                    changes.sibling = sibling;
                    changes.stack_mode = xlib::Above;
                    (self.xlib.XConfigureWindow)(
                        self.xdisplay,
                        self.xid,
                        (xlib::CWSibling | xlib::CWStackMode) as u32,
                        &mut changes,
                    );
                    (self.xlib.XFlush)(self.xdisplay);
                }
                Some(())
            });
        }
    }

    /// Runs X requests with GDK's error trap, so a window that disappears between two requests cannot abort the app.
    /// Returns None when the closure did, or when X reported an error for any request in it.
    fn trapped<T>(&self, requests: impl FnOnce() -> Option<T>) -> Option<T> {
        // SAFETY: the GDK display is the live default one; push and pop are paired.
        unsafe { gdkx11::ffi::gdk_x11_display_error_trap_push(self.gdk_display) };
        let result = requests();
        // SAFETY: as above; pop flushes and returns the first trapped error code, or 0.
        let error = unsafe { gdkx11::ffi::gdk_x11_display_error_trap_pop(self.gdk_display) };
        if error != 0 {
            log::debug!("web_view: overlay X request failed with error {error}");
            return None;
        }
        result
    }

    /// Reads GPUI's window: None when it cannot be read or is not viewable.
    fn probe(&self, query_stack: bool) -> Option<Probe> {
        use x11_dl::xlib;
        let xlib_api = &*self.xlib;
        // SAFETY: called inside the error trap on GDK's open X11 display; every out-parameter is a local.
        unsafe {
            let mut attributes: xlib::XWindowAttributes = std::mem::zeroed();
            if (xlib_api.XGetWindowAttributes)(self.xdisplay, self.gpui_window, &mut attributes)
                == 0
                || attributes.map_state != xlib::IsViewable
            {
                return None;
            }
            let (mut root_x, mut root_y) = (0, 0);
            let mut child = 0;
            if (xlib_api.XTranslateCoordinates)(
                self.xdisplay,
                self.gpui_window,
                attributes.root,
                0,
                0,
                &mut root_x,
                &mut root_y,
                &mut child,
            ) == 0
            {
                return None;
            }
            let (frame_x, frame_y, width, height) = self.frame.get();
            let pane = (root_x + frame_x, root_y + frame_y, width, height);
            let stacking = if query_stack {
                // A failed stacking read hides the overlay rather than guessing.
                Some(self.stacking(attributes.root, pane)?)
            } else {
                None
            };
            Some(Probe {
                root_x,
                root_y,
                stacking,
            })
        }
    }

    /// Finds the window above GPUI's top-level that overlaps the pane, if any, and the window to restack above when the overlay is not directly above GPUI's top-level yet.
    /// None when the tree cannot be read.
    ///
    /// # Safety
    /// Call inside the error trap, on the live display.
    #[allow(unsafe_op_in_unsafe_fn)]
    unsafe fn stacking(&self, root: c_ulong, pane: (i32, i32, i32, i32)) -> Option<Stacking> {
        use x11_dl::xlib;
        /// Real window trees are a few levels deep; more means the tree changed under us.
        const MAX_DEPTH: usize = 16;
        let xlib_api = &*self.xlib;
        // The child of the root that contains GPUI's window (the window manager's frame, or GPUI's window itself).
        let mut top = self.gpui_window;
        let mut reached_root = false;
        for _ in 0..MAX_DEPTH {
            let (mut root_return, mut parent) = (0, 0);
            let mut children = std::ptr::null_mut();
            let mut count = 0;
            if (xlib_api.XQueryTree)(
                self.xdisplay,
                top,
                &mut root_return,
                &mut parent,
                &mut children,
                &mut count,
            ) == 0
            {
                return None;
            }
            if !children.is_null() {
                (xlib_api.XFree)(children.cast());
            }
            if parent == root {
                reached_root = true;
                break;
            }
            if parent == 0 {
                return None;
            }
            top = parent;
        }
        if !reached_root {
            return None;
        }
        let (mut root_return, mut parent) = (0, 0);
        let mut children = std::ptr::null_mut();
        let mut count = 0;
        if (xlib_api.XQueryTree)(
            self.xdisplay,
            root,
            &mut root_return,
            &mut parent,
            &mut children,
            &mut count,
        ) == 0
        {
            return None;
        }
        let list: Vec<c_ulong> = if children.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(children, count as usize).to_vec()
        };
        if !children.is_null() {
            (xlib_api.XFree)(children.cast());
        }
        let top_index = list.iter().position(|window| *window == top)?;
        let own_index = list.iter().position(|window| *window == self.xid);
        let mut covering = None;
        for window in &list[top_index + 1..] {
            if *window == self.xid {
                continue;
            }
            let mut attributes: xlib::XWindowAttributes = std::mem::zeroed();
            // A window that vanished since the tree was read fails here; it is simply not a cover.
            if (xlib_api.XGetWindowAttributes)(self.xdisplay, *window, &mut attributes) == 0
                || attributes.map_state != xlib::IsViewable
                || attributes.class != xlib::InputOutput
                // Override-redirect windows are menus and tooltips; the page's own tooltips are among them and must not hide the page.
                || attributes.override_redirect != 0
            {
                continue;
            }
            let overlaps = attributes.x < pane.0 + pane.2
                && attributes.x + attributes.width > pane.0
                && attributes.y < pane.1 + pane.3
                && attributes.y + attributes.height > pane.1;
            if overlaps {
                covering = Some(*window);
                break;
            }
        }
        let raise_above = match own_index {
            Some(index) if index == top_index + 1 => None,
            _ => Some(top),
        };
        Some(Stacking {
            covering,
            raise_above,
        })
    }
}

/// What one read of GPUI's window found.
struct Probe {
    /// GPUI window origin in root coordinates.
    root_x: i32,
    root_y: i32,
    /// Present on the rounds that read the stacking order.
    stacking: Option<Stacking>,
}

struct Stacking {
    /// A normal top-level window above GPUI's that overlaps the pane.
    covering: Option<c_ulong>,
    /// Restack the overlay above this window.
    raise_above: Option<c_ulong>,
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
            overlay: RefCell::new(None),
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
        if placement() == Placement::Child {
            self.match_gpui_visual(x11_display, &xlib, xdisplay);
        }
        self.window.realize();
        let xid = self
            .window
            .window()
            .and_then(|window| window.downcast::<gdkx11::X11Window>().ok())
            .context("the GTK window has no X11 window")?
            .xid();
        if placement() == Placement::Overlay {
            let overlay = Rc::new(Overlay {
                window: self.window.clone(),
                xid,
                gpui_window: self.gpui_window,
                xlib: Rc::new(xlib),
                xdisplay,
                frame: Cell::new((0, 0, 1, 1)),
                want_visible: Cell::new(false),
                shown: Cell::new(false),
                covering: Cell::new(None),
                gdk_display: x11_display.as_ptr(),
                last_position: Cell::new(None),
            });
            OVERLAYS.with(|overlays| overlays.borrow_mut().push(Rc::downgrade(&overlay)));
            *self.overlay.borrow_mut() = Some(overlay);
            log::info!(
                "web_view: GTK window {xid:#x} stays a top-level overlay above GPUI window {:#x}",
                self.gpui_window
            );
            return Ok(());
        }
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
        x11_display: &gdkx11::X11Display,
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
            // A trapped X error (the window vanished) must not abort the app.
            gdkx11::ffi::gdk_x11_display_error_trap_push(x11_display.as_ptr());
            let mut attributes: x11_dl::xlib::XWindowAttributes = std::mem::zeroed();
            let found =
                (xlib.XGetWindowAttributes)(xdisplay, self.gpui_window, &mut attributes) != 0;
            let error = gdkx11::ffi::gdk_x11_display_error_trap_pop(x11_display.as_ptr());
            if found && error == 0 {
                ((xlib.XVisualIDFromVisual)(attributes.visual), attributes.depth)
            } else {
                (0, 0)
            }
        };
        let visual = (visual_id != 0)
            .then(|| screen.lookup_visual(visual_id))
            .flatten()
            .map(|visual| visual.upcast::<gdk::Visual>())
            .or_else(|| screen.upcast_ref::<gdk::Screen>().rgba_visual());
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
        if let Some(overlay) = self.overlay.borrow().as_ref() {
            overlay.frame.set((x, y, width.max(1), height.max(1)));
            overlay.sync(true);
            return;
        }
        self.window.move_(x, y);
        self.window.resize(width.max(1), height.max(1));
    }

    pub(crate) fn set_visible(&self, visible: bool) {
        if let Some(overlay) = self.overlay.borrow().as_ref() {
            overlay.want_visible.set(visible);
            overlay.sync(true);
            return;
        }
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
