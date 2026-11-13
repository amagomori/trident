//! Decoded images and where they are placed on the grid. The graphics protocols add images
//! and placements here; the renderer reads [`Terminal::visible_placements`].
//!
//! A placement is anchored to the cell it was put on and moves with the text: into
//! scrollback, through scroll regions, across resizes. It is dropped when its line leaves
//! scrollback or the cells under it are erased.

use super::Terminal;
use std::collections::HashMap;

/// Largest accepted width or height, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 10_000;
/// Default limit on decoded pixel bytes held by the store.
pub const DEFAULT_IMAGE_QUOTA: usize = 256 << 20;

/// A decoded image: `pixels` is RGBA8, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// A rectangle of an image, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// How to place an image at the cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceOptions {
    /// Placing again with the same non-zero id replaces that placement; 0 always adds one.
    pub placement_id: u32,
    /// Cell span; 0 fits the (cropped) image at the current cell size.
    pub cols: usize,
    pub rows: usize,
    /// Offset of the image's top-left corner inside the anchor cell, in pixels.
    pub x_offset: u32,
    pub y_offset: u32,
    /// Part of the image to show; `None` shows all of it.
    pub source: Option<Rect>,
    /// Negative draws below text, zero or more above it; higher draws on top.
    pub z: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub image_id: u32,
    pub placement_id: u32,
    /// The anchor cell: an absolute line, as in [`crate::Point`].
    pub line: u64,
    pub col: usize,
    pub cols: usize,
    pub rows: usize,
    pub x_offset: u32,
    pub y_offset: u32,
    pub source: Rect,
    pub z: i32,
    /// The client gave the cell span: the image is scaled to fill it on that axis instead
    /// of being drawn at its pixel size.
    pub fixed_cols: bool,
    pub fixed_rows: bool,
}

/// A placement that overlaps the viewport. `row` is the viewport row of its top edge and is
/// negative when the top is scrolled above the viewport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisiblePlacement {
    pub placement: Placement,
    pub row: isize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Zero-sized, or `pixels` is not `width * height * 4` bytes.
    InvalidSize,
    /// Wider or taller than [`MAX_IMAGE_SIDE`].
    TooLarge,
    /// Does not fit in the quota even after evicting every image that is not visible.
    OverQuota,
    NotFound,
}

struct StoredImage {
    image: Image,
    last_used: u64,
    version: u64,
}

pub(crate) struct ImageStore {
    images: HashMap<u32, StoredImage>,
    bytes: usize,
    pub(crate) quota: usize,
    clock: u64,
    pub(crate) generation: u64,
}

impl ImageStore {
    pub(crate) fn new() -> ImageStore {
        ImageStore {
            images: HashMap::new(),
            bytes: 0,
            quota: DEFAULT_IMAGE_QUOTA,
            clock: 0,
            generation: 0,
        }
    }

    /// An empty store that keeps the settings of `self`, for RIS.
    pub(crate) fn reset(&self) -> ImageStore {
        ImageStore {
            quota: self.quota,
            generation: self.generation + 1,
            // Versions must not repeat across a reset, or a renderer could keep a texture
            // of the old image for a new one under the same id.
            clock: self.clock,
            ..ImageStore::new()
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

/// The number of `cell`-pixel cells `px` pixels after `offset` cover.
fn span(px: u32, offset: u32, cell: f32) -> usize {
    (((px + offset) as f32 / cell.max(1.0)).ceil() as usize).max(1)
}

/// Placements are stored with `line` relative to a per-screen base: absolute for the primary
/// screen, the screen row for the alternate one (which has no scrollback, so its rows keep
/// their place while the primary's history grows).
fn screen_row(stored: u64, base: u64, history: u64) -> i64 {
    (stored + base) as i64 - history as i64
}

impl Terminal {
    pub fn add_image(&mut self, id: u32, image: Image) -> Result<(), ImageError> {
        if image.width > MAX_IMAGE_SIDE || image.height > MAX_IMAGE_SIDE {
            return Err(ImageError::TooLarge);
        }
        let len = image.width as usize * image.height as usize * 4;
        if len == 0 || image.pixels.len() != len {
            return Err(ImageError::InvalidSize);
        }
        if len > self.images.quota {
            return Err(ImageError::OverQuota);
        }
        let visible = self.images_on_screen();
        let pinned: usize = self
            .images
            .images
            .iter()
            .filter(|(k, _)| **k != id && visible.contains(k))
            .map(|(_, s)| s.image.pixels.len())
            .sum();
        if pinned + len > self.images.quota {
            return Err(ImageError::OverQuota);
        }
        // Retransmitting an id replaces the image and drops its placements.
        self.delete_image(id);
        while self.images.bytes + len > self.images.quota {
            let visible = self.images_on_screen();
            let victim = self
                .images
                .images
                .iter()
                .filter(|(id, _)| !visible.contains(id))
                .min_by_key(|(_, img)| img.last_used)
                .map(|(&id, _)| id);
            match victim {
                Some(victim) => {
                    self.delete_image(victim);
                }
                None => return Err(ImageError::OverQuota),
            }
        }
        let last_used = self.images.tick();
        self.images.bytes += len;
        self.images.images.insert(
            id,
            StoredImage {
                image,
                last_used,
                version: last_used,
            },
        );
        self.images.generation += 1;
        Ok(())
    }

    pub fn image(&self, id: u32) -> Option<&Image> {
        self.images.images.get(&id).map(|s| &s.image)
    }

    /// The image with id `id` and a number that changes whenever that id is given new pixels,
    /// so a renderer can tell a cached copy is stale.
    pub fn image_with_version(&self, id: u32) -> Option<(&Image, u64)> {
        self.images.images.get(&id).map(|s| (&s.image, s.version))
    }

    /// Removes an image and all its placements on both screens.
    pub fn delete_image(&mut self, id: u32) -> bool {
        let Some(old) = self.images.images.remove(&id) else {
            return false;
        };
        self.images.bytes -= old.image.pixels.len();
        self.primary.placements.retain(|p| p.image_id != id);
        self.alternate.placements.retain(|p| p.image_id != id);
        self.images.generation += 1;
        true
    }

    /// Removes one placement from the active screen.
    pub fn delete_placement(&mut self, image_id: u32, placement_id: u32) -> bool {
        let placements = &mut self.screen_mut().placements;
        let before = placements.len();
        placements.retain(|p| (p.image_id, p.placement_id) != (image_id, placement_id));
        let removed = placements.len() != before;
        if removed {
            self.images.generation += 1;
        }
        removed
    }

    /// Removes the active screen's placements for which `remove` returns true, given the
    /// placement and the screen row of its top edge. Returns the images they showed.
    pub(crate) fn delete_placements_where(
        &mut self,
        mut remove: impl FnMut(&Placement, i64) -> bool,
    ) -> Vec<u32> {
        let (history, base) = (self.history, self.placement_base(self.alt_active));
        let mut removed = Vec::new();
        self.screen_mut().placements.retain(|p| {
            let hit = remove(p, screen_row(p.line, base, history));
            if hit {
                removed.push(p.image_id);
            }
            !hit
        });
        if !removed.is_empty() {
            self.images.generation += 1;
        }
        removed
    }

    /// The ids of all stored images.
    pub(crate) fn image_ids(&self) -> Vec<u32> {
        self.images.images.keys().copied().collect()
    }

    /// Whether either screen still shows image `id`.
    pub(crate) fn image_placed(&self, id: u32) -> bool {
        let placed = |s: &crate::Screen| s.placements.iter().any(|p| p.image_id == id);
        placed(&self.primary) || placed(&self.alternate)
    }

    /// Places an image with its top-left corner in the cursor cell, on the active screen.
    pub fn place_image(
        &mut self,
        image_id: u32,
        opts: PlaceOptions,
    ) -> Result<Placement, ImageError> {
        let clock = self.images.tick();
        let stored = self
            .images
            .images
            .get_mut(&image_id)
            .ok_or(ImageError::NotFound)?;
        stored.last_used = clock;
        let (width, height) = (stored.image.width, stored.image.height);
        let source = opts
            .source
            .map(|r| {
                let x = r.x.min(width);
                let y = r.y.min(height);
                let w = if r.width == 0 { width } else { r.width };
                let h = if r.height == 0 { height } else { r.height };
                Rect {
                    x,
                    y,
                    width: w.min(width - x),
                    height: h.min(height - y),
                }
            })
            .unwrap_or(Rect {
                x: 0,
                y: 0,
                width,
                height,
            });
        let (cell_w, cell_h) = self.cell_pixels;
        let base = self.placement_base(self.alt_active);
        let placement = Placement {
            image_id,
            placement_id: opts.placement_id,
            line: self.history + self.cursor.row as u64,
            col: self.cursor.col,
            cols: match opts.cols {
                0 => span(source.width, opts.x_offset, cell_w),
                n => n,
            },
            rows: match opts.rows {
                0 => span(source.height, opts.y_offset, cell_h),
                n => n,
            },
            x_offset: opts.x_offset,
            y_offset: opts.y_offset,
            source,
            z: opts.z,
            fixed_cols: opts.cols != 0,
            fixed_rows: opts.rows != 0,
        };
        let placements = &mut self.screen_mut().placements;
        if opts.placement_id != 0 {
            placements.retain(|p| (p.image_id, p.placement_id) != (image_id, opts.placement_id));
        }
        placements.push(Placement {
            line: placement.line - base,
            ..placement
        });
        self.images.generation += 1;
        Ok(placement)
    }

    /// Placements on the active screen that overlap the viewport, in drawing order (by `z`,
    /// then oldest first).
    pub fn visible_placements(&self) -> Vec<VisiblePlacement> {
        self.visible_placements_above(0)
    }

    pub fn visible_placements_above(&self, extra_rows: usize) -> Vec<VisiblePlacement> {
        let base = self.placement_base(self.alt_active);
        let top = self.history as i64 - self.display_offset as i64;
        let mut visible: Vec<_> = self
            .screen()
            .placements
            .iter()
            .filter_map(|p| {
                let line = p.line + base;
                let row = line as i64 - top;
                let on = p.col < self.cols
                    && row < self.rows as i64
                    && row + p.rows as i64 > -(extra_rows as i64);
                on.then_some(VisiblePlacement {
                    placement: Placement { line, ..*p },
                    row: row as isize,
                })
            })
            .collect();
        visible.sort_by_key(|v| v.placement.z);
        visible
    }

    /// Changes whenever images or placements are added, removed or moved. Scrolling the view
    /// does not change it; positions come from [`Terminal::visible_placements`].
    pub fn image_generation(&self) -> u64 {
        self.images.generation
    }

    /// Decoded pixel bytes held by the image store.
    pub fn image_bytes(&self) -> usize {
        self.images.bytes
    }

    pub fn set_image_quota(&mut self, bytes: usize) {
        self.images.quota = bytes;
    }

    /// The size of one cell in pixels, used to size images and for `CSI 14/16 t`.
    /// Placements the client did not size in cells are re-spanned to keep their pixel size.
    pub fn set_cell_pixel_size(&mut self, width: f32, height: f32) {
        if self.cell_pixels == (width, height) {
            return;
        }
        self.cell_pixels = (width, height);
        let mut changed = false;
        for p in self
            .primary
            .placements
            .iter_mut()
            .chain(self.alternate.placements.iter_mut())
        {
            let cols = if p.fixed_cols {
                p.cols
            } else {
                span(p.source.width, p.x_offset, width)
            };
            let rows = if p.fixed_rows {
                p.rows
            } else {
                span(p.source.height, p.y_offset, height)
            };
            changed |= (cols, rows) != (p.cols, p.rows);
            (p.cols, p.rows) = (cols, rows);
        }
        if changed {
            self.images.generation += 1;
        }
    }

    fn placement_base(&self, alt: bool) -> u64 {
        if alt { self.history } else { 0 }
    }

    /// Images with a placement on either screen's visible rows (or the scrolled-back view),
    /// which eviction must keep.
    fn images_on_screen(&self) -> Vec<u32> {
        let rows = self.rows as i64;
        let on = |p: &Placement, base: u64, top: i64| {
            let row = screen_row(p.line, base, self.history);
            p.col < self.cols && row < rows && row + p.rows as i64 > top
        };
        let primary_top = -(self.display_offset as i64);
        let primary = self
            .primary
            .placements
            .iter()
            .filter(|p| on(p, 0, primary_top));
        let alt = self
            .alternate
            .placements
            .iter()
            .filter(|p| on(p, self.history, 0));
        primary.chain(alt).map(|p| p.image_id).collect()
    }

    /// Moves placements anchored in screen rows `top..=bottom` by `delta` rows as those rows
    /// scroll, dropping the ones pushed out of the region. With `to_history` the scrolled-out
    /// rows go to scrollback, so the region keeps its absolute lines and the rows below it
    /// shift instead.
    pub(crate) fn scroll_placements(
        &mut self,
        top: usize,
        bottom: usize,
        delta: isize,
        to_history: bool,
    ) {
        let (history, base) = (self.history, self.placement_base(self.alt_active));
        let (top, bottom, delta) = (top as i64, bottom as i64, delta as i64);
        self.screen_mut().placements.retain_mut(|p| {
            let row = screen_row(p.line, base, history);
            if to_history {
                if row > bottom {
                    p.line = (p.line as i64 - delta) as u64;
                }
                return true;
            }
            if !(top..=bottom).contains(&row) {
                return true;
            }
            let moved = row + delta;
            p.line = (p.line as i64 + delta) as u64;
            (top..=bottom).contains(&moved)
        });
        self.images.generation += 1;
    }

    /// Drops placements on the active screen that cover any cell of rows `top..=bottom`,
    /// columns `from..to` (screen coordinates).
    pub(crate) fn erase_placements(&mut self, top: usize, bottom: usize, from: usize, to: usize) {
        let (history, base) = (self.history, self.placement_base(self.alt_active));
        let (top, bottom) = (top as i64, bottom as i64);
        let placements = &mut self.screen_mut().placements;
        let before = placements.len();
        placements.retain(|p| {
            let row = screen_row(p.line, base, history);
            let rows_hit = row <= bottom && row + p.rows as i64 > top;
            let cols_hit = p.col < to && p.col + p.cols > from;
            !(rows_hit && cols_hit)
        });
        if placements.len() != before {
            self.images.generation += 1;
        }
    }

    /// Drops primary placements anchored above `first_line`.
    pub(crate) fn drop_placements_before(&mut self, first_line: u64) {
        let placements = &mut self.primary.placements;
        let before = placements.len();
        placements.retain(|p| p.line >= first_line);
        if placements.len() != before {
            self.images.generation += 1;
        }
    }

    /// Keeps placements on their cells across a resize: `shifted` rows of a screen were
    /// removed from its top without going to scrollback, and rows from `rows` down are gone.
    pub(crate) fn resize_placements(&mut self, alt: bool, shifted: usize, rows: usize) {
        let base = self.placement_base(alt);
        let history = self.history;
        let screen = if alt {
            &mut self.alternate
        } else {
            &mut self.primary
        };
        if screen.placements.is_empty() {
            return;
        }
        screen.placements.retain_mut(|p| {
            let row = screen_row(p.line, base, history) - shifted as i64;
            p.line = (p.line as i64 - shifted as i64) as u64;
            (!alt || row >= 0) && row < rows as i64
        });
        self.images.generation += 1;
    }
}

#[cfg(test)]
mod tests;
