use super::super::*;
use std::fmt::Write;
use std::path::Path;

#[test]
fn shell_integration_clearing() {
    let cases: &[(&[u8], &[&str])] = &[
        (b"output\r\n\x1b]133;A\x07$ \x1b[2J", &["output", "$ "]),
        (b"output\r\n$ \x1b[2J", &["output", "$ "]),
        (
            b"\x1b]133;A\x07$ cmd\x1b]133;C\x07\r\noutput\x1b[2J",
            &["$ cmd", "output"],
        ),
        (
            b"output\r\n\x1b]133;A\x07$ \x1b]133;B\x07clear\x1b]133;C\x07\x1b[H\x1b[2J",
            &["output", "$ clear"],
        ),
        (
            b"output\r\n\x1b]133;A\x07$ \x1b]133;B\x07clear\x1b]133;C\x07\x1b[3J\x1b[H\x1b[2J",
            &[],
        ),
        (
            b"\x1b[?1049houtput\r\n\x1b]133;A\x07$ \x1b[2J\x1b[?1049l",
            &[],
        ),
    ];
    for (input, expected) in cases {
        let mut t = Terminal::new(20, 4);
        t.advance(input);
        let actual: Vec<_> = t.primary.scrollback.iter().map(Row::text).collect();
        let expected: Vec<_> = expected.iter().map(|s| s.trim_end().to_string()).collect();
        assert_eq!(actual, expected, "{input:?}");
        assert!(t.primary.lines.iter().all(|line| line.text().is_empty()));
    }
}

#[test]
fn shell_integration_status_and_events() {
    for (code, expected, flag) in [
        ("0", Some(0), Some(RowFlags::SUCCEEDED)),
        ("12", Some(12), Some(RowFlags::FAILED)),
        ("-1", Some(-1), Some(RowFlags::FAILED)),
        ("", None, None),
        ("bad", None, None),
    ] {
        let mut t = Terminal::new(20, 2);
        t.advance(
            format!("\x1b]133;A;opt\x07$ cmd\r\n\x1b]133;C\x07out\r\n\x1b]133;D;{code};opt\x07")
                .as_bytes(),
        );
        let prompt = &t.primary.scrollback[0];
        assert!(prompt.flags.contains(RowFlags::PROMPT_START));
        assert_eq!(
            prompt.flags.contains(RowFlags::FAILED),
            flag == Some(RowFlags::FAILED)
        );
        assert_eq!(
            prompt.flags.contains(RowFlags::SUCCEEDED),
            flag == Some(RowFlags::SUCCEEDED)
        );
        assert_eq!(
            t.take_command_finished(),
            vec![CommandFinished {
                exit_code: expected
            }]
        );
        assert!(t.take_command_finished().is_empty());
    }
    let mut t = Terminal::new(20, 2);
    t.advance(b"\x1b]133;D\x07\x1b]133;Z\x07");
    assert!(t.take_command_finished().is_empty());
}

#[test]
fn shell_integration_completion_queue_is_bounded() {
    let mut t = Terminal::new(20, 3);
    assert!(t.take_command_finished().is_empty());
    for code in 0..100 {
        t.advance(format!("\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;{code}\x07").as_bytes());
        assert!(t.shell.command_finished.len() <= 16);
    }
    assert_eq!(
        t.take_command_finished(),
        (84..100)
            .map(|code| CommandFinished {
                exit_code: Some(code),
            })
            .collect::<Vec<_>>()
    );
    assert!(t.take_command_finished().is_empty());
    t.advance(b"\x1b]133;A\x07\x1b]133;C\x07\x1b]133;D;bad\x07");
    assert_eq!(
        t.take_command_finished(),
        vec![CommandFinished { exit_code: None }]
    );
}

#[test]
fn shell_integration_spurious_completion_preserves_status() {
    for prior_exit in [0, 1] {
        let mut t = Terminal::new(20, 4);
        t.advance(
            format!("\x1b]133;A\x07$ cmd\x1b]133;C\x07\r\nout\r\n\x1b]133;D;{prior_exit}\x07")
                .as_bytes(),
        );
        assert_eq!(t.take_command_finished().len(), 1);
        let prior = t.primary.lines[0].flags;
        for input in [
            b"\x1b]133;D;2\x07".as_slice(),
            b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;D;0\x07",
            b"\x1b]133;A\x07$ \x1b]133;B\x07^C\r\n\x1b]133;D;130\x07",
        ] {
            t.modes.bracketed_paste = true;
            t.advance(input);
            assert!(!t.modes.bracketed_paste);
            assert!(t.take_command_finished().is_empty());
            let previous = t.primary.scrollback.front().unwrap_or(&t.primary.lines[0]);
            assert_eq!(previous.flags, prior);
            assert!(!t.primary.lines.iter().skip(1).any(|row| {
                row.flags.contains(RowFlags::FAILED) || row.flags.contains(RowFlags::SUCCEEDED)
            }));
        }
    }
}

#[test]
fn shell_integration_prompt_redraw_and_empty_output() {
    for erase in ["\x1b[K", "\x1b[2K"] {
        let mut t = Terminal::new(20, 3);
        t.advance(format!("\x1b]133;A\x07{erase}$ prompt").as_bytes());
        assert!(t.primary.lines[0].flags.contains(RowFlags::PROMPT_START));
    }
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ true\r\n\x1b]133;C\x07\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    t.select_command_output();
    assert!(t.selection.is_none());
    assert!(t.selection_text().is_none());
}

#[test]
fn shell_integration_output_ends_before_missing_newline_marker() {
    for marker in ["%", "⏎"] {
        let mut t = Terminal::new(20, 3);
        t.advance(format!("\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\x1b]133;D;0\x07{marker}                    \r\x1b]133;A\x07$ ").as_bytes());
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("abc"));
    }
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\x1b]133;A\x07$ ");
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some("abc"));

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ seq 1 40\r\n\x1b]133;C\x07");
    for n in 1..=40 {
        t.advance(format!("{n}\r\n").as_bytes());
    }
    t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    t.select_command_output();
    assert_eq!(
        t.selection_text().unwrap(),
        (1..=40)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn shell_integration_carriage_return_output_extent() {
    for (output, expected) in [
        ("abc\r", "abc"),
        ("abc\rx", "xbc"),
        ("abc\r\x1b[K", ""),
        ("abc\r\n", "abc"),
        ("abc\r\n\r", "abc"),
        ("a界\r", "a界"),
    ] {
        let input = format!(
            "\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07{output}\x1b]133;D;0\x07\r\n\x1b]133;A\x07$ "
        );
        for streamed in [false, true] {
            let mut t = Terminal::new(20, 3);
            if streamed {
                for byte in input.bytes() {
                    t.advance(&[byte]);
                }
            } else {
                t.advance(input.as_bytes());
            }
            t.select_command_output();
            assert_eq!(
                t.selection_text().as_deref().unwrap_or(""),
                expected,
                "output {output:?}, streamed {streamed}"
            );
        }
    }
}

#[test]
fn shell_marks_seen_persists_for_session() {
    let mut t = Terminal::new(20, 3);
    assert!(!t.has_shell_marks());
    t.advance(b"plain\x1b]133;invalid\x07");
    assert!(!t.has_shell_marks());
    t.advance(b"\x1b]133;A\x07\x1b[3J\x1b[2J");
    assert!(t.has_shell_marks());
}

#[test]
fn shell_output_copies_real_pty_captures() {
    fn unhex(input: &str) -> Vec<u8> {
        input
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }
    for (shell, capture) in [
        ("zsh 5.8.1", include_str!("golden/zsh-output.hex")),
        ("bash 5.1.16", include_str!("golden/bash-output.hex")),
        ("fish 3.3.1", include_str!("golden/fish-output.hex")),
    ] {
        for streamed in [false, true] {
            let mut t = Terminal::new(80, 24);
            for record in capture.lines().filter(|line| !line.starts_with('#')) {
                let mut fields = record.split('|');
                let case = fields.next().unwrap();
                let expected = unhex(fields.next().unwrap());
                let bytes = unhex(fields.next().unwrap());
                if streamed {
                    for byte in bytes {
                        t.advance(&[byte]);
                    }
                } else {
                    t.advance(&bytes);
                }
                if case == "startup" {
                    t.take_command_finished();
                    continue;
                }
                t.select_command_output();
                assert_eq!(
                    t.selection_text().unwrap_or_default().as_bytes(),
                    expected,
                    "{shell} {case}, streamed={streamed}"
                );
                let events = t.take_command_finished();
                assert_eq!(
                    events,
                    [CommandFinished {
                        exit_code: Some(if case == "status" { 1 } else { 0 })
                    }],
                    "{shell} {case}"
                );
            }
        }
    }
}

#[test]
fn shell_output_snapshot_lifetime() {
    fn completed() -> Terminal {
        let mut t = Terminal::new(20, 3);
        t.advance(b"\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\r\x1b]133;D;0\x07\x1b]133;A\x07$ ");
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("abc"));
        t
    }
    for invalidate in [b"\x1b[3J".as_slice(), b"\x1b[?1049h\x1b[?1049l", b"\x1bc"] {
        let mut t = completed();
        t.advance(invalidate);
        assert!(t.shell.output_ends.is_empty());
    }
    let mut t = completed();
    t.resize(10, 4);
    assert!(t.shell.output_ends.is_empty());

    let mut t = completed();
    t.set_scrollback_limit(1);
    t.advance(b"\r\n".repeat(10).as_slice());
    assert!(t.selection_text().is_none());
    t.advance(b"\x1b]133;A\x07");
    assert!(t.shell.output_ends.is_empty());

    let mut t = Terminal::new(20, 40);
    for _ in 0..40 {
        t.advance(b"\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\x1b]133;D;0\x07\r\n");
    }
    assert_eq!(t.shell.output_ends.len(), 16);
    assert_eq!(std::mem::size_of::<Row>(), 40);
}

#[test]
fn shell_output_starts_on_prompt_row() {
    for (output, expected) in [
        ("result", "result"),
        ("", ""),
        ("first\r\nlast", "first\nlast"),
        ("12345678901234567890", "12345678901234567890"),
    ] {
        for streamed in [false, true] {
            let mut t = Terminal::new(20, 3);
            let bytes = format!(
                "\x1b]133;A\x07$ run\x1b]133;B\x07\x1b]133;C\x07{output}\x1b]133;D;0\x07\x1b]133;A\x07$ "
            );
            for chunk in bytes
                .as_bytes()
                .chunks(if streamed { 1 } else { bytes.len() })
            {
                t.advance(chunk);
            }
            t.select_command_output();
            assert_eq!(t.selection_text().as_deref().unwrap_or(""), expected);
            if expected.is_empty() {
                assert!(t.selection.is_none());
            }
        }
    }
}

#[test]
fn shell_integration_unterminated_output() {
    for (output, expected) in [
        ("foo", "foo"),
        ("first\r\nlast", "first\nlast"),
        ("x", "x"),
        ("", ""),
        ("12345678901234567890", "12345678901234567890"),
    ] {
        let mut t = Terminal::new(20, 3);
        t.advance(
            format!(
                "\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07{output}\x1b]133;D;0\x07\x1b]133;A\x07$ "
            )
            .as_bytes(),
        );
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref().unwrap_or(""), expected);
        if expected.is_empty() {
            assert!(t.selection.is_none());
        }
        t.resize(10, 3);
        t.resize(20, 3);
        t.advance(b"\x1b[2J");
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref().unwrap_or(""), expected);
    }
}

#[test]
fn shell_integration_navigation_selection_and_streaming() {
    let input = b"\x1b]133;A\x07$ one\x1b]133;B\x07\r\n\x1b]133;C\x07first\r\n\r\n\x1b]133;D;1\x07\x1b]133;A\x07$ two\r\n\x1b]133;C\x07second\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ";
    let mut t = Terminal::new(20, 2);
    t.advance(input);
    let mut streamed = Terminal::new(20, 2);
    for byte in input {
        streamed.advance(&[*byte]);
    }
    assert_eq!(t.primary.lines, streamed.primary.lines);
    assert_eq!(t.primary.scrollback, streamed.primary.scrollback);
    assert_eq!(t.take_command_finished(), streamed.take_command_finished());
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some("second"));
    t.jump_to_prompt(true);
    assert_eq!(t.viewport_row(0).text(), "$ two");
    t.jump_to_prompt(true);
    assert_eq!(t.viewport_row(0).text(), "$ one");
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some("first"));
    t.jump_to_prompt(false);
    assert_eq!(t.viewport_row(0).text(), "$ two");
    t.jump_to_prompt(false);
    assert_eq!(t.display_offset, 0);
    t.advance(b"\x1b[2J");
    assert!(
        t.primary
            .lines
            .iter()
            .all(|row| row.flags == RowFlags::default())
    );
    let mut empty = Terminal::new(20, 3);
    empty.select_command_output();
    empty.jump_to_prompt(true);
    empty.jump_to_prompt(false);
    assert!(empty.selection.is_none());
    assert_eq!(empty.display_offset, 0);
}

#[test]
fn shell_integration_multiline_resize_and_guard_reset() {
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07prompt\r\n\x1b]133;B\x07input\r\nmore\x1b]133;C\x07");
    assert!(
        t.primary
            .lines
            .iter()
            .all(|row| row.flags.contains(RowFlags::PROMPT))
    );
    t.resize(10, 3);
    assert!(t.primary.lines[0].flags.contains(RowFlags::PROMPT_START));
    t.advance(b"\x1b[3J");
    t.advance(b"\x1b[2J");
    assert_eq!(t.primary.scrollback.len(), 3);
    t.advance(b"\x1b]133;C\x07\r\n\x1b]133;D\x07\x1b]133;A\x07\x1b[2J");
    assert_eq!(t.primary.lines[t.cursor.row].flags, RowFlags::default());
}

#[test]
fn shell_prompt_rows_survive_ghost_batches() {
    for mark in *b"AB" {
        for limit in [0, 2, 100] {
            let mut batched = Terminal::new(20, 3);
            let mut streamed = Terminal::new(20, 3);
            for term in [&mut batched, &mut streamed] {
                term.set_scrollback_limit(limit);
                term.advance(b"\x1b]133;A\x07");
                if mark == b'B' {
                    term.advance(b"\x1b]133;B\x07");
                }
            }
            let bytes = b"prompt or input\r\n".repeat(400);
            crate::ghost::ADVANCES.set(0);
            batched.advance(&bytes);
            assert!(crate::ghost::ADVANCES.get() > 0);
            for byte in &bytes {
                streamed.advance(&[*byte]);
            }
            for term in [&mut batched, &mut streamed] {
                term.advance(b"\x1b]133;C\x07");
            }
            assert_eq!(batched.primary.lines, streamed.primary.lines);
            assert_eq!(batched.primary.scrollback, streamed.primary.scrollback);
            assert!(
                batched
                    .primary
                    .scrollback
                    .iter()
                    .chain(&batched.primary.lines)
                    .all(|row| row.flags.contains(RowFlags::PROMPT))
            );
            assert!(batched.primary.scrollback.len() == limit);
        }
    }
}

#[test]
fn shell_integration_lazy_prompt_rows() {
    for limit in [0, 2, 100] {
        let mut t = Terminal::new(20, 3);
        t.set_scrollback_limit(limit);
        t.advance(b"\x1b]133;A\x07prompt\r\n\x1b]133;B\x07");
        let input = b"input\r\n".repeat(200);
        t.advance(&input);
        t.advance(b"\x1b]133;C\x07");
        assert!(
            t.primary
                .scrollback
                .iter()
                .chain(&t.primary.lines)
                .all(|row| { row.flags.contains(RowFlags::PROMPT) })
        );
    }

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;D\x07");
    assert!(t.primary.lines[0].flags.contains(RowFlags::PROMPT));
    assert!(!t.primary.lines[1].flags.contains(RowFlags::PROMPT));
    assert!(t.take_command_finished().is_empty());

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07prompt\r\n\x1b]133;invalid\x07");
    assert!(!t.primary.lines[1].flags.contains(RowFlags::PROMPT));
    t.advance(b"\x1b]133;B\x07");
    assert!(t.primary.lines[1].flags.contains(RowFlags::PROMPT));
}

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
