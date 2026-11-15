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
    assert_eq!(t.take_responses(), b"\x1b[3;4R\x1b[?62;22c");
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
    t.primary
        .scrollback
        .iter()
        .chain(&t.primary.lines)
        .map(|row| (row.text(), row.cells.clone(), row.wrapped()))
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
        [
            ("warning: b".to_owned(), crate::ROW_CHANGED),
            ("took 1.4s".to_owned(), crate::ROW_CHANGED),
        ]
    );
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
