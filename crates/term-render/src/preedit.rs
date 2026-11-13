use unicode_width::UnicodeWidthChar;

/// IME text is an overlay, never part of the terminal grid. Cursor offsets are UTF-8 bytes.
#[derive(Clone, Copy, Debug)]
pub struct Preedit<'a> {
    pub text: &'a str,
    pub cursor: Option<(usize, usize)>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct PreeditLayout {
    pub column: usize,
    pub start: usize,
    pub width: usize,
    pub caret: Option<usize>,
    pub selection: (usize, usize),
}

pub(crate) struct PreeditGlyph {
    pub character: char,
    pub column: usize,
    pub base_column: usize,
    pub width: usize,
}

fn columns(text: &str, end: usize) -> usize {
    text.char_indices()
        .take_while(|(byte, _)| *byte < end)
        .map(|(_, c)| c.width().unwrap_or(0))
        .sum()
}

impl Preedit<'_> {
    pub(crate) fn glyphs(self) -> impl Iterator<Item = PreeditGlyph> {
        self.text
            .chars()
            .filter_map(|c| c.width().map(|width| (c, width)))
            .scan((0, 0), |(column, base_column), (character, width)| {
                if width != 0 {
                    *base_column = *column;
                }
                let glyph = PreeditGlyph {
                    character,
                    column: *column,
                    base_column: *base_column,
                    width,
                };
                *column += width;
                Some(glyph)
            })
    }

    /// Fits a single-line composition in the grid, keeping its selected clause visible.
    pub fn layout(self, column: usize, cols: usize) -> PreeditLayout {
        let cols = cols.max(1);
        let width = columns(self.text, self.text.len());
        let selection = self.cursor.map_or((0, 0), |(a, b)| {
            let a = columns(self.text, a);
            let b = columns(self.text, b);
            (a.min(b), a.max(b))
        });
        let caret = self.cursor.map(|(a, _)| columns(self.text, a));
        let occupied = width.max(caret.map_or(0, |c| c + 1)).max(1);
        let column = column.min(cols.saturating_sub(occupied.min(cols)));
        let available = cols - column;
        let end = selection.1.max(caret.map_or(0, |c| c + 1));
        let start = end
            .saturating_sub(available)
            .min(caret.unwrap_or(usize::MAX));
        PreeditLayout {
            column,
            start,
            width: occupied.saturating_sub(start).min(available),
            caret: caret.map(|c| column + c.saturating_sub(start).min(available - 1)),
            selection,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combining_glyphs_use_the_pen_after_their_base() {
        let preedit = Preedit {
            text: "e\u{301}\u{300}日\u{3099}",
            cursor: None,
        };
        let glyphs: Vec<_> = preedit.glyphs().collect();
        assert_eq!(
            glyphs
                .iter()
                .map(|g| (g.column, g.base_column, g.width))
                .collect::<Vec<_>>(),
            [(0, 0, 1), (1, 0, 0), (1, 0, 0), (1, 1, 2), (3, 1, 0)]
        );

        let mut fonts = crate::font::Fonts::new(None, 16.0).expect("bundled font");
        let accent = fonts
            .rasterize('\u{301}', crate::font::Style::Regular, 1)
            .expect("combining acute");
        assert!(accent.left < 0.0);
        let left = glyphs[1].column as f32 * fonts.metrics.width + accent.left;
        assert!(left >= 0.0, "accent must not land before its base cell");
        assert!(left + accent.width as f32 <= fonts.metrics.width + 1.0);
    }

    #[test]
    fn japanese_byte_offsets_become_wide_columns() {
        let layout = Preedit {
            text: "a日本",
            cursor: Some((1, 7)),
        }
        .layout(10, 80);
        assert_eq!(layout.column, 10);
        assert_eq!(layout.caret, Some(11));
        assert_eq!(layout.selection, (1, 5));
        assert_eq!(layout.width, 5);
    }

    #[test]
    fn composition_shifts_left_at_the_right_edge() {
        let layout = Preedit {
            text: "日本",
            cursor: Some((6, 6)),
        }
        .layout(79, 80);
        assert_eq!(layout.column, 75);
        assert_eq!(layout.caret, Some(79));
        assert_eq!(layout.width, 5);
    }

    #[test]
    fn converted_clause_stays_visible_when_selection_starts_at_zero() {
        let layout = Preedit {
            text: "日本語",
            cursor: Some((0, 9)),
        }
        .layout(79, 80);
        assert_eq!(layout.column, 74);
        assert_eq!(layout.start, 0);
        assert_eq!(layout.caret, Some(74));
        assert_eq!(layout.width, 6);
    }

    #[test]
    fn long_preedit_scrolls_to_caret_and_clamps_to_tiny_grids() {
        let preedit = Preedit {
            text: "abcdefghijklmnop",
            cursor: Some((16, 16)),
        };
        let layout = preedit.layout(3, 10);
        assert_eq!(
            (layout.column, layout.start, layout.width, layout.caret),
            (0, 7, 10, Some(9))
        );
        assert_eq!(preedit.layout(3, 1).caret, Some(0));
    }

    #[test]
    fn selection_wider_than_the_grid_keeps_the_caret_on_its_character() {
        let layout = Preedit {
            text: "abcdefghijklmnopqrst",
            cursor: Some((0, 20)),
        }
        .layout(0, 10);
        assert_eq!(
            (layout.column, layout.start, layout.width, layout.caret),
            (0, 0, 10, Some(0))
        );
        let layout = Preedit {
            text: "abcdefghijklmnopqrst",
            cursor: Some((5, 20)),
        }
        .layout(0, 10);
        assert_eq!((layout.start, layout.caret), (5, Some(0)));
    }

    #[test]
    fn combining_marks_do_not_advance_and_missing_cursor_stays_hidden() {
        let preedit = Preedit {
            text: "e\u{301}",
            cursor: Some((3, 3)),
        };
        assert_eq!(preedit.layout(2, 80).caret, Some(3));
        assert_eq!(
            Preedit {
                cursor: None,
                ..preedit
            }
            .layout(2, 80)
            .caret,
            None
        );
    }
}
