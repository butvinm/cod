//! Cod's side of the off-screen browser (issue #16): starts the `cod-browser` helper process, talks to it over a Unix socket and hands out one `Page` per browser view.
//!
//! The helper renders with CEF and sends BGRA frames; only the latest frame of each page is kept, and a `PageEvent::Frame` tells the GPUI side that a newer one is waiting.

use std::{
    collections::HashMap,
    io::BufReader,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::{Context as _, Result, bail};
use futures::channel::mpsc::UnboundedSender;

#[path = "../../cod_browser/src/protocol.rs"]
#[allow(dead_code)]
pub(crate) mod protocol;

use protocol::{FromBrowser, ToBrowser};

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

struct Slot {
    events: UnboundedSender<PageEvent>,
    latest: Mutex<Option<PageFrame>>,
    frame_signalled: AtomicBool,
}

struct Client {
    writer: Mutex<UnixStream>,
    slots: Mutex<HashMap<u32, Arc<Slot>>>,
    next_page: AtomicU32,
    helper: Mutex<Child>,
    alive: AtomicBool,
}

static CLIENT: OnceLock<std::result::Result<Arc<Client>, String>> = OnceLock::new();

/// Browser views opened and not yet closed, for the driver and the log.
static LIVE_BROWSERS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn live_browsers() -> usize {
    LIVE_BROWSERS.load(Ordering::Acquire)
}

fn helper_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("COD_BROWSER_BIN") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().context("finding the Cod executable")?;
    let sibling = exe.with_file_name("cod-browser");
    if sibling.exists() {
        return Ok(sibling);
    }
    bail!(
        "the cod-browser helper is not next to the Cod executable ({}); build it with `cargo build -p cod_browser` or set COD_BROWSER_BIN",
        sibling.display()
    )
}

fn client(data_dir: &Path) -> Result<Arc<Client>> {
    CLIENT
        .get_or_init(|| start(data_dir).map_err(|error| format!("{error:#}")))
        .clone()
        .map_err(anyhow::Error::msg)
}

fn start(data_dir: &Path) -> Result<Arc<Client>> {
    let helper = helper_path()?;
    let cache = data_dir.join("cef");
    std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;

    // The helper gets one end of a socket pair as its standard input: there is no path in the file system that another process could connect to.
    let (stream, helper_end) = UnixStream::pair().context("creating the helper connection")?;
    let mut command = Command::new(&helper);
    command
        .arg(format!("--cod-cache={}", cache.display()))
        .stdin(Stdio::from(OwnedFd::from(helper_end)));
    if let Ok(dir) = std::env::var("COD_CEF_DIR") {
        let library_path = match std::env::var("LD_LIBRARY_PATH") {
            Ok(existing) if !existing.is_empty() => format!("{dir}:{existing}"),
            _ => dir,
        };
        command.env("LD_LIBRARY_PATH", library_path);
    }
    let started = Instant::now();
    let child = command
        .spawn()
        .with_context(|| format!("starting {}", helper.display()))?;
    log::info!(
        "web_view: cod-browser helper {} started in {:?}",
        helper.display(),
        started.elapsed()
    );

    let client = Arc::new(Client {
        writer: Mutex::new(stream.try_clone()?),
        slots: Mutex::new(HashMap::new()),
        next_page: AtomicU32::new(1),
        helper: Mutex::new(child),
        alive: AtomicBool::new(true),
    });
    let reader_client = client.clone();
    std::thread::Builder::new()
        .name("cod-browser-reader".into())
        .spawn(move || read_loop(reader_client, stream))
        .context("starting the reader thread")?;
    Ok(client)
}

fn read_loop(client: Arc<Client>, stream: UnixStream) {
    let mut input = BufReader::with_capacity(1 << 16, stream);
    loop {
        let message = match FromBrowser::read_from(&mut input) {
            Ok(message) => message,
            Err(error) => {
                log::error!("web_view: connection to cod-browser lost: {error}");
                break;
            }
        };
        let slot = |page: u32| {
            client
                .slots
                .lock()
                .ok()
                .and_then(|slots| slots.get(&page).cloned())
        };
        match message {
            FromBrowser::Ready => log::info!("web_view: cod-browser is ready"),
            FromBrowser::Failed { message } => log::error!("web_view: cod-browser failed: {message}"),
            FromBrowser::Frame {
                page,
                width,
                height,
                bgra,
            } => {
                let Some(slot) = slot(page) else { continue };
                if let Ok(mut latest) = slot.latest.lock() {
                    *latest = Some(PageFrame {
                        width,
                        height,
                        bgra,
                    });
                }
                if !slot.frame_signalled.swap(true, Ordering::AcqRel) {
                    slot.events.unbounded_send(PageEvent::Frame).ok();
                }
            }
            FromBrowser::Created { page } => {
                LIVE_BROWSERS.fetch_add(1, Ordering::AcqRel);
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Created).ok();
                }
            }
            FromBrowser::Title { page, title } => {
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Title(title)).ok();
                }
            }
            FromBrowser::Address { page, url } => {
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Address(url)).ok();
                }
            }
            FromBrowser::Loading { page, loading } => {
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Loading(loading)).ok();
                }
            }
            FromBrowser::Console { page, message } => {
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Console(message)).ok();
                }
            }
            FromBrowser::Closed { page } => {
                let live = LIVE_BROWSERS
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                        Some(live.saturating_sub(1))
                    })
                    .unwrap_or(0)
                    .saturating_sub(1);
                log::info!("web_view: browser view {page} closed and freed, {live} still open");
                let removed = client
                    .slots
                    .lock()
                    .ok()
                    .and_then(|mut slots| slots.remove(&page));
                if let Some(slot) = removed {
                    slot.events.unbounded_send(PageEvent::Closed).ok();
                }
            }
        }
    }
    client.alive.store(false, Ordering::Release);
    if let Ok(mut slots) = client.slots.lock() {
        for (_, slot) in slots.drain() {
            slot.events.unbounded_send(PageEvent::Closed).ok();
        }
    }
    if let Ok(mut helper) = client.helper.lock() {
        helper.kill().ok();
        helper.wait().ok();
    }
}

impl Client {
    fn send(&self, message: &ToBrowser) {
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut writer) = self.writer.lock() else {
            return;
        };
        if let Err(error) = message.write_to(&mut *writer) {
            log::error!("web_view: cannot reach cod-browser: {error}");
            self.alive.store(false, Ordering::Release);
        }
    }
}

/// One browser view. All methods can be called from the GPUI thread; the helper does the work in its own process.
pub(crate) struct Page {
    client: Arc<Client>,
    id: u32,
    slot: Arc<Slot>,
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
        let client = client(data_dir)?;
        let id = client.next_page.fetch_add(1, Ordering::Relaxed);
        let slot = Arc::new(Slot {
            events,
            latest: Mutex::new(None),
            frame_signalled: AtomicBool::new(false),
        });
        client
            .slots
            .lock()
            .map_err(|_| anyhow::anyhow!("browser client state poisoned"))?
            .insert(id, slot.clone());
        client.send(&ToBrowser::Create {
            page: id,
            width,
            height,
            scale,
            url: url.to_owned(),
        });
        Ok(Self { client, id, slot })
    }

    /// The latest frame, if one arrived since the last call. Clears the signal so the next frame sends a new event.
    pub fn take_frame(&self) -> Option<PageFrame> {
        self.slot.frame_signalled.store(false, Ordering::Release);
        self.slot.latest.lock().ok()?.take()
    }

    pub fn resize(&self, width: i32, height: i32, scale: f32) {
        self.client.send(&ToBrowser::Resize {
            page: self.id,
            width,
            height,
            scale,
        });
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.client.send(&ToBrowser::Hidden {
            page: self.id,
            hidden,
        });
    }

    pub fn set_focus(&self, focused: bool) {
        self.client.send(&ToBrowser::Focus {
            page: self.id,
            focused,
        });
    }

    pub fn load_url(&self, url: &str) {
        self.client.send(&ToBrowser::Load {
            page: self.id,
            url: url.to_owned(),
        });
    }

    pub fn execute_javascript(&self, script: &str) {
        self.client.send(&ToBrowser::Script {
            page: self.id,
            script: script.to_owned(),
        });
    }

    pub fn mouse_move(&self, x: i32, y: i32, modifiers: u32, leave: bool) {
        self.client.send(&ToBrowser::MouseMove {
            page: self.id,
            x,
            y,
            modifiers,
            leave,
        });
    }

    pub fn mouse_button(
        &self,
        x: i32,
        y: i32,
        modifiers: u32,
        button: u8,
        up: bool,
        clicks: i32,
    ) {
        self.client.send(&ToBrowser::MouseButton {
            page: self.id,
            x,
            y,
            modifiers,
            button,
            up,
            clicks,
        });
    }

    pub fn mouse_wheel(&self, x: i32, y: i32, modifiers: u32, delta_x: i32, delta_y: i32) {
        self.client.send(&ToBrowser::Wheel {
            page: self.id,
            x,
            y,
            modifiers,
            delta_x,
            delta_y,
        });
    }

    pub fn key(&self, kind: u8, code: i32, character: u16, modifiers: u32) {
        self.client.send(&ToBrowser::Key {
            page: self.id,
            kind,
            code,
            character,
            modifiers,
        });
    }

    /// Asks the helper to close the view. `PageEvent::Closed` follows when it is gone.
    pub fn close(&self) {
        self.client.send(&ToBrowser::Close { page: self.id });
    }
}
