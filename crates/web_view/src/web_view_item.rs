use std::{
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

use anyhow::Context as _;
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Render,
    SharedString, Subscription, Task, Window, actions, canvas,
};
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{Workspace, item::Item};
use wry::dpi::{LogicalPosition, LogicalSize};

use crate::spike_driver;

actions!(
    web_view,
    [
        /// Opens COD_SPIKE_URL, or a public GitLab merge request, in a new web view tab.
        OpenSpikeUrl,
    ]
);

const DEFAULT_URL: &str = "https://gitlab.com/gitlab-org/gitlab-runner/-/merge_requests/6557";

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
            cx.defer_in(window, move |workspace, window, cx| {
                let Some(page) = open_url(workspace, url, window, cx).log_err() else {
                    return;
                };
                if std::env::var("COD_SPIKE_DRIVE").is_ok_and(|value| value == "1") {
                    spike_driver::start(page, window, cx);
                }
            });
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
    let (events_tx, events_rx) = mpsc::unbounded();
    let native = NativeWebView::new(&url, window, events_tx)?;
    let page = cx.new(|cx| WebView::new(url, native, events_rx, window, cx));
    workspace.add_item_to_active_pane(Box::new(page.clone()), None, true, window, cx);
    Ok(page)
}

pub struct WebView {
    url: SharedString,
    title: Option<SharedString>,
    loaded_url: Option<String>,
    focus_handle: FocusHandle,
    native: Rc<NativeWebView>,
    _native_events: Task<()>,
    _focus_subscriptions: [Subscription; 2],
}

enum NativeEvent {
    LoadStarted(String),
    LoadFinished(String),
    TitleChanged(String),
}

impl WebView {
    fn new(
        url: String,
        native: NativeWebView,
        mut events: mpsc::UnboundedReceiver<NativeEvent>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let native_events = cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update(cx, |this, cx| this.handle_native_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        // The native view sits outside GPUI's focus tree, so keyboard focus has to be handed to it (and taken back) explicitly when GPUI focus enters or leaves this item.
        let focus_handle = cx.focus_handle();
        let focus_subscriptions = [
            cx.on_focus(&focus_handle, window, |this, _, _| {
                this.native.webview.focus().log_err();
            }),
            cx.on_blur(&focus_handle, window, |this, _, _| {
                this.native.webview.focus_parent().log_err();
            }),
        ];

        Self {
            url: url.into(),
            title: None,
            loaded_url: None,
            focus_handle,
            native: Rc::new(native),
            _native_events: native_events,
            _focus_subscriptions: focus_subscriptions,
        }
    }

    fn handle_native_event(&mut self, event: NativeEvent, cx: &mut Context<Self>) {
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
        }
        cx.notify();
    }

    pub fn loaded_url(&self) -> Option<&str> {
        self.loaded_url.as_deref()
    }

    pub(crate) fn native(&self) -> Rc<NativeWebView> {
        self.native.clone()
    }
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
        self.native.set_visible(false);
    }

    fn on_removed(&self, _cx: &mut Context<Self>) {
        self.native.set_visible(false);
    }
}

impl Render for WebView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let native = self.native.clone();
        div().track_focus(&self.focus_handle).size_full().child(
            canvas(
                move |bounds, _window, _cx| native.place(bounds),
                |_, _, _, _| {},
            )
            .size_full(),
        )
    }
}

/// The OS web view, a child of the GPUI window's content view.
pub(crate) struct NativeWebView {
    pub(crate) webview: wry::WebView,
    frame: Cell<Option<Bounds<Pixels>>>,
    visible: Cell<bool>,
}

impl NativeWebView {
    fn new(
        url: &str,
        window: &Window,
        events: mpsc::UnboundedSender<NativeEvent>,
    ) -> anyhow::Result<Self> {
        let load_events = events.clone();
        let builder = wry::WebViewBuilder::new()
            .with_visible(false)
            .with_bounds(wry::Rect {
                position: LogicalPosition::new(0.0, 0.0).into(),
                size: LogicalSize::new(0.0, 0.0).into(),
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

        let webview = builder
            .build_as_child(window)
            .context("creating the OS web view")?;
        Ok(Self {
            webview,
            frame: Cell::new(None),
            visible: Cell::new(false),
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
        self.set_visible(true);
    }

    fn set_visible(&self, visible: bool) {
        if self.visible.get() != visible {
            log::info!("web_view: visible={visible}");
            self.webview.set_visible(visible).log_err();
            self.visible.set(visible);
        }
    }

    pub(crate) fn frame(&self) -> Option<Bounds<Pixels>> {
        self.frame.get()
    }
}
