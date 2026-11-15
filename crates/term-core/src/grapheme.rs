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
    releases: u64,
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
        self.free.is_empty() && self.entries.len() >= self.next_release.clamp(MIN_RELEASE, CAPACITY)
    }

    /// Frees every entry not marked in `used`.
    fn retain(&mut self, used: &[bool]) {
        let free = self.free.len();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if !used[index] && !entry.is_empty() {
                self.lookup.remove(&**entry);
                *entry = Box::from("");
                self.free.push(index as u32);
            }
        }
        if self.free.len() > free {
            self.releases += 1;
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

fn regional(c: char) -> bool {
    matches!(c as u32, 0x1F1E6..=0x1F1FF)
}

fn skin_tone(c: char) -> bool {
    matches!(c as u32, 0x1F3FB..=0x1F3FF)
}

/// Emoji and the symbols emoji sequences are built from (an approximation of
/// `Extended_Pictographic`).
fn pictographic(c: char) -> bool {
    matches!(c as u32,
        0x00A9 | 0x00AE | 0x203C | 0x2049 | 0x2122 | 0x2139 | 0x2194..=0x21AA
        | 0x231A..=0x23FF | 0x24C2 | 0x25AA..=0x27BF | 0x2934 | 0x2935 | 0x2B05..=0x2B55
        | 0x3030 | 0x303D | 0x3297 | 0x3299 | 0x1F000..=0x1FAFF)
}

/// Whether printing `c` after `cluster` extends one emoji sequence: a pictograph after a
/// zero width joiner, a skin tone after a pictograph, or a flag's second regional indicator.
fn continues_sequence(cluster: &str, c: char) -> bool {
    let mut chars = cluster.chars();
    let first = chars.next();
    if regional(c) {
        return first.is_some_and(regional) && chars.next().is_none();
    }
    let last = chars.next_back().or(first);
    if skin_tone(c) {
        return last.is_some_and(pictographic);
    }
    last == Some('\u{200D}') && first.is_some_and(pictographic)
}

/// Whether `cluster` is an emoji sequence the renderer shapes as one glyph.
pub fn is_emoji_sequence(cluster: &str) -> bool {
    let mut chars = cluster.chars();
    let first = chars.next();
    match first {
        Some(r) if regional(r) => chars.next().is_some_and(regional),
        Some(p) if pictographic(p) => chars.any(|c| c == '\u{200D}' || skin_tone(c)),
        _ => false,
    }
}

/// The precomposed Hangul syllable a cluster of conjoining jamo spells (leading consonant,
/// vowel and optional trailing consonant), if it is one.
pub fn compose_hangul(cluster: &str) -> Option<char> {
    let mut chars = cluster.chars().map(|c| c as u32);
    let l = chars.next()?.checked_sub(0x1100).filter(|&l| l < 19)?;
    let v = chars.next()?.checked_sub(0x1161).filter(|&v| v < 21)?;
    let t = match chars.next() {
        None => 0,
        Some(t) => t.checked_sub(0x11A7).filter(|&t| (1..28).contains(&t))?,
    };
    if chars.next().is_some() {
        return None;
    }
    char::from_u32(0xAC00 + (l * 21 + v) * 28 + t)
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

    /// Counts the scans that freed clusters. A freed index can be handed to a different
    /// cluster, so cells compared across a change of this count may look unchanged when not.
    pub fn grapheme_releases(&self) -> u64 {
        self.graphemes.releases
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
    /// The cell a character printed now would join: the one before the cursor, or under it
    /// while a wrap is pending, the left half when that is a wide character.
    fn cluster_cell(&self) -> Option<(usize, usize)> {
        let (row, mut col) = (self.cursor.row, self.cursor.col);
        if !self.cursor.pending_wrap {
            if col == 0 {
                return None;
            }
            col -= 1;
        }
        let cells = &self.screen().lines[row].cells;
        if col > 0 && cells[col].flags.contains(Flags::WIDE_SPACER) {
            col -= 1;
        }
        let flags = cells[col].flags;
        (!flags.intersects(Flags::WIDE_SPACER | Flags::PAD | Flags::WIDE_WRAP))
            .then_some((row, col))
    }

    /// Appends `c` to the cluster in `(row, col)`; `false` if it is full.
    fn append(&mut self, row: usize, col: usize, c: char) -> bool {
        let cell = self.screen().lines[row].cells[col];
        let mut text = String::new();
        self.push_cell_text(&cell, &mut text);
        if text.chars().count() >= MAX_CHARS {
            return false;
        }
        text.push(c);
        if self.graphemes.needs_release() {
            self.release_graphemes();
        }
        let Some(index) = self.graphemes.insert(&text) else {
            return false;
        };
        let cell = &mut self.lines_mut()[row].cells[col];
        cell.c = index;
        cell.flags.insert(Flags::GRAPHEME);
        true
    }

    pub(crate) fn join_zero_width(&mut self, c: char) {
        if !joins(c) {
            return;
        }
        if let Some((row, col)) = self.cluster_cell() {
            self.append(row, col, c);
        }
    }

    /// Joins a printing character that continues the emoji sequence before the cursor (see
    /// `continues_sequence`) to it rather than giving it cells of its own. A flag spans two
    /// columns, so its second regional indicator widens the first one's cell.
    pub(crate) fn join_sequence(&mut self, c: char) -> bool {
        if !(pictographic(c) || regional(c)) {
            return false;
        }
        let Some((row, col)) = self.cluster_cell() else {
            return false;
        };
        let cell = self.screen().lines[row].cells[col];
        if !cell.flags.contains(Flags::GRAPHEME) && !regional(cell.c) && !pictographic(cell.c) {
            return false;
        }
        let mut text = String::new();
        self.push_cell_text(&cell, &mut text);
        if !continues_sequence(&text, c) || !self.append(row, col, c) {
            return false;
        }
        let cols = self.cols;
        if regional(c) && !self.cursor.pending_wrap && col + 1 < cols && self.cursor.col == col + 1
        {
            let line = &mut self.lines_mut()[row];
            crate::clear_wide_pair(&mut line.cells, col + 1);
            line.cells[col].flags.insert(Flags::WIDE);
            let mut spacer = line.cells[col];
            spacer.c = ' ';
            spacer.flags.remove(Flags::WIDE | Flags::GRAPHEME);
            spacer.flags.insert(Flags::WIDE_SPACER);
            line.cells[col + 1] = spacer;
            line.touch(col + 2);
            if col + 2 >= cols {
                self.cursor.col = cols - 1;
                self.cursor.pending_wrap = true;
            } else {
                self.cursor.col = col + 2;
            }
        }
        true
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
        assert_eq!(t.cursor_position(), Some((0, 4)));
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[0]), Some("e\u{301}"));
        assert_eq!(t.base_char(&row.cells[0]), 'e');
        assert_eq!(
            t.grapheme(&row.cells[1]),
            Some("\u{1f468}\u{200d}\u{1f469}")
        );
        assert!(row.cells[1].flags.contains(Flags::WIDE));
        assert_eq!(t.base_char(&row.cells[3]), 'x');
    }

    #[test]
    fn emoji_sequences_share_one_wide_cell() {
        let mut t = Terminal::new(20, 2);
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        let rainbow = "\u{1f3f3}\u{fe0f}\u{200d}\u{1f308}";
        let thumb = "\u{1f44d}\u{1f3fd}";
        let coder = "\u{1f469}\u{1f3fd}\u{200d}\u{1f4bb}";
        t.advance(format!("{family}|{rainbow}|{thumb}|{coder}|").as_bytes());
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[0]), Some(family));
        assert!(row.cells[0].flags.contains(Flags::WIDE));
        assert_eq!(t.base_char(&row.cells[2]), '|');
        // U+1F3F3 is narrow; the sequence keeps its first character's width.
        assert_eq!(t.grapheme(&row.cells[3]), Some(rainbow));
        assert_eq!(t.base_char(&row.cells[4]), '|');
        assert_eq!(t.grapheme(&row.cells[5]), Some(thumb));
        assert_eq!(t.base_char(&row.cells[7]), '|');
        assert_eq!(t.grapheme(&row.cells[8]), Some(coder));
        assert_eq!(t.base_char(&row.cells[10]), '|');
        assert_eq!(t.cursor_position(), Some((0, 11)));
        assert_eq!(text(&t, 0), format!("{family}|{rainbow}|{thumb}|{coder}|"));
        assert!(is_emoji_sequence(family) && is_emoji_sequence(rainbow));
        assert!(is_emoji_sequence(thumb) && is_emoji_sequence(coder));
        assert!(!is_emoji_sequence("e\u{301}") && !is_emoji_sequence("\u{1f468}\u{fe0f}"));
    }

    #[test]
    fn regional_indicator_pairs_become_wide_flags() {
        let mut t = Terminal::new(6, 2);
        t.advance("\u{1f1ef}\u{1f1f5}\u{1f1fa}\u{1f1f8}\u{1f1e9}x".as_bytes());
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[0]), Some("\u{1f1ef}\u{1f1f5}"));
        assert!(row.cells[0].flags.contains(Flags::WIDE));
        assert!(row.cells[1].flags.contains(Flags::WIDE_SPACER));
        assert_eq!(t.grapheme(&row.cells[2]), Some("\u{1f1fa}\u{1f1f8}"));
        // A third indicator starts a new flag rather than joining the second.
        assert_eq!(t.base_char(&row.cells[4]), '\u{1f1e9}');
        assert!(!row.cells[4].flags.contains(Flags::WIDE));
        assert_eq!(t.base_char(&row.cells[5]), 'x');
        assert_eq!(
            text(&t, 0),
            "\u{1f1ef}\u{1f1f5}\u{1f1fa}\u{1f1f8}\u{1f1e9}x"
        );
    }

    #[test]
    fn a_flag_split_by_a_pending_wrap_keeps_one_cell() {
        let mut t = Terminal::new(3, 2);
        t.advance("ab\u{1f1ef}\u{1f1f5}c".as_bytes());
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[2]), Some("\u{1f1ef}\u{1f1f5}"));
        assert!(!row.cells[2].flags.contains(Flags::WIDE));
        assert_eq!(t.base_char(&t.viewport_row(1).cells[0]), 'c');
    }

    #[test]
    fn a_pictograph_without_a_joiner_takes_its_own_cells() {
        let mut t = Terminal::new(10, 2);
        t.advance("\u{1f468}\u{1f469}a\u{1f3fd}".as_bytes());
        let row = t.viewport_row(0);
        assert_eq!(t.grapheme(&row.cells[0]), None);
        assert_eq!(t.base_char(&row.cells[2]), '\u{1f469}');
        assert_eq!(t.base_char(&row.cells[5]), '\u{1f3fd}');
        assert_eq!(t.cursor_position(), Some((0, 7)));
    }

    #[test]
    fn conjoining_jamo_compose_to_a_syllable() {
        let mut t = Terminal::new(10, 2);
        t.advance("\u{1100}\u{1161}\u{11a8}\u{1112}\u{1161}x".as_bytes());
        let row = t.viewport_row(0);
        let first = t.grapheme(&row.cells[0]).unwrap();
        assert_eq!(compose_hangul(first), Some('\u{ac01}'));
        assert_eq!(
            compose_hangul(t.grapheme(&row.cells[2]).unwrap()),
            Some('\u{d558}')
        );
        assert_eq!(t.base_char(&row.cells[4]), 'x');
        assert_eq!(compose_hangul("\u{1100}"), None);
        assert_eq!(compose_hangul("\u{1100}\u{1161}\u{11a8}\u{11a8}"), None);
        assert_eq!(compose_hangul("e\u{301}"), None);
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
    fn copying_widened_wrapped_lines_keeps_clusters() {
        let mut t = Terminal::new(5, 3);
        t.advance("he\u{301}llo wo\u{308}rld".as_bytes());
        t.resize(8, 3);
        // Reflow rejoins the three rows into two at the new width.
        t.start_selection(Point { line: 0, col: 0 }, SelectionKind::Simple);
        t.update_selection(Point { line: 1, col: 7 });
        assert_eq!(t.selection_text().unwrap(), "he\u{301}llo wo\u{308}rld");
        let mut t = Terminal::new(5, 2);
        t.advance("abcd\u{754c}\u{301}x".as_bytes());
        t.start_selection(Point { line: 0, col: 0 }, SelectionKind::Simple);
        t.update_selection(Point { line: 1, col: 2 });
        assert_eq!(t.selection_text().unwrap(), "abcd\u{754c}\u{301}x");
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
    fn full_table_is_released_however_big_the_grid() {
        let mut t = Terminal::new(4, 2);
        t.graphemes.next_release = usize::MAX;
        t.graphemes.entries = vec![Box::from("x\u{301}"); CAPACITY];
        t.advance("e\u{301}".as_bytes());
        assert_eq!(t.grapheme(&t.viewport_row(0).cells[0]), Some("e\u{301}"));
    }

    #[test]
    fn overlay_skips_format_characters() {
        let marks: String = overlay_marks("e\u{301}\u{200d}\u{fe0f}\u{323}").collect();
        assert_eq!(marks, "\u{301}\u{323}");
    }
}
