use super::*;

mod golden;

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
    assert_eq!(t.scrollback_len(), 1);
    t.resize(12, 3);
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
        1, 6, 7, 25, 47, 1047, 1049, 1000, 1002, 1003, 1004, 1006, 1007, 2004,
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
    for mode in [1000, 1002, 1003] {
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
    for mode in [1000, 1002, 1003, 1006, 1004, 2004] {
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
    assert!(t.viewport_row(0).wrapped);
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
        assert_eq!(t.display_offset(), 0);
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
