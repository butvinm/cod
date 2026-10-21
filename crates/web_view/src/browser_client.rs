//! Cod's side of the off-screen browser (issue #16): starts the `cod-browser` helper process, talks to it over a Unix socket and hands out one `Page` per browser view.
//!
//! The helper renders with CEF and sends BGRA frames; only the latest frame of each page is kept, and a `PageEvent::Frame` tells the GPUI side that a newer one is waiting.

use std::{
    collections::{HashMap, VecDeque},
    io::BufReader,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
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
    /// Title, address, loading state or console output changed: see `Page::take_diagnostics`.
    Diagnostics,
    Created,
    Closed,
    /// The helper died or was cut off: the page is gone and will not paint again.
    Failed(String),
    /// The main frame could not load a URL; the view stays usable.
    LoadFailed { url: String, error: String },
    /// The page wanted a new window for this URL; the helper cancelled it.
    PopupRequested(String),
}

/// Console messages kept per page between two looks by the UI; later ones are counted and dropped.
const MAX_PENDING_CONSOLE: usize = 64;

/// Whether a console line is kept at this backlog (RC review B5, final batch): an ordinary
/// diagnostic only below `MAX_PENDING_CONSOLE`, a bridge call (`BRIDGE_PREFIX`) also through a
/// full backlog of ordinary lines, up to `protocol::BRIDGE_HEADROOM` more, so a flood of
/// bridge-prefixed lines stays bounded.
fn keep_despite_backlog(message: &str, current_len: usize) -> bool {
    if message.starts_with(protocol::BRIDGE_PREFIX) {
        current_len < MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM
    } else {
        current_len < MAX_PENDING_CONSOLE
    }
}

/// What the page reported since the UI last looked. Title, address and loading state keep only the latest value, console output is capped, so a busy UI never accumulates an unbounded backlog.
#[derive(Default)]
pub(crate) struct Diagnostics {
    pub title: Option<String>,
    pub address: Option<String>,
    pub loading: Option<bool>,
    pub console: Vec<ConsoleLine>,
    pub dropped_console: usize,
}

/// One console line and the document CEF attributes it to (RC review B2 for `--js` pages).
pub(crate) struct ConsoleLine {
    pub message: String,
    pub source: String,
}

struct Slot {
    events: UnboundedSender<PageEvent>,
    latest: Mutex<Option<PageFrame>>,
    frame_signalled: AtomicBool,
    diagnostics: Mutex<Diagnostics>,
    diagnostics_signalled: AtomicBool,
}

impl Slot {
    /// Records a diagnostic change and wakes the UI once, however many more arrive before it looks.
    fn record(&self, update: impl FnOnce(&mut Diagnostics)) {
        if let Ok(mut diagnostics) = self.diagnostics.lock() {
            update(&mut diagnostics);
        }
        if !self.diagnostics_signalled.swap(true, Ordering::AcqRel) {
            self.events.unbounded_send(PageEvent::Diagnostics).ok();
        }
    }
}

/// Commands waiting for the writer thread. When it is full the helper is not keeping up.
const MAX_QUEUED_COMMANDS: usize = 512;
/// A write that blocks longer than this means the helper has stopped reading.
const WRITE_STALL: Duration = Duration::from_secs(5);
const START_TIMEOUT: Duration = Duration::from_secs(30);

struct Client {
    outbox: Mutex<VecDeque<ToBrowser>>,
    wake: Condvar,
    /// A second handle on the socket, to cut the connection when the helper is declared dead.
    control: UnixStream,
    slots: Mutex<HashMap<u32, Arc<Slot>>>,
    next_page: AtomicU32,
    helper: Mutex<Option<Child>>,
    alive: AtomicBool,
    live: AtomicUsize,
    ready: AtomicBool,
    write_started: Mutex<Option<Instant>>,
}

/// The running helper connection. A dead one is replaced by the next page that opens.
static CLIENT: Mutex<Option<Arc<Client>>> = Mutex::new(None);

/// Browser views of the running helper that were opened and not yet closed, for the driver and the log.
pub(crate) fn live_browsers() -> usize {
    CLIENT
        .lock()
        .ok()
        .and_then(|client| client.clone())
        .filter(|client| client.alive.load(Ordering::Acquire))
        .map_or(0, |client| client.live.load(Ordering::Acquire))
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

/// The live helper connection, starting a new helper when there is none or the last one died.
fn client(data_dir: &Path) -> Result<Arc<Client>> {
    let mut current = CLIENT
        .lock()
        .map_err(|_| anyhow::anyhow!("browser client state poisoned"))?;
    if let Some(client) = current.as_ref()
        && client.alive.load(Ordering::Acquire)
    {
        return Ok(client.clone());
    }
    let client = start(data_dir)?;
    *current = Some(client.clone());
    Ok(client)
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
    let client = Arc::new(Client {
        outbox: Mutex::new(VecDeque::new()),
        wake: Condvar::new(),
        control: stream.try_clone()?,
        slots: Mutex::new(HashMap::new()),
        next_page: AtomicU32::new(1),
        helper: Mutex::new(None),
        alive: AtomicBool::new(true),
        live: AtomicUsize::new(0),
        ready: AtomicBool::new(false),
        write_started: Mutex::new(None),
    });
    let reader_stream = stream.try_clone()?;
    let reader_client = client.clone();
    // If any thread cannot start, `fail` stops the ones already running (they exit when the client is dead and the socket is cut) and ends the helper if the writer already started it.
    let started = std::thread::Builder::new()
        .name("cod-browser-reader".into())
        .spawn(move || read_loop(reader_client, reader_stream))
        .map(drop)
        .context("starting the reader thread")
        .and_then(|()| {
            // Starting the process and every write to it happen on this thread, never on the GPUI thread.
            let writer_client = client.clone();
            std::thread::Builder::new()
                .name("cod-browser-writer".into())
                .spawn(move || write_loop(writer_client, command, helper, stream))
                .map(drop)
                .context("starting the writer thread")
        })
        .and_then(|()| {
            let watchdog_client = client.clone();
            std::thread::Builder::new()
                .name("cod-browser-watchdog".into())
                .spawn(move || watch(watchdog_client))
                .map(drop)
                .context("starting the watchdog thread")
        });
    if let Err(error) = started {
        client.fail(&format!("{error:#}"));
        return Err(error);
    }
    Ok(client)
}

fn write_loop(client: Arc<Client>, mut command: Command, helper: PathBuf, mut stream: UnixStream) {
    let started = Instant::now();
    match command.spawn() {
        Ok(child) => {
            log::info!(
                "web_view: cod-browser helper {} started in {:?}",
                helper.display(),
                started.elapsed()
            );
            // fail() sets `alive` to false and only then takes the child under this lock, so checking `alive` while holding it decides who owns the child: either fail() finds it and ends it, or the shutdown already happened and it is ended here.
            let mut slot = match client.helper.lock() {
                Ok(slot) => slot,
                Err(poisoned) => poisoned.into_inner(),
            };
            if client.alive.load(Ordering::Acquire) {
                *slot = Some(child);
            } else {
                drop(slot);
                let mut child = child;
                child.kill().ok();
                child.wait().ok();
                return;
            }
        }
        Err(error) => {
            client.fail(&format!("cannot start {}: {error}", helper.display()));
            return;
        }
    }
    // The command holds the helper's end of the socket until it is dropped.
    drop(command);
    loop {
        let message = {
            let Ok(mut outbox) = client.outbox.lock() else {
                return;
            };
            loop {
                if !client.alive.load(Ordering::Acquire) {
                    return;
                }
                if let Some(message) = outbox.pop_front() {
                    break message;
                }
                outbox = match client.wake.wait(outbox) {
                    Ok(outbox) => outbox,
                    Err(_) => return,
                };
            }
        };
        if let Ok(mut write_started) = client.write_started.lock() {
            *write_started = Some(Instant::now());
        }
        let result = message.write_to(&mut stream);
        if let Ok(mut write_started) = client.write_started.lock() {
            *write_started = None;
        }
        if let Err(error) = result {
            client.fail(&format!("cannot write to cod-browser: {error}"));
            return;
        }
    }
}

/// Notices a helper that exited, never became ready, or stopped reading its commands, and ends it.
fn watch(client: Arc<Client>) {
    let started = Instant::now();
    while client.alive.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(250));
        let exited = client
            .helper
            .lock()
            .ok()
            .and_then(|mut helper| helper.as_mut().and_then(|child| child.try_wait().ok().flatten()));
        if let Some(status) = exited {
            client.fail(&format!("the cod-browser helper exited: {status}"));
        } else if !client.ready.load(Ordering::Acquire) && started.elapsed() > START_TIMEOUT {
            client.fail("the cod-browser helper did not become ready in time");
        } else if client
            .write_started
            .lock()
            .ok()
            .and_then(|write_started| *write_started)
            .is_some_and(|since| since.elapsed() > WRITE_STALL)
        {
            client.fail("the cod-browser helper stopped reading commands");
        }
    }
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
            FromBrowser::Ready => {
                client.ready.store(true, Ordering::Release);
                log::info!("web_view: cod-browser is ready");
            }
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
                client.live.fetch_add(1, Ordering::AcqRel);
                if let Some(slot) = slot(page) {
                    slot.events.unbounded_send(PageEvent::Created).ok();
                }
            }
            FromBrowser::Title { page, title } => {
                if let Some(slot) = slot(page) {
                    slot.record(|diagnostics| diagnostics.title = Some(title));
                }
            }
            FromBrowser::Address { page, url } => {
                if let Some(slot) = slot(page) {
                    slot.record(|diagnostics| diagnostics.address = Some(url));
                }
            }
            FromBrowser::Loading { page, loading } => {
                if let Some(slot) = slot(page) {
                    slot.record(|diagnostics| diagnostics.loading = Some(loading));
                }
            }
            FromBrowser::LoadFailed { page, url, error } => {
                if let Some(slot) = slot(page) {
                    slot.events
                        .unbounded_send(PageEvent::LoadFailed { url, error })
                        .ok();
                }
            }
            FromBrowser::PopupRequested { page, url } => {
                if let Some(slot) = slot(page) {
                    slot.events
                        .unbounded_send(PageEvent::PopupRequested(url))
                        .ok();
                }
            }
            FromBrowser::Console { page, message, source } => {
                if let Some(slot) = slot(page) {
                    // RC review B5 (final batch): a bridge call (`cod:bridge:...`) must still
                    // reach `OffscreenView::handle_event` even once an unrelated flood of
                    // ordinary page console output has filled this queue, or a busy UI reading
                    // it late could drop the one line `codctl open --html --wait` is waiting
                    // for. Only ordinary diagnostics are best effort and capped; a bridge call
                    // always gets pushed.
                    slot.record(|diagnostics| {
                        if keep_despite_backlog(&message, diagnostics.console.len()) {
                            diagnostics.console.push(ConsoleLine { message, source });
                        } else {
                            diagnostics.dropped_console += 1;
                        }
                    });
                }
            }
            FromBrowser::Closed { page } => {
                let live = client
                    .live
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
    client.fail("the connection to cod-browser was lost");
}

/// Ends a helper process and collects its exit status on a thread of its own, so the caller (possibly the GPUI thread) never waits for it.
fn reap(mut child: Child) {
    child.kill().ok();
    let spawned = std::thread::Builder::new()
        .name("cod-browser-reaper".into())
        .spawn(move || {
            child.wait().ok();
        });
    if let Err(error) = spawned {
        log::error!("web_view: cannot start the reaper thread: {error}");
    }
}

impl Client {
    /// Queues a command for the writer thread; never blocks.
    /// Pointer moves and resizes replace an identical kind of command at the end of the queue, and when the queue is full pointer moves and wheel events are dropped.
    fn send(&self, message: &ToBrowser) {
        if !self.alive.load(Ordering::Acquire) {
            return;
        }
        let overflow = {
            let Ok(mut outbox) = self.outbox.lock() else {
                return;
            };
            let replaces_last = match (outbox.back(), message) {
                (
                    Some(ToBrowser::MouseMove { page: queued, .. }),
                    ToBrowser::MouseMove { page, .. },
                ) => queued == page,
                (
                    Some(ToBrowser::Resize { page: queued, .. }),
                    ToBrowser::Resize { page, .. },
                ) => queued == page,
                _ => false,
            };
            if replaces_last {
                if let Some(last) = outbox.back_mut() {
                    *last = message.clone();
                }
                false
            } else if outbox.len() >= MAX_QUEUED_COMMANDS {
                !matches!(message, ToBrowser::MouseMove { .. } | ToBrowser::Wheel { .. })
            } else {
                outbox.push_back(message.clone());
                self.wake.notify_one();
                false
            }
        };
        if overflow {
            self.fail("the cod-browser command queue is full");
        }
    }

    /// Declares the helper dead: stops the writer, cuts the connection and ends the process.
    fn fail(&self, reason: &str) {
        if !self.alive.swap(false, Ordering::AcqRel) {
            return;
        }
        log::error!("web_view: {reason}");
        self.control.shutdown(std::net::Shutdown::Both).ok();
        let child = self.helper.lock().ok().and_then(|mut helper| helper.take());
        if let Some(child) = child {
            reap(child);
        }
        self.live.store(0, Ordering::Release);
        // `alive` is already false. The writer checks it while holding the queue lock before it sleeps, so clearing the queue and waking it under that same lock means it cannot miss this wakeup and sleep forever.
        match self.outbox.lock() {
            Ok(mut outbox) => {
                outbox.clear();
                self.wake.notify_all();
            }
            Err(poisoned) => {
                poisoned.into_inner().clear();
                self.wake.notify_all();
            }
        }
        // Every open page is told, so its pane can say what happened instead of freezing.
        if let Ok(mut slots) = self.slots.lock() {
            for (_, slot) in slots.drain() {
                slot.events
                    .unbounded_send(PageEvent::Failed(reason.to_owned()))
                    .ok();
            }
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
            diagnostics: Mutex::new(Diagnostics::default()),
            diagnostics_signalled: AtomicBool::new(false),
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
        // The helper may have died between getting the client and registering the page.
        if !client.alive.load(Ordering::Acquire)
            && let Ok(mut slots) = client.slots.lock()
            && let Some(slot) = slots.remove(&id)
        {
            slot.events
                .unbounded_send(PageEvent::Failed("the cod-browser helper is not running".into()))
                .ok();
        }
        Ok(Self { client, id, slot })
    }

    /// The latest frame, if one arrived since the last call. Clears the signal so the next frame sends a new event.
    pub fn take_frame(&self) -> Option<PageFrame> {
        self.slot.frame_signalled.store(false, Ordering::Release);
        self.slot.latest.lock().ok()?.take()
    }

    /// What the page reported since the last call. Clears the signal so the next change sends a new event.
    pub fn take_diagnostics(&self) -> Diagnostics {
        self.slot.diagnostics_signalled.store(false, Ordering::Release);
        self.slot
            .diagnostics
            .lock()
            .map(|mut diagnostics| std::mem::take(&mut *diagnostics))
            .unwrap_or_default()
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

    /// Back, forward or reload: one of `protocol::NAVIGATE_*`.
    pub fn navigate(&self, action: u8) {
        self.client.send(&ToBrowser::Navigate {
            page: self.id,
            action,
        });
    }

    /// Asks the helper to close the view. `PageEvent::Closed` follows when it is gone.
    pub fn close(&self) {
        self.client.send(&ToBrowser::Close { page: self.id });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RC review B5, final batch: a bridge call sharing this backlog with ordinary page
    // console spam must never be the one it drops here either, or the engine's own exemption
    // for the same reason is undone again on Cod's side of the wire.

    #[test]
    fn a_bridge_call_is_kept_even_once_the_backlog_is_completely_full() {
        let line = format!("{}{{\"name\":\"submit\",\"args\":{{}}}}", protocol::BRIDGE_PREFIX);
        assert!(keep_despite_backlog(&line, MAX_PENDING_CONSOLE));
        assert!(keep_despite_backlog(
            &line,
            MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM - 1
        ));
    }

    // RC review residual: bridge-prefixed lines bypassed the cap entirely, so a `--js` page
    // flooding them could grow this queue without bound.
    #[test]
    fn a_flood_of_bridge_lines_stays_bounded() {
        let line = format!("{}{{\"name\":\"submit\",\"args\":{{}}}}", protocol::BRIDGE_PREFIX);
        assert!(!keep_despite_backlog(
            &line,
            MAX_PENDING_CONSOLE + protocol::BRIDGE_HEADROOM
        ));
    }

    #[test]
    fn an_ordinary_console_line_is_dropped_once_the_backlog_is_full() {
        assert!(keep_despite_backlog("just a log line", MAX_PENDING_CONSOLE - 1));
        assert!(!keep_despite_backlog("just a log line", MAX_PENDING_CONSOLE));
    }
}
