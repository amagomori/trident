//! The configured font: a file or a family name, plus optional files or families for the
//! bold, italic and bold-italic faces. A style the font lacks is drawn from the closest face
//! it has, emboldened or slanted.

use crate::fallback;
use crate::font::{self, Style};
use ab_glyph::{FontArc, FontRef, FontVec};
use std::path::Path;
use std::rc::Rc;

/// The `font`, `font_bold`, `font_italic` and `font_bold_italic` settings, each a font file
/// path or a family name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontSpec {
    /// Regular font file/family, or `None` for the bundled family.
    pub regular: Option<String>,
    /// Bold override, or `None` to choose from the regular family.
    pub bold: Option<String>,
    /// Italic override, or `None` to choose from the regular family.
    pub italic: Option<String>,
    /// Bold-italic override, or `None` to choose from the regular family.
    pub bold_italic: Option<String>,
}

/// Interpretation of a font setting; see [`Self::of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    File(&'a Path),
    Family(&'a str),
}

impl<'a> Source<'a> {
    /// A file if `value` is one or looks like a path or font file name; otherwise a family
    /// (or full face) name.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use term_render::FontSource;
    /// assert!(matches!(FontSource::of("/fonts/mono.ttf"), FontSource::File(_)));
    /// assert_eq!(FontSource::of("JetBrains Mono"), FontSource::Family("JetBrains Mono"));
    /// ```
    pub fn of(value: &'a str) -> Source<'a> {
        let path = Path::new(value);
        let font_file = path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["ttf", "otf", "ttc", "otc"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        });
        if font_file
            || value.contains('/')
            || value.contains(std::path::MAIN_SEPARATOR)
            || path.is_file()
        {
            Source::File(path)
        } else {
            Source::Family(value)
        }
    }
}

/// A face to draw one style with: `embolden` and `slant` synthesize a style it lacks.
#[derive(Clone)]
pub(crate) struct Face {
    pub font: FontArc,
    pub index: u32,
    pub embolden: bool,
    pub slant: bool,
}

/// The faces for each [`Style`], with synthesis for missing styles.
#[derive(Clone)]
pub struct FontFaces([Face; 4]);

impl FontFaces {
    /// Returns the bundled `JetBrains Mono` faces.
    pub fn bundled() -> FontFaces {
        FontFaces(font::BUNDLED.map(|data| Face {
            font: FontArc::try_from_slice(data).expect("bundled font"),
            index: 0,
            embolden: false,
            slant: false,
        }))
    }

    pub(crate) fn face(&self, style: Style) -> &Face {
        &self.0[style as usize]
    }

    /// Finds the faces `spec` names, with warnings for those it could not find. A family
    /// that is not installed falls back to the bundled font, and a missing style override
    /// to the style from `font`; only an unreadable `font` file is an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the regular font file cannot be read or has no usable
    /// faces. Failed style overrides are returned as warnings instead.
    pub fn resolve(spec: &FontSpec) -> Result<(FontFaces, Vec<String>), String> {
        let mut warnings = Vec::new();
        let mut faces = match spec.regular.as_deref().map(Source::of) {
            None => FontFaces::bundled(),
            Some(Source::File(path)) => from_file(path)?,
            Some(Source::Family(name)) => match from_system(name) {
                Some(faces) => faces,
                None => {
                    if !name.eq_ignore_ascii_case(font::FAMILY) {
                        warnings.push(format!(
                            "font family \"{name}\" not found; using the bundled {}",
                            font::FAMILY
                        ));
                    }
                    FontFaces::bundled()
                }
            },
        };
        for (style, key, value) in [
            (Style::Bold, "font_bold", &spec.bold),
            (Style::Italic, "font_italic", &spec.italic),
            (Style::BoldItalic, "font_bold_italic", &spec.bold_italic),
        ] {
            let Some(value) = value else { continue };
            let found = match Source::of(value) {
                Source::File(path) => from_file(path),
                Source::Family(name) => {
                    from_system(name).ok_or_else(|| format!("font family \"{name}\" not found"))
                }
            };
            match found {
                Ok(found) => faces.0[style as usize] = found.0[style as usize].clone(),
                Err(e) => warnings.push(format!("{key}: {e}; using the font's own face")),
            }
        }
        Ok((faces, warnings))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Traits {
    weight: u16,
    italic: bool,
    /// 1 (ultra-condensed) to 9 (ultra-expanded); 5 is normal.
    width: u16,
}

fn traits(data: &[u8], index: u32) -> Option<Traits> {
    let face = ttf_parser::Face::parse(data, index).ok()?;
    Some(Traits {
        weight: face.weight().to_number(),
        italic: face.is_italic() || face.is_oblique(),
        width: face.width().to_number(),
    })
}

/// Which face to draw a style with, and what to synthesize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pick {
    index: usize,
    embolden: bool,
    slant: bool,
}

const BOLD_WEIGHT: u16 = 600;

/// Picks a face for each style from a family's `faces`, preferring normal widths and the
/// weights closest to 400 and 700; a style with no face of its own is synthesized from the
/// closest one that has.
fn pick(faces: &[Traits]) -> Option<[Pick; 4]> {
    let bold = |t: &Traits| t.weight >= BOLD_WEIGHT;
    let best = |italic: bool, want_bold: bool| {
        let target = if want_bold { 700 } else { 400 };
        faces
            .iter()
            .enumerate()
            .filter(|(_, t)| t.italic == italic && bold(t) == want_bold)
            .min_by_key(|(_, t)| (t.width.abs_diff(5), t.weight.abs_diff(target)))
            .map(|(i, _)| i)
    };
    let regular = best(false, false).or_else(|| {
        faces
            .iter()
            .enumerate()
            .min_by_key(|(_, t)| (t.italic, t.width.abs_diff(5), t.weight.abs_diff(400)))
            .map(|(i, _)| i)
    })?;
    let from = |index: usize, want_bold: bool, italic: bool| Pick {
        index,
        embolden: want_bold && !bold(&faces[index]),
        slant: italic && !faces[index].italic,
    };
    let bold_italic = best(true, true)
        .or(best(true, false))
        .or(best(false, true))
        .unwrap_or(regular);
    Some([
        from(regular, false, false),
        from(best(false, true).unwrap_or(regular), true, false),
        from(best(true, false).unwrap_or(regular), false, true),
        from(bold_italic, true, true),
    ])
}

enum Data {
    Cached(&'static [u8]),
    Read(Rc<Vec<u8>>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Cached(data) => data,
            Data::Read(data) => data,
        }
    }

    fn font(&self, index: u32) -> Option<FontArc> {
        match self {
            Data::Cached(data) => FontRef::try_from_slice_and_index(data, index)
                .ok()
                .map(FontArc::new),
            Data::Read(data) => FontVec::try_from_vec_and_index(data.to_vec(), index)
                .ok()
                .map(FontArc::new),
        }
    }
}

/// Loads the faces `pick` chooses from `candidates`, each (data, index) once.
fn build(candidates: Vec<(Rc<Data>, u32)>) -> Option<FontFaces> {
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter_map(|(data, index)| Some((traits(data.bytes(), index)?, data, index)))
        .collect();
    let traits: Vec<Traits> = candidates.iter().map(|c| c.0).collect();
    let picks = pick(&traits)?;
    let mut loaded: Vec<(usize, FontArc)> = Vec::new();
    let mut faces = Vec::with_capacity(4);
    for p in picks {
        let font = match loaded.iter().find(|(i, _)| *i == p.index) {
            Some((_, font)) => font.clone(),
            None => {
                let (_, data, index) = &candidates[p.index];
                let font = data.font(*index)?;
                loaded.push((p.index, font.clone()));
                font
            }
        };
        faces.push(Face {
            font,
            index: candidates[p.index].2,
            embolden: p.embolden,
            slant: p.slant,
        });
    }
    Some(FontFaces(faces.try_into().ok()?))
}

/// Every face in a font file or collection.
fn from_file(path: &Path) -> Result<FontFaces, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("cannot read font {}: {e}", path.display()))?;
    let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
    let data = Rc::new(Data::Read(Rc::new(bytes)));
    build((0..count).map(|i| (data.clone(), i)).collect())
        .ok_or_else(|| format!("cannot parse font {}", path.display()))
}

/// The installed faces of family `name`, or else the one face with that full or PostScript
/// name.
fn from_system(name: &str) -> Option<FontFaces> {
    let mut files = fallback::family_faces(name);
    if files.is_empty() {
        files.extend(fallback::named_face(name));
    }
    let candidates = files
        .into_iter()
        .filter_map(|(path, index)| {
            Some((Rc::new(Data::Cached(font::read_cached(&path).ok()?)), index))
        })
        .collect();
    build(candidates)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(weight: u16, italic: bool) -> Traits {
        Traits {
            weight,
            italic,
            width: 5,
        }
    }

    fn real(index: usize) -> Pick {
        Pick {
            index,
            embolden: false,
            slant: false,
        }
    }

    #[test]
    fn paths_and_font_file_names_are_files_and_other_values_are_families() {
        assert_eq!(
            Source::of("/fonts/mono.ttf"),
            Source::File(Path::new("/fonts/mono.ttf"))
        );
        assert_eq!(Source::of("Mono.OTF"), Source::File(Path::new("Mono.OTF")));
        assert_eq!(
            Source::of("fonts/collection.ttc"),
            Source::File(Path::new("fonts/collection.ttc"))
        );
        assert_eq!(Source::of("./mono"), Source::File(Path::new("./mono")));
        assert_eq!(Source::of("Fira Code"), Source::Family("Fira Code"));
        assert_eq!(Source::of("Menlo"), Source::Family("Menlo"));
        assert_eq!(
            Source::of("JetBrains Mono NL"),
            Source::Family("JetBrains Mono NL")
        );
    }

    #[test]
    fn a_full_family_uses_its_own_faces() {
        let faces = [t(700, true), t(400, false), t(400, true), t(700, false)];
        assert_eq!(pick(&faces), Some([real(1), real(3), real(2), real(0)]));
    }

    #[test]
    fn a_lone_face_synthesizes_every_other_style() {
        let picks = pick(&[t(400, false)]).unwrap();
        assert_eq!(
            picks.map(|p| (p.embolden, p.slant)),
            [(false, false), (true, false), (false, true), (true, true)]
        );
    }

    #[test]
    fn bold_italic_is_synthesized_from_the_closest_face() {
        let from_italic = pick(&[t(400, false), t(700, false), t(400, true)]).unwrap();
        assert_eq!(
            from_italic[Style::BoldItalic as usize],
            Pick {
                index: 2,
                embolden: true,
                slant: false
            }
        );
        let from_bold = pick(&[t(400, false), t(700, false)]).unwrap();
        assert_eq!(
            from_bold[Style::Italic as usize],
            Pick {
                index: 0,
                embolden: false,
                slant: true
            }
        );
        assert_eq!(
            from_bold[Style::BoldItalic as usize],
            Pick {
                index: 1,
                embolden: false,
                slant: true
            }
        );
    }

    #[test]
    fn the_closest_weights_and_normal_widths_are_preferred() {
        let condensed = Traits {
            weight: 400,
            italic: false,
            width: 3,
        };
        let faces = [
            condensed,
            t(300, false),
            t(900, false),
            t(600, false),
            t(400, false),
        ];
        let picks = pick(&faces).unwrap();
        assert_eq!(picks[Style::Regular as usize], real(4));
        assert_eq!(picks[Style::Bold as usize], real(3));
    }

    #[test]
    fn a_family_with_only_bold_faces_draws_regular_with_them() {
        let picks = pick(&[t(700, false)]).unwrap();
        assert_eq!(picks[Style::Regular as usize], real(0));
        assert_eq!(picks[Style::Bold as usize], real(0));
        assert_eq!(pick(&[]), None);
    }

    #[test]
    fn a_bundled_font_file_resolves_its_own_styles() {
        let dir = std::env::temp_dir().join(format!("trident-family-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("JetBrainsMono-Italic.ttf");
        std::fs::write(&path, font::BUNDLED[Style::Italic as usize]).unwrap();
        let faces = from_file(&path).unwrap();
        let italic = faces.face(Style::Italic);
        assert!(!italic.slant && !italic.embolden);
        let bold = faces.face(Style::BoldItalic);
        assert!(bold.embolden && !bold.slant);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_lone_regular_face_draws_synthesized_bold_and_italic() {
        let dir = std::env::temp_dir().join(format!("trident-synth-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("JetBrainsMono-Regular.ttf");
        std::fs::write(&path, font::REGULAR).unwrap();
        let spec = FontSpec {
            regular: Some(path.to_str().unwrap().into()),
            ..FontSpec::default()
        };
        let (faces, warnings) = FontFaces::resolve(&spec).unwrap();
        assert!(warnings.is_empty());
        let mut fonts = font::Fonts::new(&faces, 32.0);
        let ink = |g: &font::RasterizedGlyph| g.coverage.iter().map(|&c| c as u32).sum::<u32>();
        // How far right the top quarter's ink sits from the bottom quarter's, in pixels.
        let lean = |g: &font::RasterizedGlyph| {
            let center = |rows: std::ops::Range<usize>| {
                let (mut sum, mut total) = (0.0, 0.0);
                for y in rows {
                    for x in 0..g.width {
                        let c = g.coverage[y * g.width + x] as f32;
                        sum += c * (g.left + x as f32);
                        total += c;
                    }
                }
                sum / total
            };
            center(0..g.height / 4) - center(g.height * 3 / 4..g.height)
        };
        let regular = fonts.rasterize('I', Style::Regular, 1).unwrap();
        let bold = fonts.rasterize('I', Style::Bold, 1).unwrap();
        let italic = fonts.rasterize('I', Style::Italic, 1).unwrap();
        let bold_italic = fonts.rasterize('I', Style::BoldItalic, 1).unwrap();
        assert!(ink(&bold) > ink(&regular) * 11 / 10, "bold is heavier");
        assert_eq!(bold.height, regular.height);
        assert!(lean(&regular).abs() < 0.5);
        assert!(lean(&italic) > 2.0, "italic leans {}", lean(&italic));
        assert_eq!(italic.height, regular.height);
        assert!(ink(&bold_italic) > ink(&italic) * 11 / 10 && lean(&bold_italic) > 2.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_collection_face_index_is_loaded() {
        let data = crate::fallback::tests::collection(&[
            font::BUNDLED[Style::Bold as usize],
            font::BUNDLED[Style::Regular as usize],
        ]);
        let dir = std::env::temp_dir().join(format!("trident-ttc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mono.ttc");
        std::fs::write(&path, data).unwrap();
        let faces = from_file(&path).unwrap();
        let regular = FontArc::try_from_slice(font::BUNDLED[0]).unwrap();
        let bold = FontArc::try_from_slice(font::BUNDLED[1]).unwrap();
        use ab_glyph::Font;
        let id = regular.glyph_id('W');
        let width = |f: &FontArc| f.outline(id).map(|o| o.bounds.max.x - o.bounds.min.x);
        assert_eq!(width(&faces.face(Style::Regular).font), width(&regular));
        assert_eq!(width(&faces.face(Style::Bold).font), width(&bold));
        assert!(!faces.face(Style::Bold).embolden);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unknown_family_warns_and_uses_the_bundled_font() {
        let spec = FontSpec {
            regular: Some("No Such Family Trident".into()),
            italic: Some("/no/such/italic.ttf".into()),
            ..FontSpec::default()
        };
        let (faces, warnings) = FontFaces::resolve(&spec).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("\"No Such Family Trident\""),
            "{warnings:?}"
        );
        assert!(warnings[1].starts_with("font_italic:"), "{warnings:?}");
        assert!(!faces.face(Style::Italic).slant);
        let (_, warnings) = FontFaces::resolve(&FontSpec {
            regular: Some(font::FAMILY.to_lowercase()),
            ..FontSpec::default()
        })
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let missing = FontFaces::resolve(&FontSpec {
            regular: Some("/no/such/font.ttf".into()),
            ..FontSpec::default()
        });
        assert!(missing.is_err());
    }
}
