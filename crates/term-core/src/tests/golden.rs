use super::super::*;
use std::fmt::Write;
use std::path::Path;

struct Case {
    name: &'static str,
    cols: usize,
    rows: usize,
    input: &'static [u8],
    resize: Option<(usize, usize)>,
}

fn cases() -> Vec<Case> {
    let inputs: &[(&str, usize, usize, &[u8])] = &[
        ("empty", 5, 2, b""),
        ("printable_trailing_spaces", 6, 2, b"ab  "),
        ("utf8", 8, 2, "caf\u{e9}".as_bytes()),
        ("carriage_return", 6, 2, b"abcd\rXY"),
        ("linefeed", 6, 3, b"ab\ncd"),
        ("crlf", 6, 3, b"ab\r\ncd"),
        ("backspace", 6, 2, b"abc\x08X"),
        ("tab", 12, 2, b"a\tb"),
        ("pending_wrap", 4, 2, b"abcd"),
        ("soft_wrap", 4, 3, b"abcdef"),
        ("autowrap_off", 4, 2, b"\x1b[?7labcdef"),
        (
            "cursor_absolute_relative",
            6,
            3,
            b"\x1b[2;3HX\x1b[A\x1b[2DY",
        ),
        ("cursor_save_restore", 6, 3, b"ab\x1b7\x1b[3;1HX\x1b8Y"),
        ("cursor_hidden_bar", 5, 2, b"ab\x1b[?25l\x1b[5 q"),
        ("erase_line_end", 6, 2, b"abcdef\x1b[1;3H\x1b[K"),
        ("erase_line_start", 6, 2, b"abcdef\x1b[1;3H\x1b[1K"),
        ("erase_line_all_background", 6, 2, b"abcdef\x1b[44m\x1b[2K"),
        (
            "erase_display_end",
            4,
            3,
            b"aaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[J",
        ),
        (
            "erase_display_start",
            4,
            3,
            b"aaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[1J",
        ),
        ("erase_display_all", 4, 2, b"abcd\r\nef\x1b[2J"),
        ("sgr_basic_reset", 6, 2, b"\x1b[1;31;44mA\x1b[0mB"),
        (
            "insert_delete_characters",
            6,
            2,
            b"abcd\x1b[1;2H\x1b[@X\x1b[P",
        ),
        ("scrollback", 4, 2, b"one\r\ntwo\r\ntri\r\nfour"),
        ("reset", 4, 2, b"a\r\nb\r\nc\x1b[31m\x1bcZ"),
        (
            "sgr_styles",
            10,
            2,
            b"\x1b[1;2;3;4;7;8;9mA\x1b[22mB\x1b[23;24;27;28;29mC\x1b[4:0mD",
        ),
        (
            "sgr_extended_colors",
            8,
            2,
            b"\x1b[91;104mA\x1b[38;5;200;48;5;236mB\x1b[38;2;1;2;3;48:2::4:5:6mC\x1b[39;49mD",
        ),
        ("cjk_wrap", 5, 3, "a你好b".as_bytes()),
        ("cjk_edge", 4, 2, "abc你".as_bytes()),
        (
            "alternate_active",
            6,
            2,
            b"shell\x1b[?1049h\x1b[H\x1b[32mvim",
        ),
        (
            "alternate_restored",
            6,
            2,
            b"shell\x1b[?1049h\x1b[Hvim\x1b[?1049l!",
        ),
        (
            "scroll_region_forward",
            4,
            4,
            b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[3;1H\n",
        ),
        (
            "scroll_region_reverse",
            4,
            4,
            b"1\r\n2\r\n3\r\n4\x1b[2;3r\x1b[2;1H\x1bM",
        ),
        (
            "insert_delete_lines",
            4,
            4,
            b"1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[L\x1b[M",
        ),
        ("unsupported_sequences", 6, 2, b"a\x1b[999z\x1b[9999mb"),
        ("invalid_utf8", 6, 2, b"a\xffb\r\nZ"),
    ];
    let mut cases: Vec<_> = inputs
        .iter()
        .map(|&(name, cols, rows, input)| Case {
            name,
            cols,
            rows,
            input,
            resize: None,
        })
        .collect();
    cases.push(Case {
        name: "resize_height",
        cols: 6,
        rows: 3,
        input: b"one\r\ntwo\r\nthree",
        resize: Some((6, 2)),
    });
    cases
}

fn color(color: Color) -> String {
    match color {
        Color::DefaultFg => "default-fg".into(),
        Color::DefaultBg => "default-bg".into(),
        Color::Indexed(index) => format!("index({index})"),
        Color::Rgb(r, g, b) => format!("rgb({r},{g},{b})"),
    }
}

fn attributes(cell: &Cell) -> String {
    let flags: Vec<_> = [
        (Flags::BOLD, "bold"),
        (Flags::DIM, "dim"),
        (Flags::ITALIC, "italic"),
        (Flags::UNDERLINE, "underline"),
        (Flags::INVERSE, "inverse"),
        (Flags::HIDDEN, "hidden"),
        (Flags::STRIKE, "strike"),
        (Flags::WIDE, "wide"),
        (Flags::WIDE_SPACER, "wide-spacer"),
        (Flags::PAD, "pad"),
    ]
    .into_iter()
    .filter_map(|(flag, name)| cell.flags.contains(flag).then_some(name))
    .collect();
    format!(
        "fg={} bg={} flags=[{}]",
        color(cell.fg),
        color(cell.bg),
        flags.join(",")
    )
}

fn rows<'a>(out: &mut String, label: &str, rows: impl IntoIterator<Item = &'a Row>) {
    writeln!(out, "{label}:").unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        let text: String = row.cells.iter().map(|cell| cell.c).collect();
        writeln!(out, "  row {index} wrap={} text={text:?}", row.wrapped).unwrap();
        let mut start = 0;
        while start < row.cells.len() {
            let attr = attributes(&row.cells[start]);
            let mut end = start + 1;
            while end < row.cells.len() && attributes(&row.cells[end]) == attr {
                end += 1;
            }
            writeln!(out, "    cells {start}..{end}: {attr}").unwrap();
            start = end;
        }
    }
}

fn snapshot(t: &Terminal) -> String {
    let mut out = format!(
        "size={}x{} active={}\ncursor row={} col={} visible={} shape={} pending_wrap={}\n",
        t.cols,
        t.rows,
        if t.alt_active { "alternate" } else { "primary" },
        t.cursor.row,
        t.cursor.col,
        t.modes.show_cursor,
        match t.cursor_shape {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Bar => "bar",
        },
        t.cursor.pending_wrap,
    );
    writeln!(out, "cursor attributes: {}", attributes(&t.cursor.template)).unwrap();
    writeln!(
        out,
        "scroll_region={}..={} history={} display_offset={}",
        t.scroll_top, t.scroll_bottom, t.history, t.display_offset
    )
    .unwrap();
    rows(&mut out, "primary", t.primary.lines.iter());
    rows(
        &mut out,
        "scrollback (oldest first)",
        t.primary.scrollback.iter(),
    );
    rows(&mut out, "alternate", t.alternate.lines.iter());
    out
}

fn difference(name: &str, expected: &str, actual: &str) -> String {
    let mut out = format!("golden case {name}\n--- expected\n+++ actual\n");
    let expected: Vec<_> = expected.lines().collect();
    let actual: Vec<_> = actual.lines().collect();
    for index in 0..expected.len().max(actual.len()) {
        if expected.get(index) != actual.get(index) {
            writeln!(out, "@@ line {} @@", index + 1).unwrap();
            if let Some(line) = expected.get(index) {
                writeln!(out, "-{line}").unwrap();
            }
            if let Some(line) = actual.get(index) {
                writeln!(out, "+{line}").unwrap();
            }
        }
    }
    out
}

#[test]
fn golden_terminal_state() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden");
    let update = std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1");
    for case in cases() {
        let run = |chunks: Vec<&[u8]>| {
            let mut t = Terminal::new(case.cols, case.rows);
            for chunk in chunks {
                t.advance(chunk);
            }
            if let Some((cols, rows)) = case.resize {
                t.resize(cols, rows);
            }
            snapshot(&t)
        };
        let actual = run(vec![case.input]);
        let path = dir.join(format!("{}.snap", case.name));
        if update {
            std::fs::write(&path, &actual).unwrap();
        } else {
            let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("golden case {}: {}: {error}; use UPDATE_GOLDEN=1 cargo test -p term-core golden", case.name, path.display())
            });
            assert!(
                expected == actual,
                "{}",
                difference(case.name, &expected, &actual)
            );
        }
        for split in 1..case.input.len() {
            // vte's invalid-UTF-8 consumption can depend on chunk boundaries.
            if std::str::from_utf8(case.input).is_err() && !case.input[split - 1].is_ascii() {
                continue;
            }
            let chunked = run(vec![&case.input[..split], &case.input[split..]]);
            assert!(
                actual == chunked,
                "split {split}: {}",
                difference(case.name, &actual, &chunked)
            );
        }
        if std::str::from_utf8(case.input).is_ok() {
            let bytewise = run(case.input.chunks(1).collect());
            assert!(
                actual == bytewise,
                "bytewise: {}",
                difference(case.name, &actual, &bytewise)
            );
        }
    }
}
