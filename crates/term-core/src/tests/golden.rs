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
fn shell_output_start_is_clamped_on_resize() {
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ run\x1b]133;B\x07\x1b]133;C\x07result\x1b]133;D;0\x07");
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some("result"));
    t.resize(4, 5);
    t.select_command_output();
    let (start, _) = t.selection_range().unwrap();
    assert_eq!(start.col, 0);
    assert_eq!(start.line, t.first_line());
    assert_eq!(t.selection_text().as_deref(), Some("$ ru"));

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07");
    t.resize(8, 3);
    t.select_command_output();
    assert_eq!(t.selection_range().unwrap().0, Point { line: 0, col: 5 });
    assert_eq!(t.selection_text().as_deref(), Some("res"));
}

#[test]
fn shell_output_snapshots_do_not_follow_different_rows() {
    for movement in [
        "\x1b[1;1H\x1b[M",
        "\x1b[1;2r\x1b[2;1H\n",
        "\x1b[1;1H\x1b[T",
        "\x1b[1;1H\x1b[L",
        "\x1b[1;1H\x1bM",
        "\x1b[1;2r\x1b[S",
    ] {
        for streamed in [false, true] {
            let mut t = Terminal::new(20, 4);
            let bytes = format!(
                "\x1b]133;A\x07$ run\x1b]133;C\x07first!\x1b]133;D;0\x07\r\n\x1b]133;A\x07$ run\x1b]133;C\x07second\x1b]133;D;0\x07{movement}"
            );
            for chunk in bytes
                .as_bytes()
                .chunks(if streamed { 1 } else { bytes.len() })
            {
                t.advance(chunk);
            }
            t.select_command_output();
            assert_eq!(t.selection_text().as_deref(), Some("second"));
        }
    }
}

#[test]
fn shell_output_in_partial_regions_uses_live_rows() {
    for streamed in [false, true] {
        let mut t = Terminal::new(20, 4);
        let bytes = b"\x1b[1;2r\x1b]133;A\x07$ run\x1b]133;C\x07first!\x1b]133;D;0\x07\r\n\x1b]133;A\x07$ run\x1b]133;C\x07second\x1b]133;D;0\x07\r\n\x1b[r";
        for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
            t.advance(chunk);
        }
        assert!(t.shell.output_ends.is_empty());
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("second"));
    }
}

#[test]
fn shell_output_snapshots_survive_unaffected_region_operations() {
    for (before, after, expected) in [
        ("", "\x1b[1;2r", "abc"),
        ("", "\x1b[3;1H\x1b[M", "abc"),
        ("", "\x1b[3;1H\x1b[L", "abc"),
        ("", "\x1b[3;4r\x1b[T", "abc"),
        ("", "\x1b[3;4r\x1b[S", "abc"),
        ("\x1b[1;2r", "", "abc"),
        ("\x1b[3;1H", "\x1b[1;2r\x1b[S", "abc"),
        ("\x1b[3;1H", "\x1b[1;2r\x1b[2;1H\n", "abc"),
        ("\x1b[3;1H", "\x1b[1;2r\x1b[M", "abc"),
    ] {
        for streamed in [false, true] {
            let mut t = Terminal::new(20, 4);
            let bytes = format!(
                "{before}\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\r\x1b]133;D;0\x07\x1b]133;A\x07$ {after}"
            );
            for chunk in bytes
                .as_bytes()
                .chunks(if streamed { 1 } else { bytes.len() })
            {
                t.advance(chunk);
            }
            t.select_command_output();
            assert_eq!(
                t.selection_text().as_deref(),
                Some(expected),
                "{before:?}, {after:?}"
            );
        }
    }
}

#[test]
fn shell_same_row_output_snapshot_in_partial_region() {
    for streamed in [false, true] {
        let mut t = Terminal::new(20, 4);
        let bytes = b"\x1b[1;2r\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07\r\x1b]133;A\x07$ next prompt";
        for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
            t.advance(chunk);
        }
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("result"));
    }
}

#[test]
fn shell_pending_wrap_command_before_crlf() {
    for streamed in [false, true] {
        let mut t = Terminal::new(20, 4);
        let bytes = b"\x1b]133;A\x07$ 123456789012345678\x1b]133;B\x07\x1b]133;C\x07\r\nresult\x1b]133;D;0\x07";
        for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
            t.advance(chunk);
        }
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("result"));
    }
}

#[test]
fn shell_same_row_output_survives_prompt_erase() {
    for erase in ["\x1b[K", "\x1b[2K"] {
        for streamed in [false, true] {
            let mut t = Terminal::new(20, 3);
            let bytes = format!(
                "\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07\r\x1b]133;A\x07{erase}$ "
            );
            let bytes = bytes.as_bytes();
            for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
                t.advance(chunk);
            }
            t.select_command_output();
            assert_eq!(t.selection_text().as_deref(), Some("result"));
        }
    }
}

#[test]
fn shell_same_row_output_starts_survive_history() {
    for streamed in [false, true] {
        let mut t = Terminal::new(20, 1);
        let bytes = b"\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07\r\n".repeat(25);
        for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
            t.advance(chunk);
        }
        t.display_offset = t.history as usize;
        t.select_command_output();
        assert_eq!(t.selection_range().unwrap().0, Point { line: 0, col: 5 });
        assert_eq!(t.selection_text().as_deref(), Some("result"));
    }
}

#[test]
fn shell_same_row_output_start_survives_alternate_screen() {
    for streamed in [false, true] {
        let mut t = Terminal::new(20, 3);
        let bytes =
            b"\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07\x1b[?1049hvim\x1b[?1049l";
        for chunk in bytes.chunks(if streamed { 1 } else { bytes.len() }) {
            t.advance(chunk);
        }
        t.select_command_output();
        assert_eq!(t.selection_text().as_deref(), Some("result"));
    }
}

#[test]
fn shell_output_starts_on_prompt_row() {
    for (output, expected) in [
        ("result", "result"),
        ("", ""),
        ("first\r\nlast", "first\nlast"),
        ("\r\nresult", "result"),
        ("\r\nfirst\r\nlast", "first\nlast"),
        ("\r\n", ""),
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
        ("combining_mark", 6, 2, "e\u{301}x".as_bytes()),
        (
            "combining_marks_stacked",
            6,
            2,
            "a\u{323}\u{302}b".as_bytes(),
        ),
        (
            "combining_backspace_overwrite",
            6,
            2,
            "e\u{301}\x08a".as_bytes(),
        ),
        (
            "combining_delete",
            6,
            2,
            "e\u{301}xy\x1b[1G\x1b[P".as_bytes(),
        ),
        (
            "combining_erase",
            6,
            2,
            "e\u{301}xy\x1b[1G\x1b[X".as_bytes(),
        ),
        (
            "combining_insert_mode",
            6,
            2,
            "e\u{301}\x1b[4h\x1b[1Gx".as_bytes(),
        ),
        ("combining_at_line_start", 6, 2, "\u{301}a".as_bytes()),
        ("combining_after_cr", 6, 2, "ab\r\u{301}".as_bytes()),
        ("combining_at_pending_wrap", 3, 2, "abc\u{301}d".as_bytes()),
        ("combining_on_wide", 6, 2, "\u{4e2d}\u{301}x".as_bytes()),
        (
            "combining_on_wide_at_edge",
            4,
            2,
            "ab\u{4e2d}\u{301}".as_bytes(),
        ),
        (
            "combining_overwrite_wide_half",
            6,
            2,
            "\u{4e2d}\u{301}\x1b[2Gx".as_bytes(),
        ),
        (
            "emoji_zwj_sequence",
            8,
            2,
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}".as_bytes(),
        ),
        (
            "emoji_variation_selector",
            6,
            2,
            "\u{2764}\u{fe0f}x".as_bytes(),
        ),
        ("emoji_skin_tone", 6, 2, "\u{1f44d}\u{1f3fd}x".as_bytes()),
        (
            "emoji_tag_sequence",
            6,
            2,
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}x".as_bytes(),
        ),
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
        ("erase_characters_default", 6, 2, b"abcdef\x1b[1;3H\x1b[X"),
        ("erase_characters_zero", 6, 2, b"abcdef\x1b[1;3H\x1b[0X"),
        (
            "erase_characters_background",
            8,
            2,
            b"abcdefgh\x1b[1;3H\x1b[1;31;44m\x1b[3X",
        ),
        (
            "erase_characters_clamped",
            6,
            2,
            b"abcdef\r\nghijkl\x1b[1;5H\x1b[99X",
        ),
        (
            "erase_characters_wide",
            8,
            2,
            "a你b好cd\x1b[1;3H\x1b[3X".as_bytes(),
        ),
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
            "sgr_underline_styles",
            12,
            2,
            b"\x1b[4mA\x1b[4:2mB\x1b[4:3mC\x1b[4:4mD\x1b[4:5mE\x1b[21mF\x1b[4:0mG\x1b[4:1mH\x1b[24mI",
        ),
        (
            "sgr_underline_grapheme",
            8,
            2,
            "\x1b[4:3;58:5:1me\u{301}\x1b[4:2m\u{1f469}\u{200d}\u{1f4bb}\x1b[21ma\u{323}\x1b[24mx".as_bytes(),
        ),
        (
            "sgr_underline_color",
            12,
            2,
            b"\x1b[4:3;58:2::255:0:0mA\x1b[58;5;4mB\x1b]8;;https://e\x07C\x1b[59mD\x1b]8;;\x07E\x1b[58:5:2mF\x1b[0mG",
        ),
        (
            "cursor_style_blink",
            6,
            2,
            b"\x1b[5 q",
        ),
        ("cursor_style_steady", 6, 2, b"\x1b[5 q\x1b[?12l\x1b[2 q"),
        ("cursor_style_mode_12", 6, 2, b"\x1b[4 q\x1b[?12h"),
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
        (
            "sgr_extended_color_forms",
            8,
            2,
            b"\x1b[48;2;7;8;9mA\x1b[38:5:100mB\x1b[48:5:17mC\x1b[38:2:10:20:30mD\x1b[48:2:40:50:60mE\x1b[38:2::70:80:90;49mF\x1b[0mG",
        ),
        (
            "sgr_bright_colors",
            10,
            2,
            b"\x1b[90;100mA\x1b[97;107mB\x1b[93;41mC\x1b[32;103mD\x1b[39mE\x1b[49mF\x1b[21mG\x1b[mH",
        ),
        (
            "sgr_blink",
            8,
            2,
            b"\x1b[5mA\x1b[25mB\x1b[6mC\x1b[0mD\x1b[1;5;31mE\x1b[22;25mF\x1b[5m\x1b[mG",
        ),
        (
            "sgr_blink_combining",
            6,
            2,
            "\x1b[5me\u{301}\x1b[25ma\u{308}".as_bytes(),
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
        ("alternate_47_active", 8, 2, b"shell\x1b[?47h\x1b[Halt"),
        (
            "alternate_47_restored",
            8,
            2,
            // Unlike 1049, neither 47 nor 1047 saves/restores the cursor or SGR.
            b"shell\x1b7\x1b[?47h\x1b[2;3H\x1b[32mALT\x1b[?47l!",
        ),
        (
            "alternate_47_reentered",
            8,
            2,
            b"shell\x1b[?47h\x1b[Halt\x1b[?47l\x1b[?47h",
        ),
        (
            "alternate_1047_active",
            8,
            2,
            // 1047 preserves existing alternate contents on entry, like 47.
            b"shell\x1b[?47h\x1b[Halt\x1b[?47l\x1b[?1047h",
        ),
        (
            "alternate_1047_restored",
            8,
            2,
            b"shell\x1b7\x1b[?1047h\x1b[Halt\x1b[2;3H\x1b[32m\x1b[?1047l!",
        ),
        (
            "alternate_1047_cleared",
            8,
            2,
            // Reenter via 47 so a misplaced clear-on-entry cannot hide a bad reset.
            b"shell\x1b[?1047h\x1b[Halt\x1b[?1047l\x1b[?47h",
        ),
        ("alternate_1047_reset_primary", 8, 2, b"shell\x1b[?1047l"),
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
        ("origin_mode_home", 6, 5, b"\x1b[2;4r\x1b[5;6H\x1b[?6h"),
        (
            "origin_mode_cup",
            6,
            5,
            b"1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[?6h\x1b[2;3HX\x1b[99;99HY",
        ),
        (
            "origin_mode_vpa",
            6,
            5,
            b"\x1b[2;4r\x1b[?6h\x1b[2;3HX\x1b[1dY\x1b[99dZ",
        ),
        (
            "origin_mode_relative_clamped",
            6,
            5,
            b"\x1b[2;4r\x1b[?6h\x1b[99Bv\x1b[99A^",
        ),
        (
            "origin_mode_reset_home",
            6,
            5,
            b"\x1b[2;4r\x1b[?6h\x1b[2;3HX\x1b[?6l",
        ),
        (
            "origin_mode_reset_absolute",
            6,
            5,
            b"\x1b[2;4r\x1b[?6h\x1b[2;3HX\x1b[?6l\x1b[5;2HY",
        ),
        (
            "origin_mode_region_home",
            6,
            5,
            b"\x1b[2;5r\x1b[?6h\x1b[4;3HX\x1b[3;4r",
        ),
        (
            "insert_delete_lines",
            4,
            4,
            b"1\r\n2\r\n3\r\n4\x1b[2;1H\x1b[L\x1b[M",
        ),
        ("unsupported_sequences", 6, 2, b"a\x1b[999z\x1b[9999mb"),
        (
            "terminfo_truecolor",
            6,
            2,
            b"\x1b[38;2;1;2;3m\x1b[48;2;4;5;6mab\x1b[39;49mc",
        ),
        (
            "terminfo_styled_underlines",
            8,
            2,
            b"\x1b[4:1ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me\x1b[4:0mf",
        ),
        (
            "sgr_underline_color_ignored",
            8,
            2,
            b"\x1b[1;31m\x1b[58:2::1:2:3ma\x1b[58;2;0;0;0mb\x1b[58;5;4mc\x1b[59md",
        ),
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

fn attributes(t: &Terminal, cell: &Cell) -> String {
    let flags: Vec<_> = [
        (Flags::BOLD, "bold"),
        (Flags::DIM, "dim"),
        (Flags::ITALIC, "italic"),
        (Flags::UNDERLINE, "underline"),
        (Flags::BLINK, "blink"),
        (Flags::INVERSE, "inverse"),
        (Flags::HIDDEN, "hidden"),
        (Flags::STRIKE, "strike"),
        (Flags::WIDE, "wide"),
        (Flags::WIDE_SPACER, "wide-spacer"),
        (Flags::PAD, "pad"),
        (Flags::GRAPHEME, "grapheme"),
    ]
    .into_iter()
    .filter_map(|(flag, name)| match (flag, cell.flags.underline()) {
        (Flags::UNDERLINE, Some(style)) => Some(match style {
            UnderlineStyle::Single => "underline",
            UnderlineStyle::Double => "underline-double",
            UnderlineStyle::Curly => "underline-curly",
            UnderlineStyle::Dotted => "underline-dotted",
            UnderlineStyle::Dashed => "underline-dashed",
        }),
        _ => cell.flags.contains(flag).then_some(name),
    })
    .collect();
    let mut out = format!(
        "fg={} bg={} flags=[{}]",
        color(cell.fg),
        color(cell.bg),
        flags.join(",")
    );
    if let Some(c) = t.underline_color(cell) {
        write!(out, " underline-color={}", color(c)).unwrap();
    }
    out
}

fn rows<'a>(out: &mut String, t: &Terminal, label: &str, rows: impl IntoIterator<Item = &'a Row>) {
    writeln!(out, "{label}:").unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        let mut text = String::new();
        for cell in &row.cells {
            t.push_cell_text(cell, &mut text);
        }
        writeln!(out, "  row {index} wrap={} text={text:?}", row.wrapped()).unwrap();
        let mut start = 0;
        while start < row.cells.len() {
            let attr = attributes(t, &row.cells[start]);
            let mut end = start + 1;
            while end < row.cells.len() && attributes(t, &row.cells[end]) == attr {
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
            CursorShape::Block if t.cursor_blink => "blinking-block",
            CursorShape::Block => "block",
            CursorShape::Underline if t.cursor_blink => "blinking-underline",
            CursorShape::Underline => "underline",
            CursorShape::Bar if t.cursor_blink => "blinking-bar",
            CursorShape::Bar => "bar",
        },
        t.cursor.pending_wrap,
    );
    writeln!(
        out,
        "cursor attributes: {}",
        attributes(t, &t.cursor.template)
    )
    .unwrap();
    writeln!(
        out,
        "scroll_region={}..={} history={} display_offset={}",
        t.scroll_top, t.scroll_bottom, t.history, t.display_offset
    )
    .unwrap();
    rows(&mut out, t, "primary", t.primary.lines.iter());
    rows(
        &mut out,
        t,
        "scrollback (oldest first)",
        t.primary.scrollback.iter(),
    );
    rows(&mut out, t, "alternate", t.alternate.lines.iter());
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
fn golden_keypad_modes() {
    let steps: &[(&str, &[u8])] = &[
        ("default", b""),
        ("DECKPAM", b"\x1b="),
        ("DECCKM set", b"\x1b[?1h"),
        ("DECPNM", b"\x1b>"),
        ("DECKPAM again", b"\x1b="),
        ("DECCKM reset", b"\x1b[?1l"),
        ("RIS", b"\x1bc"),
    ];
    let run = |bytewise: bool| {
        let mut term = Terminal::new(5, 2);
        let mut out = String::new();
        for &(label, bytes) in steps {
            if bytewise {
                for byte in bytes.chunks(1) {
                    term.advance(byte);
                }
            } else {
                term.advance(bytes);
            }
            writeln!(
                out,
                "{label}: app_keypad={} app_cursor={}",
                term.modes.app_keypad, term.modes.app_cursor
            )
            .unwrap();
        }
        out
    };
    let actual = run(false);
    assert_eq!(actual, run(true));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/keypad_modes.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("keypad_modes", &expected, &actual)
        );
    }
}

/// Runs each string the terminfo entry adds that changes modes or the title.
#[test]
fn golden_terminfo_modes() {
    let source = include_str!("../../../term-app/terminfo/xterm-terminal.terminfo");
    let steps: &[(&str, &str, &[u8])] = &[
        ("BE", "BE=\\E[?2004h", b"\x1b[?2004h"),
        ("BD", "BD=\\E[?2004l", b"\x1b[?2004l"),
        (
            "Sync 1",
            "Sync=\\E[?2026%?%p1%{1}%-%tl%eh%;",
            b"\x1b[?2026h",
        ),
        ("Sync 0", "Sync=", b"\x1b[?2026l"),
        ("tsl fsl", "tsl=\\E]2;", b"\x1b]2;vim\x07"),
        ("dsl", "dsl=\\E]2;\\007", b"\x1b]2;\x07"),
    ];
    let run = |bytewise: bool| {
        let mut term = Terminal::new(5, 2);
        let mut out = String::new();
        for &(label, cap, bytes) in steps {
            assert!(source.contains(cap), "terminfo entry lacks {cap}");
            if bytewise {
                for byte in bytes.chunks(1) {
                    term.advance(byte);
                }
            } else {
                term.advance(bytes);
            }
            let title = term.take_title();
            writeln!(
                out,
                "{label}: bracketed_paste={} synchronized_output={} title={title:?} text={:?}",
                term.modes.bracketed_paste,
                term.modes.synchronized_output,
                term.primary.lines[0]
                    .cells
                    .iter()
                    .map(|c| c.c)
                    .collect::<String>(),
            )
            .unwrap();
        }
        out
    };
    let actual = run(false);
    assert_eq!(actual, run(true));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/terminfo_modes.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("terminfo_modes", &expected, &actual)
        );
    }
}

#[test]
fn golden_mouse_modes() {
    let steps: &[(&str, &[u8])] = &[
        ("default", b"\x1b[?9$p"),
        ("X10 set", b"\x1b[?9h\x1b[?9$p\x1b[?1000$p"),
        ("1000 reset keeps X10", b"\x1b[?1000l\x1b[?9$p"),
        ("1000 replaces X10", b"\x1b[?1000h\x1b[?9$p\x1b[?1000$p"),
        ("X10 reset keeps 1000", b"\x1b[?9l\x1b[?1000$p"),
        ("X10 replaces 1000", b"\x1b[?9h\x1b[?1000$p\x1b[?9$p"),
        ("SGR with X10", b"\x1b[?1006h"),
        ("X10 reset", b"\x1b[?9l\x1b[?9$p"),
        ("X10 set again", b"\x1b[?9;1006h"),
        ("RIS", b"\x1bc\x1b[?9$p"),
    ];
    let run = |bytewise: bool| {
        let mut term = Terminal::new(5, 2);
        let mut out = String::new();
        for &(label, bytes) in steps {
            if bytewise {
                for byte in bytes.chunks(1) {
                    term.advance(byte);
                }
            } else {
                term.advance(bytes);
            }
            let replies = String::from_utf8(term.take_responses()).unwrap();
            writeln!(
                out,
                "{label}: mouse={:?} sgr={} replies={replies:?}",
                term.modes.mouse, term.modes.sgr_mouse
            )
            .unwrap();
        }
        out
    };
    let actual = run(false);
    assert_eq!(actual, run(true));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/mouse_modes.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("mouse_modes", &expected, &actual)
        );
    }
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

#[test]
fn golden_command_budget() {
    let mut input = String::new();
    for (name, lines) in [("alpha", 3), ("beta", 3), ("gamma", 3), ("make", 20_000)] {
        input.push_str(&format!(
            "\x1b]133;A\x07$ \x1b]133;B\x07{name}\r\n\x1b]133;C\x07"
        ));
        for i in 0..lines {
            input.push_str(&format!("{name} {i}\r\n"));
        }
        input.push_str("\x1b]133;D;0\x07");
    }
    input.push_str("\x1b]133;A\x07$ \x1b]133;B\x07");
    let run = |bytewise: bool| {
        let mut t = Terminal::new(40, 4);
        t.set_scrollback_limit(100);
        if bytewise {
            for byte in input.as_bytes().chunks(1) {
                t.advance(byte);
            }
        } else {
            t.advance(input.as_bytes());
        }
        snapshot(&t)
    };
    let actual = run(false);
    assert!(actual == run(true), "bytewise differs");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/command_budget.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("command_budget", &expected, &actual)
        );
    }
}
