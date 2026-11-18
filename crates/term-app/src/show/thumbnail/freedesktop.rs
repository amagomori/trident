//! Matches system-installed thumbnailer entries against the shared MIME
//! database. Parse the command before inserting filenames, never as shell code.

use std::ffi::OsString;
use std::fmt::Write;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use xdg_mime::SharedMimeInfo;

pub(super) fn commands(
    path: &Path,
    bytes: &[u8],
    temp: &Path,
    side: u32,
) -> Vec<(Command, PathBuf)> {
    installed(
        Path::new("/usr/share/thumbnailers"),
        path,
        bytes,
        temp,
        side,
    )
}

fn installed(
    entries: &Path,
    path: &Path,
    bytes: &[u8],
    temp: &Path,
    side: u32,
) -> Vec<(Command, PathBuf)> {
    let Ok(entries) = fs::read_dir(entries) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|s| s == "thumbnailer"))
        .collect();
    paths.sort();
    static DB: LazyLock<SharedMimeInfo> = LazyLock::new(SharedMimeInfo::new);
    let guess = DB
        .guess_mime_type()
        .file_name(&path.file_name().unwrap_or_default().to_string_lossy())
        .data(bytes)
        .guess();
    paths
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let text = fs::read_to_string(entry).ok()?;
            let entry = Entry::parse(&text)?;
            if !entry
                .types
                .split(';')
                .filter_map(|t| t.trim().parse().ok())
                .any(|t| DB.mime_type_equal(&t, guess.mime_type()))
                || !entry.try_exec.is_none_or(executable)
            {
                return None;
            }
            let output = temp.join(format!("{i}.png"));
            Some((command(entry.exec, path, &output, side)?, output))
        })
        .collect()
}

struct Entry<'a> {
    exec: &'a str,
    types: &'a str,
    try_exec: Option<&'a str>,
}

impl<'a> Entry<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let mut section = false;
        let (mut exec, mut types, mut try_exec) = (None, None, None);
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line == "[Thumbnailer Entry]";
            } else if section && let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "Exec" => exec = Some(value.trim()),
                    "TryExec" => try_exec = Some(value.trim()),
                    "MimeType" => types = Some(value.trim()),
                    _ => {}
                }
            }
        }
        Some(Self {
            exec: exec?,
            types: types?,
            try_exec,
        })
    }
}

fn executable(name: &str) -> bool {
    let is_executable = |path: &Path| {
        fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if name.contains('/') {
        return Path::new(name).is_absolute() && is_executable(Path::new(name));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .any(|dir| is_executable(&dir.join(name)))
}

fn command(exec: &str, input: &Path, output: &Path, side: u32) -> Option<Command> {
    let mut words = shlex::split(exec)?.into_iter();
    let mut cmd = Command::new(words.next()?);
    for word in words {
        let argument: OsString = match word.as_str() {
            "%i" => input.into(),
            "%o" => output.into(),
            "%u" => file_uri(input).into(),
            "%s" => side.to_string().into(),
            "%%" => "%".into(),
            other if !other.contains('%') => other.into(),
            _ => return None,
        };
        cmd.arg(argument);
    }
    Some(cmd)
}

fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(char::from(b));
        } else {
            write!(uri, "%{b:02X}").unwrap();
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_entry_expands_paths_without_resplitting() {
        let dir = tempfile::tempdir().unwrap();
        let descriptor = "[Thumbnailer Entry]\nTryExec=sh\nExec=helper 'one argument' %s %u %i %o %%\nMimeType=video/mp4;\n[Ignored]\nExec=ignored\n";
        fs::write(dir.path().join("valid.thumbnailer"), descriptor).unwrap();
        fs::write(
            dir.path().join("wrong.thumbnailer"),
            descriptor.replace("video/mp4", "image/png"),
        )
        .unwrap();
        fs::write(
            dir.path().join("absent.thumbnailer"),
            descriptor.replace("TryExec=sh", "TryExec=/does/not/exist"),
        )
        .unwrap();
        let path = Path::new("/tmp/50% 'café'.mp4");
        let commands = installed(dir.path(), path, b"\0\0\0\x18ftypmp42", dir.path(), 320);
        assert_eq!(commands.len(), 1);
        let (cmd, output) = &commands[0];
        assert_eq!(cmd.get_program(), "helper");
        assert_eq!(
            cmd.get_args().collect::<Vec<_>>(),
            [
                "one argument",
                "320",
                "file:///tmp/50%25%20%27caf%C3%A9%27.mp4",
                path.to_str().unwrap(),
                output.to_str().unwrap(),
                "%"
            ]
        );
    }
}
