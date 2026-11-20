//! Horizontal bounds for VT420 cursor movement and rectangular scrolling.

use super::{Cell, Flags, Row, Terminal};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LrMode {
    Off,
    Full,
    Partial,
}

impl Terminal {
    pub(super) fn has_partial_width(&self) -> bool {
        self.lr_margin_mode == LrMode::Partial
    }

    pub(super) fn in_horizontal_margins(&self) -> bool {
        (self.scroll_left..=self.scroll_right).contains(&self.cursor.col)
    }

    pub(super) fn left_bound(&self) -> usize {
        if self.cursor.col >= self.scroll_left || self.modes.origin {
            self.scroll_left
        } else {
            0
        }
    }

    pub(super) fn right_bound(&self) -> usize {
        if self.cursor.col <= self.scroll_right {
            self.scroll_right
        } else {
            self.cols - 1
        }
    }

    pub(super) fn address_col(&self, col: usize) -> usize {
        if self.modes.origin {
            (self.scroll_left + col).min(self.scroll_right)
        } else {
            col.min(self.cols - 1)
        }
    }

    pub(super) fn reported_position(&self) -> (usize, usize) {
        let (mut row, mut col) = (self.cursor.row, self.cursor.col);
        if self.modes.origin {
            row = row.saturating_sub(self.scroll_top);
            col = col.saturating_sub(self.scroll_left);
        }
        (row + 1, col + 1)
    }

    pub(super) fn reset_lr_margins(&mut self) {
        self.scroll_left = 0;
        self.scroll_right = self.cols - 1;
        if self.lr_margin_mode != LrMode::Off {
            self.lr_margin_mode = LrMode::Full;
        }
    }

    pub(super) fn set_lr_margins(&mut self, left: usize, right: usize) {
        let right = right.min(self.cols - 1);
        if left < right {
            self.scroll_left = left;
            self.scroll_right = right;
            self.lr_margin_mode = if left == 0 && right + 1 == self.cols {
                LrMode::Full
            } else {
                LrMode::Partial
            };
            self.goto(0, 0);
        }
    }

    pub(super) fn wrap_line(&mut self) {
        self.sync_cursor_row();
        let row = self.cursor.row;
        if self.has_partial_width() {
            let (left, right) = (self.scroll_left, self.scroll_right);
            self.lines_mut()[row].set_split_wrapped(left, right, true);
        } else {
            self.lines_mut()[row].set_wrapped(true);
        }
        self.linefeed();
        self.cursor.col = self.scroll_left;
    }

    pub(super) fn scroll_up(&mut self, top: usize, bottom: usize, n: usize) {
        if self.has_partial_width() {
            self.scroll_rectangle(top, bottom, n, false);
        } else {
            self.scroll_up_region(top, bottom, n);
        }
    }

    pub(super) fn scroll_down(&mut self, top: usize, bottom: usize, n: usize) {
        if self.has_partial_width() {
            self.scroll_rectangle(top, bottom, n, true);
        } else {
            self.scroll_down_region(top, bottom, n);
        }
    }

    /// Only the cells move. Neither scrollback nor absolute line numbers advance.
    #[cold]
    pub(super) fn scroll_rectangle(&mut self, top: usize, bottom: usize, n: usize, down: bool) {
        let (left, end) = (self.scroll_left, self.scroll_right + 1);
        let n = n.min(bottom - top + 1);
        let blank = Cell::blank(&self.cursor.template);
        self.invalidate_output_ends_in_region(top, bottom, 0);
        if !self.screen().placements.is_empty() {
            let delta = if down { n as isize } else { -(n as isize) };
            self.scroll_placements(top, bottom, left..end, delta, false);
        }
        if !self.shell.input_writes.is_empty() {
            for row in top..=bottom {
                self.erase_input_writes(row, left, end);
            }
        }
        let rows = &mut self.lines_mut()[top..=bottom];
        for row in rows.iter_mut() {
            split_pair(row, left, blank);
            split_pair(row, end, blank);
            row.touch_all();
        }
        let kept = rows.len() - n;
        for i in 0..kept {
            let i = if down { kept - 1 - i } else { i };
            let (above, below) = rows.split_at_mut(i + n);
            let (dst, src) = if down {
                (&mut below[0], &mut above[i])
            } else {
                (&mut above[i], &mut below[0])
            };
            dst.set_split_wrapped(left, end - 1, src.split_wrapped(left, end - 1));
            if src.may_have_placeholder() {
                dst.note_placeholder();
            }
            dst.cells[left..end].swap_with_slice(&mut src.cells[left..end]);
        }
        let cleared = if down {
            &mut rows[..n]
        } else {
            &mut rows[kept..]
        };
        for row in cleared {
            row.set_split_wrapped(left, end - 1, false);
            row.cells[left..end].fill(blank);
        }
    }

    pub(super) fn horizontal_index(&mut self, forward: bool) {
        let edge = if forward {
            self.scroll_right
        } else {
            self.scroll_left
        };
        if self.cursor.col != edge {
            let col = if forward {
                self.cursor.col + 1
            } else {
                self.cursor.col.saturating_sub(1)
            };
            self.move_col(col);
        } else if (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row) {
            self.invalidate_output_ends_in_region(self.scroll_top, self.scroll_bottom, 0);
            for row in self.scroll_top..=self.scroll_bottom {
                self.shift_row(row, self.scroll_left, 1, !forward);
            }
        }
        self.cursor.pending_wrap = false;
    }
}

/// A cell pair cannot straddle two regions that move independently.
fn split_pair(row: &mut Row, at: usize, blank: Cell) {
    if at > 0
        && row
            .cells
            .get(at)
            .is_some_and(|c| c.flags.contains(Flags::WIDE_SPACER))
    {
        row.cells[at - 1..=at].fill(blank);
    }
}

pub(super) fn shift_cells(
    row: &mut Row,
    col: usize,
    end: usize,
    n: usize,
    insert: bool,
    blank: Cell,
) {
    let n = n.min(end - col);
    split_pair(row, col, blank);
    split_pair(row, end, blank);
    split_pair(row, if insert { end - n } else { col + n }, blank);
    if insert {
        row.cells.copy_within(col..end - n, col + n);
        row.cells[col..col + n].fill(blank);
    } else {
        row.cells.copy_within(col + n..end, col);
        row.cells[end - n..end].fill(blank);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_a_split_wide_glyph_does_not_move_its_other_half() {
        let mut t = Terminal::new(7, 3);
        t.advance("A界12界\r\nB界34界\r\nC界56界".as_bytes());
        t.advance(b"\x1b[?69h\x1b[3;6s\x1b[S");
        let rows: Vec<_> = t.screen().lines.iter().map(Row::text).collect();
        assert_eq!(rows, ["A  34", "B  56", "C"]);
        assert_eq!(t.scrollback_len(), 0);
    }

    #[test]
    fn resizing_removes_old_bounds_but_keeps_margin_mode() {
        let mut t = Terminal::new(12, 3);
        t.advance(b"\x1b[?69h\x1b[7;11s\x1b[?6h");
        t.resize(5, 3);
        t.advance(b"\x1b[HabcdeF\x1bP$qs\x1b\\");
        assert_eq!(t.take_responses(), b"\x1bP1$r1;5s\x1b\\");
        assert_eq!(t.screen().lines[0].text(), "abcde");
        t.advance(b"\x1b[2;4s\x1b[1;1HX");
        assert_eq!(t.screen().lines[0].text(), "aXcde");
    }
}
