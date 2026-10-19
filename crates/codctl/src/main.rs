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

/// Sends the request and reads the reply. The outer error is already worded for stderr: "Cod is not running", or what went wrong with the socket.
#[cfg(unix)]
fn send(request: &codctl::Request) -> Result<Result<serde_json::Value, String>, String> {
    use std::io::{BufRead as _, BufReader, ErrorKind, Write as _};
    use std::os::unix::net::UnixStream;

    let path = codctl::default_socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
        {
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
