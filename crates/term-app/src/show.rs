//! `terminal show`: prints files in the terminal. Images (detected from their bytes) are
//! scaled to the window and sent as kitty graphics escapes, so any terminal that speaks the
//! protocol shows them; text is printed as text and other files get a one-line summary.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::{ImageDecoder, ImageFormat};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Base64 bytes per kitty escape; the protocol allows at most 4096.
const CHUNK: usize = 4096;
/// Largest side sent, matching what the terminal accepts.
const MAX_SIDE: u32 = 10_000;
/// How long to wait for the terminal to answer a size query.
#[cfg(unix)]
const QUERY_TIMEOUT_MS: u64 = 300;

#[derive(Debug, Default, PartialEq)]
struct Options {
    width: Option<u32>,
    height: Option<u32>,
    fit: bool,
    files: Vec<PathBuf>,
}

/// The terminal's text area: cells, and the pixel size of one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Window {
    cols: u32,
    rows: u32,
    cell_w: f32,
    cell_h: f32,
    /// Running inside tmux, which only hands graphics to the terminal through passthrough.
    tmux: bool,
}

impl Default for Window {
    fn default() -> Window {
        Window {
            cols: 80,
            rows: 24,
            cell_w: 8.0,
            cell_h: 16.0,
            tmux: false,
        }
    }
}

/// What a file turned out to be.
#[derive(Debug, PartialEq)]
enum Kind {
    Raster(ImageFormat),
    Svg,
    Text,
    Binary(String),
}

/// A decoded image: RGBA8 with straight alpha.
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub fn usage() -> String {
    format!(
        "USAGE: {} show [--width N] [--height N] [--fit] FILE...\n\n\
         Shows files in the terminal. Images (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, QOI, PNM,\n\
         TGA and SVG, told apart by their contents) are drawn with the kitty graphics\n\
         protocol, shrunk to fit the window; text files are printed; anything else gets a\n\
         one-line summary.\n\n\
         OPTIONS:\n  \
           --width N    Draw images N columns wide\n  \
           --height N   Draw images N rows tall (with --width: fit inside both)\n  \
           --fit        Scale images up or down to fill the window\n  \
           -h, --help   Show this help\n\n\
         Inside tmux, images go through its passthrough; tmux 3.3 and later drop them\n\
         unless `set -g allow-passthrough on` is in its config.",
        env!("CARGO_BIN_NAME")
    )
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    let cells = |name: &str, value: Option<String>| {
        value
            .and_then(|v| v.parse().ok())
            .filter(|n: &u32| *n > 0)
            .ok_or(format!("{name} needs a positive number of cells"))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--width" => options.width = Some(cells("--width", args.next())?),
            "--height" => options.height = Some(cells("--height", args.next())?),
            "--fit" => options.fit = true,
            "-h" | "--help" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            "--" => options.files.extend(args.by_ref().map(PathBuf::from)),
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unknown option: {other} (see show --help)"));
            }
            _ => options.files.push(arg.into()),
        }
    }
    if options.files.is_empty() {
        return Err("show needs a file (see show --help)".into());
    }
    Ok(options)
}

/// Runs `terminal show` with the arguments after `show`; returns the exit code.
pub fn main(args: impl IntoIterator<Item = String>) -> i32 {
    let options = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{}: {e}", env!("CARGO_BIN_NAME"));
            return 2;
        }
    };
    let window = Window {
        tmux: std::env::var_os("TMUX").is_some(),
        ..window_size()
    };
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    let mut status = 0;
    for path in &options.files {
        if options.files.len() > 1 {
            let _ = writeln!(out, "{}", printable(&path.to_string_lossy()));
        }
        let result = show(path, &options, window, &mut out).and_then(|()| {
            out.flush()
                .map_err(|e| format!("cannot write output: {}", io_message(&e)))
        });
        if let Err(e) = result {
            let _ = out.flush();
            eprintln!(
                "{} show: {}: {}",
                env!("CARGO_BIN_NAME"),
                printable(&path.to_string_lossy()),
                printable(&e)
            );
            status = 1;
        }
    }
    status
}

fn show(
    path: &Path,
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| io_message(&e))?;
    let write_err = |e: io::Error| format!("cannot write output: {}", io_message(&e));
    match detect(&bytes) {
        Kind::Raster(format) => {
            let image = decode_raster(&bytes, format, options, window)?;
            write_kitty(out, &image, window).map_err(write_err)
        }
        Kind::Svg => {
            let image = render_svg(&bytes, path, options, window)?;
            write_kitty(out, &image, window).map_err(write_err)
        }
        Kind::Text => write_text(out, &bytes).map_err(write_err),
        Kind::Binary(kind) => {
            writeln!(out, "{kind}, {}", human_size(bytes.len() as u64)).map_err(write_err)
        }
    }
}

/// `io::Error`'s message without the " (os error N)" suffix.
fn io_message(e: &io::Error) -> String {
    let message = e.to_string();
    match message.find(" (os error") {
        Some(i) => message[..i].to_string(),
        None => message,
    }
}

fn detect(bytes: &[u8]) -> Kind {
    if let Ok(format) = image::guess_format(bytes) {
        // Some signatures ("BM", "P1".."P7", "qoif") also start ordinary text; text only
        // counts as an image when it has a whole header.
        let header_ok = || {
            image::ImageReader::with_format(io::Cursor::new(bytes), format)
                .into_dimensions()
                .is_ok()
        };
        if !is_text(bytes) || header_ok() {
            if format.reading_enabled() {
                return Kind::Raster(format);
            }
            let name = format
                .extensions_str()
                .first()
                .copied()
                .unwrap_or("unknown");
            return Kind::Binary(format!("{} image (not supported)", name.to_uppercase()));
        }
    }
    if is_svg(bytes) {
        return Kind::Svg;
    }
    if bytes.ends_with(b"TRUEVISION-XFILE.\0") {
        return Kind::Raster(ImageFormat::Tga);
    }
    if is_text(bytes) {
        return Kind::Text;
    }
    if looks_like_tga(bytes) {
        return Kind::Raster(ImageFormat::Tga);
    }
    Kind::Binary(binary_kind(bytes).into())
}

fn is_svg(bytes: &[u8]) -> bool {
    let head = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut head = Vec::new();
        let decoder = flate2::read::GzDecoder::new(bytes);
        let _ = io::Read::read_to_end(&mut io::Read::take(decoder, 4096), &mut head);
        head
    } else {
        bytes[..bytes.len().min(4096)].to_vec()
    };
    let text = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&head);
    let start = text
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(text.len());
    let text = &text[start..];
    let markup = [&b"<svg"[..], b"<?xml", b"<!DOCTYPE svg", b"<!--"]
        .iter()
        .any(|p| text.starts_with(p));
    markup && memchr_find(text, b"<svg")
}

fn memchr_find(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// UTF-8 without NUL bytes.
fn is_text(bytes: &[u8]) -> bool {
    !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
}

/// TGA files without the version 2 footer have no signature; check that the header holds
/// together.
fn looks_like_tga(b: &[u8]) -> bool {
    if b.len() < 18 {
        return false;
    }
    let color_map = b[1];
    let kind = b[2];
    let width = u16::from_le_bytes([b[12], b[13]]);
    let height = u16::from_le_bytes([b[14], b[15]]);
    let mapped = matches!(kind, 1 | 9);
    color_map <= 1
        && matches!(kind, 1 | 2 | 3 | 9 | 10 | 11)
        && (!mapped || color_map == 1)
        && matches!(b[16], 8 | 15 | 16 | 24 | 32)
        && width > 0
        && height > 0
        && b[17] & 0xc0 == 0
}

fn binary_kind(b: &[u8]) -> &'static str {
    let at = |offset: usize, magic: &[u8]| b.get(offset..offset + magic.len()) == Some(magic);
    match () {
        _ if at(0, b"\x7fELF") => "ELF executable",
        _ if at(0, &[0xcf, 0xfa, 0xed, 0xfe]) || at(0, &[0xce, 0xfa, 0xed, 0xfe]) => {
            "Mach-O executable"
        }
        _ if at(0, &[0xca, 0xfe, 0xba, 0xbe]) => "Mach-O universal binary or Java class",
        _ if at(0, b"%PDF-") => "PDF document",
        _ if at(0, b"PK\x03\x04") => "Zip archive",
        _ if at(0, &[0x1f, 0x8b]) => "gzip compressed data",
        _ if at(0, b"BZh") => "bzip2 compressed data",
        _ if at(0, b"\xfd7zXZ\0") => "xz compressed data",
        _ if at(0, &[0x28, 0xb5, 0x2f, 0xfd]) => "Zstandard compressed data",
        _ if at(0, b"7z\xbc\xaf\x27\x1c") => "7-Zip archive",
        _ if at(257, b"ustar") => "tar archive",
        _ if at(0, b"\0asm") => "WebAssembly module",
        _ if at(0, b"SQLite format 3\0") => "SQLite database",
        _ if at(4, b"ftyp") => "MP4/QuickTime media",
        _ if at(0, b"OggS") => "Ogg media",
        _ if at(0, b"ID3") => "MP3 audio",
        _ if at(0, b"fLaC") => "FLAC audio",
        _ if at(0, b"RIFF") && at(8, b"WAVE") => "WAVE audio",
        _ if at(0, b"RIFF") && at(8, b"AVI ") => "AVI video",
        _ if at(0, &[0x1a, 0x45, 0xdf, 0xa3]) => "Matroska/WebM media",
        _ => "binary data",
    }
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// Pixel size to draw an image of `src` pixels at.
fn target_size(src: (f32, f32), options: &Options, window: Window) -> (u32, u32) {
    let (w, h) = (src.0.max(1.0), src.1.max(1.0));
    let box_w = options.width.map(|c| c as f32 * window.cell_w);
    let box_h = options.height.map(|r| r as f32 * window.cell_h);
    let scale = match (box_w, box_h) {
        (Some(bw), Some(bh)) => (bw / w).min(bh / h),
        (Some(bw), None) => bw / w,
        (None, Some(bh)) => bh / h,
        (None, None) => {
            // Leave a row for the prompt.
            let win_w = window.cols as f32 * window.cell_w;
            let win_h = window.rows.saturating_sub(1).max(1) as f32 * window.cell_h;
            let fill = (win_w / w).min(win_h / h);
            if options.fit { fill } else { fill.min(1.0) }
        }
    };
    let scale = scale.min(MAX_SIDE as f32 / w).min(MAX_SIDE as f32 / h);
    let side = |v: f32| ((v * scale).round() as u32).clamp(1, MAX_SIDE);
    (side(w), side(h))
}

fn format_name(format: ImageFormat) -> String {
    let name = format.extensions_str().first().copied().unwrap_or("image");
    match format {
        ImageFormat::Jpeg => "JPEG".into(),
        ImageFormat::Tiff => "TIFF".into(),
        ImageFormat::Pnm => "PNM".into(),
        _ => name.to_uppercase(),
    }
}

fn decode_raster(
    bytes: &[u8],
    format: ImageFormat,
    options: &Options,
    window: Window,
) -> Result<Rgba, String> {
    let decode = || {
        let mut decoder =
            image::ImageReader::with_format(io::Cursor::new(bytes), format).into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        Ok::<_, image::ImageError>(image)
    };
    let image =
        decode().map_err(|e| format!("cannot decode {} image: {e}", format_name(format)))?;
    let (w, h) = target_size(
        (image.width() as f32, image.height() as f32),
        options,
        window,
    );
    let image = if (w, h) == (image.width(), image.height()) {
        image.into_rgba8()
    } else {
        image::imageops::resize(
            &image.into_rgba8(),
            w,
            h,
            image::imageops::FilterType::Triangle,
        )
    };
    Ok(Rgba {
        width: w,
        height: h,
        pixels: image.into_raw(),
    })
}

fn render_svg(
    bytes: &[u8],
    path: &Path,
    options: &Options,
    window: Window,
) -> Result<Rgba, String> {
    use resvg::{tiny_skia, usvg};
    let mut opt = usvg::Options {
        resources_dir: path.parent().map(Path::to_path_buf),
        ..Default::default()
    };
    // Scanning system fonts takes a while; only text needs them.
    if bytes.starts_with(&[0x1f, 0x8b]) || memchr_find(bytes, b"<text") {
        load_fonts(opt.fontdb_mut());
    }
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| format!("cannot read SVG: {e}"))?;
    let size = tree.size();
    let (w, h) = target_size((size.width(), size.height()), options, window);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("cannot allocate the image")?;
    let transform =
        tiny_skia::Transform::from_scale(w as f32 / size.width(), h as f32 / size.height());
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(Rgba {
        width: w,
        height: h,
        pixels: pixmap.take_demultiplied(),
    })
}

fn load_fonts(db: &mut resvg::usvg::fontdb::Database) {
    db.load_system_fonts();
    db.load_font_data(term_render::BUNDLED_FONT.to_vec());
    let has = |family: &str| {
        db.faces()
            .any(|f| f.families.iter().any(|(name, _)| name == family))
    };
    let pick = |candidates: &[&str]| {
        candidates
            .iter()
            .find(|c| has(c))
            .map_or(term_render::BUNDLED_FONT_FAMILY, |c| *c)
            .to_string()
    };
    let sans = pick(&[
        "Helvetica",
        "Arial",
        "DejaVu Sans",
        "Noto Sans",
        "Liberation Sans",
    ]);
    let serif = pick(&[
        "Times New Roman",
        "Times",
        "DejaVu Serif",
        "Noto Serif",
        "Liberation Serif",
    ]);
    db.set_sans_serif_family(sans);
    db.set_serif_family(serif);
    db.set_monospace_family(term_render::BUNDLED_FONT_FAMILY);
}

/// Sends `image` as one kitty `a=T` command (zlib-compressed RGBA, split into chunks) and
/// moves to the line below it.
///
/// Inside tmux each escape goes through its passthrough. tmux keeps its own cursor, which
/// wouldn't follow the terminal's past the image, so the image leaves the cursor alone
/// (`C=1`) and newlines step over its rows instead. tmux also homes the terminal's cursor
/// after every passthrough, and the image is placed when its last chunk arrives, so the
/// first chunk saves the cursor (`ESC 7`) and the last restores it (`ESC 8`).
fn write_kitty(out: &mut impl Write, image: &Rgba, window: Window) -> io::Result<()> {
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
    zlib.write_all(&image.pixels)?;
    let payload = BASE64.encode(zlib.finish()?);
    let chunks: Vec<&[u8]> = payload.as_bytes().chunks(CHUNK).collect();
    let keep_cursor = if window.tmux { ",C=1" } else { "" };
    let mut escape = Vec::with_capacity(CHUNK + 64);
    for (i, chunk) in chunks.iter().enumerate() {
        let more = u8::from(i + 1 < chunks.len());
        escape.clear();
        if window.tmux && chunks.len() > 1 {
            if i == 0 {
                escape.extend_from_slice(b"\x1b7");
            } else if more == 0 {
                escape.extend_from_slice(b"\x1b8");
            }
        }
        if i == 0 {
            write!(
                escape,
                "\x1b_Ga=T,f=32,o=z,q=2{keep_cursor},s={},v={},m={more};",
                image.width, image.height
            )?;
        } else {
            write!(escape, "\x1b_Gm={more};")?;
        }
        escape.extend_from_slice(chunk);
        escape.extend_from_slice(b"\x1b\\");
        if window.tmux {
            out.write_all(&tmux_passthrough(&escape))?;
        } else {
            out.write_all(&escape)?;
        }
    }
    if window.tmux {
        let rows = (image.height as f32 / window.cell_h).ceil().max(1.0) as usize;
        out.write_all(&b"\n".repeat(rows))
    } else {
        out.write_all(b"\n")
    }
}

/// `escape` wrapped in tmux's DCS passthrough (`ESC P tmux; ... ESC \`, every ESC inside
/// doubled), which tmux forwards to the terminal it runs in. tmux 3.3 and later only do so
/// with `allow-passthrough` on, and drop it silently otherwise.
fn tmux_passthrough(escape: &[u8]) -> Vec<u8> {
    let mut wrapped = Vec::with_capacity(escape.len() + 16);
    wrapped.extend_from_slice(b"\x1bPtmux;");
    for &b in escape {
        if b == 0x1b {
            wrapped.push(b);
        }
        wrapped.push(b);
    }
    wrapped.extend_from_slice(b"\x1b\\");
    wrapped
}

/// Prints text with control characters made visible (`^[` for ESC), so a file can't
/// drive the terminal.
fn write_text(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    // CR LF and lone CR (old Mac files, progress logs) both end a line.
    let text = String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut clean = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let (line, newline) = line.strip_suffix('\n').map_or((line, ""), |l| (l, "\n"));
        push_printable(&mut clean, line, true);
        clean.push_str(newline);
    }
    if !clean.is_empty() && !clean.ends_with('\n') {
        clean.push('\n');
    }
    out.write_all(clean.as_bytes())
}

/// `text` with control characters shown as `^X`, safe to print on a terminal.
fn printable(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    push_printable(&mut clean, text, false);
    clean
}

fn push_printable(clean: &mut String, text: &str, keep_tabs: bool) {
    for c in text.chars() {
        match c {
            '\t' if keep_tabs => clean.push(c),
            '\x7f' => clean.push_str("^?"),
            c if (c as u32) < 0x20 => {
                clean.push('^');
                clean.push(char::from(c as u8 + 0x40));
            }
            '\u{80}'..='\u{9f}' => clean.push('\u{fffd}'),
            c => clean.push(c),
        }
    }
}

fn window_size() -> Window {
    #[cfg(unix)]
    if let Some(window) = unix::window_size() {
        return window;
    }
    Window::default()
}

/// Parses the reply to `CSI 14 t`, `CSI 4 ; height ; width t`, into (width, height).
fn parse_pixel_report(reply: &[u8]) -> Option<(u32, u32)> {
    let start = reply.windows(4).position(|w| w == b"\x1b[4;")? + 4;
    let rest = &reply[start..];
    let end = rest.iter().position(|&b| b == b't')?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let (h, w) = body.split_once(';')?;
    let (w, h) = (w.parse().ok()?, h.parse().ok()?);
    (w > 0 && h > 0).then_some((w, h))
}

#[cfg(unix)]
mod unix {
    use super::{QUERY_TIMEOUT_MS, Window, parse_pixel_report};
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::time::Instant;

    /// Cells from `TIOCGWINSZ`; pixels from it too when the terminal fills them in, else
    /// from asking the terminal (`CSI 14 t`).
    pub(super) fn window_size() -> Option<Window> {
        let tty = File::options().read(true).write(true).open("/dev/tty").ok();
        let fds = [libc::STDOUT_FILENO, libc::STDIN_FILENO, libc::STDERR_FILENO];
        let ws = fds
            .into_iter()
            .chain(tty.as_ref().map(|t| t.as_raw_fd()))
            .find_map(winsize)?;
        let (cols, rows) = (u32::from(ws.ws_col), u32::from(ws.ws_row));
        let pixels = if ws.ws_xpixel > 0 && ws.ws_ypixel > 0 {
            Some((u32::from(ws.ws_xpixel), u32::from(ws.ws_ypixel)))
        } else {
            tty.and_then(query_pixels)
        };
        let default = Window::default();
        let (cell_w, cell_h) = pixels.map_or((default.cell_w, default.cell_h), |(w, h)| {
            (w as f32 / cols as f32, h as f32 / rows as f32)
        });
        Some(Window {
            cols,
            rows,
            cell_w,
            cell_h,
            tmux: false,
        })
    }

    fn winsize(fd: i32) -> Option<libc::winsize> {
        // SAFETY: TIOCGWINSZ writes a winsize into the pointed-to struct.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0;
        (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some(ws)
    }

    /// Asks for the text area size, followed by a device attributes query that every
    /// terminal answers, so one that ignores the first doesn't cost the whole timeout.
    fn query_pixels(mut tty: File) -> Option<(u32, u32)> {
        let fd = tty.as_raw_fd();
        // SAFETY: tcgetattr/tcsetattr read and write a termios struct for an open fd.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        // Timed reads (VMIN=0, VTIME in tenths of a second) rather than poll(): macOS's
        // poll() doesn't support ttys and reports POLLNVAL at once.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        let mut reply = Vec::new();
        if tty.write_all(b"\x1b[14t\x1b[c").is_ok() {
            let start = Instant::now();
            while start.elapsed().as_millis() < u128::from(QUERY_TIMEOUT_MS) {
                let mut buf = [0; 256];
                match tty.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => reply.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
                if da1_answered(&reply) {
                    break;
                }
            }
        }
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        parse_pixel_report(&reply)
    }

    fn da1_answered(reply: &[u8]) -> bool {
        reply
            .windows(3)
            .position(|w| w == b"\x1b[?")
            .is_some_and(|i| {
                reply[i + 3..]
                    .iter()
                    .find(|b| !b.is_ascii_digit() && **b != b';')
                    == Some(&b'c')
            })
    }
}

#[cfg(test)]
mod tests;
