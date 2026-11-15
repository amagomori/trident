//! Fuzz harness: arbitrary bytes through the parser and grid. The cargo-fuzz target in
//! `fuzz/` and the replay test in `tests/fuzz.rs` both drive it.
//!
//! The first four bytes pick the grid size, the scrollback limit and a seed; the seed
//! splits the rest into `advance` batches and mixes in resizes, viewport scrolling,
//! selections and the other calls the app makes between reads. After every step the
//! harness checks grid invariants and hands the caller a memory budget for what the
//! terminal may still hold.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::{Cell, Row, SelectionKind, Terminal};

/// Decoded image bytes the harness lets the store keep.
pub const IMAGE_QUOTA: usize = 1 << 20;

/// Bytes a terminal may hold beyond its grid, scrollback, images and input-sized state.
const FIXED_BUDGET: usize = 2 << 20;

/// Retained bytes allowed per input byte (titles, links, prompt records and their slack).
const BYTES_PER_INPUT: usize = 64;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Runs `data` through a terminal, panicking on any broken invariant.
pub fn run(data: &[u8]) {
    run_with(data, |_, _| {});
}

/// Like [`run`], calling `check(terminal, budget)` after every step, where `budget` is the
/// most heap memory, in bytes, the terminal may hold at that point.
pub fn run_with(data: &[u8], mut check: impl FnMut(&Terminal, usize)) {
    let header = |i: usize| data.get(i).copied().unwrap_or(0) as usize;
    let body = data.get(4..).unwrap_or_default();
    let mut rng = Rng(u64::from_le_bytes([
        header(3) as u8,
        header(0) as u8,
        header(1) as u8,
        header(2) as u8,
        0,
        0,
        0,
        0,
    ]));
    let scrollback = header(2) % 64;
    let mut t = Terminal::new(header(0) % 100, header(1) % 40);
    t.set_scrollback_limit(scrollback);
    t.set_image_quota(IMAGE_QUOTA);
    let mut fuzz = Fuzz {
        scrollback,
        max_cols: t.cols(),
        max_rows: t.rows(),
        fed: 0,
    };
    let mut rest = body;
    while !rest.is_empty() {
        let max = [1, 16, 4096][rng.below(3)];
        let (chunk, tail) = rest.split_at((1 + rng.below(max)).min(rest.len()));
        rest = tail;
        fuzz.fed += chunk.len();
        match rng.below(8) {
            0 => {
                let mut left = chunk;
                while !left.is_empty() {
                    let used = t.advance_until_sync_end(left);
                    assert!(used > 0 && used <= left.len());
                    left = &left[used..];
                }
            }
            1 => t.advance_preserving_wrap(chunk),
            _ => t.advance(chunk),
        }
        fuzz.settle(&mut t, &mut check);
        if rng.below(4) == 0 {
            fuzz.poke(&mut t, &mut rng);
            fuzz.settle(&mut t, &mut check);
        }
    }
}

struct Fuzz {
    scrollback: usize,
    max_cols: usize,
    max_rows: usize,
    fed: usize,
}

impl Fuzz {
    /// One of the calls the app makes between reads.
    fn poke(&mut self, t: &mut Terminal, rng: &mut Rng) {
        let point = |t: &Terminal, rng: &mut Rng| {
            t.viewport_to_point_signed(rng.below(3 * t.rows()) as isize - t.rows() as isize, {
                rng.below(t.cols() + 2)
            })
        };
        match rng.below(12) {
            0 | 1 => {
                let (cols, rows) = (rng.below(100), rng.below(40));
                t.resize(cols, rows);
                self.max_cols = self.max_cols.max(t.cols());
                self.max_rows = self.max_rows.max(t.rows());
            }
            2 => t.scroll_display(
                rng.below(2 * self.scrollback + 3) as isize - self.scrollback as isize - 1,
            ),
            3 => t.scroll_to_bottom(),
            4 => {
                self.scrollback = rng.below(64);
                t.set_scrollback_limit(self.scrollback);
            }
            5 => {
                let kind = [
                    SelectionKind::Simple,
                    SelectionKind::Word,
                    SelectionKind::Line,
                    SelectionKind::CommandOutput,
                    SelectionKind::Block,
                ][rng.below(5)];
                let start = point(t, rng);
                t.start_selection(start, kind);
                let end = point(t, rng);
                t.update_selection(end);
                let range = t.selection_range();
                let _ = t.is_selected(range, end);
                let _ = t.selection_text();
            }
            6 => t.jump_to_prompt(rng.below(2) == 0),
            7 => t.select_command_output(),
            8 => {
                let line = point(t, rng).line;
                t.scroll_to_line(line);
                let _ = t.link_at(point(t, rng));
            }
            9 => {
                let query = ["a", "E", "\u{2500}", "(.)\\1", "日"][rng.below(5)];
                if let Ok(found) = t.search(query, rng.below(2) == 0)
                    && let Some(m) = found.first()
                {
                    t.reveal_line(m.start.line);
                }
            }
            10 => {
                let _ = t.pinned_command(rng.below(t.rows()) as isize);
                let _ = t.reset_input_modes();
            }
            _ => {
                let dark = rng.below(2) == 0;
                let mut palette = crate::colors::Palette::default();
                if !dark {
                    std::mem::swap(&mut palette.foreground, &mut palette.background);
                }
                t.set_palette(palette);
            }
        }
    }

    /// Drains what the app takes after each read, checks invariants and calls `check`.
    fn settle(&self, t: &mut Terminal, check: &mut impl FnMut(&Terminal, usize)) {
        let _ = t.take_responses();
        let _ = t.take_title();
        let _ = t.take_command_finished();
        let _ = t.take_synchronized_output_started();
        let _ = t.take_shrunk();
        let _ = t.take_recycled_memory_freed();
        t.bell = false;
        let (cols, rows) = (t.cols(), t.rows());
        assert!(cols >= 2 && rows >= 1);
        assert!(t.scrollback_len() <= self.scrollback || t.is_alt_screen());
        assert!(t.display_offset() <= t.scrollback_len());
        for row in 0..rows {
            assert_eq!(t.viewport_row(row).cells.len(), cols, "row {row}");
        }
        for row in [-(rows as isize), -1] {
            let _ = t.viewport_row_signed(row);
        }
        if let Some((row, col)) = t.cursor_position() {
            assert!(
                row < rows && col < cols,
                "cursor at {row},{col} in {cols}x{rows}"
            );
        }
        if let Some((row, col)) = t.input_cursor_position() {
            assert!(row < rows && col < cols, "input cursor at {row},{col}");
        }
        assert!(t.image_bytes() <= IMAGE_QUOTA);
        let _ = t.visible_placements_above(rows);
        check(t, self.budget());
    }

    fn budget(&self) -> usize {
        // Both screens, the scrollback and saved prompt rows, each up to the widest grid
        // seen (narrowing keeps cut cells), at twice their size for growth slack.
        let rows = 2 * self.max_rows + self.scrollback + 1 + crate::MAX_COMMANDS;
        let row_bytes = std::mem::size_of::<Row>() + self.max_cols * std::mem::size_of::<Cell>();
        FIXED_BUDGET + 2 * rows * row_bytes + 2 * IMAGE_QUOTA + BYTES_PER_INPUT * self.fed
    }
}

/// A global allocator counting live heap bytes, for memory checks in fuzz targets and
/// tests: `#[global_allocator] static ALLOC: CountingAlloc = CountingAlloc;`.
pub struct CountingAlloc;

static LIVE: AtomicUsize = AtomicUsize::new(0);

impl CountingAlloc {
    /// Heap bytes currently allocated through this allocator.
    pub fn live() -> usize {
        LIVE.load(Ordering::Relaxed)
    }
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE.fetch_add(new_size, Ordering::Relaxed);
        }
        p
    }
}
