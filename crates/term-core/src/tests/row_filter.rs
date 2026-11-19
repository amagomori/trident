use crate::placeholder::{CHAR, DIACRITICS as M};
use crate::{Cell, Image, Row, Terminal, VisiblePlacement};

fn register(t: &mut Terminal, id: u32) {
    t.add_image(
        id,
        Image {
            width: 4,
            height: 2,
            pixels: vec![100; 32],
        },
    )
    .unwrap();
    t.advance(format!("\x1b_Ga=p,U=1,i={id},p=7,c=4,r=2,q=2\x1b\\").as_bytes());
}

fn fixture(cols: usize, rows: usize) -> Terminal {
    let mut t = Terminal::new(cols, rows);
    t.set_cell_pixel_size(1., 1.);
    register(&mut t, 1);
    t.advance(b"\x1b[31m");
    t
}

/// Compare whole ordered placements (including the exact slice), not just image counts.
fn check(t: &Terminal, stage: &str) -> Vec<VisiblePlacement> {
    let mut visible = Vec::new();
    for extra in [1, 0] {
        let mut expected = Vec::new();
        let above = extra.min(t.scrollback_len() - t.display_offset());
        for r in -(above as isize)..t.rows() as isize {
            let line = t.viewport_to_point_signed(r, 0).line;
            t.placeholder_runs(&t.line(line).cells, |tile, col, len| {
                if let Some(v) = t.placeholder_slice(tile, col, len, r, line) {
                    expected.push(v);
                }
            });
        }
        visible = t.visible_placements_above(extra);
        visible.retain(|v| v.slice.is_some());
        assert_eq!(visible, expected, "{stage} +{extra}");
    }
    // Unregistered/malformed bases and the inactive screen need coverage even when no
    // image could be drawn. Inspect the real grapheme base, not the encoded cell index.
    for screen in [&t.primary, &t.alternate] {
        for row in screen.lines.iter().chain(&screen.scrollback) {
            assert!(
                row.may_have_placeholder() || !row.cells.iter().any(|c| t.base_char(c) == CHAR),
                "{stage}: skipped a base"
            );
        }
    }
    visible
}

fn feed(t: &mut Terminal, bytes: &str) -> Vec<VisiblePlacement> {
    t.advance(bytes.as_bytes());
    check(t, bytes)
}

fn pos(v: &[VisiblePlacement]) -> Vec<(isize, usize, usize)> {
    v.iter()
        .map(|v| (v.row, v.placement.col, v.placement.cols))
        .collect()
}

#[test]
fn edits_and_erases_match_a_full_scan() {
    // Start each edit from a registered, painted row, including a grapheme-encoded tile.
    let paint = format!("\x1b[2;4H{CHAR}{}{CHAR}", M[0]);
    for (op, want) in [
        ("\x1b[2;3H\x1b[2@", Some((1, 5, 2))),   // ICH
        ("\x1b[2;2H\x1b[2P", Some((1, 1, 2))),   // DCH
        ("\x1b[2;3H\x1b[4h界", Some((1, 5, 2))), // IRM
        ("\x1b[2;4H界", None),
        ("\x1b[2;4Hxx", None),           // ASCII run
        ("\x1b[2;4Hx", Some((1, 4, 1))), // scalar
        ("\x1b[2;4H\x1b[2X", None),
        ("\x1b[2;5H\x1b[K", Some((1, 3, 1))),
        ("\x1b[2;4H\x1b[1K", Some((1, 4, 1))),
        ("\x1b[2K", None),
        ("\x1b[1;4H\x1b[J", None),
        ("\x1b[3;4H\x1b[1J", None),
        ("\x1b[2J", None),
        ("\x1b#8", None), // DECALN
        ("\x1bc", None),  // RIS
    ] {
        let mut t = fixture(12, 4);
        assert_eq!(pos(&feed(&mut t, &paint)), [(1, 3, 2)]);
        assert_eq!(
            pos(&feed(&mut t, op)),
            want.into_iter().collect::<Vec<_>>(),
            "{op:?}"
        );
        if op == "\x1bc" {
            register(&mut t, 1);
            feed(&mut t, "\x1b[31m");
        }
        feed(&mut t, "\x1b[4l\x1b[2;1H\x1b[2K");
        assert_eq!(pos(&feed(&mut t, &paint)), [(1, 3, 2)]);
    }
}

#[test]
fn late_registration_bad_marks_and_rewrites() {
    let mut t = fixture(12, 4);
    let before = feed(&mut t, &format!("\x1b[32m{CHAR}{}{CHAR}", M[1]));
    assert!(before.is_empty());
    register(&mut t, 2);
    let original = check(&t, "late registration");
    assert_eq!(pos(&original), [(0, 0, 2)]);
    assert_eq!(original[0].slice.unwrap().source, [0., 1., 2., 2.]);
    assert!(feed(&mut t, "\x1b_Ga=d,d=i,i=2,p=7\x1b\\").is_empty());
    feed(&mut t, "\x1b_Ga=p,U=1,i=2,p=7,c=4,r=2,q=2\x1b\\");
    assert_eq!(check(&t, "placement returned"), original);
    feed(&mut t, &format!("\r{CHAR}\u{0301}{CHAR}\u{0301}"));
    assert!(check(&t, "both invalid; repeat layout").is_empty());
    feed(&mut t, "\x1b]133;A\x07\r\x1b]133;B\x07");
    assert_eq!(pos(&feed(&mut t, &format!("{CHAR}\x1b[2b"))), [(0, 0, 3)]); // REP
    feed(&mut t, "\x1b]133;C\x07");
    feed(&mut t, &format!("\x1b[3;12H{CHAR}{CHAR}")); // soft-wrap
    assert_eq!(check(&t, "wrapped").len(), 3);
}

#[test]
fn region_moves_and_recycling() {
    for (op, row) in [
        ("\x1b[2;4r\x1b[S", 1),
        ("\x1b[2;4r\x1b[T", 3),
        ("\x1b[2;4r\x1b[2;1H\x1bM", 3), // RI
        ("\x1b[2;4r\x1b[2;1H\x1b[L", 3),
        ("\x1b[2;4r\x1b[2;1H\x1b[M", 1),
        ("\x1b[1;4r\x1b[S", 1), // top-aligned partial region pushes history
    ] {
        let mut t = fixture(10, 5);
        feed(&mut t, &format!("\x1b[3;1H{CHAR}{}", M[1]));
        assert_eq!(pos(&feed(&mut t, op)), [(row, 0, 1)], "{op:?}");
    }
    for limit in [0, 2, 8] {
        let mut t = fixture(10, 3);
        t.set_scrollback_limit(limit);
        feed(&mut t, &format!("{CHAR}\x1b[3;1H"));
        assert!(feed(&mut t, "\r\n").is_empty());
        t.scroll_display(1);
        assert_eq!(check(&t, "history").len(), usize::from(limit != 0));
        t.scroll_to_bottom();
        for _ in 0..10 {
            assert!(feed(&mut t, "text\r\n").is_empty());
        }
        t.scroll_display(100);
        assert!(check(&t, "fifo").is_empty());
    }
}

#[test]
fn both_screens_hidden_cells_and_wide_pad() {
    let mut t = fixture(10, 3);
    let primary = feed(&mut t, &format!("p{CHAR}"));
    for mode in [47, 1047, 1049] {
        feed(&mut t, &format!("\x1b[?{mode}h\x1b[H\x1b[31m"));
        // A wide glyph straddles the narrowing edge; the marker is in the suffix.
        let alt = feed(&mut t, &format!("\x1b[1;5H界{CHAR}{}", M[1]));
        assert_eq!(pos(&alt), [(0, 6, 1)]);
        t.resize(5, 3);
        assert!(check(&t, "hidden").is_empty());
        assert!(feed(&mut t, "\x1b[1;5Hx").is_empty()); // overwrite the PAD
        t.resize(10, 3);
        assert_eq!(check(&t, "restored past overwritten wide pad"), alt);
        t.resize(5, 3);
        check(&t, "hide again");
        feed(&mut t, "\x1b[?47l");
        t.resize(10, 3); // restore while inactive
        assert_eq!(check(&t, "primary restored"), primary);
        feed(&mut t, "\x1b[?47h");
        assert_eq!(check(&t, "inactive restore"), alt);
        t.resize(5, 3);
        check(&t, "hide before erase");
        feed(&mut t, "\x1b[1;5H\x1b[K"); // discard hidden
        t.resize(10, 3);
        assert!(check(&t, "discarded suffix").is_empty());
        assert_eq!(feed(&mut t, &format!("\x1b[?{mode}l")), primary);
    }
}

#[test]
fn reflow_and_history_restoration() {
    let mut t = fixture(8, 4);
    t.set_scrollback_limit(32);
    let painted = feed(&mut t, &format!("abcde{CHAR}{}xy{CHAR}{}", M[0], M[1]));
    assert_eq!(pos(&painted), [(0, 5, 1), (1, 0, 1)]);
    t.resize(4, 4); // split one logical line across new physical rows
    assert_eq!(pos(&check(&t, "split")), [(1, 1, 1), (2, 0, 1)]);
    t.resize_while_editing(8, 4, &mut []);
    assert_eq!(check(&t, "join"), painted);
    feed(&mut t, "\x1b[4;1H");
    t.resize(8, 2); // height-only shrink, pulling back on grow
    assert!(check(&t, "pushed to history").is_empty());
    t.resize(8, 4);
    assert_eq!(check(&t, "height restored"), painted);
    feed(&mut t, "\x1b[2J");
    t.scroll_display(99);
    assert!(!check(&t, "ED2 saved").is_empty());
    t.scroll_to_bottom();
    feed(&mut t, "\x1b[3J");
    assert_eq!(t.scrollback_len(), 0);
    assert!(check(&t, "ED3").is_empty());
    // A hard history row can stay short; viewport padding clones it, height growth pads it in place.
    feed(&mut t, &format!("\x1b[H{CHAR}\r\n\r\n\r\n\r\n"));
    t.resize(16, 4);
    t.scroll_display(99);
    assert!(!check(&t, "short hard history row").is_empty());
    assert_eq!(t.viewport_row(0).cells.len(), 16);
    assert_eq!(t.line(t.first_line()).cells.len(), 8);
    t.scroll_to_bottom();
    t.resize(16, 5);
    assert_eq!(pos(&check(&t, "padded row pulled back")), [(0, 0, 1)]);
}

#[test]
fn reflow_takes_a_hidden_suffix_after_a_negative_scan() {
    let mut t = fixture(10, 3);
    let p = feed(&mut t, &format!("\x1b[1;8H{CHAR}{}", M[1]));
    assert_eq!(pos(&p), [(0, 7, 1)]);
    t.primary.lines[0].resize(5);
    assert!(check(&t, "narrow stored row").is_empty());
    t.resize(4, 3); // logical-line reconstruction calls take_cells, not Row::resize
    let restored = check(&t, "hidden suffix reflowed");
    assert_eq!(pos(&restored), [(1, 3, 1)]);
    assert_eq!(restored[0].slice, p[0].slice);
}

#[test]
fn negative_hint_is_physical_and_clone_independent() {
    let mut t = fixture(10, 2);
    feed(&mut t, &format!("{CHAR}"));
    let original = t.primary.lines[0].clone();
    t.advance(b"\rx"); // stale positive, until a full scan proves otherwise
    assert!(t.primary.lines[0].may_have_placeholder());
    assert!(check(&t, "last marker overwritten").is_empty());
    assert!(!t.primary.lines[0].may_have_placeholder());
    assert!(original.may_have_placeholder());
    t.primary.lines[1] = original;
    assert_eq!(pos(&check(&t, "restored cloned row")), [(1, 0, 1)]);
}

#[test]
fn budget_gap_and_ghost_rebuilds() {
    let mut t = fixture(40, 4);
    t.set_scrollback_limit(40);
    t.set_command_budget(true, 0.5);
    // The generated gap row contains the first command word, which can itself contain a base.
    feed(
        &mut t,
        &format!("\x1b]133;A\x07$ \x1b]133;B\x07cmd{CHAR}\r\n\x1b]133;C\x07"),
    );
    for i in 0..55 {
        feed(&mut t, &format!("line{i} {CHAR}\r\n"));
    }
    let batch = format!("{}last{CHAR}", "plain\r\n".repeat(100));
    let before = crate::ghost::ADVANCES.get();
    feed(&mut t, &batch);
    assert!(crate::ghost::ADVANCES.get() > before);
    assert!(
        t.gap_rows(t.first_line(), t.scrollback_len())
            .iter()
            .any(|(_, s)| s.contains(CHAR))
    );
    for cols in [40, 28, 48] {
        t.resize(cols, 4);
        for offset in 0..=t.scrollback_len() {
            t.scroll_to_bottom();
            t.scroll_display(offset as isize);
            check(&t, "budget history and resized gap");
        }
    }
    t.set_scrollback_limit(2);
    check(&t, "trim");
}

#[test]
fn exact_occupied_bound_is_independent_of_layout() {
    assert_eq!(std::mem::size_of::<Row>(), 40);
    let mut t = fixture(320, 2);
    for end in 1..=320 {
        // EL2 resets occupancy; printing and then erasing a suffix update it differently.
        feed(&mut t, &format!("\r\x1b[2K\x1b[{end}G{CHAR}"));
        let bound = if end >= 255 { 320 } else { end };
        assert_eq!(t.primary.lines[0].occupied(), bound, "write through {end}");
        feed(&mut t, &format!("\x1b[{end}G\x1b[K"));
        let expected = if end > 255 { 320 } else { end - 1 };
        assert_eq!(
            t.primary.lines[0].occupied(),
            expected,
            "blank from {}",
            end - 1
        );
    }
    let mut row = Row::new(320, &Cell::default());
    row.touch(254);
    assert_eq!(row.occupied(), 254);
    row.touch_all();
    assert_eq!(row.occupied(), 320);
}

#[test]
fn cluster_widening_and_wide_wrap_padding() {
    for insert in [false, true] {
        let mut t = fixture(8, 3);
        feed(&mut t, "\x1b[?47h\x1b[31m");
        feed(&mut t, &format!("\x1b[1;3H{CHAR}\x1b[2;1H{CHAR}x{CHAR}"));
        if insert {
            feed(&mut t, "\x1b[4h");
        }
        feed(&mut t, "\x1b[1;8H©\u{200d}🚀"); // move the encoded wide cluster across rows
        assert!(t.screen().lines[0].wrapped());
        assert!(t.grapheme(&t.screen().lines[1].cells[0]).is_some());
        t.resize(12, 3);
        check(&t, "wide wrap padding");
        feed(&mut t, "\x1b[1;9H\x1b[@"); // materialize synthetic wrap padding before edits
        feed(&mut t, "\x1b[2;4H©\u{200d}🚀"); // widen in place against existing cells
        feed(&mut t, "\x1b[2;5Hx"); // overwrite spacer, clearing its pair
    }
}

#[test]
fn decoding_tables_survive_restore_and_reuse() {
    let mut t = fixture(10, 3);
    feed(&mut t, "\x1b[?47h");
    let original = feed(&mut t, &format!("\x1b[1;9H\x1b[31;58;5;7m{CHAR}{}", M[1]));
    assert_eq!(original.len(), 1);
    t.resize(5, 3);
    assert!(check(&t, "hidden named placement").is_empty());
    feed(&mut t, "\x1b[0m\x1b[?47l");
    // The hidden cell is now the only user of the named placement color. An all-live
    // table must compact rather than hand that index to a different color.
    let before = t.link_releases();
    assert_eq!(t.intern_link(b"next", b"next", None, 1), 0);
    assert!(t.link_releases() > before);
    for i in 0..4200 {
        t.advance(format!("\r{}\u{301}", char::from_u32(0x4e00 + i).unwrap()).as_bytes());
    }
    assert!(t.grapheme_releases() > 0);
    check(&t, "reused grapheme indices");
    t.resize(10, 3);
    assert_eq!(feed(&mut t, "\x1b[?47h"), original);

    // Link eviction can make an unresolved named placement resolve to the default one
    // without writing the placeholder character again.
    let mut t = fixture(24, 3);
    assert!(feed(&mut t, &format!("\x1b[58;5;9m{CHAR}")).is_empty());
    feed(&mut t, "\x1b[59m\x1b[3;1H\r\n");
    for i in 0..7 {
        feed(&mut t, &format!("\x1b]8;;link-{i}\x07x\x1b]8;;\x07"));
    }
    t.scroll_display(1);
    assert!(check(&t, "unresolved history").is_empty());
    assert_ne!(t.intern_link(b"new", b"new", None, 8), 0);
    assert_eq!(
        pos(&check(&t, "default placement after link eviction")),
        [(0, 0, 1)]
    );
}

#[test]
fn mixed_row_transitions_match_a_full_scan_after_each_step() {
    let mut seed = 0xd05b_f328_e189_47ac_u64;
    let mut pick = |n| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed as usize % n
    };
    let edits = [
        "\x1b[2@",
        "\x1b[2P",
        "\x1b[3X",
        "\x1b[K",
        "\x1b[1K",
        "\x1b[2K",
        "\x1b[J",
        "\x1b[1J",
        "\x1b[2J",
        "\x1b[3J",
        "\x1b#8",
        "\x1b[2S",
        "\x1b[2T",
        "\x1b[2L",
        "\x1b[2M",
        "\x1bM",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[?7h",
        "\x1b[?7l",
        "\x1b[?47h",
        "\x1b[?47l",
        "\x1b[?1047h",
        "\x1b[?1047l",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[2;4r",
        "\x1b[r",
        "\x1b7",
        "\x1b8",
        "\r\n",
        "\x1bD",
        "\x1bE",
        "界",
        "©\u{200d}🚀",
        "\x1b]133;A\x07",
        "\x1b]133;B\x07",
        "\x1b]133;C\x07",
        "\x1b[4b",
    ];
    for width in [8, 80, 300] {
        let mut t = fixture(width, 5);
        t.set_scrollback_limit(8);
        for _ in 0..1500 {
            match pick(12) {
                0 => {
                    let w = 2 + pick(width + 10);
                    let h = 1 + pick(8);
                    t.resize(w, h);
                }
                1 => t.scroll_display(pick(15) as isize - 7),
                2 => t.advance(
                    format!("\x1b[{};{}H", 1 + pick(t.rows()), 1 + pick(t.cols())).as_bytes(),
                ),
                3 => t.advance("x".repeat(1 + pick(320)).as_bytes()),
                4 => t.advance(format!("\x1b[31m{CHAR}{}{CHAR}", M[pick(2)]).as_bytes()),
                5 => t.advance_preserving_wrap(format!("{CHAR}").as_bytes()),
                _ => t.advance(edits[pick(edits.len())].as_bytes()),
            }
            check(&t, "mixed row transitions");
        }
    }
}
