//! Decoded images and where they are placed on the grid. The graphics protocols add images
//! and placements here; the renderer reads [`Terminal::visible_placements`].
//!
//! A placement is anchored to the cell it was put on and moves with the text: into
//! scrollback, through scroll regions, across resizes. It is dropped when its line leaves
//! scrollback or the cells under it are erased, except by a line editor redrawing its prompt
//! after a resize (until the next line feed); ED 2, RIS and kitty deletes always drop it.
//!
//! # Examples
//!
//! ```rust
//! # fn main() -> Result<(), term_core::ImageError> {
//! use term_core::{Image, PlaceOptions, Terminal};
//! let mut term = Terminal::new(80, 24);
//! term.add_image(1, Image { width: 1, height: 1, pixels: vec![255; 4] })?;
//! term.place_image(1, PlaceOptions::default())?;
//! assert_eq!(term.visible_placements().len(), 1);
//! # Ok(())
//! # }
//! ```

use super::Terminal;
use std::collections::HashMap;

/// Largest accepted width or height, in pixels.
pub const MAX_IMAGE_SIDE: u32 = 10_000;
/// Default limit on decoded pixel bytes held by the store.
pub const DEFAULT_IMAGE_QUOTA: usize = 256 << 20;

/// A decoded image: `pixels` is RGBA8, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Image width in pixels, validated by [`Terminal::add_image`].
    pub width: u32,
    /// Image height in pixels, validated by [`Terminal::add_image`].
    pub height: u32,
    /// Row-major straight-alpha RGBA8 data, exactly `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// A rectangle of an image, in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Horizontal pixel offset from the image's left edge.
    pub x: u32,
    /// Vertical pixel offset from the image's top edge.
    pub y: u32,
    /// Crop width in pixels; zero means remaining width when placing an image.
    pub width: u32,
    /// Crop height in pixels; zero means remaining height when placing an image.
    pub height: u32,
}

/// How to place an image at the cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlaceOptions {
    /// Placing again with the same non-zero id replaces that placement; 0 always adds one.
    pub placement_id: u32,
    /// Width in cells; zero fits the cropped image at the current cell size.
    pub cols: usize,
    /// Height in cells; zero fits the cropped image at the current cell size.
    pub rows: usize,
    /// Horizontal pixel offset inside the anchor cell.
    pub x_offset: u32,
    /// Vertical pixel offset inside the anchor cell.
    pub y_offset: u32,
    /// Part of the image to show; `None` shows all of it.
    pub source: Option<Rect>,
    /// Negative draws below text, zero or more above it; higher draws on top.
    pub z: i32,
}

/// A stored image's anchor, crop and scale; see the [module example](self).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Image-store key.
    pub image_id: u32,
    /// Protocol placement key; zero permits multiple anonymous placements.
    pub placement_id: u32,
    /// The anchor cell: an absolute line, as in [`crate::Point`].
    pub line: u64,
    /// Zero-based anchor column in cells.
    pub col: usize,
    /// Unclipped horizontal cell span.
    pub cols: usize,
    /// Unclipped vertical cell span.
    pub rows: usize,
    /// A bottom clip in cell rows, independent of the original span and source rectangle.
    /// ED 2 uses this to save only the visible part, even for stretched single-pixel images.
    pub clipped_rows: Option<usize>,
    /// Horizontal pixel offset within the anchor cell.
    pub x_offset: u32,
    /// Vertical pixel offset within the anchor cell.
    pub y_offset: u32,
    /// Clamped source rectangle in image pixels.
    pub source: Rect,
    /// Drawing order: negative below text, nonnegative above text.
    pub z: i32,
    /// Cell dimensions when placed, used to scale pixel-sized axes and offsets.
    pub cell_pixels: (f32, f32),
    /// Width was explicitly specified in cells rather than inferred from pixels.
    pub fixed_cols: bool,
    /// Height was explicitly specified in cells rather than inferred from pixels.
    pub fixed_rows: bool,
}

/// A placement that overlaps the viewport. `row` is the viewport row of its top edge and is
/// negative when the top is scrolled above the viewport.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VisiblePlacement {
    /// Stored placement with an absolute anchor line.
    pub placement: Placement,
    /// Signed viewport row of the top edge, in cells.
    pub row: isize,
}

impl Placement {
    /// Cell rows occupied after clipping, without changing the image's original scale.
    pub fn visible_rows(&self) -> usize {
        self.clipped_rows.unwrap_or(self.rows).min(self.rows)
    }
}

/// Why an image could not be stored or placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// Zero-sized, or `pixels` is not `width * height * 4` bytes.
    InvalidSize,
    /// Wider or taller than [`MAX_IMAGE_SIDE`].
    TooLarge,
    /// Does not fit in the quota even after evicting every image that is not visible.
    OverQuota,
    /// The requested image ID has not been stored or has been evicted.
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
    /// Stores decoded pixels, replacing this ID and removing its old placements.
    ///
    /// Evicts least-recently-used off-screen images to fit the quota. Visible
    /// images are protected. See the [`images` module example](crate::images).
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::TooLarge`] above [`MAX_IMAGE_SIDE`],
    /// [`ImageError::InvalidSize`] for empty or inconsistent pixel buffers, or
    /// [`ImageError::OverQuota`] when the image cannot fit beside visible images.
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

    /// Returns stored pixels, or `None` for an unknown or evicted ID.
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
    ///
    /// Clamps the source rectangle to the stored image and leaves the cursor
    /// unchanged. See the [`images` module example](crate::images).
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::NotFound`] if `image_id` is not stored.
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
            clipped_rows: None,
            x_offset: opts.x_offset,
            y_offset: opts.y_offset,
            source,
            z: opts.z,
            cell_pixels: (cell_w.max(1.0), cell_h.max(1.0)),
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

    /// Returns visible placements, also including `extra_rows` above the viewport.
    ///
    /// Results use the same stable drawing order as [`Self::visible_placements`].
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
                    && row + p.visible_rows() as i64 > -(extra_rows as i64);
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

    /// Sets the decoded-pixel budget in bytes, enforced on subsequent additions.
    ///
    /// Does not immediately evict existing images when the quota is reduced.
    pub fn set_image_quota(&mut self, bytes: usize) {
        self.images.quota = bytes;
    }

    /// The size of one cell in pixels, used to size images and for `CSI 14/16 t`.
    /// Existing placements keep their cell spans across cell size changes.
    pub fn set_cell_pixel_size(&mut self, width: f32, height: f32) {
        self.cell_pixels = (width, height);
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
            p.col < self.cols && row < rows && row + p.visible_rows() as i64 > top
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

    /// Crops primary placements at an absolute line before ED 2 saves the visible screen.
    /// Keep the original geometry: clipping the integer source rectangle would lose
    /// sub-pixel crops (e.g. half of a single-pixel image stretched over two rows).
    pub(crate) fn clip_primary_placements(&mut self, end: u64) {
        let mut changed = false;
        self.primary.placements.retain_mut(|p| {
            if p.line.saturating_add(p.visible_rows() as u64) <= end {
                return true;
            }
            changed = true;
            let rows = end.saturating_sub(p.line) as usize;
            p.clipped_rows = Some(rows);
            rows > 0
        });
        if changed {
            self.images.generation += 1;
        }
    }

    /// Drops placements on the active screen that cover any cell of rows `top..=bottom`,
    /// columns `from..to` (screen coordinates).
    pub(crate) fn erase_placements(&mut self, top: usize, bottom: usize, from: usize, to: usize) {
        if self.prompt_redraw && !self.alt_active {
            return;
        }
        let (history, base) = (self.history, self.placement_base(self.alt_active));
        let (top, bottom) = (top as i64, bottom as i64);
        let placements = &mut self.screen_mut().placements;
        let before = placements.len();
        placements.retain(|p| {
            let row = screen_row(p.line, base, history);
            let rows_hit = row <= bottom && row + p.visible_rows() as i64 > top;
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
