//! Cell attributes and physical rows, including cells hidden by narrowing.

use std::sync::atomic::{AtomicBool, Ordering};

/// A terminal color as specified by the program; see [`crate::colors`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    DefaultFg,
    DefaultBg,
    /// An xterm 256-color palette index.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(u16);

impl Flags {
    pub const BOLD: Flags = Flags(1);
    pub const DIM: Flags = Flags(1 << 1);
    pub const ITALIC: Flags = Flags(1 << 2);
    /// Underlined text; use [`Self::underline`] to read its style.
    pub const UNDERLINE: Flags = Flags(1 << 3);
    /// Foreground and background are exchanged (SGR 7).
    pub const INVERSE: Flags = Flags(1 << 4);
    /// Text is concealed without changing its background (SGR 8).
    pub const HIDDEN: Flags = Flags(1 << 5);
    pub const STRIKE: Flags = Flags(1 << 6);
    /// First half of a double-width character.
    pub const WIDE: Flags = Flags(1 << 7);
    /// Second half of a double-width character; holds no glyph.
    pub const WIDE_SPACER: Flags = Flags(1 << 8);
    /// Blank standing in for a wide character cut off at the right edge; any write replaces it.
    pub const PAD: Flags = Flags(1 << 9);
    /// Unprinted padding before a soft wrap, from a wide character or widening the row.
    pub const WIDE_WRAP: Flags = Flags(1 << 10);
    /// SGR 5/6 blink; the renderer toggles the cell's text on a timer.
    pub const BLINK: Flags = Flags(1 << 11);
    /// `c` indexes a grapheme cluster; read it with [`crate::Terminal::grapheme`].
    pub const GRAPHEME: Flags = Flags(1 << 12);
    /// The `UnderlineStyle` of an `UNDERLINE` cell other than a single line (SGR 4:x, 21).
    pub const UNDERLINE_STYLE: Flags = Flags(0b111 << 13);

    pub const fn union(self, other: Flags) -> Flags {
        Flags(self.0 | other.0)
    }

    pub(crate) const fn bits(self) -> u16 {
        self.0
    }

    /// Whether any flag of `other` is set.
    pub fn intersects(self, other: Flags) -> bool {
        self.0 & other.0 != 0
    }

    pub fn contains(self, other: Flags) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Flags) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Flags) {
        self.0 &= !other.0;
    }
}

/// How SGR 4:x draws an underline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnderlineStyle {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Flags {
    /// The underline drawn under the cell, if any.
    pub fn underline(self) -> Option<UnderlineStyle> {
        if !self.contains(Flags::UNDERLINE) {
            return None;
        }
        Some(match (self.0 & Flags::UNDERLINE_STYLE.0) >> 13 {
            1 => UnderlineStyle::Double,
            2 => UnderlineStyle::Curly,
            3 => UnderlineStyle::Dotted,
            4 => UnderlineStyle::Dashed,
            _ => UnderlineStyle::Single,
        })
    }

    /// Replaces the underline style, or removes underlining for `None`.
    pub fn set_underline(&mut self, style: Option<UnderlineStyle>) {
        self.remove(Flags::UNDERLINE | Flags::UNDERLINE_STYLE);
        let Some(style) = style else { return };
        let bits = match style {
            UnderlineStyle::Single => 0,
            UnderlineStyle::Double => 1,
            UnderlineStyle::Curly => 2,
            UnderlineStyle::Dotted => 3,
            UnderlineStyle::Dashed => 4,
        };
        self.0 |= Flags::UNDERLINE.0 | bits << 13;
    }
}

impl std::ops::BitOr for Flags {
    type Output = Flags;
    fn bitor(self, rhs: Flags) -> Flags {
        Flags(self.0 | rhs.0)
    }
}

/// One screen cell, including its character, rendition and link-table index.
///
/// Read cells from [`crate::Terminal::viewport_row`], as in the [crate example](crate).
/// Use [`crate::Terminal::push_cell_text`] to decode grapheme-table entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// Character, or a table index when [`Flags::GRAPHEME`] is set.
    pub c: char,
    /// Text color before inverse and selection effects.
    pub fg: Color,
    /// Background color before inverse and selection effects.
    pub bg: Color,
    /// Rendition attributes and wide/grapheme storage markers.
    pub flags: Flags,
    /// Terminal-owned link, underline color, protection and directory origin; zero means none.
    pub hyperlink: u16,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            hyperlink: 0,
            c: ' ',
            fg: Color::DefaultFg,
            bg: Color::DefaultBg,
            flags: Flags::default(),
        }
    }
}

impl Cell {
    const SGR_FLAGS: Flags = Flags::BOLD
        .union(Flags::DIM)
        .union(Flags::ITALIC)
        .union(Flags::UNDERLINE)
        .union(Flags::BLINK)
        .union(Flags::INVERSE)
        .union(Flags::HIDDEN)
        .union(Flags::STRIKE);

    /// Whether both cells render with the same SGR attributes and colors.
    pub(crate) fn same_sgr(&self, other: &Cell) -> bool {
        self.fg == other.fg
            && self.bg == other.bg
            && self.flags.bits() & Self::SGR_FLAGS.bits()
                == other.flags.bits() & Self::SGR_FLAGS.bits()
    }

    /// Appends one SGR sequence that resets and then sets this cell's attributes and colors.
    pub(crate) fn push_sgr(&self, out: &mut String) {
        use std::fmt::Write;
        out.push_str("\x1b[0");
        for (flag, code) in [
            (Flags::BOLD, ";1"),
            (Flags::DIM, ";2"),
            (Flags::ITALIC, ";3"),
            (Flags::UNDERLINE, ";4"),
            (Flags::BLINK, ";5"),
            (Flags::INVERSE, ";7"),
            (Flags::HIDDEN, ";8"),
            (Flags::STRIKE, ";9"),
        ] {
            if self.flags.contains(flag) {
                out.push_str(code);
            }
        }
        for (color, base) in [(self.fg, 30), (self.bg, 40)] {
            let _ = match color {
                Color::Indexed(n) if n < 8 => write!(out, ";{}", base + u32::from(n)),
                Color::Indexed(n) if n < 16 => write!(out, ";{}", base + 52 + u32::from(n)),
                Color::Indexed(n) => write!(out, ";{};5;{n}", base + 8),
                Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
                Color::DefaultFg | Color::DefaultBg => Ok(()),
            };
        }
        out.push('m');
    }

    /// A blank cell that keeps the background of `template`, as erase operations require.
    pub fn blank(template: &Cell) -> Cell {
        Cell {
            bg: template.bg,
            ..Cell::default()
        }
    }
}

/// A physical row and any cells temporarily hidden by a narrower grid.
#[derive(Clone, Debug)]
pub struct Row {
    /// Visible cells, indexed by zero-based cell column.
    pub cells: Vec<Cell>,
    /// Shell integration and soft-wrap metadata.
    pub flags: RowFlags,
    pub(crate) prompt_col: u32,
    pub(crate) output_col: u16,
    /// Every cell from this column on equals the last one, so `reset` can skip them;
    /// `u8::MAX` means not known. Writers to `cells` must call `touch` or `touch_all`;
    /// `put_char` relies on `Terminal::sync_cursor_row`.
    occupied: u8,
    /// Rare row state is boxed to keep full-width scrolling compact.
    extra: Option<Box<RowExtra>>,
}

#[derive(Debug, Default)]
struct RowExtra {
    /// Inclusive margins where text wrapped without joining the entire rows.
    split_wraps: Vec<(usize, usize)>,
    /// Cells cut off when narrowing, restored on widening.
    cells: Vec<Cell>,
    /// The last cell of the row is a `PAD` blank standing in for a wide character that no
    /// longer fits; the character itself is the start of `cells`.
    padded: bool,
    hidden_may_have_placeholder: bool,
    // Cleared by shared layout only after decoding the entire visible row.
    possible_placeholder: AtomicBool,
}

impl Clone for RowExtra {
    fn clone(&self) -> Self {
        Self {
            split_wraps: self.split_wraps.clone(),
            cells: self.cells.clone(),
            padded: self.padded,
            hidden_may_have_placeholder: self.hidden_may_have_placeholder,
            possible_placeholder: self.possible_placeholder.load(Ordering::Relaxed).into(),
        }
    }
}

/// Bounds and scan hints do not change a row's contents.
impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
            && self.flags == other.flags
            && self.prompt_col == other.prompt_col
            && self.output_col == other.output_col
            && self.extra_contents() == other.extra_contents()
    }
}

impl Eq for Row {}

impl Row {
    fn extra_contents(&self) -> (&[(usize, usize)], &[Cell], bool) {
        self.extra
            .as_ref()
            .map_or((&[], &[], false), |e| (&e.split_wraps, &e.cells, e.padded))
    }

    fn prune_extra(&mut self) {
        if let Some(e) = &mut self.extra
            && e.split_wraps.is_empty()
            && e.cells.is_empty()
            && !*e.possible_placeholder.get_mut()
        {
            self.extra = None;
        }
    }

    #[cold]
    pub(crate) fn note_placeholder(&mut self) {
        let extra = self.extra.get_or_insert_with(Default::default);
        *extra.possible_placeholder.get_mut() = true;
    }

    pub(crate) fn may_have_placeholder(&self) -> bool {
        self.extra
            .as_ref()
            .is_some_and(|e| e.possible_placeholder.load(Ordering::Relaxed))
    }

    pub(crate) fn no_visible_placeholder(&self) {
        if let Some(extra) = &self.extra {
            extra.possible_placeholder.store(false, Ordering::Relaxed);
        }
    }

    /// Blank the row in place, keeping its allocation unless it is oversized.
    pub(crate) fn reset(&mut self, cols: usize, template: &Cell, released: &mut bool) {
        let blank = Cell::blank(template);
        if self.cells.len() == cols
            && self.cells.capacity() <= cols.saturating_mul(2)
            && self.cells.last() == Some(&blank)
        {
            let occupied = self.occupied();
            self.cells[..occupied].fill(blank);
        } else {
            self.cells.clear();
            if self.cells.capacity() > cols.saturating_mul(2) {
                self.cells.shrink_to(cols);
                *released = true;
            }
            self.cells.resize(cols, blank);
        }
        self.occupied = 0;
        self.flags = RowFlags::default();
        self.prompt_col = 0;
        self.output_col = 0;
        self.extra = None;
    }

    pub(crate) fn erase(&mut self, blank: Row) {
        self.cells = blank.cells;
        self.occupied = blank.occupied;
        self.set_wrapped(false);
        self.extra = blank.extra;
    }

    /// The line continues on the next row because it soft-wrapped.
    pub fn wrapped(&self) -> bool {
        self.flags.contains(RowFlags::WRAPPED)
    }

    /// Sets whether the logical line continues onto the next physical row.
    pub fn set_wrapped(&mut self, wrapped: bool) {
        if let Some(extra) = &mut self.extra {
            extra.split_wraps.clear();
            self.prune_extra();
        }
        if wrapped {
            self.flags.insert(RowFlags::WRAPPED);
        } else {
            self.flags.remove(RowFlags::WRAPPED);
        }
    }

    pub(crate) fn split_wrapped(&self, left: usize, right: usize) -> bool {
        self.extra
            .as_ref()
            .is_some_and(|e| e.split_wraps.contains(&(left, right)))
    }

    pub(crate) fn set_split_wrapped(&mut self, left: usize, right: usize, wrapped: bool) {
        self.flags.remove(RowFlags::WRAPPED);
        if let Some(extra) = &mut self.extra {
            extra.split_wraps.retain(|&(l, r)| r < left || l > right);
        }
        if wrapped {
            self.extra
                .get_or_insert_with(Default::default)
                .split_wraps
                .push((left, right));
        } else {
            self.prune_extra();
        }
    }

    /// The number of leading cells that may differ from the last cell.
    pub(crate) fn occupied(&self) -> usize {
        if self.occupied == u8::MAX {
            self.cells.len()
        } else {
            usize::from(self.occupied).min(self.cells.len())
        }
    }

    /// Records a write to the cells before column `end`.
    #[inline]
    pub(crate) fn touch(&mut self, end: usize) {
        self.occupied = self.occupied.max(end.min(usize::from(u8::MAX)) as u8);
    }

    /// Records a write that may have changed any cell.
    pub(crate) fn touch_all(&mut self) {
        self.occupied = u8::MAX;
    }

    /// Records that every cell from column `from` on now holds the same blank.
    pub(crate) fn set_blank_from(&mut self, from: usize) {
        self.occupied = u8::try_from(from).unwrap_or(u8::MAX);
    }

    /// Creates `cols` blank cells using only the background of `template`.
    pub fn new(cols: usize, template: &Cell) -> Row {
        Row {
            cells: vec![Cell::blank(template); cols],
            flags: RowFlags::default(),
            prompt_col: 0,
            output_col: 0,
            occupied: 0,
            extra: None,
        }
    }

    /// A row holding `cells`, as reflow builds them.
    pub(crate) fn from_cells(cells: Vec<Cell>, flags: RowFlags, possible: bool) -> Row {
        let mut row = Row {
            cells,
            flags,
            prompt_col: 0,
            output_col: 0,
            occupied: u8::MAX,
            extra: None,
        };
        if possible {
            row.note_placeholder();
        }
        row
    }

    /// Takes every cell of the row, with the cells hidden by narrowing back in place.
    pub(crate) fn take_cells(&mut self) -> Vec<Cell> {
        self.touch_all();
        let mut cells = std::mem::take(&mut self.cells);
        if let Some(extra) = &mut self.extra {
            extra.split_wraps.clear();
            // A negative scan while narrow says nothing about the restored suffix.
            *extra.possible_placeholder.get_mut() |=
                std::mem::take(&mut extra.hidden_may_have_placeholder);
            let mut hidden = std::mem::take(&mut extra.cells);
            if std::mem::take(&mut extra.padded) {
                let pad = cells.pop().unwrap_or_default();
                if !pad.flags.contains(Flags::PAD) {
                    // The stand-in was overwritten, so the wide character it covered is gone.
                    cells.push(pad);
                    hidden[..2].fill(Cell::default());
                    hidden.remove(0);
                }
            }
            cells.append(&mut hidden);
            self.prune_extra();
        }
        cells
    }

    /// Expands implicit trailing blanks when a history row becomes a screen row.
    pub(crate) fn pad_to(&mut self, cols: usize) {
        if self.cells.len() < cols {
            debug_assert!(!self.wrapped() && !self.has_hidden());
            self.touch_all();
            self.cells.resize(cols, Cell::default());
        }
    }

    /// Resizes the row, retaining cut-off text for a later widening.
    ///
    /// Unlike terminal resizing this does not reflow text onto other rows.
    ///
    /// # Panics
    ///
    /// Panics if a nonempty row is narrowed to zero columns.
    pub fn resize(&mut self, cols: usize) {
        self.resize_trimming(cols, |c| *c == Cell::default());
    }

    pub(crate) fn resize_trimming(&mut self, cols: usize, blank: impl Fn(&Cell) -> bool) {
        let mut cells = self.take_cells();
        if cells.len() > cols {
            let wide_at_edge = cells[cols - 1].flags.contains(Flags::WIDE);
            let cut = if wide_at_edge { cols - 1 } else { cols };
            let mut hidden = cells.split_off(cut);
            if wide_at_edge {
                cells.push(Cell {
                    flags: Flags::PAD,
                    ..Cell::default()
                });
            }
            if !self.wrapped() {
                let used = hidden.iter().rposition(|c| !blank(c)).map_or(0, |i| i + 1);
                hidden.truncate(used);
            }
            if wide_at_edge || !hidden.is_empty() {
                let extra = self.extra.get_or_insert_with(Default::default);
                extra.hidden_may_have_placeholder = *extra.possible_placeholder.get_mut();
                extra.cells = hidden;
                extra.padded = wide_at_edge;
            }
        } else {
            let mut blank = Cell::default();
            if self.wrapped() {
                blank.flags.insert(Flags::WIDE_WRAP);
            }
            cells.resize(cols, blank);
        }
        self.cells = cells;
    }

    /// Forgets the cells past the right edge, for when the line is erased to its end.
    pub fn clear_hidden(&mut self) {
        if let Some(extra) = &mut self.extra {
            extra.cells = Vec::new();
            extra.padded = false;
            extra.hidden_may_have_placeholder = false;
            self.prune_extra();
        }
    }

    /// Cells past the right edge, in order after the visible ones, as `resize` would bring
    /// them back.
    pub(crate) fn hidden_cells(&self) -> impl Iterator<Item = &Cell> {
        let (_, lead, cells) = self.hidden_parts().unwrap_or((0, None, &[]));
        lead.into_iter().chain(cells)
    }

    /// Cells past the right edge with their columns, as `resize` would bring them back.
    pub(crate) fn hidden_columns(&self) -> impl Iterator<Item = (usize, &Cell)> {
        let (start, lead, cells) = self.hidden_parts().unwrap_or((0, None, &[]));
        (start..).zip(lead.into_iter().chain(cells))
    }

    pub(crate) fn has_hidden(&self) -> bool {
        self.extra.as_ref().is_some_and(|e| !e.cells.is_empty())
    }

    /// The column of the first hidden cell, a blank standing in for an overwritten wide
    /// character, and the rest of the hidden cells.
    fn hidden_parts(&self) -> Option<(usize, Option<&Cell>, &[Cell])> {
        static BLANK: Cell = Cell {
            c: ' ',
            fg: Color::DefaultFg,
            bg: Color::DefaultBg,
            flags: Flags(0),
            hyperlink: 0,
        };
        let h = self.extra.as_deref()?;
        let pad_intact = self
            .cells
            .last()
            .is_some_and(|c| c.flags.contains(Flags::PAD));
        Some(if h.padded && pad_intact {
            (self.cells.len() - 1, None, &h.cells[..])
        } else if h.padded {
            // The PAD stand-in was overwritten, so the wide character it covered is gone.
            (
                self.cells.len(),
                Some(&BLANK),
                h.cells.get(2..).unwrap_or_default(),
            )
        } else {
            (self.cells.len(), None, &h.cells[..])
        })
    }

    /// Makes soft-wrap padding from column `from` onward ordinary spaces.
    ///
    /// # Panics
    ///
    /// Panics if the row is wrapped and `from` exceeds its visible cell count.
    pub fn materialize_wrap_padding(&mut self, from: usize) {
        if self.wrapped() {
            for cell in &mut self.cells[from..] {
                cell.flags.remove(Flags::WIDE_WRAP);
            }
        }
    }

    /// Marks the links the row references and returns how many were not marked yet.
    pub(crate) fn mark_hyperlinks(&self, used: &mut [bool]) -> usize {
        let hidden = self.extra.as_deref().map_or(&[][..], |h| &h.cells[..]);
        let mut marked = 0;
        for cells in [&self.cells[..], hidden] {
            // Neighbouring cells mostly share a link, so only a change of index is looked up.
            let mut last = 0;
            for cell in cells {
                let index = cell.hyperlink;
                if index != last {
                    last = index;
                    if index != 0
                        && let Some(mark) = used.get_mut(index as usize)
                        && !*mark
                    {
                        *mark = true;
                        marked += 1;
                    }
                }
            }
        }
        marked
    }

    pub(crate) fn hyperlinks_mut(&mut self) -> impl Iterator<Item = &mut u16> {
        self.cells
            .iter_mut()
            .chain(self.extra.iter_mut().flat_map(|h| h.cells.iter_mut()))
            .map(|c| &mut c.hyperlink)
    }

    pub(crate) fn is_blank(&self, links: &crate::links::LinkTable) -> bool {
        self.cells.iter().all(|c| links.is_blank(c))
            && self
                .extra
                .as_ref()
                .is_none_or(|h| h.cells.iter().all(|c| links.is_blank(c)))
    }

    /// Returns visible test text without wide-character spacer cells.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let mut s: String = self
            .cells
            .iter()
            .filter(|c| !c.flags.contains(Flags::WIDE_SPACER))
            .map(|c| c.c)
            .collect();
        s.truncate(s.trim_end().len());
        s
    }
}

/// Row-level shell integration metadata; see [`Row::flags`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowFlags(u8);

impl RowFlags {
    /// The row contains part of a shell prompt.
    pub const PROMPT: Self = Self(1);
    /// The row contains an OSC 133 prompt-start mark.
    pub const PROMPT_START: Self = Self(2);
    /// The row contains an OSC 133 command-output-start mark.
    pub const OUTPUT_START: Self = Self(4);
    /// The command associated with the prompt exited unsuccessfully.
    pub const FAILED: Self = Self(8);
    /// The command associated with the prompt exited successfully.
    pub const SUCCEEDED: Self = Self(16);
    pub(crate) const OUTPUT_BEFORE_PROMPT: Self = Self(32);
    pub(crate) const OUTPUT_END: Self = Self(64);
    pub(crate) const WRAPPED: Self = Self(128);

    /// Returns whether all bits of `other` are set.
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Sets all bits present in `other`.
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub(crate) fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub(crate) fn set_exit(&mut self, exit: Option<i32>) {
        self.0 &= !(Self::FAILED.0 | Self::SUCCEEDED.0);
        if let Some(exit) = exit {
            self.insert(if exit == 0 {
                Self::SUCCEEDED
            } else {
                Self::FAILED
            });
        }
    }
}

#[cfg(test)]
#[test]
fn row_size_unchanged() {
    assert_eq!(std::mem::size_of::<Row>(), 40);
}
