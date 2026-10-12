use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::Context as _;
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable,
    MouseDownEvent, Pixels, Render, SharedString, Subscription, Task, Window, actions, canvas,
};
#[cfg(target_os = "macos")]
use objc2::{msg_send, runtime::AnyObject};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{ModalLayer, SplitDirection, Workspace, item::Item};
#[cfg(target_os = "macos")]
use wry::WebViewExtMacOS as _;
use wry::dpi::{LogicalPosition, LogicalSize};

#[cfg(target_os = "linux")]
use crate::linux;
#[cfg(target_os = "macos")]
use crate::spike_driver;

actions!(
    web_view,
    [
        /// Opens COD_SPIKE_URL, or a public GitLab merge request, in a new web view tab.
        OpenSpikeUrl,
        /// Cuts the page's selection.
        Cut,
        /// Copies the page's selection.
        Copy,
        /// Pastes into the page's focused field.
        Paste,
        /// Selects all in the page's focused field, or the whole page.
        SelectAll,
    ]
);

const DEFAULT_URL: &str = "https://gitlab.com/gitlab-org/gitlab-runner/-/merge_requests/6557";

/// WKWebView's default user agent lacks Safari's "Version/... Safari/..." suffix, and sites behind Cloudflare answer it with an interactive "Verify you are human" check.
/// The engine is Safari's, so the spike sends Safari's user agent; a real version would read it from the system.
#[cfg(target_os = "macos")]
const SAFARI_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.6 Safari/605.1.15";

/// A click in the page goes straight to the native view and GPUI never sees it.
/// The page reports every pointer press over IPC, so the item can take GPUI focus and Zed's keymap follows the page.
const FOCUS_SCRIPT: &str = r#"addEventListener('pointerdown', () => window.ipc.postMessage('cod:pointerdown'), true);"#;

static OPENED_STARTUP_URL: AtomicBool = AtomicBool::new(false);

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        workspace.register_action(|workspace, _: &OpenSpikeUrl, window, cx| {
            open_url(workspace, spike_url(), window, cx).log_err();
        });

        let Some(window) = window else {
            return;
        };
        if let Ok(url) = std::env::var("COD_SPIKE_URL")
            && !OPENED_STARTUP_URL.swap(true, Ordering::SeqCst)
        {
            #[cfg(target_os = "macos")]
            {
                let drive = std::env::var("COD_SPIKE_DRIVE").unwrap_or_default();
                cx.defer_in(window, move |workspace, window, cx| {
                    if drive == "keys" {
                        spike_driver::start_keys(url, window, cx);
                        return;
                    }
                    let Some(page) = open_url(workspace, url, window, cx).log_err() else {
                        return;
                    };
                    if drive == "1" {
                        spike_driver::start(page, window, cx);
                    }
                });
            }
            // Linux spike runs: wait for the files from the command line to open, then put the page in a split to the right of the editor.
            #[cfg(target_os = "linux")]
            cx.spawn_in(window, async move |workspace, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(3))
                    .await;
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        let page = open_url_in_split(
                            workspace,
                            url,
                            SplitDirection::Right,
                            window,
                            cx,
                        )
                        .log_err();
                        if let Some(page) = page
                            && std::env::var("COD_SPIKE_PROBE").is_ok_and(|value| value == "1")
                        {
                            linux::start_probe(Rc::downgrade(&page.read(cx).native), cx);
                        }
                    })
                    .log_err();
            })
            .detach();
        }
    })
    .detach();
}

fn spike_url() -> String {
    std::env::var("COD_SPIKE_URL").unwrap_or_else(|_| DEFAULT_URL.to_string())
}

pub fn open_url(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<WebView>> {
    let page = new_page(workspace, url, window, cx)?;
    workspace.add_item_to_active_pane(Box::new(page.clone()), None, true, window, cx);
    Ok(page)
}

pub fn open_url_in_split(
    workspace: &mut Workspace,
    url: String,
    direction: SplitDirection,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<WebView>> {
    let page = new_page(workspace, url, window, cx)?;
    workspace.split_item(direction, Box::new(page.clone()), window, cx);
    Ok(page)
}

fn new_page(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<WebView>> {
    let (events_tx, events_rx) = mpsc::unbounded();
    let native = NativeWebView::new(&url, window, events_tx)?;
    let modal_layer = workspace.modal_layer();
    Ok(cx.new(|cx| WebView::new(url, native, events_rx, modal_layer, window, cx)))
}

pub struct WebView {
    url: SharedString,
    title: Option<SharedString>,
    loaded_url: Option<String>,
    focus_handle: FocusHandle,
    native: Rc<NativeWebView>,
    _native_events: Task<()>,
    _subscriptions: [Subscription; 3],
}

enum NativeEvent {
    LoadStarted(String),
    LoadFinished(String),
    TitleChanged(String),
    PointerDown,
}

impl WebView {
    fn new(
        url: String,
        native: NativeWebView,
        mut events: mpsc::UnboundedReceiver<NativeEvent>,
        modal_layer: Entity<ModalLayer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let native_events = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.handle_native_event(event, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        // The native view sits outside GPUI's focus tree, so keyboard focus has to be handed to it (and taken back) explicitly when GPUI focus enters or leaves this item.
        let focus_handle = cx.focus_handle();
        let subscriptions = [
            cx.on_focus(&focus_handle, window, |this, _, _| {
                log::info!("web_view: GPUI focus in, native view becomes first responder");
                #[cfg(target_os = "macos")]
                this.native.webview.focus().log_err();
                #[cfg(target_os = "linux")]
                linux::focus_page(&this.native);
            }),
            cx.on_blur(&focus_handle, window, |this, _, _| {
                log::info!("web_view: GPUI focus out, GPUI view becomes first responder");
                #[cfg(target_os = "macos")]
                this.native.webview.focus_parent().log_err();
                #[cfg(target_os = "linux")]
                linux::focus_gpui(this.native.gpui_window);
            }),
            // The native view draws above everything GPUI paints, so it hides while a modal (palette, file finder) is open.
            cx.observe(&modal_layer, |this, modal_layer, cx| {
                let open = modal_layer.read(cx).has_active_modal();
                log::info!("web_view: modal layer changed, open={open}");
                this.native.set_covered(open);
            }),
        ];

        // A modal may already be open when the page is created (for example Zed's project trust prompt).
        native.set_covered(modal_layer.read(cx).has_active_modal());

        #[cfg(target_os = "linux")]
        linux::route_keys(&native, focus_handle.clone(), window.to_async(cx));

        Self {
            url: url.into(),
            title: None,
            loaded_url: None,
            focus_handle,
            native: Rc::new(native),
            _native_events: native_events,
            _subscriptions: subscriptions,
        }
    }

    fn handle_native_event(&mut self, event: NativeEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            NativeEvent::LoadStarted(url) => {
                log::info!("web_view: load started {url}");
                self.loaded_url = None;
            }
            NativeEvent::LoadFinished(url) => {
                log::info!(
                    "web_view: load finished {url} title={:?}",
                    self.title.as_deref().unwrap_or_default()
                );
                self.loaded_url = Some(url);
            }
            NativeEvent::TitleChanged(title) => {
                log::info!("web_view: title {title:?}");
                self.title = Some(title.into());
            }
            NativeEvent::PointerDown => {
                let was_focused = self.focus_handle.is_focused(window);
                log::info!("web_view: pointer down in the page, GPUI focus was on this item: {was_focused}");
                if !was_focused {
                    window.focus(&self.focus_handle, cx);
                }
            }
        }
        cx.notify();
    }

    pub fn loaded_url(&self) -> Option<&str> {
        self.loaded_url.as_deref()
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn native(&self) -> Rc<NativeWebView> {
        self.native.clone()
    }

    #[cfg(target_os = "linux")]
    fn edit_command(&mut self, command: EditCommand) {
        linux::edit_command(&self.native, command);
    }

    /// Sends a standard editing command (`copy:`, `paste:`, ...) to the native view, as AppKit's Edit menu would.
    #[cfg(target_os = "macos")]
    fn edit_command(&mut self, command: EditCommand) {
        let webview = self.native.webview.webview();
        let sender: *const AnyObject = std::ptr::null();
        log::info!("web_view: {command:?} sent to the native view");
        // SAFETY: WKWebView implements these NSResponder editing actions, and a nil sender is allowed.
        unsafe {
            match command {
                EditCommand::Cut => {
                    let _: () = msg_send![&*webview, cut: sender];
                }
                EditCommand::Copy => {
                    let _: () = msg_send![&*webview, copy: sender];
                }
                EditCommand::Paste => {
                    let _: () = msg_send![&*webview, paste: sender];
                }
                EditCommand::SelectAll => {
                    let _: () = msg_send![&*webview, selectAll: sender];
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum EditCommand {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

impl EventEmitter<()> for WebView {}

impl Focusable for WebView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for WebView {
    type Event = ();

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title.clone().unwrap_or_else(|| self.url.clone())
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some(self.url.clone())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn deactivated(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.native.set_shown(false);
    }

    fn on_removed(&self, _cx: &mut Context<Self>) {
        log::info!("web_view: item removed from its pane");
        self.native.set_shown(false);
    }
}

impl Render for WebView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let native = self.native.clone();
        div()
            .key_context("WebView")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Cut, _, _| this.edit_command(EditCommand::Cut)))
            .on_action(cx.listener(|this, _: &Copy, _, _| this.edit_command(EditCommand::Copy)))
            .on_action(cx.listener(|this, _: &Paste, _, _| this.edit_command(EditCommand::Paste)))
            .on_action(
                cx.listener(|this, _: &SelectAll, _, _| this.edit_command(EditCommand::SelectAll)),
            )
            .size_full()
            .child(
                canvas(
                    move |bounds, _window, _cx| native.place(bounds),
                    |_, _, window, _| {
                        // Spike diagnostics: which clicks GPUI itself receives.
                        window.on_mouse_event(|event: &MouseDownEvent, phase, window, _| {
                            log::info!(
                                "web_view: GPUI got a mouse down at {:?} phase={phase:?} default_prevented={}",
                                event.position,
                                window.default_prevented(),
                            );
                        });
                    },
                )
                .size_full(),
            )
    }
}

/// The OS web view, a child of the GPUI window's content view.
pub(crate) struct NativeWebView {
    pub(crate) webview: wry::WebView,
    frame: Cell<Option<Bounds<Pixels>>>,
    /// The item is the active one in its pane and has been laid out.
    shown: Cell<bool>,
    /// A GPUI modal is open over the window.
    covered: Cell<bool>,
    visible: Cell<bool>,
    /// GPUI's X11 window, which gets keyboard focus back when the item loses GPUI focus.
    #[cfg(target_os = "linux")]
    gpui_window: std::os::raw::c_ulong,
}

impl NativeWebView {
    fn new(
        url: &str,
        window: &Window,
        events: mpsc::UnboundedSender<NativeEvent>,
    ) -> anyhow::Result<Self> {
        let load_events = events.clone();
        let ipc_events = events.clone();
        let builder = wry::WebViewBuilder::new()
            .with_visible(false)
            // GPUI decides when the page gets keyboard focus.
            .with_focused(false)
            // Like the rest of Zed, the first click into an inactive window should reach the page.
            .with_accept_first_mouse(true);
        #[cfg(target_os = "macos")]
        let builder = builder.with_user_agent(SAFARI_USER_AGENT);
        #[cfg(target_os = "macos")]
        const INITIAL_SIZE: f64 = 0.0;
        #[cfg(target_os = "linux")]
        const INITIAL_SIZE: f64 = 1.0;
        let builder = builder
            .with_initialization_script(FOCUS_SCRIPT)
            .with_ipc_handler(move |request| {
                if request.body() == "cod:pointerdown" {
                    ipc_events
                        .unbounded_send(NativeEvent::PointerDown)
                        .log_err();
                }
            })
            .with_bounds(wry::Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                // XCreateSimpleWindow fails on a zero size, so Linux starts at 1x1.
                size: LogicalSize::new(INITIAL_SIZE, INITIAL_SIZE).into(),
            })
            .with_on_page_load_handler(move |event, url| {
                let event = match event {
                    wry::PageLoadEvent::Started => NativeEvent::LoadStarted(url),
                    wry::PageLoadEvent::Finished => NativeEvent::LoadFinished(url),
                };
                load_events.unbounded_send(event).log_err();
            })
            .with_document_title_changed_handler(move |title| {
                events
                    .unbounded_send(NativeEvent::TitleChanged(title))
                    .log_err();
            });

        // WKWebView's loadRequest does not load file URLs, so local pages are passed as HTML.
        let builder = if let Some(path) = url.strip_prefix("file://") {
            let html = std::fs::read_to_string(path)
                .with_context(|| format!("reading local page {path}"))?;
            builder.with_html(html)
        } else {
            builder.with_url(url)
        };

        #[cfg(target_os = "macos")]
        let webview = builder
            .build_as_child(window)
            .context("creating the OS web view")?;
        #[cfg(target_os = "linux")]
        let parent = linux::xlib_parent(window)?;
        #[cfg(target_os = "linux")]
        let webview = builder
            .build_as_child(&parent)
            .context("creating the OS web view")?;
        Ok(Self {
            webview,
            frame: Cell::new(None),
            shown: Cell::new(false),
            covered: Cell::new(false),
            visible: Cell::new(false),
            #[cfg(target_os = "linux")]
            gpui_window: parent.0,
        })
    }

    /// Moves the native view onto the bounds GPUI laid out for the item, in window coordinates.
    fn place(&self, bounds: Bounds<Pixels>) {
        if self.frame.get() != Some(bounds) {
            let x = f64::from(bounds.origin.x);
            let y = f64::from(bounds.origin.y);
            let width = f64::from(bounds.size.width);
            let height = f64::from(bounds.size.height);
            log::info!("web_view: frame x={x} y={y} width={width} height={height}");
            self.webview
                .set_bounds(wry::Rect {
                    position: LogicalPosition::new(x, y).into(),
                    size: LogicalSize::new(width, height).into(),
                })
                .log_err();
            self.frame.set(Some(bounds));
        }
        self.set_shown(true);
    }

    fn set_shown(&self, shown: bool) {
        self.shown.set(shown);
        self.update_visibility();
    }

    fn set_covered(&self, covered: bool) {
        // Spike switch to show the problem the hiding solves.
        if std::env::var("COD_SPIKE_KEEP_PAGE_OVER_MODALS").is_ok_and(|value| value == "1") {
            log::info!("web_view: modal open={covered}, page kept visible (COD_SPIKE_KEEP_PAGE_OVER_MODALS)");
            return;
        }
        if self.covered.get() != covered {
            log::info!("web_view: modal open={covered}");
            self.covered.set(covered);
            self.update_visibility();
        }
    }

    fn update_visibility(&self) {
        let visible = self.shown.get() && !self.covered.get();
        if self.visible.get() != visible {
            log::info!("web_view: visible={visible}");
            self.webview.set_visible(visible).log_err();
            self.visible.set(visible);
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn frame(&self) -> Option<Bounds<Pixels>> {
        self.frame.get()
    }
}

impl Drop for NativeWebView {
    fn drop(&mut self) {
        log::info!("web_view: native view dropped, wry removes it from the window");
    }
}
