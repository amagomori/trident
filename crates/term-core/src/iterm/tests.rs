use super::*;

const PNG: &[u8] = include_bytes!("fixtures/imgcat.bin");
const GIF: &[u8] = include_bytes!("fixtures/imgcat-gif.bin");

#[test]
fn imgcat_multipart_png_and_legacy_animated_gif() {
    // Captured from https://iterm2.com/utilities/imgcat -W 63px and --legacy.
    for chunk_size in [1, 7, 8192] {
        let mut t = Terminal::new(80, 24);
        for chunk in PNG[..PNG.len() - 1].chunks(chunk_size) {
            t.advance(chunk);
        }
        let p = t.visible_placements()[0].placement;
        let im = t.image(p.image_id).unwrap();
        assert_eq!((im.width, im.height), (40, 20));
        assert_eq!(&im.pixels[..8], &[0, 0, 0, 255, 13, 1, 0, 255]);
        assert_eq!((p.display_pixels, p.cols, p.rows), ((63, 32), 8, 2));
        assert_eq!(t.cursor_position(), Some((1, 8)));
        t.advance(b"\r\n");
        t.advance(GIF);
        let placements = t.visible_placements();
        let gif = placements[1].placement;
        assert_eq!((gif.line, gif.col), (2, 0));
        assert_ne!(p.image_id, gif.image_id);
        assert_eq!(
            t.image(gif.image_id).unwrap().pixels,
            [255, 0, 0, 255].repeat(4)
        );
        assert!(t.take_responses().is_empty());
    }
}

// A 9×18 JPEG with EXIF orientation 6 displays as 18×9.
const JPEG: &[u8] = include_bytes!("fixtures/portrait.jpg");

fn inline_jpegs(cursor_option: &str) -> [String; 2] {
    let header = format!("inline=1;width=3;height=3;preserveAspectRatio=0{cursor_option}");
    let data = STANDARD.encode(JPEG);
    [
        format!("\x1b]1337;File={header}:{data}\x07"),
        format!(
            "\x1b]1337;MultipartFile={header}\x07\x1b]1337;FilePart={data}\x07\x1b]1337;FileEnd\x07"
        ),
    ]
}

#[test]
fn stationary_image_leaves_next_write_at_its_anchor() {
    for image in inline_jpegs(";doNotMoveCursor=1") {
        for row in [1, 4] {
            let mut t = Terminal::new(8, 5);
            t.advance(format!("\x1b[{};3H", row + 1).as_bytes());
            t.advance(image.as_bytes());
            let placements = t.visible_placements();
            assert_eq!(placements.len(), 1);
            let p = placements[0].placement;
            assert_eq!((p.line, p.col, p.cols, p.rows), (row as u64, 2, 3, 3));
            assert_eq!(t.cursor_position(), Some((row, 2)));
            t.advance(b"x");
            assert_eq!(t.viewport_row(row).cells[2].c, 'x');
            assert_eq!(t.scrollback_len(), 0);
        }
    }
}

#[test]
fn stationary_image_preserves_pending_wrap_at_bottom_right() {
    for image in inline_jpegs(";doNotMoveCursor=1") {
        let mut t = Terminal::new(8, 5);
        t.advance(b"\x1b[5;8Hx");
        t.advance(image.as_bytes());
        let p = t.visible_placements()[0].placement;
        assert_eq!((p.line, p.col), (4, 7));
        assert_eq!(t.scrollback_len(), 0);
        t.advance(b"y");
        assert_eq!(t.scrollback_len(), 1);
        assert_eq!(t.viewport_row(3).cells[7].c, 'x');
        assert_eq!(t.viewport_row(4).cells[0].c, 'y');
    }
}

#[test]
fn only_exact_cursor_option_suppresses_image_advance() {
    for option in ["", ";doNotMoveCursor=0", ";doNotMoveCursor=01"] {
        for image in inline_jpegs(option) {
            let mut t = Terminal::new(8, 5);
            t.advance(b"\x1b[5;3H");
            t.advance(image.as_bytes());
            assert_eq!(t.scrollback_len(), 2);
            assert_eq!(t.cursor_position(), Some((4, 5)));
            t.advance(b"x");
            assert_eq!(t.viewport_row(4).cells[5].c, 'x');
        }
    }
}

#[test]
fn jpeg_geometry_and_st_termination() {
    let payload = STANDARD.encode(JPEG);
    for (opts, size) in [
        ("width=auto;height=auto", (18, 9)),
        ("width=3;height=auto", (24, 12)),
        ("width=auto;height=25%", (192, 96)),
        ("width=50%;height=1", (32, 16)),
        ("width=3px;height=7px;preserveAspectRatio=0", (3, 7)),
    ] {
        let mut t = Terminal::new(80, 24);
        t.advance(format!("\x1b]1337;File=name=YS5qcGc=;inline=1;{opts}:{payload}\x1b").as_bytes());
        assert_eq!(t.image_bytes(), 0);
        t.advance(b"\\");
        assert_eq!(t.visible_placements()[0].placement.display_pixels, size);
        assert_eq!(t.image_bytes(), 18 * 9 * 4);
    }
}

#[test]
fn rejected_files_have_no_effect_and_parser_recovers() {
    let encoded = STANDARD.encode(JPEG);
    for (opts, payload, end) in [
        ("inline=0", encoded.as_str(), "\x07"),
        ("name=YS5qcGc=", &encoded, "\x07"),
        ("inline=1;width=bad", &encoded, "\x07"),
        ("inline=1;size=1", &encoded, "\x07"),
        ("inline=1", "****", "\x07"),
        ("inline=1", "Qk0=", "\x07"),
        ("inline=1", &encoded, "\x18"),
        ("inline=1", &encoded, "\x1a"),
        ("inline=1", &encoded, "\x1b[0m"),
    ] {
        let mut t = Terminal::new(80, 24);
        t.advance(format!("\x1b]1337;File={opts}:{payload}{end}").as_bytes());
        assert!(t.visible_placements().is_empty());
        assert_eq!(t.cursor_position(), Some((0, 0)));
        t.advance(b"x");
        assert_eq!(t.cursor_position(), Some((0, 1)));
    }
    let mut t = Terminal::new(80, 24);
    t.set_image_quota(18 * 9 * 4 - 1);
    t.advance(format!("\x1b]1337;File=inline=1:{encoded}\x07").as_bytes());
    assert_eq!(t.image_bytes(), 0);
    assert!(Options::parse(b"inline=1;size=16777217").is_none());
    assert!(Options::parse(&[b'x'; MAX_HEADER + 1]).is_none());
}

#[test]
fn multipart_download_and_replacement_discard_incomplete_files() {
    let mut t = Terminal::new(80, 24);
    t.advance(b"\x1b]1337;MultipartFile=inline=1\x07\x1b]1337;FilePart=abcd\x07");
    t.advance(b"\x1b]1337;MultipartFile=inline=0\x07");
    assert!(t.images.multipart.is_none());
    t.advance(b"\x1b]1337;FileEnd\x07");
    assert_eq!(t.image_bytes(), 0);
    t.advance(PNG);
    assert_eq!(t.visible_placements().len(), 1);
}
