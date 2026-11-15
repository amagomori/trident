//! System font discovery for characters the primary font lacks: fontconfig on Linux and
//! other Unixes (loaded at runtime, so it is optional), Core Text on macOS. Nothing is
//! queried until the first character that needs a fallback.

use std::path::PathBuf;

/// A font file and the face index inside it (for collections such as `.ttc`).
pub type FontFile = (PathBuf, u32);

#[cfg(target_os = "macos")]
pub use coretext::SystemFonts;
#[cfg(not(target_os = "macos"))]
pub use fontconfig::SystemFonts;

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
    use fontconfig_sys::constants::{FC_CHARSET, FC_COLOR, FC_FILE, FC_INDEX};
    use fontconfig_sys::statics::{LIB, LIB_RESULT};
    use fontconfig_sys::*;
    use std::ffi::{CStr, OsStr};
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

        /// Fonts fontconfig says cover `c`, best first.
        pub fn lookup(&mut self, c: char, emoji: bool) -> Vec<FontFile> {
            self.init();
            let covers =
                |f: &&Candidate| unsafe { (LIB.FcCharSetHasChar)(f.charset, c as FcChar32) != 0 };
            let emoji_fonts = self.emoji.iter().filter(|f| emoji && f.color);
            emoji_fonts
                .chain(&self.monospace)
                .filter(covers)
                .map(|f| f.file.clone())
                .collect()
        }
    }

    impl Drop for SystemFonts {
        fn drop(&mut self) {
            for set in self.sets.take().into_iter().flatten() {
                unsafe { (LIB.FcFontSetDestroy)(set) };
            }
        }
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

    unsafe fn candidates(set: *mut FcFontSet) -> Vec<Candidate> {
        let set = unsafe { &*set };
        let fonts = unsafe { std::slice::from_raw_parts(set.fonts, set.nfont.max(0) as usize) };
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
}

#[cfg(target_os = "macos")]
mod coretext {
    use super::FontFile;
    use objc2_core_foundation::{CFArray, CFRange, CFRetained, CFString, CFType, CFURL};
    use objc2_core_text::{CTFont, CTFontDescriptor, kCTFontNameAttribute, kCTFontURLAttribute};

    /// Core Text's cascade list for Menlo, the system monospace font.
    #[derive(Default)]
    pub struct SystemFonts {
        base: Option<CFRetained<CTFont>>,
    }

    impl SystemFonts {
        pub fn new() -> SystemFonts {
            SystemFonts { base: None }
        }

        /// The font Core Text picks for `c` (its cascade list already prefers Apple Color
        /// Emoji for emoji), then the rest of the cascade list, since Core Text can pick fonts
        /// whose outlines only it can read.
        pub fn lookup(&mut self, c: char, _emoji: bool) -> Vec<FontFile> {
            let base = self.base.get_or_insert_with(|| unsafe {
                CTFont::with_name(&CFString::from_str("Menlo"), 14.0, std::ptr::null())
            });
            let mut buf = [0u8; 4];
            let string = CFString::from_str(c.encode_utf8(&mut buf));
            let range = CFRange::new(0, c.len_utf16() as isize);
            let font = unsafe { base.for_string(&string, range) };
            let mut files = Vec::new();
            let url = unsafe { font.attribute(kCTFontURLAttribute) };
            files.extend(file(
                url,
                Some(unsafe { font.post_script_name() }.to_string()),
            ));
            if let Some(list) = unsafe { base.default_cascade_list_for_languages(None) } {
                let list: &CFArray<CTFontDescriptor> =
                    unsafe { &*(&*list as *const CFArray).cast() };
                for descriptor in list.iter() {
                    let url = unsafe { descriptor.attribute(kCTFontURLAttribute) };
                    let name = unsafe { descriptor.attribute(kCTFontNameAttribute) }
                        .and_then(|n| n.downcast::<CFString>().ok())
                        .map(|n| n.to_string());
                    files.extend(file(url, name));
                }
            }
            files.dedup();
            files
        }
    }

    fn file(url: Option<CFRetained<CFType>>, postscript: Option<String>) -> Option<FontFile> {
        let path = url?.downcast::<CFURL>().ok()?.to_file_path()?;
        let index = postscript.map_or(0, |name| face_index(&path, &name));
        Some((path, index))
    }

    /// Core Text names the font but not its index inside a collection; find it by
    /// PostScript name.
    fn face_index(path: &std::path::Path, postscript: &str) -> u32 {
        let Ok(data) = crate::font::map(path) else {
            return 0;
        };
        let count = ttf_parser::fonts_in_collection(data).unwrap_or(1);
        (0..count)
            .find(|&i| {
                ttf_parser::Face::parse(data, i).is_ok_and(|face| {
                    face.names().into_iter().any(|n| {
                        n.name_id == ttf_parser::name_id::POST_SCRIPT_NAME
                            && n.to_string().as_deref() == Some(postscript)
                    })
                })
            })
            .unwrap_or(0)
    }
}
