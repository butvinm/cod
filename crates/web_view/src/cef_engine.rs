//! The off-screen browser engine for issue #16: CEF (Chromium Embedded Framework) in windowless mode.
//!
//! CEF runs its own UI thread (`multi_threaded_message_loop`), draws without any window (`--ozone-platform=headless`, software compositing) and hands every frame to `on_paint` as a BGRA buffer.
//! Frames are copied into an owned buffer and only the latest one is kept; the GPUI side turns it into an image.
//! Zed's own binary doubles as the CEF helper process: `run_subprocess_if_requested` runs first in `main`.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::Instant,
};

use anyhow::{Result, bail};
use cef::*;
use futures::channel::mpsc::UnboundedSender;

pub(crate) const EVENT_SHIFT: u32 = 1 << 1;
pub(crate) const EVENT_CONTROL: u32 = 1 << 2;
pub(crate) const EVENT_ALT: u32 = 1 << 3;
pub(crate) const EVENT_LEFT_BUTTON: u32 = 1 << 4;
pub(crate) const EVENT_MIDDLE_BUTTON: u32 = 1 << 5;
pub(crate) const EVENT_RIGHT_BUTTON: u32 = 1 << 6;
pub(crate) const EVENT_COMMAND: u32 = 1 << 7;

/// One painted frame: tightly packed BGRA, `width * height * 4` bytes.
pub(crate) struct PageFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub(crate) enum PageEvent {
    /// A newer frame is waiting in `Page::take_frame`.
    Frame,
    Title(String),
    Address(String),
    Loading(bool),
    Console(String),
    Created,
    Closed,
}

pub(crate) struct PageStats {
    pub frames: u64,
    pub copy_micros: u64,
    pub started: Instant,
}

struct BrowserHandle(Browser);

// SAFETY: CEF browser objects are reference counted with atomic counts and the host methods used here may be called from any thread in the browser process.
unsafe impl Send for BrowserHandle {}

struct View {
    width: i32,
    height: i32,
    scale: f32,
}

struct Shared {
    view: Mutex<View>,
    latest: Mutex<Option<PageFrame>>,
    frame_signalled: AtomicBool,
    browser: Mutex<Option<BrowserHandle>>,
    close_requested: AtomicBool,
    events: UnboundedSender<PageEvent>,
    frames: AtomicU32,
    copy_micros: Mutex<u64>,
}

impl Shared {
    fn send(&self, event: PageEvent) {
        self.events.unbounded_send(event).ok();
    }
}

#[derive(Clone)]
struct PageHandler {
    shared: Arc<Shared>,
}

wrap_app! {
    struct EngineApp {}

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            command_line.append_switch_with_value(
                Some(&"ozone-platform".into()),
                Some(&"headless".into()),
            );
            command_line.append_switch(Some(&"disable-gpu".into()));
            command_line.append_switch(Some(&"disable-gpu-compositing".into()));
            command_line.append_switch(Some(&"no-sandbox".into()));
            command_line.append_switch(Some(&"disable-dev-shm-usage".into()));
        }
    }
}

wrap_render_handler! {
    struct PageRender {
        handler: PageHandler,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else { return };
            let Ok(view) = self.handler.shared.view.lock() else { return };
            rect.x = 0;
            rect.y = 0;
            rect.width = view.width.max(1);
            rect.height = view.height.max(1);
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            let (Some(screen_info), Ok(view)) = (screen_info, self.handler.shared.view.lock()) else {
                return 0;
            };
            screen_info.device_scale_factor = view.scale;
            1
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            if type_ != PaintElementType::VIEW || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let started = Instant::now();
            // SAFETY: CEF guarantees `buffer` holds `width * height` BGRA pixels for the duration of this call.
            let bytes = unsafe {
                std::slice::from_raw_parts(buffer, width as usize * height as usize * 4)
            };
            let frame = PageFrame {
                width: width as u32,
                height: height as u32,
                bgra: bytes.to_vec(),
            };
            let shared = &self.handler.shared;
            if let Ok(mut latest) = shared.latest.lock() {
                *latest = Some(frame);
            }
            if let Ok(mut micros) = shared.copy_micros.lock() {
                *micros += started.elapsed().as_micros() as u64;
            }
            shared.frames.fetch_add(1, Ordering::Relaxed);
            if !shared.frame_signalled.swap(true, Ordering::AcqRel) {
                shared.send(PageEvent::Frame);
            }
        }
    }
}

wrap_load_handler! {
    struct PageLoad {
        handler: PageHandler,
    }

    impl LoadHandler {
        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            _can_go_back: ::std::os::raw::c_int,
            _can_go_forward: ::std::os::raw::c_int,
        ) {
            self.handler.shared.send(PageEvent::Loading(is_loading != 0));
        }
    }
}

wrap_display_handler! {
    struct PageDisplay {
        handler: PageHandler,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if let Some(title) = title {
                self.handler.shared.send(PageEvent::Title(title.to_string()));
            }
        }

        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            if let Some(url) = url {
                self.handler.shared.send(PageEvent::Address(url.to_string()));
            }
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            _source: Option<&CefString>,
            _line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            if let Some(message) = message {
                self.handler.shared.send(PageEvent::Console(message.to_string()));
            }
            0
        }
    }
}

wrap_life_span_handler! {
    struct PageLifeSpan {
        handler: PageHandler,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(browser) = browser else { return };
            let shared = &self.handler.shared;
            if shared.close_requested.load(Ordering::Acquire) {
                if let Some(host) = browser.host() {
                    host.close_browser(1);
                }
                return;
            }
            if let Ok(mut slot) = shared.browser.lock() {
                *slot = Some(BrowserHandle(browser.clone()));
            }
            shared.send(PageEvent::Created);
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let shared = &self.handler.shared;
            if let Ok(mut slot) = shared.browser.lock() {
                slot.take();
            }
            shared.send(PageEvent::Closed);
        }
    }
}

wrap_client! {
    struct PageClient {
        handler: PageHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(PageRender::new(self.handler.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(PageLoad::new(self.handler.clone()))
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(PageDisplay::new(self.handler.clone()))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(PageLifeSpan::new(self.handler.clone()))
        }
    }
}

/// Runs the CEF helper role when this process was started by CEF as a renderer, GPU or utility process, and exits.
/// Returns at once in the browser process.
pub fn run_subprocess_if_requested() {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        return;
    };
    if command_line.has_switch(Some(&CefString::from("type"))) != 1 {
        return;
    }
    let mut app = EngineApp::new();
    let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
    std::process::exit(code.max(0));
}

static INITIALIZED: OnceLock<bool> = OnceLock::new();

/// Starts CEF once, on the thread that becomes its main thread. `COD_CEF_DIR` names the folder with `libcef.so` and the `.pak` files.
fn ensure_initialized(data_dir: &Path) -> Result<()> {
    let ok = INITIALIZED.get_or_init(|| {
        let args = cef::args::Args::new();
        let mut app = EngineApp::new();
        let cache = data_dir.join("cef");
        let mut settings = Settings {
            windowless_rendering_enabled: 1,
            multi_threaded_message_loop: 1,
            no_sandbox: 1,
            root_cache_path: CefString::from(cache.to_string_lossy().as_ref()),
            log_file: CefString::from(cache.join("cef.log").to_string_lossy().as_ref()),
            log_severity: LogSeverity::WARNING,
            ..Default::default()
        };
        if let Ok(dir) = std::env::var("COD_CEF_DIR") {
            let dir = PathBuf::from(dir);
            settings.resources_dir_path = CefString::from(dir.to_string_lossy().as_ref());
            settings.locales_dir_path =
                CefString::from(dir.join("locales").to_string_lossy().as_ref());
        }
        let started = initialize(
            Some(args.as_main_args()),
            Some(&settings),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        log::info!("web_view: CEF initialized: {}", started == 1);
        started == 1
    });
    if !ok {
        bail!("CEF failed to start");
    }
    Ok(())
}

/// One browser view. All methods can be called from the GPUI thread; CEF does the work on its own threads.
pub(crate) struct Page {
    shared: Arc<Shared>,
    started: Instant,
}

impl Page {
    pub fn new(
        url: &str,
        width: i32,
        height: i32,
        scale: f32,
        data_dir: &Path,
        events: UnboundedSender<PageEvent>,
    ) -> Result<Self> {
        ensure_initialized(data_dir)?;
        let shared = Arc::new(Shared {
            view: Mutex::new(View { width, height, scale }),
            latest: Mutex::new(None),
            frame_signalled: AtomicBool::new(false),
            browser: Mutex::new(None),
            close_requested: AtomicBool::new(false),
            events,
            frames: AtomicU32::new(0),
            copy_micros: Mutex::new(0),
        });
        let handler = PageHandler {
            shared: shared.clone(),
        };
        let window_info = WindowInfo {
            windowless_rendering_enabled: 1,
            ..Default::default()
        };
        let browser_settings = BrowserSettings {
            windowless_frame_rate: 60,
            background_color: 0xFFFF_FFFF,
            ..Default::default()
        };
        let created = browser_host_create_browser(
            Some(&window_info),
            Some(&mut PageClient::new(handler)),
            Some(&url.into()),
            Some(&browser_settings),
            None,
            None,
        );
        if created != 1 {
            bail!("CEF refused to create a browser");
        }
        Ok(Self {
            shared,
            started: Instant::now(),
        })
    }

    /// The latest frame, if one arrived since the last call. Clears the signal so the next paint sends a new event.
    pub fn take_frame(&self) -> Option<PageFrame> {
        self.shared.frame_signalled.store(false, Ordering::Release);
        self.shared.latest.lock().ok()?.take()
    }

    pub fn stats(&self) -> PageStats {
        PageStats {
            frames: u64::from(self.shared.frames.load(Ordering::Relaxed)),
            copy_micros: self.shared.copy_micros.lock().map(|micros| *micros).unwrap_or(0),
            started: self.started,
        }
    }

    fn with_host(&self, run: impl FnOnce(&BrowserHost)) {
        let Ok(slot) = self.shared.browser.lock() else {
            return;
        };
        if let Some(host) = slot.as_ref().and_then(|handle| handle.0.host()) {
            run(&host);
        }
    }

    /// Sets the view size in logical pixels and the device scale, and tells the engine to lay the page out again.
    pub fn resize(&self, width: i32, height: i32, scale: f32) {
        let changed_scale = {
            let Ok(mut view) = self.shared.view.lock() else {
                return;
            };
            if view.width == width && view.height == height && view.scale == scale {
                return;
            }
            let changed_scale = view.scale != scale;
            *view = View { width, height, scale };
            changed_scale
        };
        self.with_host(|host| {
            if changed_scale {
                host.notify_screen_info_changed();
            }
            host.was_resized();
        });
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.with_host(|host| host.was_hidden(hidden as i32));
    }

    pub fn set_focus(&self, focused: bool) {
        self.with_host(|host| host.set_focus(focused as i32));
    }

    pub fn load_url(&self, url: &str) {
        let Ok(slot) = self.shared.browser.lock() else {
            return;
        };
        if let Some(frame) = slot.as_ref().and_then(|handle| handle.0.main_frame()) {
            frame.load_url(Some(&url.into()));
        }
    }

    pub fn execute_javascript(&self, script: &str) {
        let Ok(slot) = self.shared.browser.lock() else {
            return;
        };
        if let Some(frame) = slot.as_ref().and_then(|handle| handle.0.main_frame()) {
            frame.execute_java_script(Some(&script.into()), Some(&"cod://web_view".into()), 0);
        }
    }

    pub fn mouse_move(&self, x: i32, y: i32, modifiers: u32, leave: bool) {
        let event = MouseEvent { x, y, modifiers };
        self.with_host(|host| host.send_mouse_move_event(Some(&event), leave as i32));
    }

    pub fn mouse_button(
        &self,
        x: i32,
        y: i32,
        modifiers: u32,
        button: MouseButtonType,
        up: bool,
        click_count: i32,
    ) {
        let event = MouseEvent { x, y, modifiers };
        self.with_host(|host| {
            host.send_mouse_click_event(Some(&event), button, up as i32, click_count)
        });
    }

    pub fn mouse_wheel(&self, x: i32, y: i32, modifiers: u32, delta_x: i32, delta_y: i32) {
        let event = MouseEvent { x, y, modifiers };
        self.with_host(|host| host.send_mouse_wheel_event(Some(&event), delta_x, delta_y));
    }

    pub fn key(&self, kind: KeyEventType, windows_key_code: i32, character: u16, modifiers: u32) {
        let event = KeyEvent {
            type_: kind,
            modifiers,
            windows_key_code,
            native_key_code: windows_key_code,
            character,
            unmodified_character: character,
            ..Default::default()
        };
        self.with_host(|host| host.send_key_event(Some(&event)));
    }

    /// Asks the engine to close the view. `Closed` follows when it is gone.
    pub fn close(&self) {
        self.shared.close_requested.store(true, Ordering::Release);
        self.with_host(|host| host.close_browser(1));
    }
}
