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
fn last_command_output_is_latest_nonempty_output_wherever_the_view_is() {
    let mut t = Terminal::new(20, 3);
    assert_eq!(t.last_command_output(), None);
    t.advance(b"\x1b]133;A\x07$ seq 1 9\r\n\x1b]133;C\x07");
    for n in 1..=9 {
        t.advance(format!("{n}\r\n").as_bytes());
    }
    t.advance(b"\x1b]133;D;0\x07\x1b]133;A\x07$ true\r\n\x1b]133;C\x07\x1b]133;D;0\x07");
    t.advance(b"\x1b]133;A\x07$ ");
    let nine = (1..=9)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(t.last_command_output().as_deref(), Some(nine.as_str()));
    t.advance(b"echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    t.scroll_to_line(0);
    assert_eq!(t.last_command_output().as_deref(), Some("hi"));
    assert!(t.selection.is_none());
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some(nine.as_str()));
    t.advance(b"\x1b[?1049h");
    assert_eq!(t.last_command_output(), None);
    t.advance(b"\x1b[?1049l");
    assert_eq!(t.last_command_output().as_deref(), Some("hi"));
}

#[test]
fn last_command_output_keeps_styles_as_sgr() {
    let mut t = Terminal::new(40, 4);
    t.advance(b"\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07");
    t.advance(b"\x1b[1;31mred\x1b[0m plain \x1b[38;5;200mx\x1b[48;2;1;2;3my\x1b[m\r\n");
    t.advance(b"\x1b[3;94;102mbright\r\nnext\x1b[m\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ ");
    assert_eq!(
        t.last_command_output().as_deref(),
        Some(concat!(
            "\x1b[0;1;31mred\x1b[0m plain \x1b[0;38;5;200mx\x1b[0;38;5;200;48;2;1;2;3my\x1b[0m\n",
            "\x1b[0;3;94;102mbright\x1b[0m\n",
            "\x1b[0;3;94;102mnext\x1b[0m",
        ))
    );
}

#[test]
fn last_command_output_joins_soft_wraps_and_stops_at_a_partial_line() {
    let mut t = Terminal::new(10, 4);
    t.advance(
        "\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07日本語abcdefgh\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ "
            .as_bytes(),
    );
    assert_eq!(t.last_command_output().as_deref(), Some("日本語abcdefgh"));
    let mut t = Terminal::new(20, 3);
    t.advance("\x1b]133;A\x07$ cmd\r\n\x1b]133;C\x07abc\x1b]133;D;0\x07%                    \r\x1b]133;A\x07$ ".as_bytes());
    assert_eq!(t.last_command_output().as_deref(), Some("abc"));
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
fn shell_marks_follow_reflow() {
    let rows = |t: &Terminal| -> Vec<_> {
        t.primary
            .scrollback
            .iter()
            .chain(&t.primary.lines)
            .map(|r| (r.text(), r.flags, r.prompt_col, r.output_col))
            .collect()
    };
    let mut t = Terminal::new(20, 6);
    t.advance(
        b"\x1b]133;A\x07$ \x1b]133;B\x07echo 0123456789abcdef\x1b]133;C\x07\r\n\
          out-line-that-wraps-once\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07",
    );
    let original = rows(&t);
    t.resize(8, 6);
    let first = t.first_line();
    let prompt: Vec<_> = (first..first + 3).map(|l| t.line(l).flags).collect();
    assert!(prompt.iter().all(|f| f.contains(RowFlags::PROMPT)));
    assert!(prompt[0].contains(RowFlags::PROMPT_START) && prompt[0].contains(RowFlags::SUCCEEDED));
    assert!(!prompt[1].contains(RowFlags::PROMPT_START));
    assert!(!t.line(first + 3).flags.contains(RowFlags::PROMPT));
    let command = &t.shell.commands[0];
    assert_eq!(
        (command.prompt_line, command.input_line, command.input_col),
        (first, first, 2)
    );
    let next = command.next_prompt.unwrap();
    assert!(t.line(next).flags.contains(RowFlags::PROMPT_START));
    assert_eq!(t.line(next).text(), "$");
    assert_eq!(t.cursor_position(), Some((t.rows() - 1, 2)));
    t.select_command_output();
    assert_eq!(
        t.selection_text().as_deref(),
        Some("out-line-that-wraps-once")
    );
    t.jump_to_prompt(true);
    assert_eq!(t.display_offset() as u64, t.history - first);

    t.resize(20, 6);
    assert_eq!(rows(&t), original);
    t.select_command_output();
    assert_eq!(
        t.selection_text().as_deref(),
        Some("out-line-that-wraps-once")
    );
}

#[test]
fn shell_output_start_follows_reflow() {
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ run\x1b]133;B\x07\x1b]133;C\x07result\x1b]133;D;0\x07");
    t.select_command_output();
    assert_eq!(t.selection_text().as_deref(), Some("result"));
    t.resize(4, 5);
    t.select_command_output();
    let (start, _) = t.selection_range().unwrap();
    assert_eq!(start, Point { line: 1, col: 1 });
    assert_eq!(t.selection_text().as_deref(), Some("result"));

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]133;A\x07$ run\x1b]133;C\x07result\x1b]133;D;0\x07");
    t.resize(8, 3);
    t.select_command_output();
    assert_eq!(t.selection_range().unwrap().0, Point { line: 0, col: 5 });
    assert_eq!(t.selection_text().as_deref(), Some("result"));
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
        ("reverse_wrap", 4, 3, b"abcdef\r\x1b[?45h\x08X"),
        ("reverse_wrap_unwrapped", 4, 3, b"\x1b[?45hab\x1b[2;1H\x08X"),
        ("reverse_wrap_ext", 4, 3, b"\x1b[?1045hab\x1b[2;1H\x08X"),
        (
            "reverse_wrap_ext_region",
            5,
            4,
            b"\x1b[?1045h\x1b[2;3r\x1b[2;1H\x08X",
        ),
        ("reverse_wrap_cub", 4, 3, b"\x1b[?45habcdef\x1b[5DX"),
        ("reverse_wrap_pending", 4, 2, b"abcd\x1b[?45h\x08X"),
        ("cub_wrap_pending", 4, 3, b"abcd\x1b[2DX"),
        ("decawm_reset_clears_wrap", 4, 3, b"abcd\x1b[?7l\x08X"),
        (
            "reverse_wrap_requires_autowrap",
            4,
            3,
            b"\x1b[?7l\x1b[?45h\x1b[2;1H\x08X",
        ),
        (
            "reverse_wrap_pending_autowrap_off",
            4,
            3,
            b"\x1b[?7l\x1b[?45habcd\x08X",
        ),
        (
            "reverse_wrap_ext_pending_autowrap_off",
            4,
            3,
            b"\x1b[?7l\x1b[?1045habcd\x08X",
        ),
        (
            "autowrap_off_combining_last_col",
            4,
            3,
            "\x1b[?7labcd\u{301}".as_bytes(),
        ),
        ("more_fix_tab", 12, 3, b"\x1b[?41habcdefghijkl\tX"),
        ("more_fix_tab_off", 12, 3, b"abcdefghijkl\tX"),
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
            "erase_characters_wide_left_half_background",
            6,
            1,
            "\x1b[41m你好x\x1b[44m\x1b[1;1H\x1b[X".as_bytes(),
        ),
        (
            "erase_characters_wide_split_background",
            8,
            1,
            "\x1b[41ma你b好cd\x1b[44m\x1b[1;3H\x1b[3X".as_bytes(),
        ),
        (
            "erase_line_wide_split_background",
            8,
            2,
            "\x1b[41ma你b好cd\r\na你b好cd\x1b[44m\x1b[1;3H\x1b[K\x1b[2;5H\x1b[1K".as_bytes(),
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
            "sgr_underline_color_forms",
            8,
            2,
            b"\x1b[1;31m\x1b[58:2::1:2:3ma\x1b[58;2;0;0;0mb\x1b[58;5;4mc\x1b[59md",
        ),
        ("invalid_utf8", 6, 2, b"a\xffb\r\nZ"),
        (
            "osc_window_title",
            6,
            2,
            b"a\x1b]2;first\x07b\x1b]0;second;title\x1b\\c",
        ),
        (
            "osc8_hyperlink",
            8,
            2,
            b"a\x1b]8;;https://example.com\x07bc\x1b]8;;\x07d\x1b]8;id=x;file:///tmp\x1b\\e\x1b]8;;\x1b\\f",
        ),
        (
            "osc52_clipboard_store",
            6,
            2,
            b"a\x1b]52;c;aGVsbG8=\x07b\x1b]52;p;d29ybGQ\x1b\\c",
        ),
        (
            "osc52_clipboard_query",
            6,
            2,
            b"a\x1b]52;c;?\x07b\x1b]52;q;aGk=\x07\x1b]52;c;!!\x07c",
        ),
        (
            "osc7_working_directory",
            6,
            2,
            b"a\x1b]7;file://box/home/me/my%20dir\x07b\x1b]7;bogus\x1b\\c",
        ),
        (
            "decaln_homes_and_resets_margins",
            4,
            3,
            b"\x1b[2;3r\x1b[2;3H\x1b#8\x1b[3BX",
        ),
        (
            "decstr_resets_saved_cursor",
            6,
            2,
            b"a\x1b7\x1b(0\x1b[!pb\x1b8cq",
        ),
        (
            "uk_charset_pound",
            7,
            2,
            b"\x1b(A#a\x1b)A\x0e#\x0f\x1b(B#",
        ),
        (
            "xtsave_restores_autowrap",
            4,
            2,
            b"\x1b[?7s\x1b[?7l\x1b[?7rabcdef",
        ),
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
    type Reflow = (&'static str, usize, usize, &'static [u8], (usize, usize));
    let reflows: &[Reflow] = &[
        ("reflow_narrow", 8, 3, b"0123456789\r\n$ ", (4, 3)),
        ("reflow_widen", 4, 4, b"0123456789\r\n$ ", (8, 4)),
        ("reflow_pending_wrap", 8, 2, b"abcdefgh", (4, 2)),
        ("reflow_wide_char", 5, 3, "ab\u{754c}cd".as_bytes(), (3, 3)),
        (
            "reflow_styled",
            6,
            3,
            b"\x1b[1;31mabc\x1b[42mdefgh\x1b[m!",
            (4, 3),
        ),
        (
            "reflow_alternate",
            8,
            3,
            b"0123456789\x1b[?1049hfullscreen app",
            (4, 3),
        ),
    ];
    for &(name, cols, rows, input, resize) in reflows {
        cases.push(Case {
            name,
            cols,
            rows,
            input,
            resize: Some(resize),
        });
    }
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
    if t.links
        .get(cell.hyperlink)
        .is_some_and(|(_, uri)| !uri.is_empty())
    {
        write!(out, " link={}", cell.hyperlink).unwrap();
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
    if let Some(title) = &t.title {
        writeln!(out, "title: {title:?}").unwrap();
    }
    for index in 1..=t.links.len() as u16 {
        if let Some((id, uri)) = t.links.get(index).filter(|(_, uri)| !uri.is_empty()) {
            let (id, uri) = (String::from_utf8_lossy(id), String::from_utf8_lossy(uri));
            writeln!(out, "link {index}: id={id:?} uri={uri:?}").unwrap();
        }
    }
    for request in &t.shell.clipboard {
        writeln!(out, "clipboard request: {request:?}").unwrap();
    }
    if let Some(cwd) = t.working_directory() {
        writeln!(
            out,
            "working directory: host={:?} path={:?}",
            cwd.host, cwd.path
        )
        .unwrap();
    }
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
    let source = include_str!("../../../term-app/terminfo/xterm-trident.terminfo");
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
fn golden_conformance_reports() {
    let steps: &[(&str, &[u8])] = &[
        ("DECRQCRA off by default", b"AB\x1b[1*y"),
        ("enable checksums", b""),
        ("DECRQCRA one cell", b"\x1b[1;0;1;1;1;1*y"),
        ("DECRQCRA blank cell", b"\x1b[2;0;2;1;2;1*y"),
        ("DECRQCRA bold A", b"\x1b[H\x1b[1mA\x1b[m\x1b[3;0;1;1;1;1*y"),
        ("DECRQCRA whole screen", b"\x1b[4*y"),
        (
            "DECRQCRA origin mode",
            b"\x1b[2;3r\x1b[?6h\x1b[HZ\x1b[5;0;1;1;1;1*y\x1b[?6l\x1b[r",
        ),
        ("DECRQCRA clamps", b"\x1b[6;0;9;9;99;99*y"),
        ("DECXCPR", b"\x1b[2;3H\x1b[?6n"),
        ("printer, UDK, keyboard", b"\x1b[?15n\x1b[?25n\x1b[?26n"),
        ("locator", b"\x1b[?53n\x1b[?55n\x1b[?56n"),
        ("macros, DECCKSR", b"\x1b[?62n\x1b[?63;7n"),
        ("integrity, sessions", b"\x1b[?75n\x1b[?85n"),
        ("DECID", b"\x1bZ"),
        (
            "DECRQM IRM, LNM",
            b"\x1b[4$p\x1b[4h\x1b[4$p\x1b[4l\x1b[20$p",
        ),
        (
            "DECRQM fixed ANSI modes",
            b"\x1b[12$p\x1b[2$p\x1b[19$p\x1b[99$p",
        ),
        (
            "DECRQM stored DEC modes",
            b"\x1b[?3;4;5;18;19;40;41;42;45;66;67;1045h\x1b[?3$p\x1b[?4$p\x1b[?5$p\x1b[?18$p\x1b[?19$p\x1b[?40$p\x1b[?41$p\x1b[?42$p\x1b[?45$p\x1b[?66$p\x1b[?67$p\x1b[?1045$p\x1b[?3;4;5;18;19;40;41;42;45;66;67;1045l\x1b[?3$p\x1b[?45$p",
        ),
        (
            "DECCOLM needs mode 40",
            b"\x1b[?3h\x1b[?3$p\x1b[?40h\x1b[?3h\x1b[?3$p\x1b[?40l\x1b[?3l",
        ),
        ("DECRQM KAM", b"\x1b[2h\x1b[2$p\x1b[2l\x1b[2$p"),
        ("DECRQSS SGR default", b"\x1bP$qm\x1b\\"),
        (
            "DECRQSS SGR styled",
            b"\x1b[1;3;4:3;7;31;104m\x1bP$qm\x1b\\\x1b[38;5;200;48;2;1;2;3m\x1bP$qm\x1b\\\x1b[m",
        ),
        (
            "DECRQSS SGR underline color",
            b"\x1b[4;58:5:9m\x1bP$qm\x1b\\\x1b[58:2::10:20:30m\x1bP$qm\x1b\\\x1b[m",
        ),
        (
            "DECRQSS DECSTBM",
            b"\x1b[2;3r\x1bP$qr\x1b\\\x1b[r\x1bP$qr\x1b\\",
        ),
        (
            "DECRQSS DECSCUSR",
            b"\x1b[4 q\x1bP$q q\x1b\\\x1b[5 q\x1bP$q q\x1b\\\x1b[0 q",
        ),
        ("DECRQSS DECSCL", b"\x1b[65;1\"p\x1bP$q\"p\x1b\\"),
        (
            "DECRQSS DECSCL after drop",
            b"\x1b[62;2\"p\x1b[64;1\"p\x1bP$q\"p\x1b\\",
        ),
        (
            "DECRQSS stored settings",
            b"\x1b[1\"q\x1bP$q\"q\x1b\\\x1b[2*x\x1bP$q*x\x1b\\\x1b[1$}\x1bP$q$}\x1b\\\x1b[2$~\x1bP$q$~\x1b\\\x1b[24*|\x1bP$q*|\x1b\\\x1b[1+q\x1bP$q+q\x1b\\\x1b[3*}\x1bP$q*}\x1b\\\x1b[2+r\x1bP$q+r\x1b\\",
        ),
        ("DECRQSS unknown", b"\x1bP$q x\x1b\\\x1bP$qtoolong!\x1b\\"),
        (
            "DECSCL below 63 silences DECRQM",
            b"\x1b[62;1\"p\x1b[?25$p\x1b[4$p\x1bP$qm\x1b\\\x1b[64;1\"p\x1b[?25$p\x1b[4$p",
        ),
        (
            "DECSTR resets new modes",
            b"\x1b[?45;1045h\x1b[2h\x1b[1\"q\x1b[1$}\x1b[!p\x1b[?45$p\x1b[?1045$p\x1b[2$p\x1bP$q\"q\x1b\\\x1bP$q$}\x1b\\",
        ),
        (
            "XTSAVE/XTRESTORE",
            b"\x1b[?7;2004;9999s\x1b[?7l\x1b[?2004h\x1b[?7;2004;9999r\x1b[?7$p\x1b[?2004$p",
        ),
        (
            "XTRESTORE unsaved",
            b"\x1b[?25l\x1b[?25r\x1b[?25$p\x1b[?25h",
        ),
        ("RIS keeps checksums", b"\x1bc\x1b[8;0;1;1;1;1*y"),
    ];
    let run = |bytewise: bool| {
        let mut term = Terminal::new(4, 3);
        let mut out = String::new();
        for &(label, bytes) in steps {
            if label == "enable checksums" {
                term.set_checksum_reports(true);
            }
            if bytewise {
                for byte in bytes.chunks(1) {
                    term.advance(byte);
                }
            } else {
                term.advance(bytes);
            }
            let replies = String::from_utf8(term.take_responses()).unwrap();
            writeln!(out, "{label}: {replies:?}").unwrap();
        }
        out
    };
    let actual = run(false);
    assert_eq!(actual, run(true));
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/conformance_reports.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("conformance_reports", &expected, &actual)
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

#[test]
fn desktop_notification_sequences() {
    let n = |title: &str, body: &str| Notification {
        title: title.into(),
        body: body.into(),
        ..Notification::default()
    };
    let cases: Vec<(&[u8], Vec<Notification>)> = vec![
        (b"\x1b]9;Build done\x07", vec![n("", "Build done")]),
        (b"\x1b]9;a;b\x1b\\", vec![n("", "a;b")]),
        (b"\x1b]9;\x07", vec![]),
        (b"\x1b]9;4;1;50\x07\x1b]9;2;box\x07\x1b]9;12\x07", vec![]),
        (b"\x1b]9;13\x07", vec![n("", "13")]),
        (
            b"\x1b]777;notify;Title;Body;more\x07",
            vec![n("Title", "Body;more")],
        ),
        (b"\x1b]777;notify;Title\x07", vec![n("Title", "")]),
        (b"\x1b]777;other;Title;Body\x07", vec![]),
        (b"\x1b]99;;Hello\x1b\\", vec![n("Hello", "")]),
        (
            b"\x1b]99;i=1:d=0;Hel\x1b\\\x1b]99;i=1:d=0;lo\x1b\\\x1b]99;i=1:p=body;World\x1b\\",
            vec![n("Hello", "World")],
        ),
        (
            b"\x1b]99;i=a:d=0;A\x07\x1b]99;i=b;B\x07\x1b]99;i=a:p=body;a\x07",
            vec![n("B", ""), n("A", "a")],
        ),
        (b"\x1b]99;e=1;SGk7dGhlcmU=\x1b\\", vec![n("Hi;there", "")]),
        (b"\x1b]99;e=1;!!\x1b\\", vec![]),
        (
            b"\x1b]99;i=s:d=0:e=1;SGk\x1b\\\x1b]99;i=s:p=body;World\x1b\\",
            vec![n("Hi", "World")],
        ),
        (
            b"\x1b]99;i=s:d=0:e=1;SGk7\x1b\\\x1b]99;i=s:d=0:e=1;dGhl\x1b\\\x1b]99;i=s:e=1;cmU=\x1b\\",
            vec![n("Hi;there", "")],
        ),
        (
            b"\x1b]99;i=s:d=0:e=1;SG\x1b\\\x1b]99;i=s:d=0:e=1;k7dGh\x1b\\\x1b]99;i=s:e=1;lcmU\x1b\\",
            vec![n("Hi;there", "")],
        ),
        (
            b"\x1b]99;i=s:d=0:e=1;w6\x1b\\\x1b]99;i=s:e=1;k=\x1b\\\x1b]99;i=t:d=0;\xc3\x1b\\\x1b]99;i=t;\xa9\x1b\\",
            vec![n("\u{e9}", ""), n("\u{e9}", "")],
        ),
        (b"\x1b]99;p=close;x\x07\x1b]99;p=icon;x\x07", vec![]),
        (b"\x1b]99;;a\x08b\tc\x07", vec![n("abc", "")]),
        (b"\x1b]9;one\x07\x1bc", vec![n("", "one")]),
        (
            b"\x1b]99;i=u:d=0:u=2:o=unfocused;T\x07\x1b]99;i=u;!\x07",
            vec![Notification {
                title: "T!".into(),
                urgency: Urgency::Critical,
                occasion: Occasion::Unfocused,
                ..Notification::default()
            }],
        ),
        (
            b"\x1b]99;u=0:o=invisible;T\x07",
            vec![Notification {
                title: "T".into(),
                urgency: Urgency::Low,
                occasion: Occasion::Invisible,
                ..Notification::default()
            }],
        ),
    ];
    for (input, expected) in cases {
        let mut t = Terminal::new(20, 3);
        t.advance(b"x");
        let before = snapshot(&t);
        t.advance(input);
        if input != b"\x1b]9;one\x07\x1bc" {
            assert_eq!(snapshot(&t), before, "{input:?}");
        }
        let mut taken = t.take_notifications();
        taken.iter_mut().for_each(|n| n.id.clear());
        assert_eq!(taken, expected, "{input:?}");
        assert!(t.take_notifications().is_empty());
        assert!(t.take_responses().is_empty(), "{input:?}");
    }

    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]99;i=rep;v1\x07\x1b]99;i=rep:d=0;v\x07\x1b]99;i=rep;2\x07\x1b]9;x\x07");
    let ids: Vec<_> = t.take_notifications().into_iter().map(|n| n.id).collect();
    assert_eq!(ids, ["rep", "rep", ""]);
}

#[test]
fn desktop_notification_query_and_limits() {
    let mut t = Terminal::new(20, 3);
    t.advance(b"\x1b]99;i=q1:p=?;\x1b\\\x1b]99;p=?\x07");
    assert_eq!(
        String::from_utf8(t.take_responses()).unwrap(),
        "\x1b]99;i=q1:p=?;o=always,unfocused,invisible:p=title,body,?:u=0,1,2\x1b\\\
         \x1b]99;i=:p=?;o=always,unfocused,invisible:p=title,body,?:u=0,1,2\x07"
    );
    assert!(t.take_notifications().is_empty());

    for i in 0..40 {
        t.advance(format!("\x1b]9;{i}x\x07").as_bytes());
    }
    let queued = t.take_notifications();
    assert_eq!(queued.len(), 16);
    assert_eq!(queued[0].body, "0x");
    assert_eq!(queued[15].body, "15x");

    t.advance(format!("\x1b]99;;{}\x07", "a".repeat(5000)).as_bytes());
    assert_eq!(t.take_notifications()[0].title.chars().count(), 1024);
    for _ in 0..10 {
        t.advance(format!("\x1b]99;i=big:d=0;{}\x07", "b".repeat(900)).as_bytes());
    }
    t.advance(b"\x1b]99;i=big;\x07");
    assert_eq!(t.take_notifications()[0].title.len(), 1024);

    for i in 0..20 {
        t.advance(format!("\x1b]99;i=p{i}:d=0;{i}\x07").as_bytes());
    }
    t.advance(b"\x1b]99;i=p0;!\x07\x1b]99;i=p19;!\x07");
    assert_eq!(
        t.take_notifications(),
        vec![
            Notification {
                id: "p0".into(),
                title: "!".into(),
                ..Notification::default()
            },
            Notification {
                id: "p19".into(),
                title: "19!".into(),
                ..Notification::default()
            },
        ]
    );
}

#[test]
fn golden_progress_reports() {
    let steps: &[(&str, &[u8])] = &[
        ("default", b""),
        ("set 42", b"\x1b]9;4;1;42\x07"),
        ("same again", b"\x1b]9;4;1;42\x1b\\"),
        ("set without percent", b"\x1b]9;4;1\x07"),
        ("set 250", b"\x1b]9;4;1;250\x07"),
        ("set 60 first", b"\x1b]9;4;1;60\x07"),
        ("set huge", b"\x1b]9;4;1;99999999999999999999\x07"),
        ("set 60", b"\x1b]9;4;1;60\x07"),
        ("error keeps percent", b"\x1b]9;4;2\x07"),
        ("error 10", b"\x1b]9;4;2;10\x07"),
        ("indeterminate", b"\x1b]9;4;3\x07"),
        ("paused after indeterminate", b"\x1b]9;4;4\x07"),
        ("paused 75", b"\x1b]9;4;4;75\x07"),
        ("unknown state", b"\x1b]9;4;5;20\x07"),
        ("bad percent", b"\x1b]9;4;1;4x\x07"),
        ("bad state", b"\x1b]9;4;-1;20\x07"),
        ("remove", b"\x1b]9;4;0;0\x07"),
        ("remove again", b"\x1b]9;4;0\x07"),
        ("set 5", b"\x1b]9;4;1;5\x07"),
        ("empty state removes", b"\x1b]9;4;\x07"),
        ("set 7", b"\x1b]9;4;1;7\x07"),
        ("bare 9;4 removes", b"\x1b]9;4\x07"),
        ("notification is not progress", b"\x1b]9;build done\x07"),
        ("set 33 amid text", b"a\x1b]9;4;1;33\x07b"),
        ("RIS", b"\x1bc"),
        ("RIS again", b"\x1bc"),
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
            let change = match term.take_progress() {
                Some(progress) => format!("changed to {progress:?}"),
                None => "unchanged".to_string(),
            };
            writeln!(out, "{label}: {change}").unwrap();
        }
        out
    };
    let actual = run(false);
    assert_eq!(actual, run(true));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tests/golden/progress_reports.snap");
    if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
        std::fs::write(path, &actual).unwrap();
    } else {
        let expected = std::fs::read_to_string(path).unwrap();
        assert!(
            expected == actual,
            "{}",
            difference("progress_reports", &expected, &actual)
        );
    }
}

#[test]
fn csi_fast_path_matches_bytewise() {
    let input: &[u8] = b"\x1b[2J\x1b[H\x1b[1;31mred\x1b[0m \x1b[38;5;208mo\x1b[38;2;1;2;3mx\x1b[m\
        \x1b[5;10H\x1b[4:3mcurly\x1b[24m\x1b[?25l\x1b[3;8r\x1b[8;1H\n\n\x1b[r\x1b[2 q\
        \x1b[1;\r2m\x1b[3\x1b[4mcut\x1b[65536;99999m\x1b[;;7m\x1b[K\x1b[2A\x1b[3C\x1b[@\x1b[?1049h\
        \x1b[1;1Halt\x1b[?1049l\x1b[?25h";
    let mut whole = Terminal::new(20, 8);
    whole.advance(input);
    for len in [1, 2, 3, 7] {
        let mut split = Terminal::new(20, 8);
        for chunk in input.chunks(len) {
            split.advance(chunk);
        }
        assert_eq!(snapshot(&whole), snapshot(&split), "chunks of {len}");
    }
}
