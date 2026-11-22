//! Box drawing (U+2500–257F), block elements (U+2580–259F), Braille patterns
//! (U+2800–28FF) and powerline separators (U+E0B0–E0BF) drawn from the cell
//! metrics instead of the font.

use crate::font::{CellMetrics, RasterizedGlyph};

/// Whether `c` is drawn here rather than taken from a font.
pub fn covers(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{259F}' | '\u{2800}'..='\u{28FF}' | '\u{E0B0}'..='\u{E0BF}')
}

/// Draws `c` over exactly `cells` cells, or returns `None` for blank or unsupported glyphs.
pub fn rasterize(c: char, metrics: &CellMetrics, cells: usize) -> Option<RasterizedGlyph> {
    if c == '\u{2800}' || !covers(c) {
        return None;
    }
    let w = (metrics.width * cells.max(1) as f32).round().max(1.0) as i32;
    let h = metrics.height.round().max(1.0) as i32;
    let mut canvas = Canvas::new(w, h, metrics.stroke);
    let code = c as u32;
    match code {
        0x2500..=0x257F => canvas.box_drawing(code),
        0x2580..=0x259F => canvas.block(code),
        0x2800..=0x28FF => canvas.braille((code - 0x2800) as u8),
        _ => canvas.powerline(code),
    }
    Some(RasterizedGlyph {
        width: w as usize,
        height: h as usize,
        left: 0.0,
        top: 0.0,
        coverage: canvas
            .alpha
            .iter()
            .map(|a| (a.clamp(0.0, 1.0) * 255.0).round() as u8)
            .collect(),
        color: false,
    })
}

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// Arms of the line glyphs U+2500–254B and U+2550–256C (up, right, down, left).
/// Dashed lines (U+2504–250B, U+254C–254F) are drawn separately and list `[0; 4]` here.
#[rustfmt::skip]
const LINES: [[u8; 4]; 0x6D] = [
    [0,1,0,1], [0,2,0,2], [1,0,1,0], [2,0,2,0], // ─━│┃
    [0;4], [0;4], [0;4], [0;4], [0;4], [0;4], [0;4], [0;4], // ┄┅┆┇┈┉┊┋
    [0,1,1,0], [0,2,1,0], [0,1,2,0], [0,2,2,0], // ┌┍┎┏
    [0,0,1,1], [0,0,1,2], [0,0,2,1], [0,0,2,2], // ┐┑┒┓
    [1,1,0,0], [1,2,0,0], [2,1,0,0], [2,2,0,0], // └┕┖┗
    [1,0,0,1], [1,0,0,2], [2,0,0,1], [2,0,0,2], // ┘┙┚┛
    [1,1,1,0], [1,2,1,0], [2,1,1,0], [1,1,2,0], // ├┝┞┟
    [2,1,2,0], [2,2,1,0], [1,2,2,0], [2,2,2,0], // ┠┡┢┣
    [1,0,1,1], [1,0,1,2], [2,0,1,1], [1,0,2,1], // ┤┥┦┧
    [2,0,2,1], [2,0,1,2], [1,0,2,2], [2,0,2,2], // ┨┩┪┫
    [0,1,1,1], [0,1,1,2], [0,2,1,1], [0,2,1,2], // ┬┭┮┯
    [0,1,2,1], [0,1,2,2], [0,2,2,1], [0,2,2,2], // ┰┱┲┳
    [1,1,0,1], [1,1,0,2], [1,2,0,1], [1,2,0,2], // ┴┵┶┷
    [2,1,0,1], [2,1,0,2], [2,2,0,1], [2,2,0,2], // ┸┹┺┻
    [1,1,1,1], [1,1,1,2], [1,2,1,1], [1,2,1,2], // ┼┽┾┿
    [2,1,1,1], [1,1,2,1], [2,1,2,1], [2,1,1,2], // ╀╁╂╃
    [2,2,1,1], [1,1,2,2], [1,2,2,1], [2,2,1,2], // ╄╅╆╇
    [1,2,2,2], [2,1,2,2], [2,2,2,1], [2,2,2,2], // ╈╉╊╋
    [0;4], [0;4], [0;4], [0;4], // ╌╍╎╏
    [0,3,0,3], [3,0,3,0], [0,3,1,0], [0,1,3,0], // ═║╒╓
    [0,3,3,0], [0,0,1,3], [0,0,3,1], [0,0,3,3], // ╔╕╖╗
    [1,3,0,0], [3,1,0,0], [3,3,0,0], [1,0,0,3], // ╘╙╚╛
    [3,0,0,1], [3,0,0,3], [1,3,1,0], [3,1,3,0], // ╜╝╞╟
    [3,3,3,0], [1,0,1,3], [3,0,3,1], [3,0,3,3], // ╠╡╢╣
    [0,3,1,3], [0,1,3,1], [0,3,3,3], [1,3,0,3], // ╤╥╦╧
    [3,1,0,1], [3,3,0,3], [1,3,1,3], [3,1,3,1], // ╨╩╪╫
    [3,3,3,3], // ╬
];

/// Half lines U+2574–257F (up, right, down, left).
#[rustfmt::skip]
const HALVES: [[u8; 4]; 12] = [
    [0,0,0,1], [1,0,0,0], [0,1,0,0], [0,0,1,0], // ╴╵╶╷
    [0,0,0,2], [2,0,0,0], [0,2,0,0], [0,0,2,0], // ╸╹╺╻
    [0,2,0,1], [1,0,2,0], [0,1,0,2], [2,0,1,0], // ╼╽╾╿
];

const UP: usize = 0;
const RIGHT: usize = 1;
const DOWN: usize = 2;
const LEFT: usize = 3;

/// Samples per pixel side for anti-aliased shapes.
const SS: usize = 4;

/// One glyph's coverage, one value in 0..=1 per pixel.
struct Canvas {
    w: i32,
    h: i32,
    /// Light stroke width in pixels; heavy is twice that and double is three of them.
    light: i32,
    alpha: Vec<f32>,
}

impl Canvas {
    fn new(w: i32, h: i32, stroke: f32) -> Canvas {
        let light = (stroke.round() as i32).clamp(1, (w.min(h) / 3).max(1));
        Canvas {
            w,
            h,
            light,
            alpha: vec![0.0; (w * h) as usize],
        }
    }

    fn thickness(&self, weight: u8) -> i32 {
        match weight {
            HEAVY => self.light * 2,
            DOUBLE => self.light * 3,
            _ => self.light,
        }
    }

    /// The pixel range a stroke of `weight` takes across an axis of `len` pixels. Every
    /// glyph centres its strokes the same way, so neighbouring cells line up.
    fn span(&self, weight: u8, len: i32) -> (i32, i32) {
        let light = (len - self.light) / 2;
        let t = self.thickness(weight);
        let start = match weight {
            DOUBLE => light - self.light,
            HEAVY => (len - t) / 2,
            _ => light,
        };
        (start, start + t)
    }

    fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, a: f32) {
        for y in y0.max(0)..y1.min(self.h) {
            for x in x0.max(0)..x1.min(self.w) {
                let p = &mut self.alpha[(y * self.w + x) as usize];
                *p = p.max(a);
            }
        }
    }

    /// Fills a rectangle given along and across an axis: x/y for horizontal arms, y/x
    /// for vertical ones.
    fn fill_axis(&mut self, horizontal: bool, along: (i32, i32), across: (i32, i32)) {
        if horizontal {
            self.fill(along.0, across.0, along.1, across.1, 1.0);
        } else {
            self.fill(across.0, along.0, across.1, along.1, 1.0);
        }
    }

    /// Coverage of every pixel from `inside` evaluated on a grid of sample points.
    fn shape(&mut self, inside: impl Fn(f32, f32) -> bool) {
        for y in 0..self.h {
            for x in 0..self.w {
                let mut hits = 0;
                for sy in 0..SS {
                    for sx in 0..SS {
                        let px = x as f32 + (sx as f32 + 0.5) / SS as f32;
                        let py = y as f32 + (sy as f32 + 0.5) / SS as f32;
                        hits += usize::from(inside(px, py));
                    }
                }
                let p = &mut self.alpha[(y * self.w + x) as usize];
                *p = p.max(hits as f32 / (SS * SS) as f32);
            }
        }
    }

    fn box_drawing(&mut self, code: u32) {
        match code {
            0x2504..=0x250B => {
                let i = code - 0x2504;
                let dashes = if i < 4 { 3 } else { 4 };
                let weight = if i.is_multiple_of(2) { LIGHT } else { HEAVY };
                self.dashes(i % 4 < 2, weight, dashes);
            }
            0x254C..=0x254F => {
                let i = code - 0x254C;
                let weight = if i.is_multiple_of(2) { LIGHT } else { HEAVY };
                self.dashes(i < 2, weight, 2);
            }
            0x256D..=0x2570 => self.arc(code),
            0x2571..=0x2573 => self.diagonals(code),
            0x2574..=0x257F => self.lines(HALVES[(code - 0x2574) as usize]),
            _ => self.lines(LINES[(code - 0x2500) as usize]),
        }
    }

    fn lines(&mut self, arms: [u8; 4]) {
        let (w, h) = (self.w, self.h);
        let mut outer = Canvas::new(w, h, self.light as f32);
        let mut channel = Canvas::new(w, h, self.light as f32);
        for side in [UP, RIGHT, DOWN, LEFT] {
            let weight = arms[side];
            if weight == NONE {
                continue;
            }
            let horizontal = side == LEFT || side == RIGHT;
            let (len, across_len) = if horizontal { (w, h) } else { (h, w) };
            let perp = if horizontal {
                [arms[UP], arms[DOWN]]
            } else {
                [arms[LEFT], arms[RIGHT]]
            };
            let opposite = arms[(side + 2) % 4];
            let toward_start = side == LEFT || side == UP;
            // `near` is where the arm reaches into the cell, measured along its axis:
            // the end for arms from the left/top edge, the start for the others.
            let reach = |span: (i32, i32)| if toward_start { span.1 } else { span.0 };
            let along = |near: i32| if toward_start { (0, near) } else { (near, len) };
            if weight == DOUBLE {
                let start = if perp.contains(&DOUBLE) {
                    reach(self.span(DOUBLE, len))
                } else {
                    reach(self.span(LIGHT, len))
                };
                outer.fill_axis(horizontal, along(start), self.span(DOUBLE, across_len));
                let start = reach(self.span(LIGHT, len));
                channel.fill_axis(horizontal, along(start), self.span(LIGHT, across_len));
                continue;
            }
            let near = if perp == [NONE, NONE] {
                reach(self.span(weight, len))
            } else if perp == [DOUBLE, DOUBLE] && opposite == NONE {
                // Stop at the nearer of the two lines it meets.
                let (s, e) = self.span(DOUBLE, len);
                if toward_start {
                    s + self.light
                } else {
                    e - self.light
                }
            } else {
                let spans = perp
                    .iter()
                    .filter(|&&p| p != NONE)
                    .map(|&p| self.span(p, len));
                let start = spans.clone().map(|s| s.0).min().unwrap_or(0);
                let end = spans.map(|s| s.1).max().unwrap_or(len);
                if toward_start { end } else { start }
            };
            self.fill_axis(horizontal, along(near), self.span(weight, across_len));
        }
        for (i, a) in self.alpha.iter_mut().enumerate() {
            if outer.alpha[i] > 0.0 && channel.alpha[i] == 0.0 {
                *a = 1.0;
            }
        }
    }

    fn dashes(&mut self, horizontal: bool, weight: u8, count: i32) {
        let (len, across_len) = if horizontal {
            (self.w, self.h)
        } else {
            (self.h, self.w)
        };
        let across = self.span(weight, across_len);
        let period = len as f32 / count as f32;
        let gap = (period / 3.0).round().max(1.0);
        for i in 0..count {
            let start = (i as f32 * period + gap / 2.0).round() as i32;
            let end = ((i + 1) as f32 * period - gap / 2.0).round() as i32;
            self.fill_axis(horizontal, (start, end.max(start + 1)), across);
        }
    }

    /// ╭╮╯╰: a quarter circle joined to straight runs so both ends meet the light
    /// lines of the neighbouring cells exactly.
    fn arc(&mut self, code: u32) {
        let (right, down) = match code {
            0x256D => (true, true),
            0x256E => (false, true),
            0x256F => (false, false),
            _ => (true, false),
        };
        let t = self.light;
        let (x0, x1) = self.span(LIGHT, self.w);
        let (y0, y1) = self.span(LIGHT, self.h);
        let cx = (x0 + x1) as f32 / 2.0;
        let cy = (y0 + y1) as f32 / 2.0;
        // Keep at least one pixel of straight run at the edges.
        let room_x = if right { self.w as f32 - cx } else { cx } - 1.0;
        let room_y = if down { self.h as f32 - cy } else { cy } - 1.0;
        let r = room_x.min(room_y).max(t as f32 / 2.0).floor();
        let ox = if right { cx + r } else { cx - r };
        let oy = if down { cy + r } else { cy - r };
        let half = t as f32 / 2.0;
        self.shape(|x, y| {
            let in_quadrant =
                (if right { x <= ox } else { x >= ox }) && (if down { y <= oy } else { y >= oy });
            in_quadrant && ((x - ox).hypot(y - oy) - r).abs() <= half
        });
        let (ox, oy) = (ox.round() as i32, oy.round() as i32);
        let (w, h) = (self.w, self.h);
        if down {
            self.fill(x0, oy, x1, h, 1.0);
        } else {
            self.fill(x0, 0, x1, oy, 1.0);
        }
        if right {
            self.fill(ox, y0, w, y1, 1.0);
        } else {
            self.fill(0, y0, ox, y1, 1.0);
        }
    }

    fn diagonals(&mut self, code: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let half = self.light as f32 / 2.0;
        let norm = w.hypot(h);
        // Distances to the lines through opposite corners.
        let rising = move |x: f32, y: f32| (h * x + w * y - w * h).abs() / norm <= half;
        let falling = move |x: f32, y: f32| (h * x - w * y).abs() / norm <= half;
        match code {
            0x2571 => self.shape(rising),
            0x2572 => self.shape(falling),
            _ => self.shape(|x, y| rising(x, y) || falling(x, y)),
        }
    }

    fn block(&mut self, code: u32) {
        let (w, h) = (self.w, self.h);
        // Every partial block keeps at least one pixel and leaves one for its
        // complement, so ▉ and ▕ (or ▔ and ▇) tile the cell even when it is tiny.
        let eighth = |len: i32, n: i32| {
            let at = (len as f32 * n as f32 / 8.0).round() as i32;
            if n == 0 || n == 8 || len < 2 {
                at
            } else {
                at.clamp(1, len - 1)
            }
        };
        let eighth_x = |n: i32| eighth(w, n);
        let eighth_y = |n: i32| eighth(h, n);
        let (mx, my) = (eighth_x(4), eighth_y(4));
        let quads = |c: &mut Canvas, ul: bool, ur: bool, ll: bool, lr: bool| {
            for (on, x0, y0, x1, y1) in [
                (ul, 0, 0, mx, my),
                (ur, mx, 0, w, my),
                (ll, 0, my, mx, h),
                (lr, mx, my, w, h),
            ] {
                if on {
                    c.fill(x0, y0, x1, y1, 1.0);
                }
            }
        };
        match code {
            0x2580 => self.fill(0, 0, w, my, 1.0),
            0x2581..=0x2588 => {
                let n = (code - 0x2580) as i32;
                self.fill(0, eighth_y(8 - n), w, h, 1.0);
            }
            0x2589..=0x258F => {
                let n = (0x2590 - code) as i32;
                self.fill(0, 0, eighth_x(n), h, 1.0);
            }
            0x2590 => self.fill(mx, 0, w, h, 1.0),
            0x2591..=0x2593 => self.fill(0, 0, w, h, (code - 0x2590) as f32 / 4.0),
            0x2594 => self.fill(0, 0, w, eighth_y(1), 1.0),
            0x2595 => self.fill(eighth_x(7), 0, w, h, 1.0),
            0x2596 => quads(self, false, false, true, false),
            0x2597 => quads(self, false, false, false, true),
            0x2598 => quads(self, true, false, false, false),
            0x2599 => quads(self, true, false, true, true),
            0x259A => quads(self, true, false, false, true),
            0x259B => quads(self, true, true, true, false),
            0x259C => quads(self, true, true, false, true),
            0x259D => quads(self, false, true, false, false),
            0x259E => quads(self, false, true, true, false),
            _ => quads(self, false, true, true, true),
        }
    }

    fn braille(&mut self, mask: u8) {
        // In Unicode bit order: down the first three rows of each column,
        // then across the bottom row.
        let dots = [
            (0, 0),
            (0, 1),
            (0, 2),
            (1, 0),
            (1, 1),
            (1, 2),
            (0, 3),
            (1, 3),
        ];
        let side = (self.w.min(self.h / 2) / 3).max(1);
        for (bit, (col, row)) in dots.into_iter().enumerate() {
            if mask & (1 << bit) != 0 {
                // Round the origin so each square is centred in its 1/2 × 1/4 slot.
                let x = ((2 * col + 1) * self.w - 2 * side + 2) / 4;
                let y = ((2 * row + 1) * self.h - 4 * side + 4) / 8;
                self.fill(x, y, x + side, y + side, 1.0);
            }
        }
    }

    fn powerline(&mut self, code: u32) {
        let (w, h) = (self.w as f32, self.h as f32);
        let half = self.light as f32 / 2.0;
        // Distance from (x, y) to the segment (ax, ay)-(bx, by).
        let segment = |x: f32, y: f32, (ax, ay): (f32, f32), (bx, by): (f32, f32)| {
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            (x - ax - t * dx).hypot(y - ay - t * dy)
        };
        let mid = h / 2.0;
        // x of the diagonal from the top-left to the bottom-right corner at height y.
        let falling = move |y: f32| y / h * w;
        match code {
            // Solid triangles pointing right / left.
            0xE0B0 => self.shape(|x, y| x <= w * (1.0 - (y - mid).abs() / mid)),
            0xE0B2 => self.shape(|x, y| x >= w * (y - mid).abs() / mid),
            // Thin chevrons pointing right / left.
            0xE0B1 => self.shape(|x, y| {
                segment(x, y, (0.0, 0.0), (w, mid)).min(segment(x, y, (w, mid), (0.0, h))) <= half
            }),
            0xE0B3 => self.shape(|x, y| {
                segment(x, y, (w, 0.0), (0.0, mid)).min(segment(x, y, (0.0, mid), (w, h))) <= half
            }),
            // Solid half discs bulging right / left, and their outlines.
            0xE0B4..=0xE0B7 => {
                let right = code < 0xE0B6;
                let solid = code.is_multiple_of(2);
                let cx = if right { 0.0 } else { w };
                let ellipse = move |x: f32, y: f32, rx: f32, ry: f32| {
                    ((x - cx) / rx).powi(2) + ((y - mid) / ry).powi(2) <= 1.0
                };
                let t = self.light as f32;
                self.shape(|x, y| {
                    ellipse(x, y, w, mid)
                        && (solid || !ellipse(x, y, (w - t).max(0.5), (mid - t).max(0.5)))
                });
            }
            0xE0B8 => self.shape(|x, y| x <= falling(y)),
            0xE0BA => self.shape(|x, y| x >= w - falling(y)),
            0xE0BC => self.shape(|x, y| x < w - falling(y)),
            0xE0BE => self.shape(|x, y| x > falling(y)),
            0xE0B9 | 0xE0BF => self.shape(|x, y| segment(x, y, (0.0, 0.0), (w, h)) <= half),
            _ => self.shape(|x, y| segment(x, y, (0.0, h), (w, 0.0)) <= half),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Fonts;

    /// Cell metrics from the bundled font at sizes from tiny to high-DPI.
    fn sizes() -> Vec<CellMetrics> {
        [
            7.0, 9.0, 11.0, 13.0, 14.0, 15.5, 16.0, 19.0, 24.0, 28.0, 33.0, 48.0,
        ]
        .into_iter()
        .map(|px| Fonts::bundled(px).metrics)
        .collect()
    }

    fn nonblank() -> impl Iterator<Item = char> {
        ('\u{2500}'..='\u{259F}')
            .chain('\u{2801}'..='\u{28FF}')
            .chain('\u{E0B0}'..='\u{E0BF}')
    }

    struct Bitmap(RasterizedGlyph);

    impl Bitmap {
        fn of(c: char, m: &CellMetrics) -> Bitmap {
            Bitmap(rasterize(c, m, 1).expect("built-in glyph"))
        }
        fn at(&self, x: usize, y: usize) -> u8 {
            self.0.coverage[y * self.0.width + x]
        }
        fn column(&self, x: usize) -> Vec<u8> {
            (0..self.0.height).map(|y| self.at(x, y)).collect()
        }
        fn row(&self, y: usize) -> Vec<u8> {
            (0..self.0.width).map(|x| self.at(x, y)).collect()
        }
        /// The pixels on one edge: `UP`, `RIGHT`, `DOWN` or `LEFT`.
        fn edge(&self, side: usize) -> Vec<u8> {
            let (w, h) = (self.0.width, self.0.height);
            match side {
                UP => self.row(0),
                RIGHT => self.column(w - 1),
                DOWN => self.row(h - 1),
                _ => self.column(0),
            }
        }
    }

    /// Arms of every solid line glyph: the table entries plus the light arcs.
    fn arms() -> Vec<(char, [u8; 4])> {
        let lines = (0x2500..=0x256C)
            .filter(|c| !matches!(c, 0x2504..=0x250B | 0x254C..=0x254F))
            .map(|c| (c, LINES[(c - 0x2500) as usize]));
        let halves = (0x2574..=0x257F).map(|c| (c, HALVES[(c - 0x2574) as usize]));
        let arcs = [
            (0x256D, [0, 1, 1, 0]),
            (0x256E, [0, 0, 1, 1]),
            (0x256F, [1, 0, 0, 1]),
            (0x2570, [1, 1, 0, 0]),
        ];
        lines
            .chain(halves)
            .chain(arcs)
            .map(|(c, a)| (char::from_u32(c).unwrap(), a))
            .collect()
    }

    #[test]
    fn every_glyph_fills_exactly_its_cells() {
        for m in sizes() {
            for c in nonblank() {
                for cells in [1, 2] {
                    let g = rasterize(c, &m, cells).expect("built-in glyph");
                    assert_eq!(g.width, (m.width * cells as f32) as usize, "{c:?}");
                    assert_eq!(g.height, m.height as usize, "{c:?}");
                    assert_eq!((g.left, g.top, g.color), (0.0, 0.0, false));
                    assert_eq!(g.coverage.len(), g.width * g.height);
                    assert!(g.coverage.iter().any(|&a| a > 0), "{c:?} is blank");
                }
            }
        }
        assert!(rasterize('A', &sizes()[0], 1).is_none());
    }

    #[test]
    fn braille_ink_matches_unicode_dot_numbers() {
        let mut m = Fonts::bundled(14.0).metrics;
        // Read the bitmap spatially, not in the rasterizer's bit order.
        let numbers = [[1, 4], [2, 5], [3, 6], [7, 8]];
        for (w, h) in [(2, 4), (3, 5), (7, 13), (10, 20)] {
            m.width = w as f32;
            m.height = h as f32;
            for mask in 0..=255 {
                let c = char::from_u32(0x2800 + mask).unwrap();
                assert!(covers(c));
                if mask == 0 {
                    assert!(rasterize(c, &m, 1).is_none());
                    continue;
                }
                let glyph = Bitmap::of(c, &m);
                let mut observed = 0;
                for y in 0..h {
                    for x in 0..w {
                        if glyph.at(x, y) > 0 {
                            let dot = numbers[(2 * y + 1) * 2 / h][(2 * x + 1) / w];
                            observed |= 1 << (dot - 1);
                        }
                    }
                }
                assert_eq!(observed, mask, "{c:?} at {w}×{h}");
            }
        }
        for c in ['\u{27ff}', '\u{2900}'] {
            assert!(rasterize(c, &m, 1).is_none());
        }
    }

    /// Each arm reaches its edge with exactly the profile of the plain line of its
    /// weight, so it joins any neighbour with no gap or step; edges without an arm are
    /// empty.
    #[test]
    fn lines_join_their_neighbours_at_every_size() {
        for m in sizes() {
            let horizontal = ['\0', '─', '━', '═'].map(|c| (c != '\0').then(|| Bitmap::of(c, &m)));
            let vertical = ['\0', '│', '┃', '║'].map(|c| (c != '\0').then(|| Bitmap::of(c, &m)));
            for (c, arms) in arms() {
                let glyph = Bitmap::of(c, &m);
                for side in [UP, RIGHT, DOWN, LEFT] {
                    let edge = glyph.edge(side);
                    let weight = arms[side] as usize;
                    // Lines parallel to this edge run across the other axis; a double
                    // line needs a free pixel either side of it to keep off the edge.
                    let across = if side == LEFT || side == RIGHT {
                        m.width
                    } else {
                        m.height
                    };
                    let light = Canvas::new(m.width as i32, m.height as i32, m.stroke).light;
                    let roomy = across as i32 >= 3 * light + 2 || !arms.contains(&DOUBLE);
                    if !roomy {
                        continue;
                    }
                    let expected = match (weight, side) {
                        (0, _) => vec![0; edge.len()],
                        (_, LEFT | RIGHT) => horizontal[weight].as_ref().unwrap().edge(side),
                        _ => vertical[weight].as_ref().unwrap().edge(side),
                    };
                    assert_eq!(
                        edge, expected,
                        "{c:?} side {side} at {}x{}",
                        m.width, m.height
                    );
                }
            }
        }
    }

    #[test]
    fn plain_lines_are_solid_and_centred_the_same_way() {
        for m in sizes() {
            let light = Bitmap::of('─', &m);
            let rows: Vec<usize> = (0..m.height as usize)
                .filter(|&y| light.row(y).iter().all(|&a| a == 255))
                .collect();
            assert!(!rows.is_empty());
            assert!(rows.len() >= m.stroke as usize || rows.len() == 1);
            for y in 0..m.height as usize {
                let full = light.row(y).iter().all(|&a| a == 255);
                let empty = light.row(y).iter().all(|&a| a == 0);
                assert!(full || empty, "─ row {y} is partly covered");
            }
            // The double line's middle gap is where the light line runs.
            let double = Bitmap::of('═', &m);
            for &y in &rows {
                assert!(double.row(y).iter().all(|&a| a == 0));
            }
            assert!(double.row(rows[0] - 1).iter().all(|&a| a == 255));
            assert!(
                double
                    .row(rows[rows.len() - 1] + 1)
                    .iter()
                    .all(|&a| a == 255)
            );
        }
    }

    #[test]
    fn dashed_lines_stay_on_the_line_and_leave_gaps() {
        for m in sizes() {
            let solid = Bitmap::of('─', &m);
            for (c, dashes) in [('┄', 3), ('┈', 4), ('╌', 2)] {
                let g = Bitmap::of(c, &m);
                for y in 0..m.height as usize {
                    for x in 0..m.width as usize {
                        assert!(g.at(x, y) <= solid.at(x, y), "{c:?} off the line");
                    }
                }
                let y = (0..m.height as usize)
                    .find(|&y| solid.at(0, y) == 255)
                    .unwrap();
                let row = g.row(y);
                let runs = row.windows(2).filter(|p| p[0] == 0 && p[1] > 0).count()
                    + usize::from(row[0] > 0);
                if m.width as usize >= 2 * dashes {
                    assert_eq!(runs, dashes, "{c:?} at width {}", m.width);
                }
            }
        }
    }

    #[test]
    fn blocks_partition_the_cell() {
        for m in sizes() {
            let sum = |cs: &[char]| -> Vec<u32> {
                let mut total = vec![0u32; (m.width * m.height) as usize];
                for &c in cs {
                    for (t, &a) in total.iter_mut().zip(&Bitmap::of(c, &m).0.coverage) {
                        *t += a as u32;
                    }
                }
                total
            };
            let full = sum(&['█']);
            assert!(full.iter().all(|&a| a == 255));
            for parts in [
                &['▀', '▄'][..],
                &['▌', '▐'],
                &['▘', '▝', '▖', '▗'],
                &['▚', '▞'],
                &['▙', '▝'],
                &['▛', '▗'],
                &['▜', '▖'],
                &['▟', '▘'],
                &['▔', '▇'],
                &['▉', '▕'],
            ] {
                assert_eq!(sum(parts), full, "{parts:?} at {}x{}", m.width, m.height);
            }
            let covered = |c: char| {
                Bitmap::of(c, &m)
                    .0
                    .coverage
                    .iter()
                    .filter(|&&a| a > 0)
                    .count()
            };
            let lower: Vec<usize> = "▁▂▃▄▅▆▇█".chars().map(covered).collect();
            let left: Vec<usize> = "▏▎▍▌▋▊▉█".chars().map(covered).collect();
            assert!(lower.is_sorted() && left.is_sorted());
            let shades: Vec<u8> = "░▒▓".chars().map(|c| Bitmap::of(c, &m).at(0, 0)).collect();
            assert!(shades[0] > 0 && shades[0] < shades[1] && shades[1] < shades[2]);
            assert!(shades[2] < 255);
        }
    }

    #[test]
    fn powerline_separators_meet_the_cell_edges() {
        for m in sizes() {
            let (w, h) = (m.width as usize, m.height as usize);
            let right = Bitmap::of('\u{E0B0}', &m);
            // The base is the left edge; only the tapering corner rows are partial.
            assert!(right.column(0)[1..h - 1].iter().all(|&a| a == 255));
            assert!(right.at(w - 1, 0) == 0 && right.at(w - 1, h - 1) == 0);
            assert!(right.at(w - 1, h / 2) > 0);
            let left = Bitmap::of('\u{E0B2}', &m);
            assert!(left.column(w - 1)[1..h - 1].iter().all(|&a| a == 255));
            assert!(left.at(0, 0) == 0 && left.at(0, h - 1) == 0);
            // A solid separator and its mirror together cover the cell once.
            for (a, b) in [('\u{E0B8}', '\u{E0BE}'), ('\u{E0BA}', '\u{E0BC}')] {
                let (a, b) = (Bitmap::of(a, &m), Bitmap::of(b, &m));
                for y in 0..h {
                    for x in 0..w {
                        let total = a.at(x, y) as u32 + b.at(x, y) as u32;
                        assert!((250..=260).contains(&total), "{total} at {x},{y}");
                    }
                }
            }
            for c in ['\u{E0B4}', '\u{E0B6}'] {
                let disc = Bitmap::of(c, &m);
                let edge = if c == '\u{E0B4}' { 0 } else { w - 1 };
                assert_eq!(disc.at(edge, h / 2), 255);
            }
        }
    }

    #[test]
    fn fonts_draw_built_ins_at_cell_size() {
        let mut fonts = Fonts::bundled(16.0);
        let m = fonts.metrics;
        let g = fonts
            .rasterize('│', crate::font::Style::Bold, 1)
            .expect("glyph");
        assert_eq!((g.width, g.height), (m.width as usize, m.height as usize));
        assert_eq!((g.left, g.top), (0.0, 0.0));
    }
}
