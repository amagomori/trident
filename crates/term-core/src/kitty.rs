//! The kitty graphics protocol: `ESC _ G <control> ; <payload> ESC \` transmits, places,
//! deletes and queries images (https://sw.kovidgoyal.net/kitty/graphics-protocol/).
//! Shared memory, animation and Unicode placeholders are not supported.
//!
//! vte drops APC strings, so [`Terminal::advance`] diverts them here before the parser
//! sees their bodies. Payloads are base64-decoded as they stream in, so a large image never
//! sits in memory twice.

use super::Terminal;
use crate::images::{Image, ImageError, MAX_IMAGE_SIDE, PlaceOptions, Rect};
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use memchr::memmem;
use std::collections::HashMap;
use std::io::Read;
use std::sync::LazyLock;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
/// Longest control part (`a=T,f=100,...`) accepted.
const MAX_CONTROL: usize = 1024;
/// Longest file path accepted for `t=f` / `t=t`.
const MAX_PATH: usize = 4096;
/// Temp files are only read (and then deleted) when their name says they are meant for this.
const TEMP_MARKER: &str = "tty-graphics-protocol";

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Where [`Terminal::advance`] is in an APC string, across calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scan {
    #[default]
    Ground,
    /// The last chunk ended with ESC outside an APC.
    Esc,
    Apc,
    /// The last chunk ended with ESC inside an APC.
    ApcEsc,
}

#[derive(Default)]
pub(crate) struct Kitty {
    pub(crate) scan: Scan,
    apc: Apc,
    /// A chunked transmission (`m=1`) waiting for its next chunk.
    loading: Option<Box<Transmission>>,
    /// Image number (`I`) to the id of the newest image transmitted with it.
    numbers: HashMap<u32, u32>,
    next_id: u32,
}

#[derive(Default)]
struct Apc {
    control: Vec<u8>,
    /// The `;` was seen; bytes are payload now.
    in_payload: bool,
    /// Not a graphics command, or a control part too long to be one.
    ignored: bool,
    /// The command this APC carries, once its control part is complete.
    command: Option<Control>,
}

#[derive(Clone, Copy, Debug)]
struct Control {
    action: u8,
    quiet: u8,
    format: u32,
    medium: u8,
    compressed: bool,
    width: u32,
    height: u32,
    size: u32,
    offset: u32,
    id: u32,
    number: u32,
    placement: u32,
    more: bool,
    source: Rect,
    x_offset: u32,
    y_offset: u32,
    cols: u32,
    rows: u32,
    z: i32,
    keep_cursor: bool,
    delete: u8,
    invalid: bool,
    /// Only `m` and `q` were given, as in the second and later chunks of a transmission.
    chunk_only: bool,
}

impl Default for Control {
    fn default() -> Control {
        Control {
            action: b't',
            quiet: 0,
            format: 32,
            medium: b'd',
            compressed: false,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            id: 0,
            number: 0,
            placement: 0,
            more: false,
            source: Rect::default(),
            x_offset: 0,
            y_offset: 0,
            cols: 0,
            rows: 0,
            z: 0,
            keep_cursor: false,
            delete: b'a',
            invalid: false,
            chunk_only: true,
        }
    }
}

impl Control {
    fn parse(bytes: &[u8]) -> Control {
        let mut c = Control::default();
        for pair in bytes.split(|&b| b == b',').filter(|p| !p.is_empty()) {
            let (key, value) = match pair {
                [key, b'=', value @ ..] if !value.is_empty() => (*key, value),
                _ => {
                    c.invalid = true;
                    continue;
                }
            };
            c.chunk_only &= matches!(key, b'm' | b'q');
            let num = || std::str::from_utf8(value).ok()?.parse::<u32>().ok();
            let char_value = || match value {
                [v] => Some(*v),
                _ => None,
            };
            let ok = match key {
                b'a' => char_value().map(|v| c.action = v),
                b't' => char_value().map(|v| c.medium = v),
                b'o' => char_value().map(|v| c.compressed = v == b'z'),
                b'd' => char_value().map(|v| c.delete = v),
                b'z' => std::str::from_utf8(value)
                    .ok()
                    .and_then(|v| v.parse::<i32>().ok())
                    .map(|v| c.z = v),
                _ => num().map(|v| match key {
                    b'q' => c.quiet = v.min(2) as u8,
                    b'f' => c.format = v,
                    b's' => c.width = v,
                    b'v' => c.height = v,
                    b'S' => c.size = v,
                    b'O' => c.offset = v,
                    b'i' => c.id = v,
                    b'I' => c.number = v,
                    b'p' => c.placement = v,
                    b'm' => c.more = v == 1,
                    b'x' => c.source.x = v,
                    b'y' => c.source.y = v,
                    b'w' => c.source.width = v,
                    b'h' => c.source.height = v,
                    b'X' => c.x_offset = v,
                    b'Y' => c.y_offset = v,
                    b'c' => c.cols = v,
                    b'r' => c.rows = v,
                    b'C' => c.keep_cursor = v == 1,
                    _ => {}
                }),
            };
            c.invalid |= ok.is_none();
        }
        c.invalid |= c.id != 0 && c.number != 0;
        c
    }
}

struct Transmission {
    control: Control,
    data: Vec<u8>,
    /// Base64 characters left over from the last chunk (fewer than 4).
    carry: Vec<u8>,
    limit: usize,
    error: Option<&'static str>,
}

impl Transmission {
    fn push(&mut self, mut bytes: &[u8]) {
        if self.error.is_some() || bytes.is_empty() {
            return;
        }
        if !self.carry.is_empty() {
            let take = (4 - self.carry.len()).min(bytes.len());
            self.carry.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.carry.len() < 4 {
                return;
            }
            let carry = std::mem::take(&mut self.carry);
            self.decode(&carry);
        }
        let whole = bytes.len() / 4 * 4;
        self.decode(&bytes[..whole]);
        self.carry.extend_from_slice(&bytes[whole..]);
    }

    fn finish(&mut self) {
        let carry = std::mem::take(&mut self.carry);
        self.decode(&carry);
    }

    fn decode(&mut self, chunk: &[u8]) {
        if self.error.is_some() || chunk.is_empty() {
            return;
        }
        let padding = chunk.iter().rev().take(2).filter(|&&b| b == b'=').count();
        let decoded = (chunk.len() / 4 * 3).saturating_sub(padding);
        if self.data.len() + decoded > self.limit {
            self.fail("EFBIG:image data too large");
        } else if BASE64.decode_vec(chunk, &mut self.data).is_err() {
            self.fail("EINVAL:bad base64 payload");
        }
    }

    fn fail(&mut self, error: &'static str) {
        self.error = Some(error);
        self.data = Vec::new();
    }
}

impl Terminal {
    /// Feeds bytes to the VT parser, diverting `ESC _ ... ESC \` to the graphics protocol.
    pub(crate) fn advance_bytes(&mut self, parser: &mut vte::Parser, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            bytes = match self.kitty.scan {
                Scan::Ground => match find_apc(bytes) {
                    Some(i) => {
                        parser.advance(self, &bytes[..i + 2]);
                        self.kitty.scan = Scan::Apc;
                        self.kitty.apc = Apc::default();
                        &bytes[i + 2..]
                    }
                    None if bytes.last() == Some(&ESC) => {
                        parser.advance(self, &bytes[..bytes.len() - 1]);
                        self.kitty.scan = Scan::Esc;
                        &[]
                    }
                    None => {
                        parser.advance(self, bytes);
                        &[]
                    }
                },
                Scan::Esc => {
                    self.kitty.scan = Scan::Ground;
                    if bytes[0] == b'_' {
                        parser.advance(self, b"\x1b_");
                        self.kitty.scan = Scan::Apc;
                        self.kitty.apc = Apc::default();
                        &bytes[1..]
                    } else {
                        parser.advance(self, b"\x1b");
                        bytes
                    }
                }
                Scan::Apc => match memchr::memchr3(ESC, CAN, SUB, bytes) {
                    Some(i) if bytes[i] == ESC => {
                        self.apc_push(&bytes[..i]);
                        self.kitty.scan = Scan::ApcEsc;
                        &bytes[i + 1..]
                    }
                    Some(i) => {
                        // CAN and SUB abort the APC; vte returns to ground on them too.
                        self.kitty.apc = Apc::default();
                        self.kitty.scan = Scan::Ground;
                        &bytes[i..]
                    }
                    None => {
                        self.apc_push(bytes);
                        &[]
                    }
                },
                Scan::ApcEsc => {
                    self.kitty.scan = Scan::Ground;
                    if bytes[0] == b'\\' {
                        parser.advance(self, b"\x1b\\");
                        self.apc_end();
                        &bytes[1..]
                    } else {
                        // Any other escape cancels the APC, as it does in vte.
                        self.kitty.apc = Apc::default();
                        parser.advance(self, b"\x1b");
                        bytes
                    }
                }
            }
        }
    }

    fn apc_push(&mut self, mut bytes: &[u8]) {
        let apc = &mut self.kitty.apc;
        if apc.ignored || bytes.is_empty() {
            return;
        }
        if !apc.in_payload {
            let end = memchr::memchr(b';', bytes);
            let control = &bytes[..end.unwrap_or(bytes.len())];
            if apc.control.len() + control.len() > MAX_CONTROL {
                apc.ignored = true;
                return;
            }
            apc.control.extend_from_slice(control);
            if apc.control.first().is_some_and(|&b| b != b'G') {
                apc.ignored = true;
                return;
            }
            let Some(end) = end else { return };
            if apc.control.is_empty() {
                apc.ignored = true;
                return;
            }
            bytes = &bytes[end + 1..];
            self.kitty.apc.in_payload = true;
            self.apc_command();
        }
        if let Some(loading) = &mut self.kitty.loading {
            loading.push(bytes);
        }
    }

    fn apc_end(&mut self) {
        let apc = &self.kitty.apc;
        if apc.ignored || apc.control.is_empty() {
            return;
        }
        if !apc.in_payload {
            self.apc_command();
        }
        let Some(control) = self.kitty.apc.command else {
            return;
        };
        self.kitty.apc = Apc::default();
        match self.kitty.loading.take() {
            Some(loading) if control.more => self.kitty.loading = Some(loading),
            Some(mut loading) => {
                loading.finish();
                if control.quiet != 0 {
                    loading.control.quiet = control.quiet;
                }
                self.transmitted(*loading);
            }
            None => self.graphics_command(control),
        }
    }

    /// The control part of the current APC is complete: start or continue a transmission.
    fn apc_command(&mut self) {
        let control = Control::parse(&self.kitty.apc.control[1..]);
        if !control.chunk_only {
            // A new command abandons a transmission whose last chunk never came.
            self.kitty.loading = None;
        }
        if self.kitty.loading.is_none() && matches!(control.action, b't' | b'T' | b'q') {
            let limit = match control.medium {
                b'f' | b't' | b's' => MAX_PATH,
                _ => self.images.quota,
            };
            self.kitty.loading = Some(Box::new(Transmission {
                control,
                data: Vec::new(),
                carry: Vec::new(),
                limit,
                error: control.invalid.then_some("EINVAL:bad control data"),
            }));
        }
        self.kitty.apc.command = Some(control);
    }

    fn graphics_command(&mut self, control: Control) {
        let result = match control.action {
            _ if control.invalid => Err("EINVAL:bad control data"),
            b'p' => self.kitty_put(&control),
            b'd' => return self.kitty_delete(&control),
            _ => Err("EINVAL:unsupported action"),
        };
        let id = *result.as_ref().unwrap_or(&control.id);
        self.kitty_reply(&control, id, result.map(|_| ()));
    }

    fn transmitted(&mut self, t: Transmission) {
        let mut control = t.control;
        let result = match t.error {
            Some(error) => Err(error),
            None => decode_transmission(&control, t.data, self.images.quota),
        };
        let image = match result {
            Ok(image) => image,
            Err(error) => return self.kitty_reply(&control, control.id, Err(error)),
        };
        if control.action == b'q' {
            return self.kitty_reply(&control, control.id, Ok(()));
        }
        let id = match control.id {
            0 => self.free_image_id(),
            id => id,
        };
        let mut result = self.add_image(id, image).map_err(image_error);
        if result.is_ok() && control.number != 0 {
            self.kitty.numbers.insert(control.number, id);
        }
        if result.is_ok() && control.action == b'T' {
            control.id = id;
            control.number = 0;
            result = self.kitty_put(&control).map(|_| ());
        }
        // An image sent without i or I gets an id, but no reply naming it.
        let reply_id = if t.control.id == 0 && t.control.number == 0 {
            0
        } else {
            id
        };
        self.kitty_reply(&t.control, reply_id, result);
    }

    fn free_image_id(&mut self) -> u32 {
        loop {
            // Counting down from the top keeps clear of the small ids clients pick.
            self.kitty.next_id = self.kitty.next_id.wrapping_sub(1);
            let id = self.kitty.next_id;
            if id != 0 && self.image(id).is_none() {
                return id;
            }
        }
    }

    fn resolve_image(&self, control: &Control) -> Result<u32, &'static str> {
        let id = match (control.id, control.number) {
            (0, 0) => return Err("EINVAL:no image id"),
            (0, n) => *self.kitty.numbers.get(&n).ok_or("ENOENT:no such image")?,
            (id, _) => id,
        };
        match self.image(id) {
            Some(_) => Ok(id),
            None => Err("ENOENT:no such image"),
        }
    }

    fn kitty_put(&mut self, control: &Control) -> Result<u32, &'static str> {
        let id = self.resolve_image(control)?;
        let (cell_w, cell_h) = self.cell_pixels;
        let s = control.source;
        let opts = PlaceOptions {
            placement_id: control.placement,
            cols: control.cols as usize,
            rows: control.rows as usize,
            x_offset: control.x_offset.min((cell_w as u32).saturating_sub(1)),
            y_offset: control.y_offset.min((cell_h as u32).saturating_sub(1)),
            source: (s != Rect::default()).then_some(s),
            z: control.z,
        };
        let placement = self.place_image(id, opts).map_err(image_error)?;
        if !control.keep_cursor {
            // Like kitty: the cursor ends on the image's last row, just past its right edge.
            for _ in 1..placement.rows.min(self.rows) {
                self.linefeed();
            }
            let col = self.cursor.col.saturating_add(placement.cols);
            self.cursor.col = col.min(self.cols - 1);
            self.cursor.pending_wrap = col >= self.cols;
        }
        Ok(id)
    }

    fn kitty_delete(&mut self, c: &Control) {
        let free = c.delete.is_ascii_uppercase();
        let rows = self.rows as i64;
        // 1-based cell coordinates, as the protocol sends them.
        let (x, y) = (c.source.x as usize, c.source.y as i64);
        let covers_col = |p: &crate::Placement, col: usize| p.col < col && col <= p.col + p.cols;
        let covers_row =
            |p: &crate::Placement, top: i64, row: i64| top < row && row <= top + p.rows as i64;
        let (cursor_row, cursor_col) = (self.cursor.row as i64 + 1, self.cursor.col + 1);
        let target = match c.delete.to_ascii_lowercase() {
            b'i' | b'n' => match self.resolve_image(c) {
                Ok(id) => Some(id),
                Err(_) => return,
            },
            _ => None,
        };
        let removed = self.delete_placements_where(|p, top| match c.delete.to_ascii_lowercase() {
            b'a' => top < rows && top + p.rows as i64 > 0,
            b'i' | b'n' => {
                Some(p.image_id) == target && (c.placement == 0 || p.placement_id == c.placement)
            }
            b'c' => covers_col(p, cursor_col) && covers_row(p, top, cursor_row),
            b'p' => covers_col(p, x) && covers_row(p, top, y),
            b'q' => covers_col(p, x) && covers_row(p, top, y) && p.z == c.z,
            b'x' => covers_col(p, x),
            b'y' => covers_row(p, top, y),
            b'z' => p.z == c.z,
            b'r' => (c.source.x..=c.source.y).contains(&p.image_id),
            _ => false,
        });
        if free {
            let mut ids: Vec<u32> = removed;
            ids.extend(target);
            if c.delete == b'R' {
                let range = c.source.x..=c.source.y;
                ids.extend(self.image_ids().into_iter().filter(|id| range.contains(id)));
            }
            ids.sort_unstable();
            ids.dedup();
            for id in ids {
                if !self.image_placed(id) {
                    self.delete_image(id);
                }
            }
        }
    }

    fn kitty_reply(&mut self, control: &Control, id: u32, result: Result<(), &'static str>) {
        let quiet = match result {
            Ok(()) => control.quiet >= 1,
            Err(_) => control.quiet >= 2,
        };
        if quiet || (id == 0 && control.number == 0) {
            return;
        }
        let mut keys = Vec::new();
        if id != 0 {
            keys.push(format!("i={id}"));
        }
        if control.number != 0 {
            keys.push(format!("I={}", control.number));
        }
        if control.placement != 0 {
            keys.push(format!("p={}", control.placement));
        }
        let message = result.err().unwrap_or("OK");
        let reply = format!("\x1b_G{};{message}\x1b\\", keys.join(","));
        self.responses.extend_from_slice(reply.as_bytes());
    }
}

/// The first `ESC _` in `bytes`.
fn find_apc(bytes: &[u8]) -> Option<usize> {
    static FINDER: LazyLock<memmem::Finder<'static>> =
        LazyLock::new(|| memmem::Finder::new(b"\x1b_"));
    FINDER.find(bytes)
}

fn image_error(error: ImageError) -> &'static str {
    match error {
        ImageError::InvalidSize => "EINVAL:bad image size",
        ImageError::TooLarge => "EINVAL:image too large",
        ImageError::OverQuota => "ENOSPC:image storage full",
        ImageError::NotFound => "ENOENT:no such image",
    }
}

fn decode_transmission(c: &Control, data: Vec<u8>, limit: usize) -> Result<Image, &'static str> {
    let data = match c.medium {
        b'd' => data,
        // One error for every refusal, so replies say little about files the client can't read.
        b'f' | b't' => read_file(&data, c.medium == b't', c.offset, c.size, limit)
            .map_err(|_| "EBADF:cannot read image file")?,
        _ => return Err("EINVAL:unsupported transmission medium"),
    };
    let bpp = match c.format {
        24 => 3,
        32 => 4,
        100 => 0,
        _ => return Err("EINVAL:unsupported format"),
    };
    let raw_len = if bpp == 0 {
        limit
    } else {
        if c.width == 0 || c.height == 0 {
            return Err("EINVAL:missing image size");
        }
        if c.width > MAX_IMAGE_SIDE || c.height > MAX_IMAGE_SIDE {
            return Err("EINVAL:image too large");
        }
        c.width as usize * c.height as usize * bpp
    };
    let data = if c.compressed {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(&data[..])
            .take(raw_len as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|_| "EINVAL:bad zlib data")?;
        if bpp == 0 && out.len() > limit {
            return Err("EFBIG:image data too large");
        }
        out
    } else {
        data
    };
    if bpp == 0 {
        return decode_png(&data, limit);
    }
    if data.len() < raw_len {
        return Err("ENODATA:not enough image data");
    }
    let pixels = match bpp {
        4 => {
            let mut data = data;
            data.truncate(raw_len);
            data
        }
        _ => rgb_to_rgba(&data[..raw_len]),
    };
    Ok(Image {
        width: c.width,
        height: c.height,
        pixels,
    })
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.as_chunks::<3>().0 {
        out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
    }
    out
}

fn decode_png(data: &[u8], limit: usize) -> Result<Image, &'static str> {
    const BAD: &str = "EBADPNG:cannot decode PNG";
    let mut decoder =
        png::Decoder::new_with_limits(std::io::Cursor::new(data), png::Limits { bytes: limit });
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|_| BAD)?;
    let (width, height) = reader.info().size();
    if width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return Err("EINVAL:image too large");
    }
    let mut buf = vec![0; reader.output_buffer_size().ok_or(BAD)?];
    let frame = reader.next_frame(&mut buf).map_err(|_| BAD)?;
    buf.truncate(frame.buffer_size());
    let pixels = match frame.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => rgb_to_rgba(&buf),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&[g, a]| [g, g, g, a])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 0xff]).collect(),
        png::ColorType::Indexed => return Err(BAD),
    };
    Ok(Image {
        width,
        height,
        pixels,
    })
}

/// Reads an image file named by a `t=f` or `t=t` transmission. Only regular files outside
/// `/proc`, `/sys` and `/dev` are read, at most `limit` bytes; `t=t` files must also be in
/// a temp directory, and are deleted afterwards.
#[cfg(unix)]
fn read_file(
    path: &[u8],
    temp: bool,
    offset: u32,
    size: u32,
    limit: usize,
) -> Result<Vec<u8>, &'static str> {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};

    let path = Path::new(std::ffi::OsStr::from_bytes(path));
    if !path.is_absolute() {
        return Err("EINVAL:path must be absolute");
    }
    let path = std::fs::canonicalize(path).map_err(|_| "ENOENT:no such file")?;
    if ["/proc", "/sys", "/dev"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return Err("EPERM:not an image file");
    }
    if temp {
        let in_temp = [std::env::temp_dir(), PathBuf::from("/tmp")]
            .iter()
            .filter_map(|dir| std::fs::canonicalize(dir).ok())
            .any(|dir| path.starts_with(dir));
        let marked = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy().contains(TEMP_MARKER));
        if !in_temp || !marked {
            return Err("EPERM:not a graphics temp file");
        }
    }
    // O_NONBLOCK so a FIFO swapped in after the check cannot hang the terminal.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(&path)
        .map_err(|_| "ENOENT:cannot open file")?;
    let meta = file.metadata().map_err(|_| "EBADF:cannot read file")?;
    if !meta.file_type().is_file() {
        return Err("EPERM:not a regular file");
    }
    let available = meta.len().saturating_sub(offset as u64);
    let len = match size {
        0 => available,
        n => (n as u64).min(available),
    };
    if len > limit as u64 {
        return Err("EFBIG:file too large");
    }
    let mut data = Vec::with_capacity(len as usize);
    file.seek(SeekFrom::Start(offset as u64))
        .and_then(|_| file.take(len).read_to_end(&mut data))
        .map_err(|_| "EBADF:cannot read file")?;
    if temp {
        let _ = std::fs::remove_file(&path);
    }
    Ok(data)
}

#[cfg(not(unix))]
fn read_file(_: &[u8], _: bool, _: u32, _: u32, _: usize) -> Result<Vec<u8>, &'static str> {
    Err("EINVAL:file transmission is not supported")
}

#[cfg(test)]
mod tests;
