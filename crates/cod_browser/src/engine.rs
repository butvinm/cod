use std::{
    collections::HashMap,
    io::{BufReader, BufWriter, Write},
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};

use cef::*;

use crate::protocol::{self, FromBrowser, ToBrowser};

const MAX_CONSOLE_BYTES: usize = 4096;
const MAX_TITLE_BYTES: usize = 1024;
const MAX_URL_BYTES: usize = 8192;
const MAX_PENDING_CONSOLE: usize = 64;

/// Console messages queued for sending and not yet written.
static PENDING_CONSOLE: AtomicUsize = AtomicUsize::new(0);

/// Cuts text from the page to at most `limit` bytes on a character boundary, so no message can exceed what Cod accepts.
fn limited(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

enum Outgoing {
    Message(FromBrowser),
    /// A newer frame of this page is waiting in its slot.
    FrameReady(u32),
}

struct PageView {
    width: i32,
    height: i32,
    scale: f32,
}

struct BrowserHandle(Browser);

// SAFETY: CEF browser objects are reference counted with atomic counts, and the host methods used here may be called from any thread of the browser process.
unsafe impl Send for BrowserHandle {}

struct PageState {
    id: u32,
    view: Mutex<PageView>,
    latest: Mutex<Option<(u32, u32, Vec<u8>)>>,
    frame_pending: AtomicBool,
    browser: Mutex<Option<BrowserHandle>>,
    close_requested: AtomicBool,
    out: mpsc::Sender<Outgoing>,
    pages: Pages,
}

type Pages = Arc<Mutex<HashMap<u32, Arc<PageState>>>>;

impl PageState {
    fn send(&self, message: FromBrowser) {
        self.out.send(Outgoing::Message(message)).ok();
    }

    fn with_host(&self, run: impl FnOnce(&BrowserHost)) {
        let Ok(slot) = self.browser.lock() else {
            return;
        };
        if let Some(host) = slot.as_ref().and_then(|handle| handle.0.host()) {
            run(&host);
        }
    }

    fn main_frame(&self) -> Option<Frame> {
        self.browser
            .lock()
            .ok()?
            .as_ref()
            .and_then(|handle| handle.0.main_frame())
    }
}

#[derive(Clone)]
struct PageHandler {
    state: Arc<PageState>,
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
            let Ok(view) = self.handler.state.view.lock() else { return };
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
            let (Some(screen_info), Ok(view)) = (screen_info, self.handler.state.view.lock()) else {
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
            // SAFETY: CEF guarantees `buffer` holds `width * height` BGRA pixels for the duration of this call.
            let bytes = unsafe {
                std::slice::from_raw_parts(buffer, width as usize * height as usize * 4)
            };
            let state = &self.handler.state;
            if let Ok(mut latest) = state.latest.lock() {
                *latest = Some((width as u32, height as u32, bytes.to_vec()));
            }
            if !state.frame_pending.swap(true, Ordering::AcqRel) {
                state.out.send(Outgoing::FrameReady(state.id)).ok();
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
            let state = &self.handler.state;
            state.send(FromBrowser::Loading { page: state.id, loading: is_loading != 0 });
        }
    }
}

wrap_display_handler! {
    struct PageDisplay {
        handler: PageHandler,
    }

    impl DisplayHandler {
        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            let state = &self.handler.state;
            if let Some(title) = title {
                let title = limited(&title.to_string(), MAX_TITLE_BYTES);
                state.send(FromBrowser::Title { page: state.id, title });
            }
        }

        fn on_address_change(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let state = &self.handler.state;
            if let Some(url) = url {
                let url = limited(&url.to_string(), MAX_URL_BYTES);
                state.send(FromBrowser::Address { page: state.id, url });
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
            let state = &self.handler.state;
            // Diagnostics are best effort: a page that logs in a loop must not grow the queue or crowd out frames.
            let Some(message) = message else { return 0 };
            if PENDING_CONSOLE.fetch_add(1, Ordering::AcqRel) < MAX_PENDING_CONSOLE {
                let message = limited(&message.to_string(), MAX_CONSOLE_BYTES);
                state.send(FromBrowser::Console { page: state.id, message });
            } else {
                PENDING_CONSOLE.fetch_sub(1, Ordering::AcqRel);
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
            let state = &self.handler.state;
            if state.close_requested.load(Ordering::Acquire) {
                if let Some(host) = browser.host() {
                    host.close_browser(1);
                }
                return;
            }
            if let Ok(mut slot) = state.browser.lock() {
                *slot = Some(BrowserHandle(browser.clone()));
            }
            state.send(FromBrowser::Created { page: state.id });
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let state = &self.handler.state;
            if let Ok(mut slot) = state.browser.lock() {
                slot.take();
            }
            let open = match state.pages.lock() {
                Ok(mut pages) => {
                    pages.remove(&state.id);
                    pages.len()
                }
                Err(_) => 0,
            };
            eprintln!("cod-browser: page {} closed and freed, {open} still open", state.id);
            state.send(FromBrowser::Closed { page: state.id });
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

fn switch_value(prefix: &str) -> Option<String> {
    std::env::args().find_map(|argument| argument.strip_prefix(prefix).map(str::to_owned))
}

pub fn main() {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let Some(command_line) = args.as_cmd_line() else {
        eprintln!("cod-browser: cannot read the command line");
        return;
    };
    let mut app = EngineApp::new();
    if command_line.has_switch(Some(&CefString::from("type"))) == 1 {
        let code = execute_process(Some(args.as_main_args()), Some(&mut app), std::ptr::null_mut());
        std::process::exit(code.max(0));
    }

    let (Some(socket_path), Some(cache)) = (
        switch_value("--cod-socket="),
        switch_value("--cod-cache="),
    ) else {
        eprintln!("cod-browser: started without --cod-socket and --cod-cache, it is meant to be started by Cod");
        std::process::exit(2);
    };
    let stream = match UnixStream::connect(&socket_path) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("cod-browser: cannot connect to {socket_path}: {error}");
            std::process::exit(2);
        }
    };
    let (out, outgoing) = mpsc::channel::<Outgoing>();
    let pages: Pages = Arc::new(Mutex::new(HashMap::new()));

    let started = Instant::now();
    let mut settings = Settings {
        windowless_rendering_enabled: 1,
        multi_threaded_message_loop: 1,
        no_sandbox: 1,
        root_cache_path: CefString::from(cache.as_str()),
        log_file: CefString::from(format!("{cache}/cef.log").as_str()),
        log_severity: LogSeverity::WARNING,
        ..Default::default()
    };
    if let Ok(dir) = std::env::var("COD_CEF_DIR") {
        settings.resources_dir_path = CefString::from(dir.as_str());
        settings.locales_dir_path = CefString::from(format!("{dir}/locales").as_str());
    }
    let initialized = initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    );
    if initialized != 1 {
        let mut writer = BufWriter::new(&stream);
        FromBrowser::Failed {
            message: "CEF failed to start".into(),
        }
        .write_to(&mut writer)
        .ok();
        writer.flush().ok();
        std::process::exit(3);
    }
    eprintln!("cod-browser: CEF started in {:?}", started.elapsed());

    let writer_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("cod-browser: cannot clone the socket: {error}");
            std::process::exit(2);
        }
    };
    let writer_pages = pages.clone();
    std::thread::spawn(move || write_loop(writer_stream, outgoing, writer_pages));
    out.send(Outgoing::Message(FromBrowser::Ready)).ok();

    let mut input = BufReader::new(stream);
    loop {
        let message = match ToBrowser::read_from(&mut input) {
            Ok(message) => message,
            Err(error) => {
                // Cod closed the socket or exited: nothing is left to draw for.
                eprintln!("cod-browser: connection ended ({error}), exiting");
                std::process::exit(0);
            }
        };
        handle(message, &out, &pages);
    }
}

fn write_loop(stream: UnixStream, outgoing: mpsc::Receiver<Outgoing>, pages: Pages) {
    let mut writer = BufWriter::with_capacity(1 << 16, stream);
    while let Ok(item) = outgoing.recv() {
        let result = match item {
            Outgoing::Message(message) => {
                if matches!(message, FromBrowser::Console { .. }) {
                    PENDING_CONSOLE.fetch_sub(1, Ordering::AcqRel);
                }
                message.write_to(&mut writer)
            }
            Outgoing::FrameReady(page) => {
                let state = pages.lock().ok().and_then(|pages| pages.get(&page).cloned());
                let Some(state) = state else { continue };
                state.frame_pending.store(false, Ordering::Release);
                let frame = state.latest.lock().ok().and_then(|mut latest| latest.take());
                match frame {
                    Some((width, height, bgra)) => {
                        protocol::write_frame(&mut writer, page, width, height, &bgra)
                    }
                    None => continue,
                }
            }
        };
        if result.and_then(|()| writer.flush()).is_err() {
            std::process::exit(0);
        }
    }
}

fn handle(message: ToBrowser, out: &mpsc::Sender<Outgoing>, pages: &Pages) {
    if let ToBrowser::Create {
        page,
        width,
        height,
        scale,
        url,
    } = message
    {
        let state = Arc::new(PageState {
            id: page,
            view: Mutex::new(PageView {
                width,
                height,
                scale,
            }),
            latest: Mutex::new(None),
            frame_pending: AtomicBool::new(false),
            browser: Mutex::new(None),
            close_requested: AtomicBool::new(false),
            out: out.clone(),
            pages: pages.clone(),
        });
        if let Ok(mut table) = pages.lock() {
            table.insert(page, state.clone());
        }
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
            Some(&mut PageClient::new(PageHandler { state })),
            Some(&url.as_str().into()),
            Some(&browser_settings),
            None,
            None,
        );
        if created != 1 {
            eprintln!("cod-browser: CEF refused to create page {page}");
            out.send(Outgoing::Message(FromBrowser::Closed { page })).ok();
        }
        return;
    }

    let page = match &message {
        ToBrowser::Create { .. } => return,
        ToBrowser::Resize { page, .. }
        | ToBrowser::Hidden { page, .. }
        | ToBrowser::Focus { page, .. }
        | ToBrowser::Load { page, .. }
        | ToBrowser::Script { page, .. }
        | ToBrowser::MouseMove { page, .. }
        | ToBrowser::MouseButton { page, .. }
        | ToBrowser::Wheel { page, .. }
        | ToBrowser::Key { page, .. }
        | ToBrowser::Close { page } => *page,
    };
    let Some(state) = pages.lock().ok().and_then(|table| table.get(&page).cloned()) else {
        return;
    };
    match message {
        ToBrowser::Create { .. } => {}
        ToBrowser::Resize {
            width,
            height,
            scale,
            ..
        } => {
            let scale_changed = {
                let Ok(mut view) = state.view.lock() else {
                    return;
                };
                if view.width == width && view.height == height && view.scale == scale {
                    return;
                }
                let scale_changed = view.scale != scale;
                *view = PageView {
                    width,
                    height,
                    scale,
                };
                scale_changed
            };
            state.with_host(|host| {
                if scale_changed {
                    host.notify_screen_info_changed();
                }
                host.was_resized();
            });
        }
        ToBrowser::Hidden { hidden, .. } => state.with_host(|host| host.was_hidden(hidden as i32)),
        ToBrowser::Focus { focused, .. } => state.with_host(|host| host.set_focus(focused as i32)),
        ToBrowser::Load { url, .. } => {
            if let Some(frame) = state.main_frame() {
                frame.load_url(Some(&url.as_str().into()));
            }
        }
        ToBrowser::Script { script, .. } => {
            if let Some(frame) = state.main_frame() {
                frame.execute_java_script(
                    Some(&script.as_str().into()),
                    Some(&"cod://browser".into()),
                    0,
                );
            }
        }
        ToBrowser::MouseMove {
            x,
            y,
            modifiers,
            leave,
            ..
        } => {
            let event = MouseEvent { x, y, modifiers };
            state.with_host(|host| host.send_mouse_move_event(Some(&event), leave as i32));
        }
        ToBrowser::MouseButton {
            x,
            y,
            modifiers,
            button,
            up,
            clicks,
            ..
        } => {
            let event = MouseEvent { x, y, modifiers };
            let button = match button {
                protocol::BUTTON_MIDDLE => MouseButtonType::MIDDLE,
                protocol::BUTTON_RIGHT => MouseButtonType::RIGHT,
                _ => MouseButtonType::LEFT,
            };
            state.with_host(|host| {
                host.send_mouse_click_event(Some(&event), button, up as i32, clicks)
            });
        }
        ToBrowser::Wheel {
            x,
            y,
            modifiers,
            delta_x,
            delta_y,
            ..
        } => {
            let event = MouseEvent { x, y, modifiers };
            state.with_host(|host| host.send_mouse_wheel_event(Some(&event), delta_x, delta_y));
        }
        ToBrowser::Key {
            kind,
            code,
            character,
            modifiers,
            ..
        } => {
            let type_ = match kind {
                protocol::KEY_UP => KeyEventType::KEYUP,
                protocol::KEY_CHAR => KeyEventType::CHAR,
                _ => KeyEventType::RAWKEYDOWN,
            };
            let event = KeyEvent {
                type_,
                modifiers,
                windows_key_code: code,
                native_key_code: code,
                character,
                unmodified_character: character,
                ..Default::default()
            };
            state.with_host(|host| host.send_key_event(Some(&event)));
        }
        ToBrowser::Close { .. } => {
            state.close_requested.store(true, Ordering::Release);
            state.with_host(|host| host.close_browser(1));
        }
    }
}
