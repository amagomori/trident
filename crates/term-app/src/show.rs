//! `trident show`: prints files in the terminal. Images (detected from their
//! bytes) and PDF pages are
//! scaled to the window and sent as kitty graphics escapes, so any terminal that speaks the
//! protocol shows them. Text is printed as text. Other files get a system
//! thumbnail when available, or a one-line summary.

mod thumbnail;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::{ImageDecoder, ImageFormat};
use std::io::{self, IsTerminal, Read, Write};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod pdf;
mod png;
mod stream;

/// Base64 bytes per kitty escape; the protocol allows at most 4096.
const CHUNK: usize = 4096;
const STDIN_LIMIT_MIB: u64 = 64;
/// Largest side sent, matching what the terminal accepts.
const MAX_SIDE: u32 = 10_000;
/// How long to wait for a tmux command; a tmux on PATH older than the server can hang.
const TMUX_TIMEOUT: Duration = Duration::from_secs(2);

/// Pause after each chunk inside tmux before 3.3: below the rate at which a slow terminal
/// reading 4 KiB every 5 ms fills tmux's buffer, so a 500 KiB image takes about 0.6 s.
const OLD_TMUX_CHUNK_PAUSE: Duration = Duration::from_millis(5);
/// How long to wait for the terminal to answer a size query.
#[cfg(unix)]
const QUERY_TIMEOUT_MS: u64 = 300;

#[derive(Debug, Default, PartialEq)]
struct Options {
    width: Option<u32>,
    height: Option<u32>,
    fit: bool,
    pages: Option<RangeInclusive<i32>>,
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
    Pdf,
    Text,
    DecodedText(String),
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
        "USAGE: {} show [--width N] [--height N] [--fit] [--page N[-M]] (FILE... | -)\n\n\
         Shows files in the terminal. Images (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, QOI, PNM,\n\
         TGA and SVG, told apart by their contents) are drawn with the kitty graphics\n\
         protocol, shrunk to fit the window; PDF pages fill its width. Text files are\n\
         printed. Other files get a system thumbnail or a one-line summary.\n\n\
         Use - alone to read a piped raster image (up to {STDIN_LIMIT_MIB} MiB).\n\
         Use ./- for a file named -.\n\n\
         OPTIONS:\n  \
           --width N    Draw images N columns wide\n  \
           --height N   Draw images N rows tall (with --width: fit inside both)\n  \
           --fit        Scale images up or down to fill the window\n  \
           --page N[-M] Show PDF page N or pages N through M (first page is 1; default: all)\n  \
           -h, --help   Show this help\n\n\
         Inside tmux, images go through its passthrough. tmux 3.3 and later drop that\n\
         unless allow-passthrough is on, so, like `kitten icat`, show turns it on for\n\
         the pane it runs in.",
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
            "--page" => options.pages = Some(pdf::page_range(&args.next().unwrap_or_default())?),
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
    if options.files.len() > 1 && options.files.iter().any(|p| p.as_os_str() == "-") {
        return Err("use - as the only operand to read an image from stdin".into());
    }
    Ok(options)
}

/// Runs `trident show` with the arguments after `show`; returns the exit code.
pub fn main(args: impl IntoIterator<Item = String>) -> i32 {
    let options = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            diag!("{}: {e}", env!("CARGO_BIN_NAME"));
            return 2;
        }
    };
    let from_stdin = options.files[0].as_os_str() == "-";
    if from_stdin && io::stdin().is_terminal() {
        diag!(
            "{} show: pipe a raster image to stdin",
            env!("CARGO_BIN_NAME")
        );
        return 1;
    }
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
        let result = if from_stdin {
            show_stdin(&options, window, &mut out)
        } else {
            show(path, &options, window, &mut out)
        }
        .and_then(|()| out.flush().map_err(write_err));
        if let Err(e) = result {
            let _ = out.flush();
            diag!(
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

fn show_stdin(options: &Options, window: Window, out: &mut impl Write) -> Result<(), String> {
    let mut bytes = Vec::new();
    let limit = STDIN_LIMIT_MIB * 1024 * 1024;
    io::stdin()
        .lock()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("cannot read stdin: {}", io_message(&e)))?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "stdin image exceeds the {STDIN_LIMIT_MIB} MiB limit"
        ));
    }
    if bytes.is_empty() {
        return Err("stdin is empty; expected a raster image".into());
    }
    let format = image::guess_format(&bytes)
        .ok()
        .filter(|format| format.reading_enabled())
        .or_else(|| is_tga(&bytes).then_some(ImageFormat::Tga))
        .ok_or("stdin is not a supported raster image")?;
    let bytes = stream::complete_image(&bytes, format)?;
    animation::validate_input(bytes, format)?;
    show_raster(bytes, format, options, window, out)
}

fn show(
    path: &Path,
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> Result<(), String> {
    let (bytes, size) = read_for_show(path).map_err(|e| io_message(&e))?;
    match detect(&bytes) {
        Kind::Raster(format) => show_raster(&bytes, format, options, window, out),
        Kind::Svg => {
            let image = render_svg(&bytes, path, options, window)?;
            write_kitty(out, &image, window, tmux_wait(window)).map_err(write_err)
        }
        Kind::Pdf => pdf::show(&bytes, options, window, out),
        Kind::Text => write_text(out, &String::from_utf8_lossy(&bytes)).map_err(write_err),
        Kind::DecodedText(text) => write_text(out, &text).map_err(write_err),
        Kind::Binary(kind) => {
            if let Some(image) = thumbnail::render(path, &bytes, options, window) {
                return write_kitty(out, &image, window, tmux_wait(window)).map_err(write_err);
            }
            writeln!(out, "{kind}, {}", human_size(size)).map_err(write_err)
        }
    }
}

fn show_raster(
    bytes: &[u8],
    format: ImageFormat,
    options: &Options,
    window: Window,
    out: &mut impl Write,
) -> Result<(), String> {
    if matches!(format, ImageFormat::Gif | ImageFormat::WebP) {
        return animation::show(bytes, format, options, window, out);
    }
    let image = read_raster(bytes, format)?;
    let size = raster_size(&image, options, window);
    if format == ImageFormat::Png && !window.tmux && png::can_forward(bytes, size) {
        send_chunks(out, bytes, "f=100,q=2", None, false, &mut || {})
            .and_then(|()| out.write_all(b"\n"))
            .map_err(write_err)
    } else {
        let image = sized_rgba(image, size);
        write_kitty(out, &image, window, tmux_wait(window)).map_err(write_err)
    }
}

fn write_err(e: io::Error) -> String {
    format!("cannot write output: {}", io_message(&e))
}

/// Large videos and Office archives only need a header for classification.
/// Their native thumbnailer reads the original path, not this buffer.
fn read_for_show(path: &Path) -> io::Result<(Vec<u8>, u64)> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let mut bytes = Vec::new();
    if size > 1024 * 1024 {
        (&mut file).take(64 * 1024).read_to_end(&mut bytes)?;
        if (bytes.starts_with(b"PK\x03\x04") || bytes.get(4..8) == Some(b"ftyp"))
            && matches!(detect(&bytes), Kind::Binary(_))
        {
            return Ok((bytes, size));
        }
    }
    file.read_to_end(&mut bytes)?;
    let size = bytes.len() as u64;
    Ok((bytes, size))
}

/// Small, image-only drop preview. This is deliberately separate from `show`: no text,
/// terminal queries, tmux commands, or errors are emitted for unsupported files.
pub(super) fn drop_preview(
    path: &Path,
    cols: usize,
    rows: usize,
    cell: (f32, f32),
) -> Option<(Vec<u8>, usize)> {
    const MAX_BYTES: u64 = 16 * 1024 * 1024;
    if rows < 2 || !path.metadata().ok()?.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let window = Window {
        cols: cols as u32,
        rows: rows as u32,
        cell_w: cell.0,
        cell_h: cell.1,
        tmux: false,
    };
    let options = Options {
        width: Some(cols.min(32) as u32),
        height: Some((rows - 1).min(6) as u32),
        ..Options::default()
    };
    let image = match detect(&bytes) {
        Kind::Raster(format) => decode_raster(&bytes, format, &options, window).ok()?,
        Kind::Svg => render_svg(&bytes, path, &options, window).ok()?,
        _ => return None,
    };
    let image_rows = (image.height as f32 / cell.1).ceil() as usize;
    let mut output = Vec::new();
    write_kitty(&mut output, &image, window, || {}).ok()?;
    Some((output, image_rows))
}

/// What a hover preview of a file shows.
pub(super) enum Preview {
    /// RGBA8 with straight alpha, at most the requested size; with a description such as
    /// "PNG 640×480".
    Image(u32, u32, Vec<u8>, String),
    Text(String),
    /// A description of contents there is no preview for.
    Binary(String),
}

/// Previews `bytes`, the start of the file at `path` (`complete` when they are all of it):
/// images scaled down to fit `max` pixels, or text.
pub(super) fn preview(bytes: &[u8], complete: bool, path: &Path, max: (u32, u32)) -> Preview {
    let window = Window {
        cols: 1,
        rows: 2,
        cell_w: max.0 as f32,
        cell_h: max.1 as f32,
        tmux: false,
    };
    let options = Options::default();
    let image = |rgba: Result<Rgba, String>, name: String| match rgba {
        Ok(i) => Preview::Image(i.width, i.height, i.pixels, name),
        Err(e) => Preview::Binary(e),
    };
    match detect(bytes) {
        Kind::Raster(format) if complete => {
            let name = match image::ImageReader::with_format(io::Cursor::new(bytes), format)
                .into_dimensions()
            {
                Ok((w, h)) => format!("{} {w}×{h}", format_name(format)),
                Err(_) => format_name(format),
            };
            image(decode_raster(bytes, format, &options, window), name)
        }
        Kind::Svg if complete => image(render_svg(bytes, path, &options, window), "SVG".into()),
        Kind::Raster(format) => Preview::Binary(format!("{} image", format_name(format))),
        Kind::Svg => Preview::Binary("SVG image".into()),
        Kind::Pdf => Preview::Binary("PDF document".into()),
        Kind::Text => Preview::Text(String::from_utf8_lossy(bytes).into_owned()),
        Kind::DecodedText(text) => Preview::Text(text),
        Kind::Binary(kind) => Preview::Binary(kind),
    }
}

pub(super) fn size_label(bytes: u64) -> String {
    human_size(bytes)
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
    // Even a PDF with entirely ASCII contents has a version header on its own line.
    if let Some([major, b'.', minor, b'\r' | b'\n', ..]) = bytes.strip_prefix(b"%PDF-")
        && major.is_ascii_digit()
        && minor.is_ascii_digit()
    {
        return Kind::Pdf;
    }
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
    let binary = binary_kind(bytes);
    if binary != "binary data" {
        return Kind::Binary(binary.into());
    }
    if let Some(text) = decode_text(bytes) {
        return Kind::DecodedText(text);
    }
    Kind::Binary(binary.into())
}

fn ascii_text(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | b'\r' | 0x20..=0x7e)
}

fn decode_text(bytes: &[u8]) -> Option<String> {
    let bom = match bytes.get(..2) {
        Some([0xff, 0xfe]) => Some(true),
        Some([0xfe, 0xff]) => Some(false),
        _ => None,
    };
    let endian = bom.or_else(|| {
        if bytes.len() < 8 || !bytes.len().is_multiple_of(2) {
            return None;
        }
        [true, false].into_iter().find(|&little| {
            let ascii = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|pair| {
                    let (low, high) = if little {
                        (pair[0], pair[1])
                    } else {
                        (pair[1], pair[0])
                    };
                    high == 0 && ascii_text(low)
                })
                .count();
            ascii * 4 >= bytes.len() / 2 * 3
        })
    });
    if let Some(little) = endian {
        let body = if bom.is_some() { &bytes[2..] } else { bytes };
        if !body.len().is_multiple_of(2) {
            return None;
        }
        let units: Vec<u16> = body
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| {
                let pair = [pair[0], pair[1]];
                if little {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        if bom.is_none()
            && units
                .iter()
                .any(|&u| u == 0 || (u < 0x20 && !ascii_text(u as u8)))
        {
            return None;
        }
        return Some(String::from_utf16_lossy(&units));
    }
    if bytes.is_empty() || bytes.contains(&0) || std::str::from_utf8(bytes).is_ok() {
        return None;
    }
    let printable = bytes
        .iter()
        .filter(|&&b| ascii_text(b) || b >= 0x80)
        .count();
    if printable * 100 < bytes.len() * 95 {
        return None;
    }
    const C1: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    Some(
        bytes
            .iter()
            .map(|&b| {
                if (0x80..=0x9f).contains(&b) {
                    C1[(b - 0x80) as usize]
                } else {
                    char::from(b)
                }
            })
            .collect(),
    )
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

fn is_tga(bytes: &[u8]) -> bool {
    bytes.ends_with(b"TRUEVISION-XFILE.\0") || looks_like_tga(bytes)
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
    read_raster(bytes, format).map(|image| scale_raster(image, options, window))
}

fn read_raster(bytes: &[u8], format: ImageFormat) -> Result<image::DynamicImage, String> {
    let decode = || {
        let mut decoder =
            image::ImageReader::with_format(io::Cursor::new(bytes), format).into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        Ok::<_, image::ImageError>(image)
    };
    decode().map_err(|e| format!("cannot decode {} image: {e}", format_name(format)))
}

fn raster_size(image: &image::DynamicImage, options: &Options, window: Window) -> (u32, u32) {
    target_size(
        (image.width() as f32, image.height() as f32),
        options,
        window,
    )
}

fn scale_raster(image: image::DynamicImage, options: &Options, window: Window) -> Rgba {
    let size = raster_size(&image, options, window);
    sized_rgba(image, size)
}

fn sized_rgba(image: image::DynamicImage, (w, h): (u32, u32)) -> Rgba {
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
    Rgba {
        width: w,
        height: h,
        pixels: image.into_raw(),
    }
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

mod animation;
mod tiles;
use tiles::{TileGrid, reserve_tile_ids};

/// Transmit pixels directly unless tmux needs text cells it can replay from history.
fn write_kitty(
    out: &mut impl Write,
    image: &Rgba,
    window: Window,
    wait: impl FnMut(),
) -> io::Result<()> {
    if let Some(grid) = transmit_image(out, image, window, wait)? {
        grid.write_cells(out)?;
    }
    Ok(())
}

/// Returns tmux cells to write after transmitting all pixels for a page.
fn transmit_image(
    out: &mut impl Write,
    image: &Rgba,
    window: Window,
    mut wait: impl FnMut(),
) -> io::Result<Option<TileGrid>> {
    if window.tmux {
        let grid = TileGrid::new(image, window, reserve_tile_ids)?;
        grid.send(out, image, None, &mut wait)?;
        Ok(Some(grid))
    } else {
        send_direct(out, image, None, None)?;
        out.write_all(b"\n")?;
        Ok(None)
    }
}

fn send_direct(
    out: &mut impl Write,
    image: &Rgba,
    id: Option<u32>,
    gap: Option<u32>,
) -> io::Result<()> {
    let mut compressed = ZlibEncoder::new(Vec::new(), Compression::fast());
    compressed.write_all(&image.pixels)?;
    let id = id.map(|id| format!("i={id},")).unwrap_or_default();
    send_chunks(
        out,
        &compressed.finish()?,
        &format!("f=32,o=z,q=2,{id}s={},v={}", image.width, image.height),
        gap,
        false,
        &mut || {},
    )
}

fn send_chunks(
    out: &mut impl Write,
    payload: &[u8],
    header: &str,
    gap: Option<u32>,
    tmux: bool,
    wait: &mut impl FnMut(),
) -> io::Result<()> {
    // Each full raw chunk ends on a base64 triple, so only the tail is padded.
    let chunks = payload.chunks(CHUNK / 4 * 3);
    let mut encoded = [0; CHUNK];
    let count = chunks.len();
    for (i, chunk) in chunks.enumerate() {
        let more = u8::from(i + 1 < count);
        let mut escape = if i == 0 {
            let action = gap.map_or_else(|| "T".into(), |gap| format!("f,z={gap},X=1"));
            format!("\x1b_Ga={action},{header},m={more};")
        } else if gap.is_some() {
            format!("\x1b_Ga=f,m={more};")
        } else {
            format!("\x1b_Gm={more};")
        }
        .into_bytes();
        let len = BASE64
            .encode_slice(chunk, &mut encoded)
            .expect("one raw chunk fits the base64 scratch");
        escape.extend_from_slice(&encoded[..len]);
        escape.extend_from_slice(b"\x1b\\");
        if tmux {
            out.write_all(&tmux_passthrough(&escape))?;
            out.flush()?;
            wait();
        } else {
            out.write_all(&escape)?;
        }
    }
    Ok(())
}

fn tmux_wait(window: Window) -> impl FnMut() {
    let old_tmux = window.tmux && allow_passthrough();
    move || {
        if old_tmux {
            // Older tmux discards passthrough when its client falls behind.
            std::thread::sleep(OLD_TMUX_CHUNK_PAUSE);
        }
    }
}

/// Turns `allow-passthrough` on for this tmux pane unless it already is, as `kitten icat`
/// does: tmux 3.3 and later default it to off and then drop passthrough silently. Whether
/// the tmux predates the option (`show` fails), in which case everything is passed through.
fn allow_passthrough() -> bool {
    static OLD_TMUX: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OLD_TMUX.get_or_init(|| {
        let pane = std::env::var("TMUX_PANE").ok();
        let target: Vec<&str> = pane.iter().flat_map(|p| ["-t", p.as_str()]).collect();
        let Some(current) = tmux(&[&["show", "-Ap"], &target[..], &["allow-passthrough"]].concat())
        else {
            return true;
        };
        if !passthrough_allowed(&current)
            && tmux(&[&["set", "-p"], &target[..], &["allow-passthrough", "on"]].concat()).is_none()
        {
            diag!(
                "{} show: couldn't turn on tmux's allow-passthrough, so images may not draw",
                env!("CARGO_BIN_NAME")
            );
        }
        false
    })
}

/// Whether `tmux show -Ap allow-passthrough` output (`allow-passthrough* off`) allows it.
fn passthrough_allowed(show: &str) -> bool {
    let value = show.trim();
    value.ends_with(" on") || value.ends_with(" all")
}

/// Runs tmux with `args`; its output if it succeeds within [`TMUX_TIMEOUT`].
fn tmux(args: &[&str]) -> Option<String> {
    let mut child = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + TMUX_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let mut output = String::new();
                io::Read::read_to_string(&mut child.stdout.take()?, &mut output).ok()?;
                return Some(output);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(Some(_)) => return None,
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `escape` wrapped in tmux's DCS passthrough (`ESC P tmux; ... ESC \`, every ESC inside
/// doubled), which tmux forwards to the terminal it runs in (3.3 and later only with
/// `allow-passthrough` on).
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
fn write_text(out: &mut impl Write, text: &str) -> io::Result<()> {
    let bytes = text.as_bytes();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        let caret;
        let replacement: &[u8] = match c {
            '\t' | '\n' => continue,
            '\r' if bytes.get(i + 1) == Some(&b'\n') => b"",
            '\r' => b"\n",
            '\0'..='\x1f' => {
                caret = [b'^', c as u8 + b'@'];
                &caret
            }
            '\x7f' => b"^?",
            '\u{80}'..='\u{9f}' => "�".as_bytes(),
            _ => continue,
        };
        out.write_all(&bytes[start..i])?;
        out.write_all(replacement)?;
        start = i + c.len_utf8();
    }
    out.write_all(&bytes[start..])?;
    if !text.is_empty() && !text.ends_with(['\r', '\n']) {
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// `text` with control characters shown as `^X`, safe to print on a terminal.
fn printable(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\x7f' => clean.push_str("^?"),
            c if (c as u32) < 0x20 => {
                clean.push('^');
                clean.push(char::from(c as u8 + 0x40));
            }
            '\u{80}'..='\u{9f}' => clean.push('\u{fffd}'),
            c => clean.push(c),
        }
    }
    clean
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
        // SAFETY: winsize contains only integer fields, all valid when zeroed.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ writes one winsize to the supplied writable pointer.
        let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0;
        (ok && ws.ws_col > 0 && ws.ws_row > 0).then_some(ws)
    }

    /// Asks for the text area size, followed by a device attributes query that every
    /// terminal answers, so one that ignores the first doesn't cost the whole timeout.
    fn query_pixels(tty: File) -> Option<(u32, u32)> {
        parse_pixel_report(&query(tty, b"\x1b[14t\x1b[c", da1_answered))
    }

    /// Writes `request` to the tty and reads the reply until `done` or the timeout.
    fn query(mut tty: File, request: &[u8], done: fn(&[u8]) -> bool) -> Vec<u8> {
        let fd = tty.as_raw_fd();
        // SAFETY: termios contains integer fields/arrays, all valid when zeroed.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: tty owns fd; saved is writable for one termios.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Vec::new();
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        // Timed reads (VMIN=0, VTIME in tenths of a second) rather than poll(): macOS's
        // poll() doesn't support ttys and reports POLLNVAL at once.
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;
        // SAFETY: raw came from tcgetattr, and tty still owns the open descriptor.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        let mut reply = Vec::new();
        if tty.write_all(request).is_ok() {
            let start = Instant::now();
            while start.elapsed().as_millis() < u128::from(QUERY_TIMEOUT_MS) {
                let mut buf = [0; 256];
                match tty.read(&mut buf) {
                    Ok(0) => {}
                    Ok(n) => reply.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
                if done(&reply) {
                    break;
                }
            }
        }
        // SAFETY: saved is the initialized original termios and tty is still open.
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        reply
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
