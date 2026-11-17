//! Font loading, cell metrics and glyph rasterization through `ab_glyph`.
//!
//! System fallback fonts are copied on first use into private, immutable file
//! snapshots. Mapping those snapshots keeps unused font-collection pages out of
//! the resident set; later source-file changes cannot invalidate borrowed bytes.

use crate::builtin;
use crate::color;
use crate::fallback::{self, FontFile, SystemFonts};
use crate::family::FontFaces;
use ab_glyph::{
    Font, FontArc, FontRef, GlyphId, Outline, OutlineCurve, OutlinedGlyph, Point, PxScale,
    ScaleFont, point,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use term_core::grapheme;

/// The bundled regular face, also used by `terminal show` for SVG text.
pub static REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");
/// Family name of the bundled fonts.
pub const FAMILY: &str = "JetBrains Mono";
pub(crate) const BOLD: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Bold.ttf");
const ITALIC: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Italic.ttf");
const BOLD_ITALIC: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-BoldItalic.ttf");
/// The bundled faces, by [`Style`].
pub(crate) const BUNDLED: [&[u8]; 4] = [REGULAR, BOLD, ITALIC, BOLD_ITALIC];

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
    /// One byte of coverage per pixel, or straight-alpha RGBA when `color` is set.
    pub coverage: Vec<u8>,
    pub color: bool,
}

/// A loaded fallback font; `face` is set for fonts with color glyphs.
struct Fallback {
    font: FontArc,
    face: Option<ttf_parser::Face<'static>>,
    data: &'static [u8],
    index: u32,
}

/// Caches an immutable font snapshot for the process lifetime, once per path.
///
/// A private, unlinked copy keeps borrowed parsers valid through source-file
/// updates without retaining entire large font collections in heap memory.
pub(crate) fn read_cached(path: &Path) -> std::io::Result<&'static [u8]> {
    static FILES: OnceLock<Mutex<HashMap<std::path::PathBuf, &'static [u8]>>> = OnceLock::new();
    let mut files = FILES
        .get_or_init(Default::default)
        .lock()
        .expect("font snapshots");
    if let Some(data) = files.get(path) {
        return Ok(data);
    }
    let mut source = std::fs::File::open(path)?;
    let mut snapshot = tempfile::tempfile()?;
    std::io::copy(&mut source, &mut snapshot)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if snapshot.metadata()?.nlink() != 0 {
            return Err(std::io::Error::other("font snapshot is still linked"));
        }
    }
    // SAFETY: Only this function has a handle to the unlinked snapshot. Copying
    // is complete, and no writes or truncations occur after mapping. Dropping the
    // sole file handle below leaves only the read-only mapping, so modifications
    // to the source font cannot change these bytes or invalidate the mapping.
    let map = unsafe { memmap2::Mmap::map(&snapshot)? };
    drop(snapshot);
    let data: &'static [u8] = Box::leak(Box::new(map));
    files.insert(path.to_owned(), data);
    Ok(data)
}

impl Fallback {
    /// Whether `c` has a color glyph, an outline, or is meant to be blank here; fonts can
    /// map characters to glyphs in formats this renderer cannot read (such as Apple's `hvgl`).
    fn can_draw(&self, c: char) -> bool {
        let id = self.font.glyph_id(c);
        id != GlyphId(0)
            && (self.face.is_some()
                || c.is_whitespace()
                || self.font.outline(id).is_some_and(|o| !o.curves.is_empty()))
    }
}

fn load_fallback((path, index): &FontFile) -> Option<Fallback> {
    let data = read_cached(path).ok()?;
    let font = FontArc::new(FontRef::try_from_slice_and_index(data, *index).ok()?);
    let face = ttf_parser::Face::parse(data, *index)
        .ok()
        .filter(color::has_color);
    Some(Fallback {
        font,
        face,
        data,
        index: *index,
    })
}

pub struct Fonts {
    faces: FontFaces,
    system: SystemFonts,
    /// Fallback fonts by file, each loaded on first need (`None` if it failed to load).
    fallbacks: HashMap<FontFile, Option<Fallback>>,
    pub px: f32,
    pub metrics: CellMetrics,
}

/// ab_glyph scales by line height; convert from a font size in pixels per em.
fn scale(font: &FontArc, px: f32) -> PxScale {
    let units_per_em = font.units_per_em().unwrap_or(1000.0);
    PxScale::from(px * font.height_unscaled() / units_per_em)
}

/// Horizontal shear of synthesized italics, about 11 degrees.
const SLANT: f32 = 0.2;

/// Shears `outline` (in font units, y up) to the right around height `pivot`, so glyphs
/// lean without drifting out of their cells.
fn slant_outline(outline: &mut Outline, pivot: f32) {
    let shear = |p: &mut Point| p.x += (p.y - pivot) * SLANT;
    let mut min = point(f32::MAX, f32::MAX);
    let mut max = point(f32::MIN, f32::MIN);
    for curve in &mut outline.curves {
        let points: Vec<&mut Point> = match curve {
            OutlineCurve::Line(a, b) => vec![a, b],
            OutlineCurve::Quad(a, b, c) => vec![a, b, c],
            OutlineCurve::Cubic(a, b, c, d) => vec![a, b, c, d],
        };
        for p in points {
            shear(p);
            min = point(min.x.min(p.x), min.y.min(p.y));
            max = point(max.x.max(p.x), max.y.max(p.y));
        }
    }
    if min.x <= max.x {
        // ab_glyph keeps the top (largest y) in `min`, as pixel rows grow downwards.
        outline.bounds = ab_glyph::Rect {
            min: point(min.x, max.y),
            max: point(max.x, min.y),
        };
    }
}

/// Widens each row of a `width`-wide coverage map `strength` pixels to the right, taking
/// the strongest coverage in reach: a synthesized bold.
fn embolden_coverage(coverage: &[u8], width: usize, strength: usize) -> Vec<u8> {
    let wide = width + strength;
    let mut out = vec![0u8; wide * (coverage.len() / width)];
    for (row, out) in coverage.chunks_exact(width).zip(out.chunks_exact_mut(wide)) {
        for (x, v) in out.iter_mut().enumerate() {
            let from = x.saturating_sub(strength);
            *v = row[from..(x + 1).min(width)]
                .iter()
                .copied()
                .max()
                .unwrap_or(0);
        }
    }
    out
}

impl Fonts {
    pub fn new(faces: &FontFaces, px: f32) -> Fonts {
        let mut fonts = Fonts {
            faces: faces.clone(),
            system: SystemFonts::new(),
            fallbacks: HashMap::new(),
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
        fonts
    }

    /// The bundled JetBrains Mono.
    pub fn bundled(px: f32) -> Fonts {
        Fonts::new(&FontFaces::bundled(), px)
    }

    pub fn set_size(&mut self, px: f32) {
        self.px = px;
        let font = &self.faces.face(Style::Regular).font;
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

    /// The first system font that can draw `c`; with `text_only`, skipping color fonts.
    fn fallback_for(&mut self, c: char, emoji: bool, text_only: bool) -> Option<&Fallback> {
        let fallbacks = &mut self.fallbacks;
        let file = self.system.find(c, emoji, |file| {
            fallbacks
                .entry(file.clone())
                .or_insert_with(|| load_fallback(file))
                .as_ref()
                .is_some_and(|f| f.can_draw(c) && !(text_only && f.face.is_some()))
        })?;
        self.fallbacks.get(&file)?.as_ref()
    }

    /// Rasterizes `c` to fit `cells` cells; `None` for blank glyphs. `cells == 0` is a mark
    /// drawn over another glyph: one cell wide, from text fonts only (an opaque color glyph,
    /// such as the emoji keycap for U+20E3, would hide its base), and `None` if none has it.
    pub fn rasterize(&mut self, c: char, style: Style, cells: usize) -> Option<RasterizedGlyph> {
        let metrics = self.metrics;
        if builtin::covers(c) {
            return builtin::rasterize(c, &metrics, cells);
        }
        let px = self.px;
        let mark = cells == 0;
        let cells = cells.max(1);
        let face = self.faces.face(style);
        let (mut embolden, mut slant) = (face.embolden, face.slant);
        let emoji = !mark && fallback::prefers_emoji(c, cells);
        let font = if face.font.glyph_id(c) != GlyphId(0) && !emoji {
            face.font.clone()
        } else {
            (embolden, slant) = (false, false);
            match self.fallback_for(c, emoji, mark) {
                Some(Fallback {
                    face: Some(color_face),
                    ..
                }) => {
                    let target = color::Target {
                        width: metrics.width * cells as f32,
                        height: metrics.height,
                        baseline: metrics.baseline,
                        px,
                    };
                    let id = color_face.glyph_index(c)?;
                    if let Some(glyph) = color::rasterize(color_face, id, target) {
                        return Some(glyph);
                    }
                    self.fallback_for(c, emoji, mark)?.font.clone()
                }
                Some(f) => f.font.clone(),
                None if mark => return None,
                None => {
                    let face = self.faces.face(style);
                    (embolden, slant) = (face.embolden, face.slant);
                    face.font.clone()
                }
            }
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
        let glyph = id.with_scale_and_position(px_scale, point(0.0, 0.0));
        let outlined = if slant {
            let mut outline = font.outline(id)?;
            slant_outline(&mut outline, font.ascent_unscaled() / 3.0);
            OutlinedGlyph::new(glyph, outline, font.as_scaled(px_scale).scale_factor())
        } else {
            font.outline_glyph(glyph)?
        };
        let bounds = outlined.px_bounds();
        let mut width = bounds.width() as usize;
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
        if embolden {
            let strength = (px / 20.0).round().max(1.0) as usize;
            coverage = embolden_coverage(&coverage, width, strength);
            width += strength;
        }
        Some(RasterizedGlyph {
            width,
            height,
            left: bounds.min.x,
            top: metrics.baseline + bounds.min.y,
            coverage,
            color: false,
        })
    }

    /// An emoji sequence (ZWJ sequence, flag, skin tone) shaped by a color emoji font that
    /// draws one of its characters, when that font draws the whole sequence as one glyph.
    pub fn rasterize_sequence(&mut self, text: &str, cells: usize) -> Option<RasterizedGlyph> {
        let metrics = self.metrics;
        let px = self.px;
        // A text-default base (U+2764, U+261D) can resolve to a text font, so the color font
        // may only be found through a later character of the sequence.
        let mut tried = Vec::new();
        for c in text
            .chars()
            .filter(|&c| c as u32 >= 0x80 && c != '\u{200D}' && !grapheme::tag(c))
        {
            let Some(fallback) = self.fallback_for(c, true, false) else {
                continue;
            };
            let Some(face) = fallback.face.clone() else {
                continue;
            };
            if tried.contains(&fallback.data.as_ptr()) {
                continue;
            }
            tried.push(fallback.data.as_ptr());
            let (data, index) = (fallback.data, fallback.index);
            if let Some(glyph) = shape_sequence(text, cells, &face, data, index, metrics, px) {
                return Some(glyph);
            }
        }
        None
    }
}

/// `text` as one color glyph from `face`, if the font's shaping draws it as one.
fn shape_sequence(
    text: &str,
    cells: usize,
    face: &ttf_parser::Face<'static>,
    data: &'static [u8],
    index: u32,
    metrics: CellMetrics,
    px: f32,
) -> Option<RasterizedGlyph> {
    let font = harfrust::Font::new(data, index)?;
    let mut buffer = harfrust::Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    harfrust::shape(
        &harfrust::ShaperFont::new(&font),
        &mut buffer,
        harfrust::ShapeOptions::new(),
    )
    .ok()?;
    let mut drawn = buffer
        .glyph_infos()
        .iter()
        .zip(buffer.glyph_positions())
        .filter(|(_, p)| p.x_advance != 0);
    let (info, _) = drawn.next()?;
    if drawn.next().is_some() {
        return None;
    }
    let target = color::Target {
        width: metrics.width * cells.max(1) as f32,
        height: metrics.height,
        baseline: metrics.baseline,
        px,
    };
    color::rasterize(face, ttf_parser::GlyphId(info.glyph_id as u16), target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_font_bytes_survive_truncation_and_replacement() {
        let dir =
            std::env::temp_dir().join(format!("trident-font-snapshot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("font.ttf");
        std::fs::write(&path, REGULAR).unwrap();
        let data = read_cached(&path).unwrap();
        let face = FontRef::try_from_slice(data).unwrap();
        let a = face.glyph_id('A');
        std::fs::write(&path, []).unwrap();
        assert_eq!(data, REGULAR);
        assert_eq!(face.glyph_id('A'), a);
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, BOLD).unwrap();
        assert!(std::ptr::eq(data, read_cached(&path).unwrap()));
        assert_eq!(data, REGULAR);
        assert!(face.outline_glyph(a.with_scale(16.0)).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_font_reads_are_not_cached() {
        let path =
            std::env::temp_dir().join(format!("trident-font-retry-{}.ttf", std::process::id()));
        let _ = std::fs::remove_file(&path);
        assert!(read_cached(&path).is_err());
        std::fs::write(&path, BOLD).unwrap();
        assert_eq!(read_cached(&path).unwrap(), BOLD);
        std::fs::remove_file(path).unwrap();
    }

    fn fonts() -> Fonts {
        Fonts::bundled(16.0)
    }

    #[test]
    fn primary_font_glyphs_are_coverage() {
        let glyph = fonts().rasterize('A', Style::Regular, 1).expect("glyph");
        assert!(!glyph.color);
        assert_eq!(glyph.coverage.len(), glyph.width * glyph.height);
    }

    /// Needs a system emoji font (Apple Color Emoji, Noto Color Emoji); skipped without one.
    #[test]
    fn emoji_render_in_color_within_two_cells() {
        let mut fonts = fonts();
        if fonts
            .fallback_for('😀', true, false)
            .is_none_or(|f| f.face.is_none())
        {
            eprintln!("no color emoji font on this system; skipping");
            return;
        }
        let glyph = fonts.rasterize('😀', Style::Regular, 2).expect("emoji");
        assert!(glyph.color);
        assert_eq!(glyph.coverage.len(), glyph.width * glyph.height * 4);
        assert!(glyph.width as f32 <= fonts.metrics.width * 2.0 + 1.0);
        assert!(glyph.height as f32 <= fonts.metrics.height + 1.0);
        let colorful = glyph
            .coverage
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] > 128 && (p[0].abs_diff(p[1]) > 40 || p[1].abs_diff(p[2]) > 40));
        assert!(colorful, "emoji has no saturated pixels");
    }

    /// A keycap mark (U+20E3) only a color emoji font covers must not draw as an opaque
    /// emoji box over its base. Needs such a font (Noto Color Emoji); skipped without one.
    #[test]
    fn marks_never_draw_from_color_fonts() {
        let mut fonts = fonts();
        let keycap = '\u{20e3}';
        if fonts
            .fallback_for(keycap, false, false)
            .is_none_or(|f| f.face.is_none())
        {
            eprintln!("no color font draws U+20E3 on this system; skipping");
            return;
        }
        assert!(
            fonts
                .rasterize(keycap, Style::Regular, 1)
                .expect("keycap")
                .color
        );
        assert!(
            fonts
                .rasterize(keycap, Style::Regular, 0)
                .is_none_or(|g| !g.color)
        );
        let acute = fonts
            .rasterize('\u{301}', Style::Regular, 0)
            .expect("acute");
        assert!(!acute.color);
    }

    /// Needs a system font covering CJK; skipped without one.
    #[test]
    fn missing_glyphs_come_from_system_fonts() {
        let mut fonts = fonts();
        if fonts.fallback_for('中', false, false).is_none() {
            eprintln!("no CJK font on this system; skipping");
            return;
        }
        let glyph = fonts
            .rasterize('中', Style::Regular, 2)
            .expect("fallback glyph");
        assert!(!glyph.color);
        assert!(glyph.coverage.iter().any(|&c| c > 0));
    }

    /// Needs a color emoji font with ZWJ and flag sequences (Noto Color Emoji, Apple Color
    /// Emoji); skipped without one.
    #[test]
    fn emoji_sequences_shape_to_one_color_glyph() {
        let mut fonts = fonts();
        if fonts
            .fallback_for('\u{1f468}', true, false)
            .is_none_or(|f| f.face.is_none())
        {
            eprintln!("no color emoji font on this system; skipping");
            return;
        }
        for text in [
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}",
            "\u{1f1ef}\u{1f1f5}",
            "\u{1f44d}\u{1f3fd}",
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
        ] {
            let glyph = fonts.rasterize_sequence(text, 2).expect(text);
            assert!(glyph.color, "{text}");
            assert!(
                glyph.width as f32 <= fonts.metrics.width * 2.0 + 1.0,
                "{text}"
            );
        }
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        let family = fonts.rasterize_sequence(family, 2).unwrap();
        let man = fonts.rasterize('\u{1f468}', Style::Regular, 2).unwrap();
        assert_ne!(family.coverage, man.coverage);
        let scotland = "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}";
        let scotland = fonts.rasterize_sequence(scotland, 2).unwrap();
        let black_flag = fonts.rasterize('\u{1f3f4}', Style::Regular, 2).unwrap();
        assert_ne!(scotland.coverage, black_flag.coverage);
        // Two unrelated emoji are not one glyph.
        assert!(fonts.rasterize_sequence("\u{1f468}\u{1f600}", 2).is_none());
    }

    /// Needs a font with CJK Extension B (HanaMinB, Noto Serif CJK, SimSun-ExtB); skipped
    /// without one.
    #[test]
    fn cjk_extension_b_comes_from_a_fallback_font() {
        let mut fonts = fonts();
        if fonts.fallback_for('\u{20000}', false, false).is_none() {
            eprintln!("no CJK Extension B font on this system; skipping");
            return;
        }
        for c in ['\u{20000}', '\u{20001}', '\u{2a6d6}'] {
            let glyph = fonts.rasterize(c, Style::Regular, 2).expect("Ext-B glyph");
            assert!(glyph.coverage.iter().any(|&v| v > 0));
        }
    }
}
