/// A terminal color as the application specified it; resolved to RGB at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Color {
    DefaultFg,
    DefaultBg,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(u16);

impl Flags {
    pub const BOLD: Flags = Flags(1);
    pub const DIM: Flags = Flags(1 << 1);
    pub const ITALIC: Flags = Flags(1 << 2);
    pub const UNDERLINE: Flags = Flags(1 << 3);
    pub const INVERSE: Flags = Flags(1 << 4);
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
    /// `c` indexes a grapheme cluster; read it with `Terminal::grapheme`.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
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
    /// A blank cell that keeps the background of `template`, as erase operations require.
    pub fn blank(template: &Cell) -> Cell {
        Cell {
            bg: template.bg,
            ..Cell::default()
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    pub cells: Vec<Cell>,
    pub flags: RowFlags,
    pub(crate) prompt_col: u32,
    pub(crate) output_col: u16,
    /// Every cell from this column on equals the last one, so `reset` can skip them;
    /// `u8::MAX` means not known. Writers to `cells` must call `touch` or `touch_all`;
    /// `put_char` relies on `Terminal::sync_cursor_row`.
    occupied: u8,
    /// Cells past the right edge, kept when the window narrows so widening brings them back.
    /// Boxed so rows that were never cut stay small; every scroll moves rows around.
    hidden: Option<Box<Hidden>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hidden {
    cells: Vec<Cell>,
    /// The last cell of the row is a `PAD` blank standing in for a wide character that no
    /// longer fits; the character itself is the start of `cells`.
    padded: bool,
}

/// `occupied` is only a bound on the content, so rows that show the same thing are equal.
impl PartialEq for Row {
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
            && self.flags == other.flags
            && self.prompt_col == other.prompt_col
            && self.output_col == other.output_col
            && self.hidden == other.hidden
    }
}

impl Eq for Row {}

impl Row {
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
        self.hidden = None;
    }

    pub(crate) fn erase(&mut self, blank: Row) {
        self.cells = blank.cells;
        self.occupied = blank.occupied;
        self.set_wrapped(false);
        self.hidden = None;
    }

    /// The line continues on the next row because it soft-wrapped.
    pub fn wrapped(&self) -> bool {
        self.flags.contains(RowFlags::WRAPPED)
    }

    /// Marks the row as autowrapped into the next one; cells cut off past its edge no longer
    /// follow its text, so they are dropped.
    pub fn set_wrapped(&mut self, wrapped: bool) {
        if wrapped {
            self.hidden = None;
            self.flags.insert(RowFlags::WRAPPED);
        } else {
            self.flags.remove(RowFlags::WRAPPED);
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

    pub fn new(cols: usize, template: &Cell) -> Row {
        Row {
            cells: vec![Cell::blank(template); cols],
            flags: RowFlags::default(),
            prompt_col: 0,
            output_col: 0,
            occupied: 0,
            hidden: None,
        }
    }

    /// A row holding `cells`, as reflow builds them.
    pub(crate) fn from_cells(cells: Vec<Cell>, flags: RowFlags) -> Row {
        Row {
            cells,
            flags,
            prompt_col: 0,
            output_col: 0,
            occupied: u8::MAX,
            hidden: None,
        }
    }

    /// Takes every cell of the row, with the cells hidden by narrowing back in place.
    pub(crate) fn take_cells(&mut self) -> Vec<Cell> {
        self.touch_all();
        let mut cells = std::mem::take(&mut self.cells);
        if let Some(hidden) = self.hidden.take() {
            let Hidden {
                cells: mut hidden,
                padded,
            } = *hidden;
            if padded {
                let pad = cells.pop().unwrap_or_default();
                if !pad.flags.contains(Flags::PAD) {
                    // The stand-in was overwritten, so the wide character it covered is gone.
                    cells.push(pad);
                    hidden[..2].fill(Cell::default());
                    hidden.remove(0);
                }
            }
            cells.append(&mut hidden);
        }
        cells
    }

    pub fn resize(&mut self, cols: usize) {
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
                let used = hidden
                    .iter()
                    .rposition(|c| *c != Cell::default())
                    .map_or(0, |i| i + 1);
                hidden.truncate(used);
            }
            if wide_at_edge || !hidden.is_empty() {
                self.hidden = Some(Box::new(Hidden {
                    cells: hidden,
                    padded: wide_at_edge,
                }));
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
        self.hidden = None;
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
        self.hidden.is_some()
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
        let h = self.hidden.as_deref()?;
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

    pub fn materialize_wrap_padding(&mut self, from: usize) {
        if self.wrapped() {
            for cell in &mut self.cells[from..] {
                cell.flags.remove(Flags::WIDE_WRAP);
            }
        }
    }

    /// Marks the links the row references and returns how many were not marked yet.
    pub(crate) fn mark_hyperlinks(&self, used: &mut [bool]) -> usize {
        let hidden = self.hidden.as_deref().map_or(&[][..], |h| &h.cells[..]);
        let mut marked = 0;
        for cells in [&self.cells[..], hidden] {
            // Neighbouring cells mostly share a link, so only a change of index is looked up.
            let mut last = 0;
            for cell in cells {
                let index = cell.hyperlink;
                if index != last {
                    last = index;
                    if index != 0 && !used[index as usize] {
                        used[index as usize] = true;
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
            .chain(self.hidden.iter_mut().flat_map(|h| h.cells.iter_mut()))
            .map(|c| &mut c.hyperlink)
    }

    pub(crate) fn is_blank(&self) -> bool {
        self.cells.iter().all(|c| *c == Cell::default())
            && self
                .hidden
                .as_ref()
                .is_none_or(|h| h.cells.iter().all(|c| *c == Cell::default()))
    }

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowFlags(u8);

impl RowFlags {
    pub const PROMPT: Self = Self(1);
    pub const PROMPT_START: Self = Self(2);
    pub const OUTPUT_START: Self = Self(4);
    pub const FAILED: Self = Self(8);
    pub const SUCCEEDED: Self = Self(16);
    pub(crate) const OUTPUT_BEFORE_PROMPT: Self = Self(32);
    pub(crate) const OUTPUT_END: Self = Self(64);
    pub(crate) const WRAPPED: Self = Self(128);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

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
