//! Cmd-hover (Ctrl on Linux and elsewhere) previews of file paths printed in the output: a
//! small card with an image thumbnail, a PDF's first page, the first lines of a text file or
//! a directory listing. Opt-in with `hover_preview = true` in the config.
//!
//! Relative paths use the OSC 7 origin of their printed cells. A worker checks
//! that origin, reads the file and draws the card away from the UI thread.

use crate::{App, UserEvent, links};
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use term_core::colors::Rgb;
use term_core::images::Image;
use term_core::{DirectoryOrigin, Flags, Link, Point, Row, Terminal};
use term_render::FloatingImage;

/// Paths longer than this are not looked for.
const MAX_PATH: usize = 4096;
/// Files up to this size are read whole, so images can be decoded.
const MAX_BYTES: u64 = 16 * 1024 * 1024;
/// How much of a bigger file is read for its text.
const TEXT_BYTES: u64 = 1024 * 1024;
const MAX_LINES: usize = 14;
const THUMBNAILER_TIMEOUT: Duration = Duration::from_secs(3);
/// Space between a card and the row it previews, in physical pixels.
const PLACE_GAP: f32 = 4.0;
static NEXT_PAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A stretch of a row that may name a file: `text`, drawn in cells `start..end`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub text: String,
    pub start: usize,
    pub end: usize,
    /// A line number given after the path, as in `src/main.rs:12:5`.
    pub line: Option<usize>,
}

/// Appends `row`'s characters as `(character, column, width)` in cells.
///
/// Columns are counted from `offset`; wide-character spacers and wrap padding
/// are omitted. Grapheme clusters are expanded through the terminal's table.
pub fn row_text(term: &Terminal, row: &Row, offset: usize, out: &mut Vec<(char, usize, usize)>) {
    for (col, cell) in row.cells.iter().enumerate() {
        if cell.flags.intersects(Flags::WIDE_SPACER) || cell.flags.intersects(Flags::WIDE_WRAP) {
            continue;
        }
        let width = if cell.flags.contains(Flags::WIDE) {
            2
        } else {
            1
        };
        let col = offset + col;
        match term.grapheme(cell) {
            Some(cluster) => out.extend(cluster.chars().map(|c| (c, col, width))),
            None => out.push((if cell.c == '\0' { ' ' } else { cell.c }, col, width)),
        }
    }
}

/// How many rows a path may soft-wrap across on either side of the hovered one.
const MAX_WRAPPED: isize = 8;

/// The logical line through viewport `row`, its rows joined across soft wraps: the first
/// row and the characters, columns counting on from one row to the next.
pub fn line_text(term: &Terminal, row: isize) -> (isize, Vec<(char, usize, usize)>) {
    let (lowest, last) = (row.min(0), term.rows() as isize - 1);
    let mut first = row;
    while first > lowest
        && first > row - MAX_WRAPPED
        && term.viewport_row_signed(first - 1).wrapped()
    {
        first -= 1;
    }
    let mut end = row;
    while end < last && end < row + MAX_WRAPPED && term.viewport_row_signed(end).wrapped() {
        end += 1;
    }
    let mut chars = Vec::new();
    for (i, r) in (first..=end).enumerate() {
        row_text(
            term,
            &term.viewport_row_signed(r),
            i * term.cols(),
            &mut chars,
        );
    }
    (first, chars)
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

/// Possible paths at cell `col`, in lookup order: a quoted name, then unquoted names
/// spanning single spaces, then the word at the pointer with its location or git prefix
/// optionally removed. Runs of spaces between columns are not part of unquoted names.
pub fn candidates_at(chars: &[(char, usize, usize)], col: usize) -> Vec<Candidate> {
    let mut out = Vec::new();
    let Some(i) = chars
        .iter()
        .position(|&(_, start, width)| (start..start + width).contains(&col))
    else {
        return out;
    };
    for q in ['"', '\''] {
        let open = chars[..i].iter().rposition(|c| c.0 == q);
        let close = chars[i..].iter().position(|c| c.0 == q).map(|p| p + i);
        if let (Some(a), Some(b)) = (open, close)
            && i < b
            && b - a - 1 <= MAX_PATH
        {
            if q == '"'
                && let Some(line) = traceback_line(chars, a, b)
            {
                push_candidate(&mut out, chars, a + 1, b, Some(line));
                return out;
            }
            if chars[a + 1..b].iter().any(|c| c.0 == ' ') {
                push_variants(&mut out, chars, a + 1, b);
            }
        }
    }
    let joins_words = |k: usize| {
        chars[k].0 == ' '
            && k > 0
            && !breaks(chars[k - 1].0)
            && chars.get(k + 1).is_some_and(|c| !breaks(c.0))
    };
    let space = chars[i].0 == ' ';
    if breaks(chars[i].0) && !joins_words(i) {
        return out;
    }
    let mut a = i;
    while a > 0 && !breaks(chars[a - 1].0) {
        a -= 1;
    }
    let mut b = i + 1;
    while b < chars.len() && !breaks(chars[b].0) {
        b += 1;
    }
    // Try the whole stretch first. Trim at most three words of surrounding prose on
    // either side, bounding allocations and filesystem probes even on a line of prose.
    let (mut left, mut right) = (a, b);
    while left > 0 && (!breaks(chars[left - 1].0) || joins_words(left - 1)) {
        left -= 1;
    }
    while right < chars.len() && (!breaks(chars[right].0) || joins_words(right)) {
        right += 1;
    }
    if space || left != a || right != b {
        let starts = std::iter::once(left).chain(
            (left..a)
                .filter(|&k| chars[k].0 == ' ')
                .map(|k| k + 1)
                .take(3),
        );
        let ends =
            std::iter::once(right).chain((b..right).rev().filter(|&k| chars[k].0 == ' ').take(3));
        for from in starts {
            for to in ends.clone() {
                if to - from <= MAX_PATH && (space || (from, to) != (a, b)) {
                    push_path(&mut out, chars, from, to);
                }
            }
        }
    }
    if !space && b - a <= MAX_PATH {
        push_variants(&mut out, chars, a, b);
    }
    out
}

/// The one-based line in Python's `File "name", line N[, in function]`.
fn traceback_line(chars: &[(char, usize, usize)], open: usize, close: usize) -> Option<usize> {
    let prefix = open.checked_sub("File ".len())?;
    if (prefix > 0 && !chars[prefix - 1].0.is_whitespace())
        || !chars[prefix..open].iter().map(|c| c.0).eq("File ".chars())
    {
        return None;
    }
    let mut tail = chars[close + 1..].iter().map(|c| c.0).peekable();
    if !", line ".chars().all(|c| tail.next() == Some(c)) {
        return None;
    }
    let mut line = 0usize;
    while let Some(digit) = tail.next_if(char::is_ascii_digit) {
        line = line
            .checked_mul(10)?
            .checked_add(digit.to_digit(10)? as usize)?;
    }
    (line > 0 && tail.peek().is_none_or(|&c| c == ',' || c.is_whitespace())).then_some(line)
}

fn push_candidate(
    out: &mut Vec<Candidate>,
    chars: &[(char, usize, usize)],
    from: usize,
    to: usize,
    line: Option<usize>,
) {
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
}

/// Tries the literal path before removing punctuation or a trailing compiler location.
fn push_path(
    out: &mut Vec<Candidate>,
    chars: &[(char, usize, usize)],
    a: usize,
    b: usize,
) -> (usize, Option<usize>) {
    push_candidate(out, chars, a, b, None);
    let mut trimmed = b;
    while trimmed > a && matches!(chars[trimmed - 1].0, '.' | ',' | ':' | ';' | '!' | '?') {
        trimmed -= 1;
    }
    let (path, line) = strip_location(chars, a, trimmed);
    if path < trimmed {
        push_candidate(out, chars, a, path, line);
    }
    if trimmed < b {
        push_candidate(out, chars, a, trimmed, None);
    }
    (path, line)
}

fn push_variants(out: &mut Vec<Candidate>, chars: &[(char, usize, usize)], a: usize, b: usize) {
    let (path, line) = push_path(out, chars, a, b);
    // `path:line:text`, as grep -n prints.
    if let Some(colon) = (a + 1..b)
        .find(|&k| chars[k].0 == ':' && chars.get(k + 1).is_some_and(|c| c.0.is_ascii_digit()))
    {
        let mut end = colon + 1;
        while end < b && chars[end].0.is_ascii_digit() {
            end += 1;
        }
        let number: String = chars[colon + 1..end].iter().map(|c| c.0).collect();
        push_candidate(out, chars, a, colon, number.parse().ok());
    }
    if path > a + 2 && matches!(chars[a].0, 'a' | 'b') && chars[a + 1].0 == '/' {
        push_candidate(out, chars, a + 2, path, line);
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

fn printed_candidates(
    term: &Terminal,
    row: isize,
    col: usize,
) -> (u64, Vec<(Candidate, DirectoryOrigin)>) {
    let (first, chars) = line_text(term, row);
    let line = term.viewport_to_point_signed(first, 0).line;
    let cols = term.cols();
    let candidates = candidates_at(&chars, (row - first) as usize * cols + col)
        .into_iter()
        .map(|candidate| {
            let start = Point {
                line: line + (candidate.start / cols) as u64,
                col: candidate.start % cols,
            };
            (candidate, term.directory_origin_at(start))
        })
        .collect();
    (line, candidates)
}

/// The first candidate naming a file that exists, and its path.
fn resolve_first<'a>(
    candidates: impl IntoIterator<Item = (&'a Candidate, &'a DirectoryOrigin)>,
    process: impl FnOnce() -> Option<PathBuf>,
    home: Option<&Path>,
) -> Option<(&'a Candidate, PathBuf)> {
    let process = std::cell::LazyCell::new(process);
    candidates.into_iter().find_map(|(c, origin)| {
        links::with_file_base(
            origin,
            || (*process).clone(),
            |cwd| Some((c, resolve(&c.text, cwd, home)?)),
        )
    })
}

/// Where the shell said it is, with OSC 7.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reported {
    Local(PathBuf),
    /// On another machine, as over ssh: its paths don't name files here.
    Remote,
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

/// Why a helper program gave no output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunError {
    Missing,
    TimedOut,
    Failed,
}

/// Runs `command` and returns what it printed, if it succeeds within `timeout`.
#[cfg(not(target_os = "linux"))]
fn run(command: &mut Command, timeout: Duration) -> Option<Vec<u8>> {
    try_run(command, timeout).ok()
}

fn try_run(command: &mut Command, timeout: Duration) -> Result<Vec<u8>, RunError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => RunError::Missing,
            _ => RunError::Failed,
        })?;
    let mut stdout = child.stdout.take().ok_or(RunError::Failed)?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status
                    .success()
                    .then(|| reader.join().ok())
                    .flatten()
                    .ok_or(RunError::Failed);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            waited => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match waited {
                    Ok(None) => RunError::TimedOut,
                    _ => RunError::Failed,
                });
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

/// The padding and line height of a card at `scale`, in physical pixels.
fn card_metrics(scale: f32) -> (f32, f32) {
    let pad = (8.0 * scale).round();
    let line_h = ((12.0 * scale).round() * 1.4).round();
    (pad, line_h)
}

/// Room for a card's body for a word whose row spans `top..bottom` in a `window`:
/// the whole card fits on the roomier side of the row, or failing that in the window.
fn fit_room(scale: f32, top: f32, bottom: f32, window: (f32, f32)) -> (u32, u32) {
    let (pad, line_h) = card_metrics(scale);
    let frame_h = line_h + 2.0 * pad;
    let side = (top - PLACE_GAP).max(window.1 - bottom - PLACE_GAP);
    let height = if side - frame_h >= 4.0 * line_h {
        side
    } else {
        window.1
    };
    let max = body_room(scale);
    let w = (window.0 - 2.0 * pad).max(0.0) as u32;
    let h = (height - frame_h).max(line_h) as u32;
    (max.0.min(w), max.1.min(h))
}

/// How many text lines fit in `height` physical pixels of body, at least one.
fn lines_in(height: u32, scale: f32) -> usize {
    let (_, line_h) = card_metrics(scale);
    ((height as f32 / line_h) as usize).clamp(1, MAX_LINES)
}

fn content(path: &Path, line: Option<usize>, max: (u32, u32), rows: usize) -> Option<Content> {
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
        let more = names.len().saturating_sub(rows);
        names.truncate(rows);
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
            Ok((w, h, pixels)) => (format!("PDF · {size}"), Body::Image(w, h, pixels)),
            Err(why) => (size, note(pdf_note(why))),
        };
        return Some(Content {
            title,
            detail,
            body,
        });
    }
    if !complete && let Some(end) = memchr::memrchr(b'\n', &bytes) {
        // UTF-16LE spells a newline `0A 00`: keep the whole code unit.
        let utf16le = bytes.starts_with(&[0xFF, 0xFE]);
        let keep = end + 1 + usize::from(utf16le && end % 2 == 0);
        bytes.truncate(keep);
    }
    let (detail, body) = match crate::show::preview(&bytes, complete, path, max) {
        crate::show::Preview::Image(w, h, pixels, name) => {
            (format!("{name} · {size}"), Body::Image(w, h, pixels))
        }
        crate::show::Preview::Text(text) => (size, text_lines(&text, line, rows)),
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

/// Up to `rows` lines of `text`, numbered and centred on `line` (from 1) when given.
fn text_lines(text: &str, line: Option<usize>, rows: usize) -> Body {
    let total = text.lines().count();
    let target = line.filter(|&l| l >= 1 && l <= total).map(|l| l - 1);
    let start = target.map_or(0, |t| {
        t.saturating_sub((rows - 1) / 2)
            .min(total.saturating_sub(rows))
    });
    let lines = text
        .lines()
        .skip(start)
        .take(rows)
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

/// A fresh directory only we can write to, so nobody can plant a symlink where a
/// thumbnailer writes its output.
fn private_dir() -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = std::env::temp_dir().join(format!(
        "trident-hover-{}-{}",
        std::process::id(),
        NEXT_PAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
    Some(dir)
}

/// What a PDF card says when there is no page to show.
fn pdf_note(why: RunError) -> &'static str {
    match why {
        RunError::Missing if cfg!(not(target_os = "macos")) => {
            "PDF document (install poppler-utils for a page preview)"
        }
        RunError::TimedOut => "PDF document (the page preview took too long)",
        _ => "PDF document",
    }
}

/// The first page of the PDF at `path`, rendered by the system's tools to fit `max`.
fn pdf_page(path: &Path, max: (u32, u32)) -> Result<(u32, u32, Vec<u8>), RunError> {
    let side = max.0.max(max.1).to_string();
    #[cfg(target_os = "macos")]
    let png = {
        let dir = private_dir().ok_or(RunError::Failed)?;
        let mut command = Command::new("qlmanage");
        command
            .arg("-t")
            .args(["-s", &side])
            .arg("-o")
            .arg(&dir)
            .arg(path);
        let ran = try_run(&mut command, THUMBNAILER_TIMEOUT);
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".png");
        let png = std::fs::read(dir.join(name)).ok();
        let _ = std::fs::remove_dir_all(&dir);
        ran?;
        png.ok_or(RunError::Failed)?
    };
    #[cfg(not(target_os = "macos"))]
    let png = {
        // pdftoppm 22 writes `-singlefile` output to `<root>.png` even for root `-`.
        let dir = private_dir().ok_or(RunError::Failed)?;
        let root = dir.join("page");
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
            .arg(&root);
        let out = root.with_extension("png");
        let ran = try_run(&mut command, THUMBNAILER_TIMEOUT);
        let png = std::fs::read(&out).ok();
        let _ = std::fs::remove_dir_all(&dir);
        ran?;
        png.ok_or(RunError::Failed)?
    };
    let page = image::load_from_memory(&png).map_err(|_| RunError::Failed)?;
    let page = if page.width() > max.0 || page.height() > max.1 {
        page.thumbnail(max.0, max.1)
    } else {
        page
    };
    let page = page.into_rgba8();
    Ok((page.width(), page.height(), page.into_raw()))
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
fn draw(content: &Content, style: Style, room: (u32, u32)) -> Option<Image> {
    use resvg::{tiny_skia, usvg};
    let s = style.scale;
    let (pad, line_h) = card_metrics(s);
    let font = (12.0 * s).round();
    let char_w = font * 0.6;
    let header = line_h + pad;
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
    let w = (body_w.max(header_w).max((160.0 * s).min(room.0 as f32)) + 2.0 * pad).ceil();
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

    // The bundled font first, then the system's for scripts it lacks (CJK and the like).
    static FONTS: std::sync::OnceLock<Arc<usvg::fontdb::Database>> = std::sync::OnceLock::new();
    let fontdb = FONTS.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        db.load_font_data(term_render::BUNDLED_FONT.to_vec());
        db.load_system_fonts();
        db.set_monospace_family(family);
        Arc::new(db)
    });
    let options = usvg::Options {
        fontdb: fontdb.clone(),
        ..Default::default()
    };
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
    let gap = PLACE_GAP;
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
    pane: u64,
    /// The first row of the hovered line; candidate columns count on across its rows.
    line: u64,
    cols: usize,
    candidates: Vec<(Candidate, DirectoryOrigin)>,
    /// The file a hovered OSC 8 link names, byte for byte.
    link: Option<PathBuf>,
    /// Whose working directory relative paths start at without OSC 7.
    process: Option<u32>,
    style: Style,
    /// Room for the card's body, in physical pixels.
    room: (u32, u32),
}

fn build(job: &Job, latest: &AtomicU64) -> Option<(Candidate, Image)> {
    let process = || job.process.and_then(process_cwd);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let (candidate, path) = match &job.link {
        Some(path) => {
            let (candidate, origin) = job.candidates.first()?;
            links::with_file_base(origin, || None, |_| Some((candidate, path.clone())))?
        }
        None => resolve_first(
            job.candidates.iter().map(|(c, origin)| (c, origin)),
            process,
            home.as_deref(),
        )?,
    };
    if latest.load(Ordering::Relaxed) != job.generation {
        return None;
    }
    let content = content(
        &path,
        candidate.line,
        job.room,
        lines_in(job.room.1, job.style.scale),
    )?;
    Some((candidate.clone(), draw(&content, job.style, job.room)?))
}

/// A card the worker finished, for the UI thread.
pub struct Ready {
    generation: u64,
    pane: u64,
    line: u64,
    cols: usize,
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
    pane: u64,
    line: u64,
    candidates: Vec<(Candidate, DirectoryOrigin)>,
    link: Option<PathBuf>,
    room: (u32, u32),
}

struct Shown {
    pane: u64,
    line: u64,
    cols: usize,
    candidate: Candidate,
    image: Arc<Image>,
    version: u64,
}

pub struct HoverPreview {
    hostname: Vec<u8>,
    latest: Arc<AtomicU64>,
    pending: Option<Key>,
    shown: Option<Shown>,
}

impl HoverPreview {
    pub fn new() -> HoverPreview {
        HoverPreview {
            hostname: hostname(),
            latest: Arc::new(AtomicU64::new(0)),
            pending: None,
            shown: None,
        }
    }
}

impl App {
    /// What the pointer is over, if a preview should be looked for.
    fn hover_preview_target(&self) -> Option<Job> {
        if !self.config.hover_preview
            || !self.focused
            || !links::command_modifier(self.modifiers.state())
        {
            return None;
        }
        let s = self.session.as_ref()?;
        let cell = (s.fonts.metrics.width, s.fonts.metrics.height);
        let pad = self.config.padding * s.window.scale_factor() as f32;
        if crate::pointer_in_search_bar(self.search.is_some(), self.mouse_pos, s.grid_px(), cell)
            || term_render::ledger_bar(&s.term, pad, s.scroll.offset as f32, cell.1)
                .is_some_and(|bar| bar.contains(self.mouse_pos.y as f32))
            || !crate::pointer_in_grid(
                self.mouse_pos,
                pad as f64,
                (s.term.cols(), s.term.rows()),
                cell,
            )
        {
            return None;
        }
        let (row, col) = self.cell_at(self.mouse_pos)?;
        let point = s.term.viewport_to_point_signed(row, col);
        let mut line = point.line;
        let mut link_path = None;
        let candidates = match s.term.link_at(point) {
            Some(link) => {
                let Reported::Local(path) =
                    parse_uri(link.uri.as_bytes(), &self.hover_preview.hostname)?
                else {
                    return None;
                };
                let text = path.to_string_lossy().into_owned();
                link_path = Some(path);
                vec![(
                    Candidate {
                        text,
                        start: col,
                        end: col,
                        line: None,
                    },
                    s.term.directory_origin_at(link.start),
                )]
            }
            None => {
                let (first, candidates) = printed_candidates(&s.term, row, col);
                line = first;
                candidates
            }
        };
        if candidates.is_empty() {
            return None;
        }
        let scale = s.window.scale_factor() as f32;
        let row_top = pad + row as f32 * cell.1 + s.scroll.offset as f32;
        let grid = s.grid_px();
        let room = fit_room(
            scale,
            row_top,
            row_top + cell.1,
            (grid.width as f32, grid.height as f32),
        );
        Some(Job {
            generation: 0,
            pane: s.pane,
            line,
            cols: s.term.cols(),
            candidates,
            link: link_path,
            process: None,
            style: Style {
                fg: s.term.palette.foreground,
                bg: s.term.palette.background,
                scale,
            },
            room,
        })
    }

    /// Starts looking for a preview of what the pointer is over, or hides the one shown.
    pub(crate) fn refresh_hover_preview(&mut self) {
        let job = self.hover_preview_target();
        let preview = &mut self.hover_preview;
        let key = job.as_ref().map(|job| Key {
            pane: job.pane,
            line: job.line,
            candidates: job.candidates.clone(),
            link: job.link.clone(),
            room: job.room,
        });
        if preview.pending == key {
            return;
        }
        let generation = preview.latest.fetch_add(1, Ordering::Relaxed) + 1;
        preview.pending = key;
        let hid = preview.shown.take().is_some();
        if let Some(mut job) = job {
            // Cloning the proxy wakes the event loop on macOS, so only do it for a new job.
            let proxy = self.proxy.clone();
            job.generation = generation;
            job.process = self.session.as_ref().and_then(|s| {
                s.pty
                    .running_program()
                    .and_then(|group| u32::try_from(group).ok())
                    .or_else(|| s.pty.shell_pid())
            });
            let latest = preview.latest.clone();
            let _ = std::thread::Builder::new()
                .name("hover preview".into())
                .spawn(move || {
                    let card = build(&job, &latest);
                    let ready = Ready {
                        generation,
                        pane: job.pane,
                        line: job.line,
                        cols: job.cols,
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
        let preview = &mut self.hover_preview;
        if ready.generation != preview.latest.load(Ordering::Relaxed) {
            return;
        }
        preview.shown = ready.card.map(|(candidate, image)| Shown {
            pane: ready.pane,
            line: ready.line,
            cols: ready.cols,
            candidate,
            image: Arc::new(image),
            version: ready.generation,
        });
        if let Some(s) = &mut self.session {
            s.request_frame();
        }
    }

    /// The card and the underline under the previewed path, for this frame.
    pub(crate) fn hover_preview_overlay(&self) -> Option<FloatingImage> {
        let shown = self.hover_preview.shown.as_ref()?;
        let s = self.session.as_ref().filter(|s| s.pane == shown.pane)?;
        let c = &shown.candidate;
        let cols = shown.cols.max(1);
        let at = |v: usize| Point {
            line: shown.line + (v / cols) as u64,
            col: v % cols,
        };
        let start = at(c.start);
        let top = s.term.viewport_to_point(0, 0).line;
        let row = start
            .line
            .checked_sub(top)
            .filter(|&r| r < s.term.rows() as u64)?;
        let underline = (c.end > c.start).then(|| Link {
            uri: String::new(),
            start,
            end: at(c.end - 1),
            pieces: Vec::new(),
            osc8: false,
        });
        let (cw, ch) = (s.fonts.metrics.width, s.fonts.metrics.height);
        let pad = self.config.padding * s.window.scale_factor() as f32;
        let row_top = pad + row as f32 * ch + s.scroll.offset as f32;
        let size = s.grid_px();
        let pos = place(
            pad + start.col as f32 * cw,
            row_top,
            row_top + ch,
            (shown.image.width as f32, shown.image.height as f32),
            (size.width as f32, size.height as f32),
        );
        let floating = FloatingImage {
            image: shown.image.clone(),
            version: shown.version,
            pos,
            underline,
        };
        Some(floating)
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
        assert!(
            texts(diff, at(diff, "a/src")).ends_with(&["a/src/lib.rs".into(), "src/lib.rs".into()])
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
        assert!(
            found
                .iter()
                .any(|c| c.text == "foo.c" && c.line == Some(10))
        );

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
        let found = candidates_at(&chars(line), at(line, "python3"));
        assert_eq!(found[0].text, "/usr/lib/python3/foo.py");
        assert_eq!(found[0].line, Some(12));

        for line in [
            r#"NotFile "notes.txt", line 25"#,
            r#"File "notes.txt", line 25x"#,
            r#"File "notes.txt", line 0"#,
            r#"File "notes.txt", line 999999999999999999999999"#,
        ] {
            assert_eq!(candidates_at(&chars(line), at(line, "notes"))[0].line, None);
        }
    }

    #[test]
    fn quoted_names_with_spaces() {
        let line = "'my notes.txt'  other.txt";
        let found = texts(line, at(line, "notes"));
        assert_eq!(found[0], "my notes.txt");
        assert!(found.contains(&"notes.txt".to_string()));

        let line = "Don't delete 'my notes (copy).txt'";
        assert_eq!(texts(line, at(line, "notes"))[0], "my notes (copy).txt");
    }

    #[test]
    fn trailing_punctuation_and_prose() {
        let line = "See README.md.";
        assert!(
            texts(line, at(line, "README")).ends_with(&["README.md.".into(), "README.md".into()])
        );
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
        let mut text = Vec::new();
        row_text(&term, &term.viewport_row(0), 0, &mut text);
        assert_eq!(text[0], ('日', 0, 2));
        assert_eq!(text[1], ('a', 2, 1));
    }

    #[test]
    fn line_text_joins_soft_wraps() {
        let mut term = term_core::Terminal::new(10, 3);
        term.advance(b"ab src/main.rs:3\r\n$ ");
        let (first, chars) = line_text(&term, 1);
        assert_eq!(first, 0);
        let texts: Vec<_> = candidates_at(&chars, 11)
            .into_iter()
            .map(|c| c.text)
            .collect();
        assert!(texts.contains(&"src/main.rs".to_string()), "{texts:?}");
        assert_eq!(line_text(&term, 2).0, 2);
    }

    #[test]
    fn line_text_spells_out_graphemes() {
        let mut term = term_core::Terminal::new(20, 2);
        term.advance("cafe\u{301}.txt".as_bytes());
        let text: String = line_text(&term, 0).1.iter().map(|c| c.0).collect();
        assert!(text.starts_with("cafe\u{301}.txt"), "{text:?}");
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

    fn resolve_in<'a>(found: &'a [Candidate], dir: &Path) -> Option<(&'a Candidate, PathBuf)> {
        resolve_first(
            found.iter().map(|c| (c, &DirectoryOrigin::Unreported)),
            || Some(dir.into()),
            None,
        )
    }

    #[test]
    fn preview_and_open_use_the_printed_origin() {
        let a = scratch();
        let b = a.with_extension("b");
        std::fs::create_dir_all(b.join("src")).unwrap();
        let file = "src/main.rs";
        std::fs::write(b.join(file), "other file").unwrap();
        let mut term = Terminal::new(40, 6);
        let report =
            |host: &str, path: &Path| format!("\x1b]7;file://{host}{}\x07", path.display());
        let printed = "src/main.rs:7:3\r\n";
        term.advance(
            format!(
                "{printed}{}{printed}{}{printed}{}{printed}",
                report("", &a),
                report("elsewhere.invalid", &b),
                report("", &b)
            )
            .as_bytes(),
        );
        let check = |term: &Terminal, row, col, expected: Option<PathBuf>| {
            let link = term
                .file_link_at(term.viewport_to_point_signed(row, col))
                .unwrap();
            assert_eq!(links::resolve_file(&link, || Some(b.clone())), expected);
            let (_, candidates) = printed_candidates(term, row, col);
            let preview = resolve_first(
                candidates.iter().map(|(c, origin)| (c, origin)),
                || Some(b.clone()),
                None,
            );
            assert_eq!(preview.map(|(_, path)| path), expected);
        };
        check(&term, 0, 2, Some(b.join(file))); // No report yet.
        check(&term, 1, 2, Some(a.join(file)));
        check(&term, 2, 2, None);
        check(&term, 3, 2, Some(b.join(file)));
        std::fs::remove_file(a.join(file)).unwrap();
        check(&term, 1, 2, None);
        std::fs::remove_dir_all(&a).unwrap();
        check(&term, 1, 2, None);
        std::fs::remove_dir_all(b).unwrap();
    }

    #[test]
    fn first_existing_candidate_wins() {
        let dir = scratch();
        let line = "error: src/main.rs:3:1: oops";
        let found = candidates_at(&chars(line), at(line, "main"));
        let (candidate, path) = resolve_in(&found, &dir).unwrap();
        assert_eq!(candidate.text, "src/main.rs");
        assert_eq!(candidate.line, Some(3));
        assert_eq!(path, dir.join("src/main.rs"));

        let diff = "+++ b/src/main.rs";
        let found = candidates_at(&chars(diff), at(diff, "main"));
        let (candidate, _) = resolve_in(&found, &dir).unwrap();
        assert_eq!(candidate.text, "src/main.rs");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn spaced_paths_resolve_across_every_cell() {
        let dir = scratch();
        std::fs::create_dir(dir.join("my dir")).unwrap();
        let file = dir.join("my dir/space name.txt");
        std::fs::write(&file, "full path").unwrap();
        std::fs::write(dir.join("name.txt"), "partial match").unwrap();
        for (line, name, number) in [
            ("my dir/space name.txt", "my dir/space name.txt", None),
            (
                "my dir/space name.txt:25:3: error",
                "my dir/space name.txt",
                Some(25),
            ),
            (
                "open ./my dir/space name.txt here",
                "./my dir/space name.txt",
                None,
            ),
            (
                "prefix  'my dir/space name.txt'  suffix",
                "my dir/space name.txt",
                None,
            ),
        ] {
            let start = line.find(name).unwrap();
            for col in start..start + name.len() {
                let found = candidates_at(&chars(line), col);
                let (candidate, path) = resolve_in(&found, &dir).unwrap();
                assert_eq!(path, file, "{line} at {col}");
                assert_eq!(candidate.line, number);
                assert_eq!(
                    (candidate.start, candidate.end),
                    (start, start + name.len())
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn traceback_preview_marks_the_referenced_line() {
        let dir = scratch();
        let name = "my notes.txt";
        std::fs::write(
            dir.join(name),
            (1..=50).map(|n| format!("row {n}\n")).collect::<String>(),
        )
        .unwrap();
        let mut term = Terminal::new(12, 4);
        term.advance(format!(r#"  File "{name}", line 25, in main"#).as_bytes());
        let (_, chars) = line_text(&term, 1);
        let found = candidates_at(&chars, 11);
        let (candidate, path) = resolve_in(&found, &dir).unwrap();
        assert_eq!((candidate.start, candidate.end), (8, 8 + name.len()));
        let Body::Lines { lines, first, mark } =
            content(&path, candidate.line, (400, 200), 5).unwrap().body
        else {
            panic!("expected text preview")
        };
        assert_eq!((first, mark), (Some(23), Some(2)));
        assert_eq!(lines[2], "row 25");
        std::fs::remove_dir_all(dir).unwrap();
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
    fn cards_fit_small_windows() {
        let style = Style {
            fg: (0xc0, 0xca, 0xf5),
            bg: (0x1a, 0x1b, 0x26),
            scale: 1.0,
        };
        let text: String = (1..=40)
            .map(|i| format!("{}\n", "x".repeat(i * 3)))
            .collect();
        let image = Body::Image(500, 400, vec![255; 500 * 400 * 4]);
        for window in [
            (440.0, 302.0),
            (435.0, 316.0),
            (300.0, 200.0),
            (800.0, 600.0),
        ] {
            for row in [0.0, window.1 / 2.0 - 10.0, window.1 - 20.0] {
                let room = fit_room(1.0, row, row + 20.0, window);
                let rows = lines_in(room.1, 1.0);
                let lines = Content {
                    title: "main.rs".into(),
                    detail: "1.2 KB".into(),
                    body: text_lines(&text, Some(20), rows),
                };
                let (w, h) = (room.0.min(500), room.1.min(400));
                let picture = Content {
                    title: "big.png".into(),
                    detail: "PNG · 1 MB".into(),
                    body: match &image {
                        Body::Image(..) => Body::Image(w, h, vec![255; (w * h * 4) as usize]),
                        _ => unreachable!(),
                    },
                };
                for content in [lines, picture] {
                    let card = draw(&content, style, room).unwrap();
                    let size = (card.width as f32, card.height as f32);
                    assert!(
                        size.0 <= window.0 && size.1 <= window.1,
                        "{size:?} in {window:?}"
                    );
                    let [x, y] = place(10.0, row, row + 20.0, size, window);
                    assert!(x + size.0 <= window.0 && y + size.1 <= window.1);
                }
            }
        }
        assert_eq!(
            fit_room(1.0, 300.0, 320.0, (1600.0, 1200.0)),
            body_room(1.0),
            "big windows keep the full size"
        );
    }

    #[test]
    fn pdf_notes_name_the_problem() {
        assert_eq!(
            pdf_note(RunError::TimedOut),
            "PDF document (the page preview took too long)"
        );
        assert_eq!(pdf_note(RunError::Failed), "PDF document");
        assert!(
            !pdf_note(RunError::TimedOut).contains("install"),
            "a slow pdftoppm is installed"
        );
    }

    #[test]
    fn text_cards_show_the_reported_line() {
        let text: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        let Body::Lines { lines, first, mark } = text_lines(&text, Some(20), MAX_LINES) else {
            panic!("lines");
        };
        assert_eq!((first, mark), (Some(14), Some(6)), "line 20 in the middle");
        assert_eq!(lines[mark.unwrap()], "line 20");
        assert_eq!(lines.len(), MAX_LINES);
        let Body::Lines { first, mark, .. } = text_lines(&text, Some(2), MAX_LINES) else {
            panic!("lines");
        };
        assert_eq!((first, mark), (Some(1), Some(1)), "near the top");
        let Body::Lines { lines, first, mark } = text_lines(&text, Some(39), MAX_LINES) else {
            panic!("lines");
        };
        assert_eq!((first, mark), (Some(27), Some(12)), "near the end");
        assert_eq!(lines.len(), MAX_LINES);
        let Body::Lines { lines, first, mark } = text_lines(&text, Some(20), 5) else {
            panic!("lines");
        };
        assert_eq!((lines.len(), first, mark), (5, Some(18), Some(2)));
        let Body::Lines { lines, first, mark } = text_lines("a\tb\n", Some(99), MAX_LINES) else {
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
        let text = content(&dir.join("src/main.rs"), None, body_room(1.0), MAX_LINES).unwrap();
        assert_eq!(text.title, "main.rs");
        let card = draw(&text, style, body_room(1.0)).unwrap();
        assert_eq!(card.pixels.len(), (card.width * card.height * 4) as usize);

        let png = dir.join("dot.png");
        image::RgbaImage::from_pixel(600, 300, image::Rgba([255, 0, 0, 255]))
            .save(&png)
            .unwrap();
        let picture = content(&png, None, body_room(1.0), MAX_LINES).unwrap();
        assert!(
            picture.detail.starts_with("PNG 600×300"),
            "{}",
            picture.detail
        );
        let Body::Image(w, h, _) = picture.body else {
            panic!("image");
        };
        assert!(w <= 420 && h <= 260 && w == 2 * h, "{w}×{h}");
        let card = draw(&picture, style, body_room(1.0)).unwrap();
        let centre = ((card.height / 2 * card.width + card.width / 2) * 4) as usize;
        assert_eq!(&card.pixels[centre..centre + 4], &[255, 0, 0, 255]);

        let listing = content(&dir, None, body_room(1.0), MAX_LINES).unwrap();
        assert_eq!(listing.detail, "2 items");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A one-page PDF with a filled square, small enough to write inline.
    fn tiny_pdf() -> Vec<u8> {
        let stream = "0 0 1 rg 0 0 200 200 re f\n";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}endstream", stream.len()),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        out
    }

    #[test]
    fn pdfs_show_their_first_page() {
        if Command::new("pdftoppm").arg("-v").output().is_err() {
            eprintln!("skipping: no pdftoppm");
            return;
        }
        let dir = scratch();
        let pdf = dir.join("page.pdf");
        std::fs::write(&pdf, tiny_pdf()).unwrap();
        let page = content(&pdf, None, body_room(1.0), MAX_LINES).unwrap();
        assert!(page.detail.starts_with("PDF · "), "{}", page.detail);
        let Body::Image(w, h, pixels) = page.body else {
            panic!("page");
        };
        assert!(w == h && w <= 420, "{w}×{h}");
        let centre = ((h / 2 * w + w / 2) * 4) as usize;
        assert_eq!(&pixels[centre..centre + 3], &[0, 0, 255]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
