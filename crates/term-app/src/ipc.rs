//! New-window requests between Trident processes on Linux.
//!
//! The first process binds a unix socket and exports its path to child shells as
//! `TRIDENT_SOCKET`. A later `trident` run that sees the variable sends a request —
//! its working directory and `-e` command — and exits, so every window shares the
//! first process.

use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;

/// The environment variable carrying the running process's socket path.
pub const SOCKET_ENV: &str = "TRIDENT_SOCKET";

/// Where this process listens: `trident-<pid>.sock` under the runtime dir.
pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(format!("trident-{}.sock", std::process::id()))
}

/// Binds `path` for listening, first removing a stale socket file it finds.
pub fn listen(path: &Path) -> Option<UnixListener> {
    if let Ok(listener) = UnixListener::bind(path) {
        return Some(listener);
    }
    // A stale socket file survives its process; drop it and bind again.
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).ok()
}

/// Reads one request — `cwd` then the command words, NUL-separated — to its end.
pub fn read_request(stream: &mut UnixStream) -> Option<(Option<PathBuf>, Vec<String>)> {
    let mut buf = Vec::new();
    stream.take(1 << 16).read_to_end(&mut buf).ok()?;
    let mut fields = buf
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let cwd = fields
        .next()
        .map(|field| PathBuf::from(String::from_utf8_lossy(field).into_owned()));
    let command = fields
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();
    Some((cwd, command))
}

/// Sends the new-window request to `path` — `cwd` then each `command` word,
/// NUL-separated — and reports whether the socket took all of it.
pub fn forward(path: &Path, cwd: &Path, command: &[String]) -> bool {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let mut buf = cwd.as_os_str().as_encoded_bytes().to_vec();
    for arg in command {
        buf.push(0);
        buf.extend_from_slice(arg.as_bytes());
    }
    buf.push(0);
    stream.write_all(&buf).is_ok()
}
