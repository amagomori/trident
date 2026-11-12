use super::*;

fn term(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.advance(input.as_bytes());
    t
}

fn screen(t: &Terminal) -> Vec<String> {
    (0..t.rows()).map(|r| t.viewport_row(r).text()).collect()
}

#[test]
fn prints_and_wraps() {
    let t = term(5, 3, "hello world");
    assert_eq!(screen(&t), ["hello", " worl", "d"]);
    assert!(t.viewport_row(0).wrapped);
    assert_eq!(t.cursor_position(), Some((2, 1)));
}

#[test]
fn pending_wrap_does_not_scroll_early() {
    let t = term(5, 2, "abcde");
    assert_eq!(screen(&t), ["abcde", ""]);
    assert_eq!(t.cursor_position(), Some((0, 4)));
}

#[test]
fn scrolls_into_history() {
    let mut t = term(10, 2, "one\r\ntwo\r\nthree");
    assert_eq!(screen(&t), ["two", "three"]);
    assert_eq!(t.scrollback_len(), 1);
    t.scroll_display(1);
    assert_eq!(screen(&t), ["one", "two"]);
    t.scroll_display(10);
    assert_eq!(t.display_offset(), 1);
}

#[test]
fn cursor_movement_and_erase() {
    let t = term(10, 3, "abcdef\x1b[1;3H\x1b[K\x1b[2;1Hxy\x1b[1D\x1b[@");
    assert_eq!(screen(&t), ["ab", "x y", ""]);
}

#[test]
fn erase_display() {
    let t = term(4, 3, "aaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[J");
    assert_eq!(screen(&t), ["aaaa", "b", ""]);
    let t = term(4, 3, "aaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[1J");
    assert_eq!(screen(&t), ["", "  bb", "cccc"]);
}

#[test]
fn sgr_colors() {
    let t = term(
        10,
        1,
        "\x1b[1;31ma\x1b[38;5;200mb\x1b[38;2;1;2;3;48:2::4:5:6mc\x1b[0md",
    );
    let row = t.viewport_row(0);
    assert_eq!(row.cells[0].fg, Color::Indexed(1));
    assert!(row.cells[0].flags.contains(Flags::BOLD));
    assert_eq!(row.cells[1].fg, Color::Indexed(200));
    assert_eq!(row.cells[2].fg, Color::Rgb(1, 2, 3));
    assert_eq!(row.cells[2].bg, Color::Rgb(4, 5, 6));
    assert_eq!(
        row.cells[3],
        Cell {
            c: 'd',
            ..Cell::default()
        }
    );
}

#[test]
fn scroll_region() {
    let t = term(5, 4, "1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n\n");
    assert_eq!(screen(&t), ["1", "", "", "4"]);
    assert_eq!(t.scrollback_len(), 0);
    let t = term(5, 4, "1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1bM");
    assert_eq!(screen(&t), ["1", "", "2", "4"]);
}

#[test]
fn insert_and_delete_lines() {
    let t = term(5, 4, "1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[L");
    assert_eq!(screen(&t), ["1", "", "2", "3"]);
    let t = term(5, 4, "1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[2M");
    assert_eq!(screen(&t), ["1", "4", "", ""]);
}

#[test]
fn alternate_screen_restores_primary() {
    let mut t = term(10, 3, "shell$ \x1b[?1049h\x1b[Hvim");
    assert!(t.is_alt_screen());
    assert_eq!(screen(&t), ["vim", "", ""]);
    t.advance(b"\x1b[?1049l");
    assert!(!t.is_alt_screen());
    assert_eq!(screen(&t), ["shell$", "", ""]);
    assert_eq!(t.cursor_position(), Some((0, 7)));
}

#[test]
fn wide_characters() {
    let t = term(5, 2, "a\u{4f60}\u{597d}b");
    let row = t.viewport_row(0);
    assert_eq!(row.text(), "a\u{4f60}\u{597d}");
    assert!(row.cells[1].flags.contains(Flags::WIDE));
    assert!(row.cells[2].flags.contains(Flags::WIDE_SPACER));
    assert_eq!(t.viewport_row(1).text(), "b");
    // A wide char that doesn't fit in the last column wraps.
    let t = term(4, 2, "abc\u{4f60}");
    assert_eq!(screen(&t), ["abc", "\u{4f60}"]);
}

#[test]
fn device_status_reports() {
    let mut t = term(10, 5, "\x1b[3;4H\x1b[6n\x1b[c");
    assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[?62;22c");
}

#[test]
fn title_and_modes() {
    let mut t = term(
        10,
        2,
        "\x1b]0;my title\x07\x1b[?2004h\x1b[?1h\x1b[?25l\x1b[5 q",
    );
    assert_eq!(t.take_title().as_deref(), Some("my title"));
    assert!(t.modes.bracketed_paste);
    assert!(t.modes.app_cursor);
    assert_eq!(t.cursor_position(), None);
    assert_eq!(t.cursor_shape, CursorShape::Bar);
}

#[test]
fn line_drawing_charset() {
    let t = term(5, 1, "\x1b(0lqk\x1b(Bq");
    assert_eq!(t.viewport_row(0).text(), "┌─┐q");
}

#[test]
fn tabs() {
    let t = term(20, 1, "a\tb\tc");
    assert_eq!(t.viewport_row(0).text(), "a       b       c");
}

#[test]
fn selection_text_joins_wrapped_lines() {
    let mut t = term(5, 4, "hello world\r\nnext");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(3, 4));
    assert_eq!(t.selection_text().as_deref(), Some("hello world\nnext"));
    t.start_selection(t.viewport_to_point(0, 1), SelectionKind::Word);
    assert_eq!(t.selection_text().as_deref(), Some("hello"));
    t.start_selection(t.viewport_to_point(3, 1), SelectionKind::Line);
    assert_eq!(t.selection_text().as_deref(), Some("next"));
}

#[test]
fn resize_keeps_content_and_cursor() {
    let mut t = term(10, 4, "1\r\n2\r\n3\r\n4");
    t.resize(10, 2);
    assert_eq!(screen(&t), ["3", "4"]);
    assert_eq!(t.scrollback_len(), 2);
    t.resize(10, 4);
    assert_eq!(screen(&t), ["1", "2", "3", "4"]);
    t.resize(3, 4);
    t.advance(b"xy");
    assert_eq!(screen(&t), ["1", "2", "3", "4xy"]);
}

#[test]
fn reset_clears_everything() {
    let t = term(5, 2, "abc\x1b[31m\x1bcd");
    assert_eq!(screen(&t), ["d", ""]);
    assert_eq!(t.viewport_row(0).cells[0].fg, Color::DefaultFg);
}

#[test]
fn backspace_after_last_column_moves_left() {
    let t = term(5, 2, "abcde\x08X");
    assert_eq!(screen(&t), ["abcXe", ""]);
}

#[test]
fn insert_blank_shifts_wide_characters_intact() {
    let t = term(6, 1, "a\u{4f60}b\x1b[1G\x1b[@");
    let row = t.viewport_row(0);
    assert_eq!(row.text(), " a\u{4f60}b");
    assert!(row.cells[2].flags.contains(Flags::WIDE));
    // A wide char whose spacer is pushed off the line is cleared.
    let t = term(4, 1, "ab\u{4f60}\x1b[1G\x1b[@");
    assert_eq!(t.viewport_row(0).text(), " ab");
    // One that still fits in the last two columns is kept.
    let t = term(5, 1, "ab\u{4f60}\x1b[1G\x1b[@");
    assert_eq!(t.viewport_row(0).text(), " ab\u{4f60}");
}

#[test]
fn shrinking_on_alternate_screen_keeps_primary_prompt() {
    let mut t = term(10, 4, "one\r\ntwo\r\n$ \x1b[?1049h\x1b[Hvim");
    t.resize(10, 2);
    t.advance(b"\x1b[?1049l");
    assert_eq!(screen(&t), ["two", "$"]);
    assert_eq!(t.cursor_position(), Some((1, 2)));
}

#[test]
fn narrowing_keeps_cut_off_text_for_widening() {
    let mut t = term(24, 3, "BEGIN-0123456789-END\r\n$ ");
    t.resize(8, 3);
    assert_eq!(screen(&t), ["BEGIN-01", "$", ""]);
    t.resize(14, 3);
    assert_eq!(screen(&t), ["BEGIN-01234567", "$", ""]);
    t.resize(30, 3);
    assert_eq!(screen(&t), ["BEGIN-0123456789-END", "$", ""]);
}

#[test]
fn narrowing_keeps_cut_off_text_in_scrollback() {
    let mut t = term(20, 2, "BEGIN-0123456789-END\r\none\r\ntwo");
    t.resize(5, 2);
    t.resize(20, 2);
    t.scroll_display(1);
    assert_eq!(screen(&t), ["BEGIN-0123456789-END", "one"]);
}

#[test]
fn narrowing_through_a_wide_character_brings_it_back() {
    let mut t = term(6, 1, "ab\u{4f60}cd");
    t.resize(3, 1);
    assert_eq!(t.viewport_row(0).text(), "ab");
    t.resize(6, 1);
    assert_eq!(t.viewport_row(0).text(), "ab\u{4f60}cd");
    // Writing over the stand-in blank replaces the wide character.
    t.resize(3, 1);
    t.advance(b"\x1b[1;3Hx");
    t.resize(6, 1);
    assert_eq!(t.viewport_row(0).text(), "abx cd");
    // Even with a plain space.
    let mut t = term(6, 1, "ab\u{4f60}cd");
    t.resize(3, 1);
    t.advance(b"\x1b[1;3H ");
    t.resize(6, 1);
    assert_eq!(t.viewport_row(0).text(), "ab  cd");
}

#[test]
fn erasing_to_end_of_line_drops_cut_off_text() {
    let mut t = term(10, 2, "0123456789\r\nabcdefghij");
    t.resize(4, 2);
    t.advance(b"\x1b[2J\x1b[2;1Hxy\x1b[1;3H\x1b[1K");
    t.resize(10, 2);
    assert_eq!(screen(&t), ["", "xy"]);
}

#[test]
fn erasing_backward_keeps_cut_off_text() {
    let mut t = term(10, 1, "0123456789");
    t.resize(4, 1);
    t.advance(b"\x1b[1;4H\x1b[1K");
    t.resize(10, 1);
    assert_eq!(t.viewport_row(0).text(), "    456789");
    t.resize(4, 1);
    t.advance(b"\x1b[1;4H\x1b[K");
    t.resize(10, 1);
    assert_eq!(t.viewport_row(0).text(), "");
}

/// Benchmark workloads for terminal state, run by `.github/scripts/bench.py` to compare a
/// change against main. Ignored by `cargo test`; run them directly with
/// `cargo test --release --workspace -- --ignored --nocapture bench_workloads`.
///
/// Each workload prints one `BENCH {json}` line. `TERM_BENCH` picks workloads by name
/// (comma-separated, default all) and `TERM_BENCH_ITERS` sets the timed repetitions.
/// Only the `measured_*` functions are counted when CI runs this under callgrind.
mod bench {
    use super::super::{Image, PlaceOptions, Terminal};
    use std::hint::black_box;
    use std::time::Instant;

    const COLS: usize = 200;
    const ROWS: usize = 50;
    const INPUT_BYTES: usize = 2 << 20;
    const KEYSTROKES: usize = 2_000;
    const IMAGES: usize = 2_000;
    /// Bytes `term-app` passes to one `advance` call during a flood. Reads (about 1 KiB on
    /// macOS, at most 4095 bytes on Linux) are coalesced in one queue (`pty.rs`) that a flood
    /// drains every 16 ms, or once it holds 1 MiB and 32k lines (8 MiB at most). The
    /// smallest median batch measured in the app, `seq` to 3M on Linux, was about 200 KB;
    /// `cat` of large files reaches 2 to 4 MB.
    const MACOS_BATCH: usize = 192 * 1024;
    const LINUX_BATCH: usize = 192 * 1024;

    /// xorshift64, so every run feeds identical input.
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

    fn fill(mut chunk: impl FnMut(&mut Rng, &mut Vec<u8>)) -> Vec<u8> {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut out = Vec::with_capacity(INPUT_BYTES + 4096);
        while out.len() < INPUT_BYTES {
            chunk(&mut rng, &mut out);
        }
        out
    }

    fn printable(rng: &mut Rng) -> u8 {
        b' ' + rng.below(95) as u8
    }

    /// Long lines of plain ASCII that soft-wrap, like `cat` of a log file.
    fn dense_ascii() -> Vec<u8> {
        fill(|rng, out| {
            let len = 40 + rng.below(400);
            out.extend((0..len).map(|_| printable(rng)));
            out.extend_from_slice(b"\r\n");
        })
    }

    /// Short lines that scroll the screen and fill scrollback, like `seq` or build output.
    fn scrolling() -> Vec<u8> {
        fill(|rng, out| {
            let len = rng.below(30);
            out.extend((0..len).map(|_| printable(rng)));
            out.extend_from_slice(b"\r\n");
        })
    }

    /// A color change every few characters: 16, 256 and truecolor SGR plus bold/italic/reset.
    fn colored_text() -> Vec<u8> {
        fill(|rng, out| {
            match rng.below(5) {
                0 => out.extend(format!("\x1b[{}m", 30 + rng.below(8)).bytes()),
                1 => out.extend(format!("\x1b[38;5;{}m", rng.below(256)).bytes()),
                2 => out.extend(
                    format!(
                        "\x1b[38;2;{};{};{}m",
                        rng.below(256),
                        rng.below(256),
                        rng.below(256)
                    )
                    .bytes(),
                ),
                3 => out.extend_from_slice(b"\x1b[1;3;48;5;236m"),
                _ => out.extend_from_slice(b"\x1b[0m"),
            }
            let len = 1 + rng.below(8);
            out.extend((0..len).map(|_| printable(rng)));
            if rng.below(20) == 0 {
                out.extend_from_slice(b"\r\n");
            }
        })
    }

    /// Absolute cursor moves followed by short writes and erases, like htop or a vim redraw.
    fn cursor_motion() -> Vec<u8> {
        fill(|rng, out| {
            let row = 1 + rng.below(ROWS);
            let col = 1 + rng.below(COLS);
            out.extend(format!("\x1b[{row};{col}H").bytes());
            let len = 1 + rng.below(16);
            out.extend((0..len).map(|_| printable(rng)));
            if rng.below(4) == 0 {
                out.extend_from_slice(b"\x1b[K");
            }
        })
    }

    /// Full-screen repaints on the alternate screen inside a scroll region, like `less` or tmux.
    fn alt_screen() -> Vec<u8> {
        let mut first = true;
        fill(|rng, out| {
            if first {
                out.extend(format!("\x1b[?1049h\x1b[2;{}r", ROWS - 1).bytes());
                first = false;
            }
            out.extend_from_slice(b"\x1b[H\x1b[2J");
            for row in 1..=ROWS {
                out.extend(format!("\x1b[{row};1H\x1b[7m{row:>4}\x1b[27m ").bytes());
                let len = rng.below(COLS - 5);
                out.extend((0..len).map(|_| printable(rng)));
            }
            out.extend(format!("\x1b[{};1H\x1bM\x1b[3S\x1b[2T", ROWS / 2).bytes());
        })
    }

    /// Wide CJK characters, emoji and accented Latin mixed with ASCII.
    fn unicode() -> Vec<u8> {
        const WORDS: &[&str] = &[
            "日本語",
            "中文字符",
            "한국어",
            "🦀",
            "✓",
            "naïve",
            "über",
            "→",
            "😀😀",
        ];
        fill(|rng, out| {
            for _ in 0..12 {
                out.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
                out.push(b' ');
            }
            out.extend_from_slice(b"\r\n");
        })
    }

    #[inline(never)]
    fn measured_advance(term: &mut Terminal, input: &[u8], batch: usize) {
        for chunk in input.chunks(batch) {
            term.advance(black_box(chunk));
        }
    }

    /// One keystroke echoed by the shell, then the walk over every visible cell that the
    /// renderer does to build the next frame: the CPU side of keypress-to-frame latency.
    #[inline(never)]
    fn measured_keystrokes(term: &mut Terminal, count: usize) {
        for i in 0..count {
            if i % 80 == 79 {
                term.advance(black_box(b"\r\n$ "));
            } else {
                term.advance(black_box(b"x"));
            }
            let mut visible = 0usize;
            for row in 0..term.rows() {
                for cell in &term.viewport_row(row).cells {
                    visible = visible.wrapping_add(cell.c as usize);
                }
            }
            black_box(visible);
            black_box(term.cursor_position());
        }
    }

    /// Like `icat` in a loop: decode an image, place it, print a few lines of output so
    /// earlier ones scroll into scrollback. The quota holds 16 images, so most adds evict,
    /// and placements pile up in scrollback until their image goes.
    #[inline(never)]
    fn measured_images(term: &mut Terminal, image: &Image, text: &[u8], count: usize) {
        for i in 0..count {
            let id = (i % 64) as u32 + 1;
            term.add_image(id, black_box(image.clone())).unwrap();
            let opts = PlaceOptions {
                cols: 8,
                rows: 4,
                z: (i % 3) as i32 - 1,
                ..Default::default()
            };
            term.place_image(id, opts).unwrap();
            term.advance(black_box(text));
            black_box(term.visible_placements());
        }
    }

    enum Kind {
        /// Input fed in batches of the given size, like the app's PTY reads.
        Throughput(fn() -> Vec<u8>, usize),
        Keystrokes,
        Images,
    }

    const WORKLOADS: &[(&str, Kind)] = &[
        ("dense_ascii", Kind::Throughput(dense_ascii, usize::MAX)),
        ("scrolling", Kind::Throughput(scrolling, usize::MAX)),
        ("colored_text", Kind::Throughput(colored_text, usize::MAX)),
        ("cursor_motion", Kind::Throughput(cursor_motion, usize::MAX)),
        ("alt_screen", Kind::Throughput(alt_screen, usize::MAX)),
        ("unicode", Kind::Throughput(unicode, usize::MAX)),
        (
            "dense_ascii_macos",
            Kind::Throughput(dense_ascii, MACOS_BATCH),
        ),
        ("scrolling_macos", Kind::Throughput(scrolling, MACOS_BATCH)),
        (
            "colored_text_macos",
            Kind::Throughput(colored_text, MACOS_BATCH),
        ),
        (
            "cursor_motion_macos",
            Kind::Throughput(cursor_motion, MACOS_BATCH),
        ),
        (
            "alt_screen_macos",
            Kind::Throughput(alt_screen, MACOS_BATCH),
        ),
        ("unicode_macos", Kind::Throughput(unicode, MACOS_BATCH)),
        (
            "dense_ascii_linux",
            Kind::Throughput(dense_ascii, LINUX_BATCH),
        ),
        ("scrolling_linux", Kind::Throughput(scrolling, LINUX_BATCH)),
        (
            "colored_text_linux",
            Kind::Throughput(colored_text, LINUX_BATCH),
        ),
        (
            "cursor_motion_linux",
            Kind::Throughput(cursor_motion, LINUX_BATCH),
        ),
        (
            "alt_screen_linux",
            Kind::Throughput(alt_screen, LINUX_BATCH),
        ),
        ("unicode_linux", Kind::Throughput(unicode, LINUX_BATCH)),
        ("keystroke_latency", Kind::Keystrokes),
        ("images", Kind::Images),
    ];

    fn median(secs: &mut [f64]) -> f64 {
        secs.sort_by(f64::total_cmp);
        secs[secs.len() / 2]
    }

    #[test]
    #[ignore = "benchmark; run by .github/scripts/bench.py"]
    fn bench_workloads() {
        let selected = std::env::var("TERM_BENCH").unwrap_or_default();
        let iters: usize = std::env::var("TERM_BENCH_ITERS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5)
            .max(1);
        for (name, kind) in WORKLOADS {
            if !selected.is_empty() && !selected.split(',').any(|s| s == *name) {
                continue;
            }
            let (unit, count, mut secs) = match kind {
                Kind::Throughput(generate, batch) => {
                    let input = generate();
                    let secs: Vec<f64> = (0..iters)
                        .map(|_| {
                            let mut term = Terminal::new(COLS, ROWS);
                            let start = Instant::now();
                            measured_advance(&mut term, &input, *batch);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    ("bytes", input.len(), secs)
                }
                Kind::Keystrokes => {
                    let screen = dense_ascii();
                    let secs: Vec<f64> = (0..iters)
                        .map(|_| {
                            let mut term = Terminal::new(COLS, ROWS);
                            term.advance(&screen);
                            let start = Instant::now();
                            measured_keystrokes(&mut term, KEYSTROKES);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    ("keystrokes", KEYSTROKES, secs)
                }
                Kind::Images => {
                    let image = Image {
                        width: 64,
                        height: 64,
                        pixels: vec![0x80; 64 * 64 * 4],
                    };
                    let text = b"$ ls -l\r\ntotal 42\r\n-rw-r--r-- 1 user user 1234 a.png\r\n\r\n\r\n$ \r\n";
                    let secs: Vec<f64> = (0..iters)
                        .map(|_| {
                            let mut term = Terminal::new(COLS, ROWS);
                            term.set_image_quota(16 * image.pixels.len());
                            let start = Instant::now();
                            measured_images(&mut term, &image, text, IMAGES);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    ("images", IMAGES, secs)
                }
            };
            println!(
                "BENCH {{\"name\":\"{name}\",\"unit\":\"{unit}\",\"count\":{count},\"secs\":{:.9}}}",
                median(&mut secs)
            );
        }
    }
}
