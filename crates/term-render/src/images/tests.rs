use super::*;
use term_core::images::{Image, PlaceOptions, Rect};

fn image(width: u32, height: u32) -> Image {
    let mut pixels = Vec::new();
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[x as u8, y as u8, 0, 255]);
        }
    }
    Image {
        width,
        height,
        pixels,
    }
}

/// A 10x5 terminal with 10x20-pixel cells holding image 1 (20x40 px).
fn term() -> Terminal {
    let mut t = Terminal::new(10, 5);
    t.set_cell_pixel_size(10.0, 20.0);
    t.add_image(1, image(20, 40)).unwrap();
    t
}

fn kitty(t: &mut Terminal, keys: &str) {
    t.advance(format!("\x1b_Ga=p,i=1,q=2{keys}\x1b\\").as_bytes());
}

const CELL: (f32, f32) = (10.0, 20.0);

#[test]
fn command_budget_preserves_source_bands_on_both_sides_of_gap() {
    let cell = (10.0, 6.0);
    for marked in [false, true] {
        let mut t = Terminal::new(40, 4);
        t.set_scrollback_limit(32);
        t.set_command_budget(true, 0.5);
        t.set_cell_pixel_size(cell.0, cell.1);
        if marked {
            t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07img\r\n\x1b]133;C\x07");
        } else {
            t.advance(b"$ img\r\n");
        }
        t.add_image(1, image(1, 144)).unwrap();
        t.place_image(1, Default::default()).unwrap();
        t.advance(&[b'\n'; 24]);
        t.advance(b"following text");

        assert_eq!(t.image_bytes(), 576);
        assert_eq!(t.scrollback_len(), if marked { 16 } else { 22 });
        let text = |t: &Terminal, row| {
            t.viewport_row(row)
                .cells
                .iter()
                .map(|c| c.c)
                .collect::<String>()
        };
        assert_eq!(text(&t, 3).trim_end(), "following text");
        // Walk every retained row, including the three live image rows. The green
        // channel labels each source scanline, so a shifted or stretched tail fails.
        let mut bands: Vec<Option<u8>> = (0..24).map(Some).collect();
        if marked {
            bands.splice(4..11, [None]);
        }
        t.scroll_display(isize::MAX);
        let start = t.viewport_to_point(0, 0).line + 1;
        for (line, band) in (start..).zip(bands) {
            t.scroll_to_line(line);
            let row = (line - t.viewport_to_point(0, 0).line) as usize;
            let quads = layout(&t, cell, 0.0, 0.0);
            if band.is_none() {
                assert_eq!(text(&t, row).trim_end(), "… 7 lines of img output dropped");
            }
            for scan in 0..6 {
                let y = (row * 6 + scan) as f32 + 0.5;
                let sampled: Vec<_> = quads
                    .iter()
                    .filter(|q| (q.pos[1]..q.pos[1] + q.size[1]).contains(&y))
                    .map(|q| {
                        let v = q.uv[1] + (y - q.pos[1]) / q.size[1] * (q.uv[3] - q.uv[1]);
                        let image = t.image(q.image_id).unwrap();
                        image.pixels[(v * image.height as f32) as usize * 4 + 1]
                    })
                    .collect();
                assert_eq!(
                    sampled.as_slice(),
                    band.map(|b| b * 6 + scan as u8).as_slice()
                );
            }
        }
    }
}

#[test]
fn command_budget_and_erase_display_crop_a_stretched_source_pixel() {
    let mut t = Terminal::new(40, 4);
    t.set_scrollback_limit(32);
    t.set_cell_pixel_size(10.0, 6.0);
    t.advance(b"\x1b]133;A\x07$ \x1b]133;B\x07img\r\n\x1b]133;C\x07");
    t.advance(&[b'\n'; 5]);
    t.add_image(1, image(1, 3)).unwrap();
    let opts = PlaceOptions {
        source: Some(Rect {
            x: 0,
            y: 1,
            width: 1,
            height: 1,
        }),
        rows: 32,
        y_offset: 2,
        ..Default::default()
    };
    t.place_image(1, opts).unwrap();
    t.advance(&[b'\n'; 19]);
    // The anchor was discarded. ED 2 now clips the surviving tail at the
    // screen bottom: original image rows 16..20 were visible before the clear.
    t.advance(b"\x1b[2J");
    assert!(layout(&t, (10.0, 6.0), 0.0, 0.0).is_empty());
    t.scroll_display(4);
    let saved = layout(&t, (10.0, 6.0), 0.0, 0.0);
    let [q] = saved.as_slice() else {
        panic!("expected a saved tail: {saved:?}")
    };
    assert_eq!(q.pos[1] + q.size[1], 24.0);
    let height = 32.0 * 6.0 - 2.0;
    let first_pixel = 16.0 * 6.0 - 2.0;
    let last_pixel = 20.0 * 6.0 - 2.0;
    let view_top = q.uv[1] + (q.uv[3] - q.uv[1]) * (-q.pos[1] / q.size[1]);
    // Filtering a fraction of this texel must still clamp to its original center.
    assert_eq!(q.source_v, [1.0 / 3.0, 2.0 / 3.0]);
    assert!((view_top - (1.0 + first_pixel / height) / 3.0).abs() < 1e-6);
    assert!((q.uv[3] - (1.0 + last_pixel / height) / 3.0).abs() < 1e-6);
}

#[test]
fn fractional_scroll_keeps_image_just_above_viewport() {
    let mut t = term();
    kitty(&mut t, ",c=1,r=1");
    t.advance(b"\x1b[5;1H\r\n");
    assert!(layout(&t, CELL, 6.0, 0.0).is_empty());
    assert_eq!(layout(&t, CELL, 6.0, 10.0)[0].pos[1], -4.0);
}

#[test]
fn draws_at_pixel_size_on_the_cursor_cell() {
    let mut t = term();
    t.advance(b"\x1b[2;4H");
    kitty(&mut t, "");
    let quads = layout(&t, CELL, 6.0, 0.0);
    assert_eq!(
        quads,
        vec![Quad {
            image_id: 1,
            layer: Layer::AboveText,
            pos: [6.0 + 30.0, 6.0 + 20.0],
            size: [20.0, 40.0],
            uv: [0.0, 0.0, 1.0, 1.0],
            source_v: [0.0, 1.0],
        }]
    );
}

#[test]
fn cell_span_stretches_and_offsets_shift() {
    let mut t = term();
    kitty(&mut t, ",c=4,X=3,Y=5");
    let q = layout(&t, CELL, 0.0, 0.0)[0];
    assert_eq!(q.pos, [3.0, 5.0]);
    assert_eq!(q.size, [37.0, 40.0]);
    t.set_cell_pixel_size(5.0, 10.0);
    let q = layout(&t, (5.0, 10.0), 0.0, 0.0)[0];
    assert_eq!(q.pos, [1.5, 2.5]);
    assert_eq!(q.size, [18.5, 20.0]);
}

#[test]
fn source_rect_maps_to_uv() {
    let mut t = term();
    kitty(&mut t, ",x=5,y=10,w=10,h=20,r=3");
    let q = layout(&t, CELL, 0.0, 0.0)[0];
    assert_eq!(q.uv, [0.25, 0.25, 0.75, 0.75]);
    assert_eq!(q.size, [10.0, 60.0]);
}

#[test]
fn scale_changes_keep_images_in_their_original_cells() {
    for initial_cell in [(20.0, 34.0), (10.0, 17.0)] {
        let mut t = Terminal::new(80, 24);
        t.set_cell_pixel_size(initial_cell.0, initial_cell.1);
        t.add_image(1, image(160, 96)).unwrap();
        kitty(&mut t, "");
        let placement = t.visible_placements()[0].placement;
        assert_eq!(layout(&t, initial_cell, 0.0, 0.0)[0].size, [160.0, 96.0]);
        t.advance(b"\r\nfollowing text");
        for cell in [(10.0, 17.0), (20.0, 34.0), (13.0, 22.1)] {
            t.set_cell_pixel_size(cell.0, cell.1);
            let q = layout(&t, cell, 0.0, 0.0)[0];
            assert_eq!(
                q.size,
                [
                    160.0 * (cell.0 / initial_cell.0),
                    96.0 * (cell.1 / initial_cell.1)
                ]
            );
            assert_eq!(q.uv, [0.0, 0.0, 1.0, 1.0]);
            let current = t.visible_placements()[0].placement;
            assert_eq!(
                (current.cols, current.rows),
                (placement.cols, placement.rows)
            );
            assert!(q.pos[1] + q.size[1] <= t.cursor_position().unwrap().0 as f32 * cell.1);
        }
    }
}

#[test]
fn z_picks_the_layer_in_drawing_order() {
    let mut t = term();
    kitty(&mut t, ",p=1,z=5,C=1");
    kitty(&mut t, ",p=2,z=-1,C=1");
    kitty(&mut t, ",p=3,z=-1073741825,C=1");
    let layers: Vec<_> = layout(&t, CELL, 0.0, 0.0).iter().map(|q| q.layer).collect();
    assert_eq!(
        layers,
        [Layer::BelowBackgrounds, Layer::BelowText, Layer::AboveText]
    );
}

#[test]
fn moves_up_with_scrollback_and_back_down_when_scrolled_back() {
    let mut t = term();
    kitty(&mut t, ",C=1");
    t.advance(b"\x1b[5;1H\n");
    // One line scrolled out: the image's top row is cut off above the grid.
    assert_eq!(layout(&t, CELL, 0.0, 0.0)[0].pos[1], -20.0);
    t.scroll_display(1);
    assert_eq!(layout(&t, CELL, 0.0, 0.0)[0].pos[1], 0.0);
    t.scroll_display(-1);
    t.advance(b"\n");
    assert!(layout(&t, CELL, 0.0, 0.0).is_empty());
}

#[test]
fn fit_shrinks_to_the_texture_limit_keeping_aspect() {
    let big = image(300, 100);
    let small = fit(&big, 30);
    assert_eq!((small.width, small.height), (30, 10));
    assert_eq!(small.pixels.len(), 30 * 10 * 4);
    // Nearest neighbor: pixel (1, 1) comes from source (10, 10).
    assert_eq!(&small.pixels[(30 + 1) * 4..(30 + 1) * 4 + 2], &[10, 10]);
}

#[test]
fn erase_display_crops_bottom_clipped_images_without_squashing() {
    for (image_height, rows, y_offset) in [
        (40, 0, 0),
        (40, 2, 0),
        (40, 4, 0),
        (40, 0, 10),
        (40, 3, 10),
        (1, 2, 0),
        (1, 3, 10),
    ] {
        let mut t = term();
        t.add_image(1, image(20, image_height)).unwrap();
        t.advance(b"\x1b[5;1H");
        t.place_image(
            1,
            term_core::images::PlaceOptions {
                rows,
                y_offset,
                ..Default::default()
            },
        )
        .unwrap();
        let original = layout(&t, CELL, 0.0, 0.0)[0];
        t.advance(b"\x1b[2J");
        assert!(layout(&t, CELL, 0.0, 0.0).is_empty());
        t.scroll_display(5);
        let saved = layout(&t, CELL, 0.0, 0.0)[0];
        assert_eq!(saved.pos, original.pos);
        let height = CELL.1 - y_offset as f32;
        assert_eq!(saved.size, [original.size[0], height]);
        assert_eq!(saved.uv, [0.0, 0.0, 1.0, height / original.size[1]]);
    }
}

#[test]
fn pixel_sized_placement_does_not_stretch_to_rounded_cell_edges() {
    let mut t = term();
    t.place_image(
        1,
        term_core::PlaceOptions {
            display_pixels: Some((31, 17)),
            ..Default::default()
        },
    )
    .unwrap();
    let q = layout(&t, CELL, 0.0, 0.0)[0];
    assert_eq!(q.size, [31., 17.]);
    assert_eq!(q.uv, [0., 0., 1., 1.]);
}
