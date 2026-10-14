//! A workspace item that shows a web page drawn by an off-screen browser (issue #16).
//!
//! The browser (CEF, in the `cod-browser` helper process, see `browser_client`) renders into memory. Every frame becomes a fresh `RenderImage` that GPUI paints inside the pane like any other image, so the page is part of Cod's own window and works on native Wayland.
//! Mouse and keyboard events that GPUI delivers to the pane are forwarded to the browser.

use std::{cell::Cell, rc::Rc, sync::Arc};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, Bounds, Context, DispatchPhase, Entity, EventEmitter, FocusHandle, Focusable,
    HitboxBehavior, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Point, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, canvas, div, img, px,
};
use smallvec::SmallVec;
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{SplitDirection, Workspace, item::Item};

use crate::browser_client::{
    Page, PageEvent, PageFrame,
    protocol::{
        self, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, EVENT_ALT, EVENT_COMMAND, EVENT_CONTROL,
        EVENT_LEFT_BUTTON, EVENT_MIDDLE_BUTTON, EVENT_RIGHT_BUTTON, EVENT_SHIFT,
    },
};

/// Pixels one wheel "line" scrolls, for devices that report lines.
const WHEEL_LINE_PIXELS: f32 = 40.0;

pub(crate) fn backend_requested() -> bool {
    std::env::var("COD_WEBVIEW_BACKEND").is_ok_and(|value| value == "offscreen")
}

pub(crate) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, window, cx| {
        workspace.register_action(|workspace, _: &crate::web_view_item::OpenSpikeUrl, window, cx| {
            let url = std::env::var("COD_SPIKE_URL")
                .unwrap_or_else(|_| "https://en.wikipedia.org/wiki/Levenshtein_distance".into());
            open_url_in_split(workspace, url, SplitDirection::Right, window, cx).log_err();
        });

        let Some(window) = window else {
            return;
        };
        let Ok(url) = std::env::var("COD_SPIKE_URL") else {
            return;
        };
        static OPENED_STARTUP_URL: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if OPENED_STARTUP_URL.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let drive = std::env::var("COD_SPIKE_DRIVE").is_ok_and(|value| value == "offscreen");
        // Wait for the files from the command line to open, then put the page in a split to the right of the editor.
        cx.spawn_in(window, async move |workspace, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            workspace
                .update_in(cx, |workspace, window, cx| {
                    let Some(page) =
                        open_url_in_split(workspace, url, SplitDirection::Right, window, cx)
                            .log_err()
                    else {
                        return;
                    };
                    if drive {
                        crate::offscreen_driver::start(page, window, cx);
                    }
                })
                .log_err();
        })
        .detach();
    })
    .detach();
}

pub fn open_url_in_split(
    workspace: &mut Workspace,
    url: String,
    direction: SplitDirection,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<OffscreenView>> {
    let page = cx.new(|cx| OffscreenView::new(url, window, cx));
    let view = page.read(cx);
    if let Some(error) = view.start_error.clone() {
        anyhow::bail!("{error}");
    }
    workspace.split_item(direction, Box::new(page.clone()), window, cx);
    Ok(page)
}

pub(crate) fn open_url_in_active_pane(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<OffscreenView>> {
    let page = cx.new(|cx| OffscreenView::new(url, window, cx));
    if let Some(error) = page.read(cx).start_error.clone() {
        anyhow::bail!("{error}");
    }
    workspace.add_item_to_active_pane(Box::new(page.clone()), None, true, window, cx);
    Ok(page)
}

pub struct OffscreenView {
    url: SharedString,
    title: Option<SharedString>,
    loading: bool,
    page: Option<Rc<Page>>,
    start_error: Option<String>,
    image: Option<Arc<RenderImage>>,
    focus_handle: FocusHandle,
    pane_bounds: Rc<Cell<Bounds<Pixels>>>,
    hidden: Rc<Cell<bool>>,
    console: Vec<String>,
    pub(crate) frames_painted: u64,
    /// Total time spent turning frames into images, for the frame cost report.
    pub(crate) image_micros: u64,
    pub(crate) last_frame_size: (u32, u32),
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl OffscreenView {
    fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (events_tx, mut events_rx) = mpsc::unbounded();
        let scale = window.scale_factor();
        let window_size = window.viewport_size();
        let (page, start_error) = match Page::new(
            &url,
            (f32::from(window_size.width) / 2.0) as i32,
            f32::from(window_size.height) as i32,
            scale,
            paths::data_dir(),
            events_tx,
        ) {
            Ok(page) => (Some(Rc::new(page)), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };

        let events = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events_rx.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.handle_event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });

        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus(&focus_handle, window, |this, _, _| {
                log::info!("web_view: GPUI focus in, the page gets keyboard focus");
                if let Some(page) = &this.page {
                    page.set_focus(true);
                }
            }),
            cx.on_blur(&focus_handle, window, |this, _, _| {
                log::info!("web_view: GPUI focus out");
                if let Some(page) = &this.page {
                    page.set_focus(false);
                }
            }),
            cx.on_release(|this, cx| {
                if let Some(image) = this.image.take() {
                    cx.drop_image(image, None);
                }
                if let Some(page) = &this.page {
                    log::info!("web_view: item released, closing its browser view");
                    page.close();
                }
            }),
        ];

        Self {
            url: url.into(),
            title: None,
            loading: true,
            page,
            start_error,
            image: None,
            focus_handle,
            pane_bounds: Rc::new(Cell::new(Bounds::default())),
            hidden: Rc::new(Cell::new(false)),
            console: Vec::new(),
            frames_painted: 0,
            image_micros: 0,
            last_frame_size: (0, 0),
            _events: events,
            _subscriptions: subscriptions,
        }
    }

    fn handle_event(&mut self, event: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            PageEvent::Frame => {
                let Some(frame) = self.page.as_ref().and_then(|page| page.take_frame()) else {
                    return;
                };
                self.show_frame(frame, window, cx);
            }
            PageEvent::Title(title) => {
                log::info!("web_view: title {title:?}");
                self.title = Some(title.into());
            }
            PageEvent::Address(url) => {
                log::info!("web_view: address {url}");
                self.url = url.into();
            }
            PageEvent::Loading(loading) => {
                log::info!(
                    "web_view: loading={loading} title={:?}",
                    self.title.as_deref().unwrap_or_default()
                );
                self.loading = loading;
            }
            PageEvent::Console(message) => {
                log::info!("web_view: page console: {message}");
                self.console.push(message);
                if self.console.len() > 200 {
                    self.console.remove(0);
                }
            }
            PageEvent::Created => {
                log::info!("web_view: browser view created");
                if let Some(page) = &self.page {
                    page.set_focus(self.focus_handle.is_focused(window));
                }
            }
            PageEvent::Closed => {}
        }
        cx.notify();
    }

    fn show_frame(&mut self, frame: PageFrame, window: &mut Window, cx: &mut Context<Self>) {
        let started = std::time::Instant::now();
        let size = (frame.width, frame.height);
        let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra) else {
            return;
        };
        // The pixels stay BGRA: that is the order GPUI's atlas expects.
        let image = Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(buffer), 1)));
        // The atlas caches by image id, so a superseded frame has to be evicted explicitly.
        if let Some(previous) = self.image.replace(image) {
            window.drop_image(previous).log_err();
        }
        self.frames_painted += 1;
        self.image_micros += started.elapsed().as_micros() as u64;
        if self.last_frame_size != size {
            log::info!("web_view: frame size {}x{}", size.0, size.1);
        }
        self.last_frame_size = size;
        cx.notify();
    }

    pub(crate) fn is_hidden(&self) -> bool {
        self.hidden.get()
    }

    pub(crate) fn page(&self) -> Option<Rc<Page>> {
        self.page.clone()
    }

    pub(crate) fn console(&self) -> &[String] {
        &self.console
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.loading
    }

    pub(crate) fn current_url(&self) -> &str {
        &self.url
    }

    pub(crate) fn pane_bounds(&self) -> Bounds<Pixels> {
        self.pane_bounds.get()
    }

    pub(crate) fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(page) = self.page.clone() else {
            return;
        };
        let keystroke = &event.keystroke;
        let flags = modifier_flags(&keystroke.modifiers, None);
        let Some(code) = windows_key_code(keystroke) else {
            return;
        };
        page.key(protocol::KEY_RAW_DOWN, code, 0, flags);
        let typed = if keystroke.modifiers.control
            || keystroke.modifiers.alt
            || keystroke.modifiers.platform
        {
            None
        } else {
            match keystroke.key.as_str() {
                "enter" => Some('\r' as u16),
                _ => keystroke
                    .key_char
                    .as_deref()
                    .and_then(|text| text.encode_utf16().next()),
            }
        };
        if let Some(character) = typed {
            page.key(protocol::KEY_CHAR, i32::from(character), character, flags);
        }
        cx.stop_propagation();
    }

    fn key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        let Some(page) = self.page.clone() else {
            return;
        };
        let Some(code) = windows_key_code(&event.keystroke) else {
            return;
        };
        page.key(
            protocol::KEY_UP,
            code,
            0,
            modifier_flags(&event.keystroke.modifiers, None),
        );
        cx.stop_propagation();
    }
}

fn modifier_flags(modifiers: &Modifiers, pressed_button: Option<MouseButton>) -> u32 {
    let mut flags = 0;
    if modifiers.shift {
        flags |= EVENT_SHIFT;
    }
    if modifiers.control {
        flags |= EVENT_CONTROL;
    }
    if modifiers.alt {
        flags |= EVENT_ALT;
    }
    if modifiers.platform {
        flags |= EVENT_COMMAND;
    }
    match pressed_button {
        Some(MouseButton::Left) => flags |= EVENT_LEFT_BUTTON,
        Some(MouseButton::Middle) => flags |= EVENT_MIDDLE_BUTTON,
        Some(MouseButton::Right) => flags |= EVENT_RIGHT_BUTTON,
        _ => {}
    }
    flags
}

/// Windows virtual-key codes, which is what the engine's key events use on every platform.
fn windows_key_code(keystroke: &Keystroke) -> Option<i32> {
    let code = match keystroke.key.as_str() {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" => 0x0D,
        "escape" => 0x1B,
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "delete" => 0x2E,
        key => {
            let mut chars = key.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                // Function keys and the like are not forwarded in the proof.
                return None;
            }
            match first {
                'a'..='z' => first.to_ascii_uppercase() as i32,
                '0'..='9' | 'A'..='Z' => first as i32,
                other if other.is_ascii() => other as i32,
                _ => return None,
            }
        }
    };
    Some(code)
}

fn page_point(position: Point<Pixels>, bounds: Bounds<Pixels>) -> (i32, i32) {
    (
        f32::from(position.x - bounds.origin.x) as i32,
        f32::from(position.y - bounds.origin.y) as i32,
    )
}

impl EventEmitter<()> for OffscreenView {}

impl Focusable for OffscreenView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for OffscreenView {
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
        if !self.hidden.replace(true) {
            log::info!("web_view: tab deactivated, page hidden");
            if let Some(page) = &self.page {
                page.set_hidden(true);
            }
        }
    }

    fn on_removed(&self, _cx: &mut Context<Self>) {
        log::info!("web_view: item removed from its pane, closing the browser view");
        if let Some(page) = &self.page {
            page.close();
        }
    }
}

impl Render for OffscreenView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(error) = &self.start_error {
            return div()
                .size_full()
                .p_4()
                .child(format!("The off-screen browser did not start: {error}"))
                .into_any_element();
        }
        let scale = window.scale_factor();
        let page = self.page.clone();
        let pane_bounds = self.pane_bounds.clone();
        let hidden = self.hidden.clone();
        let focus_handle = self.focus_handle.clone();
        let image = self.image.clone();
        let shown_size = self.last_frame_size;

        div()
            .key_context("WebView")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key_down(event, cx)))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| this.key_up(event, cx)))
            .relative()
            .overflow_hidden()
            .size_full()
            .child(
                canvas(
                    {
                        let page = page.clone();
                        let pane_bounds = pane_bounds.clone();
                        move |bounds, window, _cx| {
                            if pane_bounds.get() != bounds {
                                log::info!(
                                    "web_view: pane bounds {:?} scale {}",
                                    bounds.size,
                                    window.scale_factor()
                                );
                                pane_bounds.set(bounds);
                            }
                            if hidden.replace(false) {
                                log::info!("web_view: tab shown, page visible");
                                if let Some(page) = &page {
                                    page.set_hidden(false);
                                }
                            }
                            if let Some(page) = &page {
                                page.resize(
                                    f32::from(bounds.size.width) as i32,
                                    f32::from(bounds.size.height) as i32,
                                    window.scale_factor(),
                                );
                            }
                            window.insert_hitbox(bounds, HitboxBehavior::Normal)
                        }
                    },
                    move |bounds, hitbox, window, _cx| {
                        let Some(page) = page else { return };
                        window.on_mouse_event({
                            let page = page.clone();
                            let focus_handle = focus_handle.clone();
                            let hitbox = hitbox.clone();
                            move |event: &MouseDownEvent, phase, window, cx| {
                                if phase != DispatchPhase::Bubble
                                    || !hitbox.is_hovered_at(event.position, window)
                                {
                                    return;
                                }
                                window.focus(&focus_handle, cx);
                                let (x, y) = page_point(event.position, bounds);
                                page.mouse_button(
                                    x,
                                    y,
                                    modifier_flags(&event.modifiers, Some(event.button)),
                                    mouse_button_type(event.button),
                                    false,
                                    event.click_count.max(1) as i32,
                                );
                                cx.stop_propagation();
                            }
                        });
                        window.on_mouse_event({
                            let page = page.clone();
                            let hitbox = hitbox.clone();
                            move |event: &MouseUpEvent, phase, window, cx| {
                                if phase != DispatchPhase::Bubble
                                    || !hitbox.is_hovered_at(event.position, window)
                                {
                                    return;
                                }
                                let (x, y) = page_point(event.position, bounds);
                                page.mouse_button(
                                    x,
                                    y,
                                    modifier_flags(&event.modifiers, None),
                                    mouse_button_type(event.button),
                                    true,
                                    event.click_count.max(1) as i32,
                                );
                                cx.stop_propagation();
                            }
                        });
                        window.on_mouse_event({
                            let page = page.clone();
                            let hitbox = hitbox.clone();
                            move |event: &MouseMoveEvent, phase, window, _cx| {
                                if phase != DispatchPhase::Bubble {
                                    return;
                                }
                                let inside = hitbox.is_hovered_at(event.position, window);
                                if !inside && event.pressed_button.is_none() {
                                    return;
                                }
                                let (x, y) = page_point(event.position, bounds);
                                page.mouse_move(
                                    x,
                                    y,
                                    modifier_flags(&event.modifiers, event.pressed_button),
                                    false,
                                );
                            }
                        });
                        window.on_mouse_event(
                            move |event: &ScrollWheelEvent, phase, window, cx| {
                                if phase != DispatchPhase::Bubble
                                    || !hitbox.is_hovered_at(event.position, window)
                                {
                                    return;
                                }
                                let delta = match event.delta {
                                    ScrollDelta::Pixels(delta) => {
                                        (f32::from(delta.x), f32::from(delta.y))
                                    }
                                    ScrollDelta::Lines(lines) => (
                                        lines.x * WHEEL_LINE_PIXELS,
                                        lines.y * WHEEL_LINE_PIXELS,
                                    ),
                                };
                                let (x, y) = page_point(event.position, bounds);
                                page.mouse_wheel(
                                    x,
                                    y,
                                    modifier_flags(&event.modifiers, None),
                                    delta.0 as i32,
                                    delta.1 as i32,
                                );
                                cx.stop_propagation();
                            },
                        );
                    },
                )
                .absolute()
                .size_full(),
            )
            .when_some(image, |this, image| {
                // Painted at its own pixel size from the top-left corner and clipped by the pane, so a frame from before a resize is never stretched.
                this.child(
                    img(image)
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(shown_size.0 as f32 / scale))
                        .h(px(shown_size.1 as f32 / scale))
                        .object_fit(ObjectFit::Fill),
                )
            })
            .into_any_element()
    }
}

fn mouse_button_type(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left | MouseButton::Navigate(_) => BUTTON_LEFT,
        MouseButton::Middle => BUTTON_MIDDLE,
        MouseButton::Right => BUTTON_RIGHT,
    }
}
