//! Shape adjacent cells without changing the terminal's logical column order.
//! Shaping clusters carry column numbers, not UTF-8 offsets. Each cluster
//! starts at its grid column; only glyphs within it advance proportionally.

use crate::family::Face;
use crate::font::{CellMetrics, Fonts, Style};
use crate::{builtin, fallback};
use ab_glyph::{Font as _, GlyphId};
use harfrust::{Buffer, Feature, Script, ShapeOptions, ShaperFont, Tag};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use term_core::{Cell, Flags, Terminal, grapheme};
use unicode_bidi::{BidiClass, bidi_class};

const MAX_INPUT: usize = 256;
const MAX_RUNS: usize = 512;

#[derive(Copy, Clone, Eq, Hash, PartialEq)]
pub(crate) struct FaceKey(usize, u32, bool, bool);

impl Face {
    pub(crate) fn key(&self) -> FaceKey {
        FaceKey(
            self.font.font_data().as_ptr() as usize,
            self.index,
            self.embolden,
            self.slant,
        )
    }
}

#[derive(Debug)]
pub(crate) struct Positioned {
    pub id: GlyphId,
    pub x: f32,
    pub y: f32,
    pub factor: f32,
}

/// Keeps the rasterizer's immutable snapshot alive without copying its bytes.
struct FontBytes(ab_glyph::FontArc);
impl AsRef<[u8]> for FontBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.font_data()
    }
}

type Input = (char, u16);
type Layout = Option<Rc<[Positioned]>>;
type Layouts = HashMap<Box<[Input]>, Layout>;

#[derive(Default)]
pub(crate) struct RunCache {
    fonts: HashMap<FaceKey, Option<harfrust::Font>>,
    entries: HashMap<(FaceKey, u16, bool), Layouts>,
    count: usize,
    input: Vec<Input>,
}

impl RunCache {
    pub fn clear(&mut self) {
        self.entries.clear();
        self.count = 0;
    }

    fn layout(
        &mut self,
        face: &Face,
        cols: u16,
        ligatures: bool,
        metrics: CellMetrics,
        px: f32,
    ) -> Layout {
        let key = (face.key(), cols, ligatures);
        if let Some(value) = self.entries.get(&key).and_then(|m| m.get(&self.input[..])) {
            return value.clone();
        }
        if self.count == MAX_RUNS {
            self.clear();
        }
        let font = self
            .fonts
            .entry(face.key())
            .or_insert_with(|| {
                harfrust::Font::new(
                    harfrust::font::Blob::Shared(Arc::new(FontBytes(face.font.clone()))),
                    face.index,
                )
            })
            .as_ref()?;
        let value = layout(font, &self.input, cols, ligatures, metrics, px);
        self.entries
            .entry(key)
            .or_default()
            .insert(self.input.clone().into(), value.clone());
        self.count += 1;
        value
    }
}

fn layout(
    font: &harfrust::Font,
    input: &[Input],
    cols: u16,
    ligatures: bool,
    metrics: CellMetrics,
    px: f32,
) -> Layout {
    let mut buf = Buffer::new();
    for &(c, col) in input {
        buf.push(c as u32, col as u32);
    }
    buf.guess_segment_properties();
    let off = [b"liga", b"clig", b"calt"].map(|t| Feature::new(Tag::new(t), 0, ..));
    let options =
        if !ligatures && matches!(buf.script(), Some(Script::LATIN | Script::COMMON) | None) {
            ShapeOptions::new().features(&off)
        } else {
            ShapeOptions::new()
        };
    harfrust::shape(&ShaperFont::new(font), &mut buf, options).ok()?;
    let units = px / f32::from(font.units_per_em().max(1));
    let infos = buf.glyph_infos();
    let positions = buf.glyph_positions();
    let rtl = buf.direction() == harfrust::Direction::RightToLeft;
    let mut glyphs = Vec::with_capacity(infos.len());
    let mut i = 0;
    while i < infos.len() {
        let col = infos[i].cluster;
        let n = infos[i..].iter().take_while(|g| g.cluster == col).count();
        let end = if rtl {
            i.checked_sub(1).map(|j| infos[j].cluster)
        } else {
            infos.get(i + n).map(|g| g.cluster)
        }
        .unwrap_or(u32::from(cols));
        let room = end.checked_sub(col)? as f32 * metrics.width;
        let advance = positions[i..i + n]
            .iter()
            .map(|p| p.x_advance as f32 * units)
            .sum::<f32>();
        let factor = if advance > room * 1.05 {
            room / advance
        } else {
            1.0
        };
        let scale = units * factor;
        let mut pen = col as f32 * metrics.width;
        for (g, p) in infos[i..i + n].iter().zip(&positions[i..i + n]) {
            let id = GlyphId(u16::try_from(g.glyph_id).ok()?);
            if id.0 == 0 {
                return None;
            }
            glyphs.push(Positioned {
                id,
                x: pen + p.x_offset as f32 * scale,
                y: -(p.y_offset as f32) * scale,
                factor,
            });
            pen += p.x_advance as f32 * scale;
        }
        i += n;
    }
    Some(glyphs.into())
}

/// The cheap default path never looks up a shaping face for plain ASCII.
pub(crate) fn candidate(cell: &Cell, ligatures: bool) -> bool {
    cell.flags.contains(Flags::GRAPHEME) || (cell.c != ' ' && (ligatures || !cell.c.is_ascii()))
}

fn rendition(cell: &Cell) -> (Flags, term_core::Color, term_core::Color, u16) {
    let mut flags = cell.flags;
    flags.remove(Flags::GRAPHEME | Flags::WIDE);
    (flags, cell.fg, cell.bg, cell.hyperlink)
}

pub(crate) struct Run {
    pub cols: usize,
    pub face: Face,
    pub glyphs: Rc<[Positioned]>,
}

/// The longest compatible prefix starting at the first cell. A failure leaves
/// the existing single-cell, builtin and emoji rasterizers in charge.
pub(crate) fn run(
    term: &Terminal,
    cells: &[Cell],
    fonts: &mut Fonts,
    style: Style,
    ligatures: bool,
    same_highlight: impl Fn(usize, &Cell) -> bool,
) -> Option<Run> {
    let mut cols = 0;
    let mut face: Option<Face> = None;
    let mut script = None;
    let mut rtl = false;
    fonts.runs.input.clear();
    while let Some(cell) = cells.get(cols) {
        if !candidate(cell, ligatures)
            || (cols != 0
                && (rendition(cell) != rendition(&cells[0]) || !same_highlight(cols, cell)))
        {
            break;
        }
        let base = term.base_char(cell);
        // Neutral punctuation stays in logical order, not mirrored by an RTL run.
        let next_rtl = !base.is_ascii() && matches!(bidi_class(base), BidiClass::R | BidiClass::AL);
        if cols != 0 && next_rtl != rtl {
            break;
        }
        rtl = next_rtl;
        let width = if cell.flags.contains(Flags::WIDE) {
            2
        } else {
            1
        };
        let cluster = term.grapheme(cell);
        if cols + width > cells.len()
            || base.is_whitespace()
            || builtin::covers(base)
            || fallback::prefers_emoji(base, width)
            || cluster.is_some_and(|s| {
                grapheme::is_emoji_sequence(s) || grapheme::compose_hangul(s).is_some()
            })
        {
            break;
        }
        let next_script = harfrust::unicode::script(base as u32);
        if !matches!(next_script, Script::COMMON | Script::INHERITED) {
            if script.is_some_and(|s| s != next_script) {
                break;
            }
            script = Some(next_script);
        }
        let Some(next) = fonts.outline_face(base, style) else {
            break;
        };
        if face.as_ref().is_some_and(|f| f.key() != next.key()) {
            break;
        }
        if let Some(text) = cluster {
            if text.len() > MAX_INPUT * 4
                || text.chars().count() + fonts.runs.input.len() > MAX_INPUT
                || !text
                    .chars()
                    .all(|c| next.font.glyph_id(c).0 != 0 || matches!(c, '\u{200c}' | '\u{200d}'))
            {
                break;
            }
            fonts
                .runs
                .input
                .extend(text.chars().map(|c| (c, cols as u16)));
        } else {
            fonts.runs.input.push((base, cols as u16));
        }
        face = Some(next);
        cols += width;
        if fonts.runs.input.len() >= MAX_INPUT {
            break;
        }
    }
    let face = face?;
    if fonts.runs.input.len() < 2 {
        return None;
    }
    let glyphs = fonts
        .runs
        .layout(&face, cols as u16, ligatures, fonts.metrics, fonts.px)?;
    Some(Run { cols, face, glyphs })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Terminal {
        let mut term = Terminal::new(80, 2);
        term.advance(text.as_bytes());
        term
    }

    fn first(term: &Terminal, fonts: &mut Fonts, ligatures: bool) -> Option<Run> {
        run(
            term,
            &term.viewport_row(0).cells,
            fonts,
            Style::Regular,
            ligatures,
            |_, _| true,
        )
    }

    #[test]
    fn bundled_operators_are_opt_in_and_stay_on_grid() {
        let mut fonts = Fonts::bundled(18.);
        for op in ["->", "!=", "===", "<=>"] {
            let text = format!("{op}X");
            let term = row(&text);
            assert!(first(&term, &mut fonts, false).is_none());
            let r = first(&term, &mut fonts, true).unwrap();
            assert_eq!(r.cols, text.len());
            let ids: Vec<_> = r.glyphs.iter().map(|g| g.id).collect();
            let nominal: Vec<_> = text.chars().map(|c| r.face.font.glyph_id(c)).collect();
            assert_ne!(ids, nominal, "{op}");
            let last = r.glyphs.last().unwrap();
            assert_eq!(last.id, r.face.font.glyph_id('X'));
            assert_eq!(last.x, op.len() as f32 * fonts.metrics.width);
        }
    }

    #[test]
    fn combining_marks_are_shaped_without_programming_ligatures() {
        let mut fonts = Fonts::bundled(16.);
        let term = row("e\u{301}é");
        let r = first(&term, &mut fonts, false).unwrap();
        assert_eq!(r.cols, 2);
        assert_eq!(r.glyphs.len(), 2);
        assert_eq!(r.glyphs[0].id, r.face.font.glyph_id('é'));
        assert_eq!(r.glyphs[1].x, fonts.metrics.width);
    }

    #[test]
    fn arabic_fallback_uses_joining_forms_in_logical_cells() {
        let mut fonts = Fonts::bundled(16.);
        let term = row("بب");
        let Some(r) = first(&term, &mut fonts, false) else {
            eprintln!("no Arabic outline font; skipping");
            return;
        };
        assert_eq!(r.cols, 2);
        assert_eq!(r.glyphs.len(), 2);
        assert_ne!(r.glyphs[0].id, r.glyphs[1].id);
        assert_eq!(r.glyphs[0].x, fonts.metrics.width);
        assert_eq!(r.glyphs[1].x, 0.);
    }

    #[test]
    fn brackets_keep_their_logical_direction_around_arabic() {
        use crate::family::{FontFaces, FontSpec};

        let (faces, _) = FontFaces::resolve(&FontSpec {
            regular: Some("DejaVu Sans Mono".into()),
            ..FontSpec::default()
        })
        .unwrap();
        if faces.face(Style::Regular).font.glyph_id('ب').0 == 0 {
            return;
        }
        let mut fonts = Fonts::new(&faces, 16.);
        let term = row("(بب)");
        assert!(first(&term, &mut fonts, true).is_none());
        let inner = run(
            &term,
            &term.viewport_row(0).cells[1..],
            &mut fonts,
            Style::Regular,
            true,
            |_, _| true,
        )
        .unwrap();
        assert_eq!(inner.cols, 2);
        assert_ne!(inner.glyphs[0].id, inner.glyphs[1].id);
    }

    #[test]
    fn run_cache_reuses_layout_but_not_after_resize() {
        let mut fonts = Fonts::bundled(14.);
        let term = row("a->b");
        let a = first(&term, &mut fonts, true).unwrap();
        let b = first(&term, &mut fonts, true).unwrap();
        assert!(Rc::ptr_eq(&a.glyphs, &b.glyphs));
        fonts.set_size(24.);
        let c = first(&term, &mut fonts, true).unwrap();
        assert!(!Rc::ptr_eq(&a.glyphs, &c.glyphs));
        assert!(c.glyphs.last().unwrap().x > b.glyphs.last().unwrap().x);
    }

    #[test]
    fn runs_end_before_rendition_highlight_emoji_and_builtin_boundaries() {
        let mut fonts = Fonts::bundled(16.);
        for text in ["->\x1b[31m=>", "->\x1b[1m=>", "->👩‍💻", "->─", "->\r\n=>"] {
            assert_eq!(first(&row(text), &mut fonts, true).unwrap().cols, 2);
        }
        let term = row("===>");
        let clipped = run(
            &term,
            &term.viewport_row(0).cells,
            &mut fonts,
            Style::Regular,
            true,
            |col, _| col < 2,
        )
        .unwrap();
        assert_eq!(clipped.cols, 2);
    }
}
