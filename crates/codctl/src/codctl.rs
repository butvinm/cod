//! `codctl`: the command line that controls the running Cod (story #37).
//!
//! Cod listens on one private Unix socket per running app (`codctl_server`). This library holds what both sides share: where the socket is, the wire protocol, and the command line of the `codctl` binary.
//!
//! Socket: `$COD_SOCKET` when set, else `$XDG_RUNTIME_DIR/cod/codctl.sock`, else `<temp dir>/cod-<user>/codctl.sock`. The folder must be owned by the user and closed to group and others; Cod refuses to listen otherwise.
//!
//! Protocol, version 1: one JSON object per line, answered by one JSON line.
//!
//! - Request: `{"v":1,"id":7,"cmd":"workspace.new","args":{"name":"...","folders":["/abs"],"focus":false}}`.
//! - Reply: `{"v":1,"id":7,"ok":true,"result":{...}}` or `{"v":1,"id":7,"ok":false,"error":"<one line>"}`.
//! - Commands: `workspace.new`, `workspace.list`, `workspace.focus`, `workspace.rename`, `workspace.close`, `workspace.add_folder`; from #38 `pane.list`, `pane.split`, `pane.focus`, `pane.close`, `pane.resize`, `open` (a file or, with a `url` argument, a page) and `diff` (two files, or two git revisions), plus `page.get`, `page.reload`, `page.back`, `page.forward` and `page.close`, where a pane is addressed by the ID `pane.list` prints and a page by the ID `open` (of a URL) prints. A workspace is addressed by its ID (the number Cod stores it under) or by its exact name.
//! - A request with another `v` gets an error reply, so a newer `codctl` fails clearly against an older Cod. The `id` lets one connection carry several requests later (`codctl events`, #41).

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The protocol version this build speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// The environment variable that overrides where the socket is.
pub const SOCKET_ENV: &str = "COD_SOCKET";

/// The environment variable that names the workspace a terminal or agent session of Cod belongs to.
pub const WORKSPACE_ENV: &str = "COD_WORKSPACE";

/// The socket's file name inside its folder.
pub const SOCKET_FILE: &str = "codctl.sock";

/// Exit code: done.
pub const EXIT_OK: i32 = 0;
/// Exit code: an error, a bad argument, or Cod is not running.
pub const EXIT_ERROR: i32 = 1;
/// Exit code of `workspace close`: the user kept the workspace.
pub const EXIT_KEPT: i32 = 2;

/// What `codctl` prints to stderr when no Cod listens on the socket.
pub const NOT_RUNNING: &str = "Cod is not running";

/// Where the socket is, read from the environment through `env`.
pub fn socket_path(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    let set = |name: &str| env(name).filter(|value| !value.is_empty());
    if let Some(path) = set(SOCKET_ENV) {
        return PathBuf::from(path);
    }
    if let Some(runtime) = set("XDG_RUNTIME_DIR") {
        return Path::new(&runtime).join("cod").join(SOCKET_FILE);
    }
    let user = set("USER")
        .or_else(|| set("LOGNAME"))
        .unwrap_or_else(|| "user".to_owned());
    std::env::temp_dir()
        .join(format!("cod-{user}"))
        .join(SOCKET_FILE)
}

/// Where the socket is, read from this process's environment.
pub fn default_socket_path() -> PathBuf {
    socket_path(&|name| std::env::var(name).ok())
}

/// One request to Cod.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Make a workspace in the window, with an optional name and folders. It takes focus only with `focus`.
    WorkspaceNew {
        name: Option<String>,
        folders: Vec<PathBuf>,
        focus: bool,
    },
    /// List every workspace of every window, pending (not loaded) ones included.
    WorkspaceList,
    /// Show the workspace in its window.
    WorkspaceFocus { workspace: String },
    /// Name the workspace.
    WorkspaceRename { workspace: String, name: String },
    /// Close the workspace; without `force` Cod asks about unsaved files as the UI does.
    WorkspaceClose { workspace: String, force: bool },
    /// Add folders to the workspace's project.
    WorkspaceAddFolder {
        workspace: String,
        folders: Vec<PathBuf>,
    },
    /// List the center panes of the workspace (#38). Pane, open and diff commands act on the named workspace, else on the shown workspace of the active window.
    PaneList { workspace: Option<String> },
    /// Split the pane; `direction` is `right` or `down`. The new pane is empty.
    PaneSplit {
        workspace: Option<String>,
        pane: u64,
        direction: String,
    },
    /// Focus the pane.
    PaneFocus { workspace: Option<String>, pane: u64 },
    /// Close the pane and its tabs; Cod asks about unsaved files as the UI does.
    PaneClose { workspace: Option<String>, pane: u64 },
    /// Sets the pane's share of its split, as a whole percentage from 1 to 99.
    PaneResize {
        workspace: Option<String>,
        pane: u64,
        size: u8,
    },
    /// Open a file, with the cursor at the 1-based line and column, in the pane or the active one.
    Open {
        workspace: Option<String>,
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
        pane: Option<u64>,
    },
    /// Opens a URL as a page tab (#31) in the active pane and returns its page ID.
    OpenPage { workspace: Option<String>, url: String },
    /// Show Zed's diff view of two files in a tab.
    Diff {
        workspace: Option<String>,
        old: PathBuf,
        new: PathBuf,
    },
    /// Show Zed's diff view of the commit `new` against its direct parent, checked to be `old`.
    DiffRevisions {
        workspace: Option<String>,
        old: String,
        new: String,
    },
    /// Prints a page's current URL, title and state (`loading`, `loaded` or `failed`).
    PageGet { page: String },
    /// Reloads the page tab.
    PageReload { page: String },
    /// Goes back in the page tab's history.
    PageBack { page: String },
    /// Goes forward in the page tab's history.
    PageForward { page: String },
    /// Closes the page tab.
    PageClose { page: String },
}

fn paths_json(paths: &[PathBuf]) -> Value {
    Value::Array(
        paths
            .iter()
            .map(|path| Value::String(path.to_string_lossy().into_owned()))
            .collect(),
    )
}

impl Request {
    /// The command name on the wire.
    pub fn command(&self) -> &'static str {
        match self {
            Request::WorkspaceNew { .. } => "workspace.new",
            Request::WorkspaceList => "workspace.list",
            Request::WorkspaceFocus { .. } => "workspace.focus",
            Request::WorkspaceRename { .. } => "workspace.rename",
            Request::WorkspaceClose { .. } => "workspace.close",
            Request::WorkspaceAddFolder { .. } => "workspace.add_folder",
            Request::PaneList { .. } => "pane.list",
            Request::PaneSplit { .. } => "pane.split",
            Request::PaneFocus { .. } => "pane.focus",
            Request::PaneClose { .. } => "pane.close",
            Request::PaneResize { .. } => "pane.resize",
            Request::Open { .. } => "open",
            Request::OpenPage { .. } => "open",
            Request::Diff { .. } => "diff",
            Request::DiffRevisions { .. } => "diff",
            Request::PageGet { .. } => "page.get",
            Request::PageReload { .. } => "page.reload",
            Request::PageBack { .. } => "page.back",
            Request::PageForward { .. } => "page.forward",
            Request::PageClose { .. } => "page.close",
        }
    }

    /// The arguments on the wire.
    pub fn args(&self) -> Value {
        match self {
            Request::WorkspaceNew {
                name,
                folders,
                focus,
            } => json!({ "name": name, "folders": paths_json(folders), "focus": focus }),
            Request::WorkspaceList => json!({}),
            Request::WorkspaceFocus { workspace } => json!({ "workspace": workspace }),
            Request::WorkspaceRename { workspace, name } => {
                json!({ "workspace": workspace, "name": name })
            }
            Request::WorkspaceClose { workspace, force } => {
                json!({ "workspace": workspace, "force": force })
            }
            Request::WorkspaceAddFolder { workspace, folders } => {
                json!({ "workspace": workspace, "folders": paths_json(folders) })
            }
            Request::PaneList { workspace } => json!({ "workspace": workspace }),
            Request::PaneSplit {
                workspace,
                pane,
                direction,
            } => json!({ "workspace": workspace, "pane": pane, "direction": direction }),
            Request::PaneFocus { workspace, pane } | Request::PaneClose { workspace, pane } => {
                json!({ "workspace": workspace, "pane": pane })
            }
            Request::PaneResize {
                workspace,
                pane,
                size,
            } => json!({ "workspace": workspace, "pane": pane, "size": size }),
            Request::Open {
                workspace,
                path,
                line,
                column,
                pane,
            } => json!({
                "workspace": workspace,
                "path": path.to_string_lossy(),
                "line": line,
                "column": column,
                "pane": pane,
            }),
            Request::OpenPage { workspace, url } => json!({ "workspace": workspace, "url": url }),
            Request::Diff {
                workspace,
                old,
                new,
            } => json!({
                "workspace": workspace,
                "old": old.to_string_lossy(),
                "new": new.to_string_lossy(),
            }),
            Request::DiffRevisions {
                workspace,
                old,
                new,
            } => json!({ "workspace": workspace, "old": old, "new": new }),
            Request::PageGet { page }
            | Request::PageReload { page }
            | Request::PageBack { page }
            | Request::PageForward { page }
            | Request::PageClose { page } => json!({ "page": page }),
        }
    }

    /// The request line, without the final newline.
    pub fn to_line(&self, id: u64) -> String {
        json!({ "v": PROTOCOL_VERSION, "id": id, "cmd": self.command(), "args": self.args() })
            .to_string()
    }
}

/// A parsed request line: the client's `id` (echoed in the reply) and the request.
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub id: Value,
    pub request: Request,
}

fn arg_string(args: &Map<String, Value>, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        Some(Value::String(_)) => Err(format!("{key} must not be empty")),
        _ => Err(format!("missing {key}")),
    }
}

fn arg_bool(args: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("{key} must be true or false")),
    }
}

fn arg_optional_string(args: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => arg_string(args, key).map(Some),
    }
}

fn arg_optional_number(args: &Map<String, Value>, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be a whole number")),
    }
}

fn arg_number(args: &Map<String, Value>, key: &str) -> Result<u64, String> {
    arg_optional_number(args, key)?.ok_or_else(|| format!("missing {key}"))
}

fn arg_position(args: &Map<String, Value>, key: &str) -> Result<Option<u32>, String> {
    match arg_optional_number(args, key)? {
        None => Ok(None),
        Some(number) => u32::try_from(number)
            .ok()
            .filter(|number| *number > 0)
            .map(Some)
            .ok_or_else(|| format!("{key} must be a number from 1")),
    }
}

fn arg_file(args: &Map<String, Value>, key: &str) -> Result<PathBuf, String> {
    let path = arg_string(args, key)?;
    if Path::new(&path).is_absolute() {
        Ok(PathBuf::from(path))
    } else {
        Err(format!("{key} must be an absolute path: {path}"))
    }
}

fn arg_folders(args: &Map<String, Value>, required: bool) -> Result<Vec<PathBuf>, String> {
    let folders = match args.get("folders") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(path) if Path::new(path).is_absolute() => Ok(PathBuf::from(path)),
                Value::String(path) => Err(format!("folder must be an absolute path: {path}")),
                _ => Err("folders must be paths".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("folders must be a list of paths".to_owned()),
    };
    if required && folders.is_empty() {
        return Err("missing folder".to_owned());
    }
    Ok(folders)
}

/// Parses one request line. An error carries the request's `id` (null when unreadable) for the error reply.
pub fn parse_request_line(line: &str) -> Result<Envelope, (Value, String)> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| (Value::Null, format!("invalid request: {error}")))?;
    let Value::Object(object) = value else {
        return Err((Value::Null, "invalid request: not a JSON object".to_owned()));
    };
    let id = object.get("id").cloned().unwrap_or(Value::Null);
    let fail = |message: String| (id.clone(), message);
    match object.get("v").and_then(Value::as_u64) {
        Some(PROTOCOL_VERSION) => {}
        Some(other) => {
            return Err(fail(format!(
                "protocol version {other} is not supported: this Cod speaks version {PROTOCOL_VERSION}"
            )));
        }
        None => return Err(fail("invalid request: missing protocol version v".to_owned())),
    }
    let command = object
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("invalid request: missing cmd".to_owned()))?;
    let empty = Map::new();
    let args = match object.get("args") {
        None | Some(Value::Null) => &empty,
        Some(Value::Object(args)) => args,
        Some(_) => return Err(fail("invalid request: args must be an object".to_owned())),
    };
    let request = match command {
        "workspace.new" => Request::WorkspaceNew {
            name: match args.get("name") {
                None | Some(Value::Null) => None,
                Some(_) => Some(arg_string(args, "name").map_err(&fail)?),
            },
            folders: arg_folders(args, false).map_err(&fail)?,
            focus: arg_bool(args, "focus").map_err(&fail)?,
        },
        "workspace.list" => Request::WorkspaceList,
        "workspace.focus" => Request::WorkspaceFocus {
            workspace: arg_string(args, "workspace").map_err(&fail)?,
        },
        "workspace.rename" => Request::WorkspaceRename {
            workspace: arg_string(args, "workspace").map_err(&fail)?,
            name: arg_string(args, "name").map_err(&fail)?,
        },
        "workspace.close" => Request::WorkspaceClose {
            workspace: arg_string(args, "workspace").map_err(&fail)?,
            force: arg_bool(args, "force").map_err(&fail)?,
        },
        "workspace.add_folder" => Request::WorkspaceAddFolder {
            workspace: arg_string(args, "workspace").map_err(&fail)?,
            folders: arg_folders(args, true).map_err(&fail)?,
        },
        "pane.list" => Request::PaneList {
            workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
        },
        "pane.split" => Request::PaneSplit {
            workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
            pane: arg_number(args, "pane").map_err(&fail)?,
            direction: match arg_string(args, "direction").map_err(&fail)?.as_str() {
                direction @ ("right" | "down") => direction.to_owned(),
                other => return Err(fail(format!("direction must be right or down, not {other}"))),
            },
        },
        "pane.focus" => Request::PaneFocus {
            workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
            pane: arg_number(args, "pane").map_err(&fail)?,
        },
        "pane.close" => Request::PaneClose {
            workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
            pane: arg_number(args, "pane").map_err(&fail)?,
        },
        "pane.resize" => Request::PaneResize {
            workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
            pane: arg_number(args, "pane").map_err(&fail)?,
            size: {
                let size = arg_number(args, "size").map_err(&fail)?;
                u8::try_from(size)
                    .ok()
                    .filter(|size| (1..=99).contains(size))
                    .ok_or_else(|| fail("size must be a whole percentage from 1 to 99".to_owned()))?
            },
        },
        "open" => match args.get("url") {
            Some(_) => Request::OpenPage {
                workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
                url: arg_string(args, "url").map_err(&fail)?,
            },
            None => Request::Open {
                workspace: arg_optional_string(args, "workspace").map_err(&fail)?,
                path: arg_file(args, "path").map_err(&fail)?,
                line: arg_position(args, "line").map_err(&fail)?,
                column: arg_position(args, "column").map_err(&fail)?,
                pane: arg_optional_number(args, "pane").map_err(&fail)?,
            },
        },
        "diff" => {
            let old = arg_string(args, "old").map_err(&fail)?;
            let new = arg_string(args, "new").map_err(&fail)?;
            let workspace = arg_optional_string(args, "workspace").map_err(&fail)?;
            match (old.starts_with('/'), new.starts_with('/')) {
                (true, true) => Request::Diff {
                    workspace,
                    old: PathBuf::from(old),
                    new: PathBuf::from(new),
                },
                (false, false) => Request::DiffRevisions { workspace, old, new },
                _ => return Err(fail("diff needs two files or two revisions, not one of each".to_owned())),
            }
        }
        "page.get" => Request::PageGet {
            page: arg_string(args, "page").map_err(&fail)?,
        },
        "page.reload" => Request::PageReload {
            page: arg_string(args, "page").map_err(&fail)?,
        },
        "page.back" => Request::PageBack {
            page: arg_string(args, "page").map_err(&fail)?,
        },
        "page.forward" => Request::PageForward {
            page: arg_string(args, "page").map_err(&fail)?,
        },
        "page.close" => Request::PageClose {
            page: arg_string(args, "page").map_err(&fail)?,
        },
        other => return Err(fail(format!("unknown command {other}"))),
    };
    Ok(Envelope { id, request })
}

/// Makes a message fit on one line.
pub fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The reply line for a request that succeeded, without the final newline.
pub fn reply_ok(id: &Value, result: Value) -> String {
    json!({ "v": PROTOCOL_VERSION, "id": id, "ok": true, "result": result }).to_string()
}

/// The reply line for a request that failed, without the final newline.
pub fn reply_error(id: &Value, error: &str) -> String {
    json!({ "v": PROTOCOL_VERSION, "id": id, "ok": false, "error": one_line(error) }).to_string()
}

/// Reads a reply line: the result, or the error message.
pub fn parse_reply_line(line: &str) -> Result<Result<Value, String>, String> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| format!("invalid reply from Cod: {error}"))?;
    match value.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(Ok(value.get("result").cloned().unwrap_or(Value::Null))),
        Some(false) => Ok(Err(one_line(
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("Cod gave no reason"),
        ))),
        None => Err("invalid reply from Cod: no ok field".to_owned()),
    }
}

/// The help text of `codctl --help`.
pub const HELP: &str = "codctl - control the running Cod

Usage: codctl workspace <command> [arguments] [--json]

Workspace commands:
  workspace new [--name <name>] [--folder <path>]... [--focus]
      Make a workspace and print its ID. It takes focus only with --focus.
  workspace list
      Print ID, status and name of every workspace, one per line.
  workspace focus [<workspace>]
      Show the workspace.
  workspace rename [<workspace>] <name>
      Name the workspace.
  workspace close [<workspace>] [--force]
      Close the workspace. Cod asks about unsaved files as the UI does; --force drops them.
  workspace add-folder [<workspace>] <path>...
      Add folders to the workspace.

Pane, file and diff commands:
  pane list
      Print ID, active or -, and the tabs of every pane, one pane per line.
  pane split <pane> --right|--down
      Split the pane, making a new empty pane, and print its ID.
  pane focus <pane>
      Focus the pane.
  pane close <pane>
      Close the pane and its tabs. Cod asks about unsaved files as the UI does.
  pane resize <pane> --size <percent>%
      Set the pane's share of its split, a whole percentage from 1 to 99.
  open <file>[:<line>[:<column>]] [--pane <pane>]
      Open the file in the active pane (or the pane), with the cursor at the line and column.
  open <url>
      Open the URL as a page tab in the active pane and print its page ID, such as page-3.
  diff <old file> <new file>
      Show the diff of two files in a tab.
  diff <old revision> <new revision>
      Show the diff of the workspace's repo between two git revisions (for example HEAD~1 HEAD)
      in a tab. A revision contains no /; anything with a / is read as a file.

Page commands:
  page get <page> [--json]
      Print the page's URL, title and state (loading, loaded or failed), tab-separated.
  page reload <page>
      Reload the page tab.
  page back <page>
      Go back in the page tab's history.
  page forward <page>
      Go forward in the page tab's history.
  page close <page>
      Close the page tab.

<workspace> is a workspace ID or its exact name, also given as --workspace <workspace>.
Without one, workspace commands act on the workspace in $COD_WORKSPACE. Pane, file and diff commands act on --workspace, else $COD_WORKSPACE, else the workspace shown in the active window.
Page commands act on the page ID alone: a page keeps the same ID across every workspace and window, and across a restart.
--json prints the result as JSON.

Exit codes: 0 done, 1 error or Cod is not running, 2 the user kept the workspace or pane (close).
";

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cli {
    /// Print the help text.
    Help,
    /// Send the request; `json` prints the result as JSON.
    Run { request: Request, json: bool },
}

/// The options given on the command line.
#[derive(Default)]
struct Options {
    json: bool,
    focus: bool,
    force: bool,
    right: bool,
    down: bool,
    name: Option<String>,
    folders: Vec<String>,
    workspace: Option<String>,
    pane: Option<String>,
    size: Option<String>,
    seen: Vec<&'static str>,
    positionals: Vec<String>,
}

fn take_value(
    flag: &'static str,
    rest: &mut std::slice::Iter<'_, String>,
) -> Result<String, String> {
    match rest.next() {
        Some(value) if !value.starts_with("--") => Ok(value.clone()),
        _ => Err(format!("{flag} needs a value")),
    }
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut rest = args.iter();
    let mut only_positionals = false;
    while let Some(arg) = rest.next() {
        if only_positionals || !arg.starts_with("--") {
            options.positionals.push(arg.clone());
            continue;
        }
        let flag: &'static str = match arg.as_str() {
            "--" => {
                only_positionals = true;
                continue;
            }
            "--json" => {
                options.json = true;
                "--json"
            }
            "--focus" => {
                options.focus = true;
                "--focus"
            }
            "--force" => {
                options.force = true;
                "--force"
            }
            "--right" => {
                options.right = true;
                "--right"
            }
            "--down" => {
                options.down = true;
                "--down"
            }
            "--pane" => {
                if options.pane.is_some() {
                    return Err("--pane is given twice".to_owned());
                }
                options.pane = Some(take_value("--pane", &mut rest)?);
                "--pane"
            }
            "--size" => {
                if options.size.is_some() {
                    return Err("--size is given twice".to_owned());
                }
                options.size = Some(take_value("--size", &mut rest)?);
                "--size"
            }
            "--name" => {
                if options.name.is_some() {
                    return Err("--name is given twice".to_owned());
                }
                options.name = Some(take_value("--name", &mut rest)?);
                "--name"
            }
            "--folder" => {
                options.folders.push(take_value("--folder", &mut rest)?);
                "--folder"
            }
            "--workspace" => {
                if options.workspace.is_some() {
                    return Err("--workspace is given twice".to_owned());
                }
                options.workspace = Some(take_value("--workspace", &mut rest)?);
                "--workspace"
            }
            other => return Err(format!("unknown option {other}")),
        };
        if !options.seen.contains(&flag) {
            options.seen.push(flag);
        }
    }
    Ok(options)
}

/// Makes a folder argument absolute: `~` and `~/...` start at `$HOME`, a relative path starts at `cwd`.
pub fn absolute_folder(
    path: &str,
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("a folder path must not be empty".to_owned());
    }
    let home = || {
        env("HOME")
            .filter(|home| !home.is_empty())
            .ok_or_else(|| format!("cannot expand {path}: HOME is not set"))
    };
    let path = if path == "~" {
        PathBuf::from(home()?)
    } else if let Some(below) = path.strip_prefix("~/") {
        Path::new(&home()?).join(below)
    } else {
        PathBuf::from(path)
    };
    Ok(if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    })
}

/// Parses the arguments after the program name. `env` reads the environment (`COD_WORKSPACE`, `HOME`); `cwd` makes relative folders absolute.
pub fn parse_cli(
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
) -> Result<Cli, String> {
    let wants_help = |arg: &String| matches!(arg.as_str(), "--help" | "-h" | "help");
    let Some(group) = args.first() else {
        return Ok(Cli::Help);
    };
    if wants_help(group) {
        return Ok(Cli::Help);
    }
    if matches!(group.as_str(), "pane" | "open" | "diff" | "page") {
        return parse_layout_cli(group, &args[1..], env, cwd);
    }
    if group != "workspace" {
        return Err(format!("unknown command {group}: run codctl --help"));
    }
    let Some(command) = args.get(1) else {
        return Ok(Cli::Help);
    };
    if wants_help(command) || args[2..].iter().any(|arg| arg == "--help") {
        return Ok(Cli::Help);
    }
    let options = parse_options(&args[2..])?;
    let allowed: &[&str] = match command.as_str() {
        "new" => &["--json", "--name", "--folder", "--focus"],
        "list" => &["--json"],
        "focus" => &["--json", "--workspace"],
        "rename" => &["--json", "--workspace"],
        "close" => &["--json", "--workspace", "--force"],
        "add-folder" => &["--json", "--workspace"],
        other => return Err(format!("unknown command workspace {other}: run codctl --help")),
    };
    if let Some(flag) = options.seen.iter().find(|flag| !allowed.contains(*flag)) {
        return Err(format!("workspace {command} does not take {flag}"));
    }
    let from_env = || {
        env(WORKSPACE_ENV)
            .filter(|workspace| !workspace.is_empty())
            .ok_or_else(|| format!("workspace {command} needs a workspace ID or name, and {WORKSPACE_ENV} is not set"))
    };
    let mut positionals = options.positionals.clone();
    let too_many = |expected: &str| format!("workspace {command} takes {expected}");
    // The workspace from --workspace, else from the first positional when `takes_positional`, else from COD_WORKSPACE.
    let target = |positionals: &mut Vec<String>, takes_positional: bool| -> Result<String, String> {
        if let Some(workspace) = options.workspace.clone() {
            return Ok(workspace);
        }
        if takes_positional && !positionals.is_empty() {
            return Ok(positionals.remove(0));
        }
        from_env()
    };
    let request = match command.as_str() {
        "new" => {
            if !positionals.is_empty() {
                return Err(too_many("no positional arguments: use --name and --folder"));
            }
            if options.name.as_deref() == Some("") {
                return Err("--name must not be empty".to_owned());
            }
            Request::WorkspaceNew {
                name: options.name.clone(),
                folders: options
                    .folders
                    .iter()
                    .map(|folder| absolute_folder(folder, env, cwd))
                    .collect::<Result<_, _>>()?,
                focus: options.focus,
            }
        }
        "list" => {
            if !positionals.is_empty() {
                return Err(too_many("no arguments"));
            }
            Request::WorkspaceList
        }
        "focus" | "close" => {
            if positionals.len() > 1 {
                return Err(too_many("at most one workspace"));
            }
            let workspace = target(&mut positionals, true)?;
            if !positionals.is_empty() {
                return Err(too_many("one workspace, given once"));
            }
            if command == "focus" {
                Request::WorkspaceFocus { workspace }
            } else {
                Request::WorkspaceClose {
                    workspace,
                    force: options.force,
                }
            }
        }
        "rename" => {
            let workspace = match positionals.len() {
                2 => target(&mut positionals, true)?,
                1 => target(&mut positionals, false)?,
                _ => return Err(too_many("a workspace and a name, or a name with COD_WORKSPACE")),
            };
            if positionals.len() != 1 {
                return Err(too_many("one workspace, given once, and a name"));
            }
            let name = positionals.remove(0);
            if name.is_empty() {
                return Err("the name must not be empty".to_owned());
            }
            Request::WorkspaceRename { workspace, name }
        }
        "add-folder" => {
            let workspace = match positionals.len() {
                0 => return Err(too_many("a workspace and at least one folder")),
                1 => target(&mut positionals, false)?,
                _ => target(&mut positionals, true)?,
            };
            if positionals.is_empty() {
                return Err(too_many("at least one folder"));
            }
            Request::WorkspaceAddFolder {
                workspace,
                folders: positionals
                    .iter()
                    .map(|folder| absolute_folder(folder, env, cwd))
                    .collect::<Result<_, _>>()?,
            }
        }
        _ => unreachable!("checked above"),
    };
    Ok(Cli::Run {
        request,
        json: options.json,
    })
}

/// Splits `path:line:column` or `path:line` into its parts. A part after a colon that is not a number from 1 belongs to the path.
pub fn split_position(target: &str) -> (&str, Option<u32>, Option<u32>) {
    let number = |text: &str| {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        text.parse::<u32>().ok().filter(|number| *number > 0)
    };
    if let Some((rest, last)) = target.rsplit_once(':')
        && let Some(last) = number(last)
    {
        if let Some((path, middle)) = rest.rsplit_once(':')
            && let Some(middle) = number(middle)
            && !path.is_empty()
        {
            return (path, Some(middle), Some(last));
        }
        if !rest.is_empty() {
            return (rest, Some(last), None);
        }
    }
    (target, None, None)
}

fn pane_id(text: &str) -> Result<u64, String> {
    text.parse::<u64>()
        .map_err(|_| format!("a pane ID is a number from codctl pane list, not {text:?}"))
}

/// Parses `pane <command> ...`, `open ...` and `diff ...` (#38). `args` are the arguments after the group.
fn parse_layout_cli(
    group: &str,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    cwd: &Path,
) -> Result<Cli, String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        return Ok(Cli::Help);
    }
    let (command, rest) = if group == "pane" || group == "page" {
        let Some(command) = args.first() else {
            return Ok(Cli::Help);
        };
        if command == "help" {
            return Ok(Cli::Help);
        }
        (format!("{group} {command}"), &args[1..])
    } else {
        (group.to_owned(), args)
    };
    let options = parse_options(rest)?;
    let allowed: &[&str] = match command.as_str() {
        "pane list" | "pane focus" | "pane close" | "diff" => &["--json", "--workspace"],
        "pane split" => &["--json", "--workspace", "--right", "--down"],
        "pane resize" => &["--json", "--workspace", "--size"],
        "open" => &["--json", "--workspace", "--pane"],
        "page get" | "page reload" | "page back" | "page forward" | "page close" => &["--json"],
        _ => return Err(format!("unknown command {command}: run codctl --help")),
    };
    if let Some(flag) = options.seen.iter().find(|flag| !allowed.contains(*flag)) {
        return Err(format!("{command} does not take {flag}"));
    }
    let workspace = options
        .workspace
        .clone()
        .or_else(|| env(WORKSPACE_ENV).filter(|workspace| !workspace.is_empty()));
    let positionals = options.positionals.as_slice();
    let takes = |expected: &str| format!("{command} takes {expected}");
    let request = match command.as_str() {
        "pane list" => {
            if !positionals.is_empty() {
                return Err(takes("no arguments"));
            }
            Request::PaneList { workspace }
        }
        "pane split" => {
            let [pane] = positionals else {
                return Err(takes("one pane ID and --right or --down"));
            };
            let direction = match (options.right, options.down) {
                (true, false) => "right",
                (false, true) => "down",
                _ => return Err(takes("one of --right and --down")),
            };
            Request::PaneSplit {
                workspace,
                pane: pane_id(pane)?,
                direction: direction.to_owned(),
            }
        }
        "pane focus" | "pane close" => {
            let [pane] = positionals else {
                return Err(takes("one pane ID"));
            };
            let pane = pane_id(pane)?;
            if command == "pane focus" {
                Request::PaneFocus { workspace, pane }
            } else {
                Request::PaneClose { workspace, pane }
            }
        }
        "pane resize" => {
            let [pane] = positionals else {
                return Err(takes("one pane ID and --size <percent>%"));
            };
            let Some(size) = options.size.as_deref() else {
                return Err(takes("--size <percent>%"));
            };
            Request::PaneResize {
                workspace,
                pane: pane_id(pane)?,
                size: parse_size_percent(size)?,
            }
        }
        "open" => {
            let [target] = positionals else {
                return Err(takes("one file, as <file>[:<line>[:<column>]], or one URL"));
            };
            if target.contains("://") {
                if options.pane.is_some() {
                    return Err(takes("a URL, which does not take --pane"));
                }
                Request::OpenPage {
                    workspace,
                    url: target.clone(),
                }
            } else {
                let (path, line, column) = split_position(target);
                Request::Open {
                    workspace,
                    path: absolute_folder(path, env, cwd)?,
                    line,
                    column,
                    pane: options.pane.as_deref().map(pane_id).transpose()?,
                }
            }
        }
        "diff" => {
            let [old, new] = positionals else {
                return Err(takes("two files, or two git revisions"));
            };
            if old.contains('/') || new.contains('/') {
                Request::Diff {
                    workspace,
                    old: absolute_folder(old, env, cwd)?,
                    new: absolute_folder(new, env, cwd)?,
                }
            } else {
                Request::DiffRevisions {
                    workspace,
                    old: old.clone(),
                    new: new.clone(),
                }
            }
        }
        "page get" | "page reload" | "page back" | "page forward" | "page close" => {
            let [page] = positionals else {
                return Err(takes("one page ID"));
            };
            let page = page.clone();
            match command.as_str() {
                "page get" => Request::PageGet { page },
                "page reload" => Request::PageReload { page },
                "page back" => Request::PageBack { page },
                "page forward" => Request::PageForward { page },
                _ => Request::PageClose { page },
            }
        }
        _ => unreachable!("checked above"),
    };
    Ok(Cli::Run {
        request,
        json: options.json,
    })
}

/// Parses `<percent>%` into a whole percentage from 1 to 99.
fn parse_size_percent(text: &str) -> Result<u8, String> {
    text.strip_suffix('%')
        .and_then(|digits| digits.parse::<u8>().ok())
        .filter(|size| (1..=99).contains(size))
        .ok_or_else(|| format!("--size must be a percentage from 1 to 99, such as 40%, not {text:?}"))
}

/// What `codctl` prints on stdout for a successful reply, and its exit code.
pub fn render(request: &Request, json: bool, result: &Value) -> (String, i32) {
    match request {
        Request::WorkspaceList => {
            if json {
                return (format!("{result}\n"), EXIT_OK);
            }
            let mut out = String::new();
            for workspace in result.as_array().into_iter().flatten() {
                let id = workspace.get("id").map(Value::to_string).unwrap_or_default();
                let status = workspace
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("-");
                let name = workspace.get("name").and_then(Value::as_str).unwrap_or("-");
                out.push_str(&format!("{id}\t{status}\t{}\n", one_line(name)));
            }
            (out, EXIT_OK)
        }
        Request::WorkspaceNew { .. } => {
            if json {
                (format!("{result}\n"), EXIT_OK)
            } else {
                let id = result.get("id").map(Value::to_string).unwrap_or_default();
                (format!("{id}\n"), EXIT_OK)
            }
        }
        Request::WorkspaceClose { .. } => {
            let closed = result.get("closed").and_then(Value::as_bool).unwrap_or(false);
            let out = if json { format!("{result}\n") } else { String::new() };
            (out, if closed { EXIT_OK } else { EXIT_KEPT })
        }
        Request::WorkspaceFocus { .. }
        | Request::WorkspaceRename { .. }
        | Request::WorkspaceAddFolder { .. }
        | Request::PaneFocus { .. }
        | Request::PaneResize { .. }
        | Request::Open { .. }
        | Request::Diff { .. }
        | Request::DiffRevisions { .. }
        | Request::PageReload { .. }
        | Request::PageBack { .. }
        | Request::PageForward { .. }
        | Request::PageClose { .. } => {
            let out = if json { format!("{result}\n") } else { String::new() };
            (out, EXIT_OK)
        }
        Request::PaneList { .. } => {
            if json {
                return (format!("{result}\n"), EXIT_OK);
            }
            let mut out = String::new();
            for pane in result.as_array().into_iter().flatten() {
                let id = pane.get("id").map(Value::to_string).unwrap_or_default();
                let active = if pane.get("active").and_then(Value::as_bool) == Some(true) {
                    "active"
                } else {
                    "-"
                };
                let tabs: Vec<String> = pane
                    .get("tabs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(one_line)
                    .collect();
                out.push_str(&format!("{id}\t{active}\t{}\n", tabs.join("\t")));
            }
            (out, EXIT_OK)
        }
        Request::PaneSplit { .. } => {
            if json {
                (format!("{result}\n"), EXIT_OK)
            } else {
                let id = result.get("id").map(Value::to_string).unwrap_or_default();
                (format!("{id}\n"), EXIT_OK)
            }
        }
        Request::PaneClose { .. } => {
            let closed = result.get("closed").and_then(Value::as_bool).unwrap_or(false);
            let out = if json { format!("{result}\n") } else { String::new() };
            (out, if closed { EXIT_OK } else { EXIT_KEPT })
        }
        Request::OpenPage { .. } => {
            if json {
                (format!("{result}\n"), EXIT_OK)
            } else {
                let id = result.get("id").and_then(Value::as_str).unwrap_or_default();
                (format!("{id}\n"), EXIT_OK)
            }
        }
        Request::PageGet { .. } => {
            if json {
                return (format!("{result}\n"), EXIT_OK);
            }
            let url = result.get("url").and_then(Value::as_str).unwrap_or_default();
            let title = result.get("title").and_then(Value::as_str).unwrap_or("-");
            let state = result.get("state").and_then(Value::as_str).unwrap_or_default();
            (format!("{url}\t{}\t{state}\n", one_line(title)), EXIT_OK)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    fn args(line: &[&str]) -> Vec<String> {
        line.iter().map(|arg| arg.to_string()).collect()
    }

    fn parse(line: &[&str], env: &[(&str, &str)]) -> Result<Cli, String> {
        parse_cli(&args(line), &env_of(env), Path::new("/cwd"))
    }

    fn run(request: Request, json: bool) -> Result<Cli, String> {
        Ok(Cli::Run { request, json })
    }

    #[test]
    fn socket_path_prefers_cod_socket_then_runtime_dir() {
        assert_eq!(
            socket_path(&env_of(&[("COD_SOCKET", "/s/c.sock"), ("XDG_RUNTIME_DIR", "/run/user/1")])),
            PathBuf::from("/s/c.sock")
        );
        assert_eq!(
            socket_path(&env_of(&[("COD_SOCKET", ""), ("XDG_RUNTIME_DIR", "/run/user/1")])),
            PathBuf::from("/run/user/1/cod/codctl.sock")
        );
        assert_eq!(
            socket_path(&env_of(&[("USER", "ana")])),
            std::env::temp_dir().join("cod-ana").join("codctl.sock")
        );
    }

    #[test]
    fn help_and_unknown_commands() {
        assert_eq!(parse(&[], &[]), Ok(Cli::Help));
        assert_eq!(parse(&["--help"], &[]), Ok(Cli::Help));
        assert_eq!(parse(&["workspace", "list", "--help"], &[]), Ok(Cli::Help));
        assert!(parse(&["frobnicate"], &[]).is_err());
        assert!(parse(&["workspace", "explode"], &[]).is_err());
        assert!(parse(&["workspace", "list", "--bogus"], &[]).is_err());
        assert!(parse(&["workspace", "list", "extra"], &[]).is_err());
        assert!(parse(&["workspace", "list", "--force"], &[]).is_err());
        for command in ["new", "list", "focus", "rename", "close", "add-folder"] {
            assert!(HELP.contains(&format!("workspace {command}")), "{command}");
        }
    }

    #[test]
    fn workspace_new() {
        assert_eq!(
            parse(
                &["workspace", "new", "--name", "[PAY-1234] (billing-api) Fix", "--folder", "~/work/billing-api", "--folder", "rel"],
                &[("HOME", "/home/ana")]
            ),
            run(
                Request::WorkspaceNew {
                    name: Some("[PAY-1234] (billing-api) Fix".to_owned()),
                    folders: vec![PathBuf::from("/home/ana/work/billing-api"), PathBuf::from("/cwd/rel")],
                    focus: false,
                },
                false
            )
        );
        assert_eq!(
            parse(&["workspace", "new", "--focus", "--json"], &[]),
            run(Request::WorkspaceNew { name: None, folders: vec![], focus: true }, true)
        );
        assert!(parse(&["workspace", "new", "--name"], &[]).is_err());
        assert!(parse(&["workspace", "new", "--name", ""], &[]).is_err());
        assert!(parse(&["workspace", "new", "positional"], &[]).is_err());
    }

    #[test]
    fn workspace_targets_from_arguments_or_cod_workspace() {
        let env = [("COD_WORKSPACE", "7")];
        assert_eq!(
            parse(&["workspace", "focus", "3"], &env),
            run(Request::WorkspaceFocus { workspace: "3".to_owned() }, false)
        );
        assert_eq!(
            parse(&["workspace", "focus"], &env),
            run(Request::WorkspaceFocus { workspace: "7".to_owned() }, false)
        );
        assert!(parse(&["workspace", "focus"], &[]).is_err());
        assert!(parse(&["workspace", "focus", "1", "2"], &env).is_err());
        assert!(parse(&["workspace", "focus", "--workspace", "1", "2"], &env).is_err());
        assert_eq!(
            parse(&["workspace", "rename", "3", "New name"], &env),
            run(Request::WorkspaceRename { workspace: "3".to_owned(), name: "New name".to_owned() }, false)
        );
        assert_eq!(
            parse(&["workspace", "rename", "New name"], &env),
            run(Request::WorkspaceRename { workspace: "7".to_owned(), name: "New name".to_owned() }, false)
        );
        assert!(parse(&["workspace", "rename", "3", ""], &env).is_err());
        assert!(parse(&["workspace", "rename"], &env).is_err());
        assert_eq!(
            parse(&["workspace", "close", "3", "--force"], &[]),
            run(Request::WorkspaceClose { workspace: "3".to_owned(), force: true }, false)
        );
        assert_eq!(
            parse(&["workspace", "add-folder", "3", "/a", "/b"], &env),
            run(
                Request::WorkspaceAddFolder {
                    workspace: "3".to_owned(),
                    folders: vec![PathBuf::from("/a"), PathBuf::from("/b")],
                },
                false
            )
        );
        assert_eq!(
            parse(&["workspace", "add-folder", "/a"], &env),
            run(
                Request::WorkspaceAddFolder { workspace: "7".to_owned(), folders: vec![PathBuf::from("/a")] },
                false
            )
        );
        assert!(parse(&["workspace", "add-folder", "3"], &[]).is_err());
    }

    #[test]
    fn requests_round_trip_through_the_wire() {
        let requests = [
            Request::WorkspaceNew {
                name: Some("n".to_owned()),
                folders: vec![PathBuf::from("/a")],
                focus: true,
            },
            Request::WorkspaceNew { name: None, folders: vec![], focus: false },
            Request::WorkspaceList,
            Request::WorkspaceFocus { workspace: "1".to_owned() },
            Request::WorkspaceRename { workspace: "1".to_owned(), name: "x".to_owned() },
            Request::WorkspaceClose { workspace: "1".to_owned(), force: true },
            Request::WorkspaceAddFolder { workspace: "1".to_owned(), folders: vec![PathBuf::from("/b")] },
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let line = request.to_line(index as u64);
            assert_eq!(
                parse_request_line(&line),
                Ok(Envelope { id: json!(index), request }),
                "{line}"
            );
        }
    }

    #[test]
    fn bad_requests_get_errors_with_their_id() {
        assert_eq!(parse_request_line("nope").unwrap_err().0, Value::Null);
        let (id, error) = parse_request_line(r#"{"v":2,"id":5,"cmd":"workspace.list"}"#).unwrap_err();
        assert_eq!(id, json!(5));
        assert!(error.contains("version 2"), "{error}");
        assert!(parse_request_line(r#"{"id":5,"cmd":"workspace.list"}"#).is_err());
        assert!(parse_request_line(r#"{"v":1,"id":5,"cmd":"workspace.fly"}"#).is_err());
        assert!(parse_request_line(r#"{"v":1,"cmd":"workspace.add_folder","args":{"workspace":"1","folders":["rel"]}}"#).is_err());
        assert!(parse_request_line(r#"{"v":1,"cmd":"workspace.add_folder","args":{"workspace":"1"}}"#).is_err());
        assert!(parse_request_line(r#"{"v":1,"cmd":"workspace.focus","args":{}}"#).is_err());
    }

    #[test]
    fn replies_are_one_line_and_parse_back() {
        let error = reply_error(&json!(3), "unknown workspace\n\"x\"");
        assert!(!error.contains('\n'));
        assert_eq!(parse_reply_line(&error), Ok(Err("unknown workspace \"x\"".to_owned())));
        assert_eq!(
            parse_reply_line(&reply_ok(&json!(3), json!({"id": 4}))),
            Ok(Ok(json!({"id": 4})))
        );
        assert!(parse_reply_line("{}").is_err());
    }

    #[test]
    fn rendering() {
        let list = json!([
            {"id": 1, "name": "[PAY-1] (api) Fix", "status": "idle", "folders": ["/a"], "active": true, "loaded": true},
            {"id": 2, "name": null, "status": "idle", "folders": [], "active": false, "loaded": false},
        ]);
        assert_eq!(
            render(&Request::WorkspaceList, false, &list),
            ("1\tidle\t[PAY-1] (api) Fix\n2\tidle\t-\n".to_owned(), EXIT_OK)
        );
        assert_eq!(render(&Request::WorkspaceList, true, &list).0, format!("{list}\n"));
        let new = Request::WorkspaceNew { name: None, folders: vec![], focus: false };
        assert_eq!(render(&new, false, &json!({"id": 12})), ("12\n".to_owned(), EXIT_OK));
        let close = Request::WorkspaceClose { workspace: "1".to_owned(), force: false };
        assert_eq!(render(&close, false, &json!({"closed": true})), (String::new(), EXIT_OK));
        assert_eq!(render(&close, false, &json!({"closed": false})), (String::new(), EXIT_KEPT));
    }

    #[test]
    fn file_positions() {
        assert_eq!(split_position("src/invoice.rs:18:9"), ("src/invoice.rs", Some(18), Some(9)));
        assert_eq!(split_position("src/invoice.rs:18"), ("src/invoice.rs", Some(18), None));
        assert_eq!(split_position("src/invoice.rs"), ("src/invoice.rs", None, None));
        assert_eq!(split_position("notes:draft.md"), ("notes:draft.md", None, None));
        assert_eq!(split_position("a.rs:0"), ("a.rs:0", None, None));
        assert_eq!(split_position(":5"), (":5", None, None));
        assert_eq!(split_position("x:y:7"), ("x:y", Some(7), None));
    }

    #[test]
    fn pane_open_and_diff_commands() {
        let none: &[(&str, &str)] = &[];
        assert_eq!(parse(&["pane", "list"], none), run(Request::PaneList { workspace: None }, false));
        assert_eq!(
            parse(&["pane", "list", "--json"], &[("COD_WORKSPACE", "4")]),
            run(Request::PaneList { workspace: Some("4".to_owned()) }, true)
        );
        assert_eq!(
            parse(&["pane", "split", "12", "--down", "--workspace", "billing"], &[("COD_WORKSPACE", "4")]),
            run(
                Request::PaneSplit { workspace: Some("billing".to_owned()), pane: 12, direction: "down".to_owned() },
                false
            )
        );
        assert!(parse(&["pane", "split", "12"], none).is_err());
        assert!(parse(&["pane", "split", "12", "--right", "--down"], none).is_err());
        assert!(parse(&["pane", "split", "twelve", "--right"], none).is_err());
        assert_eq!(parse(&["pane", "focus", "3"], none), run(Request::PaneFocus { workspace: None, pane: 3 }, false));
        assert_eq!(parse(&["pane", "close", "3"], none), run(Request::PaneClose { workspace: None, pane: 3 }, false));
        assert!(parse(&["pane", "close"], none).is_err());
        assert!(parse(&["pane", "explode", "3"], none).is_err());
        assert!(parse(&["pane", "focus", "3", "--force"], none).is_err());
        assert_eq!(
            parse(&["open", "src/invoice.rs:18:9", "--pane", "7"], none),
            run(
                Request::Open {
                    workspace: None,
                    path: PathBuf::from("/cwd/src/invoice.rs"),
                    line: Some(18),
                    column: Some(9),
                    pane: Some(7),
                },
                false
            )
        );
        assert!(parse(&["open"], none).is_err());
        assert!(parse(&["open", "a.rs", "b.rs"], none).is_err());
        assert_eq!(
            parse(&["diff", "src/invoice.rs", "/tmp/invoice_old.rs"], none),
            run(
                Request::Diff {
                    workspace: None,
                    old: PathBuf::from("/cwd/src/invoice.rs"),
                    new: PathBuf::from("/tmp/invoice_old.rs"),
                },
                false
            )
        );
        assert!(parse(&["diff", "a.rs"], none).is_err());
        assert_eq!(parse(&["pane", "--help"], none), Ok(Cli::Help));
        for command in [
            "pane list",
            "pane split",
            "pane focus",
            "pane close",
            "pane resize",
            "open",
            "diff",
            "page get",
            "page reload",
            "page back",
            "page forward",
            "page close",
        ] {
            assert!(HELP.contains(command), "{command}");
        }
    }

    #[test]
    fn pane_resize() {
        let none: &[(&str, &str)] = &[];
        assert_eq!(
            parse(&["pane", "resize", "5", "--size", "40%"], none),
            run(Request::PaneResize { workspace: None, pane: 5, size: 40 }, false)
        );
        assert_eq!(
            parse(&["pane", "resize", "5", "--size", "40%", "--workspace", "billing"], none),
            run(Request::PaneResize { workspace: Some("billing".to_owned()), pane: 5, size: 40 }, false)
        );
        assert!(parse(&["pane", "resize", "5"], none).is_err());
        assert!(parse(&["pane", "resize", "5", "--size", "100%"], none).is_err());
        assert!(parse(&["pane", "resize", "5", "--size", "0%"], none).is_err());
        assert!(parse(&["pane", "resize", "5", "--size", "40"], none).is_err());
        assert!(parse(&["pane", "resize", "5", "--size", "40%", "--right"], none).is_err());
        let line = Request::PaneResize { workspace: Some("1".to_owned()), pane: 5, size: 40 }.to_line(1);
        assert_eq!(
            parse_request_line(&line),
            Ok(Envelope {
                id: json!(1),
                request: Request::PaneResize { workspace: Some("1".to_owned()), pane: 5, size: 40 },
            })
        );
        let bad_size = r#"{"v":1,"id":1,"cmd":"pane.resize","args":{"pane":5,"size":100}}"#;
        assert!(parse_request_line(bad_size).is_err());
        assert_eq!(
            render(&Request::PaneResize { workspace: None, pane: 5, size: 40 }, false, &json!({})),
            (String::new(), EXIT_OK)
        );
    }

    #[test]
    fn open_a_url_as_a_page() {
        let none: &[(&str, &str)] = &[];
        assert_eq!(
            parse(&["open", "http://127.0.0.1:8000/tracker/PAY-1234"], none),
            run(
                Request::OpenPage {
                    workspace: None,
                    url: "http://127.0.0.1:8000/tracker/PAY-1234".to_owned(),
                },
                false
            )
        );
        assert!(parse(&["open", "http://127.0.0.1:8000/tracker/PAY-1234", "--pane", "7"], none).is_err());
        let line = Request::OpenPage { workspace: None, url: "http://127.0.0.1:8000/x".to_owned() }.to_line(1);
        assert_eq!(
            parse_request_line(&line),
            Ok(Envelope {
                id: json!(1),
                request: Request::OpenPage { workspace: None, url: "http://127.0.0.1:8000/x".to_owned() },
            })
        );
        assert_eq!(
            render(
                &Request::OpenPage { workspace: None, url: "http://127.0.0.1:8000/x".to_owned() },
                false,
                &json!({"id": "page-3"}),
            ),
            ("page-3\n".to_owned(), EXIT_OK)
        );
    }

    #[test]
    fn diff_of_two_revisions() {
        let none: &[(&str, &str)] = &[];
        assert_eq!(
            parse(&["diff", "HEAD~1", "HEAD"], none),
            run(Request::DiffRevisions { workspace: None, old: "HEAD~1".to_owned(), new: "HEAD".to_owned() }, false)
        );
        let line =
            Request::DiffRevisions { workspace: Some("1".to_owned()), old: "HEAD~1".to_owned(), new: "HEAD".to_owned() }
                .to_line(1);
        assert_eq!(
            parse_request_line(&line),
            Ok(Envelope {
                id: json!(1),
                request: Request::DiffRevisions {
                    workspace: Some("1".to_owned()),
                    old: "HEAD~1".to_owned(),
                    new: "HEAD".to_owned(),
                },
            })
        );
        let mixed = r#"{"v":1,"id":1,"cmd":"diff","args":{"old":"HEAD~1","new":"/a.rs"}}"#;
        assert!(parse_request_line(mixed).is_err());
        assert_eq!(
            render(
                &Request::DiffRevisions { workspace: None, old: "HEAD~1".to_owned(), new: "HEAD".to_owned() },
                false,
                &json!({}),
            ),
            (String::new(), EXIT_OK)
        );
    }

    #[test]
    fn page_commands() {
        let none: &[(&str, &str)] = &[];
        assert_eq!(
            parse(&["page", "get", "page-3"], none),
            run(Request::PageGet { page: "page-3".to_owned() }, false)
        );
        assert_eq!(
            parse(&["page", "get", "page-3", "--json"], none),
            run(Request::PageGet { page: "page-3".to_owned() }, true)
        );
        assert_eq!(
            parse(&["page", "reload", "page-3"], none),
            run(Request::PageReload { page: "page-3".to_owned() }, false)
        );
        assert_eq!(
            parse(&["page", "back", "page-3"], none),
            run(Request::PageBack { page: "page-3".to_owned() }, false)
        );
        assert_eq!(
            parse(&["page", "forward", "page-3"], none),
            run(Request::PageForward { page: "page-3".to_owned() }, false)
        );
        assert_eq!(
            parse(&["page", "close", "page-3"], none),
            run(Request::PageClose { page: "page-3".to_owned() }, false)
        );
        assert!(parse(&["page", "get"], none).is_err());
        assert!(parse(&["page", "get", "page-3", "--workspace", "1"], none).is_err());
        assert!(parse(&["page", "explode", "page-3"], none).is_err());
        assert_eq!(parse(&["page", "--help"], none), Ok(Cli::Help));

        for request in [
            Request::PageGet { page: "page-3".to_owned() },
            Request::PageReload { page: "page-3".to_owned() },
            Request::PageBack { page: "page-3".to_owned() },
            Request::PageForward { page: "page-3".to_owned() },
            Request::PageClose { page: "page-3".to_owned() },
        ] {
            let line = request.to_line(1);
            assert_eq!(parse_request_line(&line), Ok(Envelope { id: json!(1), request }), "{line}");
        }
        assert!(parse_request_line(r#"{"v":1,"id":1,"cmd":"page.get","args":{}}"#).is_err());

        assert_eq!(
            render(&Request::PageGet { page: "page-3".to_owned() }, false, &json!({"url": "http://127.0.0.1:8000/x", "title": "PAY-1234 Fix invoice rounding", "state": "loaded"})),
            ("http://127.0.0.1:8000/x\tPAY-1234 Fix invoice rounding\tloaded\n".to_owned(), EXIT_OK)
        );
        assert_eq!(
            render(&Request::PageGet { page: "page-3".to_owned() }, true, &json!({"url": "u", "title": null, "state": "loading"})).0,
            format!("{}\n", json!({"url": "u", "title": null, "state": "loading"}))
        );
        assert_eq!(
            render(&Request::PageClose { page: "page-3".to_owned() }, false, &json!({})),
            (String::new(), EXIT_OK)
        );
    }

    #[test]
    fn size_percentages() {
        assert_eq!(parse_size_percent("40%"), Ok(40));
        assert_eq!(parse_size_percent("1%"), Ok(1));
        assert_eq!(parse_size_percent("99%"), Ok(99));
        assert!(parse_size_percent("0%").is_err());
        assert!(parse_size_percent("100%").is_err());
        assert!(parse_size_percent("40").is_err());
        assert!(parse_size_percent("-5%").is_err());
    }

    #[test]
    fn pane_requests_round_trip_and_render() {
        let requests = [
            Request::PaneList { workspace: None },
            Request::PaneSplit { workspace: Some("1".to_owned()), pane: 5, direction: "right".to_owned() },
            Request::PaneFocus { workspace: None, pane: 5 },
            Request::PaneClose { workspace: None, pane: 5 },
            Request::Open {
                workspace: None,
                path: PathBuf::from("/w/src/invoice.rs"),
                line: Some(18),
                column: None,
                pane: None,
            },
            Request::Diff { workspace: None, old: PathBuf::from("/a.rs"), new: PathBuf::from("/b.rs") },
        ];
        for (index, request) in requests.into_iter().enumerate() {
            let line = request.to_line(index as u64);
            assert_eq!(parse_request_line(&line), Ok(Envelope { id: json!(index), request }), "{line}");
        }
        let bad = r#"{"v":1,"id":1,"cmd":"pane.split","args":{"pane":5,"direction":"up"}}"#;
        assert!(parse_request_line(bad).is_err());
        let relative = r#"{"v":1,"id":1,"cmd":"open","args":{"path":"src/a.rs"}}"#;
        assert!(parse_request_line(relative).is_err());

        let list = Request::PaneList { workspace: None };
        let panes = json!([
            {"id": 5, "active": true, "tabs": ["invoice.rs", "README.md"]},
            {"id": 9, "active": false, "tabs": []},
        ]);
        assert_eq!(
            render(&list, false, &panes),
            ("5\tactive\tinvoice.rs\tREADME.md\n9\t-\t\n".to_owned(), EXIT_OK)
        );
        let split = Request::PaneSplit { workspace: None, pane: 5, direction: "down".to_owned() };
        assert_eq!(render(&split, false, &json!({"id": 9})), ("9\n".to_owned(), EXIT_OK));
        let close = Request::PaneClose { workspace: None, pane: 5 };
        assert_eq!(render(&close, false, &json!({"closed": false})), (String::new(), EXIT_KEPT));
    }
}
