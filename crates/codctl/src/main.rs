//! The `codctl` binary: parses the command line, sends one request to the running Cod and prints the reply (see the library for the protocol).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = |name: &str| std::env::var(name).ok();
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
    let code = match codctl::parse_cli(&args, &env, &cwd) {
        Ok(codctl::Cli::Help) => {
            print!("{}", codctl::HELP);
            codctl::EXIT_OK
        }
        Ok(codctl::Cli::Run { request, json }) => match send(&request) {
            Ok(Ok(result)) => {
                let (out, code) = codctl::render(&request, json, &result);
                print!("{out}");
                code
            }
            Ok(Err(error)) => {
                eprintln!("codctl: {error}");
                codctl::EXIT_ERROR
            }
            Err(error) => {
                eprintln!("{error}");
                codctl::EXIT_ERROR
            }
        },
        Err(error) => {
            eprintln!("codctl: {}", codctl::one_line(&error));
            codctl::EXIT_ERROR
        }
    };
    ExitCode::from(code as u8)
}

/// Stats the socket path's folder and the socket file itself, so `send` can refuse to use a
/// socket planted by another user before it ever connects to it (#A3). `Ok(None)` means neither
/// exists yet: nothing to refuse, just "Cod is not running".
#[cfg(unix)]
fn stat_socket(path: &std::path::Path) -> std::io::Result<Option<codctl::SocketFacts>> {
    use std::fs::symlink_metadata;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let folder = match path.parent() {
        Some(folder) if !folder.as_os_str().is_empty() => folder,
        _ => std::path::Path::new("."),
    };
    let dir_metadata = match symlink_metadata(folder) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let socket_metadata = match symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(codctl::SocketFacts {
        dir_is_symlink: dir_metadata.file_type().is_symlink(),
        dir_owner: dir_metadata.uid(),
        dir_mode: dir_metadata.mode(),
        socket_is_symlink: socket_metadata.file_type().is_symlink(),
        socket_is_socket: socket_metadata.file_type().is_socket(),
        socket_owner: socket_metadata.uid(),
        socket_mode: socket_metadata.mode(),
    }))
}

/// Sends the request and reads the reply. The outer error is already worded for stderr: "Cod is not running", or what went wrong with the socket.
#[cfg(unix)]
fn send(request: &codctl::Request) -> Result<Result<serde_json::Value, String>, String> {
    use std::io::{BufRead as _, BufReader, ErrorKind, Write as _};
    use std::os::unix::net::UnixStream;

    let path = codctl::default_socket_path();
    // #A3: verify the socket is this user's own, privately held, before connecting at all. A
    // socket planted by another user (for example under a shared `/tmp` fallback when
    // `$XDG_RUNTIME_DIR` is unset) must never receive the request's session text and file paths.
    match stat_socket(&path) {
        Ok(Some(facts)) => {
            // SAFETY: geteuid has no preconditions and cannot fail.
            let user = unsafe { libc::geteuid() };
            if let Some(problem) = codctl::socket_problem(facts, user) {
                return Err(format!(
                    "codctl: refusing the socket at {}: {problem}",
                    path.display()
                ));
            }
        }
        Ok(None) => return Err(codctl::NOT_RUNNING.to_owned()),
        Err(error) => {
            return Err(format!(
                "codctl: cannot inspect the socket at {}: {error}",
                path.display()
            ));
        }
    }
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) if error.kind() == ErrorKind::NotFound || error.kind() == ErrorKind::ConnectionRefused => {
            return Err(codctl::NOT_RUNNING.to_owned());
        }
        Err(error) => {
            return Err(format!(
                "codctl: cannot connect to Cod at {}: {error}",
                path.display()
            ));
        }
    };
    let line = request.to_line(1);
    stream
        .write_all(format!("{line}\n").as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|error| format!("codctl: cannot send to Cod: {error}"))?;
    let mut reply = String::new();
    let read = BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|error| format!("codctl: cannot read the reply of Cod: {error}"))?;
    if read == 0 {
        return Err("codctl: Cod closed the connection without a reply".to_owned());
    }
    codctl::parse_reply_line(reply.trim_end()).map_err(|error| format!("codctl: {error}"))
}

#[cfg(not(unix))]
fn send(_request: &codctl::Request) -> Result<Result<serde_json::Value, String>, String> {
    Err("codctl: this platform has no Unix sockets".to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::stat_socket;
    use std::os::unix::fs::PermissionsExt as _;

    /// #A3: a real folder, world-writable (as a shared `/tmp` fallback would be if its own
    /// `cod-<user>` folder were planted by someone else), with a plain file standing in for the
    /// socket. `stat_socket` must report its real mode and that it is not a socket, so
    /// `codctl::socket_problem` can refuse both.
    #[test]
    fn reports_a_permissive_folder_and_a_non_socket_file_for_refusal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let socket_path = dir.path().join("codctl.sock");
        std::fs::write(&socket_path, b"not a socket").unwrap();

        let facts = stat_socket(&socket_path).unwrap().expect("the path exists");
        assert_eq!(facts.dir_mode & 0o777, 0o777);
        assert!(!facts.socket_is_socket);
        let user = unsafe { libc::geteuid() };
        assert!(codctl::socket_problem(facts, user).is_some());
    }

    #[test]
    fn reports_none_when_nothing_is_there_yet() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing").join("codctl.sock");
        assert!(stat_socket(&missing).unwrap().is_none());
    }
}
