use crate::{Image, Terminal};

fn image(t: &Terminal) -> &Image {
    let p = t.visible_placements();
    assert_eq!(p.len(), 1);
    t.image(p[0].placement.image_id).unwrap()
}

#[test]
fn img2sixel_golden_at_every_split() {
    // img2sixel 1.10.3: 13x8 red PNG, blue rectangle (2,1) through (5,6).
    let bytes = include_bytes!("img2sixel.six");
    for split in 0..=bytes.len() {
        let mut t = Terminal::new(80, 24);
        t.advance(b"abc");
        t.advance(&bytes[..split]);
        t.advance(&bytes[split..]);
        let img = image(&t);
        assert_eq!((img.width, img.height), (13, 8));
        for (n, &pixel) in img.pixels.as_chunks::<4>().0.iter().enumerate() {
            let blue = (2..=5).contains(&(n % 13)) && (1..=6).contains(&(n / 13));
            assert_eq!(
                pixel,
                if blue {
                    [0, 0, 247, 255]
                } else {
                    [247, 0, 0, 255]
                }
            );
        }
        let v = t.visible_placements()[0];
        assert_eq!(
            (v.row, v.placement.col, v.placement.cols, v.placement.rows),
            (0, 3, 2, 1)
        );
        assert_eq!(t.cursor_position(), Some((1, 3)));
    }
}

#[test]
fn layered_pixels_hls_padding_and_transparency() {
    let mut t = Terminal::new(80, 24);
    // Reprogram the register between passes. Existing dots keep their color.
    for &b in b"\x1bP0;1q\"1;1;3;3#7;1;120;50;100!2B$#7;1;0;50;100@\x9c" {
        t.advance(&[b]);
    }
    let r = [255, 0, 0, 255];
    let b = [0, 0, 255, 255];
    let z = [0; 4];
    assert_eq!(image(&t).pixels, [b, r, z, r, r, z, z, z, z].concat());
}

#[test]
fn opaque_background_and_pixel_aspect() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b[48;2;1;2;3m\x1bPq\"2;1;2;1#4;2;0;100;50@\x1b\\");
    let img = image(&t);
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(
        img.pixels,
        [[0, 255, 128, 255], [1, 2, 3, 255]].repeat(2).concat()
    );
}

#[test]
fn aborted_or_oversize_images_do_not_place_or_move_cursor() {
    let mut t = Terminal::new(80, 24);
    t.set_image_quota(128);
    for data in [
        b"\x1bPq@\x18".as_slice(),
        b"\x1bPq@\x1a",
        b"\x1bPq@\x1b[0m",
        b"\x1bPq!4097@\x1b\\",
        b"\x1bPq!999999999999~\x1b\\",
        b"\x1bPq#256~\x1b\\",
        b"\x1bPq\"1;1;8;8\x1b\\",
        b"\x1bPq!32~\x1b\\",
        b"\x1bPq#0;2;100;0;0;99~\x1b\\",
        b"\x1bPq\x1b\\",
    ] {
        t.advance(data);
        assert_eq!(t.image_bytes(), 0, "{data:?}");
        assert_eq!(t.cursor_position(), Some((0, 0)));
    }
    t.advance(b"\x1bP7q@\x1b\\ok");
    assert_eq!(image(&t).height, 1);
    assert_eq!(t.viewport_row(1).text(), "ok");
}

#[test]
fn a_full_height_image_keeps_its_visible_bands_without_hiding_following_text() {
    for setup in [b"".as_slice(), b"\x1b[?1049h", b"\x1b[2;4r\x1b[2H"] {
        let mut t = Terminal::new(5, 4);
        if setup.is_empty() {
            t.set_scrollback_limit(0);
        }
        t.advance(setup);
        t.advance(b"\x1bPq\"1;1;1;64@\x1b\\");
        assert_eq!(image(&t).height, 64);
        let visible = t.visible_placements()[0];
        assert!(visible.placement.source.y >= 16);
        assert_eq!(visible.row + visible.placement.rows as isize, 3);
        assert_eq!(t.cursor_position(), Some((3, 0)));
        t.advance(b"X");
        assert_eq!(t.cursor_position(), Some((3, 1)));
    }
}

#[test]
fn graphics_queries_are_distinct_from_scroll_up() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"hi\x1b[c\x1b[?1;1;0S\x1b[?1;4S\x1b[?2;1S\x1b[?2;4S\x1b[?3;1S\x1b[?1;3;99S");
    assert_eq!(t.take_responses(), b"\x1b[?62;4;22c\x1b[?1;0;256S\x1b[?1;0;256S\x1b[?2;0;640;384S\x1b[?2;0;4096;4096S\x1b[?3;1S\x1b[?1;2S");
    assert_eq!(t.viewport_row(0).text(), "hi");
}
