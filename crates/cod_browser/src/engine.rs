use std::{
    collections::HashMap,
    io::{self, BufReader, BufWriter, Write},
    mem::ManuallyDrop,
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::net::UnixStream,
    },
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
const MAX_PENDING_CONSOLE: usize = 64;

/// Console messages queued for sending and not yet written.
static PENDING_CONSOLE: AtomicUsize = AtomicUsize::new(0);

/// Logged once, the first time any page's `on_paint` fires on macOS (#23 round 3 diagnosis M2):
/// off-screen rendering depends on CEF's own UI-thread message loop actually making progress
/// (round 2 found it was not), so this one line makes the next failure visible without needing a
/// full frame dump.
#[cfg(target_os = "macos")]
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);

use protocol::{MAX_URL_BYTES, limited};

/// Whether a console line is forwarded at this pending-console backlog (RC review B5, final
/// batch): an ordinary diagnostic only below `MAX_PENDING_CONSOLE`, a bridge call
/// (`BRIDGE_PREFIX`) also through a full backlog of ordinary lines, up to
/// `protocol::BRIDGE_HEADROOM` more, so a flood of bridge-prefixed lines stays bounded. See
/// `on_console_message` for why a bridge call must not be what ordinary console spam crowds out.
fn forward_despite_backlog(text: &str, pending: usize) -> bool {
    if text.starts_with(protocol::BRIDGE_PREFIX) {
        pending < MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM
    } else {
        pending < MAX_PENDING_CONSOLE
    }
}

enum Outgoing {
    Message(FromBrowser),
    /// A newer frame of this page is waiting in its slot.
    FrameReady(u32),
    /// Title, address or loading state of this page changed; only the latest values are sent.
    MetaReady(u32),
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
    meta: Mutex<PageMeta>,
    meta_pending: AtomicBool,
    out: mpsc::Sender<Outgoing>,
    pages: Pages,
}

/// The latest title, address and loading state not yet sent.
#[derive(Default)]
struct PageMeta {
    title: Option<String>,
    address: Option<String>,
    loading: Option<bool>,
}

type Pages = Arc<Mutex<HashMap<u32, Arc<PageState>>>>;

impl PageState {
    /// Keeps only the latest value of each and queues at most one notification, however fast the page changes them.
    fn update_meta(&self, update: impl FnOnce(&mut PageMeta)) {
        if let Ok(mut meta) = self.meta.lock() {
            update(&mut meta);
        }
        if !self.meta_pending.swap(true, Ordering::AcqRel) {
            self.out.send(Outgoing::MetaReady(self.id)).ok();
        }
    }

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
            // Ozone is Chromium's Linux windowing abstraction and "disable the /dev/shm cache" is
            // a Linux container workaround: neither switch means anything on macOS, which has no
            // Ozone platform and no /dev/shm, so both stay Linux-only (#23 round 1: widening this
            // function to macOS must not add switches macOS does not understand).
            #[cfg(target_os = "linux")]
            {
                command_line.append_switch_with_value(
                    Some(&"ozone-platform".into()),
                    Some(&"headless".into()),
                );
                command_line.append_switch(Some(&"disable-dev-shm-usage".into()));
            }
            command_line.append_switch(Some(&"disable-gpu".into()));
            command_line.append_switch(Some(&"disable-gpu-compositing".into()));
            command_line.append_switch(Some(&"no-sandbox".into()));
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
            // The same checked size the writer and the reader use, so a bogus size from the engine is dropped before any slice or copy is made.
            let (width, height) = (width as u32, height as u32);
            #[cfg(target_os = "macos")]
            if !FIRST_FRAME_LOGGED.swap(true, Ordering::AcqRel) {
                eprintln!("cod-browser: first frame {width}x{height}");
            }
            let Ok(length) = protocol::frame_byte_len(width, height) else {
                eprintln!("cod-browser: dropped a frame of {width}x{height}");
                return;
            };
            // SAFETY: CEF guarantees `buffer` holds `width * height` BGRA pixels for the duration of this call, and `length` is exactly that, checked.
            let bytes = unsafe { std::slice::from_raw_parts(buffer, length) };
            let state = &self.handler.state;
            if let Ok(mut latest) = state.latest.lock() {
                *latest = Some((width, height, bytes.to_vec()));
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
            state.update_meta(|meta| meta.loading = Some(is_loading != 0));
        }

        fn on_load_error(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            // Only the page itself counts: a broken frame inside it is not a failed page. An aborted load is a navigation the user or the page replaced.
            if error_code == Errorcode::ABORTED || frame.is_none_or(|frame| frame.is_main() != 1)
            {
                return;
            }
            let state = &self.handler.state;
            let url = failed_url
                .map(|url| limited(&url.to_string(), MAX_URL_BYTES))
                .unwrap_or_default();
            let error = error_text
                .map(|text| limited(&text.to_string(), MAX_CONSOLE_BYTES))
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| "the page could not be loaded".to_owned());
            state.send(FromBrowser::LoadFailed { page: state.id, url, error });
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
                state.update_meta(|meta| meta.title = Some(title));
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
                state.update_meta(|meta| meta.address = Some(url));
            }
        }

        fn on_console_message(
            &self,
            _browser: Option<&mut Browser>,
            _level: LogSeverity,
            message: Option<&CefString>,
            source: Option<&CefString>,
            _line: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let state = &self.handler.state;
            // Diagnostics are best effort: a page that logs in a loop must not grow the queue or crowd out frames.
            let Some(message) = message else { return 0 };
            let text = message.to_string();
            // RC review B5 (final batch): a bridge call (`BRIDGE_PREFIX`) must always cross,
            // however full this queue already is with ordinary console spam, or a submit that
            // lands in the same burst can be dropped here and leave `codctl open --html --wait`
            // hanging forever for a reply nobody sent. Only ordinary diagnostics stay best
            // effort and may be dropped to keep the queue from growing without bound and
            // crowding out frames; the residual risk is a page able to run its own `console.log`
            // (only `--js` pages, and only from a frame this exemption does not itself
            // distinguish from the main document) flooding with forged `BRIDGE_PREFIX` lines,
            // which Cod's side still refuses once it checks the actual sender and action.
            let pending = PENDING_CONSOLE.fetch_add(1, Ordering::AcqRel);
            if forward_despite_backlog(&text, pending) {
                // A bridge call (RC review B5) keeps a far larger limit than ordinary console
                // output, and one that still does not fit becomes a clear sentinel instead of
                // truncated, invalid JSON that `codctl open --html --wait` would never answer.
                let message = protocol::console_payload(&text, MAX_CONSOLE_BYTES);
                // The document CEF attributes this line to (RC review B2 for `--js` pages): a
                // subframe's own address, distinct from the main document's, so Cod's side can
                // refuse a bridge call that did not come from the page it actually built.
                let source = source
                    .map(|source| limited(&source.to_string(), MAX_URL_BYTES))
                    .unwrap_or_default();
                state.send(FromBrowser::Console { page: state.id, message, source });
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
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            _target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // No browser windows of its own: the helper cancels every popup and Cod opens the URL like a link.
            let state = &self.handler.state;
            if let Some(url) = target_url {
                let url = limited(&url.to_string(), MAX_URL_BYTES);
                if !url.is_empty() {
                    state.send(FromBrowser::PopupRequested { page: state.id, url });
                }
            }
            1
        }

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

/// Runs an arbitrary closure on CEF's own UI thread on macOS (#23 round 3 diagnosis M1).
///
/// Linux sets `multi_threaded_message_loop` (see `main`), so CEF spins its own UI thread and
/// `handle`'s calls into `browser_host_create_browser` and the `BrowserHost`/`Frame` methods,
/// made from this helper's reader thread, are the documented "callable from any thread" entry
/// points that mode relies on. `multi_threaded_message_loop` is Windows/Linux only; without it,
/// macOS round 2 showed the browser gets created (on_after_created still fired) but no further
/// progress (the page's own HTTP request only reached the fixture server at process shutdown):
/// evidence that those same calls need to run on the thread actually driving `run_message_loop`
/// instead. `post_task(ThreadId::UI, ...)` is the marshaling cef-rs's own macOS examples use for
/// exactly this.
#[cfg(target_os = "macos")]
fn post_to_ui_thread(work: impl FnOnce() + Send + 'static) {
    let work: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>> = Arc::new(Mutex::new(Some(Box::new(work))));
    post_task(ThreadId::UI, Some(&mut UiTask::new(work)));
}

#[cfg(target_os = "macos")]
wrap_task! {
    struct UiTask {
        work: Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>,
    }

    impl Task {
        fn execute(&self) {
            let Ok(mut slot) = self.work.lock() else { return };
            if let Some(work) = slot.take() {
                work();
            }
        }
    }
}

fn switch_value(prefix: &str) -> Option<String> {
    std::env::args().find_map(|argument| argument.strip_prefix(prefix).map(str::to_owned))
}

/// macOS cannot dynamically link against "Chromium Embedded Framework.framework" the ordinary
/// way (there is no stable on-disk path at link time, since the bundle can be installed
/// anywhere): the framework has to be `dlopen`-ed at startup, before any other CEF call,
/// including `api_hash` (#23 round 1). `cef::library_loader::LibraryLoader` resolves the
/// framework relative to the running executable's own location inside the app bundle: `../
/// Frameworks` for the main `cod-browser` executable (`Contents/MacOS/cod-browser`), or `../../..`
/// for a copy running from one of the nested helper bundles this module points at
/// (`helper_subprocess_path()`, see `script/bundle-cod-mac`). Keeping the returned loader alive
/// for the rest of `main` matches every CEF Rust example (it unloads the framework on drop).
///
/// `is_helper` matches every "Cod Helper*" name (#23 round 4), not only the plain "Cod Helper":
/// round 3 shipped only that one bundle and saw CEF start, create the browser and then make no
/// further progress at all (no frame ever painted, no network request until the helper was
/// killed). Chromium's macOS child-process launching derives a per-process-type executable path
/// from `Settings::browser_subprocess_path` by inserting " (Type)" before ".app" and before the
/// final path component whenever that path already looks like it points inside a nested app
/// bundle, which it does here; it looks for that derived path on its own regardless of what this
/// module points at, so the renderer, GPU, plugin and alerts process types all failed to launch
/// (round 4's diagnosis). `script/bundle-cod-mac` now bundles all five names (plain, GPU,
/// Renderer, Plugin, Alerts), so every one of them needs this same relative-path resolution.
#[cfg(target_os = "macos")]
fn load_framework() -> cef::library_loader::LibraryLoader {
    let exe = std::env::current_exe().expect("cod-browser: cannot read its own executable path");
    let is_helper = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("Cod Helper"));
    let loader = cef::library_loader::LibraryLoader::new(&exe, is_helper);
    if !loader.load() {
        eprintln!("cod-browser: cannot load the CEF framework next to {}", exe.display());
        std::process::exit(3);
    }
    loader
}

/// Where CEF should re-launch this same helper for its renderer, GPU and utility subprocesses on
/// macOS (#23 round 1). This only needs to name the plain "Cod Helper.app": Chromium derives the
/// four other process-type variants (GPU, Renderer, Plugin, Alerts) from this same path on its
/// own, inserting " (Type)" before ".app" and before the final path component (see
/// `load_framework`'s doc comment, round 4 diagnosis); `script/bundle-cod-mac` bundles all five
/// names so every one of those derived paths actually exists.
#[cfg(target_os = "macos")]
fn helper_subprocess_path() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("cod-browser: cannot read its own executable path");
    let macos_dir = exe.parent().expect("cod-browser: no Contents/MacOS parent");
    let contents_dir = macos_dir.parent().expect("cod-browser: no Contents parent");
    contents_dir
        .join("Frameworks")
        .join("Cod Helper.app")
        .join("Contents")
        .join("MacOS")
        .join("Cod Helper")
}

pub fn main() {
    // Must happen before any other CEF call on macOS (see `load_framework`); a no-op on Linux,
    // which links libcef the ordinary way, so `_library` only exists there to be dropped last.
    #[cfg(target_os = "macos")]
    let _library = load_framework();
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    // Round 2 diagnosis (Q3b): must also happen before any other CEF call, for every process type
    // (browser, renderer, GPU), the same as every cef-rs macOS example. See mac_app's own doc
    // comment for why skipping this is the likely cause of round 1's silent crash.
    #[cfg(target_os = "macos")]
    crate::mac_app::setup();
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

    let Some(cache) = switch_value("--cod-cache=") else {
        eprintln!("cod-browser: started without --cod-cache, it is meant to be started by Cod");
        std::process::exit(2);
    };
    // Cod starts the helper with one end of a socket pair as its standard input, so there is no socket path another process could connect to.
    // SAFETY: file descriptor 0 is owned by this process and nothing else in it uses standard input. It is never dropped here: it is replaced below.
    let original = ManuallyDrop::new(unsafe { UnixStream::from_raw_fd(0) });
    if original.local_addr().is_err() {
        eprintln!("cod-browser: standard input is not a socket, it is meant to be started by Cod");
        std::process::exit(2);
    }
    // Keep the connection on a close-on-exec duplicate and point standard input at /dev/null, so no process CEF starts can inherit the control socket.
    let stream = match original.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("cod-browser: cannot duplicate the connection: {error}");
            std::process::exit(2);
        }
    };
    match std::fs::File::open("/dev/null") {
        // SAFETY: both descriptors are valid; dup2 atomically makes descriptor 0 refer to /dev/null.
        Ok(null) if unsafe { libc::dup2(null.as_raw_fd(), 0) } == 0 => {}
        other => {
            eprintln!("cod-browser: cannot redirect standard input: {other:?}");
            std::process::exit(2);
        }
    }
    let (out, outgoing) = mpsc::channel::<Outgoing>();
    let pages: Pages = Arc::new(Mutex::new(HashMap::new()));

    let started = Instant::now();
    // COD_BROWSER_LOG_VERBOSE=1 (#23 round 4 diagnosis H1) raises CEF's own log (cef.log) to
    // verbose for a smoke run or a manual repro, without changing the production default.
    let log_severity = if std::env::var("COD_BROWSER_LOG_VERBOSE").is_ok_and(|value| value == "1")
    {
        LogSeverity::VERBOSE
    } else {
        LogSeverity::WARNING
    };
    let mut settings = Settings {
        windowless_rendering_enabled: 1,
        no_sandbox: 1,
        root_cache_path: CefString::from(cache.as_str()),
        log_file: CefString::from(format!("{cache}/cef.log").as_str()),
        log_severity,
        ..Default::default()
    };
    // Only Windows and Linux support `multi_threaded_message_loop` (CEF spins its own UI thread);
    // on macOS it must stay 0 (the default) and the main thread runs `run_message_loop` instead,
    // handled below. Round 2 found that macOS still needs `handle`'s calls marshaled onto that
    // thread (`post_to_ui_thread`, #23 round 3 diagnosis M1), unlike Linux: this is the one
    // deliberate difference `input_loop` has between the two platforms.
    #[cfg(target_os = "linux")]
    {
        settings.multi_threaded_message_loop = 1;
    }
    #[cfg(target_os = "macos")]
    {
        let helper = helper_subprocess_path();
        settings.browser_subprocess_path = CefString::from(&*helper.to_string_lossy());
    }
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

    // Linux has CEF run its own UI thread (`multi_threaded_message_loop` above), so this thread
    // is free to block reading the control socket directly, exactly as it always has.
    //
    // macOS has no such option: CEF's message loop must run on the thread that called
    // `initialize` (`run_message_loop`, blocking until something calls `quit_message_loop`), so
    // the control-socket read loop moves to its own thread instead, and this one blocks in
    // `run_message_loop` until `input_loop` asks it to stop. `handle`'s own CEF calls do not
    // change: see the `multi_threaded_message_loop` comment above for why they stay safe to call
    // from this reader thread rather than the one actually running CEF's message loop.
    #[cfg(target_os = "macos")]
    {
        let input_out = out.clone();
        let input_pages = pages.clone();
        std::thread::Builder::new()
            .name("cod-browser-input".into())
            .spawn(move || {
                // `input_loop` only returns by calling `std::process::exit` itself (on the
                // control socket closing), which ends the whole process immediately regardless
                // of which thread called it, so `run_message_loop` below never actually has to
                // return through `quit_message_loop`. It is still called here, defensively, in
                // case `input_loop` is ever changed to return normally instead.
                input_loop(stream, input_out, input_pages);
                quit_message_loop();
            })
            .expect("cod-browser: cannot start the input thread");
        // #23 round 3 diagnosis M1: makes it visible in cod-browser's own stderr (collected into
        // cod-stderr.log) whether this thread ever actually enters and returns from the message
        // loop, rather than inferring it indirectly from whether pages ever load.
        eprintln!("cod-browser: entering run_message_loop");
        run_message_loop();
        eprintln!("cod-browser: run_message_loop returned");
        shutdown();
        std::process::exit(0);
    }
    #[cfg(not(target_os = "macos"))]
    input_loop(stream, out, pages);
}

/// Reads one `ToBrowser` message at a time from the control socket and runs it, until the socket
/// closes (Cod exited) or sends something unreadable twice in a row is never the case here:
/// `ToBrowser::read_from` already resyncs after a single oversized message (RC review B4).
///
/// On macOS (#23 round 3 diagnosis M1), this thread only does socket I/O and decoding: every
/// `handle` call is marshaled onto CEF's own UI thread with `post_to_ui_thread` instead of
/// running directly here. On Linux this is unchanged: `handle` still runs on this thread, exactly
/// as before `multi_threaded_message_loop` was added for #16.
fn input_loop(stream: UnixStream, out: mpsc::Sender<Outgoing>, pages: Pages) {
    let mut input = BufReader::new(stream);
    loop {
        let message = match ToBrowser::read_from(&mut input) {
            Ok(message) => message,
            // Cod itself refuses to send an oversized `Create` (RC review B4); this only catches
            // a message that still slipped through. The stream already resynced (the bytes were
            // drained), so every other page tab keeps running instead of the whole helper exiting.
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                eprintln!("cod-browser: refused an oversized message ({error}), staying up");
                continue;
            }
            Err(error) => {
                // Cod closed the socket or exited: nothing is left to draw for.
                eprintln!("cod-browser: connection ended ({error}), exiting");
                std::process::exit(0);
            }
        };
        #[cfg(target_os = "macos")]
        {
            let out = out.clone();
            let pages = pages.clone();
            post_to_ui_thread(move || handle(message, &out, &pages));
        }
        #[cfg(not(target_os = "macos"))]
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
            Outgoing::MetaReady(page) => {
                let state = pages.lock().ok().and_then(|pages| pages.get(&page).cloned());
                let Some(state) = state else { continue };
                state.meta_pending.store(false, Ordering::Release);
                let meta = state
                    .meta
                    .lock()
                    .map(|mut meta| std::mem::take(&mut *meta))
                    .unwrap_or_default();
                let mut result = Ok(());
                if let Some(title) = meta.title {
                    result = FromBrowser::Title { page, title }.write_to(&mut writer);
                }
                if let (Ok(()), Some(url)) = (&result, meta.address) {
                    result = FromBrowser::Address { page, url }.write_to(&mut writer);
                }
                if let (Ok(()), Some(loading)) = (&result, meta.loading) {
                    result = FromBrowser::Loading { page, loading }.write_to(&mut writer);
                }
                result
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
            meta: Mutex::new(PageMeta::default()),
            meta_pending: AtomicBool::new(false),
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
        | ToBrowser::Close { page }
        | ToBrowser::Navigate { page, .. } => *page,
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
        ToBrowser::Navigate { action, .. } => {
            let Ok(slot) = state.browser.lock() else {
                return;
            };
            let Some(handle) = slot.as_ref() else {
                return;
            };
            match action {
                protocol::NAVIGATE_BACK => handle.0.go_back(),
                protocol::NAVIGATE_FORWARD => handle.0.go_forward(),
                _ => handle.0.reload(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RC review B5, final batch: a bridge call sharing this backpressure with ordinary page
    // console spam must never be the one it drops, or a submit landing in the same burst as a
    // flood of console output leaves `codctl open --html --wait` hanging for a reply nobody
    // sent.

    #[test]
    fn a_bridge_call_is_forwarded_even_once_the_backlog_is_completely_full() {
        let line = format!("{}{{\"name\":\"submit\",\"args\":{{}}}}", protocol::BRIDGE_PREFIX);
        assert!(forward_despite_backlog(&line, MAX_PENDING_CONSOLE));
        assert!(forward_despite_backlog(
            &line,
            MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM - 1
        ));
    }

    // RC review residual: bridge-prefixed lines bypassed the cap entirely, so a `--js` page
    // flooding them could grow the helper's queue without bound.
    #[test]
    fn a_flood_of_bridge_lines_stays_bounded() {
        let line = format!("{}{{\"name\":\"submit\",\"args\":{{}}}}", protocol::BRIDGE_PREFIX);
        assert!(!forward_despite_backlog(
            &line,
            MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM
        ));
    }

    #[test]
    fn an_ordinary_console_line_is_dropped_once_the_backlog_is_full() {
        assert!(forward_despite_backlog("just a log line", MAX_PENDING_CONSOLE - 1));
        assert!(!forward_despite_backlog("just a log line", MAX_PENDING_CONSOLE));
    }
}
