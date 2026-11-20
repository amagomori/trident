//! VT420 editing addresses cells on the active page without moving the cursor.

use std::ops::Range;
use vte::Params;

use super::{Cell, Flags, Terminal, links::Protection, perform::arg};

struct Area {
    rows: Range<usize>,
    cols: Range<usize>,
}

impl Area {
    fn point(&self, params: &Params, index: usize) -> (usize, usize) {
        let at = |i, axis: &Range<usize>| {
            (axis.start + usize::from(arg(params, i, 1)) - 1).min(axis.end - 1)
        };
        (at(index, &self.rows), at(index + 1, &self.cols))
    }

    fn rectangle(&self, params: &Params, index: usize) -> Option<Area> {
        let (top, left) = self.point(params, index);
        let end = |i, axis: &Range<usize>| match arg(params, i, 0) {
            0 => axis.end,
            n => (axis.start + usize::from(n)).min(axis.end),
        };
        let rows = top..end(index + 2, &self.rows);
        let cols = left..end(index + 3, &self.cols);
        (!rows.is_empty() && !cols.is_empty()).then_some(Area { rows, cols })
    }
}

impl Terminal {
    #[cold]
    pub(super) fn edit_columns(&mut self, n: usize, insert: bool) {
        if self.in_horizontal_margins()
            && (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row)
        {
            self.invalidate_output_ends_in_region(self.scroll_top, self.scroll_bottom, 0);
            for row in self.scroll_top..=self.scroll_bottom {
                self.shift_row(row, self.cursor.col, n, insert);
            }
        }
    }

    #[cold]
    pub(super) fn edit_rectangle(&mut self, params: &Params, action: char) {
        let page = if self.modes.origin {
            Area {
                rows: self.scroll_top..self.scroll_bottom + 1,
                cols: self.scroll_left..self.scroll_right + 1,
            }
        } else {
            Area {
                rows: 0..self.rows,
                cols: 0..self.cols,
            }
        };
        let Some(area) = page.rectangle(params, usize::from(action == 'x')) else {
            return;
        };
        match action {
            'v' => {
                // Every page number refers to our single active page.
                let (row, col) = page.point(params, 5);
                let height = area.rows.len().min(page.rows.end - row);
                let width = area.cols.len().min(page.cols.end - col);
                let blank = Cell::blank(&self.cursor.template);
                let mut copy = Vec::with_capacity(width);
                for i in 0..height {
                    // Read before writing when either dimension overlaps.
                    let i = if row > area.rows.start {
                        height - i - 1
                    } else {
                        i
                    };
                    let src = &self.screen().lines[area.rows.start + i];
                    let possible_placeholder = src.may_have_placeholder();
                    copy.clear();
                    copy.extend(
                        src.cells[area.cols.start..area.cols.start + width]
                            .iter()
                            .map(|&cell| {
                                let mut cell = cell;
                                cell.flags.remove(Flags::WIDE_WRAP | Flags::PAD);
                                cell
                            }),
                    );
                    if copy[0].flags.contains(Flags::WIDE_SPACER) {
                        copy[0] = blank;
                    }
                    if copy[width - 1].flags.contains(Flags::WIDE) {
                        copy[width - 1] = blank;
                    }
                    self.erase_cells(row + i, col, col + width);
                    let line = &mut self.lines_mut()[row + i];
                    line.cells[col..col + width].copy_from_slice(&copy);
                    if possible_placeholder {
                        line.note_placeholder();
                    }
                    line.touch(col + width);
                }
            }
            'x' => {
                let ch = arg(params, 0, 32);
                if !matches!(ch, 32..=126 | 160..=255) {
                    return;
                }
                let fill = Cell {
                    c: self.map_character(char::from(ch as u8)),
                    ..self.cursor.template
                };
                for row in area.rows {
                    self.erase_cells(row, area.cols.start, area.cols.end);
                    let line = &mut self.lines_mut()[row];
                    line.cells[area.cols.clone()].fill(fill);
                    line.touch(area.cols.end);
                }
            }
            'z' | '{' => {
                for row in area.rows {
                    if action == 'z' {
                        self.erase_cells(row, area.cols.start, area.cols.end);
                    } else {
                        self.erase_unprotected(row, area.cols.clone(), Protection::DEC, true);
                    }
                }
            }
            'r' | 't' => {
                let stream = self.reports.decsace != 2;
                for row in area.rows.clone() {
                    let from = if stream && row != area.rows.start {
                        page.cols.start
                    } else {
                        area.cols.start
                    };
                    let to = if stream && row + 1 != area.rows.end {
                        page.cols.end
                    } else {
                        area.cols.end
                    };
                    let line = &mut self.lines_mut()[row];
                    let cols = whole_glyphs(&line.cells, from..to);
                    line.touch(cols.end);
                    for cell in &mut line.cells[cols] {
                        let attrs = params.iter().skip(4).map(|p| p[0]);
                        if params.len() <= 4 {
                            change_attribute(&mut cell.flags, 0, action == 't');
                        } else {
                            for attr in attrs {
                                change_attribute(&mut cell.flags, attr, action == 't');
                            }
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn has_protected_cells(&self, rows: Range<usize>, protected: Protection) -> bool {
        self.reports.protection_used.contains(protected)
            && self.screen().lines[rows].iter().any(|line| {
                line.cells
                    .iter()
                    .chain(line.hidden_cells())
                    .any(|cell| self.links.protection(cell.hyperlink).contains(protected))
            })
    }

    #[cold]
    pub(super) fn selective_erase(&mut self, action: char, mode: u16, protected: Protection) {
        let (row, col) = (self.cursor.row, self.cursor.col);
        let rows = match (action, mode) {
            ('K', 0..=2) => row..row + 1,
            ('J', 0) => row..self.rows,
            ('J', 1) => 0..row + 1,
            ('J', 2) => {
                self.prompt_redraw = false;
                0..self.rows
            }
            _ => return,
        };
        for r in rows {
            let cols = match (r == row, mode) {
                (true, 0) => col..usize::MAX,
                (true, 1) => 0..col + 1,
                _ => 0..usize::MAX,
            };
            if cols.end == usize::MAX {
                self.lines_mut()[r].set_wrapped(false);
            }
            self.erase_unprotected(r, cols, protected, false);
        }
    }

    pub(super) fn erase_unprotected(
        &mut self,
        row: usize,
        cols: Range<usize>,
        protected: Protection,
        keep_style: bool,
    ) {
        let width = self.cols;
        let line = &mut self.lines_mut()[row];
        // Restore the tail so a clipped wide glyph is checked as one protected character.
        let restore = cols.end >= width && line.has_hidden();
        if restore {
            line.cells = line.take_cells();
        }
        let cols = whole_glyphs(&line.cells, cols.start..cols.end.min(line.cells.len()));
        let mut at = cols.start;
        while at < cols.end {
            let start = at;
            let cells = &self.screen().lines[row].cells;
            while at < cols.end
                && !self
                    .links
                    .protection(cells[at].hyperlink)
                    .contains(protected)
            {
                at += 1;
            }
            if start != at {
                if keep_style {
                    if !self.screen().placements.is_empty() {
                        self.erase_placements(row, row, start, at);
                    }
                    if !self.shell.input_writes.is_empty() {
                        self.erase_input_writes(row, start, at);
                    }
                    let line = &mut self.lines_mut()[row];
                    for cell in &mut line.cells[start..at] {
                        cell.c = ' ';
                        cell.flags.remove(
                            Flags::WIDE
                                | Flags::WIDE_SPACER
                                | Flags::WIDE_WRAP
                                | Flags::PAD
                                | Flags::GRAPHEME,
                        );
                    }
                    line.touch(at);
                } else {
                    self.erase_cells(row, start, at);
                }
            }
            at += 1;
        }
        if restore {
            self.lines_mut()[row].resize(width);
        }
    }
}

fn whole_glyphs(cells: &[Cell], mut cols: Range<usize>) -> Range<usize> {
    if cols.start > 0 && cells[cols.start].flags.contains(Flags::WIDE_SPACER) {
        cols.start -= 1;
    }
    if cols.end < cells.len() && cells[cols.end - 1].flags.contains(Flags::WIDE) {
        cols.end += 1;
    }
    cols
}

fn change_attribute(flags: &mut Flags, param: u16, reverse: bool) {
    for (on, off, flag) in [
        (1, 22, Flags::BOLD),
        (4, 24, Flags::UNDERLINE),
        (5, 25, Flags::BLINK),
        (7, 27, Flags::INVERSE),
    ] {
        if param != 0 && param != on && (reverse || param != off) {
            continue;
        }
        let set = if reverse {
            !flags.contains(flag)
        } else {
            param == on
        };
        if set {
            flags.insert(flag);
        } else {
            flags.remove(flag);
            if flag == Flags::UNDERLINE {
                flags.remove(Flags::UNDERLINE_STYLE);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_vt420_honors_rectangle_and_column_edits() {
        let mut t = Terminal::new(4, 2);
        for (level, expected) in [(63, "AB"), (64, " XB")] {
            t.advance(format!("\x1b[{level};1\"p").as_bytes());
            t.advance(b"AB\x1b[88;1;1;1;1$x\r\x1b[1'}");
            assert_eq!(t.viewport_row(0).text(), expected);
        }
    }

    #[test]
    fn column_mode_clear_is_not_a_selective_erase() {
        let mut t = Terminal::new(80, 3);
        t.advance(b"\x1b[1\"qD\x1b[0\"q\x1bVI\x1bW\x1b[?2J");
        assert_eq!(t.viewport_row(0).text(), "DI");
        t.advance(b"\x1b[?40h\x1b[?3h");
        assert!(t.viewport_row(0).text().is_empty());
    }

    #[test]
    fn soft_reset_ends_iso_protection_for_ordinary_erases() {
        for erase in [b"\x1b[2J".as_slice(), b"\x1b[2K", b"\x1b[X"] {
            let mut t = Terminal::new(4, 2);
            t.advance(b"\x1bVP\x1bW\r");
            t.advance(erase);
            assert_eq!(t.viewport_row(0).text(), "P");
            t.advance(b"\x1b[!p");
            assert_eq!(t.viewport_row(0).text(), "P");
            t.advance(erase);
            assert!(t.viewport_row(0).text().is_empty());
        }
    }

    #[test]
    fn copied_wrap_padding_is_a_selectable_space() {
        let mut t = Terminal::new(4, 3);
        t.advance("abc界\x1b[3;1HXYZ\x1b[1;4;1;4;1;3;2;1$v".as_bytes());
        t.start_selection(t.viewport_to_point(2, 0), crate::SelectionKind::Simple);
        t.update_selection(t.viewport_to_point(2, 2));
        assert_eq!(t.selection_text().as_deref(), Some("X Z"));
    }

    #[test]
    fn erase_to_end_checks_hidden_protected_wide_glyphs() {
        for seq in [b"\x1b[?2K".as_slice(), b"\x1b[?0J", b"\x1b[?2J"] {
            let mut t = Terminal::new(9, 2);
            t.advance("\x1b[?1049hab\x1b[1\"q界\x1b[0\"qXY\x1b[1\"q!\x1b[0\"q\r".as_bytes());
            t.resize(3, 2);
            t.advance(seq);
            t.resize(9, 2);
            assert_eq!(t.viewport_row(0).text(), "  界  !", "{seq:?}");
        }
    }
}
