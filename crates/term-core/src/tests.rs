use super::*;

#[test]
fn input_cursor_ignores_visibility_but_tracks_viewport_and_alternate_screen() {
    let mut t = Terminal::new(10, 3);
    t.advance(b"\x1b[2;4H\x1b[?25l");
    assert_eq!(t.cursor_position(), None);
    assert_eq!(t.input_cursor_position(), Some((1, 3)));
    t.advance(b"\x1b[?1049h\x1b[3;7H");
    assert_eq!(t.input_cursor_position(), Some((2, 6)));
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.input_cursor_position(), Some((1, 3)));
    t.advance(b"\x1b[3;1H\n\n\n");
    t.scroll_display(3);
    assert_eq!(t.input_cursor_position(), None);
    t.scroll_to_bottom();
    assert_eq!(t.input_cursor_position(), Some((2, 0)));
}

mod budget;
mod golden;
mod row_filter;

#[test]
fn single_prints_use_the_margin_or_physical_edge() {
    for (setup, edge, wrap_to) in [
        ("", 7, 0),
        ("\x1b[?69h", 7, 0),
        ("\x1b[?69h\x1b[3;5s", 4, 2),
        ("\x1b[?69h\x1b[3;5s", 7, 2),
        ("\x1b[?69h\x1b[3;5s\x1b[?69l", 7, 0),
    ] {
        for (glyph, width) in [("x", 1), ("界", 2)] {
            let mut t = Terminal::new(8, 3);
            t.advance(setup.as_bytes());
            let start = edge + 1 - width;
            t.advance(format!("\x1b[1;{}H", start + 1).as_bytes());
            t.advance(glyph.as_bytes());
            assert_eq!(t.cursor.col, edge);
            assert!(t.cursor.pending_wrap);
            t.advance(b"y");
            assert_eq!(t.primary.lines[1].cells[wrap_to].c, 'y');
            assert_eq!((t.cursor.row, t.cursor.col), (1, wrap_to + 1));
            assert!(!t.cursor.pending_wrap);
        }
    }
}

#[test]
fn scrolling_recycles_rows_as_fresh_blanks() {
    for mode in 0..4 {
        let mut t = Terminal::new(4, 3);
        t.set_scrollback_limit(if mode == 0 { 1 } else { 0 });
        if mode == 3 {
            t.advance(b"\x1b[?1049h");
        }
        let top = if mode == 2 { 1 } else { 0 };
        let mut dirty = Row::new(8, &Cell::default());
        dirty.cells.fill(Cell {
            c: 'x',
            fg: Color::Indexed(2),
            bg: Color::Indexed(3),
            flags: Flags::BOLD | Flags::WIDE,
            hyperlink: 7,
        });
        dirty.flags = RowFlags::PROMPT_START;
        dirty.set_wrapped(true);
        dirty.flags.insert(RowFlags::FAILED);
        dirty.flags.insert(RowFlags::OUTPUT_START);
        dirty.prompt_col = 2;
        dirty.output_col = 3;
        dirty.resize(4);
        let allocation = dirty.cells.as_ptr();
        if mode == 0 {
            t.primary.scrollback.push_back(dirty);
            t.history = 1;
            t.scroll_display(1);
        } else {
            t.lines_mut()[top] = dirty;
        }
        t.lines_mut()[2].cells[0].c = 'z';
        t.advance(b"\x1b[1;31;44m");
        t.scroll_up_region(top, 2, 1);
        let blank = &t.screen().lines[2];
        assert_eq!(blank.cells.as_ptr(), allocation, "mode {mode}");
        assert_eq!(*blank, t.blank_row(), "mode {mode}");
        let mut widened = blank.clone();
        widened.resize(8);
        assert!(widened.cells[4..].iter().all(|c| *c == Cell::default()));
        assert_eq!(t.screen().lines[1].text(), "z");
        if mode == 0 {
            assert_eq!(t.primary.scrollback[0].text(), "");
            assert_eq!(t.history, 2);
            assert_eq!(t.display_offset(), 1);
        } else {
            assert_eq!(t.scrollback_len(), 0);
        }
    }
}

/// Every row's cells from `occupied()` on must equal its last cell, or a bounded
/// `Row::reset` would leave stale cells behind.
fn assert_occupancy(t: &Terminal, step: usize) {
    let rows = t.primary.lines.iter().chain(&t.primary.scrollback);
    for (i, row) in rows.chain(&t.alternate.lines).enumerate() {
        let tail = &row.cells[row.occupied()..];
        assert!(
            tail.iter().all(|c| Some(c) == row.cells.last()),
            "step {step} row {i}: {:?}",
            row.text()
        );
    }
}

#[test]
fn row_occupancy_tracks_every_write() {
    const OPS: &[&str] = &[
        "y",
        "abcdefghij",
        "界",
        "\r\n",
        "\n",
        "\r",
        "\x08",
        "\x1b[5D",
        "\x1b[9G",
        "\x1bM",
        "\x1b[K",
        "\x1b[1K",
        "\x1b[2K",
        "\x1b[J",
        "\x1b[1J",
        "\x1b[2J",
        "\x1b[3J",
        "\x1b[3@",
        "\x1b[2P",
        "\x1b[4X",
        "\x1b[2L",
        "\x1b[2M",
        "\x1b[2S",
        "\x1b[2T",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[?7l",
        "\x1b[?7h",
        "\x1b[44m",
        "\x1b[48;5;200m",
        "\x1b[0m",
        "\x1b[7m",
        "\x1b#8",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[r",
        "\x1b[2;4r",
        "\x1b[H",
    ];
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for cols in [6, 40, 300] {
        let mut t = Terminal::new(cols, 6);
        t.set_scrollback_limit(4);
        for step in 0..20_000 {
            if next(40) == 0 {
                let (c, r) = (2 + next(cols + 20), 1 + next(8));
                t.resize(c, r);
            }
            // Several ops per `advance`, so prints and cursor moves land in one batch.
            let mut batch = String::new();
            for _ in 0..1 + next(4) {
                match next(OPS.len() + 5) {
                    0..=3 => {
                        let (r, c) = (1 + next(8), 1 + next(cols + 2));
                        batch.push_str(&format!("\x1b[{r};{c}H"));
                    }
                    4 => batch.extend((0..next(2 * cols)).map(|_| 'x')),
                    op => batch.push_str(OPS[op - 5]),
                }
            }
            t.advance(batch.as_bytes());
            assert_occupancy(&t, step);
        }
    }
}

#[test]
fn bounded_reset_blanks_short_rows_in_a_scroll_region() {
    let mut t = Terminal::new(300, 6);
    t.advance(b"\x1b[?1049h\x1b[1;3r\x1b[3;1H");
    for i in 0..10 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    t.advance(b"\x1b[44m\n\x1b[0m\n");
    let rows = &t.screen().lines;
    assert_eq!(rows[0], t.blank_row());
    let colored = Row::new(
        300,
        &Cell::blank(&Cell {
            bg: Color::Indexed(4),
            ..Cell::default()
        }),
    );
    assert_eq!(rows[1], colored);
    assert_eq!(rows[2], t.blank_row());
    assert_eq!(rows[2].occupied(), 0);
}

#[test]
fn recycling_releases_oversized_rows_after_narrowing() {
    let mut t = Terminal::new(1000, 3);
    t.set_scrollback_limit(100);
    for _ in 0..103 {
        t.advance(b"wide\r\n");
    }
    assert_eq!(t.scrollback_len(), 100);
    assert!(!t.take_recycled_memory_freed());
    assert!(
        t.primary
            .scrollback
            .iter()
            .all(|r| r.cells.capacity() >= 1000)
    );

    t.resize(80, 3);
    assert!(t.take_shrunk());
    assert!(!t.take_recycled_memory_freed());
    for _ in 0..103 {
        t.advance(b"narrow\r\n");
    }
    assert_eq!(t.scrollback_len(), 100);
    for row in t.primary.scrollback.iter().chain(&t.primary.lines) {
        assert_eq!(row.cells.len(), 80);
        assert!(
            row.cells.capacity() <= 160,
            "capacity {}",
            row.cells.capacity()
        );
    }
    assert!(!t.take_shrunk());
    assert!(t.take_recycled_memory_freed());
    assert!(!t.take_recycled_memory_freed());
    for _ in 0..103 {
        t.advance(b"narrow\r\n");
    }
    assert!(!t.take_recycled_memory_freed());
    assert!(!t.take_shrunk());
}

#[test]
fn recycling_preserves_scrollback_order() {
    let mut t = Terminal::new(8, 2);
    t.set_scrollback_limit(2);
    t.advance(b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix");
    assert_eq!(screen(&t), ["five", "six"]);
    let history: Vec<_> = t.primary.scrollback.iter().map(Row::text).collect();
    assert_eq!(history, ["three", "four"]);
    assert_eq!(t.history, 4);
}

#[test]
fn configured_palette_resolves_colors_and_osc_queries() {
    let mut term = Terminal::new(80, 24);
    term.palette.background = (1, 2, 3);
    term.palette.foreground = (4, 5, 6);
    term.palette.cursor = (7, 8, 9);
    term.palette.ansi[1] = (10, 11, 12);
    term.palette.selection = (13, 14, 15);
    let palette = term.palette.clone();
    term.advance(b"\x1bc");
    assert_eq!(term.palette, palette);
    assert_eq!(term.palette.resolve(crate::Color::DefaultBg), (1, 2, 3));
    assert_eq!(term.palette.resolve(crate::Color::Indexed(1)), (10, 11, 12));
    assert_eq!(
        term.palette.resolve(crate::Color::Indexed(196)),
        (255, 0, 0)
    );
    term.advance(b"\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x1b\\");
    assert_eq!(term.take_responses(), b"\x1b]10;rgb:0404/0505/0606\x07\x1b]11;rgb:0101/0202/0303\x07\x1b]12;rgb:0707/0808/0909\x1b\\");
}

#[test]
fn osc_colors_accept_xterm_specs_and_reset() {
    let mut term = Terminal::new(80, 24);
    term.advance(b"\x1b]4;1;#fff;2;rgb:f0f0/0000/0000;3;rgbi:0.5/0.5/0.5;4;CIEXYZ:0.5/0.5/0.5\x07");
    term.advance(b"\x1b]4;1;?;2;?;3;?;4;?\x1b\\");
    assert_eq!(
        term.take_responses(),
        b"\x1b]4;1;rgb:f0f0/f0f0/f0f0\x1b\\\x1b]4;2;rgb:f0f0/0000/0000\x1b\\\x1b]4;3;rgb:c1c1/bbbb/bbbb\x1b\\\x1b]4;4;rgb:dddd/b5b5/a0a0\x1b\\"
    );
    assert_eq!(term.palette.resolve(crate::Color::Indexed(2)), (0xf0, 0, 0));
    for (spec, expected) in [
        ("#f00f00f00", "f0f0/f0f0/f0f0"),
        ("rgbi:1/1/1", "ffff/ffff/ffff"),
        ("rgbi:0.5/0.5/0.5", "c1c1/bbbb/bbbb"),
        ("CIEXYZ:1/1/1", "ffff/ffff/ffff"),
        ("CIEXYZ:0.5/0.5/0.5", "dddd/b5b5/a0a0"),
        ("CIEuvY:1/1/1", "ffff/ffff/ffff"),
        ("CIEuvY:0.5/0.5/0.5", "ffff/a3a3/aeae"),
        ("CIExyY:1/1/1", "ffff/ffff/ffff"),
        ("CIExyY:0.5/0.5/0.5", "f7f7/b3b3/0e0e"),
        ("CIELab:1/1/1", "6c6c/6767/6767"),
        ("CIELab:0.5/0.5/0.5", "5252/4f4f/4f4f"),
        ("CIELuv:1/1/1", "1616/1414/0e0e"),
        ("CIELuv:0.5/0.5/0.5", "0e0e/1313/0e0e"),
        ("TekHVC:1/1/1", "1a1a/1313/0f0f"),
        ("TekHVC:0.5/0.5/0.5", "1111/1313/0e0e"),
    ] {
        term.advance(format!("\x1b]4;5;{spec};5;?\x07").as_bytes());
        assert_eq!(
            term.take_responses(),
            format!("\x1b]4;5;rgb:{expected}\x07").as_bytes()
        );
    }
    term.advance(b"\x1b]104;2\x07");
    assert_eq!(
        term.palette.resolve(crate::Color::Indexed(2)),
        crate::colors::indexed(2)
    );
}

#[test]
fn osc_color_matches_xterm_noncanonical_specs() {
    for (spec, rgb @ (r, g, b)) in [
        ("CIELab:30/10/20", (0x00, 0x55, 0x73)),
        ("CIELuv:60/20/-30", (0xf3, 0x13, 0xed)),
        ("TekHVC:120/70/80", (0x00, 0xc9, 0x49)),
        ("TekHVC:0/54.9/100", (0xff, 0x26, 0x72)),
        ("CIELab:50/0/0", (0x83, 0x7d, 0x7e)),
        ("rgbi:0.3/0.3/0.3", (0x9e, 0x98, 0x98)),
    ] {
        let mut term = Terminal::new(80, 24);
        term.advance(format!("\x1b]4;5;{spec};5;?\x07").as_bytes());
        assert_eq!(
            term.palette.resolve(crate::Color::Indexed(5)),
            rgb,
            "{spec}"
        );
        assert_eq!(
            term.take_responses(),
            format!("\x1b]4;5;rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}\x07").as_bytes(),
            "{spec}"
        );
    }
}

#[test]
fn osc_color_preserves_full_channels_after_xyz_conversion() {
    for (spec, expected) in [
        ("CIELab:100/0/0", "ffff/ffff/ffff"),
        ("CIEXYZ:0.9496644/1/1.4060403", "ffff/ffff/ffff"),
        ("CIEXYZ:0.3202571/0.6805464/0.1429719", "0000/ffff/0000"),
        ("CIEXYZ:0.6294073/0.3194536/1.2630684", "ffff/0000/ffff"),
    ] {
        let mut term = Terminal::new(80, 24);
        term.advance(format!("\x1b]4;5;{spec};5;?\x07").as_bytes());
        assert_eq!(
            term.take_responses(),
            format!("\x1b]4;5;rgb:{expected}\x07").as_bytes(),
            "{spec}"
        );
    }
}

#[test]
fn xterm_color_setting_report() {
    let mut term = Terminal::new(80, 24);
    term.advance(b"\x1bP+q436F\x1b\\");
    assert_eq!(term.take_responses(), b"\x1bP1+r436F=323536\x1b\\");
}

#[test]
fn osc_special_and_dynamic_colors() {
    let mut term = Terminal::new(80, 24);
    term.advance(b"\x1b]4;256;#aaaabbbbcccc\x07\x1b]4;256;?\x07");
    term.advance(b"\x1b]5;0;#ddddeeeeffff\x07\x1b]5;0;?\x07");
    term.advance(b"\x1b]10;#fff;#000;?\x07");
    assert_eq!(
        term.take_responses(),
        b"\x1b]4;256;rgb:aaaa/bbbb/cccc\x07\x1b]5;0;rgb:dddd/eeee/ffff\x07\x1b]12;rgb:c0c0/caca/f5f5\x07"
    );
    assert_eq!(term.palette.foreground, (0xf0, 0xf0, 0xf0));
    assert_eq!(term.palette.background, (0, 0, 0));
    term.advance(b"\x1b]105;0\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    assert_eq!(term.palette.foreground, crate::colors::FOREGROUND);
    assert_eq!(term.palette.background, crate::colors::BACKGROUND);
    assert_eq!(term.palette.cursor, crate::colors::CURSOR);
    term.advance(b"\x1b]10;#fff;#fff;#fff;#fff;#fff;#fff;#fff;#fff;#fff;#fff\x07");
    for code in 110..120 {
        term.advance(format!("\x1b]{code}\x07").as_bytes());
    }
    assert!(term.palette.dynamic.iter().all(Option::is_none));
    term.advance(b"\x1b]5;0;#fff;1;#fff;2;#fff;3;#fff;4;#fff\x07\x1b]105\x07");
    assert!(term.palette.special.iter().all(Option::is_none));
    term.advance(b"\x1b]4;1;#fff\x07\x1b]10;#fff\x07");
    term.advance(b"\x1bc");
    assert_eq!(term.palette.foreground, crate::colors::FOREGROUND);
    assert_eq!(
        term.palette.resolve(crate::Color::Indexed(1)),
        term.palette.ansi[1]
    );
    assert!(term.palette.dynamic.iter().all(Option::is_none));
    term.advance(b"\x1b]4;1;red;2;#fff\x07\x1b]10;red;#000\x07");
    assert_eq!(term.palette.ansi_overrides[1], None);
    assert_eq!(
        term.palette.resolve(crate::Color::Indexed(2)),
        (0xf0, 0xf0, 0xf0)
    );
    assert_eq!(term.palette.background, (0, 0, 0));
    assert_eq!(term.palette.foreground, crate::colors::FOREGROUND);
    term.advance(b"\x1b]4;256;#fff\x07\x1b]4;1;#fff\x07\x1b]104\x07");
    assert_eq!(term.palette.special[0], None);
    assert_eq!(term.palette.ansi_overrides[1], None);
    term.advance(b"\x1b]10;#fff\x07\x1b]110;\x07");
    assert_eq!(term.palette.foreground, crate::colors::FOREGROUND);
}

fn term(cols: usize, rows: usize, input: &str) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.advance(input.as_bytes());
    t
}

fn screen(t: &Terminal) -> Vec<String> {
    (0..t.rows()).map(|r| t.viewport_row(r).text()).collect()
}

#[test]
fn signed_viewport_clamps_to_retained_lines() {
    let mut t = term(10, 2, "one\r\ntwo\r\nthree");
    assert_eq!(t.viewport_row_signed(-1).text(), "one");
    assert_eq!(t.viewport_row_signed(isize::MIN).text(), "one");
    assert_eq!(t.viewport_row_signed(isize::MAX).text(), "three");
    t.scroll_display(1);
    assert_eq!(t.viewport_row_signed(-1).text(), "one");
    assert_eq!(t.viewport_to_point_signed(1, 3), t.viewport_to_point(1, 3));
    t.set_scrollback_limit(0);
    assert_eq!(t.display_offset(), 0);
    assert_eq!(t.viewport_row_signed(-1).text(), "two");
    let empty = Terminal::new(10, 2);
    assert_eq!(empty.viewport_row_signed(-1).text(), "");
}

#[test]
fn prints_and_wraps() {
    let t = term(5, 3, "hello world");
    assert_eq!(screen(&t), ["hello", " worl", "d"]);
    assert!(t.viewport_row(0).wrapped());
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
fn erase_display_all_keeps_used_rows_and_cursor() {
    let mut t = term(10, 4, "one\r\n\r\nthree\x1b[2;3H");
    let cursor = t.cursor_position();
    t.advance(b"\x1b[2J");
    assert_eq!(screen(&t), ["", "", "", ""]);
    assert_eq!(t.cursor_position(), cursor);
    assert_eq!(t.scrollback_len(), 3);
    t.scroll_display(3);
    assert_eq!(screen(&t), ["one", "", "three", ""]);
}

#[test]
fn erase_display_all_blank_and_alternate_do_not_add_history() {
    let mut t = term(10, 3, "\x1b[2J\x1b[2J");
    assert_eq!(t.scrollback_len(), 0);
    t.advance(b"primary\x1b[2J\x1b[?1049halternate\x1b[2J");
    assert_eq!(screen(&t), ["", "", ""]);
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.scrollback_len(), 1);
    t.scroll_display(1);
    assert_eq!(t.viewport_row(0).text(), "primary");
}

#[test]
fn erase_display_all_obeys_history_limit_and_ed3() {
    for limit in [0, 1, 2] {
        let mut t = term(10, 3, "one\r\ntwo\r\nthree");
        t.set_scrollback_limit(limit);
        t.advance(b"\x1b[2J");
        assert_eq!(t.scrollback_len(), limit);
        if limit > 0 {
            t.scroll_display(1);
            assert_eq!(t.viewport_row(0).text(), "three");
        }
        t.advance(b"new\x1b[H\x1b[2J\x1b[3J");
        assert_eq!(t.scrollback_len(), 0);
        assert_eq!(t.display_offset(), 0);
        assert_eq!(screen(&t), ["", "", ""]);
    }
}

#[test]
fn clear_leaves_history_empty_in_either_order() {
    // macOS `clear` sends ED 3 before ED 2; ncurses on Linux sends ED 2 first.
    for clear in [&b"\x1b[3J\x1b[H\x1b[2J"[..], b"\x1b[H\x1b[2J\x1b[3J"] {
        let mut t = filled_history(50);
        t.advance(b"\r\nlast");
        t.advance(clear);
        assert_eq!(t.scrollback_len(), 0);
        assert_eq!(t.display_offset(), 0);
        assert!(screen(&t).iter().all(|r| r.is_empty()));

        // Later output is still kept by the next Ctrl+L.
        t.advance(b"after");
        t.advance(b"\x1b[H\x1b[2J");
        assert_eq!(t.scrollback_len(), 1);
        t.scroll_display(1);
        assert_eq!(t.viewport_row(0).text(), "after");
    }
}

#[test]
fn erase_display_all_after_separate_ed3_keeps_screen() {
    // A program's lone ED 3 (or the app's clear-scrollback action), then Ctrl+L later.
    for ctrl_l in [&b"\x1b[2J"[..], b"\x1b[H\x1b[2J"] {
        let mut t = term(10, 3, "one\r\ntwo");
        t.advance(b"\x1b[3J");
        t.advance(ctrl_l);
        assert_eq!(t.scrollback_len(), 2);
        t.scroll_display(2);
        assert_eq!(screen(&t)[..2], ["one", "two"]);
    }
}

#[test]
fn erase_display_all_after_ed3_and_partial_erase_keeps_screen() {
    let t = term(10, 3, "one\r\ntwo\x1b[3J\x1b[2;1H\x1b[J\x1b[H\x1b[2J");
    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(t.viewport_row(0).text(), "");
    let mut t = term(10, 3, "one\r\ntwo\x1b[3J\x1b[1;2H\x1b[1J\x1b[H\x1b[2J");
    assert_eq!(t.scrollback_len(), 2);
    t.scroll_display(2);
    assert_eq!(screen(&t)[..2], ["  e", "two"]);
}

#[test]
fn erase_display_all_after_ed3_keeps_new_output() {
    let mut t = term(10, 3, "one\r\ntwo");
    t.advance(b"\x1b[3J\x1b[3;1Hthree\x1b[H\x1b[2J");
    assert_eq!(t.scrollback_len(), 3);
    t.scroll_display(3);
    assert_eq!(screen(&t), ["one", "two", "three"]);

    for between in [
        "\x1b[?1049h\x1b[?1049l",
        "\r\n",
        "\x1b[1m",
        "\x1b]0;t\x07",
        "\x1b7",
    ] {
        let t = term(10, 3, &format!("one\r\ntwo\x1b[3J{between}\x1b[H\x1b[2J"));
        assert_eq!(t.scrollback_len(), 2, "{between:?}");
    }
}

#[test]
fn clear_sequence_with_fragmented_ed3() {
    for ed3 in [
        "\x1b[3J",
        "\x1b[03J",
        "\x1b[3;1J",
        "\x1b[3:1J",
        "\x1b[\x073J",
        "\x1b\x00[3J",
        "\x1b[3\x7fJ",
        "\x1b[?3J",
    ] {
        for between in ["", "\x1b[H", "\x1b[f", "new", "\r\n", "\x1b[J", "\x1b[1J"] {
            let bytes = format!("{ed3}{between}\x1b[H\x1b[2J");
            for split in 0..ed3.len() {
                let mut fragmented = term(10, 3, "one\r\ntwo");
                let mut whole = term(10, 3, "one\r\ntwo");
                for chunk in [&bytes.as_bytes()[..split], &bytes.as_bytes()[split..]] {
                    fragmented.advance(chunk);
                }
                whole.advance(bytes.as_bytes());
                assert_eq!(
                    fragmented.history, whole.history,
                    "{bytes:?}, split {split}"
                );
                assert_eq!(fragmented.scrollback_len(), whole.scrollback_len());
                assert_eq!(fragmented.cursor_position(), whole.cursor_position());
                assert_eq!(screen(&fragmented), screen(&whole));
                fragmented.scroll_display(isize::MAX);
                whole.scroll_display(isize::MAX);
                assert_eq!(screen(&fragmented), screen(&whole));
            }
        }
    }
}

#[test]
fn clear_tracking_preserves_repeat_and_disarms_on_unrendered_characters() {
    for c in ['x', '界', '\u{10ffff}'] {
        let mut t = Terminal::new(10, 3);
        t.advance(format!("{c}\x1b[3J\x07\x1b[b").as_bytes());
        assert_eq!(t.print_state.last(), Some(c));
        assert_eq!(t.viewport_row(0).text(), format!("{c}{c}"));
    }

    let mut t = term(10, 3, "one\r\ntwo");
    t.advance("\x1b[3J\u{301}\x1b[H\x1b[2J".as_bytes());
    assert_eq!(t.scrollback_len(), 2);
    assert_eq!(t.print_state.last(), Some('o'));

    let mut t = term(10, 3, "one\r\ntwo");
    t.advance("\x1b[?7l\x1b[3J\x1b[1;10H界\x1b[H\x1b[2J".as_bytes());
    assert_eq!(t.scrollback_len(), 2);
    assert_eq!(t.print_state.last(), Some('o'));
}

#[test]
fn erase_display_all_retains_selection_and_scrolled_view() {
    let mut t = term(10, 3, "old\r\none\r\ntwo\r\nthree");
    t.scroll_display(1);
    t.start_selection(t.viewport_to_point(1, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 2));
    t.advance(b"\x1b[2J");
    assert_eq!(screen(&t), ["old", "one", "two"]);
    assert_eq!(t.selection_text().as_deref(), Some("one"));
    assert_eq!(t.display_offset(), 4);
}

#[test]
fn erase_display_all_keeps_images_and_hidden_cells() {
    let mut t = term(12, 3, "     hidden");
    t.resize(5, 3);
    t.advance(b"\x1b[2J");
    assert_eq!(t.scrollback_len(), 3);
    t.resize(12, 3);
    assert_eq!(t.scrollback_len(), 1);
    t.scroll_display(1);
    assert_eq!(t.viewport_row(0).text(), "     hidden");

    let mut t = Terminal::new(10, 3);
    t.add_image(
        1,
        images::Image {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        },
    )
    .unwrap();
    t.advance(b"\x1b[2;1H");
    t.place_image(
        1,
        images::PlaceOptions {
            cols: 1,
            rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    t.advance(b"\x1b[2J");
    assert_eq!(t.scrollback_len(), 2);
    assert!(t.visible_placements().is_empty());
    t.scroll_display(2);
    assert_eq!(t.visible_placements()[0].row, 1);
    t.advance(b"\x1b[3J");
    assert!(t.visible_placements().is_empty());
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
fn region_scrolls_match_a_line_model() {
    // Rows outside the region stay put; rows inside move by `n`; freed rows are blank.
    let ops: [(&str, usize, usize, usize); 10] = [
        ("S", 2, 5, 1),
        ("S", 2, 5, 3),
        ("S", 2, 5, 9),
        ("T", 2, 5, 1),
        ("T", 2, 5, 2),
        ("T", 1, 7, 9),
        ("L", 3, 6, 2),
        ("M", 3, 6, 2),
        ("M", 1, 8, 4),
        ("L", 5, 6, 1),
    ];
    for (op, top, bottom, n) in ops {
        let mut t = Terminal::new(6, 8);
        let mut model: Vec<String> = (0..8).map(|i| format!("r{i}")).collect();
        t.advance(model.join("\r\n").as_bytes());
        t.advance(format!("\x1b[{top};{bottom}r").as_bytes());
        let (top, bottom) = (top - 1, bottom - 1);
        match op {
            "S" | "T" => t.advance(format!("\x1b[{n}{op}").as_bytes()),
            _ => t.advance(format!("\x1b[{};1H\x1b[{n}{op}", top + 1).as_bytes()),
        }
        let region = &mut model[top..=bottom];
        let k = n.min(region.len());
        let len = region.len();
        if op == "S" || op == "M" {
            region.rotate_left(k);
            region[len - k..].fill(String::new());
        } else {
            region.rotate_right(k);
            region[..k].fill(String::new());
        }
        assert_eq!(screen(&t), model, "{op} {top}..={bottom} n={n}");
        assert_eq!(t.scrollback_len(), 0, "{op}");
    }
}

#[test]
fn region_scrolls_recycle_rows_as_blanks() {
    for op in ["\x1b[2T", "\x1b[2;1H\x1b[2L", "\x1b[2;1H\x1b[2M", "\x1b[2S"] {
        let mut t = Terminal::new(8, 5);
        t.advance(b"\x1b[2;4r");
        for row in 1..4 {
            let line = &mut t.lines_mut()[row];
            line.cells.fill(Cell {
                c: 'x',
                bg: Color::Indexed(3),
                ..Cell::default()
            });
            line.touch_all();
            line.set_wrapped(true);
            line.flags.insert(RowFlags::PROMPT_START);
            line.prompt_col = 2;
        }
        let allocations: Vec<_> = t.screen().lines.iter().map(|r| r.cells.as_ptr()).collect();
        t.advance(b"\x1b[44m");
        t.advance(op.as_bytes());
        let lines = &t.screen().lines;
        let mut moved: Vec<_> = lines.iter().map(|r| r.cells.as_ptr()).collect();
        moved.sort();
        let mut expected = allocations.clone();
        expected.sort();
        assert_eq!(moved, expected, "{op}: rows are reused, not reallocated");
        let blanks = lines.iter().filter(|r| **r == t.blank_row()).count();
        assert_eq!(blanks, 2, "{op}");
        assert!(
            lines
                .iter()
                .all(|r| r.occupied() == 0 || r.text() == "xxxxxxxx")
        );
    }
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
    assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[?62;4;22c");
}

#[test]
fn synchronized_output() {
    let mut t = term(10, 5, "\x1b[?2026$p");
    assert!(!t.modes.synchronized_output);
    assert_eq!(t.take_responses(), b"\x1b[?2026;2$y");
    t.advance(b"\x1b[?2026h\x1b[?2026$p");
    assert!(t.modes.synchronized_output);
    assert_eq!(t.take_responses(), b"\x1b[?2026;1$y");
    t.advance(b"\x1b[?2026l\x1b[?2026$p\x1b[?9999$p");
    assert!(!t.modes.synchronized_output);
    assert_eq!(t.take_responses(), b"\x1b[?2026;2$y\x1b[?9999;0$y");
    t.advance(b"\x1b[?2026h\x1bc");
    assert!(!t.modes.synchronized_output);
}

#[test]
fn private_mode_reports() {
    for mode in [
        1, 6, 7, 9, 25, 47, 1047, 1049, 1000, 1002, 1003, 1004, 1006, 1007, 2004, 2031,
    ] {
        let mut t = term(10, 5, "");
        for (action, status) in [('h', 1), ('l', 2)] {
            t.advance(format!("\x1b[?{mode}{action}\x1b[?{mode}$p").as_bytes());
            assert_eq!(
                t.take_responses(),
                format!("\x1b[?{mode};{status}$y").as_bytes()
            );
        }
    }
    let mut t = term(10, 5, "\x1b[?1000h\x1b[?1002$p\x1b[?1003$p");
    assert_eq!(t.take_responses(), b"\x1b[?1002;2$y\x1b[?1003;2$y");
}

#[test]
fn color_scheme_reports() {
    let dark = crate::colors::Palette::default();
    let light = crate::colors::Palette {
        foreground: (0x34, 0x3b, 0x58),
        background: (0xe1, 0xe2, 0xe7),
        ..dark.clone()
    };
    assert!(dark.is_dark());
    assert!(!light.is_dark());

    let mut t = term(10, 5, "\x1b[?2031$p\x1b[?996n");
    assert_eq!(t.take_responses(), b"\x1b[?2031;2$y\x1b[?997;1n");
    t.set_palette(light.clone());
    assert_eq!(t.take_responses(), b"", "mode 2031 is off");
    t.advance(b"\x1b[?996n");
    assert_eq!(t.take_responses(), b"\x1b[?997;2n");

    t.advance(b"\x1b[?2031h");
    assert!(t.modes.color_scheme_updates);
    assert_eq!(t.take_responses(), b"");
    t.set_palette(dark.clone());
    assert_eq!(t.take_responses(), b"\x1b[?997;1n");
    t.set_palette(dark.clone());
    assert_eq!(t.take_responses(), b"", "same scheme");
    t.set_palette(light.clone());
    assert_eq!(t.take_responses(), b"\x1b[?997;2n");

    t.advance(b"\x1b[?2031l");
    t.set_palette(dark.clone());
    assert_eq!(t.take_responses(), b"");

    t.advance(b"\x1b[?2031h\x1bc");
    assert!(!t.modes.color_scheme_updates);
    t.set_palette(light);
    assert_eq!(t.take_responses(), b"");
}

#[test]
fn synchronized_frame_starts_within_batch() {
    let mut t = term(10, 5, "");
    assert!(!t.take_synchronized_output_started());
    t.advance(b"\x1b[?2026h");
    assert!(t.take_synchronized_output_started());
    assert!(!t.take_synchronized_output_started());
    t.advance(b"\x1b[?2026h");
    assert!(!t.take_synchronized_output_started());
    t.advance(b"\x1b[?2026l\x1b[?2026h");
    assert!(t.modes.synchronized_output);
    assert!(t.take_synchronized_output_started());
    assert!(!t.take_synchronized_output_started());
    t.advance(b"\x1b[?2026l");
    assert!(!t.take_synchronized_output_started());
    t.advance(b"\x1b[?2026h\x1bc");
    assert!(!t.take_synchronized_output_started());
}

#[test]
fn synchronized_frame_end_stops_before_next_frame() {
    let mut t = term(10, 2, "\x1b[?2026hOLD");
    let batch = b"\x1b[?2026l\x1b[?2026h\rNEW";
    let consumed = t.advance_until_sync_end(batch);
    assert_eq!(consumed, b"\x1b[?2026l".len());
    assert!(!t.modes.synchronized_output);
    assert_eq!(t.viewport_row(0).text(), "OLD");
    assert_eq!(
        t.advance_until_sync_end(&batch[consumed..]),
        batch.len() - consumed
    );
    assert!(t.modes.synchronized_output);
    assert_eq!(t.viewport_row(0).text(), "NEW");
    t.advance(b"\x1b[?2026");
    assert_eq!(t.advance_until_sync_end(b"lignored"), 1);
    assert_eq!(t.advance_until_sync_end(b""), 0);
    assert_eq!(t.advance_until_sync_end(b"\x1b[?2026lplain"), 13);
}

#[test]
fn synchronized_reset_after_split_unrelated_escape() {
    let mut t = term(10, 2, "\x1b[?2026hOLD\x1b[3");
    let batch = b"1mX\x1b[?2026l\x1b[?2026h\rNEW";
    assert_eq!(t.advance_until_sync_end(batch), b"1mX\x1b[?2026l".len());
    assert_eq!(t.viewport_row(0).text(), "OLDX");
    assert!(!t.modes.synchronized_output);
}

#[test]
fn synchronized_reset_split_across_multiple_reads() {
    let mut t = term(10, 2, "\x1b[?2026hOLD");
    for byte in b"\x1b[?2026" {
        assert_eq!(t.advance_until_sync_end(&[*byte]), 1);
        assert!(t.modes.synchronized_output);
    }
    assert_eq!(t.advance_until_sync_end(b"l\x1b[?2026h\rNEW"), 1);
    assert!(!t.modes.synchronized_output);
    assert_eq!(t.viewport_row(0).text(), "OLD");
}

#[test]
fn synchronized_split_reset_allows_embedded_controls() {
    for control in [0x00, 0x07, 0x08, 0x0d, 0x19, 0x1f, 0x7f] {
        for split in 1..b"\x1b[?2026l".len() {
            let mut t = term(10, 2, "\x1b[?2026hOLD");
            t.advance(&b"\x1b[?2026l"[..split]);
            // Padding itself may arrive in a separate read.
            assert_eq!(t.advance_until_sync_end(&[control]), 1);
            let mut batch = vec![control];
            batch.extend_from_slice(&b"\x1b[?2026l"[split..]);
            let end = batch.len();
            batch.extend_from_slice(b"\x1b[?2026h\rNEW\x1b[?2026l");
            assert_eq!(t.advance_until_sync_end(&batch), end);
            assert!(!t.modes.synchronized_output);
            assert_eq!(t.viewport_row(0).text(), "OLD");
        }
    }
}

#[test]
fn synchronized_advance_preserves_plain_flood_batching() {
    let mut t = term(10, 2, "");
    let batch = b"line\r\n".repeat(DEFAULT_SCROLLBACK + 100);
    assert!(t.ghost_split(&batch).is_some());
    let before = crate::ghost::ADVANCES.get();
    assert_eq!(t.advance_until_sync_end(&batch), batch.len());
    assert_eq!(crate::ghost::ADVANCES.get() - before, 1);
    assert_eq!(t.viewport_row(0).text(), "line");
}

#[test]
fn skipped_output_moves_the_cursor_over_emoji_sequences_like_built_output() {
    // Line feeds without carriage returns carry the column into the rows that are built.
    let line = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{1f1ef}\u{1f1f5}\u{1f44d}\u{1f3fd}x"
        .to_owned()
        + &"\u{1f468}\u{200d}".repeat(crate::grapheme::MAX_CHARS / 2)
        + "\u{1f469}\n";
    let batch = line.repeat(DEFAULT_SCROLLBACK + 100) + "|";
    let mut ghosted = term(80, 4, "");
    assert!(ghosted.ghost_split(batch.as_bytes()).is_some());
    ghosted.advance(batch.as_bytes());
    let mut built = term(80, 4, "");
    for chunk in batch.as_bytes().chunks(DEFAULT_SCROLLBACK) {
        assert!(built.ghost_split(chunk).is_none());
        built.advance(chunk);
    }
    assert_eq!(ghosted.cursor_position(), built.cursor_position());
    assert_eq!(screen(&ghosted), screen(&built));
}

#[test]
fn synchronized_end_uses_parser_boundaries() {
    let mut t = term(10, 2, "\x1b[?2026h");
    let title = b"\x1b]2;literal ?2026l\x07";
    assert_eq!(t.advance_until_sync_end(title), title.len());
    assert!(t.modes.synchronized_output);
    let reset = b"\x1b[?2026;2004lplain";
    assert_eq!(t.advance_until_sync_end(reset), reset.len());
    assert!(!t.modes.synchronized_output);
    for split in 1..b"\x1b[?2026l".len() {
        let mut t = term(10, 2, "\x1b[?2026h");
        let bytes = b"\x1b[?2026l\x1b[?2026h";
        assert_eq!(t.advance_until_sync_end(&bytes[..split]), split);
        assert_eq!(t.advance_until_sync_end(&bytes[split..]), 8 - split);
        assert!(!t.modes.synchronized_output);
    }
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

fn assert_input_modes(t: &Terminal, mouse: MouseMode, enabled: bool) {
    assert_eq!(t.modes.mouse, mouse);
    assert_eq!(t.modes.sgr_mouse, enabled);
    assert_eq!(t.modes.focus_events, enabled);
    assert_eq!(t.modes.bracketed_paste, enabled);
}

#[test]
fn command_end_resets_input_modes_in_order() {
    for mode in [9, 1000, 1002, 1003] {
        for end in ["\x1b]133;D\x07", "\x1b]133;D;0\x1b\\"] {
            let setup = format!("\x1b[?{mode};1006;1004;2004h");
            let mut t = term(10, 2, &setup);
            t.advance(end.as_bytes());
            assert_input_modes(&t, MouseMode::Off, false);
            t.advance(format!("{setup}{end}\x1b[?2004h").as_bytes());
            assert!(t.modes.bracketed_paste);
            t.modes.bracketed_paste = false;
            assert_input_modes(&t, MouseMode::Off, false);
        }
    }
}

#[test]
fn command_end_preserves_alt_screen_input_modes() {
    let t = term(10, 2, "\x1b[?1049h\x1b[?1003;1006;1004;2004h\x1b]133;D\x07");
    assert_input_modes(&t, MouseMode::Motion, true);
}

#[test]
fn reset_input_modes_reports_changes_and_preserves_alt_screen() {
    let mut t = Terminal::new(10, 2);
    assert!(!t.reset_input_modes());
    for mode in [9, 1000, 1002, 1003, 1006, 1004, 2004] {
        t.advance(format!("\x1b[?{mode}h").as_bytes());
        assert!(t.reset_input_modes());
        assert_input_modes(&t, MouseMode::Off, false);
        assert!(!t.reset_input_modes());
    }
    t.advance(b"\x1b[?1049h\x1b[?1002;1006;1004;2004h");
    assert!(!t.reset_input_modes());
    assert_input_modes(&t, MouseMode::Drag, true);
}

#[test]
fn other_shell_marks_do_not_reset_input_modes() {
    let t = term(
        10,
        2,
        "\x1b[?1000;1006;1004;2004h\x1b]133;A\x07\x1b]133\x07\x1b]133;invalid\x07",
    );
    assert_input_modes(&t, MouseMode::Click, true);
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
fn select_all_starts_at_first_retained_row_and_keeps_the_viewport() {
    let mut t = Terminal::new(8, 4);
    t.set_scrollback_limit(12);
    let lines: Vec<_> = (0..25).map(|i| format!("line{i:02}")).collect();
    t.advance(lines.join("\r\n").as_bytes());
    t.scroll_display(6);
    t.select_all();
    assert_eq!(t.display_offset(), 6);
    assert_eq!(
        t.selection_range(),
        Some((Point { line: 9, col: 0 }, Point { line: 24, col: 7 }))
    );
    assert_eq!(t.selection_text().unwrap(), lines[9..].join("\n"));

    t.advance(b"\x1b[?1049h\x1b[Halt");
    assert!(t.selection_range().is_none());
    t.select_all();
    assert_eq!(t.selection_text().as_deref(), Some("alt\n\n\n"));
    t.advance(b"\x1b[?1049l");
    assert!(t.selection_range().is_none());
    t.select_all();
    assert_eq!(t.selection_text().unwrap(), lines[9..].join("\n"));
}

#[test]
fn select_all_on_an_empty_or_one_character_grid() {
    // The grid clamps requests smaller than 2 x 1 to its minimum size.
    let mut t = Terminal::new(0, 0);
    for text in ["", "X"] {
        t.advance(text.as_bytes());
        t.select_all();
        assert!(t.is_selected(t.selection_range(), Point { line: 0, col: 0 }));
        assert_eq!(t.selection_text().as_deref(), Some(text));
    }
}

#[test]
fn select_all_copies_reflowed_wraps_wide_cells_and_styles() {
    let mut t = term(8, 2, "\x1b[1mabc界<&>\r\nend");
    t.select_all();
    t.resize(4, 2);
    assert!(t.selection_range().is_none());
    t.select_all();
    assert_eq!(t.selection_text().as_deref(), Some("abc界<&>\nend"));
    let html = t.selection_html().unwrap();
    assert!(html.contains("font-weight:bold"), "{html}");
    assert!(html.contains("abc界&lt;&amp;&gt;\nend"), "{html}");
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
fn selected_single_line_checks_bytes_and_logical_breaks() {
    let mut t = term(4, 4, "abc界\u{301}x\r\nnext");
    assert_eq!(t.selected_single_line(64), None);
    t.start_selection(t.viewport_to_point(1, 2), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(0, 0));
    let expected = "abc界\u{301}x";
    assert_eq!(
        t.selected_single_line(expected.len()).as_deref(),
        Some(expected)
    );
    assert_eq!(t.selected_single_line(expected.len() - 1), None);
    t.update_selection(t.viewport_to_point(2, 0));
    assert_eq!(t.selected_single_line(64), None);
    t.start_selection(t.viewport_to_point(2, 0), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(2, 3));
    assert_eq!(t.selected_single_line(64), None);
    t.start_selection(t.viewport_to_point(3, 0), SelectionKind::Line);
    assert_eq!(t.selected_single_line(64), None);
}

#[test]
fn selection_text_skips_padding_of_widened_wrapped_lines() {
    let mut t = term(5, 3, "\x1b[?1049hhello world");
    t.resize(8, 3);
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(2, 7));
    assert_eq!(t.selection_text().as_deref(), Some("hello world"));
    t.start_selection(t.viewport_to_point(0, 2), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 1));
    assert_eq!(t.selection_text().as_deref(), Some("llo w"));
    // Text written over the padding is kept.
    t.advance(b"\x1b[1;6H!");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 4));
    assert_eq!(t.selection_text().as_deref(), Some("hello! worl"));
    t.start_selection(t.viewport_to_point(0, 4), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(1, 6));
    assert_eq!(t.selection_text().as_deref(), Some("o! \nl  "));
}

#[test]
fn selection_text_skips_wide_character_wrap_blank() {
    let mut t = term(5, 2, "abcd\u{754c}x");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 2));
    assert_eq!(t.selection_text().as_deref(), Some("abcd\u{754c}x"));
}

#[test]
fn selection_html_carries_colors_and_styles() {
    let mut t = term(10, 2, "\x1b[1;31mhi\x1b[0m <&>\r\n\x1b[3;4;9mend");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 2));
    let html = t.selection_html().unwrap();
    assert!(html.starts_with("<pre>") && html.ends_with("</pre>"));
    assert!(html.contains("font-weight:bold"));
    assert!(html.contains("font-style:italic"));
    assert!(html.contains("text-decoration:underline line-through"));
    assert!(html.contains("color:#f7768e"), "{html}");
    assert!(html.contains("background-color:#1a1b26"), "{html}");
    assert!(html.contains("&lt;&amp;&gt;"), "{html}");
    assert!(html.contains("hi</span><span"), "{html}");
    assert!(html.contains("\n</span><span"), "{html}");
    assert_eq!(t.selection_text().as_deref(), Some("hi <&>\nend"));
}

#[test]
fn rich_copy_escapes_styled_graphemes_and_trims_each_hard_line() {
    let mut t = term(20, 2, "値\x1b[1m<&\u{301}>\x1b[m\u{3000}  \r\ne\u{301}  ");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 19));

    assert_eq!(
        t.selection_text().as_deref(),
        Some("値<&\u{301}>\ne\u{301}")
    );
    let mut style = String::new();
    t.push_html_style(&Cell::default(), &mut style);
    assert_eq!(
        t.selection_html().unwrap(),
        format!(
            "<pre>\n<span style=\"{style}\">値</span><span style=\"{style};font-weight:bold\">&lt;&amp;\u{301}&gt;\n</span><span style=\"{style}\">e\u{301}</span></pre>"
        )
    );
}

#[test]
fn selection_html_joins_wrapped_lines_and_trims_tail() {
    let mut t = term(5, 4, "hello world\r\nnext");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(3, 4));
    let html = t.selection_html().unwrap();
    assert!(html.contains("hello world\nnext"), "{html}");
    assert!(!html.contains("next "), "{html}");
    assert!(t.selection_html().is_some());
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    assert!(t.selection_html().is_none());
}

#[test]
fn widened_history_copies_implicit_columns_in_both_clipboard_formats() {
    let mut t = term(4, 2, "\x1b[1mabc \x1b[m\r\nnext\r\nlive\r\n");
    t.resize(8, 2);
    t.scroll_display(10);
    assert_eq!(t.primary.scrollback[0].cells.len(), 4);

    let mut plain = String::new();
    t.push_html_style(&Cell::default(), &mut plain);
    let span = format!("<span style=\"{plain}\">");
    t.start_selection(t.viewport_to_point(0, 2), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(0, 6));
    assert_eq!(t.selection_text().as_deref(), Some("c    "));
    assert_eq!(
        t.selection_html().unwrap(),
        format!("<pre>\n<span style=\"{plain};font-weight:bold\">c </span>{span}   </span></pre>")
    );

    t.start_selection(t.viewport_to_point(0, 5), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(1, 7));
    assert_eq!(t.selection_text().as_deref(), Some("   \n   "));
    assert_eq!(
        t.selection_html().unwrap(),
        format!("<pre>\n{span}   \n   </span></pre>")
    );

    t.start_selection(t.viewport_to_point(0, 5), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 7));
    assert_eq!(t.selection_text().as_deref(), Some("\nnext"));
    assert_eq!(
        t.selection_html().unwrap(),
        format!("<pre>\n\n{span}next</span></pre>")
    );
}

#[test]
fn selection_html_keeps_a_leading_blank_line() {
    let mut t = term(5, 2, "\r\nnext");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(1, 3));
    let html = t.selection_html().unwrap();
    // The parser-stripped newline after `<pre>` leaves the empty row's newline.
    assert!(html.starts_with("<pre>\n\n"), "{html}");
}

#[test]
fn selection_html_dims_toward_the_background() {
    let mut t = term(8, 1, "\x1b[31;2mdim");
    t.start_selection(t.viewport_to_point(0, 0), SelectionKind::Simple);
    t.update_selection(t.viewport_to_point(0, 2));
    let html = t.selection_html().unwrap();
    // (0xf7*3+0x1a*2)/5 = 0x9e, (0x76*3+0x1b*2)/5 = 0x51, (0x8e*3+0x26*2)/5 = 0x64
    assert!(html.contains("color:#9e5164"), "{html}");
}

#[test]
fn block_selection_is_a_rectangle_in_every_drag_direction() {
    let mut t = term(6, 3, "abcdef\r\nghijkl\r\nmnopqr");
    for (a, b) in [
        ((0, 1), (2, 3)),
        ((2, 3), (0, 1)),
        ((0, 3), (2, 1)),
        ((2, 1), (0, 3)),
    ] {
        t.start_selection(t.viewport_to_point(a.0, a.1), SelectionKind::Block);
        t.update_selection(t.viewport_to_point(b.0, b.1));
        assert_eq!(t.selection_text().as_deref(), Some("bcd\nhij\nnop"));
        let range = t.selection_range();
        for row in 0..3 {
            for col in 0..6 {
                assert_eq!(
                    t.is_selected(range, t.viewport_to_point(row, col)),
                    (1..=3).contains(&col)
                );
            }
        }
    }
}

#[test]
fn block_selection_keeps_spaces_and_physical_rows() {
    let mut t = term(5, 4, "abcdefghij\r\n\r\nx");
    t.start_selection(t.viewport_to_point(0, 1), SelectionKind::Block);
    assert_eq!(t.selection_text(), None);
    t.update_selection(t.viewport_to_point(3, 3));
    assert_eq!(t.selection_text().as_deref(), Some("bcd\nghi\n   \n   "));
    t.update_selection(t.viewport_to_point(2, 1));
    assert_eq!(t.selection_text().as_deref(), Some("b\ng\n "));
}

#[test]
fn selection_includes_either_half_of_a_wide_character() {
    let mut t = term(6, 2, "a界b\r\ncd界");
    for kind in [SelectionKind::Simple, SelectionKind::Block] {
        t.start_selection(t.viewport_to_point(0, 2), kind);
        t.update_selection(t.viewport_to_point(1, 2));
        let expected = if kind == SelectionKind::Block {
            "界\n界"
        } else {
            "界b\ncd界"
        };
        assert_eq!(t.selection_text().as_deref(), Some(expected));
        let range = t.selection_range();
        assert!(t.is_selected(range, t.viewport_to_point(0, 1)));
        assert!(t.is_selected(range, t.viewport_to_point(1, 3)));
        if kind == SelectionKind::Block {
            assert!(!t.is_selected(range, t.viewport_to_point(0, 3)));
            assert!(!t.is_selected(range, t.viewport_to_point(1, 1)));
        }
    }
}

#[test]
fn block_selection_stays_anchored_in_scrollback_and_clips_evicted_rows() {
    let mut t = term(4, 2, "abcd\r\nefgh");
    t.set_scrollback_limit(1);
    t.start_selection(t.viewport_to_point(0, 1), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(1, 2));
    t.advance(b"\r\nijkl");
    t.scroll_display(1);
    assert_eq!(t.selection_text().as_deref(), Some("bc\nfg"));
    assert!(t.is_selected(t.selection_range(), t.viewport_to_point(0, 1)));
    t.advance(b"\r\nmnop");
    assert_eq!(t.selection_text().as_deref(), Some("fg"));
    t.advance(b"\r\nqrst");
    assert_eq!(t.selection_text(), None);
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
fn resize_keeps_scrolled_back_view_on_its_top_line() {
    let mut t = Terminal::new(10, 4);
    for i in 0..20 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    t.scroll_display(10);
    let top = t.viewport_to_point(0, 0).line;
    assert_eq!(t.viewport_row(0).text(), "7");
    for (cols, rows) in [(10, 2), (6, 7), (20, 4), (20, 9)] {
        t.resize(cols, rows);
        assert_eq!(t.viewport_to_point(0, 0).line, top, "{cols}x{rows}");
        assert_eq!(t.viewport_row(0).text(), "7", "{cols}x{rows}");
    }
    t.scroll_to_bottom();
    t.resize(10, 3);
    assert_eq!(t.display_offset(), 0);
    assert_eq!(t.input_cursor_position(), Some((2, 0)));
}

#[test]
fn resize_clamps_scrolled_back_view_to_history() {
    let mut t = Terminal::new(10, 4);
    t.set_scrollback_limit(5);
    for i in 0..20 {
        t.advance(format!("{i}\r\n").as_bytes());
    }
    t.scroll_display(5);
    assert_eq!(t.viewport_row(0).text(), "12");
    t.resize(10, 2);
    assert_eq!(t.display_offset(), 5);
    assert_eq!(t.viewport_row(0).text(), "14");
    t.scroll_display(-4);
    t.resize(10, 12);
    assert_eq!(t.display_offset(), 0);
    t.advance(b"\x1b[?1049h");
    t.resize(10, 5);
    assert_eq!(t.display_offset(), 0);
}

#[test]
fn reset_clears_everything() {
    let t = term(5, 2, "abc\x1b[31m\x1bcd");
    assert_eq!(screen(&t), ["d", ""]);
    assert_eq!(t.viewport_row(0).cells[0].fg, Color::DefaultFg);
}

#[test]
fn backspace_after_last_column_moves_left() {
    // BS cancels the do-wrap position and lands one cell left of it; a counted
    // CUB instead spends a step on the cancel itself (see golden
    // cub_wrap_pending).
    let t = term(5, 2, "abcde\x08X");
    assert_eq!(screen(&t), ["abcXe", ""]);
}

#[test]
fn printing_in_insert_mode_preserves_width_colors_and_wrap() {
    let t = term(6, 2, "abcde\x1b[1;2H\x1b[4h\x1b[31;44mX");
    assert_eq!(screen(&t), ["aXbcde", ""]);
    let row = t.viewport_row(0);
    assert_eq!(row.cells.len(), 6);
    assert_eq!(row.cells[1].fg, Color::Indexed(1));
    assert_eq!(row.cells[1].bg, Color::Indexed(4));
    assert_eq!(row.cells[2].fg, Color::DefaultFg);

    let t = term(6, 2, "ab你cd\x1b[1;2H\x1b[4h好");
    assert_eq!(screen(&t), ["a好b你", ""]);
    let row = t.viewport_row(0);
    assert_eq!(row.cells.len(), 6);
    for col in [1, 4] {
        assert!(row.cells[col].flags.contains(Flags::WIDE));
        assert!(row.cells[col + 1].flags.contains(Flags::WIDE_SPACER));
    }

    let t = term(6, 2, "abcde\x1b[4hZQ");
    assert_eq!(screen(&t), ["abcdeZ", "Q"]);
    assert_eq!(t.cursor_position(), Some((1, 1)));
    assert!(t.viewport_row(0).wrapped());
    assert_eq!(t.viewport_row(1).cells.len(), 6);
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
    let mut t = term(24, 3, "\x1b[?1049hBEGIN-0123456789-END\r\n$ ");
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
    let mut t = term(6, 1, "\x1b[?1049hab\u{4f60}cd");
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
    let mut t = term(6, 1, "\x1b[?1049hab\u{4f60}cd");
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
    let mut t = term(10, 1, "\x1b[?1049h0123456789");
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

    /// One-character lines scrolling the top half of the alternate screen, like vtebench
    /// `scrolling_bottom_small_region`.
    fn scroll_region() -> Vec<u8> {
        let mut first = true;
        fill(|_, out| {
            if first {
                out.extend(format!("\x1b[?1049h\x1b[1;{}r\x1b[{}H", ROWS / 2, ROWS / 2).bytes());
                first = false;
            }
            out.extend_from_slice(b"y\r\n");
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

    #[inline(never)]
    fn measured_search(term: &Terminal, query: &str) {
        black_box(term.search(black_box(query), false).unwrap());
    }

    enum Kind {
        /// Input fed in batches of the given size, like the app's PTY reads.
        Throughput(fn() -> Vec<u8>, usize),
        Keystrokes,
        Images,
        /// Search over this many history lines of `a`s for the query.
        Search(usize, &'static str),
        /// The second of two OSC 133 runs of one command near the compare and budget caps,
        /// with compare-with-last-run on or off; every line differs when `rewrite`.
        Rerun {
            compare: bool,
            rewrite: bool,
        },
        /// `COMMANDS` short OSC 133 commands, all distinct or all the same command text,
        /// with compare-with-last-run on or off.
        Commands {
            compare: bool,
            distinct: bool,
        },
    }

    /// Enough commands that compare-with-last-run's stored runs reach `rerun::MAX_STORED`.
    const COMMANDS: usize = 20_000;

    /// `COMMANDS` 12-line runs with a timestamp, PID, duration and temp path on every line;
    /// every third run fails one test.
    fn commands_output(distinct: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for run in 0..COMMANDS {
            let command = if distinct {
                format!("./job {run}")
            } else {
                "make test".to_owned()
            };
            out.extend_from_slice(
                format!("\x1b]133;A\x07$ \x1b]133;B\x07{command}\r\n\x1b]133;C\x07").as_bytes(),
            );
            for i in 0..12 {
                let result = if i == 5 && run % 3 == 0 {
                    "FAILED"
                } else {
                    "ok"
                };
                out.extend_from_slice(
                    format!(
                        "[2026-11-15T07:{:02}:{:02}.{:03}Z] pid {} case_{i} ... {result} ({}.{:03}s) /tmp/tmp.{run:06}\r\n",
                        run % 60,
                        i % 60,
                        run % 1000,
                        1000 + run,
                        run % 7,
                        (run * 7 + i) % 1000
                    )
                    .as_bytes(),
                );
            }
            out.extend_from_slice(b"\x1b]133;D;0\x07");
        }
        out.extend_from_slice(b"\x1b]133;A\x07$ ");
        out
    }

    /// Output lines per run: just under `rerun::MAX_LINES` and the 20,000-line per-command
    /// share of a 40,000-line history.
    const RERUN_LINES: usize = 19_500;

    /// One `cargo test`-like run: every line has a timestamp and a duration, and every
    /// 500th line's result depends on `run` (or every line, when `rewrite`).
    fn rerun_output(run: usize, rewrite: bool) -> Vec<u8> {
        let mut out = b"\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C\x07".to_vec();
        for i in 0..RERUN_LINES {
            let result = if rewrite || (i % 500 == 0 && run % 2 == 1) {
                "FAILED"
            } else {
                "ok"
            };
            let line = if rewrite {
                format!("[run {run}] test suite::case_{i} ... {result}\r\n")
            } else {
                format!(
                    "[2026-11-15T07:{:02}:{:02}.{:03}Z] test suite::case_{i} ... {result} ({}.{:03}s)\r\n",
                    run % 60,
                    i % 60,
                    i % 1000,
                    run,
                    (i * 7 + run) % 1000
                )
            };
            out.extend_from_slice(line.as_bytes());
        }
        out.extend_from_slice(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        out
    }

    const WORKLOADS: &[(&str, Kind)] = &[
        ("dense_ascii", Kind::Throughput(dense_ascii, usize::MAX)),
        ("scrolling", Kind::Throughput(scrolling, usize::MAX)),
        ("colored_text", Kind::Throughput(colored_text, usize::MAX)),
        ("cursor_motion", Kind::Throughput(cursor_motion, usize::MAX)),
        ("alt_screen", Kind::Throughput(alt_screen, usize::MAX)),
        ("unicode", Kind::Throughput(unicode, usize::MAX)),
        ("scroll_region", Kind::Throughput(scroll_region, usize::MAX)),
        (
            "osc133_rerun",
            Kind::Rerun {
                compare: true,
                rewrite: false,
            },
        ),
        (
            "osc133_rerun_off",
            Kind::Rerun {
                compare: false,
                rewrite: false,
            },
        ),
        (
            "osc133_rerun_rewrite",
            Kind::Rerun {
                compare: true,
                rewrite: true,
            },
        ),
        (
            "osc133_distinct",
            Kind::Commands {
                compare: true,
                distinct: true,
            },
        ),
        (
            "osc133_distinct_off",
            Kind::Commands {
                compare: false,
                distinct: true,
            },
        ),
        (
            "osc133_repeat",
            Kind::Commands {
                compare: true,
                distinct: false,
            },
        ),
        (
            "osc133_repeat_off",
            Kind::Commands {
                compare: false,
                distinct: false,
            },
        ),
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
            "scroll_region_macos",
            Kind::Throughput(scroll_region, MACOS_BATCH),
        ),
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
        (
            "scroll_region_linux",
            Kind::Throughput(scroll_region, LINUX_BATCH),
        ),
        ("keystroke_latency", Kind::Keystrokes),
        ("images", Kind::Images),
        ("search_dense_10k", Kind::Search(10_000, "a")),
        ("search_dense_100k", Kind::Search(100_000, "a")),
        ("search_dense_1m", Kind::Search(1_000_000, "a")),
        ("search_sparse_100k", Kind::Search(100_000, "b")),
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
                Kind::Search(lines, query) => {
                    let mut term = Terminal::new(80, 24);
                    term.set_scrollback_limit(*lines);
                    let line = [b'a'; 80];
                    for _ in 0..lines + 23 {
                        term.advance(&line);
                        term.advance(b"\r\n");
                    }
                    let secs = (0..iters)
                        .map(|_| {
                            let start = Instant::now();
                            measured_search(&term, query);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    ("keystrokes", 1, secs)
                }
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
                Kind::Rerun { compare, rewrite } => {
                    let first = rerun_output(0, *rewrite);
                    let second = rerun_output(1, *rewrite);
                    let secs: Vec<f64> = (0..iters)
                        .map(|_| {
                            let mut term = Terminal::new(COLS, ROWS);
                            term.set_scrollback_limit(2 * 20_000);
                            term.set_command_budget(true, 0.5);
                            term.set_compare_runs(*compare);
                            term.advance(&first);
                            let start = Instant::now();
                            measured_advance(&mut term, &second, LINUX_BATCH);
                            start.elapsed().as_secs_f64()
                        })
                        .collect();
                    ("bytes", second.len(), secs)
                }
                Kind::Commands { compare, distinct } => {
                    let input = commands_output(*distinct);
                    let secs: Vec<f64> = (0..iters)
                        .map(|_| {
                            let mut term = Terminal::new(COLS, ROWS);
                            term.set_compare_runs(*compare);
                            let start = Instant::now();
                            measured_advance(&mut term, &input, LINUX_BATCH);
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

fn filled_history(lines: usize) -> Terminal {
    let mut t = Terminal::new(20, 5);
    for i in 0..lines {
        t.advance(format!("line {i}\r\n").as_bytes());
    }
    t
}

#[test]
fn scrolling_output_does_not_signal_shrink() {
    let mut t = filled_history(50);
    t.set_scrollback_limit(20);
    assert!(t.take_shrunk());
    for i in 0..100 {
        t.advance(format!("more {i}\r\n").as_bytes());
    }
    assert_eq!(t.scrollback_len(), 20);
    assert!(!t.take_shrunk());
    t.advance(b"\x1b[2J\x1b[H\x1b[1;3r\x1b[3;1H\n\n\n\x1b[r");
    assert!(!t.take_shrunk());
}

#[test]
fn erase_saved_lines_signals_shrink() {
    let mut t = filled_history(50);
    t.advance(b"\x1b[2J\x1b[3J");
    assert_eq!(t.scrollback_len(), 0);
    assert!(t.take_shrunk());
    assert!(!t.take_shrunk());
    // Nothing left to free.
    t.advance(b"\x1b[3J");
    assert!(!t.take_shrunk());
}

#[test]
fn reset_with_scrollback_signals_shrink() {
    let mut t = filled_history(50);
    t.advance(b"\x1bc");
    assert_eq!(t.scrollback_len(), 0);
    assert!(t.take_shrunk());
}

#[test]
fn shrinking_resize_signals_shrink() {
    let mut t = filled_history(50);
    t.resize(30, 8);
    assert!(!t.take_shrunk());
    t.resize(30, 4);
    assert!(t.take_shrunk());
    t.resize(10, 4);
    assert!(t.take_shrunk());
    t.resize(10, 4);
    assert!(!t.take_shrunk());
}

#[test]
fn shrinking_resize_obeys_scrollback_limit() {
    for limit in [0, 2, 10] {
        let mut t = Terminal::new(20, 5);
        t.set_scrollback_limit(limit);
        t.advance(b"zero\r\none\r\ntwo\r\nthree\r\nfour");
        assert_eq!(t.cursor.row, 4);
        t.take_shrunk();
        t.resize(20, 2);
        assert_eq!(t.scrollback_len(), limit.min(3));
        assert_eq!(t.history, 3);
        assert_eq!(t.first_line(), 3 - limit.min(3) as u64);
        assert_eq!(screen(&t), ["three", "four"]);
        assert_eq!(t.display_offset(), 0);
        assert!(t.take_shrunk());
        t.scroll_display(100);
        t.resize(20, 1);
        assert_eq!(t.scrollback_len(), limit.min(4));
        assert_eq!(t.history, 4);
        assert_eq!(t.first_line(), 4 - limit.min(4) as u64);
        assert_eq!(t.display_offset(), limit.min(4));
        t.scroll_display(100);
        assert_eq!(t.display_offset(), limit.min(4));
        assert_eq!(
            t.viewport_row(0).text(),
            match limit {
                0 => "four",
                2 => "two",
                _ => "zero",
            }
        );
    }
}

#[test]
fn lowering_scrollback_limit_signals_shrink() {
    let mut t = filled_history(50);
    let before = t.scrollback_len();
    t.set_scrollback_limit(10);
    assert!(t.take_shrunk());
    assert_eq!(t.scrollback_len(), 10);
    t.scroll_display(100);
    assert_eq!(t.viewport_row(0).text(), "line 36");
    t.set_scrollback_limit(before);
    assert!(!t.take_shrunk());
    assert_eq!(t.scrollback_len(), 10);
}

fn long_command(output_lines: usize) -> String {
    let mut input = String::from("\x1b]133;A\x07$ \x1b]133;B\x07cargo test\r\n\x1b]133;C\x07");
    for i in 0..output_lines {
        input.push_str(&format!("line {i}\r\n"));
    }
    input.push_str("\x1b]133;D;101\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    input
}

fn pinned_text(t: &Terminal, row: isize) -> Option<String> {
    t.pinned_command(row)
        .map(|p| p.command.cells.iter().map(|c| c.c).collect())
}

#[test]
fn pinned_command_needs_shell_integration_and_scrollback() {
    let mut t = term(20, 5, &"plain output\r\n".repeat(30));
    t.scroll_display(10);
    assert!(t.pinned_command(0).is_none());

    let mut t = term(20, 5, &long_command(20));
    assert!(
        t.pinned_command(0).is_none(),
        "nothing pinned at the bottom"
    );
    t.scroll_display(10);
    let pinned = t.pinned_command(0).unwrap();
    assert_eq!(pinned_text(&t, 0).as_deref(), Some("$ cargo test"));
    assert_eq!(pinned.command.input_col, 2);
    assert_eq!(pinned.command.exit_code, Some(101));
    assert!(pinned.command.duration().is_some());
    assert!(!pinned.command.continued);
    assert_eq!(pinned.next_prompt_row, None);

    t.advance(b"\x1b[?1049h");
    assert!(
        t.pinned_command(0).is_none(),
        "no header on the alternate screen"
    );
}

#[test]
fn pinned_command_hides_when_its_prompt_is_visible() {
    let mut t = term(20, 5, &long_command(20));
    t.scroll_display(17);
    assert_eq!(t.viewport_row(0).text().trim_end(), "$ cargo test");
    assert!(t.pinned_command(0).is_none());
    t.scroll_display(-1);
    assert!(t.pinned_command(0).is_some());
    // A partial row above the top keeps the prompt in view.
    assert!(t.pinned_command(-1).is_none());
}

#[test]
fn next_prompt_pushes_the_pinned_command_off() {
    let mut t = term(20, 5, &long_command(20));
    t.advance(b"ls\r\n\x1b]133;C\x07a\r\nb\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    t.advance(&b"\r\nz".repeat(10));
    // Lines: prompt 0, output 1..=20, `ls` prompt 21, its output 22..=23, prompt 24.
    t.scroll_display(t.scrollback_len() as isize);
    t.scroll_display(-20);
    assert_eq!(t.viewport_row(0).text().trim_end(), "line 19");
    let pinned = t.pinned_command(0).unwrap();
    assert_eq!(pinned.command.exit_code, Some(101));
    assert_eq!(pinned.next_prompt_row, Some(1));
    t.scroll_display(-1);
    assert!(
        t.pinned_command(0).is_none(),
        "the next prompt reached the top"
    );
    t.scroll_display(-1);
    let pinned = t.pinned_command(0).unwrap();
    assert_eq!(pinned_text(&t, 0).as_deref(), Some("$ ls"));
    assert_eq!(pinned.command.exit_code, Some(0));
    assert_eq!(pinned.next_prompt_row, Some(2));
}

#[test]
fn empty_prompt_ends_the_previous_command() {
    let mut input = long_command(20);
    input.push_str("\r\n\x1b]133;A\x07$ \x1b]133;B\x07\r\n");
    for i in 0..10 {
        input.push_str(&format!("noise {i}\r\n"));
    }
    let mut t = term(20, 5, &input);
    t.scroll_display(t.scrollback_len() as isize - 22);
    assert_eq!(t.viewport_row(0).text().trim_end(), "$");
    assert!(t.pinned_command(0).is_none());
    t.scroll_display(1);
    assert!(
        t.pinned_command(0).is_none(),
        "the empty prompt is not a command"
    );
    t.scroll_display(1);
    assert!(t.pinned_command(0).is_some());
}

#[test]
fn running_command_and_prompt_without_input_mark() {
    let mut input = String::from("\x1b]133;A\x07$ make\r\n\x1b]133;C\x07");
    input.push_str(&"out\r\n".repeat(20));
    let mut t = term(20, 5, &input);
    t.scroll_display(5);
    let pinned = t.pinned_command(0).unwrap();
    assert_eq!(pinned.command.input_col, 0);
    assert_eq!(pinned.command.finished, None);
    assert_eq!(pinned.command.duration(), None);
    assert_eq!(pinned.command.exit_code, None);
}

#[test]
fn pinned_command_snapshot_survives_prompt_eviction() {
    let mut t = Terminal::new(20, 5);
    t.set_scrollback_limit(10);
    t.advance(long_command(40).as_bytes());
    t.scroll_display(10);
    assert!(t.viewport_row(0).text().starts_with("line"));
    assert_eq!(pinned_text(&t, 0).as_deref(), Some("$ cargo test"));
    let prompt = t.pinned_command(0).unwrap().command.prompt_line;
    t.scroll_to_line(prompt);
    assert_eq!(
        t.display_offset(),
        10,
        "evicted prompt jumps to the oldest line"
    );
}

#[test]
fn scroll_to_line_jumps_to_the_prompt() {
    let mut t = term(20, 5, &long_command(20));
    t.scroll_display(5);
    let prompt = t.pinned_command(0).unwrap().command.prompt_line;
    t.scroll_to_line(prompt);
    assert_eq!(t.viewport_row(0).text().trim_end(), "$ cargo test");
    assert!(t.pinned_command(0).is_none());
    t.scroll_to_line(u64::MAX);
    assert_eq!(t.display_offset(), 0);
}

#[test]
fn wrapped_command_is_marked_continued() {
    let mut input = String::from("\x1b]133;A\x07$ \x1b]133;B\x07");
    input.push_str(&"x".repeat(25));
    input.push_str("\r\n\x1b]133;C\x07");
    input.push_str(&"out\r\n".repeat(20));
    let mut t = term(20, 5, &input);
    t.scroll_display(5);
    let pinned = t.pinned_command(0).unwrap();
    assert!(pinned.command.continued);
    assert_eq!(pinned.command.cells.len(), 20);
}

#[test]
fn reset_forgets_commands() {
    let mut t = term(20, 5, &long_command(20));
    t.advance(b"\x1bc");
    t.advance(&b"x\r\n".repeat(20));
    t.scroll_display(5);
    assert!(t.pinned_command(0).is_none());
}

#[test]
fn inserted_lines_move_the_prompt_being_typed() {
    let mut t = term(20, 5, &long_command(20));
    // What drop_files::insert_preview writes: room above the prompt, an image, cursor back.
    t.advance(b"\x1b[3S\x1b[2;1H\x1b[3Limg\r\nimg\r\nimg\x1b[5;3H");
    t.advance(b"ls\r\n\x1b]133;C\x07");
    t.advance(&b"out\r\n".repeat(20));
    t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    t.scroll_display(5);
    assert_eq!(pinned_text(&t, 0).as_deref(), Some("$ ls"));
}

#[test]
fn clear_drops_the_commands_it_overwrites() {
    let mut input = long_command(3);
    input.push_str("clear\r\n\x1b]133;C\x07\x1b[3J\x1b[H\x1b[2J\x1b]133;D;0\x07");
    input.push_str("\x1b]133;A\x07$ \x1b]133;B\x07seq\r\n\x1b]133;C\x07");
    for i in 0..20 {
        input.push_str(&format!("{i}\r\n"));
    }
    input.push_str("\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    let mut t = term(20, 5, &input);
    for offset in 1..=t.scrollback_len() {
        t.scroll_display(-(t.display_offset() as isize) + offset as isize);
        if let Some(text) = pinned_text(&t, 0) {
            assert_eq!(text, "$ seq", "offset {offset}");
        }
    }
    t.scroll_display(5 - t.display_offset() as isize);
    assert_eq!(pinned_text(&t, 0).as_deref(), Some("$ seq"));
}

#[test]
fn inserted_lines_drop_commands_pushed_out_of_the_region() {
    let mut t = term(20, 8, &"x\r\n".repeat(20));
    t.advance(b"\x1b[4;1H\x1b]133;A\x07$ \x1b]133;B\x07one\r\n\x1b]133;C\x07out\x1b]133;D;0\x07");
    t.advance(b"\x1b[7;1H\x1b]133;A\x07$ \x1b]133;B\x07two\r\n\x1b]133;C\x07out\x1b]133;D;0\x07");
    t.advance(b"\x1b[1;5r\x1b[1;1H\x1b[4L\x1b[r");
    let commands = &t.shell.commands;
    assert!(
        commands
            .iter()
            .zip(commands.iter().skip(1))
            .all(|(a, b)| a.input_line < b.prompt_line),
        "commands stay ordered"
    );
    let two = commands.back().unwrap();
    assert_eq!(two.cells.iter().map(|c| c.c).collect::<String>(), "$ two");
}

fn primary_rows(t: &Terminal) -> Vec<(String, Vec<Cell>, bool)> {
    (t.first_line()..t.history + t.rows() as u64)
        .map(|line| {
            let row = t.padded_line(line);
            (row.text(), row.cells.clone(), row.wrapped())
        })
        .collect()
}

#[test]
fn reflow_matches_printing_at_the_new_width() {
    let inputs = [
        "0123456789abcdefghij\r\nshort\r\n\x1b[1;31mstyled text that wraps around\x1b[m",
        "ab\u{754c}cd\u{4f60}\u{597d}efg\r\n\u{754c}\u{754c}\u{754c}\u{754c}\u{754c}x",
        "\x1b]8;;https://example.com\x07a link that wraps\x1b]8;;\x07 after\r\n\r\nend",
        "exactly8",
        "a\u{301}bcde\u{301}fghij",
    ];
    for input in inputs {
        for from in 2..=12 {
            let mut t = Terminal::new(from, 4);
            t.advance(input.as_bytes());
            for to in (2..=12).chain([7, 3, 11, 2]) {
                t.resize(to, 4);
                let mut fresh = Terminal::new(to, 4);
                fresh.advance(input.as_bytes());
                let context = format!("{input:?} from {from} to {to}");
                let (rows, expected) = (primary_rows(&t), primary_rows(&fresh));
                for (i, (row, expected)) in rows.iter().zip(&expected).enumerate() {
                    assert_eq!(row, expected, "{context}: row {i}");
                }
                assert_eq!(rows.len(), expected.len(), "{context}");
                assert_eq!(t.cursor_position(), fresh.cursor_position(), "{context}");
                assert_eq!(
                    t.cursor.pending_wrap, fresh.cursor.pending_wrap,
                    "{context}"
                );
                assert_eq!(t.history, fresh.history, "{context}");
            }
        }
    }
}

#[test]
fn reflow_keeps_the_cursor_on_its_cell() {
    // On a wrapped line, the cursor stays on the character it was on.
    let mut t = term(10, 3, "abcdefghijkl\x1b[1;4H");
    t.resize(4, 3);
    assert_eq!(screen(&t), ["abcd", "efgh", "ijkl"]);
    assert_eq!(t.cursor_position(), Some((0, 3)));
    t.advance(b"X");
    t.resize(12, 3);
    assert_eq!(screen(&t), ["abcXefghijkl", "", ""]);
    assert_eq!(t.cursor_position(), Some((0, 4)));

    // Past the end of the text it keeps the blanks before it.
    let mut t = term(10, 3, "abc\x1b[1;7H");
    t.resize(4, 3);
    assert_eq!(t.cursor_position(), Some((1, 2)));
    t.advance(b"X");
    t.resize(10, 3);
    assert_eq!(screen(&t), ["abc   X", "", ""]);

    // A pending wrap survives at the end of a full row.
    let mut t = term(8, 2, "abcdefgh");
    t.resize(4, 2);
    assert!(t.cursor.pending_wrap);
    t.advance(b"i");
    assert_eq!(screen(&t), ["efgh", "i"]);
    assert!(t.viewport_row(0).wrapped());
}

#[test]
fn reflow_lets_bash_redraw_a_wrapped_command_line() {
    // Recorded from bash 5 with a coloured prompt: a wrapping command, the cursor moved
    // back into it, the window narrowed step by step, text inserted, widened, Enter.
    // On each resize readline moves up as many rows as its cursor was below the
    // prompt at the old width, then redraws the whole command.
    let prompt = "\x1b[01;32muser@host\x1b[00m:\x1b[01;34m~\x1b[00m$ ";
    let command =
        "echo word00 word01 word02 word03 word04 word05 word06 word07 word08 word09 word10 word11";
    let inserted = command.replace("word07", "wordQQ07");
    let right = |n| "\x1b[C".repeat(n);
    let redraw = |up: usize, text: &str, tail: String| {
        format!("\r\n\r\r\x1b[K{}{prompt}{text}{tail}", "\x1b[A".repeat(up))
    };
    let steps = [
        (None, format!("\x1b[?2004h{prompt}{command}")),
        (
            None,
            format!(
                "{}\r\x1b[A{}{}",
                "\x08".repeat(20),
                right(40),
                "\x08".repeat(8)
            ),
        ),
        (Some(36), redraw(2, command, format!("\x1b[A{}", right(6)))),
        (Some(32), redraw(2, command, format!("\x1b[A{}", right(2)))),
        (Some(28), redraw(3, command, "\x1b[A\x08\x08".into())),
        (
            Some(24),
            redraw(3, command, format!("\x1b[A\x1b[A{}", right(18))),
        ),
        (
            None,
            "QQ07 word08 word09 word10 word11\x1b[A\r\x1b[C".into(),
        ),
        (
            Some(40),
            redraw(4, &inserted, format!("\x1b[A{}", right(10))),
        ),
    ];
    let mut t = Terminal::new(40, 10);
    for (cols, output) in steps {
        if let Some(cols) = cols {
            t.resize_while_editing(cols, 10, &mut []);
        }
        t.advance(output.as_bytes());
        let screen = screen(&t);
        let joined: String = screen.concat();
        assert_eq!(joined.matches("user@host").count(), 1, "{screen:#?}");
        assert_eq!(joined.matches("word11").count(), 1, "{screen:#?}");
    }
    t.advance(
        format!(
            "\r\n\r\r\n\x1b[?2004l\r{}\r\n\x1b[?2004h{prompt}",
            &inserted[5..]
        )
        .as_bytes(),
    );
    assert_eq!(
        screen(&t),
        [
            "user@host:~$ echo word00 word01 word02 w",
            "ord03 word04 word05 word06 wordQQ07 word",
            "08 word09 word10 word11",
            "word00 word01 word02 word03 word04 word0",
            "5 word06 wordQQ07 word08 word09 word10 w",
            "ord11",
            "user@host:~$",
            "",
            "",
            "",
        ]
    );
}

#[test]
fn reflow_moves_a_cursor_before_text_with_its_cell_without_a_line_editor() {
    // Text after the cursor alone does not mean a line editor will redraw the line.
    let mut t = term(8, 4, "abcdefghijkl\x1b[2;3H");
    t.resize(4, 4);
    t.advance(b"X");
    assert_eq!(screen(&t), ["abcd", "efgh", "ijXl", ""]);
}

#[test]
fn reflow_keeps_the_row_of_a_cursor_in_shell_input() {
    // At the end of a wrapped command in the shell's input zone, the cursor keeps its
    // row in the line, where the line editor will redraw from.
    let input = "\x1b]133;A\x07$ \x1b]133;B\x070123456789abc";
    let mut t = term(8, 4, input);
    assert_eq!(t.cursor_position(), Some((1, 7)));
    t.resize(16, 4);
    assert_eq!(screen(&t), ["$ 0123456789abc", "", "", ""]);
    assert_eq!(t.cursor_position(), Some((1, 7)));

    // Without the shell's marks it stays after the text.
    let mut t = term(8, 4, &input.replace("\x1b]133;B\x07", ""));
    t.resize(16, 4);
    assert_eq!(t.cursor_position(), Some((0, 15)));
}

#[test]
fn widening_keeps_history_allocations_and_projects_blank_columns() {
    let mut t = Terminal::new(8, 2);
    t.advance(b"\x1b[41mheader  \x1b[m\r\n");
    for _ in 0..64 {
        t.advance(b"short\r\n");
    }
    let allocations = |t: &Terminal| {
        t.primary
            .scrollback
            .iter()
            .map(|r| (r.cells.as_ptr(), r.cells.capacity()))
            .collect::<Vec<_>>()
    };
    let before = allocations(&t);
    t.scroll_display(200);
    let mut points = [Point { line: 1, col: 4 }];
    t.resize_tracking(16, 2, &mut points);
    assert_eq!(points[0], Point { line: 1, col: 4 });
    assert_eq!(t.viewport_row(0).cells[7].bg, Color::Indexed(1));
    assert_eq!(t.viewport_row(0).cells[15], Cell::default());
    assert!(t.link_at(t.viewport_to_point(0, 15)).is_none());
    t.start_selection(t.viewport_to_point(0, 12), SelectionKind::Block);
    t.update_selection(t.viewport_to_point(1, 15));
    assert_eq!(t.selection_text().as_deref(), Some("    \n    "));
    assert!(t.is_selected(t.selection_range(), t.viewport_to_point(1, 15)));
    t.resize(8, 2);
    t.resize(24, 2);
    assert_eq!(allocations(&t), before);

    // A height-only resize pulls a short row into the writable grid.
    t.scroll_to_bottom();
    t.resize(24, 3);
    assert_eq!(t.primary.lines[0].cells.len(), 24);
    t.advance(b"\x1b[1;24HX");
    assert_eq!(t.viewport_row(0).cells[23].c, 'X');
    assert_eq!(t.primary.scrollback[0].cells.len(), 8);
}

#[test]
fn widening_leaves_hard_rows_short_after_a_rewrapped_line() {
    let mut t = term(5, 2, "short\r\nabcdefghij\r\nx\r\ny\r\nz\r\n");
    t.resize(9, 2);
    let rows: Vec<_> = t
        .primary
        .scrollback
        .iter()
        .map(|r| (r.text(), r.cells.len()))
        .collect();
    assert_eq!(
        rows,
        [
            ("short".into(), 5),
            ("abcdefghi".into(), 9),
            ("j".into(), 1),
            ("x".into(), 5),
            ("y".into(), 5)
        ]
    );
}

#[test]
fn reflow_moves_lines_through_scrollback() {
    let mut t = term(8, 2, "aaaabbbbccccdddd\r\n$ ");
    assert_eq!(screen(&t), ["ccccdddd", "$"]);
    assert_eq!(t.scrollback_len(), 1);
    t.resize(16, 2);
    assert_eq!(screen(&t), ["aaaabbbbccccdddd", "$"]);
    assert_eq!(t.scrollback_len(), 0);
    t.resize(4, 2);
    assert_eq!(screen(&t), ["dddd", "$"]);
    assert_eq!(t.scrollback_len(), 3);
    assert_eq!(t.cursor_position(), Some((1, 2)));
    t.scroll_display(3);
    assert_eq!(screen(&t), ["aaaa", "bbbb"]);

    // Rows past the scrollback limit are dropped.
    let mut t = term(8, 2, "aaaabbbbccccdddd\r\n$ ");
    t.set_scrollback_limit(2);
    t.resize(4, 2);
    assert_eq!(t.scrollback_len(), 2);
    t.scroll_display(2);
    assert_eq!(screen(&t), ["bbbb", "cccc"]);
}

#[test]
fn reflow_keeps_the_screen_top_while_the_cursor_fits() {
    // Rows below the cursor take the new rows before anything scrolls off.
    let mut t = term(8, 4, "0123456789\r\n$ ");
    t.resize(4, 4);
    assert_eq!(screen(&t), ["0123", "4567", "89", "$"]);
    assert_eq!(t.scrollback_len(), 0);
    t.resize(8, 4);
    assert_eq!(screen(&t), ["01234567", "89", "$", ""]);
    assert_eq!(t.cursor_position(), Some((2, 2)));

    // Text below the cursor stays on screen while the cursor does.
    let mut t = term(8, 3, "top\r\nmiddle\r\n0123456789\x1b[2;1H");
    assert_eq!(screen(&t), ["middle", "01234567", "89"]);
    t.resize(4, 3);
    assert_eq!(screen(&t), ["0123", "4567", "89"]);
    assert_eq!(t.cursor_position(), Some((0, 0)));

    // Rows added in the same resize take the text below the cursor.
    let mut t = term(8, 2, "x\r\nabcdefgh\x1b[1;1H");
    t.resize(4, 3);
    assert_eq!(screen(&t), ["x", "abcd", "efgh"]);
    assert_eq!(t.cursor_position(), Some((0, 0)));
}

#[test]
fn reflow_restores_the_saved_cursor() {
    let mut t = term(10, 3, "0123456789abc\x1b7\r\n");
    t.resize(5, 3);
    assert_eq!(screen(&t), ["56789", "abc", ""]);
    t.advance(b"\x1b8X");
    assert_eq!(screen(&t), ["56789", "abcX", ""]);

    // A cursor saved while waiting to wrap still waits after the resize.
    let mut t = term(4, 3, "abcd\x1b7\r\n");
    t.resize(8, 3);
    t.advance(b"\x1b8X");
    assert_eq!(screen(&t), ["abcdX", "", ""]);
    let mut t = term(8, 3, "abcdefgh\x1b7\r\n");
    t.resize(4, 3);
    t.advance(b"\x1b8X");
    assert_eq!(screen(&t), ["abcd", "efgh", "X"]);
}

#[test]
fn reflow_leaves_the_alternate_screen_cut() {
    let mut t = term(10, 3, "0123456789abc\x1b[?1049h\x1b[Hfullscreen");
    t.resize(5, 3);
    assert_eq!(screen(&t), ["fulls", "", ""]);
    t.resize(10, 3);
    assert_eq!(screen(&t), ["fullscreen", "", ""]);
    t.resize(4, 3);
    t.advance(b"\x1b[?1049l!");
    assert_eq!(screen(&t), ["4567", "89ab", "c!"]);
    assert_eq!(t.scrollback_len(), 1);
    assert_eq!(t.cursor_position(), Some((2, 2)));
    t.resize(13, 3);
    assert_eq!(screen(&t), ["0123456789abc", "!", ""]);
}

/// Bash's prompt and a half-typed command of 146 chars: two rows at 100 columns.
fn half_typed_command() -> String {
    format!(
        "bash$ printf '%s\\n' 'BEGIN-{}-END'",
        "0123456789".repeat(12)
    )
}

#[test]
fn resize_while_editing_lets_the_editor_redraw_its_line_once() {
    let line = half_typed_command();
    assert_eq!(line.len(), 152);
    // What bash 5.1 and zsh 5.8 send after SIGWINCH: up to the line's first row, redraw.
    let redraws = [
        format!("\r\x1b[K\x1b[A{line}"),
        format!("\x1b[A\r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J{line}\x1b[K"),
    ];
    for redraw in redraws {
        let mut t = term(100, 8, &format!("out\r\n{line}"));
        t.resize_while_editing(60, 8, &mut []);
        // The line rewraps, and the cursor keeps its row in it, where the editor moves up from.
        let mut want = vec!["out", &line[..60], &line[60..120], &line[120..]];
        want.resize(8, "");
        assert_eq!(screen(&t), want);
        assert_eq!((t.cursor.row, t.cursor.col), (2, 52));
        t.advance(redraw.as_bytes());
        assert_eq!(screen(&t), want);
        // Run, then widen: the command rewraps whole, nothing cut from it comes back.
        t.advance(b"\r\nran\r\n");
        t.resize(100, 8);
        let mut want = vec!["out", &line[..100], &line[100..], "ran"];
        want.resize(8, "");
        assert_eq!(screen(&t), want);
    }
}

#[test]
fn resize_while_editing_keeps_text_no_editor_redraws() {
    // Output with no newline before the prompt, or a silent `read`: nothing redraws it.
    let out = format!("{}{}", "0123456789".repeat(15), "bash$ half");
    let mut t = term(100, 6, &format!("top\r\n{out}"));
    let before = primary_rows(&t);
    t.resize_while_editing(60, 6, &mut []);
    assert_eq!(screen(&t), screen(&term(60, 6, &format!("top\r\n{out}"))));
    t.resize_while_editing(100, 6, &mut []);
    assert_eq!(primary_rows(&t), before);
    t.resize_while_editing(60, 6, &mut []);
    t.resize(40, 6);
    assert_eq!(screen(&t), screen(&term(40, 6, &format!("top\r\n{out}"))));
}

#[test]
fn resize_rewraps_the_cursor_line_without_an_editor() {
    let line = half_typed_command();
    let mut t = term(100, 8, &format!("out\r\n{line}"));
    t.resize(60, 8);
    let mut want = vec!["out", &line[..60], &line[60..120], &line[120..]];
    want.resize(8, "");
    assert_eq!(screen(&t), want);
}

#[test]
fn resize_while_editing_rewraps_the_lines_above() {
    let line = half_typed_command();
    let long = "x".repeat(150);
    let mut t = term(100, 8, &format!("{long}\r\n{line}"));
    t.resize_while_editing(60, 8, &mut []);
    let mut want = vec![
        &long[..60],
        &long[60..120],
        &long[120..],
        &line[..60],
        &line[60..120],
        &line[120..],
    ];
    want.resize(8, "");
    assert_eq!(screen(&t), want);
}

#[test]
fn resize_while_editing_lets_bash_3_redraw_a_line_edited_mid_line() {
    // bash 3.2 (readline 5.2) after SIGWINCH: CR, EL on the cursor's row, up as many rows
    // as the cursor was below the line's start at the old width, then the whole line, with
    // nothing erased below it; then it moves the cursor back to its place.
    let words: Vec<String> = (0..26).map(|n| format!("word{n:02}")).collect();
    let line = format!("$ {}", words.join(" "));
    assert_eq!(line.len(), 183);
    let place = |at: usize, cols: usize| {
        let (end_row, row) = ((line.len()) / cols, at / cols);
        format!(
            "{}\r{}",
            "\x1b[A".repeat(end_row - row),
            "\x1b[C".repeat(at % cols)
        )
    };
    for back in [0, 30] {
        let at = line.len() - back;
        let mut t = term(100, 10, &format!("out\r\n{line}{}", place(at, 100)));
        let mut cols = 100;
        for _ in 0..2 {
            for new_cols in [60, 100] {
                let up = at / cols;
                t.resize_while_editing(new_cols, 10, &mut []);
                cols = new_cols;
                t.advance(
                    format!("\r\x1b[K{}{line}{}", "\x1b[A".repeat(up), place(at, cols)).as_bytes(),
                );
                let mut want = vec!["out".to_string()];
                want.extend(
                    line.as_bytes()
                        .chunks(cols)
                        .map(|c| String::from_utf8_lossy(c).trim_end().to_string()),
                );
                want.resize(10, String::new());
                assert_eq!(screen(&t), want, "{back} back, {cols} cols");
                assert_eq!(
                    t.cursor_position(),
                    Some((1 + at / cols, at % cols)),
                    "{back} back, {cols} cols"
                );
            }
        }
    }
}

#[test]
fn resize_while_editing_back_and_forth_leaves_no_stale_tail() {
    let line = format!("$ echo {} CMDMATCH {}", "a".repeat(165), "p".repeat(60));
    let output: String = (1..=26).map(|i| format!("output line {i}\r\n")).collect();
    // What bash 5.1 and zsh 5.8 send after SIGWINCH, moving up the rows the cursor was
    // below the line's start at the old width.
    let redraws: [fn(usize, &str) -> String; 2] = [
        |up, line| format!("\r\x1b[K{}{line}", "\x1b[A".repeat(up)),
        |up, line| {
            format!(
                "{}\r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J{line}\x1b[K",
                "\x1b[A".repeat(up)
            )
        },
    ];
    for redraw in redraws {
        let mut t = term(100, 30, &format!("{output}{line}"));
        for (cols, rows, up) in [(60, 20, 2), (100, 30, 4), (60, 20, 2), (100, 30, 4)] {
            t.resize_while_editing(cols, rows, &mut []);
            t.advance(redraw(up, &line).as_bytes());
            let shown = screen(&t);
            let top = shown.iter().position(|r| r.starts_with('$')).unwrap();
            let chunks: Vec<_> = line.as_bytes().chunks(cols).collect();
            assert_eq!(shown[top - 1], "output line 26", "{cols}x{rows}");
            for (row, chunk) in shown[top..].iter().zip(&chunks) {
                assert_eq!(row, std::str::from_utf8(chunk).unwrap(), "{cols}x{rows}");
            }
            assert!(
                shown[top + chunks.len()..].iter().all(String::is_empty),
                "{cols}x{rows}: {shown:?}"
            );
            assert_eq!(
                (t.cursor.row, t.cursor.col),
                (top + chunks.len() - 1, 242 % cols)
            );
        }
    }
}

#[test]
fn resize_while_editing_mid_line_leaves_no_stale_tail() {
    let words: Vec<String> = (1..=46).map(|i| format!("w{i:03}")).collect();
    let line = format!(
        "\x1b[1;32mdevin@qa-mac\x1b[0m:~/projects/terminal (main)$ echo {} CMDMATCH {}",
        words[..23].join(" "),
        words[23..].join(" ")
    );
    let shown_line = line.replace("\x1b[1;32m", "").replace("\x1b[0m", "");
    let len = shown_line.len();
    let point = len - 30;
    let output: String = (1..=40).map(|i| format!("output-line-{i}\r\n")).collect();
    // Readline 5.2 (bash 3.2) after SIGWINCH: clear the cursor's row, move up the rows the
    // cursor was below the line's start, redraw the line, then move back to the point.
    let place = |cols: usize| {
        let up = (len - 1) / cols - point / cols;
        format!("{}\r\x1b[{}C", "\x1b[A".repeat(up), point % cols)
    };
    let mut t = term(100, 30, &format!("{output}{line}{}", place(100)));
    let mut old = 100;
    for (cols, rows) in [
        (60, 30),
        (40, 30),
        (140, 30),
        (100, 30),
        (60, 20),
        (100, 30),
    ] {
        t.resize_while_editing(cols, rows, &mut []);
        let redraw = format!(
            "\r\x1b[K{}{line}{}",
            "\x1b[A".repeat(point / old),
            place(cols)
        );
        t.advance(redraw.as_bytes());
        old = cols;
        let shown = screen(&t);
        let top = shown.iter().position(|r| r.starts_with("devin@")).unwrap();
        let chunks: Vec<_> = shown_line.as_bytes().chunks(cols).collect();
        assert_eq!(shown[top - 1], "output-line-40", "{cols}x{rows}");
        for (row, chunk) in shown[top..].iter().zip(&chunks) {
            assert_eq!(
                row,
                std::str::from_utf8(chunk).unwrap().trim_end(),
                "{cols}x{rows}"
            );
        }
        assert!(
            shown[top + chunks.len()..].iter().all(String::is_empty),
            "{cols}x{rows}: {shown:?}"
        );
        assert_eq!(
            (t.cursor.row, t.cursor.col),
            (top + point / cols, point % cols),
            "{cols}x{rows}"
        );
    }
}

fn run(cmd: &str, output: &[&str]) -> String {
    let mut s = format!("\x1b]133;A\x07$ \x1b]133;B\x07{cmd}\r\n\x1b]133;C\x07");
    for line in output {
        s.push_str(line);
        s.push_str("\r\n");
    }
    s.push_str("\x1b]133;D;0\x07");
    s
}

fn marked_rows(t: &Terminal) -> Vec<(String, u8)> {
    let mut marks = Vec::new();
    t.rerun_marks(0, &mut marks);
    (0..t.rows())
        .filter(|&r| marks[r] != 0)
        .map(|r| (t.viewport_row(r).text(), marks[r]))
        .collect()
}

#[test]
fn rerun_marks_lines_that_differ_from_the_last_run() {
    let mut input = run("make", &["build a", "took 1.2s", "ok"]);
    input.push_str(&run("ls", &["x"]));
    input.push_str(&run("make", &["build a", "warning: b", "took 1.4s", "ok"]));
    input.push_str("\x1b]133;A\x07$ \x1b]133;B\x07");
    let t = term(20, 20, &input);
    assert_eq!(
        marked_rows(&t),
        [("warning: b".to_owned(), crate::ROW_CHANGED)]
    );
}

fn cargo_test(results: &[&str], secs: &str, pid: u32) -> Vec<String> {
    let mut out = vec![
        format!("   Compiling app v0.1.0 at 2026-11-15T07:02:{secs}Z"),
        format!("    Finished `test` profile in {secs}.5s"),
        format!("     Running /tmp/cargo-{pid}/deps/app-1a2b (pid {pid})"),
    ];
    out.extend(results.iter().map(|r| format!("test {r}")));
    out.push(format!("test result: ok. finished in 0.{secs}s"));
    out
}

#[test]
fn rerun_ignores_timing_but_marks_a_changed_test_result() {
    let rerun = |a: Vec<String>, b: Vec<String>, ignore: &crate::Ignore| {
        let mut t = Terminal::new(80, 20);
        t.set_compare_ignore(ignore);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        let b: Vec<&str> = b.iter().map(String::as_str).collect();
        t.advance(run("cargo test", &a).as_bytes());
        t.advance(run("cargo test", &b).as_bytes());
        t.advance(b"\x1b]133;A\x07$ ");
        marked_rows(&t)
    };
    let defaults = crate::Ignore::default();
    let first = cargo_test(&["a ... ok", "b ... ok"], "11", 4242);
    let timing_only = cargo_test(&["a ... ok", "b ... ok"], "37", 5151);
    assert!(rerun(first.clone(), timing_only.clone(), &defaults).is_empty());
    let failed = cargo_test(&["a ... ok", "b ... FAILED"], "37", 5151);
    assert_eq!(
        rerun(first.clone(), failed, &defaults),
        [("test b ... FAILED".to_owned(), crate::ROW_CHANGED)]
    );
    // With no patterns, every volatile line lights up.
    let none = crate::Ignore::new::<&str>(&[]).unwrap();
    assert_eq!(rerun(first, timing_only, &none).len(), 4);
}

#[test]
fn rerun_compares_combining_marks_and_skips_wrapped_commands() {
    let mut t = Terminal::new(40, 20);
    t.advance(run("echo", &["cafe\u{301} ok"]).as_bytes());
    t.advance(run("echo", &["cafe\u{300} ok"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(marked_rows(&t).len(), 1);
    // Only the first row of a wrapped command is kept, so its runs aren't compared.
    let long = "x".repeat(50);
    let mut t = Terminal::new(40, 20);
    t.advance(run(&long, &["a"]).as_bytes());
    t.advance(run(&long, &["b"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert!(marked_rows(&t).is_empty());
}

#[test]
fn rerun_skips_trailing_blanks_however_they_were_written() {
    let mut t = Terminal::new(10, 20);
    t.advance(
        run(
            "cat",
            &["ok", "0123456789", "x\x1b[41m\x1b[K\x1b[m", "abc\r   "],
        )
        .as_bytes(),
    );
    t.advance(run("cat", &["ok   ", "0123456789", "x", ""]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert!(marked_rows(&t).is_empty());
    t.advance(run("cat", &["ok", "012345678x", "x", "y"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    let marked: Vec<String> = marked_rows(&t).into_iter().map(|(text, _)| text).collect();
    assert_eq!(marked, ["012345678x", "y"]);
}

#[test]
fn reset_keeps_compare_settings() {
    let mut t = Terminal::new(40, 20);
    t.set_compare_runs(false);
    t.advance(b"\x1bc");
    assert!(!t.compare_runs());
    let builds = crate::Ignore::new(&[r"#\d+"]).unwrap();
    t.set_compare_runs(true);
    t.set_compare_ignore(&builds);
    t.advance(b"\x1bc");
    t.advance(run("make", &["build #1"]).as_bytes());
    t.advance(run("make", &["build #2"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert!(marked_rows(&t).is_empty());
}

#[test]
fn new_ignore_patterns_forget_stored_runs_but_keep_marks() {
    let mut t = Terminal::new(40, 20);
    t.advance(run("make", &["build #1", "ok"]).as_bytes());
    t.advance(run("make", &["build #2", "ok"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(
        marked_rows(&t),
        [("build #2".to_owned(), crate::ROW_CHANGED)]
    );
    let builds = crate::Ignore::new(&[r"#\d+"]).unwrap();
    t.set_compare_ignore(&builds);
    assert_eq!(marked_rows(&t).len(), 1);
    // The run before the change isn't compared with the next one.
    t.advance(run("make", &["build #3", "failed"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(marked_rows(&t).len(), 1);
    t.advance(run("make", &["build #4", "failed"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(marked_rows(&t).len(), 1);
    // Setting the same patterns again keeps the stored runs.
    t.set_compare_ignore(&crate::Ignore::new(&[r"#\d+"]).unwrap());
    t.advance(run("make", &["build #5", "ok"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    let marked: Vec<String> = marked_rows(&t).into_iter().map(|(text, _)| text).collect();
    assert_eq!(marked, ["build #2", "ok"]);
}

#[test]
fn rerun_marks_removed_lines_and_ignores_first_runs() {
    let mut input = run("test", &["a", "b", "c"]);
    let t = term(20, 20, &format!("{input}\x1b]133;A\x07$ "));
    assert!(marked_rows(&t).is_empty());
    input.push_str(&run("test", &["a", "c"]));
    input.push_str(&run("test", &["a", "c"]));
    let t = term(20, 20, &format!("{input}\x1b]133;A\x07$ "));
    // The second run keeps its mark; the unchanged third run adds none.
    let removed = |text: &str| (text.to_owned(), crate::ROW_REMOVED_ABOVE);
    assert_eq!(marked_rows(&t), [removed("c")]);
    let mut t = term(20, 20, &input);
    t.advance(run("test", &["a"]).as_bytes());
    t.advance(b"\x1b]133;A\x07$ ");
    assert_eq!(marked_rows(&t), [removed("c"), removed("$")]);
}

#[test]
fn rerun_marks_follow_reflow_and_can_be_turned_off() {
    let mut input = run("cat", &["0123456789", "same"]);
    input.push_str(&run("cat", &["0123456789abcdefghij", "same"]));
    input.push_str("\x1b]133;A\x07$ ");
    let mut t = term(30, 10, &input);
    assert_eq!(
        marked_rows(&t),
        [("0123456789abcdefghij".to_owned(), crate::ROW_CHANGED)]
    );
    t.resize(10, 10);
    let marked: Vec<String> = marked_rows(&t).into_iter().map(|(text, _)| text).collect();
    assert_eq!(marked, ["0123456789", "abcdefghij"]);
    t.set_compare_runs(false);
    assert!(marked_rows(&t).is_empty());
}

fn click(t: &Terminal, row: usize, col: usize) -> Option<PromptClick> {
    t.prompt_click(t.viewport_to_point(row, col))
}

#[test]
fn prompt_click_moves_by_characters_within_the_input() {
    let mut t = term(20, 4, "out\r\n\x1b]133;A\x07$ \x1b]133;B\x07echo hello");
    assert_eq!(click(&t, 1, 7), Some(PromptClick::Move(-5)));
    assert_eq!(click(&t, 1, 12), Some(PromptClick::Move(0)));
    // Past the end of the text, and onto the prompt itself, clamp to the input.
    assert_eq!(click(&t, 1, 19), Some(PromptClick::Move(0)));
    assert_eq!(click(&t, 1, 0), Some(PromptClick::Move(-10)));
    // Rows outside the command line: earlier output, blank rows below.
    assert_eq!(click(&t, 0, 1), None);
    assert_eq!(click(&t, 2, 1), None);
    t.advance(b"\x1b[5D");
    assert_eq!(click(&t, 1, 19), Some(PromptClick::Move(5)));
    // Trailing typed spaces before the cursor stay reachable.
    t.advance(b"\x1b[5C  ");
    assert_eq!(click(&t, 1, 19), Some(PromptClick::Move(0)));
    assert_eq!(click(&t, 1, 2), Some(PromptClick::Move(-12)));
}

#[test]
fn prompt_click_counts_wide_characters_once_and_follows_soft_wraps() {
    // 10 columns: "$ " then "ab日本cdefg" wraps after "日本" + "cd".
    let t = term(10, 4, "\x1b]133;A\x07$ \x1b]133;B\x07ab日本cdefgh");
    assert_eq!(screen(&t)[0], "$ ab日本cd");
    assert_eq!(click(&t, 0, 4), Some(PromptClick::Move(-8)));
    // The right half of a wide character is that character.
    assert_eq!(click(&t, 0, 5), Some(PromptClick::Move(-8)));
    assert_eq!(click(&t, 0, 6), Some(PromptClick::Move(-7)));
    assert_eq!(click(&t, 1, 0), Some(PromptClick::Move(-4)));
    assert_eq!(click(&t, 1, 9), Some(PromptClick::Move(0)));
    // A wide character that does not fit leaves wrap padding, which is not a character.
    let t = term(6, 4, "\x1b]133;A\x07$ \x1b]133;B\x07abc日x");
    assert_eq!(click(&t, 0, 5), Some(PromptClick::Move(-2)));
    assert_eq!(click(&t, 1, 0), Some(PromptClick::Move(-2)));
    assert_eq!(click(&t, 1, 2), Some(PromptClick::Move(-1)));
}

#[test]
fn prompt_click_replays_native_readline_erased_margin_and_real_space_controls() {
    let fixtures: [(&[u8], bool, isize); 6] = [
        (
            include_bytes!("../tests/fixtures/readline/bash32-wide.bin"),
            true,
            -40,
        ),
        (
            include_bytes!("../tests/fixtures/readline/bash53-wide.bin"),
            true,
            -40,
        ),
        (
            include_bytes!("../tests/fixtures/readline/bash32-typed-space.bin"),
            false,
            -41,
        ),
        (
            include_bytes!("../tests/fixtures/readline/bash53-typed-space.bin"),
            false,
            -41,
        ),
        (
            include_bytes!("../tests/fixtures/readline/bash32-ascii.bin"),
            false,
            -41,
        ),
        (
            include_bytes!("../tests/fixtures/readline/bash53-ascii.bin"),
            false,
            -41,
        ),
    ];
    for (bytes, padding, first_row_delta) in fixtures {
        for scrollback in [0, 100] {
            for chunk in [1, 7, bytes.len()] {
                let mut t = Terminal::new(44, 34);
                t.set_scrollback_limit(scrollback);
                for bytes in bytes.chunks(chunk) {
                    t.advance(bytes);
                }
                assert_eq!(t.shell.input_start, Some((t.history, 2)));
                let wide = t.viewport_row(1).cells[0].c == '日';
                assert_eq!(t.cursor_position(), Some((1, if wide { 8 } else { 5 })));
                assert_eq!(click(&t, 0, 8), Some(PromptClick::Move(first_row_delta)));
                assert_eq!(
                    t.viewport_row(0).cells[43].flags.contains(Flags::WIDE_WRAP),
                    padding
                );
                if wide {
                    for (col, delta) in [(0, -5), (1, -5), (2, -4), (5, -3), (8, 0)] {
                        assert_eq!(click(&t, 1, col), Some(PromptClick::Move(delta)));
                    }
                    assert_eq!(
                        click(&t, 0, 43),
                        Some(PromptClick::Move(if padding { -5 } else { -6 }))
                    );
                }
            }
        }
    }
}

#[test]
fn prompt_click_erased_margin_requires_pending_wide_wrap_provenance() {
    let prefix = "\x1b]133;A\x07$ \x1b]133;B\x07abc";
    // In six columns, a printed real space at the margin remains editable.
    for (paint, padding, delta) in [
        (" 日x", false, -6),
        (" \x1b[K日x", true, -5),
        // An erased trailing real space still holds the editable end cursor.
        (" \x1b[K", false, -4),
        // Reprinting the erased space restores its occupancy.
        (" \x1b[K\r\x1b[5C 日x", false, -6),
        // Erasing a real margin space after cursor motion is not Readline's
        // pending-wrap placeholder, even with an otherwise identical grid.
        (" \r\x1b[5C\x1b[K\r\n日x", false, -6),
        // ASCII after the erased space cannot establish wide-wrap padding.
        (" \x1b[Kxyz", false, -7),
        // Erasing a non-space at the margin does not prove a placeholder.
        ("Z\x1b[K日x", false, -6),
    ] {
        for chunk in [1, 100] {
            let mut t = Terminal::new(6, 4);
            for bytes in format!("{prefix}{paint}").as_bytes().chunks(chunk) {
                t.advance(bytes);
            }
            assert_eq!(
                t.viewport_row(0).cells[5].flags.contains(Flags::WIDE_WRAP),
                padding,
                "{paint:?}"
            );
            // Explicit CR/LF is not a soft wrap; it must not be guessed across.
            let expected = if paint.contains('\n') {
                None
            } else {
                Some(PromptClick::Move(delta))
            };
            assert_eq!(click(&t, 0, 2), expected, "{paint:?}");
        }
    }
    // Erasing real spaces within the row keeps their cursor positions.
    let t = term(
        20,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07ab  日x  \x1b[1;5H\x1b[2X\x1b[1;12H",
    );
    assert_eq!(click(&t, 0, 2), Some(PromptClick::Move(-8)));
    assert_eq!(click(&t, 0, 5), Some(PromptClick::Move(-5)));
    assert_eq!(click(&t, 0, 19), Some(PromptClick::Move(0)));

    // Wide suggestions beyond the erased margin must not be accepted by Right.
    let t = term(6, 4, &format!("{prefix} \x1b[K\x1b[90m日x\x1b[m\x1b[1;6H"));
    assert_eq!(click(&t, 1, 5), Some(PromptClick::Move(0)));
    assert_eq!(click(&t, 0, 2), Some(PromptClick::Move(-3)));

    // A later real-space repaint replaces the padding marker and its meaning.
    let mut t = term(6, 4, &format!("{prefix} \x1b[K日x"));
    t.advance(b"\x1b[1;6H ");
    t.advance("日x".as_bytes());
    assert!(!t.viewport_row(0).cells[5].flags.contains(Flags::WIDE_WRAP));
    assert_eq!(click(&t, 0, 2), Some(PromptClick::Move(-6)));
}

#[test]
fn prompt_click_stops_before_a_greyed_autosuggestion() {
    // zsh-autosuggestions: typed `echo hel`, suggestion `lo world` in fg=8, cursor
    // moved back to the end of the typed text.
    let zsh = "\x1b]133;A\x07% \x1b]133;B\x07echo hel\x1b[90mlo world\x1b[39m\x1b[8D";
    let t = term(30, 4, zsh);
    assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(0)));
    assert_eq!(click(&t, 0, 14), Some(PromptClick::Move(0)));
    assert_eq!(click(&t, 0, 4), Some(PromptClick::Move(-6)));
    // fish: syntax-coloured input, suggestion in 256-colour or truecolor grey.
    for grey in ["38;5;240", "38;2;85;85;85", "2"] {
        let fish = format!(
            "\x1b]133;A\x07> \x1b]133;B\x07\x1b[38;5;26mecho\x1b[m \x1b[38;5;39mhel\
             \x1b[{grey}mlo world\x1b[m\x1b[8D"
        );
        let t = term(30, 4, &fish);
        assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(0)), "{grey}");
        assert_eq!(click(&t, 0, 12), Some(PromptClick::Move(0)), "{grey}");
    }
    // With the cursor moved left, clicks still reach the end of the real input only.
    let mut t = term(30, 4, zsh);
    t.advance(b"\x1b[3D");
    assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(3)));
    // Coloured text that is not grey, or grey text before more input, is typed input.
    let t = term(
        30,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07echo \x1b[33m'hi'\x1b[m\x1b[4D",
    );
    assert_eq!(click(&t, 0, 20), Some(PromptClick::Move(4)));
    let t = term(
        30,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07\x1b[90m#x\x1b[m ab\x1b[4D",
    );
    assert_eq!(click(&t, 0, 20), Some(PromptClick::Move(4)));
    // A trailing grey run holding the cursor (a typed comment) is typed input too.
    let t = term(
        30,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07echo \x1b[90m# hello\x1b[m\x1b[4D",
    );
    assert_eq!(click(&t, 0, 20), Some(PromptClick::Move(4)));
    assert_eq!(click(&t, 0, 8), Some(PromptClick::Move(-2)));
}

#[test]
fn prompt_click_excludes_suggestion_spaces_and_separate_right_prompts() {
    for grey in ["90", "38;5;240", "38;2;85;85;85", "2"] {
        for (typed, suggestion) in [("echo", " hello world"), ("echo hel", "lo world")] {
            for right_prompt in [
                "",
                "\x1b[1;20H\x1b[33mRIGHT-CLOCK\x1b[m",
                "\x1b[1;50H\x1b[33mRIGHT-CLOCK\x1b[m",
                "\x1b[1;49H\x1b[33mRIGHT-CLOCK\x1b[m",
                "\x1b[1;48H\x1b[33mRIGHT-CLOCK\x1b[m",
                "\x1b[1;46H\x1b[33mRIGHT-CLOCK\x1b[m",
                "\x1b[1;40H\x1b[33mRIGHT-CLOCK\x1b[m",
            ] {
                let t = term(
                    60,
                    4,
                    &format!(
                        "\x1b]133;A\x07$ \x1b]133;B\x07{typed}\x1b[{grey}m{suggestion}\x1b[m\
                         {right_prompt}\x1b[1;{}H",
                        typed.len() + 3
                    ),
                );
                for col in [typed.len() + 2, typed.len() + 5, 35, 59] {
                    assert_eq!(
                        click(&t, 0, col),
                        Some(PromptClick::Move(0)),
                        "{grey} {typed:?} {right_prompt:?} {col}"
                    );
                }
                assert_eq!(
                    click(&t, 0, 3),
                    Some(PromptClick::Move(1 - typed.len() as isize))
                );
                let mut t = t;
                t.advance(b"\x1b[2D");
                assert_eq!(click(&t, 0, 59), Some(PromptClick::Move(2)));
            }
        }
    }
    // Leading spaces in the same style as typed input remain navigable.
    let t = term(30, 4, "\x1b]133;A\x07$ \x1b]133;B\x07echo  hi\x1b[4D");
    assert_eq!(click(&t, 0, 29), Some(PromptClick::Move(4)));
}

#[test]
fn prompt_click_keeps_syntax_coloured_input_suffixes_reachable() {
    for spaces in [
        " ",
        "  ",
        "     ",
        "                    ",
        "                            ",
    ] {
        for cols in [20, 40, 80] {
            if 11 + spaces.len() >= cols {
                continue;
            }
            let output = format!(
                "\x1b]133;A\x07$ \x1b]133;B\x07\x1b[90mecho\x1b[m{spaces}\x1b[33mhello\x1b[m\x1b[1;3H"
            );
            for chunk in [1, output.len()] {
                let mut t = Terminal::new(cols, 4);
                for bytes in output.as_bytes().chunks(chunk) {
                    t.advance(bytes);
                }
                assert_eq!(
                    click(&t, 0, cols - 1),
                    Some(PromptClick::Move((9 + spaces.len()) as isize)),
                    "{cols} {spaces:?} {chunk}"
                );
                t.advance(b"\x1b[4C\x1b[4D");
                assert_eq!(
                    click(&t, 0, cols - 1),
                    Some(PromptClick::Move((9 + spaces.len()) as isize))
                );
            }
        }
    }
}

#[test]
fn prompt_click_forgets_erased_input_padding_before_a_right_prompt() {
    let mut t = term(
        60,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07\x1b[90mecho\x1b[m                            \x1b[33mhello\x1b[m\x1b[1;3H",
    );
    t.advance(b"echo hel\x1b[90mlo world\x1b[m\x1b[K\x1b[1;40H\x1b[33mRIGHT-CLOCK\x1b[m\x1b[1;11H");
    assert_eq!(click(&t, 0, 59), Some(PromptClick::Move(0)));
    t.resize(80, 4);
    assert_eq!(click(&t, 0, 79), Some(PromptClick::Move(0)));
}

#[test]
fn prompt_click_ignores_an_alternate_screen_line_editor_widget() {
    let mut t = term(
        60,
        4,
        "\x1b]133;A\x07$ \x1b]133;B\x07echo hel\x1b[90mlo world\x1b[m\x1b[1;40H\x1b[33mRIGHT-CLOCK\x1b[m\x1b[1;11H",
    );
    t.advance(b"\x1b[?1049h\x1b]2;widget\x07                                                            \x1b[1;20H\x1b[3P\x1b[4@\x1b[?1049l");
    assert_eq!(click(&t, 0, 59), Some(PromptClick::Move(0)));
    t.advance(b"\x1b[1;3H\x1b[90mecho\x1b[m                    \x1b[33mhello\x1b[m\x1b[K\x1b[1;3H");
    assert_eq!(click(&t, 0, 59), Some(PromptClick::Move(29)));
}

#[test]
fn prompt_click_reaches_a_syntax_highlighted_comment() {
    // zsh-syntax-highlighting draws a typed comment `fg=black,bold`; after Home a click
    // past it or inside it lands there.
    let line = "\x1b]133;A\x07% \x1b]133;B\x07: foo \x1b[1;30m# note\x1b[m";
    let t = term(30, 4, &format!("{line}\x1b[12D"));
    assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(12)));
    assert_eq!(click(&t, 0, 13), Some(PromptClick::Move(11)));
    // A comment-only line, and the same comment in bold fg=8.
    let t = term(
        30,
        4,
        "\x1b]133;A\x07% \x1b]133;B\x07\x1b[1;30m# todo later\x1b[m\x1b[12D",
    );
    assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(12)));
    let t = term(
        30,
        4,
        "\x1b]133;A\x07% \x1b]133;B\x07: foo \x1b[1;90m# note\x1b[m\x1b[12D",
    );
    assert_eq!(click(&t, 0, 25), Some(PromptClick::Move(12)));
    // A trailing autosuggestion after the comment still stops the click.
    let t = term(30, 4, &format!("{line}\x1b[90ms more\x1b[39m\x1b[18D"));
    assert_eq!(click(&t, 0, 28), Some(PromptClick::Move(12)));
}

#[test]
fn prompt_click_needs_an_input_zone_on_the_primary_screen() {
    assert_eq!(click(&term(20, 4, "$ echo hi"), 0, 2), None);
    assert_eq!(click(&term(20, 4, "\x1b]133;A\x07$ "), 0, 1), None);
    let running = "\x1b]133;A\x07$ \x1b]133;B\x07sleep 9\r\n\x1b]133;C\x07";
    assert_eq!(click(&term(20, 4, running), 0, 3), None);
    let done = format!("{running}\x1b]133;D;0\x07");
    assert_eq!(click(&term(20, 4, &done), 0, 3), None);
    let mut t = term(20, 4, "\x1b]133;A\x07$ \x1b]133;B\x07vim");
    t.advance(b"\x1b[?1049h");
    assert_eq!(click(&t, 0, 2), None);
    // A hard newline (a continuation prompt) leaves the marked line.
    let t = term(20, 4, "\x1b]133;A\x07$ \x1b]133;B\x07echo \\\r\n> x");
    assert_eq!(click(&t, 0, 2), None);
    assert_eq!(click(&t, 1, 1), None);
}

#[test]
fn prompt_click_reports_clicks_when_the_prompt_asks_for_them() {
    let mut t = term(
        10,
        4,
        "out\r\n\x1b]133;A;click_events=1\x07~\r\n$ \x1b]133;B\x07abcdefghij",
    );
    assert_eq!(
        click(&t, 1, 3),
        Some(PromptClick::Report { row: 1, col: 3 })
    );
    assert_eq!(
        click(&t, 3, 0),
        Some(PromptClick::Report { row: 3, col: 0 })
    );
    assert_eq!(click(&t, 0, 0), None);
    // The next prompt without the option goes back to cursor keys.
    t.advance(b"\r\n\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07ab");
    assert_eq!(click(&t, 3, 2), Some(PromptClick::Move(-2)));
}

#[test]
fn prompt_click_reports_ascii_input_after_a_unicode_prompt() {
    for prompt in ["λ> ", "日本語> ", "e\u{301}> "] {
        let t = term(
            40,
            4,
            &format!("\x1b]133;A;click_events=1\x07{prompt}\x1b]133;B\x07abc"),
        );
        let (_, start_col) = t.shell.input_start.unwrap();
        assert_eq!(
            click(&t, 0, start_col + 1),
            Some(PromptClick::Report {
                row: 0,
                col: start_col + 1
            }),
            "{prompt:?}"
        );
    }
}

#[test]
fn prompt_click_unadapted_reports_reject_leading_combining_input() {
    for prompt in ["$ ", "界"] {
        for scrollback in [0, 100] {
            let mut t = Terminal::new(40, 4);
            t.set_scrollback_limit(scrollback);
            t.advance(
                format!("\x1b]133;A;click_events=1\x07{prompt}\x1b]133;B\x07\u{301}abc").as_bytes(),
            );
            let (_, start_col) = t.shell.input_start.unwrap();
            assert_eq!(click(&t, 0, start_col + 1), None);
            // Once the marked cell is erased, a Unicode prompt is still excluded.
            t.advance(
                format!("\r\x1b[2K\x1b]133;A;click_events=1\x07{prompt}\x1b]133;B\x07abc")
                    .as_bytes(),
            );
            assert_eq!(
                click(&t, 0, start_col + 1),
                Some(PromptClick::Report {
                    row: 0,
                    col: start_col + 1
                })
            );
        }
    }
}

#[test]
fn prompt_click_unadapted_reports_reject_unicode_input() {
    // Native fish's report handler treats a screen column as an index into its
    // character array. Never feed wide spacers or combining boundaries to it.
    let t = term(
        40,
        4,
        "\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07: 日本語abc",
    );
    for col in [5, 6, 7, 8, 10, 11] {
        assert_eq!(click(&t, 0, col), None, "{col}");
    }
    let t = term(
        40,
        4,
        "\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07: e\u{301}abc",
    );
    assert_eq!(click(&t, 0, 5), None);
    let t = term(
        40,
        4,
        "\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07: 😀abc",
    );
    assert_eq!(click(&t, 0, 5), None);
    assert_eq!(click(&t, 0, 6), None);

    let t = term(
        10,
        4,
        "\x1b]133;A;click_events=1\x07$ \x1b]133;B\x07: 日本語abc",
    );
    assert_eq!(click(&t, 0, 7), None);
    assert_eq!(click(&t, 1, 1), None);
}

#[test]
fn prompt_click_adapter_loss_rejects_explicit_redraw_rows_and_missing_origin() {
    for scrollback in [0, 100] {
        let mut t = Terminal::new(10, 5);
        t.set_scrollback_limit(scrollback);
        t.advance(b"\x1b]133;A;click_events=1\x07> \x1b]777;trident;prompt-input;ready\x07\x1b]133;B\x07abc");
        assert!(t.has_prompt_input_adapter());
        t.advance(
            "\r\x1b]133;A;click_events=1\x07> \x1b]133;B\x07abcdefg\r\n日本😀e\u{301}".as_bytes(),
        );
        assert!(!t.has_prompt_input_adapter());
        assert!(!t.line(0).wrapped());
        assert_eq!(click(&t, 0, 8), None);
        assert_eq!(click(&t, 1, 1), None);
        // The cursor can return to the first row without removing the next row.
        t.advance(b"\x1b[1;8H");
        assert_eq!(click(&t, 0, 8), None);
        t.advance(b"\r\x1b]133;A;click_events=1\x07> abc");
        assert_eq!(click(&t, 0, 3), None);
    }
}

#[test]
fn prompt_click_works_without_scrollback() {
    let mut t = Terminal::new(20, 4);
    t.set_scrollback_limit(0);
    t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi");
    assert_eq!(click(&t, 0, 2), Some(PromptClick::Move(-7)));
    t.advance(b"\r\n\r\n\r\n\x1b]133;A\x07$ \x1b]133;B\x07echo hello there, wrap");
    assert_eq!(click(&t, 2, 2), Some(PromptClick::Move(-22)));
    assert_eq!(click(&t, 3, 0), Some(PromptClick::Move(-4)));
    // A top-aligned region above the prompt scrolls without moving it.
    t.advance(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07ab\x1b7\x1b[1;2r\x1b[2H\n\x1b[r\x1b8");
    assert_eq!(click(&t, 3, 2), Some(PromptClick::Move(-2)));
}

#[test]
fn kitty_keyboard_flag_query_push_pop_set() {
    let mut t = Terminal::new(10, 3);
    t.advance(b"\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?0u");

    t.advance(b"\x1b[=9;1u");
    assert_eq!(t.kitty_flags(), 0b1001);
    t.advance(b"\x1b[=2;2u");
    assert_eq!(t.kitty_flags(), 0b1011);
    t.advance(b"\x1b[=1;3u");
    assert_eq!(t.kitty_flags(), 0b1010);
    // Unsupported bits are masked out.
    t.advance(b"\x1b[=255;1u");
    assert_eq!(t.kitty_flags(), 0b11111);

    t.advance(b"\x1b[>5u");
    assert_eq!(t.kitty_flags(), 5);
    t.advance(b"\x1b[>u");
    assert_eq!(t.kitty_flags(), 0);
    t.advance(b"\x1b[<u");
    assert_eq!(t.kitty_flags(), 5);
    // Popping past the bottom resets the flags.
    t.advance(b"\x1b[<3u");
    assert_eq!(t.kitty_flags(), 0);

    t.advance(b"\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?0u");
}

#[test]
fn kitty_keyboard_flags_are_per_screen() {
    let mut t = Terminal::new(10, 3);
    t.advance(b"\x1b[=5;1u");
    t.advance(b"\x1b[?1049h");
    assert_eq!(t.kitty_flags(), 0);
    t.advance(b"\x1b[>11u\x1b[?u");
    assert_eq!(t.take_responses(), b"\x1b[?11u");
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.kitty_flags(), 5);
    t.advance(b"\x1b[?1049h");
    assert_eq!(t.kitty_flags(), 11);
    // A full reset clears both stacks.
    t.advance(b"\x1b[?1049l\x1bc");
    assert_eq!(t.kitty_flags(), 0);
}
