//! System font discovery for characters the primary font lacks: fontconfig on Linux and
//! other Unixes (loaded at runtime, so it is optional), Core Text on macOS. Nothing is
//! queried until the first character that needs a fallback. Also finds the faces of a
//! configured font family.

use std::path::PathBuf;

/// A font file and the face index inside it (for collections such as `.ttc`).
pub type FontFile = (PathBuf, u32);

#[cfg(target_os = "macos")]
pub use coretext::{SystemFonts, family_faces, named_face};
#[cfg(not(target_os = "macos"))]
pub use fontconfig::{SystemFonts, family_faces, named_face};

/// The face in a font file or collection whose PostScript name is `postscript`, reading
/// only each face's `name` table; 0 if none matches.
#[cfg(any(target_os = "macos", test))]
fn face_index(data: &[u8], postscript: &str) -> u32 {
    let name = ttf_parser::Tag::from_bytes(b"name");
    let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
    (0..count)
        .find(|&i| {
            ttf_parser::RawFace::parse(data, i)
                .ok()
                .and_then(|face| face.table(name))
                .and_then(ttf_parser::name::Table::parse)
                .is_some_and(|table| {
                    table.names.into_iter().any(|n| {
                        n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME
                            && n.to_string().as_deref() == Some(postscript)
                    })
                })
        })
        .unwrap_or(0)
}

/// Characters that default to emoji presentation take two cells, so a wide character in
/// the emoji blocks is drawn from a color emoji font in preference to a text font.
pub fn prefers_emoji(c: char, cells: usize) -> bool {
    cells == 2
        && matches!(c as u32,
            0x2190..=0x21FF | 0x2300..=0x23FF | 0x2460..=0x27BF | 0x2900..=0x2BFF
            | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
}

#[cfg(not(target_os = "macos"))]
mod fontconfig {
    use super::FontFile;
    use fontconfig_sys::constants::{
        FC_CHARSET, FC_COLOR, FC_FAMILY, FC_FILE, FC_FULLNAME, FC_INDEX, FC_POSTSCRIPT_NAME,
    };
    use fontconfig_sys::statics::{LIB, LIB_RESULT};
    use fontconfig_sys::*;
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::ptr;

    struct Candidate {
        file: FontFile,
        charset: *const FcCharSet,
        color: bool,
    }

    /// fontconfig's sorted font lists for "monospace" and "emoji", built on first use.
    /// The charsets point into the font sets, which live as long as this does.
    pub struct SystemFonts {
        sets: Option<Vec<*mut FcFontSet>>,
        monospace: Vec<Candidate>,
        emoji: Vec<Candidate>,
    }

    impl Default for SystemFonts {
        fn default() -> Self {
            Self::new()
        }
    }

    impl SystemFonts {
        pub fn new() -> SystemFonts {
            SystemFonts {
                sets: None,
                monospace: Vec::new(),
                emoji: Vec::new(),
            }
        }

        fn init(&mut self) {
            if self.sets.is_some() {
                return;
            }
            let mut sets = Vec::new();
            if LIB_RESULT.is_ok() {
                unsafe {
                    let config = (LIB.FcInitLoadConfigAndFonts)();
                    if !config.is_null() {
                        for (name, out) in [
                            (c"monospace", &mut self.monospace),
                            (c"emoji:color=true", &mut self.emoji),
                        ] {
                            if let Some(set) = sort(config, name) {
                                *out = candidates(set);
                                sets.push(set);
                            }
                        }
                        (LIB.FcConfigDestroy)(config);
                    }
                }
            }
            self.sets = Some(sets);
        }

        /// The first of the fonts fontconfig says cover `c`, best first, that `accept`s.
        pub fn find(
            &mut self,
            c: char,
            emoji: bool,
            mut accept: impl FnMut(&FontFile) -> bool,
        ) -> Option<FontFile> {
            self.init();
            let covers =
                |f: &&Candidate| unsafe { (LIB.FcCharSetHasChar)(f.charset, c as FcChar32) != 0 };
            let emoji_fonts = self.emoji.iter().filter(|f| emoji && f.color);
            emoji_fonts
                .chain(&self.monospace)
                .filter(covers)
                .map(|f| &f.file)
                .find(|f| accept(f))
                .cloned()
        }
    }

    impl Drop for SystemFonts {
        fn drop(&mut self) {
            for set in self.sets.take().into_iter().flatten() {
                unsafe { (LIB.FcFontSetDestroy)(set) };
            }
        }
    }

    /// The installed faces of family `name` (matched case-insensitively), sorted by file.
    pub fn family_faces(name: &str) -> Vec<FontFile> {
        list(FC_FAMILY, name)
    }

    /// The installed face whose full or PostScript name is `name`.
    pub fn named_face(name: &str) -> Option<FontFile> {
        list(FC_FULLNAME, name)
            .into_iter()
            .next()
            .or_else(|| list(FC_POSTSCRIPT_NAME, name).into_iter().next())
    }

    fn list(object: &CStr, value: &str) -> Vec<FontFile> {
        let Ok(value) = CString::new(value) else {
            return Vec::new();
        };
        if LIB_RESULT.is_err() {
            return Vec::new();
        }
        let mut files = Vec::new();
        unsafe {
            let config = (LIB.FcInitLoadConfigAndFonts)();
            if config.is_null() {
                return files;
            }
            let pattern = (LIB.FcPatternCreate)();
            let objects = (LIB.FcObjectSetCreate)();
            if !pattern.is_null() && !objects.is_null() {
                (LIB.FcPatternAddString)(pattern, object.as_ptr(), value.as_ptr().cast());
                (LIB.FcObjectSetAdd)(objects, FC_FILE.as_ptr());
                (LIB.FcObjectSetAdd)(objects, FC_INDEX.as_ptr());
                let set = (LIB.FcFontList)(config, pattern, objects);
                if !set.is_null() {
                    let fonts = &*set;
                    let fonts = match fonts.nfont {
                        n if n > 0 && !fonts.fonts.is_null() => {
                            std::slice::from_raw_parts(fonts.fonts, n as usize)
                        }
                        _ => &[],
                    };
                    for &font in fonts {
                        let mut file: *mut FcChar8 = ptr::null_mut();
                        let mut index = 0;
                        if (LIB.FcPatternGetString)(font, FC_FILE.as_ptr(), 0, &mut file)
                            != FcResultMatch
                        {
                            continue;
                        }
                        (LIB.FcPatternGetInteger)(font, FC_INDEX.as_ptr(), 0, &mut index);
                        let path = PathBuf::from(OsStr::from_bytes(
                            CStr::from_ptr(file.cast()).to_bytes(),
                        ));
                        // The high bits number a variable font's named instances.
                        files.push((path, (index.max(0) & 0xffff) as u32));
                    }
                    (LIB.FcFontSetDestroy)(set);
                }
            }
            if !objects.is_null() {
                (LIB.FcObjectSetDestroy)(objects);
            }
            if !pattern.is_null() {
                (LIB.FcPatternDestroy)(pattern);
            }
            (LIB.FcConfigDestroy)(config);
        }
        files.sort();
        files.dedup();
        files
    }

    unsafe fn sort(config: *mut FcConfig, name: &CStr) -> Option<*mut FcFontSet> {
        unsafe {
            let pattern = (LIB.FcNameParse)(name.as_ptr().cast());
            if pattern.is_null() {
                return None;
            }
            (LIB.FcConfigSubstitute)(config, pattern, FcMatchPattern);
            (LIB.FcDefaultSubstitute)(pattern);
            let mut result = FcResultNoMatch;
            let set = (LIB.FcFontSort)(config, pattern, 1, ptr::null_mut(), &mut result);
            (LIB.FcPatternDestroy)(pattern);
            (!set.is_null()).then_some(set)
        }
    }

    /// Copies file names while borrowing the set's character coverage.
    ///
    /// # Safety
    ///
    /// `set` must point to an initialized `FcFontSet`. For a nonempty set,
    /// fontconfig must be loaded and its patterns and charsets must stay alive
    /// while any returned candidate's charset is used.
    unsafe fn candidates(set: *mut FcFontSet) -> Vec<Candidate> {
        // SAFETY: The caller guarantees a live, non-null fontconfig set.
        let set = unsafe { &*set };
        if set.nfont <= 0 || set.fonts.is_null() {
            return Vec::new();
        }
        // SAFETY: A nonempty fontconfig set has nfont initialized pattern pointers.
        // The array is non-null and remains owned by the caller's live set.
        let fonts = unsafe { std::slice::from_raw_parts(set.fonts, set.nfont as usize) };
        fonts
            .iter()
            .filter_map(|&pattern| unsafe {
                let mut file: *mut FcChar8 = ptr::null_mut();
                let mut index = 0;
                let mut charset: *mut FcCharSet = ptr::null_mut();
                let mut color = 0;
                if (LIB.FcPatternGetString)(pattern, FC_FILE.as_ptr(), 0, &mut file)
                    != FcResultMatch
                    || (LIB.FcPatternGetCharSet)(pattern, FC_CHARSET.as_ptr(), 0, &mut charset)
                        != FcResultMatch
                {
                    return None;
                }
                (LIB.FcPatternGetInteger)(pattern, FC_INDEX.as_ptr(), 0, &mut index);
                (LIB.FcPatternGetBool)(pattern, FC_COLOR.as_ptr(), 0, &mut color);
                let path = PathBuf::from(OsStr::from_bytes(CStr::from_ptr(file.cast()).to_bytes()));
                Some(Candidate {
                    file: (path, index.max(0) as u32),
                    charset,
                    color: color != 0,
                })
            })
            .collect()
    }

    #[test]
    fn empty_font_sets_have_no_candidates() {
        let mut set = FcFontSet {
            nfont: 0,
            sfont: 0,
            fonts: ptr::null_mut(),
        };
        // SAFETY: set is initialized and live; an empty set borrows no font data
        // and makes no fontconfig calls. A null empty array must not form a slice.
        assert!(unsafe { candidates(&mut set) }.is_empty());
    }
}

#[cfg(target_os = "macos")]
mod coretext {
    use super::FontFile;
    use objc2_core_foundation::{CFArray, CFRange, CFRetained, CFString, CFType, CFURL};
    use objc2_core_text::{
        CTFont, CTFontCollection, CTFontDescriptor, kCTFontNameAttribute, kCTFontURLAttribute,
    };

    /// Core Text's cascade list for Menlo, the system monospace font.
    #[derive(Default)]
    pub struct SystemFonts {
        base: Option<CFRetained<CTFont>>,
    }

    impl SystemFonts {
        pub fn new() -> SystemFonts {
            SystemFonts { base: None }
        }

        /// The first font that `accept`s, of the one Core Text picks for `c` (its cascade
        /// list already prefers Apple Color Emoji for emoji) and then the rest of the cascade
        /// list, since Core Text can pick fonts whose outlines only it can read. The cascade
        /// list is only read when the picked font is rejected.
        pub fn find(
            &mut self,
            c: char,
            _emoji: bool,
            mut accept: impl FnMut(&FontFile) -> bool,
        ) -> Option<FontFile> {
            let base = self.base.get_or_insert_with(|| unsafe {
                CTFont::with_name(&CFString::from_str("Menlo"), 14.0, std::ptr::null())
            });
            let mut buf = [0u8; 4];
            let string = CFString::from_str(c.encode_utf8(&mut buf));
            let range = CFRange::new(0, c.len_utf16() as isize);
            let font = unsafe { base.for_string(&string, range) };
            let url = unsafe { font.attribute(kCTFontURLAttribute) };
            let picked = file(url, Some(unsafe { font.post_script_name() }.to_string()));
            if picked.as_ref().is_some_and(&mut accept) {
                return picked;
            }
            let list = unsafe { base.default_cascade_list_for_languages(None) }?;
            let list: &CFArray<CTFontDescriptor> = unsafe { &*(&*list as *const CFArray).cast() };
            list.iter().find_map(|descriptor| {
                let url = unsafe { descriptor.attribute(kCTFontURLAttribute) };
                let name = unsafe { descriptor.attribute(kCTFontNameAttribute) }
                    .and_then(|n| n.downcast::<CFString>().ok())
                    .map(|n| n.to_string());
                let f = file(url, name)?;
                (picked.as_ref() != Some(&f) && accept(&f)).then_some(f)
            })
        }
    }

    /// The installed faces of family `name`, sorted by file.
    pub fn family_faces(name: &str) -> Vec<FontFile> {
        let collection = unsafe { CTFontCollection::from_available_fonts(None) };
        let Some(list) = (unsafe {
            collection.matching_font_descriptors_for_family(&CFString::from_str(name), None)
        }) else {
            return Vec::new();
        };
        let list: &CFArray<CTFontDescriptor> = unsafe { &*(&*list as *const CFArray).cast() };
        let mut files: Vec<FontFile> = list
            .iter()
            .filter_map(|descriptor| {
                let url = unsafe { descriptor.attribute(kCTFontURLAttribute) };
                let name = unsafe { descriptor.attribute(kCTFontNameAttribute) }
                    .and_then(|n| n.downcast::<CFString>().ok())
                    .map(|n| n.to_string());
                file(url, name)
            })
            .collect();
        files.sort();
        files.dedup();
        files
    }

    /// The installed face whose full or PostScript name is `name`; Core Text substitutes
    /// another font for unknown names, so the found font's names are checked.
    pub fn named_face(name: &str) -> Option<FontFile> {
        let font = unsafe { CTFont::with_name(&CFString::from_str(name), 12.0, std::ptr::null()) };
        let postscript = unsafe { font.post_script_name() }.to_string();
        let full = unsafe { font.full_name() }.to_string();
        if !(postscript.eq_ignore_ascii_case(name) || full.eq_ignore_ascii_case(name)) {
            return None;
        }
        file(
            unsafe { font.attribute(kCTFontURLAttribute) },
            Some(postscript),
        )
    }

    fn file(url: Option<CFRetained<CFType>>, postscript: Option<String>) -> Option<FontFile> {
        let path = url?.downcast::<CFURL>().ok()?.to_file_path()?;
        let index = match postscript {
            Some(name) => {
                crate::font::read_cached(&path).map_or(0, |data| super::face_index(data, &name))
            }
            None => 0,
        };
        Some((path, index))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::face_index;
    use crate::font::{BOLD, REGULAR};

    /// A font collection holding `fonts` in order.
    pub(crate) fn collection(fonts: &[&[u8]]) -> Vec<u8> {
        let mut data = b"ttcf\x00\x01\x00\x00".to_vec();
        data.extend((fonts.len() as u32).to_be_bytes());
        let mut offset = data.len() + 4 * fonts.len();
        let mut faces = Vec::new();
        for font in fonts {
            data.extend((offset as u32).to_be_bytes());
            let mut face = font.to_vec();
            let tables = u16::from_be_bytes([face[4], face[5]]) as usize;
            for record in (12..12 + 16 * tables).step_by(16) {
                let at = record + 8;
                let table = u32::from_be_bytes(face[at..at + 4].try_into().unwrap());
                face[at..at + 4].copy_from_slice(&(table + offset as u32).to_be_bytes());
            }
            offset += face.len();
            faces.push(face);
        }
        faces.into_iter().for_each(|face| data.extend(face));
        data
    }

    #[test]
    fn a_single_font_is_face_zero() {
        assert_eq!(face_index(REGULAR, "JetBrainsMono-Regular"), 0);
    }

    #[test]
    fn a_collection_face_is_found_by_postscript_name() {
        let data = collection(&[REGULAR, BOLD]);
        assert_eq!(ttf_parser::fonts_in_collection(&data), Some(2));
        assert_eq!(face_index(&data, "JetBrainsMono-Regular"), 0);
        assert_eq!(face_index(&data, "JetBrainsMono-Bold"), 1);
        assert_eq!(face_index(&data, "NoSuchFont"), 0);
    }
}
