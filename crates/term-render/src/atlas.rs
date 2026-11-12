//! Glyph atlases packed in shelves (rows of glyphs of similar height): a single-channel
//! page for text and an RGBA page for color glyphs, created when the first one is drawn.

use crate::font::{Fonts, Style};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub c: char,
    pub style: Style,
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

pub struct Page {
    pub size: u32,
    pub texture: wgpu::Texture,
    bytes_per_pixel: u32,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
}

impl Page {
    fn new(device: &wgpu::Device, size: u32, format: wgpu::TextureFormat) -> Page {
        let size = size.min(device.limits().max_texture_dimension_2d);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
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
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Page {
            size,
            texture,
            bytes_per_pixel: format.block_copy_size(None).unwrap_or(1),
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
        }
    }

    fn clear(&mut self) {
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_height = 0;
    }

    /// Where a `w` x `h` glyph goes, or `None` if the page is full.
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.shelf_x + w + 1 > self.size {
            self.shelf_y += self.shelf_height + 1;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h + 1 > self.size {
            return None;
        }
        let at = (self.shelf_x, self.shelf_y);
        self.shelf_x += w + 1;
        self.shelf_height = self.shelf_height.max(h);
        Some(at)
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
    /// Created on the first color glyph; the renderer rebinds when `color_changed` is set.
    pub color: Option<Page>,
    pub color_changed: bool,
    srgb: bool,
    glyphs: HashMap<GlyphKey, Option<AtlasGlyph>>,
}

/// The atlas ran out of space; it has been cleared and the frame must be rebuilt.
pub struct AtlasFull;

impl Atlas {
    pub fn new(device: &wgpu::Device, srgb: bool) -> Atlas {
        Atlas {
            mono: Page::new(device, 2048, wgpu::TextureFormat::R8Unorm),
            color: None,
            color_changed: false,
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
                self.color_changed = true;
                let format = if srgb {
                    wgpu::TextureFormat::Rgba8UnormSrgb
                } else {
                    wgpu::TextureFormat::Rgba8Unorm
                };
                Page::new(device, 1024, format)
            })
        } else {
            &mut self.mono
        };
        let (w, h) = (raster.width as u32, raster.height as u32);
        if w + 1 > page.size || h + 1 > page.size {
            self.glyphs.insert(key, None);
            return Ok(None);
        }
        let Some((x, y)) = page.place(w, h) else {
            self.clear();
            return Err(AtlasFull);
        };
        page.upload(queue, (x, y), w, h, &raster.coverage);
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
