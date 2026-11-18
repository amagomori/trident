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

/// Whether the stream's peer belongs to this process's user — a `SO_PEERCRED`
/// read, so a request can't come from another user sharing the runtime dir.
pub fn same_user(stream: &UnixStream) -> bool {
    use std::os::unix::io::AsRawFd;
    // SAFETY: ucred is a plain C struct; all-zero is a valid value.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes at most `len` bytes of `ucred` into `cred`.
    let peer = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred) as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        ) == 0
    }
    .then_some(cred.uid);
    // SAFETY: getuid has no preconditions.
    peer == Some(unsafe { libc::getuid() })
}

/// A forwarded new-window request: working directory, environment pairs and
/// the `-e` command words.
pub type Request = (Option<PathBuf>, Vec<(String, String)>, Vec<String>);

/// One request, read to the peer's close: `cwd`, then `K=V` environment pairs,
/// each NUL-terminated; an empty field ends the environment and the command
/// words follow, NUL-terminated to the buffer's end.
pub fn read_request(stream: &mut UnixStream) -> Option<Request> {
    let mut buf = Vec::new();
    stream.take(1 << 16).read_to_end(&mut buf).ok()?;
    let mut fields = buf.split(|byte| *byte == 0);
    let cwd = fields
        .next()
        .filter(|field| !field.is_empty())
        .map(|field| PathBuf::from(String::from_utf8_lossy(field).into_owned()));
    let mut env = Vec::new();
    for field in fields.by_ref() {
        if field.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(field).into_owned();
        if let Some((key, value)) = text.split_once('=') {
            env.push((key.to_string(), value.to_string()));
        }
    }
    let mut command: Vec<String> = fields
        .map(|field| String::from_utf8_lossy(field).into_owned())
        .collect();
    if command.last().is_some_and(|field| field.is_empty()) {
        command.pop();
    }
    Some((cwd, env, command))
}

/// Sends the new-window request to `path` — `cwd`, each `env` pair, an empty
/// separator, then each `command` word — and reports whether the socket took
/// all of it. The peer must be this user too: a stale path in a shared
/// directory could otherwise feed the environment to a replacement listener.
pub fn forward(path: &Path, cwd: &Path, env: &[(String, String)], command: &[String]) -> bool {
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    if !same_user(&stream) {
        return false;
    }
    let mut buf = cwd.as_os_str().as_encoded_bytes().to_vec();
    buf.push(0);
    for (key, value) in env {
        buf.extend_from_slice(key.as_bytes());
        buf.push(b'=');
        buf.extend_from_slice(value.as_bytes());
        buf.push(0);
    }
    buf.push(0);
    for arg in command {
        buf.extend_from_slice(arg.as_bytes());
        buf.push(0);
    }
    stream.write_all(&buf).is_ok()
}
