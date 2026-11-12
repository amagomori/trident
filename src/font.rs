//! Font loading, cell metrics and glyph rasterization (via ab_glyph), with fallback fonts
//! for characters the primary font lacks. Fonts are parsed lazily, so even large CJK
//! fallback fonts cost little memory beyond their file contents.

use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use std::path::{Path, PathBuf};

const REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
const BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf");

#[cfg(target_os = "macos")]
const FALLBACK_FONTS: &[&str] = &[
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Apple Symbols.ttf",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/AppleSDGothicNeo.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
];

#[cfg(not(target_os = "macos"))]
const FALLBACK_FONTS: &[&str] = &[
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
    "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
    "/usr/share/fonts/truetype/unifont/unifont.ttf",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Regular = 0,
    Bold = 1,
    Italic = 2,
    BoldItalic = 3,
}

impl Style {
    pub fn new(bold: bool, italic: bool) -> Style {
        match (bold, italic) {
            (false, false) => Style::Regular,
            (true, false) => Style::Bold,
            (false, true) => Style::Italic,
            (true, true) => Style::BoldItalic,
        }
    }
}

/// Pixel metrics of one terminal cell at the current size.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    /// Distance from the top of the cell to the baseline.
    pub baseline: f32,
    pub underline_offset: f32,
    pub stroke: f32,
}

pub struct RasterizedGlyph {
    pub width: usize,
    pub height: usize,
    /// Offset of the bitmap's top-left corner from the cell's top-left corner.
    pub left: f32,
    pub top: f32,
    pub coverage: Vec<u8>,
}

pub struct Fonts {
    faces: [FontArc; 4],
    /// Fallback fonts that exist on this system, each loaded on first need.
    fallbacks: Vec<(PathBuf, Option<Option<FontArc>>)>,
    pub px: f32,
    pub metrics: CellMetrics,
}

fn load(bytes: Vec<u8>) -> Option<FontArc> {
    ab_glyph::FontVec::try_from_vec_and_index(bytes, 0)
        .ok()
        .map(FontArc::new)
}

/// ab_glyph scales by line height; convert from a font size in pixels per em.
fn scale(font: &FontArc, px: f32) -> PxScale {
    let units_per_em = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(px * font.height_unscaled() / units_per_em)
}

impl Fonts {
    /// Uses the bundled JetBrains Mono unless `custom` points to a font file.
    pub fn new(custom: Option<&Path>, px: f32) -> Result<Fonts, String> {
        let faces = match custom {
            Some(path) => {
                let bytes = std::fs::read(path)
                    .map_err(|e| format!("cannot read font {}: {e}", path.display()))?;
                let font =
                    load(bytes).ok_or_else(|| format!("cannot parse font {}", path.display()))?;
                [font.clone(), font.clone(), font.clone(), font]
            }
            None => [REGULAR, BOLD, ITALIC, BOLD_ITALIC]
                .map(|b| FontArc::try_from_slice(b).expect("bundled font")),
        };
        let mut fonts = Fonts {
            faces,
            fallbacks: FALLBACK_FONTS
                .iter()
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .map(|p| (p, None))
                .collect(),
            px,
            metrics: CellMetrics {
                width: 0.0,
                height: 0.0,
                baseline: 0.0,
                underline_offset: 0.0,
                stroke: 1.0,
            },
        };
        fonts.set_size(px);
        Ok(fonts)
    }

    pub fn set_size(&mut self, px: f32) {
        self.px = px;
        let font = &self.faces[0];
        let scaled = font.as_scaled(scale(font, px));
        let width = scaled.h_advance(font.glyph_id('M')).round().max(1.0);
        let height = (scaled.ascent() - scaled.descent()).ceil().max(1.0);
        let baseline = scaled.ascent().round();
        let stroke = (px / 14.0).round().max(1.0);
        self.metrics = CellMetrics {
            width,
            height,
            baseline,
            underline_offset: (baseline + stroke).min(height - stroke),
            stroke,
        };
    }

    fn fallback_for(&mut self, c: char) -> Option<&FontArc> {
        let index = self.fallbacks.iter_mut().position(|(path, font)| {
            font.get_or_insert_with(|| std::fs::read(&*path).ok().and_then(load))
                .as_ref()
                .is_some_and(|f| f.glyph_id(c) != GlyphId(0))
        })?;
        self.fallbacks[index].1.as_ref()?.as_ref()
    }

    /// Rasterizes `c` to fit `cells` cells; `None` for blank glyphs.
    pub fn rasterize(&mut self, c: char, style: Style, cells: usize) -> Option<RasterizedGlyph> {
        let metrics = self.metrics;
        let px = self.px;
        let face = &self.faces[style as usize];
        let font = if face.glyph_id(c) != GlyphId(0) {
            face.clone()
        } else {
            let fallback = self.fallback_for(c).cloned();
            fallback.unwrap_or_else(|| self.faces[style as usize].clone())
        };
        let id = font.glyph_id(c);
        let mut px_scale = scale(&font, px);
        let advance = font.as_scaled(px_scale).h_advance(id);
        let available = metrics.width * cells as f32;
        // Shrink glyphs (usually from fallback fonts) that would spill out of their cells.
        if advance > available * 1.05 {
            px_scale.x *= available / advance;
            px_scale.y *= available / advance;
        }
        let outlined = font.outline_glyph(id.with_scale_and_position(px_scale, point(0.0, 0.0)))?;
        let bounds = outlined.px_bounds();
        let width = bounds.width() as usize;
        let height = bounds.height() as usize;
        if width == 0 || height == 0 {
            return None;
        }
        let mut coverage = vec![0u8; width * height];
        outlined.draw(|x, y, c| {
            if let Some(v) = coverage.get_mut(y as usize * width + x as usize) {
                *v = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        });
        Some(RasterizedGlyph {
            width,
            height,
            left: bounds.min.x,
            top: metrics.baseline + bounds.min.y,
            coverage,
        })
    }
}
