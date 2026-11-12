//! A single-channel glyph atlas packed in shelves (rows of glyphs of similar height).

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
}

pub struct Atlas {
    pub size: u32,
    pub texture: wgpu::Texture,
    glyphs: HashMap<GlyphKey, Option<AtlasGlyph>>,
    shelf_x: u32,
    shelf_y: u32,
    shelf_height: u32,
}

/// The atlas ran out of space; it has been cleared and the frame must be rebuilt.
pub struct AtlasFull;

impl Atlas {
    pub fn new(device: &wgpu::Device) -> Atlas {
        let size = 2048.min(device.limits().max_texture_dimension_2d);
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
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        Atlas {
            size,
            texture,
            glyphs: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_height: 0,
        }
    }

    pub fn clear(&mut self) {
        self.glyphs.clear();
        self.shelf_x = 0;
        self.shelf_y = 0;
        self.shelf_height = 0;
    }

    pub fn get(
        &mut self,
        key: GlyphKey,
        fonts: &mut Fonts,
        queue: &wgpu::Queue,
    ) -> Result<Option<AtlasGlyph>, AtlasFull> {
        if let Some(glyph) = self.glyphs.get(&key) {
            return Ok(*glyph);
        }
        let Some(raster) = fonts.rasterize(key.c, key.style, key.cells as usize) else {
            self.glyphs.insert(key, None);
            return Ok(None);
        };
        let (w, h) = (raster.width as u32, raster.height as u32);
        if w + 1 > self.size || h + 1 > self.size {
            self.glyphs.insert(key, None);
            return Ok(None);
        }
        if self.shelf_x + w + 1 > self.size {
            self.shelf_y += self.shelf_height + 1;
            self.shelf_x = 0;
            self.shelf_height = 0;
        }
        if self.shelf_y + h + 1 > self.size {
            self.clear();
            return Err(AtlasFull);
        }
        let (x, y) = (self.shelf_x, self.shelf_y);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &raster.coverage,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.shelf_x += w + 1;
        self.shelf_height = self.shelf_height.max(h);
        let glyph = AtlasGlyph {
            x: x as f32,
            y: y as f32,
            width: w as f32,
            height: h as f32,
            left: raster.left,
            top: raster.top,
        };
        self.glyphs.insert(key, Some(glyph));
        Ok(Some(glyph))
    }
}
