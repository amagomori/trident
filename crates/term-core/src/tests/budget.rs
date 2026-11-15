use super::super::*;

fn command(name: &str, lines: usize) -> String {
    let mut out = format!("\x1b]133;A\x07$ \x1b]133;B\x07{name}\r\n\x1b]133;C\x07");
    for i in 0..lines {
        out.push_str(&format!("{name} {i}\r\n"));
    }
    out.push_str("\x1b]133;D;0\x07");
    out
}

const PROMPT: &str = "\x1b]133;A\x07$ \x1b]133;B\x07";

/// Every retained line, oldest first, from scrollback through the screen.
fn lines(t: &Terminal) -> Vec<String> {
    (t.first_line()..t.history + t.rows as u64)
        .map(|line| t.line(line).text().trim_end().to_string())
        .collect()
}

fn line_of(t: &Terminal, text: &str) -> u64 {
    let index = lines(t).iter().position(|l| l == text).unwrap();
    t.first_line() + index as u64
}

fn three_then_make(t: &mut Terminal, make: usize) {
    for name in ["alpha", "beta", "gamma"] {
        t.advance(command(name, 3).as_bytes());
    }
    t.advance(command("make", make).as_bytes());
    t.advance(PROMPT.as_bytes());
}

#[test]
fn big_command_keeps_earlier_commands_head_and_tail() {
    let mut t = Terminal::new(40, 5);
    three_then_make(&mut t, 20_000);
    let lines = lines(&t);
    let earlier: Vec<&str> = lines[..12].iter().map(String::as_str).collect();
    assert_eq!(
        earlier,
        [
            "$ alpha", "alpha 0", "alpha 1", "alpha 2", "$ beta", "beta 0", "beta 1", "beta 2",
            "$ gamma", "gamma 0", "gamma 1", "gamma 2",
        ]
    );
    assert_eq!(lines[12], "$ make");
    let placeholders: Vec<usize> = (0..lines.len())
        .filter(|&i| lines[i].starts_with('\u{2026}'))
        .collect();
    assert_eq!(placeholders.len(), 1, "{lines:?}");
    let at = placeholders[0];
    // Head: the first quarter of the 5,000-line budget, from the `C` row on.
    assert_eq!(at, 13 + 1250);
    for (i, line) in lines[13..at].iter().enumerate() {
        assert_eq!(*line, format!("make {i}"));
    }
    let tail_start: usize = lines[at + 1]
        .strip_prefix("make ")
        .unwrap()
        .parse()
        .unwrap();
    let tail: Vec<&String> = lines[at + 1..]
        .iter()
        .take_while(|l| l.starts_with("make "))
        .collect();
    for (i, line) in tail.iter().enumerate() {
        assert_eq!(**line, format!("make {}", tail_start + i));
    }
    assert_eq!(*tail.last().unwrap(), "make 19999");
    let dropped = tail_start - (at - 13);
    assert_eq!(
        lines[at],
        format!(
            "\u{2026} {} lines of make output dropped",
            budget_tests::grouped(dropped)
        )
    );
    assert_eq!(lines[at + 1 + tail.len()], "$");
    assert!(t.scrollback_len() <= DEFAULT_SCROLLBACK);
    assert!(lines.len() - 12 <= 5_000 + 5 + 1, "{}", lines.len());
}

mod budget_tests {
    pub fn grouped(n: usize) -> String {
        let digits = n.to_string();
        let mut out = String::new();
        for (i, d) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                out.push(',');
            }
            out.push(d);
        }
        out
    }
}

#[test]
fn disabled_or_unmarked_output_stays_fifo() {
    let mut off = Terminal::new(40, 5);
    off.set_command_budget(false, 0.5);
    three_then_make(&mut off, 20_000);
    let mut plain = Terminal::new(40, 5);
    let mut input = String::new();
    for name in ["alpha", "beta", "gamma"] {
        input.push_str(&format!("$ {name}\r\n"));
        for i in 0..3 {
            input.push_str(&format!("{name} {i}\r\n"));
        }
    }
    input.push_str("$ make\r\n");
    for i in 0..20_000 {
        input.push_str(&format!("make {i}\r\n"));
    }
    plain.advance(input.as_bytes());
    for t in [&off, &plain] {
        let lines = lines(t);
        assert_eq!(t.scrollback_len(), DEFAULT_SCROLLBACK);
        // The screen holds the last four lines and the cursor row.
        assert_eq!(
            lines[0],
            format!("make {}", 20_000 - 4 - DEFAULT_SCROLLBACK)
        );
        assert!(
            !lines
                .iter()
                .any(|l| l.contains("alpha") || l.contains('\u{2026}'))
        );
    }
}

#[test]
fn hot_reload_changes_the_share_and_turns_it_off() {
    let mut t = Terminal::new(40, 5);
    t.set_scrollback_limit(1_000);
    t.set_command_budget(true, 0.2);
    assert_eq!(t.command_budget(), (true, 0.2));
    three_then_make(&mut t, 3_000);
    let first = lines(&t);
    assert_eq!(first[0], "$ alpha");
    let at = first
        .iter()
        .position(|l| l.starts_with('\u{2026}'))
        .unwrap();
    assert_eq!(at, 13 + 50);
    let make = first.iter().filter(|l| l.starts_with("make ")).count();
    assert!(make <= 200, "{make}");

    t.set_command_budget(false, 0.2);
    t.advance(command("late", 2_000).as_bytes());
    let after = lines(&t);
    assert!(
        !after
            .iter()
            .any(|l| l.contains("alpha") || l.contains("make"))
    );
    assert_eq!(t.scrollback_len(), 1_000);
}

#[test]
fn finished_big_command_gives_up_rows_before_earlier_commands() {
    let mut t = Terminal::new(40, 5);
    t.set_scrollback_limit(1_000);
    t.advance(command("alpha", 3).as_bytes());
    t.advance(command("big", 900).as_bytes());
    for _ in 0..10 {
        t.advance(command("small", 50).as_bytes());
    }
    t.advance(PROMPT.as_bytes());
    let lines = lines(&t);
    assert_eq!(lines[..4], ["$ alpha", "alpha 0", "alpha 1", "alpha 2"]);
    assert_eq!(lines[4], "$ big");
    assert_eq!(lines[5], "big 0");
    let gap = lines
        .iter()
        .position(|l| l.starts_with('\u{2026}'))
        .unwrap();
    assert!(lines[gap].ends_with("lines of big output dropped"));
    assert_eq!(
        lines[gap + 1..].iter().filter(|l| *l == "$ small").count(),
        10
    );
    assert!(lines.contains(&"big 899".to_string()));
}

fn equivalent(prefix: &str, batch: &str, ghosted: bool) {
    let mut batched = Terminal::new(30, 4);
    let mut bytes = Terminal::new(30, 4);
    for t in [&mut batched, &mut bytes] {
        t.set_scrollback_limit(1_000);
    }
    batched.advance(prefix.as_bytes());
    let before = crate::ghost::ADVANCES.get();
    batched.advance(batch.as_bytes());
    assert_eq!(crate::ghost::ADVANCES.get() - before, usize::from(ghosted));
    batched.advance(PROMPT.as_bytes());
    for b in prefix.bytes().chain(batch.bytes()).chain(PROMPT.bytes()) {
        bytes.advance(&[b]);
    }
    assert_eq!(lines(&batched), lines(&bytes));
    assert_eq!(batched.history, bytes.history);
    assert_eq!(batched.cursor_position(), bytes.cursor_position());
    assert!(lines(&batched).iter().any(|l| l.starts_with('\u{2026}')));
    assert_eq!(lines(&batched)[0], "$ alpha");
}

fn output(from: usize, to: usize) -> String {
    (from..to).map(|i| format!("out {i}\r\n")).collect()
}

#[test]
fn batched_matches_byte_at_a_time_with_marks() {
    let mut start = command("alpha", 3);
    start.push_str("\x1b]133;A\x07$ \x1b]133;B\x07cat\r\n\x1b]133;C\x07");
    // Head already in scrollback: the batch skips rows, as without marks.
    equivalent(
        &(start.clone() + &output(0, 400)),
        &output(400, 4_000),
        true,
    );
    // Already split once.
    equivalent(
        &(start.clone() + &output(0, 1_500)),
        &output(1_500, 5_000),
        true,
    );
    // The head is still on screen, so the batch has to build every row.
    equivalent(&start, &output(0, 4_000), false);
}

#[test]
fn search_finds_matches_on_both_sides_of_the_gap() {
    let mut t = Terminal::new(40, 5);
    three_then_make(&mut t, 20_000);
    let found = |t: &Terminal, query: &str| -> Vec<String> {
        t.search(&format!("^{query}$"), true)
            .unwrap()
            .iter()
            .map(|m| t.line(m.start.line).text().trim_end().to_string())
            .collect()
    };
    assert_eq!(found(&t, "alpha 1"), ["alpha 1"]);
    assert_eq!(found(&t, "make 1249"), ["make 1249"]);
    assert_eq!(found(&t, "make 19999"), ["make 19999"]);
    assert_eq!(found(&t, "make 5000").len(), 0);
    assert_eq!(found(&t, ".* output dropped").len(), 1);
    let regex = t.search(r"make \d+$", true).unwrap();
    let mut last = 0;
    for m in &regex {
        assert!(m.start.line >= last);
        last = m.start.line;
    }
    assert_eq!(
        regex.len(),
        lines(&t).iter().filter(|l| l.starts_with("make ")).count()
    );
}

#[test]
fn selection_across_the_gap_copies_both_sides_and_the_placeholder() {
    let mut t = Terminal::new(40, 5);
    three_then_make(&mut t, 20_000);
    let start = line_of(&t, "make 1248");
    let gap = start + 2;
    let end = gap + 2;
    t.start_selection(
        Point {
            line: start,
            col: 0,
        },
        SelectionKind::Simple,
    );
    t.update_selection(Point { line: end, col: 39 });
    let text = t.selection_text().unwrap();
    let copied: Vec<&str> = text.lines().collect();
    assert_eq!(copied.len(), 5);
    assert_eq!(copied[..2], ["make 1248", "make 1249"]);
    assert!(copied[2].starts_with('\u{2026}'));
    assert_eq!(
        copied[3..].join("\n"),
        lines(&t)[(gap + 1 - t.first_line()) as usize..][..2].join("\n")
    );
}

#[test]
fn selection_and_viewport_stay_on_their_text_while_the_gap_grows() {
    let mut t = Terminal::new(40, 5);
    for name in ["alpha", "beta", "gamma"] {
        t.advance(command(name, 3).as_bytes());
    }
    t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07make\r\n\x1b]133;C\x07");
    t.advance(output(0, 12_000).as_bytes());
    let head = line_of(&t, "out 100");
    t.start_selection(Point { line: head, col: 0 }, SelectionKind::Line);
    let beta = line_of(&t, "beta 0");
    t.scroll_to_line(beta);
    let top = t.viewport_row(0).text();
    t.advance(output(12_000, 20_000).as_bytes());
    assert_eq!(t.selection_text().unwrap().trim_end(), "out 100");
    assert_eq!(t.viewport_row(0).text(), top);
}

#[test]
fn reset_keeps_budget_settings() {
    let mut t = Terminal::new(40, 5);
    t.set_command_budget(false, 0.25);
    t.advance(b"\x1bc");
    assert_eq!(t.command_budget(), (false, 0.25));
}

#[test]
fn shrinking_the_window_trims_the_running_command() {
    let mut t = Terminal::new(40, 30);
    t.set_scrollback_limit(100);
    t.set_command_budget(true, 0.8);
    for name in ["alpha", "beta", "gamma"] {
        t.advance(command(name, 3).as_bytes());
    }
    let mut running = format!("{PROMPT}make\r\n\x1b]133;C\x07");
    for i in 0..2000 {
        running.push_str(&format!("make {i}\r\n"));
    }
    t.advance(running.as_bytes());
    t.resize(30, 5);
    let lines = lines(&t);
    assert_eq!(lines[0], "$ alpha", "{lines:?}");
    assert_eq!(lines[8], "$ gamma");
    assert_eq!(lines[12], "$ make");
    let placeholders: Vec<&String> = lines.iter().filter(|l| l.starts_with('\u{2026}')).collect();
    assert_eq!(placeholders.len(), 1, "{lines:?}");
    assert!(lines.contains(&"make 1999".to_string()));
}

#[test]
fn inserted_lines_move_the_output_start() {
    let mut t = Terminal::new(40, 30);
    t.set_scrollback_limit(32);
    t.advance(format!("{PROMPT}make\r\n\x1b]133;C\x07").as_bytes());
    // Push the prompt and the output start down ten rows, then write below them.
    t.advance(b"\x1b[H\x1b[10L\x1b[12;1H");
    let mut out = String::new();
    for i in 0..200 {
        out.push_str(&format!("make {i}\r\n"));
    }
    out.push_str("\x1b]133;D;0\x07");
    out.push_str(PROMPT);
    t.advance(out.as_bytes());
    let lines = lines(&t);
    let at = lines
        .iter()
        .position(|l| l == "$ make")
        .expect("prompt kept");
    assert_eq!(lines[at + 1], "make 0", "{lines:?}");
    let placeholders = lines.iter().filter(|l| l.starts_with('\u{2026}')).count();
    assert_eq!(placeholders, 1, "{lines:?}");
}

#[test]
fn next_prompt_above_the_moved_output_start() {
    let mut t = Terminal::new(40, 30);
    t.set_scrollback_limit(32);
    t.advance(format!("{PROMPT}make\r\n\x1b]133;C\x07").as_bytes());
    // Lines inserted at the output start push it below where the next prompt is drawn.
    t.advance(b"\x1b[2;1H\x1b[10L\x1b]133;D;0\x07");
    t.advance(PROMPT.as_bytes());
    for i in 0..100 {
        t.advance(format!("line {i}\r\n").as_bytes());
    }
    assert!(lines(&t).contains(&"line 99".to_string()));
}
