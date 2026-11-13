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

fn columns(text: &str, end: usize) -> usize {
    text.char_indices()
        .take_while(|(byte, _)| *byte < end)
        .map(|(_, c)| c.width().unwrap_or(0))
        .sum()
}

impl Preedit<'_> {
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
        let start = end.saturating_sub(available);
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
