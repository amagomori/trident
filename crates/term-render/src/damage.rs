//! Damage tracking for the grid: which viewport rows changed since the last frame, so the
//! renderer rebuilds and re-uploads only those rows' instances.

use term_core::colors::Palette;
use term_core::search::Match;
use term_core::{Cell, CursorShape, Flags, Link, Point, Terminal};

/// Shared frame inputs whose changes damage every row.
#[derive(Clone, PartialEq)]
struct FrameKey {
    rows: usize,
    cols: usize,
    padding: f32,
    pixel_offset: f32,
    metrics: [f32; 4],
    focused: bool,
    cursor_shape: CursorShape,
    palette: Palette,
    atlas: u64,
    graphemes: u64,
    links: u64,
    /// Search and hover highlights are keyed by absolute line.
    top: Option<Point>,
    current_match: Option<usize>,
}

/// The inputs of one frame's grid that live outside the terminal.
pub(crate) struct Frame<'a> {
    pub padding: f32,
    /// Cell width, height, underline offset and stroke.
    pub metrics: [f32; 4],
    pub pixel_offset: f32,
    pub focused: bool,
    /// Cursor drawn in the grid, in viewport coordinates.
    pub cursor: Option<(usize, usize)>,
    pub search: Option<(&'a [Match], Option<usize>)>,
    pub hover: Option<&'a Link>,
    /// Bumped whenever cached glyph positions become invalid.
    pub atlas: u64,
    /// Whether blinking cells show their text this frame.
    pub blink_visible: bool,
    /// Compare-with-last-run marks of each drawn row, from `Terminal::rerun_marks`.
    pub rerun: &'a [u8],
}

#[derive(Default)]
struct RowState {
    cells: Vec<Cell>,
    cursor: Option<usize>,
    selected: Option<(usize, usize)>,
    rerun: u8,
}

#[derive(Default)]
pub(crate) struct Damage {
    key: Option<FrameKey>,
    matches: Vec<Match>,
    hover: Option<Link>,
    blink_visible: bool,
    rows: Vec<RowState>,
}

impl Damage {
    /// The first viewport row drawn: a partly visible row above the grid while smooth
    /// scrolling.
    pub fn first_row(pixel_offset: f32) -> isize {
        -isize::from(pixel_offset > 0.0)
    }

    /// Records this frame and sets `damaged[i]` for each drawn row `first_row + i` whose
    /// instances must be rebuilt.
    pub fn update(&mut self, term: &Terminal, frame: &Frame<'_>, damaged: &mut Vec<bool>) {
        let first = Self::first_row(frame.pixel_offset);
        let count = (term.rows() as isize - first) as usize;
        let selection = term.selection_range();
        let key = FrameKey {
            rows: term.rows(),
            cols: term.cols(),
            padding: frame.padding,
            pixel_offset: frame.pixel_offset,
            metrics: frame.metrics,
            focused: frame.focused,
            cursor_shape: term.cursor_shape,
            palette: term.palette.clone(),
            atlas: frame.atlas,
            graphemes: term.grapheme_releases(),
            links: term.link_releases(),
            top: (frame.search.is_some() || frame.hover.is_some())
                .then(|| term.viewport_to_point_signed(first, 0)),
            current_match: frame.search.and_then(|(_, current)| current),
        };
        let matches = frame.search.map_or(&[][..], |(matches, _)| matches);
        let mut all = self.key.as_ref() != Some(&key)
            || self.matches != matches
            || self.hover.as_ref() != frame.hover;
        if all {
            self.key = Some(key);
            self.matches.clear();
            self.matches.extend_from_slice(matches);
            self.hover = frame.hover.cloned();
        }
        if self.rows.len() != count {
            self.rows.resize_with(count, RowState::default);
            all = true;
        }
        let blink_flipped =
            std::mem::replace(&mut self.blink_visible, frame.blink_visible) != frame.blink_visible;
        damaged.clear();
        damaged.resize(count, false);
        for (i, state) in self.rows.iter_mut().enumerate() {
            let row = first + i as isize;
            let selected = selection.and_then(|range| {
                term.selection_columns(range, term.viewport_to_point_signed(row, 0).line)
            });
            let cells = &term.viewport_row_signed(row).cells;
            let cursor = frame
                .cursor
                .filter(|&(r, _)| r as isize == row)
                .map(|(_, col)| col);
            let blinked = blink_flipped
                && state
                    .cells
                    .iter()
                    .any(|cell| cell.flags.contains(Flags::BLINK));
            let rerun = frame.rerun.get(i).copied().unwrap_or(0) & term_core::ROW_CHANGED;
            if all
                || blinked
                || state.cursor != cursor
                || state.selected != selected
                || state.rerun != rerun
                || state.cells != *cells
            {
                state.cells.clone_from(cells);
                state.cursor = cursor;
                state.selected = selected;
                state.rerun = rerun;
                damaged[i] = true;
            }
        }
    }

    /// Inclusive highlighted columns in drawn row `i` of the recorded frame.
    pub fn selected_columns(&self, i: usize) -> Option<std::ops::RangeInclusive<usize>> {
        self.rows[i].selected.map(|(from, to)| from..=to)
    }

    /// Forgets the last frame, so the next one rebuilds every row.
    pub fn invalidate(&mut self) {
        self.key = None;
    }
}

/// Instance ranges to upload for one layer whose rows are packed back to back: rows that
/// were rebuilt or not uploaded yet (`pending`), and rows that moved because an earlier row
/// changed length. `offsets` holds where each row was last uploaded and is updated.
pub(crate) fn upload_spans(
    lens: &[u32],
    pending: &[bool],
    offsets: &mut Vec<u32>,
) -> Vec<std::ops::Range<usize>> {
    offsets.resize(lens.len(), u32::MAX);
    let mut spans: Vec<std::ops::Range<usize>> = Vec::new();
    let mut offset = 0u32;
    let mut open = false;
    for (i, &len) in lens.iter().enumerate() {
        let start = offset;
        offset += len;
        let dirty = pending[i] || offsets[i] != start;
        offsets[i] = start;
        if !dirty {
            open = false;
            continue;
        }
        match spans.last_mut() {
            Some(span) if open => span.end = i + 1,
            _ => spans.push(i..i + 1),
        }
        open = true;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use term_core::SelectionKind;

    fn frame(term: &Terminal) -> Frame<'static> {
        Frame {
            padding: 6.0,
            metrics: [8.0, 16.0, 14.0, 1.0],
            pixel_offset: 0.0,
            focused: true,
            cursor: term.cursor_position(),
            search: None,
            hover: None,
            atlas: 0,
            blink_visible: true,
            rerun: &[],
        }
    }

    fn damaged(damage: &mut Damage, term: &Terminal, frame: &Frame<'_>) -> Vec<usize> {
        let mut rows = Vec::new();
        damage.update(term, frame, &mut rows);
        rows.iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| i)
            .collect()
    }

    #[test]
    fn first_frame_damages_every_row_and_an_unchanged_one_none() {
        let mut term = Terminal::new(20, 5);
        term.advance(b"hello\r\nworld");
        let mut damage = Damage::default();
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 1, 2, 3, 4]);
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [] as [usize; 0]);
    }

    #[test]
    fn printing_damages_only_the_row_written() {
        let mut term = Terminal::new(20, 5);
        term.advance(b"one\r\ntwo\r\nthree");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        term.advance(b"!");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [2]);
        // A full-screen app repainting one row in place, as a status line or progress bar.
        term.advance(b"\x1b[?1049h\x1b[2J\x1b[Hstatus 1");
        damaged(&mut damage, &term, &frame(&term));
        term.advance(b"\x1b[4;1Hhi\x1b[1;8H2");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 3]);
    }

    #[test]
    fn moving_the_cursor_damages_the_rows_it_leaves_and_enters() {
        let mut term = Terminal::new(20, 5);
        term.advance(b"a\r\nb\r\nc");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        term.advance(b"\x1b[1;5H");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 2]);
        term.advance(b"\x1b[1;9H");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0]);
        term.advance(b"\x1b[?25l");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0]);
    }

    #[test]
    fn a_blink_phase_change_damages_only_rows_with_blinking_cells() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"plain\r\n\x1b[5mblink\x1b[25m\r\nplain");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        let hidden = Frame {
            blink_visible: false,
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &hidden), [1]);
        assert_eq!(damaged(&mut damage, &term, &hidden), [] as [usize; 0]);
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [1]);
    }

    #[test]
    fn a_combining_mark_damages_the_row_of_the_cell_it_joins() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"one\r\ntwo e");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        term.advance("\u{301}".as_bytes());
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [1]);
    }

    #[test]
    fn freeing_grapheme_clusters_damages_every_row() {
        let mut term = Terminal::new(20, 3);
        term.advance("a\u{301}\r\nb".as_bytes());
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        // Distinct clusters overwriting one cell until a release scan frees the older ones,
        // whose indices a later cluster may then reuse in a cell that compares equal.
        let releases = term.grapheme_releases();
        let mut i = 0;
        while term.grapheme_releases() == releases {
            let base = char::from_u32(0x4e00 + i).unwrap();
            term.advance(format!("\x1b[3;1H{base}\u{301}").as_bytes());
            i += 1;
        }
        term.advance(b"\x1b[3;1Hc");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 1, 2]);
    }

    #[test]
    fn freeing_links_damages_every_row() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"\x1b[4;58;5;1ma\r\nb");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        // Distinct underline colors overwriting one cell until a release scan frees the older
        // ones, whose indices a later color may then reuse in a cell that compares equal.
        let releases = term.link_releases();
        let mut i = 0u32;
        while term.link_releases() == releases {
            let (r, g, b) = (i & 0xff, i >> 8 & 0xff, i >> 16);
            term.advance(format!("\x1b[3;1H\x1b[58;2;{r};{g};{b}mc").as_bytes());
            i += 1;
        }
        term.advance(b"\x1b[3;1Hc");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 1, 2]);
    }

    #[test]
    fn scrolling_damages_rows_whose_content_moved() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"1\r\n2\r\n3");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        term.advance(b"\r\n4");
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 1, 2]);
        term.scroll_display(1);
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), [0, 1, 2]);
    }

    #[test]
    fn frame_wide_inputs_damage_every_row() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"some text");
        let mut damage = Damage::default();
        let all = [0, 1, 2];
        damaged(&mut damage, &term, &frame(&term));

        let unfocused = Frame {
            focused: false,
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &unfocused), all);
        assert_eq!(damaged(&mut damage, &term, &frame(&term)), all);

        let atlas_reset = Frame {
            atlas: 1,
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &atlas_reset), all);

        let smooth = Frame {
            pixel_offset: 3.0,
            atlas: 1,
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &smooth), [0, 1, 2, 3]);

        term.palette.foreground = (1, 2, 3);
        let base = Frame {
            atlas: 1,
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &base), all);
        assert_eq!(damaged(&mut damage, &term, &base), [] as [usize; 0]);

        damage.invalidate();
        assert_eq!(damaged(&mut damage, &term, &base), all);
    }

    #[test]
    fn same_row_drag_damages_one_row_per_move() {
        let mut term = Terminal::new(160, 60);
        let mut damage = Damage::default();
        let frame = frame(&term);
        damaged(&mut damage, &term, &frame);
        let point = |col| Point { line: 30, col };
        term.start_selection(point(0), SelectionKind::Simple);
        for col in 1..=40 {
            term.update_selection(point(col));
            assert_eq!(damaged(&mut damage, &term, &frame), [30]);
        }
        term.selection = None;
        assert_eq!(damaged(&mut damage, &term, &frame), [30]);
    }

    /// Applies only damaged rows, then compares with a full per-cell paint.
    fn check_highlights(
        term: &Terminal,
        offset: f32,
        damage: &mut Damage,
        painted: &mut Vec<Vec<bool>>,
    ) {
        let frame = Frame {
            pixel_offset: offset,
            ..frame(term)
        };
        let first = Damage::first_row(offset);
        let count = term.rows() + first.unsigned_abs();
        painted.resize_with(count, Vec::new);
        for i in damaged(damage, term, &frame) {
            let columns = damage.selected_columns(i);
            painted[i] = (0..term.cols())
                .map(|col| columns.as_ref().is_some_and(|s| s.contains(&col)))
                .collect();
        }
        let range = term.selection_range();
        for (i, row) in painted.iter().enumerate() {
            let row_index = first + i as isize;
            let full: Vec<_> = (0..term.cols())
                .map(|col| term.is_selected(range, term.viewport_to_point_signed(row_index, col)))
                .collect();
            assert_eq!(*row, full, "viewport row {row_index}");
        }
    }

    #[test]
    fn incremental_highlights_match_full_paint_across_wraps_and_scrolls() {
        let mut term = Terminal::new(10, 4);
        for _ in 0..4 {
            // Identical soft-wrapped pairs, so moving the viewport alone need
            // not change cells. The wide character occupies columns 3 and 4.
            term.advance("one界two  wrapped\r\n".as_bytes());
        }
        let mut damage = Damage::default();
        let mut painted = Vec::new();
        for kind in [
            SelectionKind::Simple,
            SelectionKind::Block,
            SelectionKind::Word,
            SelectionKind::Line,
            SelectionKind::CommandOutput,
        ] {
            term.scroll_display(2);
            let anchor = term.viewport_to_point(1, 4);
            term.start_selection(anchor, kind);
            // Grow, shrink, reverse across the anchor, then reach above the grid.
            for (row, col) in [(3, 7), (2, 3), (1, 6), (0, 1), (-1, 4)] {
                term.update_selection(term.viewport_to_point_signed(row, col));
                check_highlights(&term, 3.0, &mut damage, &mut painted);
            }
            term.scroll_display(-2);
            check_highlights(&term, 3.0, &mut damage, &mut painted);
            term.update_selection(term.viewport_to_point(3, 7));
            check_highlights(&term, 0.0, &mut damage, &mut painted);
            term.selection = None;
            check_highlights(&term, 0.0, &mut damage, &mut painted);
        }
    }

    #[test]
    fn search_and_hover_changes_damage_every_row() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"find me\r\nme too");
        let mut damage = Damage::default();
        damaged(&mut damage, &term, &frame(&term));
        let matches = [Match {
            start: Point { line: 0, col: 5 },
            end: Point { line: 0, col: 6 },
        }];
        let searching = Frame {
            search: Some((&matches, Some(0))),
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &searching), [0, 1, 2]);
        assert_eq!(damaged(&mut damage, &term, &searching), [] as [usize; 0]);
        let link = Link {
            uri: "https://example.com".into(),
            start: Point { line: 1, col: 0 },
            end: Point { line: 1, col: 1 },
            pieces: Vec::new(),
            osc8: false,
        };
        let hovering = Frame {
            hover: Some(&link),
            ..frame(&term)
        };
        assert_eq!(damaged(&mut damage, &term, &hovering), [0, 1, 2]);
        assert_eq!(damaged(&mut damage, &term, &hovering), [] as [usize; 0]);
    }

    fn spans(lens: &[u32], pending: &[bool], offsets: &mut Vec<u32>) -> Vec<(usize, usize)> {
        upload_spans(lens, pending, offsets)
            .into_iter()
            .map(|span| (span.start, span.end))
            .collect()
    }

    #[test]
    fn upload_covers_rebuilt_rows_and_rows_shifted_after_them() {
        let mut offsets = Vec::new();
        assert_eq!(spans(&[2, 0, 3, 1], &[false; 4], &mut offsets), [(0, 4)]);
        assert_eq!(offsets, [0, 2, 2, 5]);
        assert_eq!(spans(&[2, 0, 3, 1], &[false; 4], &mut offsets), []);
        // Same length: only the rebuilt rows.
        assert_eq!(
            spans(&[2, 0, 3, 1], &[true, false, true, false], &mut offsets),
            [(0, 1), (2, 3)]
        );
        // A row that grows moves every row after it.
        assert_eq!(
            spans(&[2, 1, 3, 1], &[false, true, false, false], &mut offsets),
            [(1, 4)]
        );
        assert_eq!(offsets, [0, 2, 3, 6]);
    }
}
