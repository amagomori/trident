//! Rewrapping the primary screen and its scrollback to a new width.

use crate::{Cell, Flags, Row, RowFlags, Terminal, Zone};

/// Where an old row went: its logical line became new rows `first..first + count`,
/// and the row's cells start `base` cells into that line.
#[derive(Clone, Copy)]
struct OldRow {
    first: usize,
    count: usize,
    base: usize,
    /// Cells the row contributes to its line; columns past them belong to the next row,
    /// except on the line's last row (`usize::MAX`).
    len: usize,
}

/// Maps positions in the old rows to positions in the rewrapped ones.
struct Map {
    old: Vec<OldRow>,
    /// The offset of each new row's first cell in its logical line.
    starts: Vec<usize>,
    old_first: u64,
    old_len: usize,
    new_len: usize,
}

impl Map {
    /// The new row and column of old row `i`, column `col`. With `end`, a column just past
    /// a full row stays at that row's end instead of the start of the next one.
    fn point(&self, i: usize, col: usize, end: bool) -> (usize, usize) {
        let o = self.old[i];
        let at = o.base + col.min(o.len);
        let starts = &self.starts[o.first..o.first + o.count];
        let mut k = starts.partition_point(|&s| s <= at).saturating_sub(1);
        if end && k > 0 && starts[k] == at {
            k -= 1;
        }
        (o.first + k, at - starts[k])
    }

    /// The new absolute line and column of an absolute old position.
    fn line(&self, line: u64, col: usize, end: bool) -> (u64, usize) {
        let Some(i) = line.checked_sub(self.old_first).map(|i| i as usize) else {
            return (line, col);
        };
        if i >= self.old_len {
            return (line - self.old_len as u64 + self.new_len as u64, col);
        }
        let (row, col) = self.point(i, col, end);
        (self.old_first + row as u64, col)
    }
}

fn wide_wrap_blank() -> Cell {
    let mut blank = Cell::default();
    blank.flags.insert(Flags::WIDE_WRAP);
    blank
}

/// Copies the shell marks of an old row onto the rewrapped rows its cells went to.
fn move_marks(
    rows: &mut [Row],
    map: &Map,
    i: usize,
    flags: RowFlags,
    prompt_col: u32,
    output_col: u16,
    len: usize,
) {
    if flags.contains(RowFlags::PROMPT) {
        let from = map.point(i, 0, false).0;
        let to = map.point(i, len, true).0;
        for row in &mut rows[from..=to] {
            row.flags.insert(RowFlags::PROMPT);
        }
    }
    if flags.contains(RowFlags::OUTPUT_START) {
        let (row, col) = map.point(i, output_col as usize, false);
        rows[row].flags.insert(RowFlags::OUTPUT_START);
        rows[row].output_col = u16::try_from(col).unwrap_or(u16::MAX);
    }
    let output_end = flags.contains(RowFlags::OUTPUT_END);
    if output_end || flags.contains(RowFlags::PROMPT_START) {
        // Without an output end on the row, `prompt_col` is where the prompt starts.
        let (row, col) = map.point(i, prompt_col as usize, output_end);
        let row = &mut rows[row];
        for flag in [
            RowFlags::OUTPUT_END,
            RowFlags::PROMPT_START,
            RowFlags::OUTPUT_BEFORE_PROMPT,
            RowFlags::FAILED,
            RowFlags::SUCCEEDED,
        ] {
            if flags.contains(flag) {
                row.flags.insert(flag);
            }
        }
        row.prompt_col = u32::try_from(col).unwrap_or(u32::MAX - 1);
    }
}

impl Terminal {
    /// Rejoins the soft-wrapped lines of the primary screen and its scrollback and wraps
    /// them again at `cols`, keeping the cursor on the cell it was on (on a line being
    /// edited, on its row in the line), and sizes the screen to `rows`. `editing` is
    /// whether a line editor owns the cursor's line. `view_top` is the viewport's top line
    /// when scrolled back; returns where that line went. `points` are moved to where
    /// their cells went.
    pub(crate) fn reflow_primary(
        &mut self,
        cols: usize,
        rows: usize,
        view_top: Option<u64>,
        editing: bool,
    ) -> Option<u64> {
        let old_rows = self.rows;
        let history_len = self.primary.scrollback.len();
        let first = self.history - history_len as u64;
        let gaps = self.gap_rows(first, history_len + self.primary.lines.len());
        // A prefix of independent lines that already fit has an identity position map.
        // Leave enough rows at the end to fill the live screen if reflow pulls it up.
        let keep = self
            .primary
            .scrollback
            .iter()
            .take(
                history_len
                    .saturating_sub(rows)
                    .min(gaps.first().map_or(history_len, |g| g.0)),
            )
            .take_while(|row| !row.wrapped() && !row.has_hidden() && row.cells.len() <= cols)
            .count();
        let back = history_len - keep;
        let old_first = first + keep as u64;
        // The primary cursor: the live one, or the one saved when the alternate screen
        // was entered. A cursor past the end of its row keeps the blanks before it, so
        // text typed after the resize lands where it would have.
        let cursor = if self.alt_active {
            self.primary.saved.map(|s| s.cursor)
        } else {
            Some(self.cursor)
        };
        let anchor = cursor.map_or((old_rows - 1, 0, false), |c| (c.row, c.col, c.pending_wrap));
        let cursor_index = back + anchor.0;
        let cursor_at = anchor.1 + usize::from(anchor.2);
        // A cursor past the end of its line is wrapped like the text before it: at the
        // end of a full row it waits there to wrap, as if the line had been typed anew.
        let mut cursor_end = false;
        // Budget placeholders stay one row, rewritten for the new width.
        let mut gaps = gaps
            .into_iter()
            .map(|(i, text)| (i - keep, text))
            .peekable();

        // Draining keeps the scrollback's buffer and avoids `make_contiguous`, whose
        // `rotate_left::<Row>` would then stop being inlined into scrolling.
        let mut old = Vec::with_capacity(back + self.primary.lines.len());
        old.extend(self.primary.scrollback.drain(keep..));
        old.append(&mut self.primary.lines);
        let old_len = old.len();

        let mut new: Vec<Row> = Vec::with_capacity(old_len);
        let mut starts = Vec::with_capacity(old_len);
        let mut map_rows = Vec::with_capacity(old_len);
        // Shell marks to move once their rows exist: (old index, flags, prompt_col, output_col, len).
        let mut marks = Vec::new();
        // The cursor's wrapped line: its first old row, and whether a line editor is
        // editing it (one owns the terminal, or the cursor is in the shell's input zone).
        let mut cursor_line = None;
        let mut line = Vec::new();
        let mut rows_iter = old.into_iter().enumerate().peekable();
        while let Some((i, mut row)) = rows_iter.next() {
            if let Some((_, text)) = gaps.next_if(|(at, _)| *at == i) {
                map_rows.push(OldRow {
                    first: new.len(),
                    count: 1,
                    base: 0,
                    len: usize::MAX,
                });
                starts.push(0);
                new.push(crate::budget::placeholder_row(cols, &text));
                continue;
            }
            let has_cursor = cursor.is_some() && i == cursor_index;
            if !row.wrapped()
                && !row.has_hidden()
                && (!has_cursor || cursor_at <= cols)
                && row
                    .cells
                    .get(cols..)
                    .is_none_or(|tail| tail.iter().all(|c| *c == Cell::default()))
            {
                // Unstored trailing cells of a hard history row are default blanks.
                row.cells.truncate(cols);
                map_rows.push(OldRow {
                    first: new.len(),
                    count: 1,
                    base: 0,
                    len: usize::MAX,
                });
                starts.push(0);
                new.push(row);
                continue;
            }

            // Gather the whole logical line.
            let first = new.len();
            let first_old = map_rows.len();
            line.clear();
            let mut next = Some((i, row));
            let mut possible_placeholder = false;
            while let Some((i, mut row)) = next.take() {
                let wrapped = row.wrapped();
                let flags = row.flags;
                let (prompt_col, output_col) = (row.prompt_col, row.output_col);
                let mut cells = row.take_cells();
                possible_placeholder |= row.may_have_placeholder();
                let len = if wrapped {
                    while cells
                        .last()
                        .is_some_and(|c| c.flags.contains(Flags::WIDE_WRAP))
                    {
                        cells.pop();
                    }
                    cells.len()
                } else {
                    let used = cells
                        .iter()
                        .rposition(|c| *c != Cell::default() && !c.flags.contains(Flags::WIDE_WRAP))
                        .map_or(0, |c| c + 1);
                    cells.truncate(used);
                    if cursor.is_some() && i == cursor_index && cells.len() <= cursor_at {
                        cells.resize(cursor_at, Cell::default());
                        cursor_end = true;
                    }
                    usize::MAX
                };
                let row_len = cells.len();
                map_rows.push(OldRow {
                    first,
                    count: 0,
                    base: line.len(),
                    len,
                });
                line.append(&mut cells);
                let mut flags = flags;
                flags.remove(RowFlags::WRAPPED);
                if flags != RowFlags::default() {
                    marks.push((i, flags, prompt_col, output_col, row_len));
                }
                if wrapped {
                    next = rows_iter.next();
                }
            }

            if !self.alt_active && (first_old..map_rows.len()).contains(&cursor_index) {
                cursor_line = Some((first_old, editing || self.shell.zone == Zone::Input));
            }
            // Wrap it at the new width.
            let mut pos = 0;
            loop {
                let mut end = (pos + cols).min(line.len());
                let wide_cut = end < line.len() && line[end - 1].flags.contains(Flags::WIDE);
                if wide_cut {
                    end -= 1;
                }
                let mut cells = line[pos..end].to_vec();
                if wide_cut {
                    cells.push(wide_wrap_blank());
                }
                let has_base = possible_placeholder
                    && cells
                        .iter()
                        .any(|cell| self.base_char(cell) == crate::placeholder::CHAR);
                let mut row = Row::from_cells(cells, RowFlags::default(), has_base);
                row.set_wrapped(end < line.len());
                starts.push(pos);
                new.push(row);
                if end >= line.len() {
                    break;
                }
                pos = end;
            }
            let count = new.len() - first;
            for o in &mut map_rows[first_old..] {
                o.count = count;
            }
        }

        let map = Map {
            old: map_rows,
            starts,
            old_first,
            old_len,
            new_len: new.len(),
        };
        for point in &mut self.tracked {
            (point.line, point.col) = map.line(point.line, point.col, false);
        }
        for (i, flags, prompt_col, output_col, len) in marks.drain(..) {
            move_marks(&mut new, &map, i, flags, prompt_col, output_col, len);
        }

        // The screen keeps its top line and grows or shrinks below it, as long as the
        // cursor stays on screen; a cursor on the bottom row stays there.
        let (mut cursor_row, mut cursor_col) = map.point(cursor_index, cursor_at, cursor_end);
        // A line editor redraws the line it is editing after a resize, moving up from the
        // cursor as many rows as the cursor was below the line's start. On the last line
        // being edited, the cursor keeps that row and its column so the redraw starts on
        // the line's first row instead of leaving a stale tail.
        if let Some((first_old, true)) = cursor_line {
            let OldRow { first, count, .. } = map.old[cursor_index];
            let row = first + (cursor_index - first_old);
            if row != cursor_row && new[first + count..].iter().all(Row::is_blank) {
                cursor_row = row;
                cursor_col = anchor.1.min(cols - 1);
                new.resize_with(new.len().max(row + 1), || Row::new(cols, &Cell::default()));
            }
        }
        let lowest = (cursor_row + 1).saturating_sub(rows);
        let mut top = if anchor.0 == old_rows - 1 {
            lowest
        } else {
            map.point(back, 0, false).0.clamp(lowest, cursor_row)
        };
        // Pull the screen up to keep rows below the cursor, as far as the cursor allows.
        let used = new
            .iter()
            .rposition(|r| !r.is_blank() || r.flags != RowFlags::default())
            .map_or(0, |r| r + 1);
        if used > top + rows {
            top = (used - rows).min(cursor_row);
        }
        let mut lines = new.split_off(top);
        lines.truncate(rows);
        lines.resize_with(rows, || Row::new(cols, &Cell::default()));
        for row in &mut lines {
            row.pad_to(cols);
        }
        self.primary.scrollback.extend(new);
        self.primary.lines = lines;
        self.history = old_first + top as u64;

        let screen_row = cursor_row - top;
        let (col, pending) = if cursor_col >= cols {
            (cols - 1, true)
        } else {
            (cursor_col, false)
        };
        if self.alt_active {
            if let Some(saved) = &mut self.primary.saved {
                saved.cursor.row = screen_row;
                saved.cursor.col = col;
                saved.cursor.pending_wrap = pending;
            }
        } else {
            self.cursor.row = screen_row;
            self.cursor.col = col;
            self.cursor.pending_wrap = pending;
            if let Some(saved) = &mut self.primary.saved {
                let pending = saved.cursor.pending_wrap;
                let (row, col) = map.point(
                    back + saved.cursor.row.min(old_rows - 1),
                    saved.cursor.col + usize::from(pending),
                    pending,
                );
                saved.cursor.row = row.saturating_sub(top).min(rows - 1);
                saved.cursor.col = col.min(cols - 1);
                saved.cursor.pending_wrap = pending && col >= cols;
            }
        }

        let shell = &mut self.shell;
        shell.mark_line = shell.mark_line.map(|l| map.line(l, 0, false).0);
        shell.prompt_line = shell.prompt_line.map(|l| map.line(l, 0, false).0);
        shell.input_start = shell.input_start.map(|(l, c)| map.line(l, c, false));
        for command in &mut shell.commands {
            command.prompt_line = map.line(command.prompt_line, 0, false).0;
            (command.input_line, command.input_col) =
                map.line(command.input_line, command.input_col, false);
            command.next_prompt = command.next_prompt.map(|l| map.line(l, 0, false).0);
        }
        let writes = std::mem::take(&mut shell.input_writes);
        for (line, cols) in writes {
            for col in cols {
                let (line, col) = map.line(line, col, false);
                self.record_input_write(line, col..col + 1);
            }
        }
        self.remap_budget_lines(|l| map.line(l, 0, false).0);
        if !self.primary.placements.is_empty() {
            for p in &mut self.primary.placements {
                let (line, col) = map.line(p.line, p.col, false);
                p.line = line;
                p.col = col;
            }
            self.images.generation += 1;
        }

        // Rows past the scrollback limit go as if scrolled off, so the command budget
        // trims an over-budget command before older commands lose their lines.
        self.cols = cols;
        self.display_offset = view_top.map_or(0, |l| {
            (self.history.saturating_sub(map.line(l, 0, false).0) as usize)
                .min(self.primary.scrollback.len())
        });
        let trimmed = if self.max_scrollback == 0 {
            let all = !self.primary.scrollback.is_empty();
            self.primary.scrollback.clear();
            all
        } else {
            self.trim_scrollback()
        };
        if trimmed {
            self.shrunk = true;
            if !self.primary.placements.is_empty() {
                let first = self.history - self.primary.scrollback.len() as u64;
                self.drop_placements_before(first);
            }
        }
        self.refresh_budget_limit();
        let offset = std::mem::take(&mut self.display_offset);
        view_top.map(|_| self.history - offset.min(self.primary.scrollback.len()) as u64)
    }
}
