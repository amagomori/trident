//! Incremental DEC Sixel decoder. Each data byte paints one six-dot column;
//! `$` rewinds the current band so a second color can paint over the first.

use crate::{Image, PlaceOptions, Terminal, colors::Rgb};

#[cfg(test)]
mod tests;

const REGISTERS: usize = 256;
const SIDE: usize = 4096;
type Pixel = [u8; 4];

fn opaque((r, g, b): Rgb) -> Pixel {
    [r, g, b, 255]
}

pub(super) struct Decoder {
    colors: [Pixel; REGISTERS],
    ink: Pixel,
    background: Pixel,
    command: u8,
    params: [u16; 5],
    param: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    scale: (usize, usize),
    // Rows grow independently: a wider band never forces earlier rows to be recopied.
    rows: Vec<Vec<Pixel>>,
    pixel_limit: usize,
    paint_budget: usize,
}

impl Decoder {
    pub(super) fn new(term: &Terminal, aspect: u16, transparent: bool) -> Self {
        let palette = &term.palette;
        let pixel_limit = (term.images.quota / 4).min(SIDE * SIDE);
        // VT340's initial color registers (RGB percentages).
        let mut colors = [[0, 0, 0, 255]; REGISTERS];
        for (slot, (r, g, b)) in colors.iter_mut().zip([
            (0, 0, 0),
            (20, 20, 80),
            (80, 13, 13),
            (20, 80, 20),
            (80, 20, 80),
            (20, 80, 80),
            (80, 80, 20),
            (53, 53, 53),
            (26, 26, 26),
            (33, 33, 60),
            (60, 26, 26),
            (33, 60, 33),
            (60, 33, 60),
            (33, 60, 60),
            (60, 60, 33),
            (80, 80, 80),
        ]) {
            *slot = rgb(r, g, b);
        }
        Self {
            colors,
            ink: opaque(palette.resolve(term.cursor.template.fg)),
            background: if transparent {
                [0; 4]
            } else {
                opaque(palette.resolve(term.cursor.template.bg))
            },
            command: 0,
            params: [0; 5],
            param: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            scale: (
                1,
                match aspect {
                    2 => 5,
                    3 | 4 => 3,
                    7..=9 => 1,
                    _ => 2,
                },
            ),
            rows: Vec::new(),
            pixel_limit,
            paint_budget: SIDE * SIDE * 8,
        }
    }

    pub(super) fn put(&mut self, byte: u8) -> Option<()> {
        if byte < b' ' {
            return Some(());
        }
        if self.command != 0 {
            match byte {
                b'0'..=b'9' => {
                    let p = &mut self.params[self.param];
                    *p = p.checked_mul(10)?.checked_add(u16::from(byte - b'0'))?;
                    return Some(());
                }
                b';' => {
                    self.param += 1;
                    return (self.param < self.params.len()).then_some(());
                }
                _ => {}
            }
        }
        let repeat = self.end_command()?;
        match byte {
            b'!' | b'#' | b'"' => {
                self.command = byte;
                self.params = [0; 5];
                self.param = 0;
            }
            b'$' => self.x = 0,
            b'-' => {
                self.x = 0;
                self.y += 6;
                if self.y > SIDE + 5 {
                    return None;
                }
            }
            b'?'..=b'~' => self.paint(byte - b'?', repeat)?,
            _ => {}
        }
        Some(())
    }

    fn end_command(&mut self) -> Option<usize> {
        match std::mem::take(&mut self.command) {
            b'!' if self.param == 0 => return Some(usize::from(self.params[0]).max(1)),
            b'#' => {
                let [n, space, a, b, c] = self.params;
                let color = self.colors.get_mut(usize::from(n))?;
                match (self.param, space) {
                    (0, _) => {}
                    (4, 1) if a <= 360 && b <= 100 && c <= 100 => *color = hls(a, b, c),
                    (4, 2) if a <= 100 && b <= 100 && c <= 100 => *color = rgb(a, b, c),
                    _ => return None,
                }
                self.ink = *color;
            }
            b'"' if self.param <= 3 => {
                let [pan, pad, w, h, _] = self.params.map(usize::from);
                let (pan, pad) = (pan.max(1), pad.max(1));
                self.scale = if pan >= pad {
                    (1, (pan + pad / 2) / pad)
                } else {
                    ((pad + pan / 2) / pan, 1)
                };
                self.grow(w, h)?;
            }
            0 => {}
            _ => return None,
        }
        Some(1)
    }

    fn grow(&mut self, width: usize, height: usize) -> Option<()> {
        self.width = self.width.max(width);
        self.height = self.height.max(height);
        let w = self.width.checked_mul(self.scale.0)?;
        let h = self.height.checked_mul(self.scale.1)?;
        (w <= SIDE && h <= SIDE && w * h <= self.pixel_limit).then_some(())
    }

    fn paint(&mut self, bits: u8, repeat: usize) -> Option<()> {
        let end = self.x + repeat;
        let dots = (u8::BITS - bits.leading_zeros()) as usize;
        self.grow(end, if dots == 0 { 0 } else { self.y + dots })?;
        // Repeated carriage returns can otherwise repaint a small canvas without bound.
        self.paint_budget = self
            .paint_budget
            .checked_sub(repeat * bits.count_ones() as usize)?;
        for bit in 0..dots {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let y = self.y + bit;
            if self.rows.len() <= y {
                self.rows.resize_with(y + 1, Vec::new);
            }
            let row = &mut self.rows[y];
            if row.len() < end {
                row.resize(end, self.background);
            }
            row[self.x..end].fill(self.ink);
        }
        self.x = end;
        Some(())
    }

    pub(super) fn finish(mut self) -> Option<Image> {
        self.end_command()?;
        if self.width == 0 {
            return None;
        }
        self.grow(0, 1)?;
        let width = self.width * self.scale.0;
        let height = self.height * self.scale.1;
        let mut pixels = Vec::with_capacity(width * height * 4);
        for y in 0..self.height {
            let start = pixels.len();
            let row = self.rows.get(y).map(Vec::as_slice).unwrap_or_default();
            for &pixel in row
                .iter()
                .chain(std::iter::repeat(&self.background))
                .take(self.width)
            {
                for _ in 0..self.scale.0 {
                    pixels.extend_from_slice(&pixel);
                }
            }
            for _ in 1..self.scale.1 {
                pixels.extend_from_within(start..start + width * 4);
            }
        }
        Some(Image {
            width: width as u32,
            height: height as u32,
            pixels,
        })
    }
}

fn rgb(r: u16, g: u16, b: u16) -> Pixel {
    let channel = |p| ((p * 255 + 50) / 100) as u8;
    [channel(r), channel(g), channel(b), 255]
}

fn hls(hue: u16, lightness: u16, saturation: u16) -> Pixel {
    // DEC hue starts at blue, not red.
    let h = f32::from((hue + 240) % 360) / 30.;
    let l = f32::from(lightness) / 100.;
    let a = f32::from(saturation) / 100. * l.min(1. - l);
    let channel = |n: f32| {
        let k = (n + h) % 12.;
        ((l - a * (k - 3.).min(9. - k).clamp(-1., 1.)) * 255.).round() as u8
    };
    [channel(0.), channel(8.), channel(4.), 255]
}

impl Terminal {
    pub(super) fn place_sixel(&mut self, image: Image) {
        let id = self.free_image_id();
        if self.add_image(id, image).is_ok()
            && let Ok(p) = self.place_image(id, PlaceOptions::default())
        {
            // Advance below the image unless doing so would evict its anchor.
            let rows = if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
                let history = if self.alt_active || self.scroll_top != 0 {
                    0
                } else {
                    self.max_scrollback
                };
                p.rows
                    .min((self.scroll_bottom - self.scroll_top).saturating_add(history))
            } else {
                p.rows
            };
            self.cursor.pending_wrap = false;
            for _ in 0..rows {
                self.linefeed();
            }
        }
    }

    pub(super) fn report_graphics(&mut self, item: u16, action: u16) {
        let reply = match (item, action) {
            (1, 1 | 4) => format!("\x1b[?1;0;{REGISTERS}S"),
            (2, 1) => {
                let (w, h) = self.cell_pixels;
                let w = ((self.cols as f32 * w) as usize).min(SIDE);
                let h = ((self.rows as f32 * h) as usize).min(SIDE);
                format!("\x1b[?2;0;{w};{h}S")
            }
            (2, 4) => format!("\x1b[?2;0;{SIDE};{SIDE}S"),
            (1 | 2, _) => format!("\x1b[?{item};2S"),
            _ => format!("\x1b[?{item};1S"),
        };
        self.responses.extend_from_slice(reply.as_bytes());
    }
}
