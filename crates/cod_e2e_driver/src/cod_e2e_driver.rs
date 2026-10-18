//! The driver of Cod's end-to-end tests (issue #22).
//!
//! Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket once its first workspace window is open. Without the variable nothing here runs.
//! The socket's folder must be a private folder of the same user (no access for group and others); the driver refuses to start otherwise, and also when a live socket or any other file is at the path.
//! The protocol is one JSON object per line, answered by one JSON line (see `tests/e2e/cod_e2e.py`):
//!
//! - `{"cmd":"keys","keys":"ctrl-p enter"}` presses each space-separated keystroke (Zed keystroke syntax). Reply `{"ok":true}`.
//! - `{"cmd":"type","text":"invoice"}` presses one key per character, with the character as the typed text. Reply `{"ok":true}`.
//! - `{"cmd":"state"}` replies `{"ok":true,"state":{...}}` with the fields of `read_state` (listed in `tests/e2e/README.md`).
//! - A failed request replies `{"ok":false,"error":"..."}`.
//!
//! Keys enter the window the way the Linux platform layer delivers them: `Window::dispatch_event` with `PlatformInput::KeyDown` and `KeyUp`, and when the key down is not handled and carries text, the window's text input receives that text (`Window::dispatch_keystroke` does both).
//! So the keymap, the focus and the text input behave as they do for a user. The driver never dispatches actions and never calls into views; `state` only reads what the window shows.
//! The socket is served on background threads. Every request is handed to one foreground task through a channel, so the GPUI thread never waits for a client.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::{cell::RefCell, rc::Rc, time::Duration};

use anyhow::{Context as _, Result, anyhow, bail};
use command_palette::CommandPaletteDelegate;
use editor::{Editor, ToPoint as _};
use fuzzy_nucleo::{Case, LengthPenalty, StringMatchCandidate};
use gpui::{
    App, AppContext as _, AsyncApp, Entity, Keystroke, Modifiers, SharedString, WeakEntity, Window,
    WindowHandle,
};
use open_path_prompt::OpenPathDelegate;
use picker::{Picker, PickerDelegate as _};
use serde::Deserialize;
use serde_json::{Value, json};
use settings::{Settings as _, SettingsStore};
use workspace::{ItemHandle, MultiWorkspace, Pane, Workspace, WorkspaceSettings};

/// The environment variable that names the driver socket.
pub const SOCKET_ENV: &str = "COD_E2E_SOCKET";

/// How often the driver looks for the first workspace window before it opens the socket.
const WINDOW_POLL: Duration = Duration::from_millis(50);

/// The pause between two key presses of one request, so the app runs its event loop in between as it does for a person typing.
const KEY_PAUSE: Duration = Duration::from_millis(10);

/// Starts the driver when `COD_E2E_SOCKET` is set. Does nothing otherwise.
#[cfg(unix)]
pub fn init(cx: &mut App) {
    let Some(path) = std::env::var_os(SOCKET_ENV).filter(|value| !value.is_empty()) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let memory = Rc::new(RefCell::new(Memory::default()));
    // The command palette keeps its picker private; the driver remembers each new palette picker to read its query and its matches.
    cx.observe_new({
        let memory = memory.clone();
        move |_: &mut Picker<CommandPaletteDelegate>, _, cx| {
            memory.borrow_mut().palette = Some(cx.weak_entity());
        }
    })
    .detach();
    cx.spawn(async move |cx| serve(path, memory, cx).await).detach();
}

/// What the driver remembers between requests. It only ever holds observations, never anything the app reads.
#[derive(Default)]
struct Memory {
    /// The picker of the newest command palette.
    palette: Option<WeakEntity<Picker<CommandPaletteDelegate>>>,
    /// The command names the palette would list if it opened now, taken before every key press while no palette is open.
    /// The palette builds its list from the same call (`Window::available_actions` with the palette filter) when the key that opens it is handled, so the list taken before that key is the palette's list.
    palette_candidates: Vec<SharedString>,
}

/// The driver uses Unix sockets, so other platforms never start it.
#[cfg(not(unix))]
pub fn init(_cx: &mut App) {}

/// One parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Key presses, in order.
    Keys(Vec<Keystroke>),
    /// Key presses for typed text, one per character.
    Type(Vec<Keystroke>),
    /// Read what the window shows.
    State,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum WireRequest {
    Keys { keys: String },
    Type { text: String },
    State,
}

/// Parses one request line.
pub fn parse_request(line: &str) -> Result<Request> {
    let request: WireRequest =
        serde_json::from_str(line).map_err(|error| anyhow!("invalid request: {error}"))?;
    Ok(match request {
        WireRequest::Keys { keys } => Request::Keys(parse_keys(&keys)?),
        WireRequest::Type { text } => Request::Type(typed_keystrokes(&text)?),
        WireRequest::State => Request::State,
    })
}

/// Parses space-separated keystrokes in Zed's syntax, for example "ctrl-p" or "escape ctrl-shift-p".
pub fn parse_keys(keys: &str) -> Result<Vec<Keystroke>> {
    let keystrokes = keys
        .split_whitespace()
        .map(|source| {
            Keystroke::parse(source)
                .map(platform_keystroke)
                .map_err(|_| anyhow!("invalid keystroke {source:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if keystrokes.is_empty() {
        bail!("keys must name at least one keystroke");
    }
    Ok(keystrokes)
}

/// Fills in the typed text of a parsed keystroke the way the Linux platform layer does: a printable key types its character (upper case with shift), space types a space, and keys with ctrl, super or fn type nothing. Enter, tab, escape and other named keys type nothing.
pub fn platform_keystroke(mut keystroke: Keystroke) -> Keystroke {
    if keystroke.key_char.is_some() {
        return keystroke;
    }
    let modifiers = keystroke.modifiers;
    if modifiers.control || modifiers.platform || modifiers.function {
        return keystroke;
    }
    let mut chars = keystroke.key.chars();
    keystroke.key_char = match (chars.next(), chars.next()) {
        _ if keystroke.key == "space" => Some(" ".to_owned()),
        (Some(character), None) if !character.is_control() => Some(if modifiers.shift {
            character.to_uppercase().collect()
        } else {
            character.to_string()
        }),
        _ => None,
    };
    keystroke
}

/// The key presses that type the text: one per character, carrying the character as the typed text.
pub fn typed_keystrokes(text: &str) -> Result<Vec<Keystroke>> {
    if text.is_empty() {
        bail!("type needs a non-empty text");
    }
    text.chars().map(typed_keystroke).collect()
}

fn typed_keystroke(character: char) -> Result<Keystroke> {
    let (key, key_char, shift) = match character {
        ' ' => ("space".to_owned(), Some(" ".to_owned()), false),
        '\n' => ("enter".to_owned(), None, false),
        '\t' => ("tab".to_owned(), None, false),
        character if character.is_control() => {
            bail!("cannot type the control character {character:?}: use keys instead")
        }
        character if character.is_uppercase() => (
            character.to_lowercase().collect(),
            Some(character.to_string()),
            true,
        ),
        character => (character.to_string(), Some(character.to_string()), false),
    };
    Ok(Keystroke {
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        key,
        key_char,
    })
}

/// How the root of the center pane layout is split, named by the divider the user sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Split {
    /// A vertical divider: panes side by side, left to right (Zed's `Axis::Horizontal`, made by "pane: split right").
    Vertical,
    /// A horizontal divider: panes stacked, top to bottom (Zed's `Axis::Vertical`, made by "pane: split down").
    Horizontal,
}

impl Split {
    pub fn name(self) -> &'static str {
        match self {
            Split::Vertical => "vertical",
            Split::Horizontal => "horizontal",
        }
    }
}

/// The screen rectangle of one pane, in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// Panes closer than this many pixels count as touching: the divider between two panes may overlap them a little.
const EDGE_TOLERANCE: f32 = 2.0;

/// Orders panes the way the layout tree holds them and names the split at its root, from the rectangles the panes cover on screen.
/// The root split is the line, crossing no pane, that cuts all panes into groups; each group is ordered the same way, recursively.
/// Returns the indices into `rects` in layout order. One pane, or none, has no split.
pub fn pane_layout(rects: &[Rect]) -> (Option<Split>, Vec<usize>) {
    order_group(rects, (0..rects.len()).collect())
}

fn order_group(rects: &[Rect], group: Vec<usize>) -> (Option<Split>, Vec<usize>) {
    if group.len() < 2 {
        return (None, group);
    }
    for split in [Split::Vertical, Split::Horizontal] {
        if let Some(parts) = cut(rects, &group, split) {
            let order = parts
                .into_iter()
                .flat_map(|part| order_group(rects, part).1)
                .collect();
            return (Some(split), order);
        }
    }
    // No straight cut exists (Zed's layouts always have one): fall back to reading order.
    let mut order = group;
    order.sort_by(|&a, &b| {
        rects[a]
            .top
            .total_cmp(&rects[b].top)
            .then(rects[a].left.total_cmp(&rects[b].left))
    });
    (None, order)
}

/// Cuts the group at every line across `split` that no pane crosses. None when there is no such line.
fn cut(rects: &[Rect], group: &[usize], split: Split) -> Option<Vec<Vec<usize>>> {
    let span = |index: usize| {
        let rect = rects[index];
        match split {
            Split::Vertical => (rect.left, rect.right),
            Split::Horizontal => (rect.top, rect.bottom),
        }
    };
    let mut sorted = group.to_vec();
    sorted.sort_by(|&a, &b| {
        let (a_start, a_end) = span(a);
        let (b_start, b_end) = span(b);
        a_start.total_cmp(&b_start).then(a_end.total_cmp(&b_end))
    });
    let mut parts = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut reach = f32::NEG_INFINITY;
    for index in sorted {
        let (start, end) = span(index);
        if !current.is_empty() && start >= reach - EDGE_TOLERANCE {
            parts.push(std::mem::take(&mut current));
        }
        current.push(index);
        reach = reach.max(end);
    }
    parts.push(current);
    (parts.len() > 1).then_some(parts)
}

/// The command the palette highlights, worked out the way the palette ranks its matches (`CommandPaletteDelegate::update_matches`): the candidates sorted by name, the query normalized, nucleo with smart case and the length penalty.
/// That ranking holds only without command history, so the tests turn `command_palette.use_command_history` off; with history the caller passes no candidates.
/// `candidates` must be the palette's own list. The answer is None unless the ranking gives exactly as many matches as the palette shows (`match_count`), which also rules out a palette that is still updating its matches.
pub fn palette_selection(
    candidates: &[SharedString],
    query: &str,
    match_count: usize,
    selected_index: usize,
) -> Option<String> {
    let mut names = candidates.to_vec();
    names.sort();
    let candidates: Vec<StringMatchCandidate> = names
        .iter()
        .enumerate()
        .map(|(index, name)| StringMatchCandidate::new(index, name.clone()))
        .collect();
    let query = command_palette::normalize_action_query(query);
    let matches = fuzzy_nucleo::match_strings(
        &candidates,
        &query,
        Case::Smart,
        LengthPenalty::On,
        10000,
    );
    if matches.is_empty() || matches.len() != match_count {
        return None;
    }
    matches
        .get(selected_index)
        .map(|found| found.string.to_string())
}

/// The reply line, without the final newline.
pub fn encode_reply(reply: &Result<Option<Value>>) -> String {
    let value = match reply {
        Ok(None) => json!({ "ok": true }),
        Ok(Some(state)) => json!({ "ok": true, "state": state }),
        Err(error) => json!({ "ok": false, "error": format!("{error:#}") }),
    };
    value.to_string()
}

/// A request from a client thread, with the way back to it.
#[cfg(unix)]
struct Job {
    request: Request,
    reply: std::sync::mpsc::Sender<Result<Option<Value>>>,
}

/// Waits for the first workspace window, opens the socket and runs every request on the foreground.
#[cfg(unix)]
async fn serve(path: std::path::PathBuf, memory: Rc<RefCell<Memory>>, cx: &mut AsyncApp) {
    use futures::StreamExt as _;

    while cx.update(|cx| workspace_window(cx)).is_none() {
        cx.background_executor().timer(WINDOW_POLL).await;
    }
    let (jobs, mut incoming) = futures::channel::mpsc::unbounded::<Job>();
    // Every file system and socket call of the setup runs on the background executor; the foreground only awaits the result.
    let listening = cx.background_executor().spawn({
        let path = path.clone();
        async move { socket::listen(&path, jobs) }
    });
    match listening.await {
        Ok(()) => log::info!("e2e driver: listening on {}", path.display()),
        Err(error) => {
            log::error!("e2e driver: not started: {error:#}");
            return;
        }
    }
    while let Some(job) = incoming.next().await {
        let reply = run(job.request, &memory, cx).await;
        job.reply.send(reply).ok();
    }
}

#[cfg(unix)]
async fn run(
    request: Request,
    memory: &Rc<RefCell<Memory>>,
    cx: &mut AsyncApp,
) -> Result<Option<Value>> {
    match request {
        Request::Keys(keystrokes) | Request::Type(keystrokes) => {
            for (index, keystroke) in keystrokes.into_iter().enumerate() {
                if index > 0 {
                    cx.background_executor().timer(KEY_PAUSE).await;
                }
                press(keystroke, memory, cx)?;
            }
            Ok(None)
        }
        Request::State => {
            // Counted outside update_window: while a window is being updated, App::windows leaves it out.
            let window_count = cx.update(|cx| cx.windows().len());
            let window = cx
                .update(|cx| workspace_window(cx))
                .context("Cod has no workspace window")?;
            cx.update_window(window.into(), |root, window, cx| {
                let multi_workspace = root
                    .downcast::<MultiWorkspace>()
                    .map_err(|_| anyhow!("the window does not show a workspace"))?;
                read_state(&multi_workspace, window, window_count, &memory.borrow(), cx)
            })
            .context("the workspace window closed")?
            .map(Some)
        }
    }
}

/// The window the driver works on: the active window when it shows a workspace, otherwise the first workspace window.
fn workspace_window(cx: &App) -> Option<WindowHandle<MultiWorkspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
        .or_else(|| {
            cx.windows()
                .into_iter()
                .find_map(|window| window.downcast::<MultiWorkspace>())
        })
}

/// Presses one key: key down, then key up, as the platform layer delivers them.
#[cfg(unix)]
fn press(keystroke: Keystroke, memory: &Rc<RefCell<Memory>>, cx: &mut AsyncApp) -> Result<()> {
    use gpui::{KeyDownEvent, KeyUpEvent, PlatformInput};

    let window = cx
        .update(|cx| workspace_window(cx))
        .context("Cod has no workspace window")?;
    cx.update_window(window.into(), |root, window, cx| {
        if let Ok(multi_workspace) = root.downcast::<MultiWorkspace>() {
            let palette_open = multi_workspace
                .read(cx)
                .workspace()
                .read(cx)
                .active_modal::<command_palette::CommandPalette>(cx)
                .is_some();
            if !palette_open {
                memory.borrow_mut().palette_candidates = palette_candidates(window, cx);
            }
        }
        // The Linux platform layer gives unhandled text keys without modifiers other than shift to the text input.
        let types_text = keystroke.key_char.is_some()
            && keystroke.modifiers.is_subset_of(&Modifiers::shift());
        if types_text {
            window.dispatch_keystroke(keystroke.clone(), cx);
        } else {
            window.dispatch_event(
                PlatformInput::KeyDown(KeyDownEvent {
                    keystroke: keystroke.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
        }
        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
    })
    .context("the workspace window closed")
}

/// The command names the palette lists when it opens now: `CommandPalette::new` takes `Window::available_actions`, drops the actions the palette filter hides and humanizes the names.
fn palette_candidates(window: &Window, cx: &App) -> Vec<SharedString> {
    let filter = command_palette_hooks::CommandPaletteFilter::try_global(cx);
    window
        .available_actions(cx)
        .into_iter()
        .filter_map(|action| {
            if filter.is_some_and(|filter| filter.is_hidden(&*action)) {
                return None;
            }
            Some(SharedString::from(command_palette::humanize_action_name(
                action.name(),
            )))
        })
        .collect()
}

/// The name of a workspace as the user gave it, or None while it has none.
/// Cod has no workspace names yet (story #24 adds them); this is the one place that reads them once they exist.
fn workspace_name(_workspace: &Entity<Workspace>, _cx: &App) -> Option<String> {
    None
}

fn worktrees_of(workspace: &Workspace, cx: &App) -> Vec<String> {
    workspace
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect()
}

fn tab_title(item: &dyn ItemHandle, cx: &App) -> String {
    item.tab_content_text(0, cx).to_string()
}

/// What the window shows. The fields are described in `tests/e2e/README.md`; the first four are the ones of story #22.
fn read_state(
    multi_workspace: &Entity<MultiWorkspace>,
    window: &Window,
    window_count: usize,
    memory: &Memory,
    cx: &App,
) -> Result<Value> {
    let multi_workspace = multi_workspace.read(cx);
    let active = multi_workspace.workspace().clone();
    let held: Vec<Entity<Workspace>> = multi_workspace.workspaces().cloned().collect();
    let active_workspace = held.iter().position(|workspace| *workspace == active);
    let workspaces: Vec<Value> = held
        .iter()
        .map(|workspace| {
            json!({
                "name": workspace_name(workspace, cx),
                "worktrees": worktrees_of(workspace.read(cx), cx),
            })
        })
        .collect();
    let workspace = active.read(cx);
    let active_tab = workspace
        .active_item(cx)
        .map(|item| tab_title(&*item, cx));
    let tabs: Vec<String> = workspace
        .active_pane()
        .read(cx)
        .items()
        .map(|item| tab_title(&**item, cx))
        .collect();
    let worktrees = worktrees_of(workspace, cx);

    // The center panes in layout order, read from where they are on screen: the pane tree itself is private to the workspace.
    let panes = workspace.panes();
    let rects: Option<Vec<Rect>> = panes
        .iter()
        .map(|pane| {
            workspace.bounding_box_for_pane(pane).map(|bounds| Rect {
                left: f32::from(bounds.origin.x),
                top: f32::from(bounds.origin.y),
                right: f32::from(bounds.origin.x + bounds.size.width),
                bottom: f32::from(bounds.origin.y + bounds.size.height),
            })
        })
        .collect();
    let (split, order): (Value, Vec<usize>) = match rects {
        _ if panes.len() < 2 => (Value::Null, (0..panes.len()).collect()),
        Some(rects) => {
            let (split, order) = pane_layout(&rects);
            (json!(split.map(Split::name)), order)
        }
        // Several panes that are not laid out yet: the split cannot be told.
        None => (json!("unknown"), (0..panes.len()).collect()),
    };
    let ordered_panes: Vec<&Entity<Pane>> =
        order.iter().map(|&index| &panes[index]).collect();
    let panes_json: Vec<Value> = ordered_panes
        .iter()
        .map(|pane| {
            let pane = pane.read(cx);
            json!({
                "tabs": pane.items().map(|item| tab_title(&**item, cx)).collect::<Vec<_>>(),
                "active_tab": pane.active_item().map(|item| tab_title(&*item, cx)),
            })
        })
        .collect();
    let dirty_tabs: Vec<String> = ordered_panes
        .iter()
        .flat_map(|pane| pane.read(cx).items())
        .filter(|item| item.is_dirty(cx))
        .map(|item| tab_title(&**item, cx))
        .collect();

    // The cursor of the active editor as the status bar shows it: 1-based line, 1-based column (in bytes).
    let cursor = workspace.active_item_as::<Editor>(cx).map(|editor| {
        let editor = editor.read(cx);
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let point = editor.selections.newest_anchor().head().to_point(&snapshot);
        json!({ "row": point.row + 1, "column": point.column + 1 })
    });

    // The command palette: its query, how many matches it shows, which one is highlighted, and the name of that command.
    let palette_open = workspace
        .active_modal::<command_palette::CommandPalette>(cx)
        .is_some();
    let palette_picker = memory
        .palette
        .as_ref()
        .and_then(|picker| picker.upgrade())
        .filter(|_| palette_open);
    let (palette, selection) = match palette_picker {
        Some(picker) => {
            let picker = picker.read(cx);
            let query = picker.query(cx);
            let match_count = picker.delegate.match_count();
            let selected_index = picker.delegate.selected_index();
            let history = cx
                .global::<SettingsStore>()
                .merged_settings()
                .command_palette
                .as_ref()
                .and_then(|settings| settings.use_command_history)
                .unwrap_or(true);
            let aliased = WorkspaceSettings::get_global(cx)
                .command_aliases
                .contains_key(&query);
            let selection = if history || aliased {
                None
            } else {
                palette_selection(
                    &memory.palette_candidates,
                    &query,
                    match_count,
                    selected_index,
                )
            };
            (
                json!({
                    "query": query,
                    "match_count": match_count,
                    "selected_index": selected_index,
                }),
                json!(selection),
            )
        }
        None => (Value::Null, Value::Null),
    };

    // Zed's own path picker (with use_system_path_prompts off): its query and how many entries match it.
    let path_prompt = workspace
        .active_modal::<Picker<OpenPathDelegate>>(cx)
        .map(|picker| {
            let picker = picker.read(cx);
            json!({
                "query": picker.query(cx),
                "match_count": picker.delegate.match_count(),
            })
        });

    // A prompt drawn in the window (Zed's ui_prompt, always used on Linux). It does not expose its text, so message and answers stay null.
    let prompt = window
        .has_active_prompt()
        .then(|| json!({ "message": null, "answers": null }));
    let modal = if workspace
        .active_modal::<file_finder::FileFinder>(cx)
        .is_some()
    {
        Some("file_finder")
    } else if workspace
        .active_modal::<command_palette::CommandPalette>(cx)
        .is_some()
    {
        Some("command_palette")
    } else if workspace.modal_layer().read(cx).has_active_modal() {
        Some("other")
    } else {
        None
    };
    Ok(json!({
        "active_tab": active_tab,
        "tabs": tabs,
        "worktrees": worktrees,
        "modal": modal,
        "window_count": window_count,
        "workspace_count": held.len(),
        "active_workspace": active_workspace,
        "workspaces": workspaces,
        "panes": panes_json,
        "split": split,
        "cursor": cursor,
        "dirty_tabs": dirty_tabs,
        "palette": palette,
        "palette_selection": selection,
        "path_prompt": path_prompt,
        "prompt": prompt,
    }))
}

/// The socket side: runs on background threads only.
#[cfg(unix)]
mod socket {
    use std::{
        fs,
        io::{BufRead as _, BufReader, Write as _},
        os::unix::{
            fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
            net::{UnixListener, UnixStream},
        },
        path::Path,
        sync::mpsc,
    };

    use anyhow::{Context as _, Result, bail};
    use futures::channel::mpsc::UnboundedSender;

    use super::{Job, encode_reply, parse_request};

    /// What the driver needs to know about the folder that holds the socket.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct FolderFacts {
        /// The path names a symbolic link (not followed).
        pub is_symlink: bool,
        /// The path names a real folder.
        pub is_dir: bool,
        /// The owner's user id.
        pub owner: u32,
        /// The mode bits, as in `st_mode`.
        pub mode: u32,
    }

    /// Why the folder may not hold the socket, or None when it is a real folder of this user that nobody else can enter.
    /// Only such a folder keeps other users away from the socket between bind and chmod.
    pub(super) fn folder_problem(facts: FolderFacts, user: u32) -> Option<String> {
        if facts.is_symlink {
            Some("is a symbolic link".to_owned())
        } else if !facts.is_dir {
            Some("is not a folder".to_owned())
        } else if facts.owner != user {
            Some(format!(
                "belongs to user {} instead of this user ({user})",
                facts.owner
            ))
        } else if facts.mode & 0o077 != 0 {
            Some(format!(
                "has mode {:o}: group and others must have no access (for example mode 700)",
                facts.mode & 0o777
            ))
        } else {
            None
        }
    }

    fn current_user() -> u32 {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Refuses a socket path whose folder is missing, a link, someone else's or open to others.
    fn check_folder(path: &Path) -> Result<()> {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        let metadata = fs::symlink_metadata(folder).with_context(|| {
            format!("the socket folder {} cannot be inspected", folder.display())
        })?;
        let facts = FolderFacts {
            is_symlink: metadata.file_type().is_symlink(),
            is_dir: metadata.file_type().is_dir(),
            owner: metadata.uid(),
            mode: metadata.mode(),
        };
        if let Some(problem) = folder_problem(facts, current_user()) {
            bail!("the socket folder {} {problem}", folder.display());
        }
        Ok(())
    }

    /// Removes a leftover socket of an earlier Cod at the path. Refuses a live socket, a socket of another user and any other kind of file.
    fn remove_stale_socket(path: &Path) -> Result<()> {
        let first = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", path.display()));
            }
        };
        let user = current_user();
        if !first.file_type().is_socket() || first.uid() != user {
            bail!("{} exists and is not a socket of this user", path.display());
        }
        match UnixStream::connect(path) {
            Ok(_) => bail!(
                "{} is the socket of a running Cod: it is left alone",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot tell whether {} is still in use", path.display())
                });
            }
        }
        // Nobody listens. Remove it only when it is still the very socket that was checked.
        let again = fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect {} again", path.display()))?;
        if !again.file_type().is_socket()
            || again.uid() != user
            || again.dev() != first.dev()
            || again.ino() != first.ino()
        {
            bail!("{} changed while it was checked", path.display());
        }
        fs::remove_file(path)
            .with_context(|| format!("cannot remove the old socket {}", path.display()))
    }

    /// Binds the socket with owner-only permissions and serves it on a background thread. Call it off the GPUI thread: it touches the file system.
    /// The folder must be a private folder of this user. A stale socket of this user is replaced; a live socket or any other file at the path is left alone and the driver does not start.
    pub(super) fn listen(path: &Path, jobs: UnboundedSender<Job>) -> Result<()> {
        check_folder(path)?;
        remove_stale_socket(path)?;
        let listener = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        // The private folder already keeps others out; the socket mode is a second fence.
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            fs::remove_file(path).ok();
            return Err(error)
                .with_context(|| format!("cannot make {} private", path.display()));
        }
        std::thread::Builder::new()
            .name("cod-e2e-driver".into())
            .spawn(move || accept(listener, jobs))
            .context("cannot start the driver thread")?;
        Ok(())
    }

    fn accept(listener: UnixListener, jobs: UnboundedSender<Job>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let jobs = jobs.clone();
                    let started = std::thread::Builder::new()
                        .name("cod-e2e-client".into())
                        .spawn(move || {
                            if let Err(error) = serve_client(stream, jobs) {
                                log::warn!("e2e driver: client connection ended: {error:#}");
                            }
                        });
                    if let Err(error) = started {
                        log::error!("e2e driver: cannot start a client thread: {error}");
                    }
                }
                Err(error) => log::warn!("e2e driver: accept failed: {error}"),
            }
            if jobs.is_closed() {
                return;
            }
        }
    }

    fn serve_client(stream: UnixStream, jobs: UnboundedSender<Job>) -> Result<()> {
        let mut writer = stream.try_clone().context("cannot clone the client stream")?;
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let line = line.context("cannot read a request")?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match parse_request(&line) {
                Ok(request) => {
                    let (reply, answer) = mpsc::channel();
                    if jobs.unbounded_send(Job { request, reply }).is_err() {
                        bail!("the driver stopped");
                    }
                    answer.recv().context("the driver stopped")?
                }
                Err(error) => Err(error),
            };
            let mut line = encode_reply(&reply);
            line.push('\n');
            writer
                .write_all(line.as_bytes())
                .and_then(|()| writer.flush())
                .context("cannot write a reply")?;
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{FolderFacts, folder_problem};

        const USER: u32 = 1000;
        const DIR: u32 = 0o040000;

        fn folder(mode: u32) -> FolderFacts {
            FolderFacts {
                is_symlink: false,
                is_dir: true,
                owner: USER,
                mode: DIR | mode,
            }
        }

        #[test]
        fn accepts_a_private_folder_of_this_user() {
            assert_eq!(folder_problem(folder(0o700), USER), None);
            assert_eq!(folder_problem(folder(0o500), USER), None);
        }

        #[test]
        fn refuses_folders_others_can_reach() {
            for mode in [0o750, 0o705, 0o710, 0o701, 0o770, 0o755, 0o777] {
                let problem = folder_problem(folder(mode), USER);
                assert!(
                    problem.as_deref().is_some_and(|problem| problem.contains("mode")),
                    "mode {mode:o} was accepted: {problem:?}"
                );
            }
        }

        #[test]
        fn refuses_links_files_and_other_owners() {
            let link = FolderFacts {
                is_symlink: true,
                is_dir: false,
                ..folder(0o700)
            };
            assert_eq!(
                folder_problem(link, USER).as_deref(),
                Some("is a symbolic link")
            );
            let file = FolderFacts {
                is_dir: false,
                ..folder(0o600)
            };
            assert_eq!(folder_problem(file, USER).as_deref(), Some("is not a folder"));
            let foreign = FolderFacts {
                owner: 0,
                ..folder(0o700)
            };
            assert!(
                folder_problem(foreign, USER)
                    .as_deref()
                    .is_some_and(|problem| problem.contains("belongs to user 0"))
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    #[test]
    fn parses_the_three_commands() {
        assert_eq!(
            parse_request(r#"{"cmd":"keys","keys":"ctrl-p enter"}"#).unwrap(),
            Request::Keys(vec![
                keystroke("p", None, Modifiers::control()),
                keystroke("enter", None, Modifiers::none()),
            ])
        );
        assert_eq!(
            parse_request(r#"{"cmd":"type","text":"aB c"}"#).unwrap(),
            Request::Type(vec![
                keystroke("a", Some("a"), Modifiers::none()),
                keystroke("b", Some("B"), Modifiers::shift()),
                keystroke("space", Some(" "), Modifiers::none()),
                keystroke("c", Some("c"), Modifiers::none()),
            ])
        );
        assert_eq!(parse_request(r#"{"cmd":"state"}"#).unwrap(), Request::State);
    }

    #[test]
    fn refuses_bad_requests() {
        for line in [
            "not json",
            r#"{"keys":"a"}"#,
            r#"{"cmd":"click"}"#,
            r#"{"cmd":"keys"}"#,
            r#"{"cmd":"keys","keys":"   "}"#,
            r#"{"cmd":"keys","keys":"ctrl-p-q"}"#,
            r#"{"cmd":"type","text":""}"#,
            r#"{"cmd":"type","text":"a\u0007"}"#,
        ] {
            assert!(parse_request(line).is_err(), "{line} was accepted");
        }
    }

    #[test]
    fn keys_type_text_like_the_linux_platform() {
        assert_eq!(
            parse_keys("a shift-a space ctrl-a enter escape alt-x").unwrap(),
            vec![
                keystroke("a", Some("a"), Modifiers::none()),
                keystroke("a", Some("A"), Modifiers::shift()),
                keystroke("space", Some(" "), Modifiers::none()),
                keystroke("a", None, Modifiers::control()),
                keystroke("enter", None, Modifiers::none()),
                keystroke("escape", None, Modifiers::none()),
                keystroke("x", Some("x"), Modifiers::alt()),
            ]
        );
    }

    #[test]
    fn typing_names_enter_and_tab() {
        assert_eq!(
            typed_keystrokes("_\n\t").unwrap(),
            vec![
                keystroke("_", Some("_"), Modifiers::none()),
                keystroke("enter", None, Modifiers::none()),
                keystroke("tab", None, Modifiers::none()),
            ]
        );
    }

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn one_pane_has_no_split() {
        assert_eq!(pane_layout(&[]), (None, vec![]));
        assert_eq!(
            pane_layout(&[rect(0., 0., 800., 600.)]),
            (None, vec![0])
        );
    }

    #[test]
    fn side_by_side_panes_are_a_vertical_split_in_left_to_right_order() {
        let right = rect(400., 0., 800., 600.);
        let left = rect(0., 0., 401., 600.);
        assert_eq!(
            pane_layout(&[right, left]),
            (Some(Split::Vertical), vec![1, 0])
        );
    }

    #[test]
    fn stacked_panes_are_a_horizontal_split_in_top_to_bottom_order() {
        let bottom = rect(0., 300., 800., 600.);
        let top = rect(0., 0., 800., 300.);
        assert_eq!(
            pane_layout(&[bottom, top]),
            (Some(Split::Horizontal), vec![1, 0])
        );
    }

    #[test]
    fn nested_layouts_name_the_root_split_and_keep_tree_order() {
        // Left column split into top and bottom, one full-height pane on the right.
        let right = rect(400., 0., 800., 600.);
        let left_bottom = rect(0., 300., 400., 600.);
        let left_top = rect(0., 0., 400., 300.);
        assert_eq!(
            pane_layout(&[right, left_bottom, left_top]),
            (Some(Split::Vertical), vec![2, 1, 0])
        );
        // A top row split into left and right, one full-width pane below.
        let bottom = rect(0., 300., 800., 600.);
        let top_right = rect(400., 0., 800., 300.);
        let top_left = rect(0., 0., 400., 300.);
        assert_eq!(
            pane_layout(&[bottom, top_right, top_left]),
            (Some(Split::Horizontal), vec![2, 1, 0])
        );
    }

    fn names(names: &[&str]) -> Vec<SharedString> {
        names.iter().map(|name| SharedString::from(*name)).collect()
    }

    #[test]
    fn the_exact_command_name_is_the_selection() {
        let candidates = names(&[
            "workspace: close window",
            "multi workspace: close workspace sidebar",
            "workspace: close workspace",
        ]);
        assert_eq!(
            palette_selection(&candidates, "workspace: close workspace", 2, 0).as_deref(),
            Some("workspace: close workspace")
        );
        assert_eq!(
            palette_selection(&candidates, "workspace: close workspace", 2, 1).as_deref(),
            Some("multi workspace: close workspace sidebar")
        );
    }

    #[test]
    fn no_selection_when_the_palette_shows_other_matches() {
        let candidates = names(&["workspace: close window", "workspace: new window"]);
        // The palette shows a different number of matches: it is still updating or lists other commands.
        assert_eq!(
            palette_selection(&candidates, "workspace: new window", 2, 0),
            None
        );
        // Nothing matches: no selection, whatever the palette shows.
        assert_eq!(
            palette_selection(&candidates, "workspace: new workspace", 0, 0),
            None
        );
    }

    #[test]
    fn encodes_replies() {
        assert_eq!(encode_reply(&Ok(None)), r#"{"ok":true}"#);
        assert_eq!(
            encode_reply(&Ok(Some(json!({ "modal": null })))),
            r#"{"ok":true,"state":{"modal":null}}"#
        );
        assert_eq!(
            encode_reply(&Err(anyhow!("Cod has no workspace window"))),
            r#"{"ok":false,"error":"Cod has no workspace window"}"#
        );
    }
}
