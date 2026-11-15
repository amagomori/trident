//! Glyph atlases packed in shelves (rows of glyphs of similar height): a single-channel
//! page for text and an RGBA page for color glyphs, created when the first one is drawn.

use crate::font::{Fonts, Style};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub c: char,
    pub style: Style,
    /// Cells the glyph spans; 0 for a mark drawn over another glyph (see `Fonts::rasterize`).
    pub cells: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct AtlasGlyph {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub left: f32,
    pub top: f32,
    /// Lives on the color page and is drawn as is rather than tinted.
    pub color: bool,
}

/// Packs glyphs in shelves inside the current area of a page, then in the areas a page
/// gains when it grows.
#[derive(Debug)]
struct Shelves {
    /// x, y, width and height of the area being filled.
    area: [u32; 4],
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
    /// Areas left to fill once this one is full, the next one last.
    spare: Vec<[u32; 4]>,
}

impl Shelves {
    fn new(size: u32) -> Shelves {
        Shelves {
            area: [0, 0, size, size],
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
            spare: Vec::new(),
        }
    }

    /// Where a `w` x `h` glyph goes, or `None` if the page is full.
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        loop {
            let [x, y, width, height] = self.area;
            if self.shelf_x + w + 1 > width {
                self.shelf_y += self.shelf_height + 1;
                self.shelf_x = 0;
                self.shelf_height = 0;
            }
            if w < width && self.shelf_y + h < height {
                let at = (x + self.shelf_x, y + self.shelf_y);
                self.shelf_x += w + 1;
                self.shelf_height = self.shelf_height.max(h);
                return Some(at);
            }
            *self = Shelves {
                area: self.spare.pop()?,
                spare: std::mem::take(&mut self.spare),
                ..Shelves::new(0)
            };
        }
    }

    /// A page grown from `old` to `new` pixels square gains the area right of the old
    /// page, filled first, and the area below it.
    fn grow(&mut self, old: u32, new: u32) {
        self.spare.insert(0, [0, old, new, new - old]);
        self.spare.insert(1, [old, 0, new - old, old]);
    }
}

pub struct Page {
    pub size: u32,
    pub texture: wgpu::Texture,
    format: wgpu::TextureFormat,
    bytes_per_pixel: u32,
    /// The page doubles in size up to this rather than being cleared when it fills.
    max_size: u32,
    shelves: Shelves,
}

fn texture(device: &wgpu::Device, size: u32, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("glyph atlas"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

impl Page {
    fn new(device: &wgpu::Device, size: u32, max_size: u32, format: wgpu::TextureFormat) -> Page {
        let limit = device.limits().max_texture_dimension_2d;
        let size = size.min(limit);
        Page {
            size,
            texture: texture(device, size, format),
            format,
            bytes_per_pixel: format.block_copy_size(None).unwrap_or(1),
            max_size: max_size.min(limit),
            shelves: Shelves::new(size),
        }
    }

    fn clear(&mut self) {
        self.shelves = Shelves::new(self.size);
    }

    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        self.shelves.place(w, h)
    }

    /// Doubles the page, keeping the glyphs already on it, unless it is at its largest.
    fn grow(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> bool {
        let size = (self.size * 2).min(self.max_size);
        if size <= self.size {
            return false;
        }
        let texture = texture(device, size, self.format);
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            self.texture.as_image_copy(),
            texture.as_image_copy(),
            wgpu::Extent3d {
                width: self.size,
                height: self.size,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        self.shelves.grow(self.size, size);
        self.texture = texture;
        self.size = size;
        true
    }

    fn upload(&self, queue: &wgpu::Queue, (x, y): (u32, u32), w: u32, h: u32, data: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * self.bytes_per_pixel),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
}

pub struct Atlas {
    pub mono: Page,
    /// Created on the first color glyph. The renderer rebinds when `textures_changed` is
    /// set: a page was created or grew.
    pub color: Option<Page>,
    pub textures_changed: bool,
    srgb: bool,
    glyphs: HashMap<GlyphKey, Option<AtlasGlyph>>,
}

/// The atlas ran out of space at its largest; it has been cleared and the frame must be rebuilt.
pub struct AtlasFull;

impl Atlas {
    pub fn new(device: &wgpu::Device, srgb: bool) -> Atlas {
        Atlas {
            mono: Page::new(device, 2048, 8192, wgpu::TextureFormat::R8Unorm),
            color: None,
            textures_changed: false,
            srgb,
            glyphs: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.glyphs.clear();
        self.mono.clear();
        if let Some(page) = &mut self.color {
            page.clear();
        }
    }

    pub fn get(
        &mut self,
        key: GlyphKey,
        fonts: &mut Fonts,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Option<AtlasGlyph>, AtlasFull> {
        if let Some(glyph) = self.glyphs.get(&key) {
            return Ok(*glyph);
        }
        let Some(raster) = fonts.rasterize(key.c, key.style, key.cells as usize) else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        let page = if raster.color {
            let srgb = self.srgb;
            self.color.get_or_insert_with(|| {
                self.textures_changed = true;
                let format = if srgb {
                    wgpu::TextureFormat::Rgba8UnormSrgb
                } else {
                    wgpu::TextureFormat::Rgba8Unorm
                };
                Page::new(device, 1024, 4096, format)
            })
        } else {
            &mut self.mono
        };
        let (w, h) = (raster.width as u32, raster.height as u32);
        if w + 1 > page.size || h + 1 > page.size {
            self.glyphs.insert(key, None);
            return Ok(None);
        }
        let mut at = page.place(w, h);
        let mut grew = false;
        while at.is_none() && page.grow(device, queue) {
            grew = true;
            at = page.place(w, h);
        }
        let Some((x, y)) = at else {
            self.clear();
            return Err(AtlasFull);
        };
        page.upload(queue, (x, y), w, h, &raster.coverage);
        self.textures_changed |= grew;
        let glyph = AtlasGlyph {
            x: x as f32,
            y: y as f32,
            width: w as f32,
            height: h as f32,
            left: raster.left,
            top: raster.top,
            color: raster.color,
        };
        self.glyphs.insert(key, Some(glyph));
        Ok(Some(glyph))
    }
}

#[cfg(test)]
mod tests {
    use super::Shelves;

    #[test]
    fn grown_shelves_fill_the_new_area_without_overlapping() {
        let mut shelves = Shelves::new(64);
        let mut placed = Vec::new();
        while let Some(at) = shelves.place(15, 15) {
            placed.push(at);
        }
        assert_eq!(placed.len(), 16);
        shelves.grow(64, 128);
        while let Some(at) = shelves.place(15, 15) {
            placed.push(at);
        }
        assert_eq!(placed.len(), 64);
        for (i, a) in placed.iter().enumerate() {
            assert!(a.0 + 16 <= 128 && a.1 + 16 <= 128);
            for b in &placed[..i] {
                assert!(
                    a.0.abs_diff(b.0) >= 16 || a.1.abs_diff(b.1) >= 16,
                    "{a:?} {b:?}"
                );
            }
        }
    }
}
