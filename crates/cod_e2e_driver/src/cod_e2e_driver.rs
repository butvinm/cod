//! The driver of Cod's end-to-end tests (issue #22).
//!
//! Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket once its first workspace window is open. Without the variable nothing here runs.
//! The socket's folder must be a private folder of the same user (no access for group and others); the driver refuses to start otherwise, and also when a live socket or any other file is at the path.
//! The protocol is one JSON object per line, answered by one JSON line (see `tests/e2e/cod_e2e.py`):
//!
//! - `{"cmd":"keys","keys":"ctrl-p enter"}` presses each space-separated keystroke (Zed keystroke syntax). Reply `{"ok":true}`.
//! - `{"cmd":"type","text":"invoice"}` presses one key per character, with the character as the typed text. Reply `{"ok":true}`.
//! - `{"cmd":"state"}` replies `{"ok":true,"state":{"active_tab":..,"tabs":[..],"worktrees":[..],"modal":..}}`.
//! - A failed request replies `{"ok":false,"error":"..."}`.
//!
//! Keys enter the window the way the Linux platform layer delivers them: `Window::dispatch_event` with `PlatformInput::KeyDown` and `KeyUp`, and when the key down is not handled and carries text, the window's text input receives that text (`Window::dispatch_keystroke` does both).
//! So the keymap, the focus and the text input behave as they do for a user. The driver never dispatches actions and never calls into views; `state` only reads what the window shows.
//! The socket is served on background threads. Every request is handed to one foreground task through a channel, so the GPUI thread never waits for a client.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{App, AsyncApp, Keystroke, Modifiers, WindowHandle};
use serde::Deserialize;
use serde_json::{Value, json};
use workspace::{ItemHandle as _, MultiWorkspace};

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
    cx.spawn(async move |cx| serve(path, cx).await).detach();
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
async fn serve(path: std::path::PathBuf, cx: &mut AsyncApp) {
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
        let reply = run(job.request, cx).await;
        job.reply.send(reply).ok();
    }
}

#[cfg(unix)]
async fn run(request: Request, cx: &mut AsyncApp) -> Result<Option<Value>> {
    match request {
        Request::Keys(keystrokes) | Request::Type(keystrokes) => {
            for (index, keystroke) in keystrokes.into_iter().enumerate() {
                if index > 0 {
                    cx.background_executor().timer(KEY_PAUSE).await;
                }
                press(keystroke, cx)?;
            }
            Ok(None)
        }
        Request::State => cx.update(|cx| read_state(cx)).map(Some),
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
fn press(keystroke: Keystroke, cx: &mut AsyncApp) -> Result<()> {
    use gpui::{AppContext as _, KeyDownEvent, KeyUpEvent, PlatformInput};

    let window = cx
        .update(|cx| workspace_window(cx))
        .context("Cod has no workspace window")?;
    cx.update_window(window.into(), |_, window, cx| {
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

/// What the window shows: the active tab, the tabs of the active pane, the folders of the project and the open modal.
fn read_state(cx: &App) -> Result<Value> {
    let window = workspace_window(cx).context("Cod has no workspace window")?;
    let workspace = window
        .read(cx)
        .context("the workspace window closed")?
        .workspace()
        .read(cx);
    let active_tab = workspace
        .active_item(cx)
        .map(|item| item.tab_content_text(0, cx).to_string());
    let tabs: Vec<String> = workspace
        .active_pane()
        .read(cx)
        .items()
        .map(|item| item.tab_content_text(0, cx).to_string())
        .collect();
    let worktrees: Vec<String> = workspace
        .visible_worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
        .collect();
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
