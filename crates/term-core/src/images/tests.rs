use super::*;
use crate::Terminal;

fn image(width: u32, height: u32) -> Image {
    Image {
        width,
        height,
        pixels: vec![0xff; (width * height * 4) as usize],
    }
}

/// A 10x5 terminal with 10x20-pixel cells holding image 1 (20x40 px = 2x2 cells).
fn term() -> Terminal {
    let mut t = Terminal::new(10, 5);
    t.set_cell_pixel_size(10.0, 20.0);
    t.add_image(1, image(20, 40)).unwrap();
    t
}

fn place_at(t: &mut Terminal, row: usize, col: usize) -> Placement {
    t.advance(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    t.place_image(1, PlaceOptions::default()).unwrap()
}

fn rows(t: &Terminal) -> Vec<isize> {
    t.visible_placements().iter().map(|v| v.row).collect()
}

#[test]
fn places_at_cursor_with_cell_span() {
    let mut t = term();
    let p = place_at(&mut t, 1, 3);
    assert_eq!((p.line, p.col, p.cols, p.rows), (1, 3, 2, 2));
    assert_eq!(
        p.source,
        Rect {
            x: 0,
            y: 0,
            width: 20,
            height: 40
        }
    );
    let visible = t.visible_placements();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].row, 1);
    assert_eq!(visible[0].placement, p);
}

#[test]
fn explicit_span_offset_crop_and_z() {
    let mut t = term();
    let p = t
        .place_image(
            1,
            PlaceOptions {
                cols: 4,
                rows: 1,
                x_offset: 3,
                y_offset: 2,
                source: Some(Rect {
                    x: 5,
                    y: 10,
                    width: 100,
                    height: 0,
                }),
                z: -1,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        (p.cols, p.rows, p.x_offset, p.y_offset, p.z),
        (4, 1, 3, 2, -1)
    );
    // The crop is clamped to the image.
    assert_eq!(
        p.source,
        Rect {
            x: 5,
            y: 10,
            width: 15,
            height: 30
        }
    );
    // Fitted span counts the offset and the crop.
    let fitted = t
        .place_image(
            1,
            PlaceOptions {
                x_offset: 5,
                source: Some(Rect {
                    x: 0,
                    y: 0,
                    width: 10,
                    height: 10,
                }),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!((fitted.cols, fitted.rows), (2, 1));
}

#[test]
fn drawn_in_z_order() {
    let mut t = term();
    for z in [5, -3, 0] {
        t.place_image(
            1,
            PlaceOptions {
                z,
                ..Default::default()
            },
        )
        .unwrap();
    }
    let zs: Vec<_> = t
        .visible_placements()
        .iter()
        .map(|v| v.placement.z)
        .collect();
    assert_eq!(zs, [-3, 0, 5]);
}

#[test]
fn same_placement_id_replaces() {
    let mut t = term();
    let opts = PlaceOptions {
        placement_id: 7,
        ..Default::default()
    };
    t.place_image(1, opts).unwrap();
    t.advance(b"\x1b[3;1H");
    t.place_image(1, opts).unwrap();
    assert_eq!(rows(&t), [2]);
    assert!(t.delete_placement(1, 7));
    assert!(t.visible_placements().is_empty());
}

#[test]
fn rejects_bad_images() {
    let mut t = Terminal::new(10, 5);
    assert_eq!(t.add_image(1, image(0, 4)), Err(ImageError::InvalidSize));
    let short = Image {
        width: 2,
        height: 2,
        pixels: vec![0; 15],
    };
    assert_eq!(t.add_image(1, short), Err(ImageError::InvalidSize));
    let wide = Image {
        width: MAX_IMAGE_SIDE + 1,
        height: 1,
        pixels: Vec::new(),
    };
    assert_eq!(t.add_image(1, wide), Err(ImageError::TooLarge));
    assert_eq!(
        t.place_image(9, PlaceOptions::default()),
        Err(ImageError::NotFound)
    );
    assert_eq!(t.image_bytes(), 0);
}

#[test]
fn scrolls_with_text_into_scrollback() {
    let mut t = term();
    place_at(&mut t, 1, 0);
    t.advance(b"\x1b[5;1H\n");
    assert_eq!(rows(&t), [0]);
    t.advance(b"\n");
    // Its top is in scrollback; the bottom row is still on screen.
    assert_eq!(rows(&t), [-1]);
    t.advance(b"\n");
    assert!(t.visible_placements().is_empty());
    assert_eq!(t.screen().placements[0].line, 1);
    // Scrolling the view back shows it again at the same line.
    t.scroll_display(3);
    assert_eq!(rows(&t), [1]);
    assert_eq!(t.visible_placements()[0].placement.line, 1);
}

#[test]
fn dropped_when_its_line_leaves_scrollback() {
    let mut t = term();
    t.max_scrollback = 2;
    place_at(&mut t, 0, 0);
    t.advance(b"\x1b[5;1H\n\n");
    assert_eq!(t.screen().placements.len(), 1);
    t.advance(b"\n");
    assert_eq!(t.screen().placements.len(), 0);
    // A placement still in scrollback is kept.
    place_at(&mut t, 4, 0);
    t.advance(b"\n\n");
    assert_eq!(t.screen().placements.len(), 1);
}

#[test]
fn erase_display_preserves_bottom_clipped_images_in_history() {
    for limit in [0, 1, 10] {
        for explicit_rows in [0, 2, 10] {
            let mut t = term();
            t.set_scrollback_limit(limit);
            t.advance(b"\x1b[5;1H");
            let p = t
                .place_image(
                    1,
                    PlaceOptions {
                        rows: explicit_rows,
                        ..Default::default()
                    },
                )
                .unwrap();
            t.advance(b"\x1b[2J");
            assert_eq!(t.scrollback_len(), limit.min(5));
            assert!(t.visible_placements().is_empty());
            t.scroll_display(5);
            if limit == 0 {
                assert!(t.visible_placements().is_empty());
            } else {
                let visible = t.visible_placements();
                assert_eq!(visible.len(), 1);
                assert_eq!(visible[0].row, limit.min(5) as isize - 1);
                assert_eq!(visible[0].placement.line, p.line);
                assert_eq!(visible[0].placement.visible_rows(), 1);
                assert_eq!(visible[0].placement.rows, p.rows);
                assert_eq!(visible[0].placement.source, p.source);
                // Repeated clears must not erase a placement already in history.
                t.advance(b"\x1b[2J");
                assert_eq!(t.visible_placements(), visible);
            }
            t.advance(b"\x1b[3J");
            assert!(t.visible_placements().is_empty());
        }
    }
}

#[test]
fn moves_with_scroll_regions() {
    let mut t = term();
    place_at(&mut t, 2, 0);
    // A region below the image does not move it.
    t.advance(b"\x1b[4;5r\x1b[5;1H\n");
    assert_eq!(rows(&t), [2]);
    // Scrolling the region it is in moves it up; out of the region drops it.
    t.advance(b"\x1b[2;5r\x1b[5;1H\n");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\n");
    assert!(t.visible_placements().is_empty());
    // Reverse index scrolls it down.
    t.advance(b"\x1b[r");
    place_at(&mut t, 1, 0);
    t.advance(b"\x1b[1;1H\x1bM");
    assert_eq!(rows(&t), [2]);
    // Insert/delete line move it too.
    t.advance(b"\x1b[1;1H\x1b[2L");
    assert_eq!(rows(&t), [4]);
    t.advance(b"\x1b[1;1H\x1b[3M");
    assert_eq!(rows(&t), [1]);
}

#[test]
fn full_screen_scroll_without_scrollback() {
    let mut t = term();
    t.max_scrollback = 0;
    place_at(&mut t, 2, 0);
    t.advance(b"\x1b[5;1H\n");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\n\n");
    assert!(t.screen().placements.is_empty());
}

#[test]
fn erasing_its_cells_removes_it() {
    // EL over another part of the row keeps it; over its cells removes it.
    let mut t = term();
    place_at(&mut t, 1, 4);
    t.advance(b"\x1b[2;7H\x1b[K");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\x1b[3;6H\x1b[1K");
    assert!(t.visible_placements().is_empty());

    // ECH.
    let mut t = term();
    place_at(&mut t, 1, 4);
    t.advance(b"\x1b[3;1H\x1b[4X");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\x1b[3;1H\x1b[5X");
    assert!(t.visible_placements().is_empty());

    // EL 2.
    let mut t = term();
    place_at(&mut t, 1, 4);
    t.advance(b"\x1b[4;1H\x1b[2K");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\x1b[3;1H\x1b[2K");
    assert!(t.visible_placements().is_empty());

    // ED below and above.
    let mut t = term();
    place_at(&mut t, 1, 4);
    t.advance(b"\x1b[4;1H\x1b[J\x1b[1;1H\x1b[1J");
    assert_eq!(rows(&t), [1]);
    t.advance(b"\x1b[3;1H\x1b[J");
    assert!(t.visible_placements().is_empty());

    // ED 2 and DECSED.
    for clear in [&b"\x1b[2J"[..], b"\x1b[?2J"] {
        let mut t = term();
        place_at(&mut t, 1, 4);
        t.advance(clear);
        assert!(t.visible_placements().is_empty());
    }
}

#[test]
fn csi_3j_and_ris_remove_placements() {
    let mut t = term();
    place_at(&mut t, 0, 0);
    place_at(&mut t, 4, 0);
    t.advance(b"\n\n\n");
    assert_eq!(t.screen().placements.len(), 2);
    t.advance(b"\x1b[3J");
    // Only the one in scrollback goes.
    assert_eq!(rows(&t), [1]);

    let generation = t.image_generation();
    t.advance(b"\x1bc");
    assert!(t.visible_placements().is_empty());
    assert!(t.image(1).is_none());
    assert_eq!(t.image_bytes(), 0);
    assert!(t.image_generation() > generation);
}

#[test]
fn screens_keep_their_own_placements() {
    let mut t = term();
    place_at(&mut t, 0, 0);
    t.advance(b"\x1b[?1049h");
    assert!(t.visible_placements().is_empty());
    place_at(&mut t, 2, 2);
    assert_eq!(rows(&t), [2]);
    // Erasing on the alternate screen leaves the primary's image alone.
    t.advance(b"\x1b[2J");
    assert!(t.visible_placements().is_empty());
    place_at(&mut t, 3, 0);
    t.advance(b"\x1b[?1049l");
    assert_eq!(rows(&t), [0]);
    // Primary scrolling into history does not move the alternate screen's placement.
    // (47 switches without clearing.)
    t.advance(b"\x1b[5;1H\n\n");
    t.advance(b"\x1b[?47h");
    assert_eq!(rows(&t), [3]);
    t.advance(b"\x1b[?47l");
    t.scroll_display(1);
    assert_eq!(rows(&t), [-1]);
    t.scroll_to_bottom();
    // 1049 clears the alternate screen on entry.
    t.advance(b"\x1b[?1049h");
    assert!(t.visible_placements().is_empty());
}

#[test]
fn alternate_screen_scrolls_placements_off() {
    let mut t = term();
    t.advance(b"\x1b[?1049h");
    place_at(&mut t, 2, 0);
    t.advance(b"\x1b[5;1H\n\n");
    assert_eq!(rows(&t), [0]);
    t.advance(b"\n");
    assert!(t.alternate.placements.is_empty());
}

#[test]
fn resize_keeps_placements_on_their_cells() {
    let mut t = term();
    t.advance(b"\x1b[2;1Hab");
    place_at(&mut t, 1, 6);
    // Narrowing past the image keeps it anchored; widening shows it again. (With the
    // cursor still on its row, the row would rewrap to keep the cursor's cell.)
    t.advance(b"\x1b[4;1H");
    t.resize(5, 5);
    assert_eq!(t.screen().placements[0].col, 6);
    assert!(t.visible_placements().is_empty());
    t.resize(10, 5);
    assert_eq!(rows(&t), [1]);
    // Shrinking with the cursor at the bottom pushes rows into scrollback; the image keeps
    // its line.
    t.advance(b"\x1b[5;1H");
    t.resize(10, 3);
    assert_eq!(rows(&t), [-1]);
    assert_eq!(t.visible_placements()[0].placement.line, 1);
    t.resize(10, 5);
    assert_eq!(rows(&t), [1]);
    assert_eq!(t.viewport_row(1).text(), "ab");
}

#[test]
fn resize_drops_placements_cut_off_the_screen() {
    let mut t = term();
    place_at(&mut t, 4, 0);
    t.advance(b"\x1b[1;1H");
    t.resize(10, 3);
    assert!(t.screen().placements.is_empty());

    // On the alternate screen rows cut from the top go nowhere.
    let mut t = term();
    t.advance(b"\x1b[?1049h");
    place_at(&mut t, 0, 0);
    place_at(&mut t, 3, 0);
    t.advance(b"\x1b[5;1H");
    t.resize(10, 3);
    assert_eq!(rows(&t), [1]);
}

#[test]
fn evicts_least_recently_used_offscreen_images() {
    let mut t = Terminal::new(10, 5);
    t.set_image_quota(3 * 400);
    for id in 1..=3 {
        t.add_image(id, image(10, 10)).unwrap();
    }
    place_at_image(&mut t, 2);
    t.place_image(1, PlaceOptions::default()).unwrap();
    t.delete_placement(1, 0);
    // 3 is the least recently used, 2 is visible.
    t.add_image(4, image(10, 10)).unwrap();
    assert!(t.image(3).is_none());
    assert!(t.image(1).is_some() && t.image(2).is_some());
    t.add_image(5, image(10, 10)).unwrap();
    assert!(t.image(1).is_none());
    assert_eq!(t.image_bytes(), 3 * 400);

    // An image that only scrolled into scrollback can be evicted.
    t.advance(b"\x1b[5;1H\n\n\n\n\n");
    t.add_image(6, image(10, 10)).unwrap();
    t.add_image(7, image(10, 10)).unwrap();
    assert!(t.image(2).is_none());
    assert!(t.screen().placements.is_empty());
}

fn place_at_image(t: &mut Terminal, id: u32) {
    t.place_image(id, PlaceOptions::default()).unwrap();
}

#[test]
fn keeps_visible_images_over_quota() {
    let mut t = Terminal::new(10, 5);
    t.set_image_quota(800);
    t.add_image(1, image(10, 10)).unwrap();
    t.add_image(2, image(10, 10)).unwrap();
    place_at_image(&mut t, 1);
    place_at_image(&mut t, 2);
    assert_eq!(t.add_image(3, image(10, 10)), Err(ImageError::OverQuota));
    assert_eq!(t.add_image(3, image(20, 20)), Err(ImageError::OverQuota));
    assert_eq!(t.visible_placements().len(), 2);
    // Scrolled back into view, an image is visible and kept.
    t.advance(b"\x1b[5;1H\n\n\n\n\n");
    t.scroll_display(5);
    assert_eq!(t.add_image(3, image(10, 10)), Err(ImageError::OverQuota));
}

#[test]
fn retransmitting_replaces_image_and_placements() {
    let mut t = term();
    place_at(&mut t, 0, 0);
    t.add_image(1, image(4, 4)).unwrap();
    assert_eq!(t.image(1).unwrap().width, 4);
    assert_eq!(t.image_bytes(), 64);
    assert!(t.visible_placements().is_empty());
    assert!(t.delete_image(1));
    assert!(!t.delete_image(1));
    assert_eq!(t.image_bytes(), 0);
}

#[test]
fn generation_changes_only_with_placements() {
    let mut t = term();
    let mut generation = t.image_generation();
    let mut changed = |t: &Terminal| {
        let now = t.image_generation();
        let did = now != generation;
        generation = now;
        did
    };
    t.advance(b"hello\r\n");
    assert!(!changed(&t));
    place_at(&mut t, 0, 0);
    assert!(changed(&t));
    // Text and erases that miss it leave it alone.
    t.advance(b"\x1b[5;1Htext\x1b[K");
    assert!(!changed(&t));
    // Scrolling the view does not, scrolling the screen does.
    t.scroll_display(1);
    assert!(!changed(&t));
    t.advance(b"\n");
    assert!(changed(&t));
    t.advance(b"\x1b[1;1H\x1b[2K");
    assert!(changed(&t));
    t.advance(b"\x1b[?1049h");
    assert!(changed(&t));
}

#[test]
fn shrinking_resize_drops_images_in_evicted_history() {
    for limit in [0, 2] {
        let mut t = term();
        t.set_scrollback_limit(limit);
        place_at(&mut t, 0, 0);
        place_at(&mut t, 1, 0);
        place_at(&mut t, 3, 0);
        t.advance(b"\x1b[5;1H");
        let generation = t.image_generation();
        t.resize(10, 2);
        assert_eq!(t.scrollback_len(), limit);
        let lines: Vec<_> = t.primary.placements.iter().map(|p| p.line).collect();
        assert_eq!(lines, if limit == 0 { vec![3] } else { vec![1, 3] });
        assert!(t.image_generation() > generation);
        assert_eq!(t.visible_placements()[0].row, 0);
        t.scroll_display(100);
        assert_eq!(t.visible_placements()[0].row, 0);
    }
}

#[test]
fn window_size_reports() {
    let mut t = Terminal::new(80, 24);
    t.set_cell_pixel_size(9.6, 19.0);
    t.advance(b"\x1b[14t\x1b[16t\x1b[18t\x1b[19t");
    assert_eq!(
        t.take_responses(),
        b"\x1b[4;456;768t\x1b[6;19;10t\x1b[8;24;80t\x1b[9;56;199t"
    );
    t.resize(100, 30);
    t.advance(b"\x1b[14t\x1b[18t");
    assert_eq!(t.take_responses(), b"\x1b[4;570;960t\x1b[8;30;100t");
    // RIS keeps the cell size the app reported.
    t.advance(b"\x1bc\x1b[16t");
    assert_eq!(t.take_responses(), b"\x1b[6;19;10t");
}

#[test]
fn failed_add_keeps_existing_images() {
    let mut t = Terminal::new(10, 5);
    t.set_image_quota(3 * 400);
    for id in 1..=3 {
        t.add_image(id, image(10, 10)).unwrap();
    }
    place_at_image(&mut t, 1);
    place_at_image(&mut t, 2);
    assert_eq!(t.add_image(4, image(20, 10)), Err(ImageError::OverQuota));
    assert!(t.image(3).is_some());
    assert_eq!(t.image_bytes(), 3 * 400);
}

#[test]
fn shifting_cells_under_an_image_removes_it() {
    for edit in [&b"\x1b[3;1H\x1b[@"[..], b"\x1b[2;5H\x1b[P"] {
        let mut t = term();
        place_at(&mut t, 1, 4);
        // Shifting cells right of it leaves it alone.
        t.advance(b"\x1b[2;7H\x1b[@\x1b[3;7H\x1b[P");
        assert_eq!(rows(&t), [1]);
        t.advance(edit);
        assert!(t.visible_placements().is_empty());
    }
}

#[test]
fn erasing_to_end_of_line_drops_cut_off_images() {
    let mut t = term();
    place_at(&mut t, 1, 6);
    t.resize(5, 5);
    t.advance(b"\x1b[3;1H\x1b[K");
    t.resize(10, 5);
    assert!(t.visible_placements().is_empty());
}

#[test]
fn alignment_pattern_removes_placements() {
    let mut t = term();
    place_at(&mut t, 1, 4);
    t.advance(b"\x1b#8");
    assert!(t.visible_placements().is_empty());
}

#[test]
fn images_past_the_right_edge_can_be_evicted() {
    let mut t = term();
    place_at(&mut t, 1, 6);
    t.advance(b"\x1b[4;1H");
    t.resize(5, 5);
    t.set_image_quota(2 * 20 * 40 * 4);
    t.add_image(2, image(20, 40)).unwrap();
    t.add_image(3, image(20, 40)).unwrap();
    assert!(t.image(1).is_none());
}

#[test]
fn cell_size_change_preserves_placement_spans() {
    let mut t = term();
    let native = place_at(&mut t, 0, 0);
    let fixed = t
        .place_image(
            1,
            PlaceOptions {
                cols: 3,
                rows: 3,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!((native.cols, native.rows), (2, 2));
    let generation = t.image_generation();
    for cell in [(5.0, 10.0), (20.0, 40.0), (9.6, 19.0)] {
        t.set_cell_pixel_size(cell.0, cell.1);
        let spans: Vec<_> = t
            .visible_placements()
            .iter()
            .map(|v| (v.placement.cols, v.placement.rows))
            .collect();
        assert_eq!(
            spans,
            [(native.cols, native.rows), (fixed.cols, fixed.rows)]
        );
        assert_eq!(t.image_generation(), generation);
    }
}

#[test]
fn image_versions_do_not_repeat_after_reset() {
    let mut t = term();
    let (_, before) = t.image_with_version(1).unwrap();
    t.advance(b"\x1bc");
    t.add_image(1, image(4, 4)).unwrap();
    let (_, after) = t.image_with_version(1).unwrap();
    assert_ne!(before, after);
}

#[test]
fn reflow_moves_placements_with_their_cells() {
    let mut t = term();
    t.advance(b"0123456789abcdef");
    place_at(&mut t, 1, 2);
    t.advance(b"\x1b[5;1H");
    t.resize(4, 5);
    let p = &t.primary.placements[0];
    assert_eq!((p.line, p.col), (3, 0));
    t.resize(10, 5);
    let p = &t.primary.placements[0];
    assert_eq!((p.line, p.col), (1, 2));
    assert_eq!(rows(&t), [1]);
}

/// What bash 5.1 and zsh 5.8 send after SIGWINCH with `$ ` as the prompt.
const PROMPT_REDRAWS: [&[u8]; 2] = [b"\r\x1b[K$ ", b"\r\r\x1b[0m\x1b[27m\x1b[24m\x1b[J$ "];

/// Image 1 on rows 1-2, the prompt on its second row, then a resize while the shell edits.
fn prompt_over_image(redraw: &[u8]) -> Terminal {
    let mut t = term();
    place_at(&mut t, 1, 0);
    t.advance(b"\x1b[3;1H$ ");
    t.resize_while_editing(8, 5, &mut []);
    t.advance(redraw);
    t
}

#[test]
fn prompt_redraw_after_resize_keeps_images_it_overlaps() {
    for redraw in PROMPT_REDRAWS {
        let mut t = prompt_over_image(redraw);
        assert_eq!(rows(&t), [1]);
        // Editing the line keeps it too, across more resizes and redraws.
        t.advance(b"ls\x08\x08\x1b[K");
        t.resize_while_editing(10, 5, &mut []);
        t.advance(redraw);
        assert_eq!(rows(&t), [1]);
        // Once the line is run, erasing its cells deletes it again.
        t.advance(b"\r\n\x1b[2;1H\x1b[K");
        assert!(t.visible_placements().is_empty());
    }
}

#[test]
fn prompt_redraw_keeps_images_cut_off_by_narrowing() {
    let mut t = term();
    place_at(&mut t, 1, 6);
    t.advance(b"\x1b[3;1H$ ");
    t.resize_while_editing(5, 5, &mut []);
    t.advance(b"\x1b[A\r\x1b[K\x1b[B$ ");
    t.resize_while_editing(10, 5, &mut []);
    assert_eq!(rows(&t), [1]);
    assert_eq!(t.visible_placements()[0].placement.col, 6);
}

#[test]
fn explicit_clears_delete_images_during_a_prompt_redraw() {
    let clears: [&[u8]; 6] = [
        b"\x1b[H\x1b[2J",
        b"\x1b[?2J",
        b"\x1bc",
        b"\x1b_Ga=d,d=i,i=1\x1b\\",
        b"\x1b_Ga=d,d=a\x1b\\",
        b"\x1b#8",
    ];
    for redraw in PROMPT_REDRAWS {
        for clear in clears {
            let mut t = prompt_over_image(redraw);
            t.advance(clear);
            assert!(t.visible_placements().is_empty(), "{clear:?}");
        }
    }
}

#[test]
fn erases_delete_images_without_a_line_editor() {
    // A program's own redraw after a resize (no line editor) still deletes what it erases.
    for redraw in PROMPT_REDRAWS {
        let mut t = term();
        place_at(&mut t, 1, 0);
        t.advance(b"\x1b[3;1H$ ");
        t.resize(8, 5);
        t.advance(redraw);
        assert!(t.visible_placements().is_empty());
    }
    // So do erases on the alternate screen.
    let mut t = term();
    t.advance(b"\x1b[?1049h");
    place_at(&mut t, 1, 0);
    t.advance(b"\x1b[3;1H");
    t.resize_while_editing(8, 5, &mut []);
    t.advance(b"\r\x1b[K");
    assert!(t.visible_placements().is_empty());
}

#[test]
fn shell_input_zone_counts_as_line_editing() {
    // OSC 133 B marks the input, so the redraw is the shell's even without `editing`.
    let mut t = term();
    place_at(&mut t, 1, 0);
    t.advance(b"\x1b[3;1H\x1b]133;A\x07$ \x1b]133;B\x07");
    t.resize(8, 5);
    t.advance(b"\r\x1b[K\x1b]133;A\x07$ \x1b]133;B\x07");
    assert_eq!(rows(&t), [1]);
}
