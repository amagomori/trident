//! Best-effort system thumbnails for explicit `show` requests. Helpers
//! write PNGs in a private directory; their stdout never reaches the PTY.

use super::{ImageFormat, Options, Rgba, Window, decode_raster};
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod freedesktop;

const TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SIDE: u32 = 2048;
const MAX_PNG_BYTES: u64 = 20 * 1024 * 1024;

pub(super) fn render(
    path: &Path,
    _bytes: &[u8],
    options: &Options,
    window: Window,
) -> Option<Rgba> {
    let path = std::path::absolute(path).ok()?;
    let dir = tempfile::Builder::new()
        .prefix("trident-thumbnail-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .ok()?;
    let width = options.width.unwrap_or(window.cols) as f32 * window.cell_w;
    let height = options.height.unwrap_or(window.rows) as f32 * window.cell_h;
    let side = (width.max(height).ceil() as u32).clamp(1, MAX_SIDE);
    let deadline = Instant::now() + TIMEOUT;
    #[cfg(target_os = "linux")]
    let candidates = freedesktop::commands(&path, _bytes, dir.path(), side);
    #[cfg(target_os = "macos")]
    let candidates = {
        let mut command = Command::new("/usr/bin/qlmanage");
        command
            .args(["-t", "-s", &side.to_string(), "-o"])
            .arg(dir.path())
            .arg(&path);
        let mut name = path.file_name()?.to_owned();
        name.push(".png");
        [(command, dir.path().join(name))]
    };
    preview(candidates, options, window, deadline)
}

fn preview(
    commands: impl IntoIterator<Item = (Command, PathBuf)>,
    options: &Options,
    window: Window,
    deadline: Instant,
) -> Option<Rgba> {
    for (mut command, output) in commands {
        if Instant::now() >= deadline {
            break;
        }
        if run(&mut command, deadline)
            && let Some(png) = read_png(&output)
            && let Ok(image) = decode_raster(&png, ImageFormat::Png, options, window)
        {
            return Some(image);
        }
    }
    None
}

fn read_png(path: &Path) -> Option<Vec<u8>> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_PNG_BYTES {
        return None;
    }
    let mut png = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_PNG_BYTES + 1)
        .read_to_end(&mut png)
        .ok()?;
    if png.len() as u64 > MAX_PNG_BYTES {
        return None;
    }
    let (w, h) = image::ImageReader::with_format(Cursor::new(&png), ImageFormat::Png)
        .into_dimensions()
        .ok()?;
    (w.max(h) <= MAX_SIDE).then_some(png)
}

fn run(command: &mut Command, deadline: Instant) -> bool {
    let Ok(mut child) = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                // SAFETY: The unreaped child leads the new process group. Kill
                // its workers too before dropping the temporary directory.
                unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_failed_and_invalid_outputs_then_uses_a_valid_png() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("invalid.png");
        fs::write(&bad, b"not a PNG").unwrap();
        let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/sample.png");
        let candidates = [
            (Command::new("false"), png.clone()),
            (Command::new("true"), bad),
            (Command::new("true"), png),
        ];
        let image = preview(
            candidates,
            &Options::default(),
            Window::default(),
            Instant::now() + TIMEOUT,
        )
        .unwrap();
        assert_eq!(
            image.pixels.len(),
            4 * (image.width * image.height) as usize
        );
    }

    #[test]
    fn timeout_does_not_wait_for_helper_completion() {
        let start = Instant::now();
        assert!(!run(
            Command::new("sleep").arg("10"),
            start + Duration::from_millis(50)
        ));
        assert!(start.elapsed() < Duration::from_secs(3));
    }
}
