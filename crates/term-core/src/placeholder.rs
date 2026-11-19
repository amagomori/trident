//! Kitty Unicode placeholder cells: the text half of a virtual image placement.

/// A placeholder occupies one cell; its combining marks encode source coordinates.
pub const CHAR: char = '\u{10eeee}';

/// Combining marks for row, column and high ID byte, in [protocol order].
///
/// [protocol order]: https://sw.kovidgoyal.net/kitty/graphics-protocol/#unicode-placeholders
#[rustfmt::skip]
pub const DIACRITICS: &[char] = &[
    '\u{0305}', '\u{030d}', '\u{030e}', '\u{0310}', '\u{0312}', '\u{033d}', '\u{033e}', '\u{033f}',
    '\u{0346}', '\u{034a}', '\u{034b}', '\u{034c}', '\u{0350}', '\u{0351}', '\u{0352}', '\u{0357}',
    '\u{035b}', '\u{0363}', '\u{0364}', '\u{0365}', '\u{0366}', '\u{0367}', '\u{0368}', '\u{0369}',
    '\u{036a}', '\u{036b}', '\u{036c}', '\u{036d}', '\u{036e}', '\u{036f}', '\u{0483}', '\u{0484}',
    '\u{0485}', '\u{0486}', '\u{0487}', '\u{0592}', '\u{0593}', '\u{0594}', '\u{0595}', '\u{0597}',
    '\u{0598}', '\u{0599}', '\u{059c}', '\u{059d}', '\u{059e}', '\u{059f}', '\u{05a0}', '\u{05a1}',
    '\u{05a8}', '\u{05a9}', '\u{05ab}', '\u{05ac}', '\u{05af}', '\u{05c4}', '\u{0610}', '\u{0611}',
    '\u{0612}', '\u{0613}', '\u{0614}', '\u{0615}', '\u{0616}', '\u{0617}', '\u{0657}', '\u{0658}',
    '\u{0659}', '\u{065a}', '\u{065b}', '\u{065d}', '\u{065e}', '\u{06d6}', '\u{06d7}', '\u{06d8}',
    '\u{06d9}', '\u{06da}', '\u{06db}', '\u{06dc}', '\u{06df}', '\u{06e0}', '\u{06e1}', '\u{06e2}',
    '\u{06e4}', '\u{06e7}', '\u{06e8}', '\u{06eb}', '\u{06ec}', '\u{0730}', '\u{0732}', '\u{0733}',
    '\u{0735}', '\u{0736}', '\u{073a}', '\u{073d}', '\u{073f}', '\u{0740}', '\u{0741}', '\u{0743}',
    '\u{0745}', '\u{0747}', '\u{0749}', '\u{074a}', '\u{07eb}', '\u{07ec}', '\u{07ed}', '\u{07ee}',
    '\u{07ef}', '\u{07f0}', '\u{07f1}', '\u{07f3}', '\u{0816}', '\u{0817}', '\u{0818}', '\u{0819}',
    '\u{081b}', '\u{081c}', '\u{081d}', '\u{081e}', '\u{081f}', '\u{0820}', '\u{0821}', '\u{0822}',
    '\u{0823}', '\u{0825}', '\u{0826}', '\u{0827}', '\u{0829}', '\u{082a}', '\u{082b}', '\u{082c}',
    '\u{082d}', '\u{0951}', '\u{0953}', '\u{0954}', '\u{0f82}', '\u{0f83}', '\u{0f86}', '\u{0f87}',
    '\u{135d}', '\u{135e}', '\u{135f}', '\u{17dd}', '\u{193a}', '\u{1a17}', '\u{1a75}', '\u{1a76}',
    '\u{1a77}', '\u{1a78}', '\u{1a79}', '\u{1a7a}', '\u{1a7b}', '\u{1a7c}', '\u{1b6b}', '\u{1b6d}',
    '\u{1b6e}', '\u{1b6f}', '\u{1b70}', '\u{1b71}', '\u{1b72}', '\u{1b73}', '\u{1cd0}', '\u{1cd1}',
    '\u{1cd2}', '\u{1cda}', '\u{1cdb}', '\u{1ce0}', '\u{1dc0}', '\u{1dc1}', '\u{1dc3}', '\u{1dc4}',
    '\u{1dc5}', '\u{1dc6}', '\u{1dc7}', '\u{1dc8}', '\u{1dc9}', '\u{1dcb}', '\u{1dcc}', '\u{1dd1}',
    '\u{1dd2}', '\u{1dd3}', '\u{1dd4}', '\u{1dd5}', '\u{1dd6}', '\u{1dd7}', '\u{1dd8}', '\u{1dd9}',
    '\u{1dda}', '\u{1ddb}', '\u{1ddc}', '\u{1ddd}', '\u{1dde}', '\u{1ddf}', '\u{1de0}', '\u{1de1}',
    '\u{1de2}', '\u{1de3}', '\u{1de4}', '\u{1de5}', '\u{1de6}', '\u{1dfe}', '\u{20d0}', '\u{20d1}',
    '\u{20d4}', '\u{20d5}', '\u{20d6}', '\u{20d7}', '\u{20db}', '\u{20dc}', '\u{20e1}', '\u{20e7}',
    '\u{20e9}', '\u{20f0}', '\u{2cef}', '\u{2cf0}', '\u{2cf1}', '\u{2de0}', '\u{2de1}', '\u{2de2}',
    '\u{2de3}', '\u{2de4}', '\u{2de5}', '\u{2de6}', '\u{2de7}', '\u{2de8}', '\u{2de9}', '\u{2dea}',
    '\u{2deb}', '\u{2dec}', '\u{2ded}', '\u{2dee}', '\u{2def}', '\u{2df0}', '\u{2df1}', '\u{2df2}',
    '\u{2df3}', '\u{2df4}', '\u{2df5}', '\u{2df6}', '\u{2df7}', '\u{2df8}', '\u{2df9}', '\u{2dfa}',
    '\u{2dfb}', '\u{2dfc}', '\u{2dfd}', '\u{2dfe}', '\u{2dff}', '\u{a66f}', '\u{a67c}', '\u{a67d}',
    '\u{a6f0}', '\u{a6f1}', '\u{a8e0}', '\u{a8e1}', '\u{a8e2}', '\u{a8e3}', '\u{a8e4}', '\u{a8e5}',
    '\u{a8e6}', '\u{a8e7}', '\u{a8e8}', '\u{a8e9}', '\u{a8ea}', '\u{a8eb}', '\u{a8ec}', '\u{a8ed}',
    '\u{a8ee}', '\u{a8ef}', '\u{a8f0}', '\u{a8f1}', '\u{aab0}', '\u{aab2}', '\u{aab3}', '\u{aab7}',
    '\u{aab8}', '\u{aabe}', '\u{aabf}', '\u{aac1}', '\u{fe20}', '\u{fe21}', '\u{fe22}', '\u{fe23}',
    '\u{fe24}', '\u{fe25}', '\u{fe26}', '\u{10a0f}', '\u{10a38}', '\u{1d185}', '\u{1d186}', '\u{1d187}',
    '\u{1d188}', '\u{1d189}', '\u{1d1aa}', '\u{1d1ab}', '\u{1d1ac}', '\u{1d1ad}', '\u{1d242}', '\u{1d243}',
    '\u{1d244}',
];

use std::collections::{BTreeMap, HashMap};

use crate::{Cell, Color, Placement, Rect, Terminal, VisiblePlacement};

pub(crate) type VirtualGrids = HashMap<u32, BTreeMap<u32, (u32, u32)>>;

/// A horizontal run of placeholders can clip between pixels when an image is scaled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slice {
    /// Left, top, right and bottom of the sample rectangle in image pixels.
    pub source: [f32; 4],
    /// Same bounds in cell units, relative to the first cell of the run.
    pub destination: [f32; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Tile {
    pub image: u32,
    placement: u32,
    x: u32,
    y: u32,
}

fn color_id(color: Color) -> u32 {
    match color {
        Color::Indexed(i) => i.into(),
        Color::Rgb(r, g, b) => u32::from_be_bytes([0, r, g, b]),
        _ => 0,
    }
}

impl Terminal {
    fn placeholder_tile(&self, cell: &Cell, prev: Option<Tile>) -> Option<Tile> {
        let cluster = self.grapheme(cell);
        let marks = match cluster {
            Some(s) => s.strip_prefix(CHAR)?,
            None if cell.c == CHAR => "",
            _ => return None,
        };
        let mut marks = marks
            .chars()
            .map(|c| DIACRITICS.binary_search(&c).ok().map(|n| n as u32));
        let low = color_id(cell.fg);
        let placement = self.underline_color(cell).map_or(0, color_id);
        let prev = prev.filter(|p| p.image & 0xff_ffff == low && p.placement == placement);
        let y = marks
            .next()
            .unwrap_or_else(|| Some(prev.map_or(0, |p| p.y)))?;
        let prev = prev.filter(|p| p.y == y);
        let x = marks
            .next()
            .unwrap_or_else(|| Some(prev.map_or(0, |p| p.x + 1)))?;
        let high = marks
            .next()
            .unwrap_or_else(|| Some(prev.filter(|p| p.x + 1 == x).map_or(0, |p| p.image >> 24)))?;
        (high <= 255 && marks.next().is_none()).then_some(Tile {
            image: low | high << 24,
            placement,
            x,
            y,
        })
    }

    /// Resolve omitted coordinates from the left before coalescing adjacent cells.
    pub(crate) fn placeholder_runs(
        &self,
        cells: &[Cell],
        mut visit: impl FnMut(Tile, usize, usize),
    ) {
        let mut run = None;
        let mut prev = None;
        for (col, cell) in cells.iter().enumerate() {
            let tile = self.placeholder_tile(cell, prev);
            if prev.map(|p| Tile { x: p.x + 1, ..p }) != tile {
                if let Some((first, start)) = run.take() {
                    visit(first, start, col - start);
                }
                run = tile.map(|t| (t, col));
            }
            prev = tile;
        }
        if let Some((first, start)) = run {
            visit(first, start, cells.len() - start);
        }
    }

    pub(crate) fn append_placeholder_rows(&self, extra: usize, out: &mut Vec<VisiblePlacement>) {
        if self.images.virtuals.is_empty() {
            return;
        }
        let above = extra.min(self.scrollback_len().saturating_sub(self.display_offset));
        for row in -(above as isize)..self.rows as isize {
            let line = self.viewport_to_point_signed(row, 0).line;
            self.placeholder_runs(&self.line(line).cells, |tile, col, len| {
                if let Some(v) = self.placeholder_slice(tile, col, len, row, line) {
                    out.push(v);
                }
            });
        }
    }

    pub(crate) fn placeholder_slice(
        &self,
        tile: Tile,
        col: usize,
        len: usize,
        row: isize,
        line: u64,
    ) -> Option<VisiblePlacement> {
        let grids = self.images.virtuals.get(&tile.image)?;
        let (&placement_id, &(cols, rows)) = if tile.placement == 0 {
            grids.first_key_value()?
        } else {
            grids.get_key_value(&tile.placement)?
        };
        let image = self.image(tile.image)?;
        let (iw, ih) = (image.width as f32, image.height as f32);
        let (cw, ch) = (self.cell_pixels.0.max(1.), self.cell_pixels.1.max(1.));
        let cols = if cols == 0 {
            (iw / cw).ceil()
        } else {
            cols as f32
        };
        let rows = if rows == 0 {
            (ih / ch).ceil()
        } else {
            rows as f32
        };
        let scale = (cols * cw / iw).min(rows * ch / ih);
        // Letterbox the whole placement once, not each clipped run independently.
        let left = (cols * cw - iw * scale) * 0.5;
        let top = (rows * ch - ih * scale) * 0.5;
        let (x, y) = (tile.x as f32 * cw, tile.y as f32 * ch);
        let dx = x.max(left);
        let dy = y.max(top);
        let right = (x + len as f32 * cw).min(left + iw * scale);
        let bottom = (y + ch).min(top + ih * scale);
        if dx >= right || dy >= bottom {
            return None;
        }
        let source = [
            (dx - left) / scale,
            (dy - top) / scale,
            ((right - left) / scale).min(iw),
            ((bottom - top) / scale).min(ih),
        ];
        let slice = Slice {
            source,
            destination: [
                (dx - x) / cw,
                (dy - y) / ch,
                (right - x) / cw,
                (bottom - y) / ch,
            ],
        };
        let [x0, y0, x1, y1] = source;
        let placement = Placement {
            image_id: tile.image,
            placement_id,
            line,
            col,
            cols: len,
            rows: 1,
            clipped_rows: None,
            x_offset: 0,
            y_offset: 0,
            z: 0,
            source: Rect {
                x: x0 as u32,
                y: y0 as u32,
                width: x1.ceil() as u32 - x0 as u32,
                height: y1.ceil() as u32 - y0 as u32,
            },
            display_pixels: ((right - dx).ceil() as u32, (bottom - dy).ceil() as u32),
            cell_pixels: (cw, ch),
            fixed_cols: false,
            fixed_rows: false,
        };
        Some(VisiblePlacement {
            placement,
            row,
            slice: Some(slice),
        })
    }
}
