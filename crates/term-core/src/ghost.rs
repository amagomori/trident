//! Eviction lookahead ("ghost writes"). A batch of plain output with at least
//! `rows + max_scrollback` linefeeds after a point pushes every line written before that
//! point out of scrollback again before the batch ends. So the part before it is parsed only
//! for what outlives it (cursor, pen, line count), and only the rest builds rows.

use super::{Charset, Row, Terminal, kitty::Scan};
use vte::{Params, Perform};

const ESC: u8 = 0x1b;

#[cfg(test)]
thread_local! {
    pub(crate) static ADVANCES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl Terminal {
    /// Where the part of `bytes` that has to be built starts, when everything before it
    /// can be skipped.
    ///
    /// On the primary screen with no scroll region, lines go to scrollback, so a line is gone
    /// once `rows + max_scrollback` linefeeds follow it. Anywhere else a region scroll drops
    /// the line it pushes out, so with the cursor inside the region, `bottom - top + 1`
    /// linefeeds move every row written before them out of it.
    pub(crate) fn ghost_split(&self, bytes: &[u8]) -> Option<usize> {
        match self.ghost_plan(bytes) {
            Plan::Ghost(split) => Some(split),
            Plan::Tail(_) | Plan::Built => None,
        }
    }

    /// [`Terminal::ghost_split`], or where the linear tail of a batch that is long enough
    /// for one but not linear starts.
    pub(crate) fn ghost_plan(&self, bytes: &[u8]) -> Plan {
        let needed = if self.scrolls_to_history() {
            self.rows + self.max_scrollback
        } else if (self.alt_active || self.scroll_top != 0)
            && (self.scroll_top..=self.scroll_bottom).contains(&self.cursor.row)
        {
            self.scroll_bottom - self.scroll_top + 1
        } else {
            return Plan::Built;
        };
        if bytes.len() <= needed
            || !self.parser_ground
            || self.kitty.scan != Scan::Ground
            || !self.modes.autowrap
            || self.modes.insert
            || self.charsets[self.active_charset] != Charset::Ascii
            || !self.screen().placements.is_empty()
            || (self.scrolls_to_history() && self.budget_armed() && !self.budget_allows_ghost())
        {
            return Plan::Built;
        }
        // The built part starts after the linefeed with `needed` more after it. Splitting
        // right after a linefeed leaves no partial UTF-8 in the parser, which vte would
        // decode differently across the split.
        let Some(lf) = memchr::memrchr_iter(b'\n', bytes).nth(needed) else {
            return Plan::Built;
        };
        if linear(bytes) {
            Plan::Ghost(lf + 1)
        } else if bytes.len() >= TAIL_MIN {
            linear_tail(bytes).map_or(Plan::Built, Plan::Tail)
        } else {
            Plan::Built
        }
    }

    /// Parse `bytes` (from [`Terminal::ghost_split`]) without writing any row.
    pub(crate) fn advance_ghost(&mut self, parser: &mut vte::Parser, bytes: &[u8]) {
        #[cfg(test)]
        ADVANCES.update(|count| count + 1);
        let mut ghost = Ghost {
            term: self,
            scrolls: 0,
        };
        parser.advance(&mut ghost, bytes);
        let scrolls = ghost.scrolls;
        if self.scrolls_to_history() {
            self.skip_history(scrolls);
        }
    }

    /// Whether a linefeed at the bottom pushes the top row into scrollback with nothing
    /// below it on the screen, the case `skip_history` accounts for.
    fn scrolls_to_history(&self) -> bool {
        !self.alt_active && self.scroll_top == 0 && self.scroll_bottom == self.rows - 1
    }

    /// Account for `n` lines scrolled into scrollback whose content no longer matters.
    fn skip_history(&mut self, n: usize) {
        if n == 0 || self.max_scrollback == 0 {
            return;
        }
        if self.budget_armed() {
            // Same evictions as building the rows, so commands lose the same middle rows.
            let mut spare = None;
            for _ in 0..n {
                let row = match spare.take() {
                    Some(mut row) => {
                        Row::reset(
                            &mut row,
                            self.cols,
                            &self.cursor.template,
                            &mut self.recycled_memory_freed,
                        );
                        row
                    }
                    None => self.blank_row(),
                };
                spare = self.push_history(row);
            }
            return;
        }
        self.history += n as u64;
        let len = self.primary.scrollback.len();
        let new_len = (len + n).min(self.max_scrollback);
        for _ in len..new_len {
            let row = self.blank_row();
            self.primary.scrollback.push_back(row);
        }
        if self.display_offset > 0 {
            self.display_offset = (self.display_offset + n).min(new_len);
        }
    }
}

/// The parser's view of a terminal in the skipped part of a batch: prints and linefeeds
/// only move the cursor and count scrolls; everything else goes to the terminal.
struct Ghost<'a> {
    term: &'a mut Terminal,
    scrolls: usize,
}

impl Ghost<'_> {
    fn linefeed(&mut self) {
        let t = &mut *self.term;
        t.cursor.pending_wrap = false;
        if t.cursor.row == t.scroll_bottom {
            self.scrolls += 1;
        } else if t.cursor.row < t.rows - 1 {
            t.cursor.row += 1;
        }
    }
}

impl Perform for Ghost<'_> {
    /// `put_char` with autowrap on, insert off and the ASCII charset, minus the cell writes.
    fn print(&mut self, c: char) {
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            return;
        }
        if self.term.cursor.pending_wrap {
            self.term.cursor.col = 0;
            self.linefeed();
        }
        let cols = self.term.cols;
        if width == 2 && self.term.cursor.col == cols - 1 {
            self.term.cursor.col = 0;
            self.linefeed();
        }
        let t = &mut *self.term;
        t.print_state = super::PrintState(c as u32);
        if t.cursor.col + width >= cols {
            t.cursor.col = cols - 1;
            t.cursor.pending_wrap = true;
        } else {
            t.cursor.col += width;
        }
    }

    fn execute(&mut self, byte: u8) {
        if let 0x0a..=0x0c = byte {
            self.linefeed();
            if self.term.modes.linefeed_newline {
                self.term.cursor.col = 0;
            }
        } else {
            Perform::execute(self.term, byte);
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        Perform::csi_dispatch(self.term, params, intermediates, ignore, action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        Perform::esc_dispatch(self.term, intermediates, ignore, byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        Perform::osc_dispatch(self.term, params, bell_terminated);
    }
}

/// Output that only prints, moves within the line (CR, TAB, BS), feeds lines and sets SGR.
/// Parsed from ground, every linefeed in it scrolls or moves the cursor down and nothing
/// moves it up, so the linefeeds after a line bound when it leaves scrollback.
fn linear(bytes: &[u8]) -> bool {
    let mut rest = bytes;
    loop {
        let end = memchr::memchr(ESC, rest).unwrap_or(rest.len());
        if !plain(&rest[..end]) {
            return false;
        }
        if end == rest.len() {
            return true;
        }
        match sgr_len(&rest[end..]) {
            Some(len) => rest = &rest[end + len..],
            None => return false,
        }
    }
}

/// How [`Terminal::advance`] takes a batch.
pub(crate) enum Plan {
    /// Skip writing the rows before this split.
    Ghost(usize),
    /// Build the bytes before this offset, then plan the rest again.
    Tail(usize),
    Built,
}

/// Batches at least this long that `linear` rejects are worth scanning for a linear tail.
pub(crate) const TAIL_MIN: usize = if cfg!(test) { 1 } else { 64 * 1024 };

/// Start of the longest suffix of `bytes` that follows a linefeed and is `linear`, when
/// something before it isn't: vtebench's sample boundaries end batches of plain output.
/// Walks back from the end, so it reads only the suffix and the escape before it.
pub(crate) fn linear_tail(bytes: &[u8]) -> Option<usize> {
    let mut end = bytes.len();
    let bad_end = loop {
        let esc = memchr::memrchr(ESC, &bytes[..end]);
        let sgr = esc.and_then(|e| sgr_len(&bytes[e..]));
        let start = match (esc, sgr) {
            (None, _) => 0,
            (Some(e), Some(len)) => e + len,
            (Some(e), None) => e + 1,
        };
        let run = &bytes[start..end];
        if !plain(run) {
            let last = run.iter().rposition(|b| !plain(std::slice::from_ref(b)));
            break start + last.map_or(0, |i| i + 1);
        }
        match (esc, sgr) {
            (None, _) => return None,
            (Some(e), Some(_)) => end = e,
            (Some(e), None) => break e + 1,
        }
    };
    let tail = bad_end + memchr::memchr(b'\n', &bytes[bad_end..])? + 1;
    (tail < bytes.len()).then_some(tail)
}

fn plain(run: &[u8]) -> bool {
    // Without early exit inside a chunk, so the check vectorizes.
    run.chunks(64).all(|chunk| {
        chunk.iter().fold(true, |ok, &b| {
            ok & ((b >= 0x20 && b != 0x7f)
                | (b == b'\n')
                | (b == b'\r')
                | (b == b'\t')
                | (b == 0x08))
        })
    })
}

/// Length of the `ESC [ <digits ; :> m` sequence at the start of `bytes`.
fn sgr_len(bytes: &[u8]) -> Option<usize> {
    if bytes.get(1) != Some(&b'[') {
        return None;
    }
    let params = bytes[2..]
        .iter()
        .take_while(|b| matches!(b, b'0'..=b'9' | b';' | b':'))
        .count();
    (bytes.get(2 + params) == Some(&b'm')).then_some(3 + params)
}

/// Whether vte is in its ground state after `bytes`, given whether it was before. False
/// when unsure: the last escape sequence in `bytes` has to be one this can see end.
pub(crate) fn ends_in_ground(before: bool, bytes: &[u8]) -> bool {
    let Some(esc) = memchr::memrchr(ESC, bytes) else {
        return before;
    };
    let mut rest = bytes[esc + 1..].iter();
    let mut intermediates = false;
    while let Some(&b) = rest.next() {
        match b {
            0x18 | 0x1a => return true,
            0x00..=0x1f | 0x7f => {}
            0x20..=0x2f => intermediates = true,
            b'[' if !intermediates => {
                return rest
                    .find(|&&b| !matches!(b, 0x00..=0x17 | 0x19 | 0x1b..=0x3f | 0x7f))
                    .is_some_and(|&b| matches!(b, 0x18 | 0x1a | 0x40..=0x7e));
            }
            b']' if !intermediates => return rest.any(|&b| matches!(b, 0x07 | 0x18 | 0x1a)),
            b'P' | b'X' | b'^' | b'_' if !intermediates => {
                return rest.any(|&b| matches!(b, 0x18 | 0x1a));
            }
            0x30..=0x7e => return true,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Row};

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % n as u64) as usize
        }
    }

    type Snapshot = (
        Vec<Row>,
        Vec<Row>,
        Vec<Row>,
        u64,
        (usize, usize, bool, Cell),
        Option<char>,
        usize,
        bool,
        Option<String>,
        Vec<u8>,
    );

    fn snapshot(t: &Terminal) -> Snapshot {
        (
            t.primary.lines.clone(),
            t.primary.scrollback.iter().cloned().collect(),
            t.alternate.lines.clone(),
            t.history,
            (
                t.cursor.row,
                t.cursor.col,
                t.cursor.pending_wrap,
                t.cursor.template,
            ),
            t.print_state.last(),
            t.display_offset,
            t.alt_active,
            t.title.clone(),
            t.responses.clone(),
        )
    }

    fn bytewise(t: &mut Terminal, input: &[u8]) {
        for b in input {
            t.advance(std::slice::from_ref(b));
        }
    }

    /// `Terminal::advance` without the batch path.
    fn advance_built(t: &mut Terminal, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut t.parser);
        t.advance_bytes(&mut parser, bytes);
        t.parser = parser;
        t.parser_ground = ends_in_ground(t.parser_ground, bytes);
    }

    #[test]
    fn batch_matches_bytewise() {
        let mut rng = Rng(0x1234_5678_9abc_def0);
        for case in 0..12 {
            let mut input = b"prefix\x1b[31m before\r\n".to_vec();
            while input.len() < 400_000 {
                match rng.below(8) {
                    0 => input.extend(format!("\x1b[{}m", 30 + rng.below(8)).bytes()),
                    1 => input.extend_from_slice("日本語🦀 e\u{301} ".as_bytes()),
                    2 => input.extend_from_slice(b"\r\n"),
                    3 => input.extend_from_slice(b"\t\x08"),
                    _ => {
                        let len = rng.below(if case % 2 == 0 { 30 } else { 300 });
                        input.extend((0..len).map(|_| b' ' + rng.below(95) as u8));
                    }
                }
                if rng.below(6) == 0 {
                    input.extend_from_slice(b"\r\n");
                }
            }
            let mut fast = Terminal::new(80, 24);
            fast.max_scrollback = 200 + case * 50;
            fast.advance(b"hello\r\nworld\x1b[44m");
            if case % 3 == 0 {
                fast.advance(&b"\r\n".repeat(100));
                fast.scroll_display(30);
            }
            if case % 4 == 1 {
                fast.advance(b"\x1b[20h");
            }
            let mut slow = Terminal::new(80, 24);
            slow.max_scrollback = fast.max_scrollback;
            bytewise(&mut slow, b"hello\r\nworld\x1b[44m");
            if case % 3 == 0 {
                bytewise(&mut slow, &b"\r\n".repeat(100));
                slow.scroll_display(30);
            }
            if case % 4 == 1 {
                bytewise(&mut slow, b"\x1b[20h");
            }
            assert!(fast.ghost_split(&input).is_some(), "case {case}");
            fast.advance(&input);
            bytewise(&mut slow, &input);
            assert!(snapshot(&fast) == snapshot(&slow), "case {case}");
        }
    }

    /// Random pieces, mostly ones a linear batch may hold, some that end it or leave the
    /// parser mid-sequence, so chunk edges land everywhere.
    fn piece(rng: &mut Rng, wild: usize, out: &mut Vec<u8>) {
        const PIECES: &[&[u8]] = &[
            b"\r\n",
            b"\n",
            b"\r",
            b"\t",
            b"\x08",
            "日本".as_bytes(),
            "🦀".as_bytes(),
            "e\u{301}".as_bytes(),
            b"\xe6\x97",
            b"\xff",
            b"\xc2\x85",
            b"\x1b[0m",
            b"\x1b[1;38;5;196m",
            b"\x1b[38:2::1:2:3m",
            b"\x1b[",
            b"\x1b",
            b"\x1b]0;title",
            b"\x07",
            b"\x1b\\",
            b"\x1bP1$r",
            b"\x1b_Ga=q,i=1;AAAA\x1b\\",
            b"\x1b[H",
            b"\x1b[2J",
            b"\x1b[3J",
            b"\x1b[2;4r",
            b"\x1b[r",
            b"\x1b[?1049h",
            b"\x1b[?1049l",
            b"\x1b[4h",
            b"\x1b[4l",
            b"\x1b[?7l",
            b"\x1b[?7h",
            b"\x1b(0",
            b"\x1b(B",
            b"\x0e",
            b"\x0f",
            b"\x1b[3b",
            b"\x1bM",
            b"\x18",
            b"\x1b[6n",
        ];
        if rng.below(3) == 0 {
            let p = rng.below(PIECES.len());
            // The linear ones are at the front.
            let p = if rng.below(wild) == 0 { p } else { p % 14 };
            out.extend_from_slice(PIECES[p]);
        } else {
            let len = rng.below(12);
            out.extend((0..len).map(|_| b' ' + rng.below(95) as u8));
            if rng.below(2) == 0 {
                out.extend_from_slice(b"\r\n");
            }
        }
    }

    /// Against the same chunks fully built: vte splits invalid UTF-8 differently byte by byte.
    #[test]
    fn fuzz_batches_match_built() {
        let cases: usize = std::env::var("TERM_FUZZ_CASES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400);
        let seed: u64 = std::env::var("TERM_FUZZ_SEED")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        let mut rng = Rng(seed);
        let mut ghosted = 0;
        for case in 0..cases {
            let (cols, rows) = (2 + rng.below(12), 1 + rng.below(6));
            let max_scrollback = rng.below(30);
            let wild = [4, 40, 400, usize::MAX][case % 4];
            let mut input = Vec::new();
            while input.len() < 200 + rng.below(3000) {
                piece(&mut rng, wild, &mut input);
            }
            let mut fast = Terminal::new(cols, rows);
            let mut slow = Terminal::new(cols, rows);
            fast.max_scrollback = max_scrollback;
            slow.max_scrollback = max_scrollback;
            let mut rest = &input[..];
            while !rest.is_empty() {
                let max = if rng.below(2) == 0 { 16 } else { 2000 };
                let len = (1 + rng.below(max)).min(rest.len());
                let (chunk, tail) = rest.split_at(len);
                ghosted += usize::from(fast.ghost_split(chunk).is_some());
                fast.advance(chunk);
                advance_built(&mut slow, chunk);
                if rng.below(8) == 0 {
                    let delta = rng.below(10) as isize;
                    fast.scroll_display(delta);
                    slow.scroll_display(delta);
                }
                assert!(
                    snapshot(&fast) == snapshot(&slow),
                    "seed {seed} case {case}: {cols}x{rows} scrollback {max_scrollback}"
                );
                rest = tail;
            }
        }
        assert!(ghosted > cases / 4, "batch path taken only {ghosted} times");
    }

    #[test]
    fn stays_off_outside_plain_linear_output() {
        let batch = "line\r\n".repeat(20_000);
        let on = Terminal::new(80, 24);
        assert!(on.ghost_split(batch.as_bytes()).is_some());
        for setup in [
            &b"\x1b[?1049h"[..],
            b"\x1b[2;10r\x1b[5H",
            b"\x1b[?1049h\x1b[1;10r\x1b[10H",
        ] {
            let mut t = Terminal::new(80, 24);
            t.advance(setup);
            assert!(t.ghost_split(batch.as_bytes()).is_some(), "{setup:?}");
        }
        for setup in [
            &b"\x1b[2;10r"[..],
            b"\x1b[1;10r\x1b[5H",
            b"\x1b[?1049h\x1b[2;10r\x1b[12H",
            b"\x1b[4h",
            b"\x1b[?7l",
            b"\x1b(0",
            b"\x1b]0;unterminated",
            b"\x1b_Ga=T,f=24,s=1,v=1;AAAA",
        ] {
            let mut t = Terminal::new(80, 24);
            t.advance(setup);
            assert!(t.ghost_split(batch.as_bytes()).is_none(), "{setup:?}");
        }
        let mut t = Terminal::new(80, 24);
        let pixel = crate::Image {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        };
        t.add_image(1, pixel).unwrap();
        t.place_image(1, Default::default()).unwrap();
        assert!(t.ghost_split(batch.as_bytes()).is_none());
        for bad in ["\x1b[H", "\x07", "\x1b[2J", "\x1b]0;t\x07", "\x0e"] {
            let batch = format!("{batch}{bad}");
            assert!(on.ghost_split(batch.as_bytes()).is_none(), "{bad:?}");
        }
    }

    /// vtebench's region workloads and a primary-screen region below the top: lines leave the
    /// region instead of entering scrollback, so the batch path skips them too.
    #[test]
    fn linear_tail_starts_after_the_last_rejected_line() {
        assert_eq!(linear_tail(b"plain\r\nmore\n"), None);
        assert_eq!(linear_tail(b"\x1b[31mred\n\x1b[0m"), None);
        assert_eq!(linear_tail(b"\x1b[Hab\ncd\n"), Some(6));
        assert_eq!(linear_tail(b"a\x07b\ncd\x1b[1mx\n"), Some(4));
        assert_eq!(linear_tail(b"\x1b[2J\x1b[H"), None);
        assert_eq!(linear_tail(b"ab\ncd\x1b[A\n"), None);
        assert_eq!(linear_tail(b"\x1b]0;t\x07\nok\n"), Some(7));
        assert_eq!(linear_tail(b"\x1b"), None);
    }

    /// vtebench resets the screen between samples, so a batch can open with escapes that
    /// keep the rest of it off the batch path.
    #[test]
    fn tails_after_sample_boundaries_match_bytewise() {
        let lines = "y\n".repeat(3000);
        for (scrollback, boundary) in [
            (100, "\x1b[2J\x1b[H"),
            (0, "\x1bc"),
            (100, "\x1b[?1049h\x1b[2;4r\x1b[4H"),
            (100, "\x1b[?1049h\x1b[1;3r\x1b[3H"),
            (100, "\x1b[r\x1b[?1049l\x07\x1b[3;1H"),
            (100, "\x1b]0;mid\x1b\\"),
        ] {
            let batch = format!("{lines}{boundary}{lines}");
            let mut fast = Terminal::new(20, 5);
            let mut slow = Terminal::new(20, 5);
            fast.max_scrollback = scrollback;
            slow.max_scrollback = scrollback;
            let before = ADVANCES.get();
            fast.advance(batch.as_bytes());
            assert_eq!(ADVANCES.get() - before, 1, "{boundary:?}");
            bytewise(&mut slow, batch.as_bytes());
            assert!(snapshot(&fast) == snapshot(&slow), "{boundary:?}");
        }
    }

    #[test]
    fn tails_stay_off_inside_unfinished_sequences() {
        for boundary in ["\x1b[\n", "\x1b]0;t\n", "\x1bP\n"] {
            let batch = format!("ab\n{boundary}{}", "y\n".repeat(100));
            let mut fast = Terminal::new(20, 5);
            let mut slow = Terminal::new(20, 5);
            let before = ADVANCES.get();
            fast.advance(batch.as_bytes());
            assert_eq!(ADVANCES.get(), before, "{boundary:?}");
            bytewise(&mut slow, batch.as_bytes());
            assert!(snapshot(&fast) == snapshot(&slow), "{boundary:?}");
        }
    }

    #[test]
    fn region_batches_match_built() {
        let mut rng = Rng(0x0bad_cafe_f00d_1234);
        for (case, setup) in [
            &b"\x1b[?1049h"[..],
            b"\x1b[?1049h\x1b[1;23r\x1b[23H",
            b"\x1b[?1049h\x1b[1;12r\x1b[12H",
            b"\x1b[?1049h\x1b[2;24r",
            b"\x1b[?1049h\x1b[12;24r\x1b[13H",
            b"top\r\nbottom\x1b[5;20r\x1b[7Hmid",
        ]
        .into_iter()
        .enumerate()
        {
            let mut input = Vec::new();
            while input.len() < 50_000 {
                if rng.below(4) == 0 {
                    input.extend_from_slice(b"y\n");
                } else {
                    piece(&mut rng, usize::MAX, &mut input);
                }
            }
            let mut fast = Terminal::new(30, 24);
            let mut slow = Terminal::new(30, 24);
            fast.max_scrollback = 50;
            slow.max_scrollback = 50;
            fast.advance(b"\x1b[33mfill\r\n\r\nmore\x1b[24Hlast");
            advance_built(&mut slow, b"\x1b[33mfill\r\n\r\nmore\x1b[24Hlast");
            fast.advance(setup);
            advance_built(&mut slow, setup);
            let before = [0, 3, 20, 23].map(|row| fast.viewport_row(row).text());
            let mut ghosted = 0;
            for chunk in input.chunks(1 + rng.below(4000)) {
                ghosted += usize::from(fast.ghost_split(chunk).is_some());
                fast.advance(chunk);
                advance_built(&mut slow, chunk);
                assert!(snapshot(&fast) == snapshot(&slow), "case {case}");
            }
            assert!(ghosted > 0, "case {case}: batch path not taken");
            if !fast.alt_active {
                let outside = |t: &Terminal| [0, 3, 20, 23].map(|row| t.viewport_row(row).text());
                assert_eq!(outside(&fast), before, "case {case}");
            }
        }
    }

    #[test]
    fn ground_tracking() {
        for (bytes, ground) in [
            (&b"text"[..], true),
            (b"\x1b[31mx", true),
            (b"\x1b[3", false),
            (b"\x1b", false),
            (b"\x1b]0;t", false),
            (b"\x1b]0;t\x07", true),
            (b"\x1b]0;t\x1b\\", true),
            (b"\x1bP1$r", false),
            (b"\x1b_Gq", false),
            (b"\x1b_Gq\x18", true),
            (b"\x1b(B", true),
            (b"\x1b(", false),
            (b"\x1b[1\n2m", true),
        ] {
            assert_eq!(ends_in_ground(true, bytes), ground, "{bytes:?}");
        }
        assert!(!ends_in_ground(false, b"text"));
    }
}
