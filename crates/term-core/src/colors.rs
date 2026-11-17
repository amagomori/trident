//! The color scheme (Tokyo Night) and resolution of terminal colors to RGB.
//!
//! ```rust
//! use term_core::{Color, colors::Palette};
//! let palette = Palette::default();
//! assert_eq!(palette.resolve(Color::DefaultFg), palette.foreground);
//! ```

use crate::Color;

/// Red, green and blue channels, each in `0..=255`.
pub type Rgb = (u8, u8, u8);

/// Default text color.
pub const FOREGROUND: Rgb = (0xc0, 0xca, 0xf5);
/// Default window background color.
pub const BACKGROUND: Rgb = (0x1a, 0x1b, 0x26);
/// Default cursor color.
pub const CURSOR: Rgb = (0xc0, 0xca, 0xf5);
/// Default selected-cell background color.
pub const SELECTION: Rgb = (0x33, 0x46, 0x7c);

const ANSI: [Rgb; 16] = [
    (0x15, 0x16, 0x1e),
    (0xf7, 0x76, 0x8e),
    (0x9e, 0xce, 0x6a),
    (0xe0, 0xaf, 0x68),
    (0x7a, 0xa2, 0xf7),
    (0xbb, 0x9a, 0xf7),
    (0x7d, 0xcf, 0xff),
    (0xa9, 0xb1, 0xd6),
    (0x41, 0x48, 0x68),
    (0xff, 0x89, 0x9d),
    (0x9f, 0xe0, 0x44),
    (0xfa, 0xba, 0x4a),
    (0x8d, 0xb0, 0xff),
    (0xc7, 0xa9, 0xff),
    (0xa4, 0xda, 0xff),
    (0xc0, 0xca, 0xf5),
];

/// Configurable default and ANSI colors; see the [module example](self).
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    /// Text color for [`Color::DefaultFg`].
    pub foreground: Rgb,
    /// Background color for [`Color::DefaultBg`].
    pub background: Rgb,
    /// Cursor color before per-cell inversion.
    pub cursor: Rgb,
    /// Selected-cell background color.
    pub selection: Rgb,
    /// Normal ANSI colors followed by their eight bright counterparts.
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            foreground: FOREGROUND,
            background: BACKGROUND,
            cursor: CURSOR,
            selection: SELECTION,
            ansi: ANSI,
        }
    }
}

impl Palette {
    /// Whether the background reads as dark, by its Rec. 709 luma.
    pub fn is_dark(&self) -> bool {
        let (r, g, b) = self.background;
        2126 * u32::from(r) + 7152 * u32::from(g) + 722 * u32::from(b) < 128 * 10000
    }

    /// Resolves defaults and ANSI indices using this palette.
    pub fn resolve(&self, color: Color) -> Rgb {
        match color {
            Color::DefaultFg => self.foreground,
            Color::DefaultBg => self.background,
            Color::Indexed(i @ 0..=15) => self.ansi[i as usize],
            other => resolve(other),
        }
    }
}

/// Resolves an xterm 256-color index using the built-in ANSI colors.
pub fn indexed(i: u8) -> Rgb {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            (level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        232..=255 => {
            let v = 8 + (i - 232) * 10;
            (v, v, v)
        }
    }
}

/// Resolves a terminal color using the built-in defaults, not a custom palette.
pub fn resolve(color: Color) -> Rgb {
    match color {
        Color::DefaultFg => FOREGROUND,
        Color::DefaultBg => BACKGROUND,
        Color::Indexed(i) => indexed(i),
        Color::Rgb(r, g, b) => (r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256_palette() {
        assert_eq!(indexed(16), (0, 0, 0));
        assert_eq!(indexed(196), (255, 0, 0));
        assert_eq!(indexed(231), (255, 255, 255));
        assert_eq!(indexed(232), (8, 8, 8));
        assert_eq!(indexed(255), (238, 238, 238));
    }
}
