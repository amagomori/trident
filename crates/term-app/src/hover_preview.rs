//! Cmd-hover (Ctrl on Linux and elsewhere) previews of file paths printed in the output: a
//! small card with an image thumbnail, a PDF's first page, the first lines of a text file or
//! a directory listing. Opt-in with `TERMINAL_HOVER_PREVIEW=1`.
//!
//! Relative paths resolve against the directory the shell last reported with OSC 7, else
//! the shell process's own working directory; a shell reporting another host turns previews
//! off. The UI thread only gathers the hovered row's text; a worker thread reads the file
//! and draws the card.

use crate::{App, UserEvent, links};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use term_core::colors::Rgb;
use term_core::images::Image;
use term_core::{Flags, Link, Point, Row};
use term_render::FloatingImage;

pub const ENV: &str = "TERMINAL_HOVER_PREVIEW";

/// Paths longer than this are not looked for.
const MAX_PATH: usize = 4096;
/// Files up to this size are read whole, so images can be decoded.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How much of a bigger file is read for its text.
const TEXT_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 14;
const THUMBNAILER_TIMEOUT: Duration = Duration::from_secs(3);

pub fn enabled(value: Option<&OsStr>) -> bool {
    value.and_then(OsStr::to_str).is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// A stretch of a row that may name a file: `text`, drawn in cells `start..end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// A line number given after the path, as in `src/main.rs:12:5`.
    pub line: Option<usize>,
}

/// The row's characters, each with the column it starts at and its width in cells.
pub fn row_text(row: &Row) -> Vec<(char, usize, usize)> {
    let mut chars = Vec::with_capacity(row.cells.len());
    for (col, cell) in row.cells.iter().enumerate() {
        if cell.flags.contains(Flags::WIDE_SPACER) {
            continue;
        }
        let width = if cell.flags.contains(Flags::WIDE) {
            2
        } else {
            1
        };
        let c = if cell.c == '\0' { ' ' } else { cell.c };
        chars.push((c, col, width));
    }
    chars
}

fn breaks(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|' | ',' | ';' | '='
        )
        // Box drawing, as `tree` and table borders print.
        || ('\u{2500}'..='\u{259f}').contains(&c)
}

/// Strings around column `col` of a row that may be paths, most literal first: the word
/// under the pointer as printed, then without trailing punctuation or a `:line:col`
/// location, then without git's `a/` and `b/` prefixes. A quoted stretch holding spaces,
/// as `ls` prints names with spaces, comes first.
pub fn candidates_at(chars: &[(char, usize, usize)], col: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Some(i) = chars
        .iter()
        .position(|&(_, start, width)| (start..start + width).contains(&col))
    else {
        return out;
    };
    if breaks(chars[i].0) {
        return out;
    }
    for q in ['\'', '"'] {
        let open = chars[..i].iter().rposition(|c| c.0 == q);
        let close = chars[i..].iter().position(|c| c.0 == q).map(|p| p + i);
        if let (Some(a), Some(b)) = (open, close)
            && b - a - 1 <= MAX_PATH
            && chars[a + 1..b].iter().any(|c| c.0 == ' ')
        {
            push_variants(&mut out, chars, a + 1, b);
        }
    }
    let mut a = i;
    while a > 0 && !breaks(chars[a - 1].0) {
        a -= 1;
    }
    let mut b = i + 1;
    while b < chars.len() && !breaks(chars[b].0) {
        b += 1;
    }
    if b - a <= MAX_PATH {
        push_variants(&mut out, chars, a, b);
    }
    out
}

fn push_variants(out: &mut Vec<Candidate>, chars: &[(char, usize, usize)], a: usize, b: usize) {
    let mut push = |from: usize, to: usize, line: Option<usize>| {
        if to <= from {
            return;
        }
        let text: String = chars[from..to].iter().map(|c| c.0).collect();
        let named = text.chars().any(char::is_alphanumeric) || text.starts_with(['/', '~']);
        if !named || text.contains("://") {
            return;
        }
        let candidate = Candidate {
            text,
            start: chars[from].1,
            end: chars[to - 1].1 + chars[to - 1].2,
            line,
        };
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    push(a, b, None);
    let mut trimmed = b;
    while trimmed > a && matches!(chars[trimmed - 1].0, '.' | ',' | ':' | ';' | '!' | '?') {
        trimmed -= 1;
    }
    let (path, line) = strip_location(chars, a, trimmed);
    push(a, path, line);
    push(a, trimmed, None);
    // `path:line:text`, as grep -n prints.
    if let Some(colon) = (a + 1..trimmed)
        .find(|&k| chars[k].0 == ':' && chars.get(k + 1).is_some_and(|c| c.0.is_ascii_digit()))
    {
        let mut end = colon + 1;
        while end < b && chars[end].0.is_ascii_digit() {
            end += 1;
        }
        let number: String = chars[colon + 1..end].iter().map(|c| c.0).collect();
        push(a, colon, number.parse().ok());
    }
    if path > a + 2 && matches!(chars[a].0, 'a' | 'b') && chars[a + 1].0 == '/' {
        push(a + 2, path, line);
    }
}

/// Where `path:line` or `path:line:col` in `chars[a..end]` ends its path, and the line.
fn strip_location(chars: &[(char, usize, usize)], a: usize, end: usize) -> (usize, Option<usize>) {
    let digits_from = |to: usize| {
        let mut from = to;
        while from > a && chars[from - 1].0.is_ascii_digit() {
            from -= 1;
        }
        from
    };
    let number = |from: usize, to: usize| {
        chars[from..to]
            .iter()
            .map(|c| c.0)
            .collect::<String>()
            .parse()
            .ok()
    };
    let last = digits_from(end);
    if last == end || last <= a + 1 || chars[last - 1].0 != ':' {
        return (end, None);
    }
    let colon = last - 1;
    let first = digits_from(colon);
    if first < colon && first > a + 1 && chars[first - 1].0 == ':' {
        return (first - 1, number(first, colon));
    }
    (colon, number(last, end))
}

/// `text` as a path that exists: `~` is the home directory and relative paths start at
/// `cwd`.
pub fn resolve(text: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let path = if text == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/") {
        home?.join(rest)
    } else if Path::new(text).is_absolute() {
        PathBuf::from(text)
    } else {
        cwd?.join(text)
    };
    path.metadata().is_ok().then_some(path)
}

/// The first candidate naming a file that exists, and its path.
pub fn resolve_first<'a>(
    candidates: &'a [Candidate],
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<(&'a Candidate, PathBuf)> {
    candidates
        .iter()
        .find_map(|c| Some((c, resolve(&c.text, cwd, home)?)))
}

/// Where the shell said it is, with OSC 7.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported {
    Local(PathBuf),
    /// On another machine, as over ssh: its paths don't name files here.
    Remote,
}

const OSC7: &[u8] = b"\x1b]7;";

/// Follows OSC 7 reports in the shell's output, across reads that split them.
#[derive(Debug, Default)]
pub struct Osc7 {
    partial: Vec<u8>,
    hostname: Vec<u8>,
    pub reported: Option<Reported>,
}

impl Osc7 {
    pub fn new() -> Osc7 {
        Osc7 {
            hostname: hostname(),
            ..Osc7::default()
        }
    }

    pub fn scan(&mut self, bytes: &[u8]) {
        if self.partial.is_empty()
            && memchr::memchr(0x1b, bytes)
                .is_none_or(|_| memchr::memmem::find(bytes, OSC7).is_none())
            && !ends_with_start(bytes)
        {
            return;
        }
        let joined;
        let data = if self.partial.is_empty() {
            bytes
        } else {
            self.partial.extend_from_slice(bytes);
            joined = std::mem::take(&mut self.partial);
            &joined[..]
        };
        let mut rest = data;
        while let Some(at) = memchr::memmem::find(rest, OSC7) {
            let body = &rest[at + OSC7.len()..];
            let Some((len, terminator)) = terminator(body) else {
                if body.len() <= MAX_PATH * 3 {
                    self.partial = rest[at..].to_vec();
                }
                return;
            };
            if let Some(reported) = parse_uri(&body[..len], &self.hostname) {
                self.reported = Some(reported);
            }
            rest = &body[len + terminator..];
        }
        if ends_with_start(rest) {
            let keep = (1..OSC7.len()).rev().find(|&k| rest.ends_with(&OSC7[..k]));
            self.partial = keep.map_or_else(Vec::new, |k| OSC7[..k].to_vec());
        }
    }
}

/// Whether `bytes` ends partway into an OSC 7 introducer.
fn ends_with_start(bytes: &[u8]) -> bool {
    (1..OSC7.len()).any(|k| bytes.ends_with(&OSC7[..k]))
}

/// The length of an OSC's text and of its BEL or ST terminator.
fn terminator(body: &[u8]) -> Option<(usize, usize)> {
    let at = memchr::memchr2(0x07, 0x1b, body)?;
    if body[at] == 0x07 {
        return Some((at, 1));
    }
    match body.get(at + 1) {
        None => None,
        Some(b'\\') => Some((at, 2)),
        Some(_) => Some((at, 1)),
    }
}

/// A `file://host/path` (or kitty's `kitty-shell-cwd://`) URI as a place on this machine.
pub fn parse_uri(uri: &[u8], hostname: &[u8]) -> Option<Reported> {
    let rest = uri
        .strip_prefix(b"file://")
        .or_else(|| uri.strip_prefix(b"kitty-shell-cwd://"))?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (host, path) = rest.split_at(slash);
    let path = percent_decode(path)?;
    let short = |h: &[u8]| {
        h.split(|&b| b == b'.')
            .next()
            .unwrap_or(h)
            .to_ascii_lowercase()
    };
    let local = host.is_empty()
        || host.eq_ignore_ascii_case(b"localhost")
        || host.eq_ignore_ascii_case(hostname)
        || (!hostname.is_empty() && short(host) == short(hostname));
    if !local {
        return Some(Reported::Remote);
    }
    Some(Reported::Local(PathBuf::from(OsString::from_vec(path))))
}

fn percent_decode(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

fn hostname() -> Vec<u8> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length, and gethostname writes at most that.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return Vec::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf[..len].to_vec()
}

/// The working directory of process `pid`.
fn process_cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let pid = pid.to_string();
        let out = run(
            Command::new("lsof").args(["-a", "-p", &pid, "-d", "cwd", "-Fn"]),
            THUMBNAILER_TIMEOUT,
        )?;
        out.split(|&b| b == b'\n')
            .find_map(|line| line.strip_prefix(b"n"))
            .map(|path| PathBuf::from(OsString::from_vec(path.to_vec())))
    }
}

/// Runs `command` and returns what it printed, if it succeeds within `timeout`.
fn run(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success().then(|| reader.join().ok()).flatten(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The colors and scale a card is drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub fg: Rgb,
    pub bg: Rgb,
    pub scale: f32,
}

enum Body {
    Image(u32, u32, Vec<u8>),
    Lines {
        lines: Vec<String>,
        /// The number of the first line, when lines are numbered.
        first: Option<usize>,
        /// The index of the line to highlight.
        mark: Option<usize>,
    },
}

struct Content {
    title: String,
    detail: String,
    body: Body,
}

/// Room for a card's body, in physical pixels.
fn body_room(scale: f32) -> (u32, u32) {
    ((420.0 * scale) as u32, (260.0 * scale) as u32)
}

fn content(path: &Path, line: Option<usize>, max: (u32, u32)) -> Option<Content> {
    let title = path
        .file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned();
    let meta = path.metadata().ok()?;
    if meta.is_dir() {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| {
                let mut name = e.file_name().to_string_lossy().into_owned();
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    name.push('/');
                }
                name
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        let detail = match names.len() {
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        let more = names.len().saturating_sub(MAX_LINES);
        names.truncate(MAX_LINES);
        if more > 0 {
            names.pop();
            names.push(format!("… {} more", more + 1));
        }
        let body = Body::Lines {
            lines: names,
            first: None,
            mark: None,
        };
        return Some(Content {
            title,
            detail,
            body,
        });
    }
    if !meta.is_file() {
        return None;
    }
    let size = crate::show::size_label(meta.len());
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    let cap = if meta.len() <= MAX_BYTES {
        MAX_BYTES
    } else {
        TEXT_BYTES
    };
    file.take(cap).read_to_end(&mut bytes).ok()?;
    let complete = bytes.len() as u64 >= meta.len();
    if bytes.starts_with(b"%PDF-") {
        let (detail, body) = match pdf_page(path, max) {
            Some((w, h, pixels)) => (format!("PDF · {size}"), Body::Image(w, h, pixels)),
            None => (
                size,
                note(if cfg!(target_os = "macos") {
                    "PDF document"
                } else {
                    "PDF document (install poppler-utils for a page preview)"
                }),
            ),
        };
        return Some(Content {
            title,
            detail,
            body,
        });
    }
    if !complete && let Some(end) = memchr::memrchr(b'\n', &bytes) {
        bytes.truncate(end + 1);
    }
    let (detail, body) = match crate::show::preview(&bytes, complete, path, max) {
        crate::show::Preview::Image(w, h, pixels, name) => {
            (format!("{name} · {size}"), Body::Image(w, h, pixels))
        }
        crate::show::Preview::Text(text) => (size, text_lines(&text, line)),
        crate::show::Preview::Binary(kind) => (size, note(&kind)),
    };
    Some(Content {
        title,
        detail,
        body,
    })
}

fn note(text: &str) -> Body {
    Body::Lines {
        lines: vec![text.to_string()],
        first: None,
        mark: None,
    }
}

/// Up to `MAX_LINES` lines of `text`, numbered and around `line` (from 1) when given.
fn text_lines(text: &str, line: Option<usize>) -> Body {
    let total = text.lines().count();
    let target = line.filter(|&l| l >= 1 && l <= total).map(|l| l - 1);
    let start = target.map_or(0, |t| t.saturating_sub(3));
    let lines = text
        .lines()
        .skip(start)
        .take(MAX_LINES)
        .map(|l| {
            l.replace('\t', "    ")
                .chars()
                .map(|c| if c.is_control() { '\u{fffd}' } else { c })
                .collect()
        })
        .collect();
    Body::Lines {
        lines,
        first: target.map(|_| start + 1),
        mark: target.map(|t| t - start),
    }
}

/// The first page of the PDF at `path`, rendered by the system's tools to fit `max`.
fn pdf_page(path: &Path, max: (u32, u32)) -> Option<(u32, u32, Vec<u8>)> {
    let side = max.0.max(max.1).to_string();
    #[cfg(target_os = "macos")]
    let png = {
        let dir = std::env::temp_dir().join(format!("terminal-hover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let mut command = Command::new("qlmanage");
        command
            .arg("-t")
            .args(["-s", &side])
            .arg("-o")
            .arg(&dir)
            .arg(path);
        run(&mut command, THUMBNAILER_TIMEOUT)?;
        let mut name = path.file_name()?.to_os_string();
        name.push(".png");
        let out = dir.join(name);
        let png = std::fs::read(&out).ok();
        let _ = std::fs::remove_file(&out);
        png?
    };
    #[cfg(not(target_os = "macos"))]
    let png = {
        let mut command = Command::new("pdftoppm");
        command
            .args([
                "-f",
                "1",
                "-l",
                "1",
                "-singlefile",
                "-png",
                "-scale-to",
                &side,
            ])
            .arg(path)
            .arg("-");
        run(&mut command, THUMBNAILER_TIMEOUT)?
    };
    let page = image::load_from_memory(&png).ok()?;
    let page = if page.width() > max.0 || page.height() > max.1 {
        page.thumbnail(max.0, max.1)
    } else {
        page
    };
    let page = page.into_rgba8();
    Some((page.width(), page.height(), page.into_raw()))
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// `text` cut to `max` characters, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Draws the card: a header with the name and what the file is, over the body.
fn draw(content: &Content, style: Style) -> Option<Image> {
    use resvg::{tiny_skia, usvg};
    let s = style.scale;
    let pad = (8.0 * s).round();
    let font = (12.0 * s).round();
    let char_w = font * 0.6;
    let line_h = (font * 1.4).round();
    let header = line_h + pad;
    let room = body_room(s);
    let max_chars = ((room.0 as f32) / char_w) as usize;
    let (gutter, body_w, body_h) = match &content.body {
        Body::Image(w, h, _) => (0.0, *w as f32, *h as f32),
        Body::Lines { lines, first, .. } => {
            let gutter = first.map_or(0.0, |f| {
                ((f + lines.len()).to_string().len() + 1) as f32 * char_w
            });
            let chars = ((room.0 as f32 - gutter) / char_w) as usize;
            let longest = lines
                .iter()
                .map(|l| l.chars().count().min(chars))
                .max()
                .unwrap_or(0);
            (
                gutter,
                gutter + longest as f32 * char_w,
                lines.len() as f32 * line_h,
            )
        }
    };
    let detail = clip(&content.detail, max_chars / 2);
    let title_chars = max_chars.saturating_sub(detail.chars().count() + 2);
    let title = clip(&content.title, title_chars);
    let header_w = (title.chars().count() + detail.chars().count() + 2) as f32 * char_w;
    let w = (body_w.max(header_w).max(160.0 * s) + 2.0 * pad).ceil();
    let h = (header + body_h + pad).ceil();

    let card = mix(style.bg, style.fg, 0.07);
    let border = mix(style.bg, style.fg, 0.35);
    let dim = mix(style.bg, style.fg, 0.6);
    let mark = mix(style.bg, style.fg, 0.18);
    let family = term_render::BUNDLED_FONT_FAMILY;
    let baseline = |top: f32| top + (line_h + font * 0.7) / 2.0;
    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" font-family="{family}" font-size="{font}">"#
    );
    svg += &format!(r#"<rect width="{w}" height="{h}" fill="{}"/>"#, hex(card));
    svg += &format!(
        r#"<rect x="{o}" y="{o}" width="{}" height="{}" fill="none" stroke="{}" stroke-width="{s}"/>"#,
        w - s,
        h - s,
        hex(border),
        o = s / 2.0,
    );
    let y = baseline(pad / 2.0);
    svg += &format!(
        r#"<text x="{pad}" y="{y}" fill="{}" xml:space="preserve">{}</text>"#,
        hex(style.fg),
        escape(&title)
    );
    svg += &format!(
        r#"<text x="{}" y="{y}" fill="{}" text-anchor="end" xml:space="preserve">{}</text>"#,
        w - pad,
        hex(dim),
        escape(&detail)
    );
    svg += &format!(
        r#"<rect x="{s}" y="{}" width="{}" height="{s}" fill="{}"/>"#,
        header - s,
        w - 2.0 * s,
        hex(border)
    );
    if let Body::Lines {
        lines,
        first,
        mark: marked,
    } = &content.body
    {
        let chars = ((room.0 as f32 - gutter) / char_w) as usize;
        for (i, line) in lines.iter().enumerate() {
            let top = header + i as f32 * line_h;
            if *marked == Some(i) {
                svg += &format!(
                    r#"<rect x="{s}" y="{top}" width="{}" height="{line_h}" fill="{}"/>"#,
                    w - 2.0 * s,
                    hex(mark)
                );
            }
            if let Some(first) = first {
                svg += &format!(
                    r#"<text x="{}" y="{}" fill="{}" text-anchor="end">{}</text>"#,
                    pad + gutter - char_w,
                    baseline(top),
                    hex(dim),
                    first + i
                );
            }
            svg += &format!(
                r#"<text x="{}" y="{}" fill="{}" xml:space="preserve">{}</text>"#,
                pad + gutter,
                baseline(top),
                hex(style.fg),
                escape(&clip(line, chars))
            );
        }
    }
    svg += "</svg>";

    let mut options = usvg::Options::default();
    options
        .fontdb_mut()
        .load_font_data(term_render::BUNDLED_FONT.to_vec());
    options.fontdb_mut().set_monospace_family(family);
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let (pw, ph) = (w as u32, h as u32);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut pixels = pixmap.take_demultiplied();
    if let Body::Image(iw, ih, src) = &content.body {
        let (x0, y0) = (pad as u32, header as u32);
        for y in 0..*ih {
            for x in 0..*iw {
                let (dx, dy) = (x0 + x, y0 + y);
                if dx >= pw || dy >= ph {
                    continue;
                }
                let si = ((y * iw + x) * 4) as usize;
                let di = ((dy * pw + dx) * 4) as usize;
                let a = src[si + 3] as u32;
                for c in 0..3 {
                    let over = src[si + c] as u32 * a + pixels[di + c] as u32 * (255 - a);
                    pixels[di + c] = (over / 255) as u8;
                }
                pixels[di + 3] = 255;
            }
        }
    }
    Some(Image {
        width: pw,
        height: ph,
        pixels,
    })
}

/// Where a `size` card goes for a word whose row spans `top..bottom` starting at `x`:
/// below the row if it fits, else above, kept inside the `window`.
pub fn place(x: f32, top: f32, bottom: f32, size: (f32, f32), window: (f32, f32)) -> [f32; 2] {
    let gap = 4.0;
    let below = bottom + gap;
    let above = top - gap - size.1;
    let y = if below + size.1 <= window.1 || (above < 0.0 && window.1 - bottom >= top) {
        below
    } else {
        above
    };
    let x = x.min(window.0 - size.0).max(0.0);
    let y = y.min(window.1 - size.1).max(0.0);
    [x.round(), y.round()]
}

struct Job {
    generation: u64,
    line: u64,
    candidates: Vec<Candidate>,
    cwd: Option<PathBuf>,
    shell: Option<u32>,
    style: Style,
}

fn build(job: &Job, latest: &AtomicU64) -> Option<(Candidate, Image)> {
    let cwd = job.cwd.clone().or_else(|| job.shell.and_then(process_cwd));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (candidate, path) = resolve_first(&job.candidates, cwd.as_deref(), home.as_deref())?;
    if latest.load(Ordering::Relaxed) != job.generation {
        return None;
    }
    let content = content(&path, candidate.line, body_room(job.style.scale))?;
    Some((candidate.clone(), draw(&content, job.style)?))
}

/// A card the worker finished, for the UI thread.
pub struct Ready {
    generation: u64,
    line: u64,
    card: Option<(Candidate, Image)>,
}

impl std::fmt::Debug for Ready {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ready")
            .field("generation", &self.generation)
            .field("line", &self.line)
            .field("candidate", &self.card.as_ref().map(|c| &c.0))
            .finish()
    }
}

#[derive(Debug, PartialEq)]
struct Key {
    line: u64,
    candidates: Vec<Candidate>,
    cwd: Option<PathBuf>,
}

struct Shown {
    line: u64,
    candidate: Candidate,
    image: Arc<Image>,
    version: u64,
}

pub struct HoverPreview {
    pub osc7: Osc7,
    latest: Arc<AtomicU64>,
    pending: Option<Key>,
    shown: Option<Shown>,
}

impl HoverPreview {
    pub fn new() -> HoverPreview {
        HoverPreview {
            osc7: Osc7::new(),
            latest: Arc::new(AtomicU64::new(0)),
            pending: None,
            shown: None,
        }
    }
}

impl App {
    /// What the pointer is over, if a preview should be looked for.
    fn hover_preview_target(&self) -> Option<Job> {
        let preview = self.hover_preview.as_ref()?;
        if !self.focused || !links::command_modifier(self.modifiers) {
            return None;
        }
        let cwd = match &preview.osc7.reported {
            Some(Reported::Remote) => return None,
            Some(Reported::Local(dir)) => Some(dir.clone()),
            None => None,
        };
        let s = self.session.as_ref()?;
        let cell = (s.fonts.metrics.width, s.fonts.metrics.height);
        if crate::pointer_in_search_bar(
            self.search.is_some(),
            self.mouse_pos,
            s.window.inner_size(),
            cell,
        ) || !crate::pointer_in_grid(
            self.mouse_pos,
            self.config.padding as f64 * s.window.scale_factor(),
            (s.term.cols(), s.term.rows()),
            cell,
        ) {
            return None;
        }
        let (row, col) = self.cell_at(self.mouse_pos)?;
        let line = s.term.viewport_to_point_signed(row, col).line;
        let candidates = match self.hover_link() {
            Some(link) => {
                let Reported::Local(path) = parse_uri(link.uri.as_bytes(), &preview.osc7.hostname)?
                else {
                    return None;
                };
                vec![Candidate {
                    text: path.to_string_lossy().into_owned(),
                    start: col,
                    end: col,
                    line: None,
                }]
            }
            None => candidates_at(&row_text(s.term.viewport_row_signed(row)), col),
        };
        if candidates.is_empty() {
            return None;
        }
        Some(Job {
            generation: 0,
            line,
            candidates,
            cwd,
            shell: s.pty.shell_pid(),
            style: Style {
                fg: s.term.palette.foreground,
                bg: s.term.palette.background,
                scale: s.window.scale_factor() as f32,
            },
        })
    }

    /// Starts looking for a preview of what the pointer is over, or hides the one shown.
    pub(crate) fn refresh_hover_preview(&mut self) {
        if self.hover_preview.is_none() {
            return;
        }
        let job = self.hover_preview_target();
        let proxy = self.proxy.clone();
        let Some(preview) = &mut self.hover_preview else {
            return;
        };
        let key = job.as_ref().map(|job| Key {
            line: job.line,
            candidates: job.candidates.clone(),
            cwd: job.cwd.clone(),
        });
        if preview.pending == key {
            return;
        }
        let generation = preview.latest.fetch_add(1, Ordering::Relaxed) + 1;
        preview.pending = key;
        let hid = preview.shown.take().is_some();
        if let Some(mut job) = job {
            job.generation = generation;
            let latest = preview.latest.clone();
            let _ = std::thread::Builder::new()
                .name("hover preview".into())
                .spawn(move || {
                    let card = build(&job, &latest);
                    let ready = Ready {
                        generation,
                        line: job.line,
                        card,
                    };
                    let _ = proxy.send_event(UserEvent::HoverPreview(ready));
                });
        }
        if hid && let Some(s) = &mut self.session {
            s.request_frame();
        }
    }

    pub(crate) fn hover_preview_ready(&mut self, ready: Ready) {
        let Some(preview) = &mut self.hover_preview else {
            return;
        };
        if ready.generation != preview.latest.load(Ordering::Relaxed) {
            return;
        }
        preview.shown = ready.card.map(|(candidate, image)| Shown {
            line: ready.line,
            candidate,
            image: Arc::new(image),
            version: ready.generation,
        });
        if let Some(s) = &mut self.session {
            s.request_frame();
        }
    }

    /// The underline under the previewed path and the card, for this frame.
    pub(crate) fn hover_preview_overlay(&self) -> (Option<Link>, Option<FloatingImage>) {
        let Some(shown) = self.hover_preview.as_ref().and_then(|p| p.shown.as_ref()) else {
            return (None, None);
        };
        let Some(s) = &self.session else {
            return (None, None);
        };
        let top = s.term.viewport_to_point(0, 0).line;
        let Some(row) = shown
            .line
            .checked_sub(top)
            .filter(|&r| r < s.term.rows() as u64)
        else {
            return (None, None);
        };
        let c = &shown.candidate;
        let link = (c.end > c.start).then(|| Link {
            uri: String::new(),
            start: Point {
                line: shown.line,
                col: c.start,
            },
            end: Point {
                line: shown.line,
                col: c.end - 1,
            },
            pieces: Vec::new(),
        });
        let (cw, ch) = (s.fonts.metrics.width, s.fonts.metrics.height);
        let pad = self.config.padding * s.window.scale_factor() as f32;
        let row_top = pad + row as f32 * ch + s.scroll.offset as f32;
        let size = s.window.inner_size();
        let pos = place(
            pad + c.start as f32 * cw,
            row_top,
            row_top + ch,
            (shown.image.width as f32, shown.image.height as f32),
            (size.width as f32, size.height as f32),
        );
        let floating = FloatingImage {
            image: shown.image.clone(),
            version: shown.version,
            pos,
        };
        (link, Some(floating))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<(char, usize, usize)> {
        text.chars().enumerate().map(|(i, c)| (c, i, 1)).collect()
    }

    fn texts(line: &str, col: usize) -> Vec<String> {
        candidates_at(&chars(line), col)
            .into_iter()
            .map(|c| c.text)
            .collect()
    }

    fn at(line: &str, word: &str) -> usize {
        line.find(word).unwrap() + 1
    }

    #[test]
    fn opt_in_values() {
        assert!(enabled(Some(OsStr::new("1"))));
        assert!(enabled(Some(OsStr::new("true"))));
        assert!(enabled(Some(OsStr::new(" On "))));
        assert!(!enabled(Some(OsStr::new("0"))));
        assert!(!enabled(Some(OsStr::new(""))));
        assert!(!enabled(None));
    }

    #[test]
    fn ls_columns() {
        let line = "Cargo.lock  Cargo.toml  README.md  crates";
        assert_eq!(texts(line, at(line, "Cargo.toml")), ["Cargo.toml"]);
        assert_eq!(texts(line, at(line, "crates")), ["crates"]);
        assert!(texts(line, 10).is_empty(), "between names");
        assert!(texts(line, 200).is_empty(), "past the text");
    }

    #[test]
    fn git_status_lines() {
        let line = "\tmodified:   crates/term-app/src/main.rs";
        assert_eq!(
            texts(line, at(line, "term-app")),
            ["crates/term-app/src/main.rs"]
        );
        let renamed = "\trenamed:    old.txt -> new.txt";
        assert_eq!(texts(renamed, at(renamed, "new")), ["new.txt"]);
        let diff = "diff --git a/src/lib.rs b/src/lib.rs";
        assert_eq!(
            texts(diff, at(diff, "a/src")),
            ["a/src/lib.rs", "src/lib.rs"]
        );
    }

    #[test]
    fn compiler_locations() {
        let rust = "  --> crates/term-app/src/main.rs:120:5";
        let found = candidates_at(&chars(rust), at(rust, "main"));
        assert_eq!(found[0].text, "crates/term-app/src/main.rs:120:5");
        assert_eq!(found[1].text, "crates/term-app/src/main.rs");
        assert_eq!(found[1].line, Some(120));
        assert_eq!((found[1].start, found[1].end), (6, 33));

        let gcc = "foo.c:10: error: expected ';'";
        let found = candidates_at(&chars(gcc), 1);
        assert_eq!(found[1].text, "foo.c");
        assert_eq!(found[1].line, Some(10));

        let grep = "src/lib.rs:42:fn main() {";
        let found = candidates_at(&chars(grep), 3);
        assert!(
            found
                .iter()
                .any(|c| c.text == "src/lib.rs" && c.line == Some(42))
        );
    }

    #[test]
    fn python_traceback_quotes() {
        let line = r#"  File "/usr/lib/python3/foo.py", line 12, in main"#;
        assert_eq!(
            texts(line, at(line, "python3")),
            ["/usr/lib/python3/foo.py"]
        );
    }

    #[test]
    fn quoted_names_with_spaces() {
        let line = "'my notes.txt'  other.txt";
        let found = texts(line, at(line, "notes"));
        assert_eq!(found[0], "my notes.txt");
        assert!(found.contains(&"notes.txt".to_string()));
    }

    #[test]
    fn trailing_punctuation_and_prose() {
        let line = "See README.md.";
        assert_eq!(texts(line, at(line, "README")), ["README.md.", "README.md"]);
        assert!(texts("  ...  ", 3).is_empty(), "no name");
        assert!(
            texts("see https://example.com/a.png", 10).is_empty(),
            "URLs are links"
        );
        assert_eq!(texts("~/notes.txt", 3), ["~/notes.txt"]);
    }

    #[test]
    fn wide_characters_keep_columns() {
        // "日本.txt": two wide characters, then four narrow ones.
        let row = vec![
            ('日', 0, 2),
            ('本', 2, 2),
            ('.', 4, 1),
            ('t', 5, 1),
            ('x', 6, 1),
            ('t', 7, 1),
        ];
        let found = candidates_at(&row, 1);
        assert_eq!(found[0].text, "日本.txt");
        assert_eq!((found[0].start, found[0].end), (0, 8));
    }

    #[test]
    fn row_text_skips_spacers() {
        let mut term = term_core::Terminal::new(10, 2);
        term.advance("日a".as_bytes());
        let text = row_text(term.viewport_row(0));
        assert_eq!(text[0], ('日', 0, 2));
        assert_eq!(text[1], ('a', 2, 1));
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hover-preview-test-{}-{}",
            std::process::id(),
            std::thread::current()
                .name()
                .unwrap_or("t")
                .replace("::", "-")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        dir
    }

    #[test]
    fn resolves_against_the_reported_directory() {
        let dir = scratch();
        assert_eq!(
            resolve("src/main.rs", Some(&dir), None),
            Some(dir.join("src/main.rs"))
        );
        assert_eq!(resolve("src/missing.rs", Some(&dir), None), None);
        assert_eq!(resolve("src/main.rs", None, None), None, "no directory");
        let absolute = dir.join("src/main.rs");
        assert_eq!(
            resolve(absolute.to_str().unwrap(), None, None),
            Some(absolute.clone())
        );
        assert_eq!(resolve("~/src", None, Some(&dir)), Some(dir.join("src")));
        assert_eq!(resolve("~", None, Some(&dir)), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn first_existing_candidate_wins() {
        let dir = scratch();
        let line = "error: src/main.rs:3:1: oops";
        let found = candidates_at(&chars(line), at(line, "main"));
        let (candidate, path) = resolve_first(&found, Some(&dir), None).unwrap();
        assert_eq!(candidate.text, "src/main.rs");
        assert_eq!(candidate.line, Some(3));
        assert_eq!(path, dir.join("src/main.rs"));

        let diff = "+++ b/src/main.rs";
        let found = candidates_at(&chars(diff), at(diff, "main"));
        let (candidate, _) = resolve_first(&found, Some(&dir), None).unwrap();
        assert_eq!(candidate.text, "src/main.rs");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn osc7_uris() {
        let host = b"box.local";
        assert_eq!(
            parse_uri(b"file:///home/me/my%20dir", host),
            Some(Reported::Local("/home/me/my dir".into()))
        );
        assert_eq!(
            parse_uri(b"file://localhost/tmp", host),
            Some(Reported::Local("/tmp".into()))
        );
        assert_eq!(
            parse_uri(b"file://BOX/tmp", host),
            Some(Reported::Local("/tmp".into()))
        );
        assert_eq!(
            parse_uri(b"kitty-shell-cwd://box.local/srv", host),
            Some(Reported::Local("/srv".into()))
        );
        assert_eq!(parse_uri(b"file://other/tmp", host), Some(Reported::Remote));
        assert_eq!(parse_uri(b"file:///bad%zz", host), None);
        assert_eq!(parse_uri(b"https://example.com/", host), None);
        assert_eq!(
            parse_uri(b"file:///caf%C3%A9", host),
            Some(Reported::Local("/café".into()))
        );
    }

    #[test]
    fn osc7_follows_the_latest_report() {
        let mut osc7 = Osc7::default();
        osc7.scan(b"plain output");
        assert_eq!(osc7.reported, None);
        osc7.scan(b"\x1b]7;file:///one\x07prompt$ \x1b]7;file:///two\x1b\\");
        assert_eq!(osc7.reported, Some(Reported::Local("/two".into())));
        osc7.scan(b"\x1b]7;file://elsewhere/x\x07");
        assert_eq!(osc7.reported, Some(Reported::Remote));
    }

    #[test]
    fn osc7_split_across_reads() {
        let report = b"ls\r\n\x1b]7;file:///srv/my%20app\x1b\\$ ";
        for split in 0..report.len() {
            let mut osc7 = Osc7::default();
            osc7.scan(&report[..split]);
            osc7.scan(&report[split..]);
            assert_eq!(
                osc7.reported,
                Some(Reported::Local("/srv/my app".into())),
                "split at {split}"
            );
        }
        let mut osc7 = Osc7::default();
        for byte in report {
            osc7.scan(std::slice::from_ref(byte));
        }
        assert_eq!(osc7.reported, Some(Reported::Local("/srv/my app".into())));
    }

    #[test]
    fn osc7_gives_up_on_unterminated_reports() {
        let mut osc7 = Osc7::default();
        osc7.scan(b"\x1b]7;file:///");
        osc7.scan(&vec![b'a'; MAX_PATH * 4]);
        assert!(osc7.partial.is_empty());
        osc7.scan(b"\x1b]7;file:///ok\x07");
        assert_eq!(osc7.reported, Some(Reported::Local("/ok".into())));
    }

    #[test]
    fn cards_go_below_or_above() {
        let window = (800.0, 600.0);
        assert_eq!(
            place(100.0, 40.0, 60.0, (200.0, 100.0), window),
            [100.0, 64.0]
        );
        assert_eq!(
            place(100.0, 540.0, 560.0, (200.0, 100.0), window),
            [100.0, 436.0],
            "no room below"
        );
        assert_eq!(
            place(700.0, 40.0, 60.0, (200.0, 100.0), window),
            [600.0, 64.0]
        );
        assert_eq!(place(0.0, 10.0, 30.0, (200.0, 700.0), window), [0.0, 0.0]);
    }

    #[test]
    fn text_cards_show_the_reported_line() {
        let text: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let Body::Lines { lines, first, mark } = text_lines(&text, Some(20)) else {
            panic!("lines");
        };
        assert_eq!(first, Some(17));
        assert_eq!(lines[mark.unwrap()], "line 20");
        assert_eq!(lines.len(), MAX_LINES);
        let Body::Lines { lines, first, mark } = text_lines("a\tb\n", Some(99)) else {
            panic!("lines");
        };
        assert_eq!(
            (lines, first, mark),
            (vec!["a    b".to_string()], None, None)
        );
    }

    #[test]
    fn draws_text_and_image_cards() {
        let dir = scratch();
        let style = Style {
            fg: (0xc0, 0xca, 0xf5),
            bg: (0x1a, 0x1b, 0x26),
            scale: 1.0,
        };
        let text = content(&dir.join("src/main.rs"), None, body_room(1.0)).unwrap();
        assert_eq!(text.title, "main.rs");
        let card = draw(&text, style).unwrap();
        assert_eq!(card.pixels.len(), (card.width * card.height * 4) as usize);

        let png = dir.join("dot.png");
        image::RgbaImage::from_pixel(600, 300, image::Rgba([255, 0, 0, 255]))
            .save(&png)
            .unwrap();
        let picture = content(&png, None, body_room(1.0)).unwrap();
        assert!(
            picture.detail.starts_with("PNG 600×300"),
            "{}",
            picture.detail
        );
        let Body::Image(w, h, _) = picture.body else {
            panic!("image");
        };
        assert!(w <= 420 && h <= 260 && w == 2 * h, "{w}×{h}");
        let card = draw(&picture, style).unwrap();
        let centre = ((card.height / 2 * card.width + card.width / 2) * 4) as usize;
        assert_eq!(&card.pixels[centre..centre + 4], &[255, 0, 0, 255]);

        let listing = content(&dir, None, body_room(1.0)).unwrap();
        assert_eq!(listing.detail, "2 items");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
