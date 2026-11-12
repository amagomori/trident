/// A terminal color as the application specified it; resolved to RGB at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line continues on the next row because it soft-wrapped.
    pub wrapped: bool,
    /// Cells past the right edge, kept when the window narrows so widening brings them back.
    hidden: Vec<Cell>,
    /// The last cell of `cells` was a `PAD` blank standing in for a wide character that no
    /// longer fits; the character itself is the start of `hidden`.
    padded: bool,
}

impl Row {
    pub fn new(cols: usize, template: &Cell) -> Row {
        Row {
            cells: vec![Cell::blank(template); cols],
            wrapped: false,
            hidden: Vec::new(),
            padded: false,
        }
    }

    pub fn resize(&mut self, cols: usize) {
        let mut cells = std::mem::take(&mut self.cells);
        let mut hidden = std::mem::take(&mut self.hidden);
        if std::mem::take(&mut self.padded) {
            let pad = cells.pop().unwrap_or_default();
            if !pad.flags.contains(Flags::PAD) {
                // The stand-in was overwritten, so the wide character it covered is gone.
                cells.push(pad);
                hidden[..2].fill(Cell::default());
                hidden.remove(0);
            }
        }
        cells.append(&mut hidden);

        if cells.len() > cols {
            let wide_at_edge = cells[cols - 1].flags.contains(Flags::WIDE);
            let cut = if wide_at_edge { cols - 1 } else { cols };
            self.hidden = cells.split_off(cut);
            if wide_at_edge {
                cells.push(Cell {
                    flags: Flags::PAD,
                    ..Cell::default()
                });
                self.padded = true;
            }
            let used = self
                .hidden
                .iter()
                .rposition(|c| *c != Cell::default())
                .map_or(0, |i| i + 1);
            self.hidden.truncate(used);
        } else {
            cells.resize(cols, Cell::default());
        }
        self.cells = cells;
    }

    /// Forgets the cells past the right edge, for when the line is erased to its end.
    pub fn clear_hidden(&mut self) {
        self.hidden.clear();
        self.padded = false;
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
