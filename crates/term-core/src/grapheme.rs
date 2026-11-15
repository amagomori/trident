//! Grapheme clusters: a character plus the zero-width marks printed after it.
//!
//! A cell holds one `char`, so a cell whose character carries combining marks, joiners or
//! variation selectors stores the whole cluster in the terminal's table and keeps its index in
//! `Cell::c`, flagged `Flags::GRAPHEME`. Width and cursor movement come from the base
//! character alone, as in xterm and Alacritty, so the grid stays in step with programs that
//! measure text with `wcwidth`.

use std::collections::HashMap;

use crate::{Cell, Flags, Terminal};

/// The first index char; indices run through the supplementary private use planes, which a
/// cell's character can never be since those have width one and are stored directly.
const BASE: u32 = 0xF0000;

/// Distinct clusters the table can index.
const CAPACITY: usize = 0x110000 - BASE as usize;

/// Characters kept per cluster; further marks are dropped, as other terminals cap them.
pub const MAX_CHARS: usize = 32;

/// Entries the table may hold before its first release scan.
const MIN_RELEASE: usize = 4096;

/// Grid cells a release scan may visit per entry it allows, bounding the amortized scan cost.
const CELLS_PER_ENTRY: usize = 8;

#[derive(Default)]
pub(crate) struct Graphemes {
    entries: Vec<Box<str>>,
    lookup: HashMap<Box<str>, u32>,
    free: Vec<u32>,
    next_release: usize,
}

impl Graphemes {
    fn get(&self, c: char) -> Option<&str> {
        let index = (c as u32).checked_sub(BASE)? as usize;
        self.entries
            .get(index)
            .map(|s| &**s)
            .filter(|s| !s.is_empty())
    }

    fn insert(&mut self, text: &str) -> Option<char> {
        if let Some(&index) = self.lookup.get(text) {
            return char::from_u32(BASE + index);
        }
        let index = match self.free.pop() {
            Some(index) => {
                self.entries[index as usize] = text.into();
                index
            }
            None if self.entries.len() < CAPACITY => {
                self.entries.push(text.into());
                self.entries.len() as u32 - 1
            }
            None => return None,
        };
        self.lookup.insert(text.into(), index);
        char::from_u32(BASE + index)
    }

    fn live(&self) -> usize {
        self.entries.len() - self.free.len()
    }

    fn needs_release(&self) -> bool {
        self.free.is_empty() && self.entries.len() >= self.next_release.max(MIN_RELEASE)
    }

    /// Frees every entry not marked in `used`.
    fn retain(&mut self, used: &[bool]) {
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if !used[index] && !entry.is_empty() {
                self.lookup.remove(&**entry);
                *entry = Box::from("");
                self.free.push(index as u32);
            }
        }
    }
}

fn mark(cells: &[Cell], used: &mut [bool]) {
    for cell in cells {
        if cell.flags.contains(Flags::GRAPHEME)
            && let Some(slot) = (cell.c as u32)
                .checked_sub(BASE)
                .and_then(|index| used.get_mut(index as usize))
        {
            *slot = true;
        }
    }
}

/// The marks of a cluster drawn over its base character: joiners, variation selectors and
/// other format characters change how a font shapes the cluster but draw nothing themselves.
pub fn overlay_marks(cluster: &str) -> impl Iterator<Item = char> + '_ {
    let mut chars = cluster.chars();
    chars.next();
    chars.filter(|&c| {
        !matches!(c as u32,
            0x034F | 0x180B..=0x180F | 0x200B..=0x200F | 0x2060..=0x206F | 0xFE00..=0xFE0F
            | 0xFEFF | 0xE0000..=0xE0FFF)
    })
}

/// Whether a zero-width character joins the cell before it.
fn joins(c: char) -> bool {
    !c.is_control()
}

impl Terminal {
    /// The grapheme cluster a cell holds, when its character carries zero-width marks.
    pub fn grapheme(&self, cell: &Cell) -> Option<&str> {
        if cell.flags.contains(Flags::GRAPHEME) {
            self.graphemes.get(cell.c)
        } else {
            None
        }
    }

    /// The character a cell shows, without the marks of its cluster.
    pub fn base_char(&self, cell: &Cell) -> char {
        self.grapheme(cell)
            .and_then(|s| s.chars().next())
            .unwrap_or(cell.c)
    }

    /// Appends the text a cell holds: its character and any marks printed after it.
    pub fn push_cell_text(&self, cell: &Cell, out: &mut String) {
        match self.grapheme(cell) {
            Some(text) => out.push_str(text),
            None => out.push(cell.c),
        }
    }

    /// Bytes `push_cell_text` appends for a cell.
    pub fn cell_text_len(&self, cell: &Cell) -> usize {
        self.grapheme(cell).map_or(cell.c.len_utf8(), str::len)
    }

    /// Adds a zero-width character to the cluster of the cell the cursor last printed into.
    #[cold]
    #[inline(never)]
    pub(crate) fn join_zero_width(&mut self, c: char) {
        if !joins(c) {
            return;
        }
        let (row, mut col) = (self.cursor.row, self.cursor.col);
        if !self.cursor.pending_wrap {
            if col == 0 {
                return;
            }
            col -= 1;
        }
        let cells = &self.screen().lines[row].cells;
        if col > 0 && cells[col].flags.contains(Flags::WIDE_SPACER) {
            col -= 1;
        }
        let cell = cells[col];
        if cell
            .flags
            .intersects(Flags::WIDE_SPACER | Flags::PAD | Flags::WIDE_WRAP)
        {
            return;
        }
        let mut text = String::new();
        self.push_cell_text(&cell, &mut text);
        if text.chars().count() >= MAX_CHARS {
            return;
        }
        text.push(c);
        if self.graphemes.needs_release() {
            self.release_graphemes();
        }
        let Some(index) = self.graphemes.insert(&text) else {
            return;
        };
        let cell = &mut self.lines_mut()[row].cells[col];
        cell.c = index;
        cell.flags.insert(Flags::GRAPHEME);
    }

    /// Frees the clusters no cell references any more.
    #[cold]
    #[inline(never)]
    fn release_graphemes(&mut self) {
        let mut used = vec![false; self.graphemes.entries.len()];
        let mut cells = 0;
        for screen in [&self.primary, &self.alternate] {
            for row in screen.lines.iter().chain(&screen.scrollback) {
                cells += row.cells.len();
                mark(&row.cells, &mut used);
                row.hidden_columns().for_each(|(_, cell)| {
                    mark(std::slice::from_ref(cell), &mut used);
                });
            }
        }
        for command in &self.shell.commands {
            mark(&command.cells, &mut used);
        }
        for end in &self.shell.output_ends {
            mark(&end.cells, &mut used);
        }
        self.graphemes.retain(&used);
        self.graphemes.next_release = (4 * self.graphemes.live()).max(cells / CELLS_PER_ENTRY);
    }

    #[cfg(test)]
    pub(crate) fn grapheme_entries(&self) -> usize {
        self.graphemes.live()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Point, SelectionKind};

    fn text(t: &Terminal, row: usize) -> String {
        let mut out = String::new();
        for cell in &t.viewport_row(row).cells {
            if !cell.flags.contains(Flags::WIDE_SPACER) {
                t.push_cell_text(cell, &mut out);
            }
        }
        out.truncate(out.trim_end().len());
        out
    }

    #[test]
    fn clusters_keep_their_marks_and_width() {
        let mut t = Terminal::new(10, 2);
        t.advance("e\u{301}\u{1f468}\u{200d}\u{1f469}x".as_bytes());
        assert_eq!(text(&t, 0), "e\u{301}\u{1f468}\u{200d}\u{1f469}x");
        assert_eq!(t.cursor_position(), Some((0, 6)));
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[0]), Some("e\u{301}"));
        assert_eq!(t.base_char(&row.cells[0]), 'e');
        assert_eq!(t.grapheme(&row.cells[1]), Some("\u{1f468}\u{200d}"));
        assert!(row.cells[1].flags.contains(Flags::WIDE));
        assert_eq!(t.grapheme(&row.cells[3]), None);
    }

    #[test]
    fn marks_are_capped() {
        let mut t = Terminal::new(4, 2);
        t.advance(format!("a{}b", "\u{301}".repeat(100)).as_bytes());
        let cluster = t.grapheme(&t.viewport_row(0).cells[0]).unwrap();
        assert_eq!(cluster.chars().count(), MAX_CHARS);
        assert_eq!(t.cursor_position(), Some((0, 2)));
    }

    #[test]
    fn selection_and_search_see_whole_clusters() {
        let mut t = Terminal::new(10, 2);
        t.advance("ae\u{301}b \u{1f44d}\u{1f3fd}".as_bytes());
        t.start_selection(Point { line: 0, col: 0 }, SelectionKind::Simple);
        t.update_selection(Point { line: 0, col: 7 });
        assert_eq!(t.selection_text().unwrap(), "ae\u{301}b \u{1f44d}\u{1f3fd}");
        t.start_selection(Point { line: 0, col: 1 }, SelectionKind::Word);
        assert_eq!(t.selection_text().unwrap(), "ae\u{301}b");
        let found = t.search("e\u{301}b", false).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, Point { line: 0, col: 1 });
        assert_eq!(found[0].end, Point { line: 0, col: 2 });
    }

    #[test]
    fn overwritten_clusters_are_released() {
        let mut t = Terminal::new(4, 2);
        t.set_scrollback_limit(0);
        for i in 0..40_000u32 {
            let base = char::from_u32(0x4e00 + i % 0x5000).unwrap();
            t.advance(
                format!(
                    "\r{base}\u{301}{}",
                    char::from_u32(0x300 + i / 0x5000).unwrap()
                )
                .as_bytes(),
            );
        }
        assert!(
            t.grapheme_entries() <= MIN_RELEASE,
            "{}",
            t.grapheme_entries()
        );
        let cell = t.viewport_row(0).cells[0];
        let cluster = t.grapheme(&cell).unwrap();
        assert!(cluster.starts_with(char::from_u32(0x4e00 + 39_999 % 0x5000).unwrap()));
    }

    #[test]
    fn overlay_skips_format_characters() {
        let marks: String = overlay_marks("e\u{301}\u{200d}\u{fe0f}\u{323}").collect();
        assert_eq!(marks, "\u{301}\u{323}");
    }
}
