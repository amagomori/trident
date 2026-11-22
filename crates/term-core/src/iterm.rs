//! iTerm2's inline image protocol (<https://iterm2.com/documentation-images.html>).
//! Current imgcat uses MultipartFile/FilePart/FileEnd; older versions use a single File.
//! The WezTerm extension `doNotMoveCursor=1` keeps the cursor at its cell.

use crate::Terminal;
use crate::images::{Image, MAX_IMAGE_SIDE, PlaceOptions};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use std::io::Cursor;

const MAX_FILE: usize = 16 << 20;
const MAX_ENCODED: usize = MAX_FILE.div_ceil(3) * 4;
const MAX_HEADER: usize = 4096;
pub(crate) const MAX_OSC: usize = MAX_HEADER + MAX_ENCODED + b"File=:".len();
const MAX_PIXELS_BYTES: usize = 64 << 20;

#[derive(Default, Clone, Copy)]
enum Dimension {
    #[default]
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u32),
}

impl Dimension {
    fn parse(value: &str) -> Option<Self> {
        if value == "auto" {
            return Some(Self::Auto);
        }
        let (number, unit): (_, fn(u32) -> Self) = if let Some(n) = value.strip_suffix("px") {
            (n, Self::Pixels)
        } else if let Some(n) = value.strip_suffix('%') {
            (n, Self::Percent)
        } else {
            (value, Self::Cells)
        };
        let n = number.parse::<u32>().ok()?;
        (n > 0).then(|| unit(n))
    }

    fn pixels(self, cell: f64, count: usize) -> Option<f64> {
        match self {
            Self::Auto => None,
            Self::Cells(n) => Some(f64::from(n) * cell),
            Self::Pixels(n) => Some(f64::from(n)),
            Self::Percent(n) => Some(f64::from(n) / 100.0 * cell * count as f64),
        }
    }
}

#[derive(Default)]
struct Options {
    width: Dimension,
    height: Dimension,
    stretch: bool,
    do_not_move_cursor: bool,
    size: Option<usize>,
}

impl Options {
    fn parse(header: &[u8]) -> Option<Self> {
        if header.len() > MAX_HEADER {
            return None;
        }
        let mut opts = Self::default();
        let mut inline = false;
        for field in std::str::from_utf8(header).ok()?.split(';') {
            let (key, value) = field.split_once('=')?;
            match key {
                "inline" => inline = value == "1",
                "width" => opts.width = Dimension::parse(value)?,
                "height" => opts.height = Dimension::parse(value)?,
                "preserveAspectRatio" => opts.stretch = value == "0",
                "doNotMoveCursor" => opts.do_not_move_cursor = value == "1",
                "size" => opts.size = Some(value.parse().ok()?),
                _ => {}
            }
        }
        (inline && opts.size.unwrap_or_default() <= MAX_FILE).then_some(opts)
    }

    fn display_size(&self, term: &Terminal, image: &Image) -> Option<(u32, u32)> {
        let (cw, ch) = term.cell_pixels;
        let (cw, ch) = (f64::from(cw.max(1.0)), f64::from(ch.max(1.0)));
        let (iw, ih) = (f64::from(image.width), f64::from(image.height));
        let (width, height) = match (
            self.width.pixels(cw, term.cols),
            self.height.pixels(ch, term.rows),
        ) {
            (Some(w), Some(h)) if self.stretch => (w, h),
            (w, h) => {
                let scale = match (w, h) {
                    (Some(w), Some(h)) => (w / iw).min(h / ih),
                    (Some(w), None) => w / iw,
                    (None, Some(h)) => h / ih,
                    (None, None) => 1.0_f64.min(term.cols as f64 * cw / iw),
                };
                (iw * scale, ih * scale)
            }
        };
        // Bound the cell span and the cursor's linefeeds as well as the decoded raster.
        (width <= MAX_IMAGE_SIDE.into() && height <= MAX_IMAGE_SIDE.into())
            .then(|| (width.round().max(1.) as u32, height.round().max(1.) as u32))
    }
}

pub(crate) struct Transfer {
    opts: Options,
    encoded: Vec<u8>,
}

impl Terminal {
    pub(crate) fn iterm_file(&mut self, body: &[u8]) {
        if let Some(file) = body.strip_prefix(b"File=") {
            self.images.multipart = None;
            if let Some(colon) = memchr::memchr(b':', &file[..file.len().min(MAX_HEADER + 1)])
                && let Some(opts) = Options::parse(&file[..colon])
            {
                self.display_file(opts, &file[colon + 1..]);
            }
        } else if let Some(header) = body.strip_prefix(b"MultipartFile=") {
            self.images.multipart = Options::parse(header).map(|opts| {
                Box::new(Transfer {
                    opts,
                    encoded: Vec::new(),
                })
            });
        } else if let Some(part) = body.strip_prefix(b"FilePart=") {
            if let Some(transfer) = self.images.multipart.as_mut() {
                if part.len() <= MAX_ENCODED - transfer.encoded.len() {
                    transfer.encoded.extend_from_slice(part);
                } else {
                    self.images.multipart = None;
                }
            }
        } else if body == b"FileEnd" {
            if let Some(transfer) = self.images.multipart.take() {
                self.display_file(transfer.opts, &transfer.encoded);
            }
        } else if body.is_empty() {
            // The raw OSC parser reports an oversized or cancelled part as an empty body.
            self.images.multipart = None;
        }
    }

    fn display_file(&mut self, opts: Options, encoded: &[u8]) {
        if encoded.len() > MAX_ENCODED {
            return;
        }
        let Ok(file) = STANDARD.decode(encoded) else {
            return;
        };
        if file.len() > MAX_FILE || opts.size.is_some_and(|n| n != file.len()) {
            return;
        }
        let Some(image) = decode(&file, self.images.quota.min(MAX_PIXELS_BYTES)) else {
            return;
        };
        let Some(size) = opts.display_size(self, &image) else {
            return;
        };
        let id = self.free_image_id();
        if self.add_image(id, image).is_ok()
            && let Ok(placement) = self.place_image(
                id,
                PlaceOptions {
                    display_pixels: Some(size),
                    ..Default::default()
                },
            )
            && !opts.do_not_move_cursor
        {
            self.advance_image_cursor(&placement);
        }
    }
}

fn decode(bytes: &[u8], budget: usize) -> Option<Image> {
    let format = image::guess_format(bytes).ok()?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif
    ) {
        return None;
    }
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(budget as u64);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) * 4 > budget as u64
        || decoder.total_bytes() > budget as u64
    {
        return None;
    }
    let orientation = decoder.orientation().ok()?;
    let mut image = DynamicImage::from_decoder(decoder).ok()?;
    image.apply_orientation(orientation);
    Some(Image {
        width: image.width(),
        height: image.height(),
        pixels: image.into_rgba8().into_raw(),
    })
}

#[cfg(test)]
mod tests;
