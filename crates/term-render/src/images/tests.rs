use super::*;
use term_core::images::Image;

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
