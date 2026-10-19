//! The codctl command server (story #37).
//!
//! Every running Cod listens on one private Unix socket, at the path `codctl::default_socket_path` names (`$COD_SOCKET`, else `$XDG_RUNTIME_DIR/cod/codctl.sock`). The socket's folder is made owner-only when it is missing, and the server refuses a folder that is a link, belongs to someone else or is open to group or others. A live socket of another Cod at the path is left alone (the first Cod keeps it); a stale one is replaced.
//! The protocol is the one of the `codctl` library: one JSON request per line, one JSON reply per line, versioned.
//!
//! The socket is served on background threads. Every request is handed to the foreground through one channel, and each runs as its own foreground task, so a command that waits (a save prompt of `workspace close`) never holds up the others and the GPUI thread never waits for a client.
//! Workspace commands use the operations of story #24 (`MultiWorkspace::activate`, `cod_show_pending`, `Workspace::new_local`, `Workspace::open_paths`), so the window shows each change as the matching UI action does.

#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use codctl::Request;
use gpui::{App, AsyncApp, Entity, WindowHandle};
use serde_json::{Value, json};
use workspace::{
    CodMember, MultiWorkspace, OpenMode, OpenOptions, OpenVisible, Workspace, WorkspaceId,
};

/// The rolled-up status of a workspace until workspace statuses exist (#25, #26).
const IDLE: &str = "idle";

/// Starts the server.
#[cfg(unix)]
pub fn init(cx: &mut App) {
    let path = codctl::default_socket_path();
    cx.spawn(async move |cx| serve(path, cx).await).detach();
}

/// The server uses Unix sockets, so other platforms never start it.
#[cfg(not(unix))]
pub fn init(_cx: &mut App) {}

/// A request from a client thread, with the way back to it.
#[cfg(unix)]
struct Job {
    request: Request,
    reply: std::sync::mpsc::Sender<Result<Value, String>>,
}

/// Opens the socket and runs every request on the foreground, each as its own task.
#[cfg(unix)]
async fn serve(path: PathBuf, cx: &mut AsyncApp) {
    use futures::StreamExt as _;

    let (jobs, mut incoming) = futures::channel::mpsc::unbounded::<Job>();
    // Every file system and socket call of the setup runs on the background executor.
    let listening = cx.background_executor().spawn({
        let path = path.clone();
        async move { socket::listen(&path, jobs) }
    });
    match listening.await {
        Ok(()) => log::info!("codctl: listening on {}", path.display()),
        Err(error) => {
            log::error!("codctl: not started: {error:#}");
            return;
        }
    }
    while let Some(job) = incoming.next().await {
        cx.spawn(async move |cx| {
            let reply = run(job.request, cx)
                .await
                .map_err(|error| format!("{error:#}"));
            job.reply.send(reply).ok();
        })
        .detach();
    }
}

/// Runs one request on the foreground.
async fn run(request: Request, cx: &mut AsyncApp) -> Result<Value> {
    match request {
        Request::WorkspaceList => Ok(cx.update(|cx| Value::Array(list(cx)))),
        Request::WorkspaceNew {
            name,
            folders,
            focus,
        } => new_workspace(name, folders, focus, cx).await,
        Request::WorkspaceFocus { workspace } => focus_workspace(&workspace, cx).await,
        Request::WorkspaceAddFolder { workspace, folders } => {
            add_folders(&workspace, folders, cx).await
        }
        Request::WorkspaceRename { workspace, .. } => {
            // Checked first, so an unknown workspace reads the same as for the other commands.
            cx.update(|cx| resolve(&workspace, cx))?;
            bail!("workspace rename is not available in this build yet (#37)")
        }
        Request::WorkspaceClose { workspace, .. } => {
            cx.update(|cx| resolve(&workspace, cx))?;
            bail!("workspace close is not available in this build yet (#37)")
        }
    }
}

/// Every open workspace window.
fn windows(cx: &App) -> Vec<WindowHandle<MultiWorkspace>> {
    cx.windows()
        .into_iter()
        .filter_map(|window| window.downcast::<MultiWorkspace>())
        .collect()
}

/// The window a new workspace goes to: the active workspace window, else the first one.
fn target_window(cx: &App) -> Option<WindowHandle<MultiWorkspace>> {
    cx.active_window()
        .and_then(|window| window.downcast::<MultiWorkspace>())
        .or_else(|| windows(cx).into_iter().next())
}

/// One workspace of a window: loaded, or pending (saved, loads when shown).
struct Member {
    window: WindowHandle<MultiWorkspace>,
    id: Option<WorkspaceId>,
    live: Option<Entity<Workspace>>,
    name: Option<String>,
    folders: Vec<String>,
    active: bool,
}

/// The members of every window in window order, as the window lists them: loaded workspaces, and pending ones that have saved data.
fn members(cx: &App) -> Vec<Member> {
    let mut found = Vec::new();
    for window in windows(cx) {
        let Ok(multi_workspace) = window.read(cx) else {
            continue;
        };
        let active = multi_workspace.workspace().clone();
        for member in multi_workspace.cod_members(cx) {
            match member {
                CodMember::Live(workspace) => {
                    let read = workspace.read(cx);
                    found.push(Member {
                        window,
                        id: read.database_id(),
                        name: workspace::cod_workspace_name(read, cx),
                        folders: read
                            .visible_worktrees(cx)
                            .map(|worktree| {
                                worktree.read(cx).abs_path().to_string_lossy().into_owned()
                            })
                            .collect(),
                        active: workspace == active,
                        live: Some(workspace),
                    });
                }
                CodMember::Pending(id) => {
                    let Some(paths) = multi_workspace.cod_pending_paths(id) else {
                        continue;
                    };
                    found.push(Member {
                        window,
                        id: Some(id),
                        live: None,
                        name: workspace::cod_workspace_name_by_id(id, cx),
                        folders: paths
                            .iter()
                            .map(|path| path.to_string_lossy().into_owned())
                            .collect(),
                        active: false,
                    });
                }
            }
        }
    }
    found
}

/// What `workspace list` reports for each workspace.
fn list(cx: &App) -> Vec<Value> {
    members(cx)
        .into_iter()
        .map(|member| {
            json!({
                "id": member.id.map(i64::from),
                "name": member.name,
                "status": IDLE,
                "folders": member.folders,
                "active": member.active,
                "loaded": member.live.is_some(),
            })
        })
        .collect()
}

/// Finds the workspace a command names: by its ID first, else by its exact name. Several workspaces with that name are an error.
fn resolve(reference: &str, cx: &App) -> Result<Member> {
    let wanted_id = reference.parse::<i64>().ok();
    let mut named = Vec::new();
    for member in members(cx) {
        if member.id.is_some_and(|id| Some(i64::from(id)) == wanted_id) {
            return Ok(member);
        }
        if member.id.is_some() && member.name.as_deref() == Some(reference) {
            named.push(member);
        }
    }
    match named.len() {
        0 => Err(anyhow!("unknown workspace {reference:?}")),
        1 => Ok(named.remove(0)),
        count => Err(anyhow!(
            "{count} workspaces are named {reference:?}: use the workspace ID"
        )),
    }
}

fn id_of(member: &Member) -> Result<WorkspaceId> {
    member.id.context("the workspace has no ID yet")
}

async fn new_workspace(
    name: Option<String>,
    folders: Vec<PathBuf>,
    focus: bool,
    cx: &mut AsyncApp,
) -> Result<Value> {
    if name.is_some() {
        bail!("workspace new --name is not available in this build yet (#37)");
    }
    let window = cx
        .update(|cx| target_window(cx))
        .context("Cod has no workspace window")?;
    let app_state = cx.update(|cx| {
        window
            .read(cx)
            .map(|multi_workspace| multi_workspace.workspace().read(cx).app_state().clone())
    })?;
    let mode = if focus {
        OpenMode::Activate
    } else {
        OpenMode::Add
    };
    let opened = cx
        .update(|cx| Workspace::new_local(folders, app_state, Some(window), None, None, mode, cx))
        .await?;
    let id = cx
        .update(|cx| opened.workspace.read(cx).database_id())
        .context("the new workspace has no ID")?;
    Ok(json!({ "id": i64::from(id) }))
}

async fn focus_workspace(reference: &str, cx: &mut AsyncApp) -> Result<Value> {
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = id_of(&member)?;
    match member.live {
        Some(workspace) => member
            .window
            .update(cx, |multi_workspace, window, cx| {
                multi_workspace.activate(workspace, None, window, cx)
            })?,
        None => {
            member
                .window
                .update(cx, |multi_workspace, window, cx| {
                    multi_workspace.cod_show_pending(id, window, cx)
                })?
                .await?
        }
    }
    Ok(json!({ "id": i64::from(id) }))
}

async fn add_folders(reference: &str, folders: Vec<PathBuf>, cx: &mut AsyncApp) -> Result<Value> {
    let member = cx.update(|cx| resolve(reference, cx))?;
    let id = i64::from(id_of(&member)?);
    let Some(workspace) = member.live else {
        bail!("workspace {id} is not loaded yet: show it first with codctl workspace focus {id}");
    };
    let opened = member
        .window
        .update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.open_paths(
                    folders,
                    OpenOptions {
                        visible: Some(OpenVisible::All),
                        ..Default::default()
                    },
                    None,
                    window,
                    cx,
                )
            })
        })?
        .await;
    for result in opened.into_iter().flatten() {
        result?;
    }
    Ok(json!({ "id": id }))
}

/// Checks on the client thread, before anything reaches the window, that every folder of the request is a folder.
pub fn check_folders(request: &Request) -> Result<(), String> {
    let folders = match request {
        Request::WorkspaceNew { folders, .. } | Request::WorkspaceAddFolder { folders, .. } => {
            folders
        }
        _ => return Ok(()),
    };
    for folder in folders {
        if !folder.is_dir() {
            return Err(format!("not a folder: {}", folder.display()));
        }
    }
    Ok(())
}

#[cfg(unix)]
mod socket {
    use std::{
        fs,
        io::{BufRead as _, BufReader, Write as _},
        os::unix::{
            fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
            net::{UnixListener, UnixStream},
        },
        path::Path,
        sync::mpsc,
    };

    use anyhow::{Context as _, Result, bail};
    use futures::channel::mpsc::UnboundedSender;

    use super::{Job, check_folders};

    /// What the server needs to know about the folder that holds the socket.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct FolderFacts {
        pub is_symlink: bool,
        pub is_dir: bool,
        pub owner: u32,
        pub mode: u32,
    }

    /// Why the folder may not hold the socket, or None when it is a real folder of this user that nobody else can enter.
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

    /// Makes the socket folder (owner-only) when it is missing, then refuses it when it is a link, someone else's or open to others.
    fn prepare_folder(path: &Path) -> Result<()> {
        let folder = match path.parent() {
            Some(folder) if !folder.as_os_str().is_empty() => folder,
            _ => Path::new("."),
        };
        if fs::symlink_metadata(folder).is_err() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)
                .with_context(|| format!("cannot make the socket folder {}", folder.display()))?;
        }
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
                "{} is the socket of another running Cod: it is left alone",
                path.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("cannot tell whether {} is still in use", path.display())
                });
            }
        }
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
    pub(super) fn listen(path: &Path, jobs: UnboundedSender<Job>) -> Result<()> {
        prepare_folder(path)?;
        remove_stale_socket(path)?;
        let listener = UnixListener::bind(path)
            .with_context(|| format!("cannot listen on {}", path.display()))?;
        if let Err(error) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            fs::remove_file(path).ok();
            return Err(error).with_context(|| format!("cannot make {} private", path.display()));
        }
        std::thread::Builder::new()
            .name("codctl-server".into())
            .spawn(move || accept(listener, jobs))
            .context("cannot start the codctl server thread")?;
        Ok(())
    }

    fn accept(listener: UnixListener, jobs: UnboundedSender<Job>) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let jobs = jobs.clone();
                    let started = std::thread::Builder::new()
                        .name("codctl-client".into())
                        .spawn(move || {
                            if let Err(error) = serve_client(stream, jobs) {
                                log::warn!("codctl: client connection ended: {error:#}");
                            }
                        });
                    if let Err(error) = started {
                        log::error!("codctl: cannot start a client thread: {error}");
                    }
                }
                Err(error) => log::warn!("codctl: accept failed: {error}"),
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
            let reply = match codctl::parse_request_line(&line) {
                Ok(envelope) => {
                    let result = match check_folders(&envelope.request) {
                        Ok(()) => {
                            let (reply, answer) = mpsc::channel();
                            if jobs
                                .unbounded_send(Job {
                                    request: envelope.request,
                                    reply,
                                })
                                .is_err()
                            {
                                bail!("the codctl server stopped");
                            }
                            answer.recv().context("the codctl server stopped")?
                        }
                        Err(error) => Err(error),
                    };
                    match result {
                        Ok(value) => codctl::reply_ok(&envelope.id, value),
                        Err(error) => codctl::reply_error(&envelope.id, &error),
                    }
                }
                Err((id, error)) => codctl::reply_error(&id, &error),
            };
            writer
                .write_all(format!("{reply}\n").as_bytes())
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
        fn accepts_only_a_private_folder_of_this_user() {
            assert_eq!(folder_problem(folder(0o700), USER), None);
            assert!(folder_problem(folder(0o750), USER).is_some());
            assert!(folder_problem(folder(0o707), USER).is_some());
            assert!(folder_problem(folder(0o700), USER + 1).is_some());
            let link = FolderFacts {
                is_symlink: true,
                ..folder(0o700)
            };
            assert!(folder_problem(link, USER).is_some());
            let file = FolderFacts {
                is_dir: false,
                ..folder(0o700)
            };
            assert!(folder_problem(file, USER).is_some());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_must_exist() {
        let missing = Request::WorkspaceAddFolder {
            workspace: "1".to_owned(),
            folders: vec![PathBuf::from("/no/such/folder/for/codctl")],
        };
        assert!(check_folders(&missing).is_err());
        let present = Request::WorkspaceNew {
            name: None,
            folders: vec![std::env::temp_dir()],
            focus: false,
        };
        assert_eq!(check_folders(&present), Ok(()));
        assert_eq!(check_folders(&Request::WorkspaceList), Ok(()));
    }
}
