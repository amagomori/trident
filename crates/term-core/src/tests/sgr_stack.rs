use super::*;

fn assert_style(t: &Terminal, col: usize, sgr: &str) {
    let mut reference = Terminal::new(2, 1);
    reference.advance(format!("\x1b[{sgr}mX").as_bytes());
    let expected = &reference.viewport_row(0).cells[0];
    let actual = &t.viewport_row(0).cells[col];
    assert_eq!(actual.c, 'X');
    assert_eq!(
        (
            actual.fg,
            actual.bg,
            actual.flags,
            t.underline_color(actual)
        ),
        (
            expected.fg,
            expected.bg,
            expected.flags,
            reference.underline_color(expected)
        ),
        "expected SGR {sgr} at column {col}"
    );
}

#[test]
fn nested_restores_change_rendition_only() {
    let mut t = Terminal::new(12, 2);
    let outer = "38;5;172;48;2;1;2;3;1;2;3;4:3;5;7;8;9;58:5:42";
    let inner = "38;2;7;8;9;44;21;58:2::4:5:6";
    t.advance(format!("\x1b[{outer}m\x1b]8;;https://outer\x07\x1b[1\"q\x1b[#{{").as_bytes());
    t.advance(format!("\x1b[0;{inner}m\x1b[#{{\x1b[0mX").as_bytes());
    t.advance(b"\x1b]8;;https://current\x07\x1b[2\"q\x1b[4G\x1b[#}X\x1b[#}X");
    assert_style(&t, 0, "0");
    assert_style(&t, 3, inner);
    assert_style(&t, 4, outer);
    for cell in &t.viewport_row(0).cells[3..5] {
        assert_eq!(t.links.get(cell.hyperlink).unwrap().1, b"https://current");
        assert_eq!(
            t.links.protection(cell.hyperlink),
            links::Protection::default()
        );
    }
    assert_eq!(t.cursor_position(), Some((0, 5)));
}

#[test]
fn selective_pop_changes_only_the_named_attributes() {
    let saved = "1;3;5;7;8;9;4:3;31;42;58:5:17";
    let live = "2;21;34;45;58:5:18";
    for (selectors, expected) in [
        ("30", "2;21;31;45;58:5:18"),
        ("31", "2;21;34;42;58:5:18"),
        ("1", "1;2;21;34;45;58:5:18"),
        ("2;4;30;31", "4:3;31;42;58:5:18"),
        ("3;5;7;8;9;999", "2;3;5;7;8;9;21;34;45;58:5:18"),
        ("21", "2;34;45;58:5:18"),
        ("4;21", "2;4:3;34;45;58:5:18"),
    ] {
        let mut t = Terminal::new(2, 1);
        t.advance(format!("\x1b[{saved}m\x1b[{selectors}#{{\x1b[0;{live}m\x1b[#}}X").as_bytes());
        assert_style(&t, 0, expected);
    }
    for (saved, selector, live, expected) in [
        ("21", 21, "4:3", "21"),
        ("21", 4, "21", "21"),
        ("21", 4, "4:5", "0"),
        ("4", 21, "4:5", "4:5"),
    ] {
        let mut t = Terminal::new(2, 1);
        t.advance(format!("\x1b[{saved}m\x1b[{selector}#{{\x1b[{live}m\x1b[#}}X").as_bytes());
        assert_style(&t, 0, expected);
    }
}

#[test]
fn unsupported_selection_has_its_own_empty_frame() {
    let mut t = Terminal::new(4, 1);
    t.advance(b"\x1b[31m\x1b[30#{\x1b[32m\x1b[32;999#{\x1b[33m\x1b[#}X\x1b[#}X");
    assert_style(&t, 0, "33");
    assert_style(&t, 1, "31");
    t.advance(b"\x1b[34m\x1b[#}X");
    assert_style(&t, 2, "34");
}

#[test]
fn only_ten_most_recent_pushes_survive() {
    let mut t = Terminal::new(12, 1);
    for color in 0..12 {
        t.advance(format!("\x1b[38;5;{color}m\x1b[#{{").as_bytes());
    }
    for col in 0..12 {
        t.advance(b"\x1b[39m\x1b[#}X");
        if col < 10 {
            assert_style(&t, col, &format!("38;5;{}", 11 - col));
        } else {
            assert_style(&t, col, "39");
        }
    }
}

#[test]
fn screens_share_the_stack_but_ris_clears_it() {
    let mut t = Terminal::new(4, 1);
    t.advance(b"\x1b[31m\x1b[#{\x1b[?1049h\x1b[32m\x1b[#{\x1b[?1049l\x1b[#}X\x1b[#}X");
    assert_style(&t, 0, "32");
    assert_style(&t, 1, "31");
    t.advance(b"\x1b[#{\x1bc\x1b[34m\x1b[#}X");
    assert_style(&t, 0, "34");
}
