//! Per-command scrollback budget (KAN-71): once history is full, a command whose output
//! (located by its OSC 133 marks) outgrows its share of `max_scrollback` loses rows from
//! its middle instead of evicting older commands. The head and tail stay, with one
//! placeholder row counting what was dropped. Everything here runs only on eviction.

use crate::{Cell, Flags, Point, Row, Terminal};

pub(crate) const DEFAULT_SHARE: f64 = 0.5;

/// Budgets below this many lines are too small to split, so eviction stays FIFO.
const MIN_BUDGET: u64 = 16;

pub(crate) struct Budget {
    enabled: bool,
    share: f64,
    /// Prompt lines of finished commands that were over budget when they ended, oldest first.
    pending: Vec<u64>,
    /// `history` past which the running command is over budget; `u64::MAX` when none is.
    pub limit: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            enabled: true,
            share: DEFAULT_SHARE,
            pending: Vec::new(),
            limit: u64::MAX,
        }
    }
}

/// The placeholder row standing in for a command's dropped output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Gap {
    pub line: u64,
    pub dropped: u64,
}

/// Rows `start..end` were removed and everything above moved down by `end - start` lines.
#[derive(Clone, Copy)]
struct Cut {
    start: u64,
    end: u64,
    placeholder: u64,
}

impl Cut {
    fn map(self, line: u64) -> u64 {
        if line >= self.end {
            line
        } else if line >= self.start {
            self.placeholder
        } else {
            line + (self.end - self.start)
        }
    }
}

impl Terminal {
    /// Turns the per-command budget on or off and sets the share of history one command
    /// may use (clamped to `0..=1`). Applies to output from here on.
    pub fn set_command_budget(&mut self, enabled: bool, share: f64) {
        let share = if share.is_finite() {
            share.clamp(0.0, 1.0)
        } else {
            DEFAULT_SHARE
        };
        let budget = &mut self.shell.budget;
        budget.enabled = enabled;
        budget.share = share;
        self.refresh_budget_pending();
    }

    /// Every recorded line moved by `f`, e.g. by a reflow.
    pub(crate) fn remap_budget_lines(&mut self, f: impl Fn(u64) -> u64) {
        let shell = &mut *self.shell;
        for c in &mut shell.commands {
            c.output_line = f(c.output_line);
            if let Some(gap) = &mut c.gap {
                gap.line = f(gap.line);
            }
            if let Some(marks) = &mut c.rerun {
                marks.map(&f);
            }
        }
        for line in &mut shell.budget.pending {
            *line = f(*line);
        }
    }

    /// Placeholder rows in `first..first + len`, as (index from `first`, text), in order.
    pub(crate) fn gap_rows(&self, first: u64, len: usize) -> Vec<(usize, String)> {
        self.shell
            .commands
            .iter()
            .filter_map(|c| {
                let gap = c.gap?;
                let i = gap.line.checked_sub(first)? as usize;
                (i < len).then(|| (i, placeholder_text(c, gap.dropped)))
            })
            .collect()
    }

    /// Evicts rows until scrollback fits `max_scrollback`, through the command budget
    /// like rows scrolled into history. Returns whether any row went.
    pub(crate) fn trim_scrollback(&mut self) -> bool {
        let mut trimmed = false;
        while self.primary.scrollback.len() > self.max_scrollback {
            trimmed = true;
            // trim_command reads display_offset as push_history leaves it mid-push, one row
            // past the viewport's top line.
            let viewing = self.display_offset > 0;
            self.display_offset -= viewing as usize;
            let evicted = self.budget_armed() && self.evict_from_command().is_some();
            self.display_offset += viewing as usize;
            if evicted {
                continue;
            }
            self.primary.scrollback.pop_front();
        }
        trimmed
    }

    /// Returns whether per-command budgeting is enabled and its history share.
    ///
    /// The share is a fraction of the configured scrollback row limit.
    pub fn command_budget(&self) -> (bool, f64) {
        (self.shell.budget.enabled, self.shell.budget.share)
    }

    fn budget_lines(&self) -> u64 {
        let budget = &self.shell.budget;
        let lines = (self.max_scrollback as f64 * budget.share) as u64;
        if budget.enabled && lines >= MIN_BUDGET {
            lines
        } else {
            0
        }
    }

    /// Whether eviction has to look at commands instead of plain FIFO.
    #[inline]
    pub(crate) fn budget_armed(&self) -> bool {
        self.shell.budget.enabled && !self.shell.commands.is_empty()
    }

    /// Whether skipping rows unbuilt (KAN-65) gives the same history as building them:
    /// every row the budget would keep as a command's head has to be in scrollback already.
    pub(crate) fn budget_allows_ghost(&self) -> bool {
        let budget = self.budget_lines();
        match self.shell.commands.back() {
            Some(c) if budget > 0 => c.gap.is_some() || c.output_line + budget / 4 < self.history,
            _ => true,
        }
    }

    /// Recomputes when the running command outgrows its share.
    pub(crate) fn refresh_budget_limit(&mut self) {
        let budget = self.budget_lines();
        let first = self.history - self.primary.scrollback.len() as u64;
        self.shell.budget.limit = match self.shell.commands.back() {
            Some(c) if budget > 0 && c.next_prompt.is_none() && c.output_line >= first => {
                c.output_line + budget
            }
            _ => u64::MAX,
        };
    }

    /// Keeps the running command within its share before history is full.
    #[cold]
    #[inline(never)]
    pub(crate) fn cap_command(&mut self) -> Option<Row> {
        let budget = self.budget_lines();
        let last = self.shell.commands.len().checked_sub(1);
        let spare = last.and_then(|last| self.trim_command(last, budget));
        if spare.is_none() {
            self.refresh_budget_limit();
            if self.history > self.shell.budget.limit {
                self.shell.budget.limit = u64::MAX;
            }
        }
        spare.flatten()
    }

    pub(crate) fn refresh_budget_pending(&mut self) {
        self.refresh_budget_limit();
        let budget = self.budget_lines();
        let pending = self
            .shell
            .commands
            .iter()
            .filter(|c| {
                budget > 0
                    && c.next_prompt
                        .is_some_and(|end| end.saturating_sub(c.output_line) > budget)
            })
            .map(|c| c.prompt_line)
            .collect();
        self.shell.budget.pending = pending;
    }

    /// Remembers the command at `index` if it just ended over budget.
    pub(crate) fn note_command_ended(&mut self, index: usize) {
        let budget = self.budget_lines();
        let Some(c) = self.shell.commands.get(index) else {
            return;
        };
        let over = c
            .next_prompt
            .is_some_and(|end| end.saturating_sub(c.output_line) > budget);
        if budget > 0 && over && self.shell.budget.pending.last() != Some(&c.prompt_line) {
            let line = c.prompt_line;
            self.shell.budget.pending.push(line);
        }
    }

    /// Makes room for one row by dropping it from the middle of an over-budget command.
    /// `Some(row)` returns a dropped row for reuse; `None` leaves eviction to FIFO.
    #[cold]
    #[inline(never)]
    pub(crate) fn evict_from_command(&mut self) -> Option<Option<Row>> {
        let budget = self.budget_lines();
        if budget == 0 {
            return None;
        }
        let last = self.shell.commands.len() - 1;
        if let Some(row) = self.trim_command(last, budget) {
            return Some(row);
        }
        while let Some(&line) = self.shell.budget.pending.first() {
            if let Ok(index) = self
                .shell
                .commands
                .binary_search_by_key(&line, |c| c.prompt_line)
                && let Some(row) = self.trim_command(index, budget)
            {
                return Some(row);
            }
            self.shell.budget.pending.remove(0);
        }
        None
    }

    fn trim_command(&mut self, index: usize, budget: u64) -> Option<Option<Row>> {
        if budget == 0 {
            return None;
        }
        let first = self.history - self.primary.scrollback.len() as u64;
        let history = self.history;
        let c = &self.shell.commands[index];
        let end = c.next_prompt.unwrap_or(history).min(history);
        if c.output_line < first || end <= c.output_line + budget {
            return None;
        }
        let placeholder = c.gap.map_or(c.output_line + budget / 4, |gap| gap.line);
        let start = placeholder + 1;
        // Keep at least the newest row of the command as its tail.
        let remove = (budget / 8).max(1).min((end - 1).saturating_sub(start));
        if remove == 0 {
            return None;
        }
        let dropped = c.gap.map_or(1, |gap| gap.dropped) + remove;
        let cut = Cut {
            start,
            end: start + remove,
            placeholder: placeholder + remove,
        };
        let text = placeholder_text(c, dropped);

        let scrollback = &mut self.primary.scrollback;
        let index_of = |line: u64| scrollback.len() - (history - line) as usize;
        let at = index_of(cut.start);
        if c.gap.is_none() {
            let before = index_of(placeholder - 1);
            scrollback[before].set_wrapped(false);
        }
        let row = &mut scrollback[at - 1];
        *row = placeholder_row(self.cols, &text);
        let mut drained = scrollback.drain(at..at + remove as usize);
        let spare = drained.next();
        drop(drained);

        let top = (self.display_offset > 0).then(|| history - 1 - self.display_offset as u64);
        self.remap_lines(cut);
        self.shell.commands[index].gap = Some(Gap {
            line: cut.placeholder,
            dropped,
        });
        if let Some(top) = top {
            if (cut.start..cut.end).contains(&top) {
                self.shrunk = true;
            }
            self.display_offset = (history - 1 - cut.map(top)) as usize;
        }
        self.recycled_memory_freed = true;
        self.refresh_budget_limit();
        Some(spare)
    }

    fn remap_lines(&mut self, cut: Cut) {
        let shell = &mut *self.shell;
        for c in &mut shell.commands {
            c.prompt_line = cut.map(c.prompt_line);
            c.input_line = cut.map(c.input_line);
            c.output_line = cut.map(c.output_line);
            c.next_prompt = c.next_prompt.map(|line| cut.map(line));
            if let Some(gap) = &mut c.gap {
                gap.line = cut.map(gap.line);
            }
            if let Some(marks) = &mut c.rerun {
                marks.map_inclusive(|line| cut.map(line));
            }
        }
        for line in &mut shell.budget.pending {
            *line = cut.map(*line);
        }
        shell.prompt_line = shell.prompt_line.map(|line| cut.map(line));
        shell.mark_line = shell.mark_line.map(|line| cut.map(line));
        if let Some((line, _)) = &mut shell.input_start {
            *line = cut.map(*line);
        }
        shell
            .output_ends
            .retain(|end| !(cut.start..cut.end).contains(&end.line));
        for end in &mut shell.output_ends {
            end.line = cut.map(end.line);
        }
        let placements = &mut self.primary.placements;
        if !placements.is_empty() {
            placements.retain(|p| !(cut.start..cut.end).contains(&p.line));
            for p in placements.iter_mut() {
                if p.line < cut.start {
                    let above = (cut.start - p.line) as usize;
                    if p.rows > above {
                        p.clipped_rows = Some(p.clipped_rows.unwrap_or(p.rows).min(above));
                    }
                    p.line = cut.map(p.line);
                }
            }
            self.images.generation += 1;
        }
        for point in &mut self.tracked {
            point.line = cut.map(point.line);
        }
        if let Some(selection) = &mut self.selection {
            let map = |point: Point| Point {
                line: cut.map(point.line),
                col: point.col,
            };
            selection.anchor = map(selection.anchor);
            selection.head = map(selection.head);
        }
    }
}

/// `… 48,213 lines of make output dropped`, naming the command's first word when known.
pub(crate) fn placeholder_text(c: &crate::Command, dropped: u64) -> String {
    let name: String = c
        .cells
        .get(c.input_col..)
        .unwrap_or_default()
        .iter()
        .filter(|cell| !cell.flags.contains(Flags::WIDE_SPACER))
        .map(|cell| cell.c)
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| !c.is_whitespace())
        .take(32)
        .collect();
    let lines = if dropped == 1 { "line" } else { "lines" };
    let of = if name.is_empty() {
        String::new()
    } else {
        format!(" of {name}")
    };
    format!(
        "\u{2026} {} {lines}{of} output dropped",
        group_thousands(dropped)
    )
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, d) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(d);
    }
    out
}

pub(crate) fn placeholder_row(cols: usize, text: &str) -> Row {
    let mut row = Row::new(cols, &Cell::default());
    let mut col = 0;
    for c in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            continue;
        }
        if col + width > cols {
            break;
        }
        row.cells[col].c = c;
        row.cells[col].flags = Flags::DIM;
        if width == 2 {
            row.cells[col].flags.insert(Flags::WIDE);
            row.cells[col + 1].flags = Flags::DIM | Flags::WIDE_SPACER;
        }
        col += width;
    }
    row.touch_all();
    row
}

#[cfg(test)]
mod tests {
    use super::group_thousands;

    #[test]
    fn groups_thousands() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(1_000), "1,000");
        assert_eq!(group_thousands(48_213), "48,213");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
