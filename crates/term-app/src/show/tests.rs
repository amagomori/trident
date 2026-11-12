use super::*;
use term_core::Terminal;

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const WHITE: [u8; 3] = [255, 255, 255];

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("samples")
        .join(name)
}

/// Big enough that the samples are drawn at their own size.
fn big_window() -> Window {
    Window {
        cols: 200,
        rows: 60,
        cell_w: 10.0,
        cell_h: 20.0,
        tmux: false,
    }
}

fn decode(name: &str) -> Rgba {
    let path = sample(name);
    let bytes = std::fs::read(&path).unwrap();
    let options = Options::default();
    match detect(&bytes) {
        Kind::Raster(format) => decode_raster(&bytes, format, &options, big_window()).unwrap(),
        Kind::Svg => render_svg(&bytes, &path, &options, big_window()).unwrap(),
        other => panic!("{name}: detected as {other:?}"),
    }
}

fn pixel(image: &Rgba, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    image.pixels[i..i + 4].try_into().unwrap()
}

/// Checks the red / green / blue / white quadrants at the middle of each quadrant.
fn assert_quadrants(name: &str, image: &Rgba, tolerance: u8) {
    let (w, h) = (image.width, image.height);
    let points = [
        (w / 4, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, 3 * h / 4),
        (3 * w / 4, 3 * h / 4),
    ];
    for ((x, y), want) in points.into_iter().zip([RED, GREEN, BLUE, WHITE]) {
        let got = pixel(image, x, y);
        let close = got[..3]
            .iter()
            .zip(want)
            .all(|(g, w)| g.abs_diff(w) <= tolerance);
        assert!(
            close && got[3] == 255,
            "{name} at ({x}, {y}): {got:?}, want {want:?}"
        );
    }
}

#[test]
fn every_format_renders_from_its_sample() {
    // (file, format, width, height, tolerance for lossy formats)
    let samples = [
        ("sample.png", Kind::Raster(ImageFormat::Png), 48, 32, 0),
        ("sample.jpg", Kind::Raster(ImageFormat::Jpeg), 48, 32, 24),
        ("sample.gif", Kind::Raster(ImageFormat::Gif), 48, 32, 0),
        ("sample.webp", Kind::Raster(ImageFormat::WebP), 48, 32, 24),
        ("sample.bmp", Kind::Raster(ImageFormat::Bmp), 48, 32, 0),
        ("sample.tiff", Kind::Raster(ImageFormat::Tiff), 48, 32, 0),
        ("sample.ico", Kind::Raster(ImageFormat::Ico), 32, 32, 0),
        ("sample.qoi", Kind::Raster(ImageFormat::Qoi), 48, 32, 0),
        ("sample.ppm", Kind::Raster(ImageFormat::Pnm), 48, 32, 0),
        ("sample.tga", Kind::Raster(ImageFormat::Tga), 48, 32, 0),
        ("sample.svg", Kind::Svg, 48, 32, 0),
    ];
    for (name, kind, w, h, tolerance) in samples {
        let bytes = std::fs::read(sample(name)).unwrap();
        assert_eq!(detect(&bytes), kind, "{name}");
        let image = decode(name);
        assert_eq!((image.width, image.height), (w, h), "{name}");
        assert_quadrants(name, &image, tolerance);
    }
}

#[test]
fn photos_are_turned_upright() {
    // A 16x8 JPEG (left red, right blue) tagged EXIF orientation 6: rotate 90 degrees clockwise.
    let image = decode("rotated.jpg");
    assert_eq!((image.width, image.height), (8, 16));
    assert!(pixel(&image, 4, 3)[0] > 200, "red on top");
    assert!(pixel(&image, 4, 12)[2] > 200, "blue below");
}

#[test]
fn svg_draws_embedded_raster_images() {
    let png = BASE64.encode(std::fs::read(sample("sample.png")).unwrap());
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="32">
        <image width="48" height="32" href="data:image/png;base64,{png}"/></svg>"#
    );
    let image = render_svg(
        svg.as_bytes(),
        Path::new("i.svg"),
        &Options::default(),
        big_window(),
    )
    .unwrap();
    assert_quadrants("embedded PNG", &image, 0);
}

#[test]
fn gif_shows_its_first_frame() {
    // The second frame is black.
    assert_eq!(pixel(&decode("sample.gif"), 0, 0), [255, 0, 0, 255]);
}

#[test]
fn type_comes_from_the_bytes_not_the_name() {
    let bytes = std::fs::read(sample("png-named.jpg")).unwrap();
    assert_eq!(detect(&bytes), Kind::Raster(ImageFormat::Png));
    assert_eq!(detect(b"P1 is a text file\n"), Kind::Text);
    assert_eq!(
        detect(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
        Kind::Svg
    );
    assert_eq!(
        detect(b"\xef\xbb\xbf  <?xml version='1.0'?>\n<svg/>"),
        Kind::Svg
    );
    assert_eq!(detect(b"<html>not <svg</html>"), Kind::Text);
    assert_eq!(detect(b"BMW service notes\n"), Kind::Text);
    assert_eq!(detect(b"qoif is a format\n"), Kind::Text);
    assert_eq!(
        detect(b"P3\n1 1\n255\n255 0 0\n"),
        Kind::Raster(ImageFormat::Pnm)
    );
}

#[test]
fn svgz_is_svg() {
    let svg = std::fs::read(sample("sample.svg")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&svg).unwrap();
    let svgz = gz.finish().unwrap();
    assert_eq!(detect(&svgz), Kind::Svg);
    let image = render_svg(
        &svgz,
        Path::new("x.svgz"),
        &Options::default(),
        big_window(),
    )
    .unwrap();
    assert_quadrants("svgz", &image, 0);
}

#[test]
fn svg_text_uses_the_bundled_font_when_needed() {
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="30">
        <rect width="80" height="30" fill="#fff"/>
        <text x="2" y="22" font-family="monospace" font-size="20" fill="#000">Hi</text></svg>"##;
    let image = render_svg(svg, Path::new("t.svg"), &Options::default(), big_window()).unwrap();
    let dark = image.pixels.chunks(4).filter(|p| p[0] < 128).count();
    assert!(dark > 20, "text drew {dark} dark pixels");
}

#[test]
fn output_places_the_image_in_a_terminal() {
    let mut out = Vec::new();
    show(
        &sample("sample.png"),
        &Options::default(),
        big_window(),
        &mut out,
    )
    .unwrap();
    let mut term = Terminal::new(80, 24);
    term.set_cell_pixel_size(10.0, 20.0);
    // The tty turns newlines into CR LF.
    let out = String::from_utf8(out).unwrap().replace('\n', "\r\n");
    term.advance(out.as_bytes());
    assert!(
        term.take_responses().is_empty(),
        "q=2 keeps replies off the shell"
    );
    let placements = term.visible_placements();
    assert_eq!(placements.len(), 1);
    let p = placements[0].placement;
    // 48x32 pixels on 10x20 cells.
    assert_eq!((placements[0].row, p.col, p.cols, p.rows), (0, 0, 5, 2));
    let image = term.image(p.image_id).unwrap();
    assert_eq!((image.width, image.height), (48, 32));
    assert_eq!(image.pixels, decode("sample.png").pixels);
    // The prompt comes back on the line below the image.
    assert_eq!(term.cursor_position(), Some((2, 0)));
}

#[test]
fn large_images_are_sent_in_chunks() {
    let image = Rgba {
        width: 300,
        height: 300,
        pixels: (0..300 * 300 * 4).map(|i| (i * 7 % 251) as u8).collect(),
    };
    let mut out = Vec::new();
    write_kitty(&mut out, &image, big_window(), |_| unreachable!()).unwrap();
    let text = String::from_utf8(out.clone()).unwrap();
    assert!(text.matches("\x1b_G").count() > 1);
    assert!(text.starts_with("\x1b_Ga=T,f=32,o=z,q=2,s=300,v=300,m=1;"));
    let mut term = Terminal::new(80, 24);
    term.set_cell_pixel_size(10.0, 20.0);
    term.advance(&out);
    let id = term.visible_placements()[0].placement.image_id;
    assert_eq!(term.image(id).unwrap().pixels, image.pixels);
}

#[test]
fn bad_files_fail_with_a_message() {
    let error = |path: &Path| {
        let mut out = Vec::new();
        let e = show(path, &Options::default(), big_window(), &mut out).unwrap_err();
        assert!(out.is_empty(), "{}: wrote {out:?}", path.display());
        e
    };
    assert_eq!(error(&sample("missing.png")), "No such file or directory");
    assert!(error(&sample("corrupt.png")).starts_with("cannot decode PNG image: "));
    assert!(error(&sample("corrupt.svg")).starts_with("cannot read SVG: "));
    assert_eq!(error(&sample("")), "Is a directory");

    let dir = std::env::temp_dir().join(format!("terminal-show-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let locked = dir.join("locked.png");
    std::fs::write(&locked, b"x").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway.
        if std::fs::read(&locked).is_err() {
            assert_eq!(error(&locked), "Permission denied");
        }
    }
    let truncated_jpeg = dir.join("cut.jpg");
    let jpeg = std::fs::read(sample("sample.jpg")).unwrap();
    std::fs::write(&truncated_jpeg, &jpeg[..jpeg.len() / 3]).unwrap();
    assert!(error(&truncated_jpeg).starts_with("cannot decode JPEG image: "));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn text_is_printed_with_controls_made_visible() {
    let mut out = Vec::new();
    show(
        &sample("sample.txt"),
        &Options::default(),
        big_window(),
        &mut out,
    )
    .unwrap();
    assert_eq!(out, std::fs::read(sample("sample.txt")).unwrap());

    let mut out = Vec::new();
    write_text(&mut out, "red \x1b[31mtext\x07\r\nnext\u{9b}".as_bytes()).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "red ^[[31mtext^G\nnext\u{fffd}\n"
    );
}

#[test]
fn binaries_get_a_summary() {
    let mut out = Vec::new();
    show(
        &sample("sample.elf"),
        &Options::default(),
        big_window(),
        &mut out,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "ELF executable, 28 bytes\n"
    );
    assert_eq!(binary_kind(b"%PDF-1.7\n"), "PDF document");
    assert_eq!(binary_kind(&[0, 1, 2, 3]), "binary data");
    assert_eq!(
        detect(b"\0\0\0\0ftypavif\0\0\0\0"),
        Kind::Binary("AVIF image (not supported)".into())
    );
    assert_eq!(human_size(1023), "1023 bytes");
    assert_eq!(human_size(1536), "1.5 KiB");
    assert_eq!(human_size(5 << 20), "5.0 MiB");
}

#[test]
fn images_shrink_to_the_window_but_never_grow() {
    let window = Window {
        cols: 80,
        rows: 25,
        cell_w: 10.0,
        cell_h: 20.0,
        tmux: false,
    };
    let none = Options::default();
    // 800x480 text area, minus a row for the prompt.
    assert_eq!(target_size((48.0, 32.0), &none, window), (48, 32));
    assert_eq!(target_size((1600.0, 400.0), &none, window), (800, 200));
    assert_eq!(target_size((1000.0, 2000.0), &none, window), (240, 480));
    let fit = Options {
        fit: true,
        ..Options::default()
    };
    assert_eq!(target_size((48.0, 32.0), &fit, window), (720, 480));
    let width = Options {
        width: Some(10),
        ..Options::default()
    };
    assert_eq!(target_size((48.0, 32.0), &width, window), (100, 67));
    let height = Options {
        height: Some(2),
        ..Options::default()
    };
    assert_eq!(target_size((48.0, 32.0), &height, window), (60, 40));
    let both = Options {
        width: Some(10),
        height: Some(1),
        ..Options::default()
    };
    assert_eq!(target_size((48.0, 32.0), &both, window), (30, 20));
    let huge = Options {
        width: Some(60_000),
        ..Options::default()
    };
    assert_eq!(target_size((100.0, 50.0), &huge, window), (10_000, 5_000));
}

#[test]
fn parses_arguments() {
    let args = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
    assert_eq!(
        args(&["--width", "20", "--fit", "a.png", "--", "--b.png"]),
        Ok(Options {
            width: Some(20),
            height: None,
            fit: true,
            files: vec!["a.png".into(), "--b.png".into()],
        })
    );
    assert!(args(&[]).is_err());
    assert!(args(&["--width", "0", "a.png"]).is_err());
    assert!(args(&["--height"]).is_err());
    assert!(args(&["--bogus", "a.png"]).is_err());
    assert_eq!(args(&["-"]).unwrap().files, vec![PathBuf::from("-")]);
}

#[test]
fn parses_the_pixel_size_report() {
    assert_eq!(
        parse_pixel_report(b"\x1b[4;480;800t\x1b[?62;22c"),
        Some((800, 480))
    );
    assert_eq!(parse_pixel_report(b"\x1b[?62;22c"), None);
    assert_eq!(parse_pixel_report(b"\x1b[4;0;0t"), None);
}

fn noise(width: u32, height: u32) -> Rgba {
    Rgba {
        width,
        height,
        pixels: (0..width * height * 4)
            .map(|i| (i * 7 % 251) as u8)
            .collect(),
    }
}

fn in_tmux() -> Window {
    Window {
        tmux: true,
        ..big_window()
    }
}

/// tmux output for `image`, with the waits it asked for marked by `<room>` and `<chunk>`.
fn tmux_output(image: &Rgba) -> Vec<u8> {
    let out = std::cell::RefCell::new(Vec::new());
    let mut writer = Marked(&out);
    write_kitty(&mut writer, image, in_tmux(), |wait| {
        out.borrow_mut().extend_from_slice(match wait {
            TmuxWait::Room => b"<room>",
            TmuxWait::Chunk => b"<chunk>",
        })
    })
    .unwrap();
    out.into_inner()
}

struct Marked<'a>(&'a std::cell::RefCell<Vec<u8>>);

impl Write for Marked<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Splits tmux output, after the room made for the image (`rows` newlines and the cursor
/// back up), into the escapes inside its passthroughs (inner ESCs undoubled, `<chunk>` marks
/// checked) and whatever follows the last one.
fn unwrap_passthrough(out: &[u8], rows: usize) -> (Vec<Vec<u8>>, Vec<u8>) {
    let room = [
        b"\n".repeat(rows),
        format!("\x1b[{rows}A<room>").into_bytes(),
    ]
    .concat();
    let mut out = out
        .strip_prefix(&room[..])
        .expect("room for the image first");
    let mut escapes = Vec::new();
    while let Some(rest) = out.strip_prefix(b"\x1bPtmux;") {
        let mut escape = Vec::new();
        let mut i = 0;
        loop {
            match (rest[i], rest.get(i + 1)) {
                (0x1b, Some(0x1b)) => {
                    escape.push(0x1b);
                    i += 2;
                }
                (0x1b, Some(b'\\')) => break,
                (0x1b, other) => panic!("lone ESC inside the passthrough, then {other:?}"),
                (b, _) => {
                    escape.push(b);
                    i += 1;
                }
            }
        }
        escapes.push(escape);
        out = &rest[i + 2..];
        if out.starts_with(b"\x1bPtmux;") {
            panic!("no pause between chunks");
        }
        if let Some(rest) = out.strip_prefix(b"<chunk>") {
            out = rest;
        }
    }
    (escapes, out.to_vec())
}

#[test]
fn inside_tmux_every_escape_goes_through_passthrough() {
    let image = noise(300, 300);
    let (escapes, rest) = unwrap_passthrough(&tmux_output(&image), 15);
    assert!(escapes.len() > 1);
    // 300 pixels on 20-pixel rows: the cursor steps over 15 rows.
    assert_eq!(rest, b"\n".repeat(15));
    let last = escapes.len() - 1;
    for (i, escape) in escapes.iter().enumerate() {
        let start = match i {
            0 => "\x1b7\x1b_Ga=T,f=32,o=z,q=2,C=1,s=300,v=300,m=1;",
            i if i == last => "\x1b8\x1b_Gm=0;",
            _ => "\x1b_Gm=1;",
        };
        assert!(escape.starts_with(start.as_bytes()), "escape {i}");
        assert!(escape.ends_with(b"\x1b\\"));
        // One kitty escape per passthrough, each within the protocol's chunk size.
        let body = &escape[start.len()..escape.len() - 2];
        assert!(!body.contains(&0x1b) && body.len() <= CHUNK);
    }
}

#[test]
fn a_small_image_inside_tmux_needs_no_saved_cursor() {
    let out = tmux_output(&noise(4, 4));
    let (escapes, rest) = unwrap_passthrough(&out, 1);
    assert_eq!(escapes.len(), 1);
    assert!(escapes[0].starts_with(b"\x1b_Ga=T,f=32,o=z,q=2,C=1,s=4,v=4,m=0;"));
    assert_eq!(rest, b"\n");
}

/// Replays tmux output for `image` the way tmux 3.2a hands it to a 80x40 terminal holding
/// `before`: newlines as CR LF, erasing the new row (`ESC [ K`) when they scroll, and after
/// each passthrough the scroll region reset and the cursor homed. Returns the terminal with
/// the cursor back where tmux keeps it.
fn replay_in_tmux(image: &Rgba, rows: usize, before: &[u8]) -> Terminal {
    let (escapes, rest) = unwrap_passthrough(&tmux_output(image), rows);
    assert_eq!(rest, b"\n".repeat(rows));
    let mut term = Terminal::new(80, 40);
    term.set_cell_pixel_size(10.0, 20.0);
    term.advance(before);
    newlines(&mut term, rows);
    term.advance(format!("\x1b[{rows}A").as_bytes());
    let (row, col) = term.cursor_position().unwrap();
    for escape in escapes {
        term.advance(&escape);
        term.advance(b"\x1b[1;40r\x1b[1;1H");
    }
    term.advance(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    newlines(&mut term, rows);
    assert!(term.take_responses().is_empty());
    term
}

fn newlines(term: &mut Terminal, n: usize) {
    for _ in 0..n {
        let at_bottom = term.cursor_position().unwrap().0 == 39;
        term.advance(if at_bottom { b"\r\n\x1b[K" } else { b"\r\n" });
    }
}

#[test]
fn tmux_output_places_the_image_at_the_cursor() {
    let image = noise(300, 300);
    let term = replay_in_tmux(&image, 15, b"one\r\ntwo\r\n");
    let placements = term.visible_placements();
    assert_eq!(placements.len(), 1);
    let p = placements[0].placement;
    assert_eq!((placements[0].row, p.col, p.cols, p.rows), (2, 0, 30, 15));
    assert_eq!(term.image(p.image_id).unwrap().pixels, image.pixels);
    // tmux's own cursor steps past the image's 15 rows.
    assert_eq!(term.cursor_position().unwrap(), (17, 0));
}

#[test]
fn tmux_output_at_the_bottom_survives_tmux_scrolling() {
    // 39 lines put the cursor on the last row: the room made scrolls the screen first.
    let before: Vec<u8> = (1..40)
        .flat_map(|n| format!("{n}\r\n").into_bytes())
        .collect();
    let mut term = replay_in_tmux(&noise(300, 300), 15, &before);
    let placements = term.visible_placements();
    assert_eq!(placements.len(), 1);
    assert_eq!((placements[0].row, placements[0].placement.rows), (24, 15));
    assert_eq!(term.cursor_position().unwrap(), (39, 0));
    // A prompt, then tmux scrolling the pane for the next line: it erases the new bottom
    // row, which the image is clear of.
    term.advance(b"$ \x1b[1;40r\x1b[40;1H\n\x1b[K");
    let placements = term.visible_placements();
    assert_eq!(placements.len(), 1);
    assert_eq!(placements[0].row, 23);
}

#[test]
fn outside_tmux_escapes_go_straight_to_the_terminal() {
    let mut out = Vec::new();
    write_kitty(&mut out, &noise(300, 300), big_window(), |_| unreachable!()).unwrap();
    assert!(out.starts_with(b"\x1b_Ga=T,f=32,o=z,q=2,s=300,v=300,m=1;"));
    assert!(out.ends_with(b"\x1b\\\n"));
    assert!(!out.windows(2).any(|w| w == b"\x1bP" || w == b"\x1b7"));
}

#[test]
fn reads_tmux_allow_passthrough() {
    assert!(!passthrough_allowed("allow-passthrough* off\n"));
    assert!(!passthrough_allowed("allow-passthrough off\n"));
    assert!(passthrough_allowed("allow-passthrough on\n"));
    assert!(passthrough_allowed("allow-passthrough* all\n"));
    assert!(!passthrough_allowed(""));
}
