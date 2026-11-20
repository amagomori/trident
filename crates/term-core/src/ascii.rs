use super::{Charset, Flags, PrintState, Terminal, clear_wide_pair};
use vte::{Params, Perform};

impl Terminal {
    pub(crate) fn advance_vt(&mut self, parser: &mut vte::Parser, bytes: &[u8]) {
        #[cfg(test)]
        if tests::PER_CHAR.get() {
            parser.advance(self, bytes);
            return;
        }
        let mut writer = Writer {
            term: self,
            bytes: [0; 256],
            len: 0,
        };
        parser.advance(&mut writer, bytes);
        writer.flush();
    }

    #[inline(never)]
    fn put_ascii_run(&mut self, mut bytes: &[u8]) {
        if !self.modes.autowrap
            || self.modes.insert
            || self.has_partial_width()
            || self.charsets[self.active_charset] != Charset::Ascii
        {
            for &b in bytes {
                self.put_char(b as char);
            }
            return;
        }
        let mut template = self.cursor.template;
        template.flags.remove(Flags::WIDE | Flags::WIDE_SPACER);
        while let Some(&last) = bytes.last() {
            if self.cursor.pending_wrap {
                self.sync_cursor_row();
                let row = self.cursor.row;
                self.lines_mut()[row].set_wrapped(true);
                self.cursor.col = 0;
                self.linefeed();
            }
            let (row, col, cols) = (self.cursor.row, self.cursor.col, self.cols);
            let count = bytes.len().min(cols - col);
            let cells = &mut self.lines_mut()[row].cells;
            clear_wide_pair(cells, col);
            if count > 1 {
                clear_wide_pair(cells, col + count - 1);
            }
            for (cell, &b) in cells[col..col + count].iter_mut().zip(&bytes[..count]) {
                *cell = template;
                cell.c = b as char;
            }
            let end = col + count;
            if self.shell.track_input {
                self.record_printed(row, col..end);
            }
            self.cursor.pending_wrap = end == cols;
            self.cursor.col = end.min(cols - 1);
            bytes = &bytes[count..];
            if bytes.is_empty() {
                self.print_state = PrintState(last as u32);
            }
        }
    }
}

struct Writer<'a> {
    term: &'a mut Terminal,
    bytes: [u8; 256],
    len: u8,
}

impl Writer<'_> {
    #[inline(always)]
    fn flush(&mut self) {
        match self.len {
            0 => {}
            // A lone byte costs less through the per-character path.
            1 => self.term.put_char(self.bytes[0] as char),
            len => self.term.put_ascii_run(&self.bytes[..len as usize]),
        }
        self.len = 0;
    }
}

impl Perform for Writer<'_> {
    #[inline]
    fn print(&mut self, c: char) {
        if (' '..='~').contains(&c) {
            self.bytes[self.len as usize] = c as u8;
            self.len = self.len.wrapping_add(1);
            if self.len == 0 {
                self.term.put_ascii_run(&self.bytes);
            }
        } else {
            self.flush();
            self.term.put_char(c);
        }
    }

    #[inline]
    fn execute(&mut self, byte: u8) {
        self.flush();
        self.term.execute(byte);
    }

    #[inline]
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.flush();
        self.term.hook(params, intermediates, ignore, action);
    }

    #[inline]
    fn put(&mut self, byte: u8) {
        self.flush();
        self.term.put(byte);
    }

    #[inline]
    fn unhook(&mut self) {
        self.flush();
        self.term.unhook();
    }

    fn await_dcs_st(&self) -> bool {
        self.term.await_dcs_st()
    }

    fn cancel_dcs(&mut self) {
        self.flush();
        self.term.cancel_dcs();
    }

    #[inline]
    fn raw_osc_limit(&self, code: &[u8]) -> Option<usize> {
        self.term.raw_osc_limit(code)
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.flush();
        self.term.osc_dispatch(params, bell_terminated);
    }

    #[inline]
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        self.flush();
        self.term
            .csi_dispatch(params, intermediates, ignore, action);
    }

    #[inline]
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.flush();
        self.term.esc_dispatch(intermediates, ignore, byte);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{Cell, Row, images::Placement, search::Match};

    thread_local! {
        /// Route `advance_vt` through `Terminal::print`, one `put_char` per character.
        pub(crate) static PER_CHAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[derive(Clone, Copy, Debug)]
    struct Setup {
        cols: usize,
        rows: usize,
        scrollback: Option<usize>,
    }

    const SETUPS: &[Setup] = &[
        Setup {
            cols: 8,
            rows: 4,
            scrollback: None,
        },
        Setup {
            cols: 5,
            rows: 3,
            scrollback: Some(0),
        },
        Setup {
            cols: 11,
            rows: 5,
            scrollback: Some(1),
        },
    ];

    fn terminal(setup: Setup) -> Terminal {
        let mut t = Terminal::new(setup.cols, setup.rows);
        if let Some(lines) = setup.scrollback {
            t.set_scrollback_limit(lines);
        }
        t
    }

    /// Everything printing can touch.
    #[allow(clippy::type_complexity)]
    fn state(
        t: &Terminal,
    ) -> (
        (&[Row], &std::collections::VecDeque<Row>, &[Row]),
        (&[Placement], &[Placement]),
        (usize, usize, bool, Cell),
        String,
        (u64, bool, usize, usize),
        ([Charset; 2], usize, u32),
        (&[u8], &Option<String>, bool),
    ) {
        (
            (&t.primary.lines, &t.primary.scrollback, &t.alternate.lines),
            (&t.primary.placements, &t.alternate.placements),
            (
                t.cursor.row,
                t.cursor.col,
                t.cursor.pending_wrap,
                t.cursor.template,
            ),
            format!("{:?} {:?}", t.primary.saved, t.alternate.saved),
            (t.history, t.alt_active, t.scroll_top, t.scroll_bottom),
            (t.charsets, t.active_charset, t.print_state.0),
            (&t.responses, &t.title, t.bell),
        )
    }

    fn searches(t: &Terminal) -> Vec<Result<Vec<Match>, String>> {
        // Unicode classes like `\w` take long to compile in debug builds.
        [
            ("a", false),
            ("e", false),
            ("日", false),
            ("é", false),
            ("ab", false),
        ]
        .into_iter()
        .chain([("[a-z0-9]+", true), (" [^ ]", true)])
        .map(|(query, regex)| t.search(query, regex))
        .collect()
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Chunk boundaries: every single split, 200 seeded random partitions, one byte per call.
    fn partitions(len: usize) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = (0..=len).map(|split| vec![split, len]).collect();
        let mut rng = Rng(0x2545_F491_4F6C_DD1D ^ len as u64);
        for _ in 0..200 {
            let mut cuts: Vec<usize> = (0..rng.below(8) + 1).map(|_| rng.below(len + 1)).collect();
            cuts.push(len);
            cuts.sort_unstable();
            out.push(cuts);
        }
        out.push((1..=len).collect());
        out
    }

    /// Feeds `input` cut at `cuts` to a batched and a per-character terminal, comparing
    /// their whole state after every chunk and what search finds at the end.
    fn check(setup: Setup, input: &[u8], cuts: &[usize], search: bool) {
        let mut batched = terminal(setup);
        let mut per_char = terminal(setup);
        let mut start = 0;
        for &end in cuts {
            let chunk = &input[start..end];
            batched.advance(chunk);
            PER_CHAR.set(true);
            per_char.advance(chunk);
            PER_CHAR.set(false);
            assert_eq!(
                state(&batched),
                state(&per_char),
                "{setup:?} input {:?} cut at {cuts:?}, after {end}",
                String::from_utf8_lossy(input),
            );
            start = end;
        }
        if search {
            assert_eq!(
                searches(&batched),
                searches(&per_char),
                "{setup:?} {cuts:?}"
            );
        }
    }

    fn compare(input: &[u8]) {
        for &setup in SETUPS {
            // Search compiles regexes, so it samples partitions; cells are compared for all.
            for (i, cuts) in partitions(input.len()).iter().enumerate() {
                check(setup, input, cuts, i % 32 == 0);
            }
        }
    }

    #[test]
    fn plain_runs_shorter_equal_and_longer_than_a_row() {
        for input in [
            "abc",
            "abcde",
            "abcdefgh",
            "abcdefghijk",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "abcdefgh\r\nijklmnop\r\nqrstuvwxyz",
        ] {
            compare(input.as_bytes());
        }
        compare(&b"0123456789ABCDEF".repeat(15));
    }

    #[test]
    fn runs_crossing_the_buffer_size() {
        let pattern = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(15);
        for len in [255, 256, 257, 512] {
            let mut input = pattern[..len].to_vec();
            compare(&input);
            input.extend_from_slice(b"\r\nxy");
            compare(&input);
        }
    }

    #[test]
    fn last_column_and_pending_wrap() {
        for input in [
            "abcde\x1b[1;5HXY",
            "abcdefgh\x1b[1;8HXYZ",
            "abcdefgh\r\nij",
            "abcdefgh\x08XY",
            "abcde\x1b[AXY",
            "abcdefghijk\x1b7\x1b[Hxy\x1b8zz",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn modes_that_fall_back_per_character() {
        for input in [
            "\x1b[?7labcdefghijklmn\x1b[?7hxyzxyzxyzxyz",
            "abcdef\x1b[1;3H\x1b[4hXYZ\x1b[4lQRSTUVWXYZ",
            "\x1b(0lqqk\x1b(B",
            "\x1b(0lqqqqqqqqqqqk\nx   x\nmqqj\x1b(Babc",
            "\x1b)0ab\x0elqqk\x0fcd\x0exx\x0f",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn writes_over_wide_characters() {
        for input in [
            "日本語\rab",
            "日本語\x1b[1;2Hx",
            "日本語\x1b[1;3Hxyz",
            "a日本語日本\x1b[1;1Hxyzxyzxyz",
            "abcd日\x1b[1;4Hxy",
            "abcdefg日",
            "abcdefgh😀",
            "abcdefg🦀x",
            "abcd日本語日本語\x1b[2;1Habcdefghij",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn attributes_and_regions() {
        for input in [
            "aaa\x1b[31mbbb\x1b[0m",
            "\x1b[1;4;7mbold\x1b[22mnormal\x1b[38;2;1;2;3mrgb\x1b[m",
            "\x1b[44m\x1b[2Kabc\x1b[Kdefghijkl\x1b[0m\x1b[J",
            "\x1b[2;3r\x1b[2;1Habcdefghijklmnopqrstuvwxyz\r\n\r\n\r\nzyxwvutsrqponm\x1b[r",
            "\x1b[2;3r\x1b[?6h\x1b[1;1Habcdefghijklmnopqrst\x1b[?6l\x1b[rxyz",
            "abc\x1b[?1049hdefghijklmnopqrst\x1b[?1049lxyz",
            "abc\x1b[?1049h\x1b[2Jdefgh\r\n\x1b[?1049l\x1b[3J\x1b[H\x1b[2Jxyz",
        ] {
            compare(input.as_bytes());
        }
        let lines: String = (0..30)
            .map(|i| format!("line {i} with some text\r\n"))
            .collect();
        compare(lines.as_bytes());
    }

    #[test]
    fn repeat_and_clear_after_runs() {
        for input in [
            "ab\x1b[3b",
            "abcdefgh\x1b[3b",
            "abcde\x1b[b",
            "abc\x1b[31m\x1b[2b",
            "abc\x1b[H\x1b[2J\x1b[3Jdef",
            "abc\x1b[3J\x1b[H\x1b[2Jdef\x1b[2b",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn unicode_and_controls_inside_runs() {
        for input in [
            "e\u{301}",
            "abce\u{301}xyz",
            "abcdefgh\u{301}ij",
            "ab👩\u{200d}💻cd",
            "abcdefg👩\u{200d}💻",
            "a\tb\x08c\rd\n",
            "abc\x07def\x00ghi\x7fjkl",
            "café",
            "naïve über façade",
            "abc\u{9b}31mdef",
            "abc\x1b]2;title\x07def\x1bPignored\x1b\\ghi\x1b[6n",
        ] {
            compare(input.as_bytes());
        }
        compare(b"abc\xf0\x9fXYZ\xffdef\x1b[\x18ghi\xc3");
    }

    #[test]
    fn mixed_real_world_output() {
        for input in [
            "╭─ user@host ~/src/terminal ─╮\r\n╰─❯ ls\r\n",
            "-rw-r--r--  1 me staff  12 Nov 13 \x1b[01;34mrésumé\x1b[0m\r\n\
             -rw-r--r--  1 me staff  99 Nov 13 \x1b[01;32mnaïve.sh\x1b[0m\r\n",
            "* \x1b[33mabc1234\x1b[m Fix wrap\r\n|\\  \r\n| * \x1b[33mdef5678\x1b[m Ünïcode\r\n|/  \r\n",
            "\x1b[1m┌──────┐\x1b[0m\r\n│ 日本 │ ok\r\n└──────┘\r\n",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn kitty_images_next_to_text() {
        for input in [
            "ab\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\cd",
            "abcdefg\x1b_Ga=T,f=24,s=1,v=1,c=2,r=1;AAAA\x1b\\hijk",
            "ab\x1b_Ga=q,i=1;AAAA\x1b\\cd\x1b_Gq\x18ef",
        ] {
            compare(input.as_bytes());
        }
    }

    #[test]
    fn ghost_writes_then_runs() {
        let setup = Setup {
            cols: 8,
            rows: 3,
            scrollback: Some(1),
        };
        let input: Vec<u8> = (0..12)
            .flat_map(|i| format!("row{i} \x1b[3{}mabcdefghij\x1b[m\n", i % 8).into_bytes())
            .collect();
        assert!(terminal(setup).ghost_split(&input).is_some());
        let before = crate::ghost::ADVANCES.get();
        check(setup, &input, &[input.len()], true);
        assert_eq!(crate::ghost::ADVANCES.get() - before, 2);
        for (i, cuts) in partitions(input.len()).iter().enumerate() {
            check(setup, &input, cuts, i % 32 == 0);
        }
    }

    #[test]
    fn seeded_fuzz() {
        const PIECES: &[&str] = &[
            "a",
            "Z",
            " ",
            "~",
            "日",
            "語",
            "😀",
            "🦀",
            "é",
            "\u{301}",
            "\u{200d}",
            "─",
            "\r",
            "\n",
            "\t",
            "\x08",
            "\x1b[31m",
            "\x1b[0m",
            "\x1b[44m",
            "\x1b[K",
            "\x1b[2J",
            "\x1b[3J",
            "\x1b[H",
            "\x1b[2;3H",
            "\x1b[A",
            "\x1b[3b",
            "\x1b[4h",
            "\x1b[4l",
            "\x1b[?7l",
            "\x1b[?7h",
            "\x1b(0",
            "\x1b(B",
            "\x1b)0",
            "\x0e",
            "\x0f",
            "\x1b[2;3r",
            "\x1b[r",
            "\x1b[?6h",
            "\x1b[?6l",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b7",
            "\x1b8",
            "\x1bM",
            "\x1b[2@",
            "\x1b[2P",
            "\x1b[L",
            "\x1b]0;t\x07",
            "\x1b_Ga=T,f=24,s=1,v=1;AAAA\x1b\\",
        ];
        for seed in 1..=256u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let setup = SETUPS[rng.below(SETUPS.len())];
            let mut input = Vec::new();
            for _ in 0..rng.below(60) + 20 {
                if rng.below(3) == 0 {
                    let len = rng.below(3 * setup.cols);
                    input.extend((0..len).map(|_| b' ' + rng.below(95) as u8));
                } else {
                    input.extend_from_slice(PIECES[rng.below(PIECES.len())].as_bytes());
                }
            }
            let mut cuts: Vec<usize> = (0..rng.below(12))
                .map(|_| rng.below(input.len() + 1))
                .collect();
            cuts.push(input.len());
            cuts.sort_unstable();
            check(setup, &input, &cuts, true);
            check(setup, &input, &(1..=input.len()).collect::<Vec<_>>(), false);
        }
    }
}
