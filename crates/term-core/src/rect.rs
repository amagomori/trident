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
                    let src = &self.screen().lines[area.rows.start + i].cells;
                    copy.clear();
                    copy.extend_from_slice(&src[area.cols.start..area.cols.start + width]);
                    if copy[0].flags.contains(Flags::WIDE_SPACER) {
                        copy[0] = blank;
                    }
                    if copy[width - 1].flags.contains(Flags::WIDE) {
                        copy[width - 1] = blank;
                    }
                    self.erase_cells(row + i, col, col + width);
                    let line = &mut self.lines_mut()[row + i];
                    line.cells[col..col + width].copy_from_slice(&copy);
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
            ('J', 3) => {
                self.erase_saved_lines();
                return;
            }
            _ => return,
        };
        for r in rows {
            let cols = match (r == row, mode) {
                (true, 0) => col..self.cols,
                (true, 1) => 0..col + 1,
                _ => 0..self.cols,
            };
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
        let cols = whole_glyphs(&self.screen().lines[row].cells, cols);
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
