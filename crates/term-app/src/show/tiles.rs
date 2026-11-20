//! Tmux replays text cells, not passthroughs. Each tile has one stable Kitty image ID
//! shared by its virtual placement and all animation frames.
use super::*;
use std::fmt::Write as _;
use term_core::placeholder::{CHAR, DIACRITICS as MARK};

// Indexed color avoids tmux RGB quantization. The high ID byte uses a diacritic.
pub(super) const TILE_IDS: usize = 240 * 255;

/// Reserves a disjoint sequence across all `show` processes in one tmux server.
/// Both commands execute in one non-yielding tmux command list. Never recycle an
/// index: even an evicted tile may still have placeholders in pane history.
pub(super) fn reserve_tile_ids(count: usize) -> io::Result<usize> {
    let counter = "@trident-image-sequence";
    let increment = format!("#{{e|+|:#{{?{counter},#{{{counter}}},0}},{count}}}");
    let response = tmux(&[
        "set",
        "-sF",
        counter,
        &increment,
        ";",
        "display",
        "-p",
        "#{pid}:#{@trident-image-sequence}",
    ])
    .ok_or_else(|| io::Error::other("could not reserve image IDs in tmux"))?;
    let (pid, end) = response
        .trim()
        .split_once(':')
        .and_then(|(p, e)| Some((p.parse::<usize>().ok()?, e.parse::<usize>().ok()?)))
        .ok_or_else(|| io::Error::other("tmux did not return an image ID range"))?;
    if end > TILE_IDS || end < count {
        return Err(io::Error::other("tmux image ID range exhausted"));
    }
    Ok((pid % TILE_IDS + end - count) % TILE_IDS)
}

pub(super) struct TileGrid {
    bands: Vec<Vec<Tile>>,
}

struct Tile {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    cols: usize,
    rows: usize,
    low: usize,
    high: usize,
}

impl Tile {
    fn id(&self) -> u32 {
        ((self.high << 24) | self.low) as u32
    }

    fn compressed(&self, image: &Rgba) -> io::Result<Vec<u8>> {
        let stride = image.width as usize * 4;
        let actual_w = self
            .width
            .min((image.width as usize).saturating_sub(self.x));
        let actual_h = self
            .height
            .min((image.height as usize).saturating_sub(self.y));
        // Pad partial cells rather than stretching their pixels.
        let zeros = vec![0; self.width * 4];
        let mut z = ZlibEncoder::new(Vec::new(), Compression::fast());
        for y in self.y..self.y + actual_h {
            let off = y * stride + self.x * 4;
            z.write_all(&image.pixels[off..off + actual_w * 4])?;
            z.write_all(&zeros[..(self.width - actual_w) * 4])?;
        }
        for _ in actual_h..self.height {
            z.write_all(&zeros)?;
        }
        z.finish()
    }
}

impl TileGrid {
    pub(super) fn new(
        image: &Rgba,
        window: Window,
        reserve: impl FnOnce(usize) -> io::Result<usize>,
    ) -> io::Result<Self> {
        let cell_w = window.cell_w.max(1.0);
        let cell_h = window.cell_h.max(1.0);
        let cols = (image.width as f32 / cell_w).ceil() as usize;
        let rows = (image.height as f32 / cell_h).ceil() as usize;
        let tile_cols = MARK
            .len()
            .min((MAX_SIDE as f32 / cell_w).floor() as usize)
            .max(1);
        let tile_rows = MARK
            .len()
            .min((MAX_SIDE as f32 / cell_h).floor() as usize)
            .max(1);
        let mut next_id = reserve(cols.div_ceil(tile_cols) * rows.div_ceil(tile_rows))?;
        let mut bands = Vec::new();
        for top in (0..rows).step_by(tile_rows) {
            let nr = (rows - top).min(tile_rows);
            let mut band = Vec::new();
            for left in (0..cols).step_by(tile_cols) {
                let nc = (cols - left).min(tile_cols);
                let x = (left as f32 * cell_w).round() as usize;
                let y = (top as f32 * cell_h).round() as usize;
                let width =
                    (((left + nc) as f32 * cell_w).round() as usize - x).min(MAX_SIDE as usize);
                let height =
                    (((top + nr) as f32 * cell_h).round() as usize - y).min(MAX_SIDE as usize);
                let low = 16 + next_id % 240;
                let high = 1 + next_id / 240;
                next_id = (next_id + 1) % TILE_IDS;
                band.push(Tile {
                    x,
                    y,
                    width,
                    height,
                    cols: nc,
                    rows: nr,
                    low,
                    high,
                });
            }
            bands.push(band);
        }
        Ok(Self { bands })
    }

    pub(super) fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.bands.iter().flatten().map(Tile::id)
    }

    pub(super) fn send(
        &self,
        out: &mut impl Write,
        image: &Rgba,
        gap: Option<u32>,
        wait: &mut impl FnMut(),
    ) -> io::Result<()> {
        for tile in self.bands.iter().flatten() {
            let id = tile.id();
            let mut header = format!("f=32,o=z,q=2,i={id},s={},v={}", tile.width, tile.height);
            if gap.is_none() {
                write!(header, ",U=1,c={},r={}", tile.cols, tile.rows).unwrap();
            }
            send_chunks(out, &tile.compressed(image)?, &header, gap, true, wait)?;
        }
        Ok(())
    }

    // Send all passthroughs before drawing: tmux can drop them during a redraw.
    pub(super) fn write_cells(&self, out: &mut impl Write) -> io::Result<()> {
        out.write_all(b"\x1b[59m")?;
        for band in &self.bands {
            for row in &MARK[..band[0].rows] {
                for tile in band {
                    write!(out, "\x1b[38;5;{}m", tile.low)?;
                    for col in &MARK[..tile.cols] {
                        write!(out, "{CHAR}{row}{col}{}", MARK[tile.high])?;
                    }
                }
                out.write_all(b"\x1b[39m\r\n")?;
            }
        }
        Ok(())
    }
}
