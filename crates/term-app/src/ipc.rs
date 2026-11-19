//! New-window requests between Trident processes on Linux.
//!
//! The first process binds a unix socket and exports its path to child shells as
//! `TRIDENT_SOCKET`. A later `trident` run that sees the variable sends a request —
//! its working directory and `-e` command — and exits, so every window shares the
//! first process.

use std::ffi::OsString;
use std::io::Read;
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
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
/// the `-e` command words. Paths and command words keep their raw bytes — a
/// non-UTF-8 directory or argument survives the hop unchanged.
pub type Request = (Option<PathBuf>, Vec<(String, String)>, Vec<OsString>);

/// Marks the end of a request on the wire: a field that cannot occur inside
/// one. Anything shorter than a full request (a writer cut mid-send or the
/// read cap) lacks it and is dropped instead of opening a wrong window.
const REQUEST_END: &[u8] = b"\x01\x02trident-eor";

/// One request, read to the peer's close: `cwd`, then `K=V` environment pairs,
/// each NUL-terminated; an empty field ends the environment and the command
/// words follow, NUL-terminated, then `REQUEST_END`. Returns `None` for a
/// buffer without the terminator — truncated requests are not requests.
pub fn read_request(stream: &mut UnixStream) -> Option<Request> {
    let mut buf = Vec::new();
    stream.take(1 << 16).read_to_end(&mut buf).ok()?;
    let body = buf.strip_suffix(REQUEST_END)?;
    let mut fields = body.split(|byte| *byte == 0);
    let cwd = fields
        .next()
        .filter(|field| !field.is_empty())
        .map(|field| PathBuf::from(OsString::from_vec(field.to_vec())));
    let mut env = Vec::new();
    for field in fields.by_ref() {
        if field.is_empty() {
            break;
        }
        // The writer only sends UTF-8 pairs; anything else is corrupt.
        let text = String::from_utf8(field.to_vec()).ok()?;
        if let Some((key, value)) = text.split_once('=') {
            env.push((key.to_string(), value.to_string()));
        }
    }
    let command = fields
        .filter(|field| !field.is_empty())
        .map(|field| OsString::from_vec(field.to_vec()))
        .collect();
    Some((cwd, env, command))
}

/// Sends the new-window request to `path` — `cwd`, each `env` pair, an empty
/// separator, each `command` word, then `REQUEST_END` — and reports whether
/// the socket took all of it. The peer must be this user too: a stale path in
/// a shared directory could otherwise feed the environment to a replacement
/// listener.
pub fn forward(path: &Path, cwd: &Path, env: &[(String, String)], command: &[OsString]) -> bool {
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
        buf.extend_from_slice(arg.as_encoded_bytes());
        buf.push(0);
    }
    buf.extend_from_slice(REQUEST_END);
    stream.write_all(&buf).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn request_stream(bytes: &[u8]) -> UnixStream {
        let (mut writer, reader) = UnixStream::pair().unwrap();
        writer.write_all(bytes).unwrap();
        drop(writer);
        reader
    }

    #[test]
    fn read_request_rejects_truncated_requests() {
        // No terminator at all, cut between fields.
        let mut stream = request_stream(b"/tmp\x00\x00ls\x00");
        assert!(read_request(&mut stream).is_none());
        // Cut mid-field: the writer died inside a command word.
        let mut stream = request_stream(b"/tmp\x00\x00ls\x00tr");
        assert!(read_request(&mut stream).is_none());
        // Cut inside the environment section.
        let mut stream = request_stream(b"/tmp\x00A=B\x00C=");
        assert!(read_request(&mut stream).is_none());
        // Terminator alone is complete: no cwd, no env, no command.
        let mut stream = request_stream(REQUEST_END);
        assert_eq!(read_request(&mut stream), Some((None, vec![], vec![])));
    }

    #[test]
    fn read_request_keeps_non_utf8_bytes() {
        // The same layout `forward` writes, with non-UTF-8 bytes in the cwd
        // and one command word.
        let mut buf = b"/tmp/\xff\xfe".to_vec();
        buf.extend_from_slice(b"\x00K=V\x00\x00echo\x00\xffarg\x00");
        buf.extend_from_slice(REQUEST_END);
        let mut stream = request_stream(&buf);
        let (cwd, env, command) = read_request(&mut stream).unwrap();
        assert_eq!(
            cwd.as_deref().map(|path| path.as_os_str().as_bytes()),
            Some(&b"/tmp/\xff\xfe"[..])
        );
        assert_eq!(env, [("K".to_string(), "V".to_string())]);
        assert_eq!(
            command.iter().map(|arg| arg.as_bytes()).collect::<Vec<_>>(),
            [&b"echo"[..], &b"\xffarg"[..]]
        );
    }
}
