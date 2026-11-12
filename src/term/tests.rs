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
