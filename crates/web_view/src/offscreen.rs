//! A workspace item that shows a web page drawn by an off-screen browser (issue #16).
//!
//! The browser (CEF, in the `cod-browser` helper process, see `browser_client`) renders into memory. Every frame becomes a fresh `RenderImage` that GPUI paints inside the pane like any other image, so the page is part of Cod's own window and works on native Wayland.
//! Mouse and keyboard events that GPUI delivers to the pane are forwarded to the browser.

use std::{
    cell::Cell,
    path::Path,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
};

use anyhow::Context as _;
use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, Bounds, Context, DispatchPhase, Entity, ImageId, EventEmitter, FocusHandle, Focusable,
    HitboxBehavior, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ObjectFit, Pixels, Point, Render, RenderImage,
    ScrollDelta, ScrollWheelEvent, SharedString, Styled as _,
    Subscription, Task, WeakEntity, Window, canvas, div, img, px,
};
use project::Project;
use smallvec::SmallVec;
use ui::prelude::*;
use util::ResultExt as _;
use workspace::{
    CodEntry, CodEntryKind, CodEntryOpen, CodStatus, CodStatusStore, ItemId, MultiWorkspaceEvent,
    SplitDirection, Workspace, WorkspaceId, delete_unloaded_items,
    item::{Item, ItemEvent, SerializableItem},
};

use crate::{
    browser_client::{
        Page, PageEvent, PageFrame,
        protocol::{
            self, BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT, EVENT_ALT, EVENT_COMMAND,
            EVENT_CONTROL, EVENT_LEFT_BUTTON, EVENT_MIDDLE_BUTTON, EVENT_RIGHT_BUTTON,
            EVENT_SHIFT,
        },
    },
    page_db::{self, CodPagesDb},
    pages::{self, PageId, PageInfo, PageKind},
};

/// Whether a page is loading, shows its page, or could not load it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageState {
    Loading,
    Loaded,
    Failed,
}

impl PageState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Loaded => "loaded",
            Self::Failed => "failed",
        }
    }
}

/// What a page tab tells its pane: the tab label follows the title, and a new URL or title is saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageItemEvent {
    TitleChanged,
    UrlChanged,
}

/// Pixels one wheel "line" scrolls, for devices that report lines.
const WHEEL_LINE_PIXELS: f32 = 40.0;

/// The off-screen browser is the Linux default (D24); `COD_WEBVIEW_BACKEND=webkitgtk` keeps the old X11 child-window spike.
pub(crate) fn backend_requested() -> bool {
    !std::env::var("COD_WEBVIEW_BACKEND").is_ok_and(|value| value == "webkitgtk")
}

pub(crate) fn init(cx: &mut App) {
    workspace::register_serializable_item::<OffscreenView>(cx);
    // Links from chats, files and previews open as page tabs (#32).
    workspace::cod_links::CodPageOpener::install(
        |workspace, url, window, cx| {
            open_or_focus_page(workspace, url, window, cx);
        },
        cx,
    );
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &crate::OpenUrl, window, cx| {
            crate::open_page_modal::OpenPageModal::toggle(workspace, window, cx);
        });
    })
    .detach();
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
    let id = pages::allocate_page_id(cx);
    let page = OffscreenView::build(
        url,
        id,
        PageKind::External,
        None,
        Some(workspace.weak_handle()),
        window,
        cx,
    );
    workspace.split_item(direction, Box::new(page.clone()), window, cx);
    Ok(page)
}

/// Opens a new page tab in the active pane, next to the files there, and focuses it. The browser starts when the tab is first painted.
pub(crate) fn open_page_in_active_pane(
    workspace: &mut Workspace,
    url: String,
    kind: PageKind,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<OffscreenView> {
    let id = pages::allocate_page_id(cx);
    let page = OffscreenView::build(
        url,
        id,
        kind,
        None,
        Some(workspace.weak_handle()),
        window,
        cx,
    );
    workspace.add_item_to_active_pane(Box::new(page.clone()), None, true, window, cx);
    page
}

/// The off-screen driver's entry point (#16).
pub(crate) fn open_url_in_active_pane(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> anyhow::Result<Entity<OffscreenView>> {
    Ok(open_page_in_active_pane(
        workspace,
        url,
        PageKind::External,
        window,
        cx,
    ))
}

/// Focuses the page tab of this workspace that shows `url`, or opens a new one (#32 AC4).
pub fn open_or_focus_page(
    workspace: &mut Workspace,
    url: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> Entity<OffscreenView> {
    let this = cx.entity();
    if let Some(existing) = pages::find_by_url(&this, &url, cx) {
        workspace.activate_item(&existing, true, true, window, cx);
        return existing;
    }
    open_page_in_active_pane(workspace, url, PageKind::External, window, cx)
}

pub struct OffscreenView {
    page_id: PageId,
    kind: PageKind,
    workspace: Option<WeakEntity<Workspace>>,
    url: SharedString,
    /// The agent document's own URL at the moment this page was built (RC review B2): bridge
    /// calls are only ever acted on while `url` still equals this. Always `None` for a page that
    /// is not `PageKind::Agent`.
    agent_document_url: Option<SharedString>,
    /// The bridge token of that agent document (`agent_bridge::document_token`): a bridge call
    /// is acted on only when it carries this token.
    agent_token: Option<String>,
    /// Set once the main frame's address changes away from `agent_document_url`: a same-tab
    /// navigation to another document (an external site, another `data:` URL) must not keep the
    /// bridge privileges of the agent document it replaced (RC review B2).
    agent_navigated_away: bool,
    title: Option<SharedString>,
    loading: bool,
    /// The last main-frame load error, cleared by the next load.
    load_error: Option<String>,
    /// Set once the browser was asked for, on the first paint with real bounds.
    browser_started: bool,
    events_tx: mpsc::UnboundedSender<PageEvent>,
    page: Option<Rc<Page>>,
    start_error: Option<String>,
    /// Set when the helper died: the pane shows this instead of a frozen page.
    failure: Option<String>,
    image: Option<Arc<RenderImage>>,
    focus_handle: FocusHandle,
    pane_bounds: Rc<Cell<Bounds<Pixels>>>,
    hidden: Rc<Cell<bool>>,
    /// The image and its pixel size that the last paint of the pane drew.
    painted: Rc<Cell<Option<(ImageId, (u32, u32))>>>,
    console: Vec<String>,
    pub(crate) frames_painted: u64,
    /// Total time spent turning frames into images, for the frame cost report.
    pub(crate) image_micros: u64,
    pub(crate) last_frame_size: (u32, u32),
    _events: Task<()>,
    _subscriptions: Vec<Subscription>,
    _multi_workspace: Option<Subscription>,
}

impl OffscreenView {
    /// A page tab with this ID, registered so links and `codctl` can find it.
    pub(crate) fn build(
        url: String,
        page_id: PageId,
        kind: PageKind,
        title: Option<String>,
        workspace: Option<WeakEntity<Workspace>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| {
            let mut view = Self::new(url, page_id.clone(), kind, window, cx);
            view.title = title.map(Into::into);
            view.workspace = workspace;
            view
        });
        pages::register(page_id, &view, cx);
        // Deferred: a page is often built inside an update of its workspace (opening a link), and the workspace cannot be read until that update ends.
        cx.defer({
            let view = view.clone();
            move |cx| view.update(cx, |view, cx| view.publish_status(cx))
        });
        view
    }

    fn new(
        url: String,
        page_id: PageId,
        kind: PageKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (events_tx, mut events_rx) = mpsc::unbounded();

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
        let this = cx.weak_entity();
        let page_focus = focus_handle.clone();
        let mut subscriptions = vec![
            // Back, forward and reload work while the page has the keyboard. Interceptors run before key bindings, so ctrl-r reloads the page instead of opening recent projects.
            cx.intercept_keystrokes(move |event, window, cx| {
                let Some(action) = navigation_for(&event.keystroke) else {
                    return;
                };
                if !page_focus.is_focused(window) {
                    return;
                }
                if this.update(cx, |this, cx| this.navigate(action, cx)).is_ok() {
                    cx.stop_propagation();
                }
            }),
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
                pages::unregister(&this.page_id, cx);
                if this.kind == PageKind::Agent {
                    // A no-op when the page already submitted, or nothing ever waited on it (#43 AC2).
                    crate::agent_bridge::closed(&this.page_id, cx);
                }
                if let Some(workspace_id) = this
                    .workspace
                    .as_ref()
                    .and_then(|workspace| workspace.upgrade())
                    .and_then(|workspace| workspace.read(cx).database_id())
                    && let Some(store) = CodStatusStore::try_global(cx)
                {
                    let key = this.page_id.to_string();
                    store.update(cx, |store, cx| store.remove_entry(workspace_id, &key, cx));
                }
                if let Some(image) = this.image.take() {
                    cx.drop_image(image, None);
                }
                if let Some(page) = &this.page {
                    log::info!("web_view: item released, closing its browser view");
                    page.close();
                }
            }),
        ];
        if kind == PageKind::Agent {
            // Keeps an agent page's theme variables current after a theme switch, without a
            // reload (#43 AC6).
            subscriptions.push(cx.observe_global::<settings::SettingsStore>(|this, cx| {
                if let Some(page) = &this.page {
                    let vars = crate::agent_bridge::current_theme_vars(cx);
                    page.execute_javascript(&crate::agent_bridge::theme_update_script(&vars));
                }
            }));
        }

        let agent_document_url = (kind == PageKind::Agent).then(|| SharedString::from(url.clone()));
        let agent_token = agent_document_url
            .as_deref()
            .and_then(crate::agent_bridge::document_token);
        Self {
            page_id,
            kind,
            workspace: None,
            url: url.into(),
            agent_document_url,
            agent_token,
            agent_navigated_away: false,
            title: None,
            loading: true,
            load_error: None,
            browser_started: false,
            events_tx,
            page: None,
            start_error: None,
            failure: None,
            image: None,
            focus_handle,
            pane_bounds: Rc::new(Cell::new(Bounds::default())),
            hidden: Rc::new(Cell::new(false)),
            painted: Rc::new(Cell::new(None)),
            console: Vec::new(),
            frames_painted: 0,
            image_micros: 0,
            last_frame_size: (0, 0),
            _events: events,
            _subscriptions: subscriptions,
            _multi_workspace: None,
        }
    }

    /// Starts the browser at the size the pane gives the page. Pages that are never shown (inactive tabs, workspaces not opened yet) never start one.
    fn start_browser(&mut self, bounds: Bounds<Pixels>, scale: f32) {
        if self.browser_started {
            return;
        }
        self.browser_started = true;
        log::info!(
            "web_view: starting the browser for {} at {:?}",
            self.page_id,
            bounds.size
        );
        match Page::new(
            &self.url,
            f32::from(bounds.size.width) as i32,
            f32::from(bounds.size.height) as i32,
            scale,
            paths::data_dir(),
            self.events_tx.clone(),
        ) {
            Ok(page) => self.page = Some(Rc::new(page)),
            Err(error) => self.start_error = Some(format!("{error:#}")),
        }
    }

    /// Starts the browser of a page that is not drawn, hidden, so it loads without painting. It shows, without a reload, when its tab is drawn.
    fn start_hidden(&mut self, bounds: Bounds<Pixels>, scale: f32, cx: &mut Context<Self>) {
        if self.browser_started {
            return;
        }
        self.hidden.set(true);
        self.start_browser(bounds, scale);
        if let Some(page) = &self.page {
            page.set_hidden(true);
        }
        cx.notify();
    }

    pub fn page_id(&self) -> &PageId {
        &self.page_id
    }

    pub fn kind(&self) -> PageKind {
        self.kind
    }

    pub fn workspace(&self) -> Option<&WeakEntity<Workspace>> {
        self.workspace.as_ref()
    }

    pub fn browser_started(&self) -> bool {
        self.browser_started
    }

    pub fn state(&self) -> PageState {
        if self.failure.is_some() || self.start_error.is_some() || self.load_error.is_some() {
            PageState::Failed
        } else if self.loading {
            PageState::Loading
        } else {
            PageState::Loaded
        }
    }

    pub fn info(&self) -> PageInfo {
        PageInfo {
            id: self.page_id.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
            state: self.state(),
            kind: self.kind,
            workspace: self.workspace.clone(),
        }
    }

    /// Publishes this page as a `CodEntry` of kind `Page` in its workspace's status store (#26), so the overview lists it and Enter on its row focuses this tab. Without a workspace, or before the workspace has a saved id, nothing is published yet; the next change (a loaded title, a failed load) tries again.
    fn publish_status(&self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.as_ref().and_then(|workspace| workspace.upgrade())
        else {
            return;
        };
        let Some(workspace_id) = workspace.read(cx).database_id() else {
            return;
        };
        let status = if self.state() == PageState::Failed {
            CodStatus::Error
        } else {
            CodStatus::Idle
        };
        let title = self.title.clone().unwrap_or_else(|| self.url.clone());
        let page_id = self.page_id.clone();
        let open: CodEntryOpen = Rc::new(move |window, cx| {
            let Some(view) = pages::find(&page_id, cx) else {
                return;
            };
            let Some(workspace) = view.read(cx).workspace().and_then(|workspace| workspace.upgrade())
            else {
                return;
            };
            workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&view, true, true, window, cx);
            });
        });
        CodStatusStore::global(cx).update(cx, |store, cx| {
            store.set_entry(
                workspace_id,
                CodEntry {
                    key: SharedString::from(self.page_id.to_string()),
                    kind: CodEntryKind::Page,
                    title,
                    status,
                    context: SharedString::from(page_context(&self.url)),
                    updated_at: std::time::SystemTime::now(),
                    open: Some(open),
                },
                cx,
            );
        });
    }

    /// Back, forward or reload (`protocol::NAVIGATE_*`). A page whose browser has not started yet ignores it.
    pub(crate) fn navigate(&mut self, action: u8, _cx: &mut Context<Self>) {
        if let Some(page) = &self.page {
            page.navigate(action);
        }
    }

    /// Acts on one bridge call from an agent page's bootstrap script (#43 AC2, AC3, AC4): checks
    /// the allowlist here, in Cod, then runs the matching action. Anything not in the allowlist is
    /// refused and logged, never acted on.
    ///
    /// The action itself runs deferred, after this view's update ends: closing the page tab (`submit`) and moving the focus to an editor or a chat (`open-file`, `focus-session`) read or release this view, which GPUI forbids while it is being updated (a double lease panic).
    fn dispatch_bridge_call(
        &mut self,
        call: crate::agent_bridge::BridgeCall,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if call.name == protocol::BRIDGE_OVERSIZED_ACTION {
            // RC review B5: the engine already gave this bridge call far more room than an
            // ordinary diagnostic (`protocol::MAX_BRIDGE_BYTES`); still over that, it sent this
            // sentinel instead of truncated, invalid JSON. Close the page so `codctl open --html
            // --wait` gets the already-defined "closed without submitting" outcome, instead of
            // waiting forever on a value it will never receive.
            log::warn!(
                "web_view: {} submitted a value too large for the bridge to carry; closing the page instead of leaving codctl waiting",
                self.page_id
            );
            let page_id = self.page_id.clone();
            window.defer(cx, move |window, cx| {
                if !pages::close(&page_id, window, cx) {
                    crate::agent_bridge::closed(&page_id, cx);
                }
            });
            return;
        }
        if !crate::agent_bridge::is_allowed(&call.name) {
            log::warn!(
                "web_view: {} refused the action {:?}: not in the bridge allowlist",
                self.page_id,
                call.name
            );
            return;
        }
        let page_id = self.page_id.clone();
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            run_bridge_action(page_id, workspace, call, window, cx)
        });
    }

    /// Whether a console line's reported source names the agent document this page was built
    /// from, so a bridge call from it should be trusted (RC review B2 for `--js` pages).
    ///
    /// Without `--js`, `frame-src 'none'` already keeps the page from embedding any subframe at
    /// all (RC review B2); with it, nothing stops an `<iframe>`, and the engine's `source` is
    /// the one thing that tells its own console output apart from the main document's. An empty
    /// source is treated as the main frame rather than refused: CEF reports it empty for some
    /// console lines, observed for the bootstrap script's own calls in this document, and
    /// refusing those would break every agent page's bridge, not just a forged one. A subframe's
    /// `source` is its own, different address and is refused; a source CEF never gives at all
    /// for a subframe's output (observed not to happen in `--js` end-to-end testing, but not
    /// guaranteed by the CEF API) would not be told apart from the main frame's own empty one,
    /// which is the known limit of this check.
    fn bridge_source_is_main_frame(&self, source: &str) -> bool {
        source_is_main_frame(self.agent_document_url.as_deref(), source)
    }

    fn hide(&mut self, reason: &str) {
        if !self.hidden.replace(true) {
            log::info!("web_view: {} hidden: {reason}", self.page_id);
            if let Some(page) = &self.page {
                page.set_hidden(true);
            }
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
            PageEvent::Diagnostics => {
                let Some(diagnostics) = self.page.as_ref().map(|page| page.take_diagnostics())
                else {
                    return;
                };
                if let Some(title) = diagnostics.title {
                    log::info!("web_view: title {title:?}");
                    if self.title.as_deref() != Some(title.as_str()) {
                        self.title = Some(title.into());
                        cx.emit(PageItemEvent::TitleChanged);
                    }
                }
                if let Some(url) = diagnostics.address {
                    log::info!("web_view: address {url}");
                    // RC review B2: once the main frame's address no longer names the agent
                    // document this page was built from, its bridge is refused for good, even if
                    // a later navigation happens to return to the same address. The engine
                    // truncates any address it reports to `protocol::MAX_URL_BYTES` (RC review
                    // #1), so a long agent document's own address comes back shortened on every
                    // load; treat that truncation of the original as still the same document.
                    let is_agent_document = self.agent_document_url.as_deref().is_some_and(
                        |agent_url| {
                            agent_url == url.as_str()
                                || protocol::limited(agent_url, protocol::MAX_URL_BYTES)
                                    == url.as_str()
                        },
                    );
                    if !is_agent_document && !self.agent_navigated_away {
                        self.agent_navigated_away = true;
                        log::warn!(
                            "web_view: {} navigated away from its agent document to {url:?}: its bridge is refused from now on",
                            self.page_id
                        );
                    }
                    if self.url.as_ref() != url.as_str() {
                        self.url = url.into();
                        cx.emit(PageItemEvent::UrlChanged);
                    }
                }
                if let Some(loading) = diagnostics.loading {
                    if loading {
                        self.load_error = None;
                    }
                    log::info!(
                        "web_view: loading={loading} title={:?}",
                        self.title.as_deref().unwrap_or_default()
                    );
                    self.loading = loading;
                }
                if diagnostics.dropped_console > 0 {
                    log::warn!(
                        "web_view: {} console messages dropped (the page logs faster than the UI reads)",
                        diagnostics.dropped_console
                    );
                }
                for line in diagnostics.console {
                    // Agent pages only, and only while still showing their own agent document
                    // (RC review B2) and from that document's own frame, not a subframe (RC
                    // review B2 for `--js` pages, where no CSP blocks `<iframe>` the way it does
                    // without `--js`): a bridge call from Cod's own bootstrap script is acted on
                    // and never shown as page console output (#43).
                    // The call must also carry the agent document's own token (RC review
                    // residual): an empty console source cannot be told apart from the main
                    // frame's, but a subframe of another origin cannot read the token.
                    if self.kind == PageKind::Agent
                        && !self.agent_navigated_away
                        && self.bridge_source_is_main_frame(&line.source)
                        && let Some(call) = crate::agent_bridge::parse_bridge_message(&line.message)
                        && self.agent_token.is_some()
                        && call.token == self.agent_token
                    {
                        self.dispatch_bridge_call(call, window, cx);
                        continue;
                    }
                    log::info!("web_view: page console: {}", line.message);
                    self.console.push(line.message);
                }
                if self.console.len() > 200 {
                    let excess = self.console.len() - 200;
                    self.console.drain(..excess);
                }
            }
            PageEvent::Created => {
                log::info!("web_view: browser view created");
                if let Some(page) = &self.page {
                    page.set_focus(self.focus_handle.is_focused(window));
                    // A hide sent before the view existed was dropped by the helper: send it again.
                    if self.hidden.get() {
                        page.set_hidden(true);
                    }
                }
            }
            PageEvent::Closed => {}
            PageEvent::LoadFailed { url, error } => {
                log::warn!("web_view: {} could not load {url}: {error}", self.page_id);
                self.load_error = Some(error);
            }
            PageEvent::PopupRequested(url) => {
                // A new window becomes a page tab of the same workspace, like a link from a chat.
                log::info!("web_view: {} asked for a new window: {url}", self.page_id);
                if let Some(workspace) = self.workspace.as_ref().and_then(|w| w.upgrade()) {
                    // Deferred: finding an open tab by URL reads every page, this one included.
                    window.defer(cx, move |window, cx| {
                        workspace.update(cx, |workspace, cx| {
                            workspace.cod_open_link(&url, window, cx);
                        });
                    });
                }
            }
            PageEvent::Failed(reason) => {
                log::error!("web_view: the page failed: {reason}");
                if let Some(image) = self.image.take() {
                    window.drop_image(image).log_err();
                }
                self.page = None;
                self.failure = Some(reason);
            }
        }
        self.publish_status(cx);
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

    /// The size in pixels of the image the last paint drew, and whether it is the newest one.
    pub(crate) fn painted_frame(&self) -> Option<((u32, u32), bool)> {
        let (id, size) = self.painted.get()?;
        let newest = self.image.as_ref().is_some_and(|image| image.id == id);
        Some((size, newest))
    }

    pub(crate) fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden.get()
    }

    pub(crate) fn page(&self) -> Option<Rc<Page>> {
        self.page.clone()
    }

    pub(crate) fn console(&self) -> &[String] {
        &self.console
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn current_url(&self) -> &str {
        &self.url
    }

    /// The page's drawn pixel bounds in the window, in the coordinate space mouse events use
    /// (#43 test support: a page's own `<button>` or `<a>` has no bounds of its own GPUI can
    /// report, so a test that clicks inside an agent page's content computes the point itself
    /// from this and the fixture's own, deterministic CSS layout).
    pub fn pane_bounds(&self) -> Bounds<Pixels> {
        self.pane_bounds.get()
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(page) = self.page.clone() else {
            return;
        };
        let keystroke = &event.keystroke;
        // Handled by the keystroke interceptor; the page never sees them.
        if navigation_for(keystroke).is_some() {
            cx.stop_propagation();
            return;
        }
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

/// Runs one allowed bridge call of the agent page `page_id` (#43 AC2, AC3). It runs deferred, with no page view being updated (see `OffscreenView::dispatch_bridge_call`).
fn run_bridge_action(
    page_id: PageId,
    workspace: Option<WeakEntity<Workspace>>,
    call: crate::agent_bridge::BridgeCall,
    window: &mut Window,
    cx: &mut App,
) {
    use crate::agent_bridge::arg_str;

    let workspace = workspace.and_then(|workspace| workspace.upgrade());
    match call.name.as_str() {
        "submit" => {
            let value = arg_str(&call, "value");
            log::info!("web_view: {page_id} submitted {value:?}");
            crate::agent_bridge::submit(&page_id, value, cx);
            // The waiter gets the value when the page view is released (its `on_release`); a page that cannot be closed answers at once.
            if !pages::close(&page_id, window, cx) {
                crate::agent_bridge::closed(&page_id, cx);
            }
        }
        "open-file" => open_file_from_page(&page_id, workspace, arg_str(&call, "path"), window, cx),
        "focus-session" => {
            focus_session_from_page(&page_id, workspace, arg_str(&call, "session"), window, cx)
        }
        other => log::warn!("web_view: {page_id} refused the action {other:?}: not in the bridge allowlist"),
    }
}

/// Whether `candidate`'s canonical path sits inside `root`'s canonical path (RC review B6):
/// resolves symlinks on both sides first, so a symlink entry inside the workspace whose target
/// escapes it is refused like any other path outside the workspace's folders. Blocking,
/// filesystem-bound: callers run it on the background executor, never on GPUI's foreground
/// thread (RC review, final batch).
fn path_is_contained(candidate: &Path, root: &Path) -> bool {
    std::fs::canonicalize(candidate)
        .ok()
        .zip(std::fs::canonicalize(root).ok())
        .is_some_and(|(candidate, root)| candidate.starts_with(&root))
}

/// `data-cod="open-file"` (#43 AC3): opens the path at its 1-based line and column in the editor, only when it resolves inside one of the workspace's own folders (security requirement #7). A path outside them, or a missing workspace, is refused and logged.
fn open_file_from_page(
    page_id: &PageId,
    workspace: Option<Entity<Workspace>>,
    raw: String,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(workspace) = workspace else {
        log::warn!("web_view: {page_id} refused to open {raw:?}: no workspace");
        return;
    };
    let (path_text, line, column) = crate::agent_bridge::split_path_position(&raw);
    let project = workspace.read(cx).project().clone();
    let Some(project_path) = project.read(cx).find_project_path(path_text, cx) else {
        log::warn!("web_view: {page_id} refused to open {raw:?}: not inside the workspace's folders");
        return;
    };
    // `find_project_path` only checks lexical containment: a symlink entry inside the workspace
    // whose target resolves outside it would otherwise still pass (RC review B6). Canonicalize
    // the actual target and re-check it against the worktree's own canonical root before opening
    // anything; refuse and log like every other containment failure here. The canonicalize calls
    // run on the background executor, never here on GPUI's foreground thread (RC review, final
    // batch): a stalled network mount or automount under the candidate path would otherwise
    // freeze the whole app for as long as the filesystem call takes, not just this one bridge
    // call.
    let Some(worktree) = project.read(cx).worktree_for_id(project_path.worktree_id, cx) else {
        log::warn!("web_view: {page_id} refused to open {raw:?}: unknown worktree");
        return;
    };
    let root = worktree.read(cx).abs_path();
    let candidate = root.join(project_path.path.as_std_path());
    // The file opens the way Zed opens a file: in the active pane.
    let page_id = page_id.clone();
    let path_text = path_text.to_owned();
    window
        .spawn(cx, async move |cx| {
            let contained = cx
                .background_executor()
                .spawn(async move { path_is_contained(&candidate, &root) })
                .await;
            if !contained {
                log::warn!(
                    "web_view: {page_id} refused to open {raw:?}: resolves outside the workspace's folders"
                );
                return anyhow::Ok(());
            }
            let item = workspace
                .update_in(cx, |workspace, window, cx| {
                    workspace.open_path(project_path, None, true, window, cx)
                })?
                .await?;
            if let Some(line) = line {
                workspace.update_in(cx, |_, window, cx| {
                    let Some(editor) = item.downcast::<editor::Editor>() else {
                        return;
                    };
                    editor.update(cx, |editor, cx| {
                        let Some(buffer) = editor.buffer().read(cx).as_singleton() else {
                            return;
                        };
                        let snapshot = buffer.read(cx).snapshot();
                        let point = snapshot.point_from_external_input(
                            line.saturating_sub(1),
                            column.unwrap_or(1).saturating_sub(1),
                        );
                        editor.go_to_singleton_buffer_point(point, window, cx);
                    });
                })?;
            }
            log::info!("web_view: {page_id} opened {path_text:?} from its page");
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}

/// `data-cod="focus-session"` (#43 AC3): focuses the chat with this session id, among the `Session` entries Cod's status store already has for this page's own workspace. An unknown session, or a missing workspace, is refused and logged.
fn focus_session_from_page(
    page_id: &PageId,
    workspace: Option<Entity<Workspace>>,
    session: String,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(workspace) = workspace else {
        log::warn!("web_view: {page_id} refused to focus session {session:?}: no workspace");
        return;
    };
    let Some(workspace_id) = workspace.read(cx).database_id() else {
        log::warn!("web_view: {page_id} refused to focus session {session:?}: the workspace has no id yet");
        return;
    };
    let entry = CodStatusStore::try_global(cx).and_then(|store| {
        store
            .read(cx)
            .entries(workspace_id)
            .iter()
            .find(|entry| entry.kind == CodEntryKind::Session && entry.key.as_ref() == session.as_str())
            .cloned()
    });
    match entry.and_then(|entry| entry.open) {
        Some(open) => {
            log::info!("web_view: {page_id} focused session {session:?}");
            open(window, cx);
        }
        None => log::warn!("web_view: {page_id} refused to focus session {session:?}: unknown session"),
    }
}

/// What a page tab's row in the pages database should carry for `url` (RC review B3): for an
/// agent page still showing the document it was built from, the complete, original `data:` URL
/// (`agent_document_url`), never the main frame's reported address, which the engine truncates to
/// `protocol::MAX_URL_BYTES` (RC review #1) for every page over about 6 KiB. A restart that
/// restores a truncated address gets invalid base64 and a broken or missing document; restoring
/// the original URL loads exactly what was shown before. Once the page has navigated away from
/// its own agent document (RC review B2's latch), the live address is what the tab actually shows
/// and is persisted as before, truncation and all, like any other page.
/// Whether a console line's `source` names `agent_document_url` (RC review B2 for `--js`
/// pages), truncation of a long address included; an empty source is treated as a match. See
/// `OffscreenView::bridge_source_is_main_frame`, which this backs, for why.
fn source_is_main_frame(agent_document_url: Option<&str>, source: &str) -> bool {
    if source.is_empty() {
        return true;
    }
    agent_document_url.is_some_and(|agent_url| {
        agent_url == source || protocol::limited(agent_url, protocol::MAX_URL_BYTES) == source
    })
}

fn persisted_url(
    kind: PageKind,
    agent_document_url: Option<&str>,
    agent_navigated_away: bool,
    url: &str,
) -> String {
    if kind == PageKind::Agent && !agent_navigated_away {
        if let Some(agent_url) = agent_document_url {
            return agent_url.to_owned();
        }
    }
    url.to_owned()
}

/// What a page tab's row in the pages database should carry for `kind` (RC review B2, final
/// batch): persisting `PageKind::Agent` once the page has navigated away from its own agent
/// document would restore it with bridge rights for whatever external address it last showed,
/// because `OffscreenView::build` grants a fresh page's `agent_document_url` from its restored
/// `url` whenever `kind` is `Agent`. Once navigated away, persist the page as an ordinary one: an
/// agent page's bridge is refused for good in this session already (the `agent_navigated_away`
/// latch), and restoring it as `External` keeps that true across a restart too.
fn persisted_kind(kind: PageKind, agent_navigated_away: bool) -> PageKind {
    if kind == PageKind::Agent && agent_navigated_away {
        PageKind::External
    } else {
        kind
    }
}

/// A strictly increasing write stamp (RC review B2, final batch). Workspace's own serialization
/// can call `OffscreenView::serialize` for the same item more than once (its throttled background
/// queue and its quit-time flush both can), each spawning its own independent write with no
/// ordering between the two; reading this here, synchronously, before either write is spawned, is
/// what lets `CodPagesDb::save_page` keep only the one whose call actually came last, regardless
/// of which finishes last. The stamp is wall-clock microseconds, raised to one more than the last
/// stamp when the clock has not moved on, so a stamp of this run is also above every stamp a
/// previous run saved: a per-run counter starting at zero would make every save after a restart
/// lose against the rows the previous run left.
fn serialize_clock() -> i64 {
    static CLOCK: AtomicI64 = AtomicI64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX));
    let mut last = CLOCK.load(Ordering::Relaxed);
    loop {
        let next = now.max(last.saturating_add(1));
        match CLOCK.compare_exchange_weak(last, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(current) => last = current,
        }
    }
}

/// The page's context line for the overview (#26): its host and path, for example `tracker.example.com/PAY-1234`. Falls back to the raw URL when it does not parse.
fn page_context(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path()),
        Err(_) => url.to_string(),
    }
}

/// The browser keys of a focused page: alt-left goes back, alt-right goes forward, ctrl-r reloads.
fn navigation_for(keystroke: &Keystroke) -> Option<u8> {
    let modifiers = &keystroke.modifiers;
    if modifiers.shift || modifiers.platform || modifiers.function {
        return None;
    }
    match (modifiers.alt, modifiers.control, keystroke.key.as_str()) {
        (true, false, "left") => Some(protocol::NAVIGATE_BACK),
        (true, false, "right") => Some(protocol::NAVIGATE_FORWARD),
        (false, true, "r") => Some(protocol::NAVIGATE_RELOAD),
        _ => None,
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
        "insert" => 0x2D,
        key if function_key_number(key).is_some() => 0x70 + function_key_number(key)? - 1,
        key => {
            let mut chars = key.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                // Other named keys have no forwarding yet.
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

/// 1 to 12 for "f1" to "f12".
fn function_key_number(key: &str) -> Option<i32> {
    let number: i32 = key.strip_prefix('f')?.parse().ok()?;
    (1..=12).contains(&number).then_some(number)
}

/// When a page is drawn, its workspace is the one shown: start the browsers of the workspace's other page tabs that have none yet, hidden (#31 AC5).
/// So the inactive page tabs of a restored workspace load again like the ones in front, while pages of workspaces that are not shown still start none.
fn start_pages_beside(page: &Entity<OffscreenView>, bounds: Bounds<Pixels>, scale: f32, cx: &mut App) {
    let Some(workspace) = page
        .read(cx)
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.upgrade())
    else {
        return;
    };
    let waiting: Vec<Entity<OffscreenView>> = workspace
        .read(cx)
        .panes()
        .iter()
        .flat_map(|pane| {
            // The active tab of a pane is drawn itself and starts its browser at its own size.
            let pane = pane.read(cx);
            let active = pane.active_item().map(|item| item.item_id());
            pane.items_of_type::<OffscreenView>()
                .filter(|other| Some(other.entity_id()) != active)
                .collect::<Vec<_>>()
        })
        .filter(|other| other != page && !other.read(cx).browser_started)
        .collect();
    for other in waiting {
        log::info!(
            "web_view: {} is a tab of the shown workspace, starting its browser hidden",
            other.read(cx).page_id
        );
        other.update(cx, |other, cx| other.start_hidden(bounds, scale, cx));
    }
}

fn page_point(position: Point<Pixels>, bounds: Bounds<Pixels>) -> (i32, i32) {
    (
        f32::from(position.x - bounds.origin.x) as i32,
        f32::from(position.y - bounds.origin.y) as i32,
    )
}

impl EventEmitter<PageItemEvent> for OffscreenView {}

impl Focusable for OffscreenView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Item for OffscreenView {
    type Event = PageItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.title
            .clone()
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| self.url.clone())
    }

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Link))
    }

    fn tab_tooltip_text(&self, _: &App) -> Option<SharedString> {
        Some(self.url.clone())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        None
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            PageItemEvent::TitleChanged | PageItemEvent::UrlChanged => f(ItemEvent::UpdateTab),
        }
    }

    fn can_split(&self) -> bool {
        true
    }

    /// The split shows the page's current URL in a new page tab with its own ID.
    fn clone_on_split(
        &self,
        _workspace_id: Option<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Entity<Self>>> {
        let id = pages::allocate_page_id(cx);
        let view = Self::build(
            self.url.to_string(),
            id,
            self.kind,
            self.title.as_ref().map(|title| title.to_string()),
            self.workspace.clone(),
            window,
            cx,
        );
        Task::ready(Some(view))
    }

    fn added_to_workspace(
        &mut self,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this_workspace = workspace.weak_handle();
        self.workspace = Some(this_workspace.clone());
        // When the window switches to another workspace, this page is not drawn any more: tell the browser so it stops painting. It shows again, without a reload, on the next paint.
        self._multi_workspace = workspace
            .multi_workspace()
            .and_then(|multi| multi.upgrade())
            .map(|multi| {
                cx.subscribe_in(&multi, window, move |this, multi, event, _window, cx| {
                    if let MultiWorkspaceEvent::ActiveWorkspaceChanged { .. } = event
                        && multi.read(cx).workspace().downgrade() != this_workspace
                    {
                        this.hide("its workspace is not the active one");
                    }
                })
            });
    }

    fn deactivated(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.hide("tab deactivated");
    }

    fn on_removed(&self, _cx: &mut Context<Self>) {
        log::info!("web_view: item removed from its pane, closing the browser view");
        if let Some(page) = &self.page {
            page.close();
        }
    }
}

impl SerializableItem for OffscreenView {
    fn serialized_item_kind() -> &'static str {
        "CodPage"
    }

    fn cleanup(
        workspace_id: WorkspaceId,
        alive_items: Vec<ItemId>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let db = CodPagesDb::global(cx);
        delete_unloaded_items(alive_items, workspace_id, page_db::TABLE, &db, cx)
    }

    /// Restores the tab from its saved row. The browser is not started here, so a restore never fails because of the browser; it starts when the tab is first shown.
    fn deserialize(
        _project: Entity<Project>,
        workspace: WeakEntity<Workspace>,
        workspace_id: WorkspaceId,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Entity<Self>>> {
        let saved = CodPagesDb::global(cx).get_page(item_id, workspace_id);
        window.spawn(cx, async move |cx| {
            let saved = saved?.context("no saved page for this item")?;
            cx.update(|window, cx| {
                let kind = PageKind::parse(&saved.kind);
                // An agent page saved before bridge tokens existed comes back in the current form,
                // or its bridge would refuse every call; unless the upgrade no longer fits in the
                // helper's Create message, where the page keeps loading as saved.
                let url = match kind {
                    PageKind::Agent => crate::agent_bridge::upgrade_legacy_document(
                        &saved.url,
                        &crate::agent_bridge::current_theme_vars(cx),
                    )
                    .filter(|upgraded| {
                        crate::browser_client::protocol::create_message_fits(upgraded.len())
                    })
                    .unwrap_or(saved.url),
                    _ => saved.url,
                };
                OffscreenView::build(
                    url,
                    PageId(saved.page_id.into()),
                    kind,
                    saved.title,
                    Some(workspace),
                    window,
                    cx,
                )
            })
        })
    }

    fn serialize(
        &mut self,
        workspace: &mut Workspace,
        item_id: ItemId,
        _closing: bool,
        cx: &mut Context<Self>,
    ) -> Option<Task<anyhow::Result<()>>> {
        let workspace_id = workspace.database_id()?;
        let db = CodPagesDb::global(cx);
        let page_id = self.page_id.to_string();
        let url = persisted_url(
            self.kind,
            self.agent_document_url.as_deref(),
            self.agent_navigated_away,
            self.url.as_ref(),
        );
        let title = self.title.as_ref().map(|title| title.to_string());
        let kind = persisted_kind(self.kind, self.agent_navigated_away)
            .as_str()
            .to_string();
        // RC review B2, final batch: read here, synchronously, before either this task or any
        // other `serialize()` call for the same item is spawned, so it orders writes by when
        // they were actually requested, not by whenever their background task happens to run.
        let written_at = serialize_clock();
        Some(cx.background_spawn(async move {
            db.save_page(item_id, workspace_id, page_id, url, title, kind, written_at)
                .await
        }))
    }

    fn should_serialize(&self, event: &Self::Event) -> bool {
        matches!(
            event,
            PageItemEvent::UrlChanged | PageItemEvent::TitleChanged
        )
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
        if let Some(reason) = &self.failure {
            return div()
                .size_full()
                .p_4()
                .child(format!(
                    "The off-screen browser stopped: {reason}. Open the page again to start a new one."
                ))
                .into_any_element();
        }
        let scale = window.scale_factor();
        let page = self.page.clone();
        let pane_bounds = self.pane_bounds.clone();
        let hidden = self.hidden.clone();
        let focus_handle = self.focus_handle.clone();
        let image = self.image.clone();
        let shown_size = self.last_frame_size;
        // Recorded when GPUI paints the pane, so the driver can tell a frame that was built from one that was drawn.
        let painted = self.painted.clone();
        let painted_marker = image.as_ref().map(|image| (image.id, shown_size));
        let needs_browser = !self.browser_started;
        let this = cx.weak_entity();

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
                        move |bounds, window, cx| {
                            let mut drawn_first = false;
                            if needs_browser
                                && bounds.size.width > px(0.0)
                                && bounds.size.height > px(0.0)
                            {
                                let scale = window.scale_factor();
                                this.update(cx, |this, cx| {
                                    this.start_browser(bounds, scale);
                                    cx.notify();
                                })
                                .log_err();
                                drawn_first = true;
                            }
                            if pane_bounds.get() != bounds {
                                log::info!(
                                    "web_view: pane bounds {:?} scale {}",
                                    bounds.size,
                                    window.scale_factor()
                                );
                                pane_bounds.set(bounds);
                            }
                            let shown_again = hidden.replace(false);
                            if shown_again {
                                log::info!("web_view: tab shown, page visible");
                                // The browser may have started hidden after this render, so read the page now.
                                let current = page.clone().or_else(|| {
                                    this.upgrade().and_then(|this| this.read(cx).page.clone())
                                });
                                if let Some(page) = &current {
                                    page.set_hidden(false);
                                }
                            }
                            if (drawn_first || shown_again)
                                && bounds.size.width > px(0.0)
                                && bounds.size.height > px(0.0)
                                && let Some(this) = this.upgrade()
                            {
                                start_pages_beside(&this, bounds, window.scale_factor(), cx);
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
                        painted.set(painted_marker);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str) -> Keystroke {
        Keystroke {
            modifiers: Modifiers::default(),
            key: key.into(),
            key_char: None,
        }
    }

    // RC review B3: an agent page over about 6 KiB must persist its complete, original document,
    // never the main frame's reported address, which the engine truncates.

    #[test]
    fn an_agent_page_still_showing_its_own_document_persists_the_original_url_not_the_truncated_address() {
        let original = format!("data:text/html;base64,{}", "A".repeat(20_000));
        let truncated = protocol::limited(&original, protocol::MAX_URL_BYTES);
        assert_ne!(truncated.len(), original.len(), "the fixture must actually get truncated");
        assert_eq!(
            persisted_url(PageKind::Agent, Some(&original), false, &truncated),
            original
        );
    }

    #[test]
    fn an_agent_page_navigated_away_persists_its_live_address_like_any_other_page() {
        let original = "data:text/html;base64,AAAA";
        let live = "https://example.org/elsewhere";
        assert_eq!(persisted_url(PageKind::Agent, Some(original), true, live), live);
    }

    // RC review B6 / final batch: the containment check itself, independent of where it runs.

    #[test]
    fn a_path_inside_the_root_is_contained() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("inside.txt");
        std::fs::write(&file, "hi").unwrap();
        assert!(path_is_contained(&file, root.path()));
    }

    #[test]
    fn a_symlink_escaping_the_root_is_not_contained() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.txt");
        std::fs::write(&target, "shh").unwrap();
        let link = root.path().join("escape.txt");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!path_is_contained(&link, root.path()));
    }

    #[test]
    fn a_non_agent_page_always_persists_its_live_address() {
        let live = "https://example.org/wiki/Home";
        assert_eq!(persisted_url(PageKind::External, None, false, live), live);
    }

    // RC review B2 (final batch): a restart must not re-grant bridge rights to whatever
    // external address an agent page last navigated to.

    // A save stamp of this run must beat every stamp a previous run left in the database, or the
    // conditional upsert in `CodPagesDb::save_page` silently keeps the old row after a restart.
    #[test]
    fn save_stamps_increase_and_start_above_any_earlier_run() {
        // 2026-01-01 in microseconds: a previous run's stamps are wall-clock readings below now.
        let earlier_run_stamp = 1_767_225_600_000_000;
        let first = serialize_clock();
        let second = serialize_clock();
        assert!(first > earlier_run_stamp);
        assert!(second > first);
    }

    #[test]
    fn an_agent_page_navigated_away_is_persisted_as_an_ordinary_page() {
        assert_eq!(persisted_kind(PageKind::Agent, true), PageKind::External);
    }

    #[test]
    fn an_agent_page_still_on_its_own_document_is_persisted_as_agent() {
        assert_eq!(persisted_kind(PageKind::Agent, false), PageKind::Agent);
    }

    #[test]
    fn a_non_agent_page_stays_external_regardless_of_the_navigated_flag() {
        assert_eq!(persisted_kind(PageKind::External, true), PageKind::External);
        assert_eq!(persisted_kind(PageKind::External, false), PageKind::External);
    }

    // RC review B2 for `--js` pages: a bridge call's console line is only trusted when its
    // source names the agent document, not a subframe's own, different address.

    #[test]
    fn a_source_matching_the_agent_document_is_the_main_frame() {
        let agent_url = "data:text/html;base64,AAAA";
        assert!(source_is_main_frame(Some(agent_url), agent_url));
    }

    #[test]
    fn an_empty_source_is_treated_as_the_main_frame() {
        assert!(source_is_main_frame(Some("data:text/html;base64,AAAA"), ""));
        assert!(source_is_main_frame(None, ""));
    }

    #[test]
    fn a_different_source_is_a_subframe_not_the_main_frame() {
        let agent_url = "data:text/html;base64,AAAA";
        assert!(!source_is_main_frame(Some(agent_url), "data:text/html,<iframe-doc>"));
        assert!(!source_is_main_frame(None, "data:text/html,<iframe-doc>"));
    }

    #[test]
    fn a_source_matching_the_truncated_agent_document_is_still_the_main_frame() {
        let agent_url = format!("data:text/html;base64,{}", "A".repeat(20_000));
        let truncated = protocol::limited(&agent_url, protocol::MAX_URL_BYTES);
        assert!(source_is_main_frame(Some(&agent_url), &truncated));
    }

    #[test]
    fn the_overview_context_is_the_host_and_path() {
        assert_eq!(page_context("https://tracker.example.com/PAY-1234"), "tracker.example.com/PAY-1234");
        assert_eq!(page_context("http://127.0.0.1:8000/wiki/Home"), "127.0.0.1/wiki/Home");
        assert_eq!(page_context("not a url"), "not a url");
    }

    #[test]
    fn editing_keys_map_to_windows_key_codes() {
        assert_eq!(windows_key_code(&keystroke("backspace")), Some(0x08));
        assert_eq!(windows_key_code(&keystroke("enter")), Some(0x0D));
        assert_eq!(windows_key_code(&keystroke("left")), Some(0x25));
        assert_eq!(windows_key_code(&keystroke("right")), Some(0x27));
        assert_eq!(windows_key_code(&keystroke("delete")), Some(0x2E));
    }

    #[test]
    fn letters_and_digits_map_to_their_upper_case_codes() {
        assert_eq!(windows_key_code(&keystroke("h")), Some('H' as i32));
        assert_eq!(windows_key_code(&keystroke("H")), Some('H' as i32));
        assert_eq!(windows_key_code(&keystroke("7")), Some('7' as i32));
    }

    #[test]
    fn keys_without_a_mapping_are_not_forwarded() {
        assert_eq!(windows_key_code(&keystroke("f13")), None);
        assert_eq!(windows_key_code(&keystroke("f0")), None);
        assert_eq!(windows_key_code(&keystroke("")), None);
    }

    #[test]
    fn function_keys_f1_to_f12_are_forwarded() {
        assert_eq!(windows_key_code(&keystroke("f1")), Some(0x70));
        assert_eq!(windows_key_code(&keystroke("f5")), Some(0x74));
        assert_eq!(windows_key_code(&keystroke("f12")), Some(0x7B));
    }

    #[test]
    fn modifier_flags_carry_modifiers_and_the_pressed_button() {
        let modifiers = Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            modifier_flags(&modifiers, Some(MouseButton::Left)),
            EVENT_CONTROL | EVENT_SHIFT | EVENT_LEFT_BUTTON
        );
        assert_eq!(modifier_flags(&Modifiers::default(), None), 0);
    }

    #[test]
    fn page_points_are_relative_to_the_pane() {
        let bounds = Bounds {
            origin: gpui::point(px(100.0), px(40.0)),
            size: gpui::size(px(300.0), px(200.0)),
        };
        assert_eq!(page_point(gpui::point(px(150.0), px(90.0)), bounds), (50, 50));
    }

    #[test]
    fn browser_keys_go_back_forward_and_reload() {
        let action = |text: &str| navigation_for(&Keystroke::parse(text).unwrap());
        assert_eq!(action("alt-left"), Some(protocol::NAVIGATE_BACK));
        assert_eq!(action("alt-right"), Some(protocol::NAVIGATE_FORWARD));
        assert_eq!(action("ctrl-r"), Some(protocol::NAVIGATE_RELOAD));
        assert_eq!(action("left"), None);
        assert_eq!(action("ctrl-left"), None);
        assert_eq!(action("ctrl-shift-r"), None);
        assert_eq!(action("r"), None);
    }
}
