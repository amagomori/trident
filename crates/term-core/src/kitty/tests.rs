use crate::Terminal;
use base64::Engine;

/// `kitty +kitten icat test.png` (kitty 0.21) and `chafa -f kitty -s 8x4 test.png` (chafa
/// 1.8) on an 80x24 pty of 800x480 pixels; test.png is a 40x30 RGB gradient.
const ICAT: &[u8] = include_bytes!("fixtures/icat.bin");
const ICAT_DETECT: &[u8] = include_bytes!("fixtures/icat-detect.bin");
/// `kitten icat --detect-support` from kitty 0.50 on Linux.
#[cfg(target_os = "linux")]
const KITTEN_DETECT: &[u8] = include_bytes!("fixtures/kitten-detect.bin");
/// `kitten icat` of a 4x3 JPEG from kitty 0.39.1, and of a two-frame 4x3 GIF from kitty 0.50, on
/// Linux where shared memory is refused, so frames go through `/dev/shm` temp files.
#[cfg(target_os = "linux")]
const KITTEN_039_JPG: &[u8] = include_bytes!("fixtures/kitten-0.39-jpg.bin");
#[cfg(target_os = "linux")]
const KITTEN_GIF: &[u8] = include_bytes!("fixtures/kitten-gif.bin");
const CHAFA: &[u8] = include_bytes!("fixtures/chafa.bin");

fn term() -> Terminal {
    let mut t = Terminal::new(80, 24);
    t.set_cell_pixel_size(10.0, 20.0);
    t
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
    format!("\x1b_G{control};{}\x1b\\", b64(payload)).into_bytes()
}

fn replies(t: &mut Terminal) -> String {
    String::from_utf8(t.take_responses()).unwrap()
}

fn placements(t: &Terminal) -> Vec<(u32, u32, isize, usize, usize, usize)> {
    t.visible_placements()
        .iter()
        .map(|v| {
            let p = &v.placement;
            (p.image_id, p.placement_id, v.row, p.col, p.cols, p.rows)
        })
        .collect()
}

fn gradient_rgb(w: u32, h: u32) -> Vec<u8> {
    (0..w * h)
        .flat_map(|i| [(i % w) as u8, (i / w) as u8, 0x80])
        .collect()
}

#[test]
fn icat_png_is_stored_and_placed() {
    let mut t = term();
    t.advance(ICAT);
    let placed = t.visible_placements();
    assert_eq!(placed.len(), 1);
    let p = placed[0].placement;
    let image = t.image(p.image_id).unwrap();
    assert_eq!((image.width, image.height), (40, 30));
    assert_eq!(&image.pixels[..4], &[0, 0, 0x80, 0xff]);
    // Centred by icat's leading spaces: 40x30 px is 4x2 cells of 10x20.
    assert_eq!((placed[0].row, p.col, p.cols, p.rows), (0, 38, 4, 2));
    // No i or I, so no reply; the trailing CRLF moved the cursor below the image.
    assert_eq!(replies(&mut t), "");
    assert_eq!(t.cursor_position(), Some((2, 0)));
}

#[test]
fn icat_detection_queries_are_answered() {
    let mut t = term();
    let path = std::env::temp_dir().join(format!("kan54-detect-{}", std::process::id()));
    std::fs::write(&path, b"abcd").unwrap();
    // The capture's second query names icat's own temp file; point it at ours.
    let first = ICAT_DETECT.windows(2).position(|w| w == b"\x1b\\").unwrap();
    t.advance(&ICAT_DETECT[..first + 2]);
    let second = format!(
        "a=q,t=f,s=1,v=1,i=2;{}",
        b64(path.as_os_str().as_encoded_bytes())
    );
    t.advance(format!("\x1b_G{second}\x1b\\").as_bytes());
    t.advance(&ICAT_DETECT[ICAT_DETECT.len() - 4..]);
    assert_eq!(replies(&mut t), "\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\");
    // Queries store nothing, and plain t=f leaves the file alone.
    assert!(t.image(1).is_none() && t.image(2).is_none());
    assert!(path.exists());
    assert_eq!(t.viewport_row(0).cells[0].c, 'f');
    std::fs::remove_file(path).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn kitten_detection_takes_shm_temp_file() {
    let mut t = term();
    let path = std::path::Path::new("/dev/shm").join(format!(
        "kitty-tty-graphics-protocol-kan63-detect-{}",
        std::process::id()
    ));
    std::fs::write(&path, b"123").unwrap();
    // The capture's t=t query names kitten's own /dev/shm temp file; point it at ours.
    let query = KITTEN_DETECT.windows(4).position(|w| w == b"t=t,").unwrap();
    let start = query
        + KITTEN_DETECT[query..]
            .iter()
            .position(|&b| b == b';')
            .unwrap()
        + 1;
    let end = start
        + KITTEN_DETECT[start..]
            .iter()
            .position(|&b| b == 0x1b)
            .unwrap();
    t.advance(&KITTEN_DETECT[..start]);
    t.advance(b64(path.as_os_str().as_encoded_bytes()).as_bytes());
    t.advance(&KITTEN_DETECT[end..]);
    let replies = replies(&mut t);
    assert!(replies.contains("\x1b_Gi=2;OK\x1b\\"), "{replies:?}");
    assert!(!path.exists());
}

/// Writes the files a capture's `t=t` commands name, sized for their `s` and `v`.
#[cfg(target_os = "linux")]
fn create_temp_files(capture: &[u8]) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for apc in capture
        .split(|&b| b == 0x1b)
        .filter_map(|s| s.strip_prefix(b"_G"))
    {
        let Some(semi) = apc.iter().position(|&b| b == b';') else {
            continue;
        };
        let control = std::str::from_utf8(&apc[..semi]).unwrap();
        if !control.split(',').any(|kv| kv == "t=t") {
            continue;
        }
        let key = |k: &str| {
            let v = control.split(',').find_map(|kv| kv.strip_prefix(k));
            v.map_or(1, |v| v.parse::<usize>().unwrap())
        };
        let mut payload = apc[semi + 1..].to_vec();
        payload.resize(payload.len().div_ceil(4) * 4, b'=');
        let path = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        let path = std::path::PathBuf::from(String::from_utf8(path).unwrap());
        std::fs::write(&path, vec![0x80; key("s=") * key("v=") * 3]).unwrap();
        paths.push(path);
    }
    paths
}

#[cfg(target_os = "linux")]
#[test]
fn old_kitten_shm_frame_is_read_and_deleted() {
    let mut t = term();
    // The detection query's marked file, then the frame in an unmarked kitty-icat-* file.
    let files = create_temp_files(KITTEN_039_JPG);
    assert_eq!(files.len(), 2);
    t.advance(KITTEN_039_JPG);
    let replies = replies(&mut t);
    assert!(replies.contains("\x1b_Gi=2;OK\x1b\\"), "{replies:?}");
    let placed = placements(&t);
    assert_eq!(placed.len(), 1);
    let image = t.image(placed[0].0).unwrap();
    assert_eq!((image.width, image.height), (4, 3));
    assert!(files.iter().all(|f| !f.exists()), "{files:?}");
}

#[cfg(target_os = "linux")]
#[test]
fn kitten_gif_frame_files_are_deleted() {
    let mut t = term();
    // Detection, the first frame, and a second frame (a=f) that is refused but still deleted.
    let files = create_temp_files(KITTEN_GIF);
    assert_eq!(files.len(), 3);
    t.advance(KITTEN_GIF);
    // Animation control (a=a) is not supported, but must not leave error replies behind.
    let replies = replies(&mut t);
    assert!(!replies.contains("I="), "{replies:?}");
    let placed = placements(&t);
    assert_eq!(placed.len(), 1);
    let image = t.image(placed[0].0).unwrap();
    assert_eq!((image.width, image.height), (4, 3));
    assert!(files.iter().all(|f| !f.exists()), "{files:?}");
}

#[test]
fn chafa_chunked_rgba_is_stored_and_placed() {
    for split in [CHAFA.len(), 7, 4096] {
        let mut t = term();
        for chunk in CHAFA.chunks(split) {
            t.advance(chunk);
        }
        let placed = placements(&t);
        assert_eq!(placed.len(), 1, "split {split}");
        let (id, _, row, col, cols, rows) = placed[0];
        assert_eq!((row, col, cols, rows), (0, 0, 8, 3));
        let image = t.image(id).unwrap();
        assert_eq!((image.width, image.height), (80, 60));
        assert_eq!(image.pixels.len(), 80 * 60 * 4);
        assert_eq!(replies(&mut t), "");
        assert_eq!(t.cursor_position(), Some((3, 0)));
    }
}

#[test]
fn plain_text_and_other_escapes_are_untouched() {
    let input: &[u8] = b"a\x1b[31mb\x1b]0;title\x1b\\c\x1b_not graphics\x1b\\d\x1b";
    let mut plain = term();
    for chunk in input.chunks(1) {
        plain.advance(chunk);
    }
    plain.advance(b"[0me");
    let text: String = plain.viewport_row(0).cells[..5]
        .iter()
        .map(|c| c.c)
        .collect();
    assert_eq!(text, "abcde");
    assert_eq!(plain.take_title().as_deref(), Some("title"));
    assert!(plain.visible_placements().is_empty());
}

#[test]
fn transmit_formats_and_compression() {
    let mut t = term();
    let rgb = gradient_rgb(3, 2);
    t.advance(&apc("a=t,f=24,s=3,v=2,i=1", &rgb));
    let rgba: Vec<u8> = rgb
        .chunks(3)
        .flat_map(|p| [p[0], p[1], p[2], 0xff])
        .collect();
    assert_eq!(t.image(1).unwrap().pixels, rgba);

    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut z, &rgba).unwrap();
    t.advance(&apc("a=t,f=32,o=z,s=3,v=2,i=2", &z.finish().unwrap()));
    assert_eq!(t.image(2).unwrap().pixels, rgba);

    let mut png = Vec::new();
    let mut enc = png::Encoder::new(&mut png, 3, 2);
    enc.set_color(png::ColorType::Rgb);
    enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    t.advance(&apc("a=t,f=100,i=3", &png));
    assert_eq!(t.image(3).unwrap().pixels, rgba);
    assert_eq!(
        replies(&mut t),
        "\x1b_Gi=1;OK\x1b\\\x1b_Gi=2;OK\x1b\\\x1b_Gi=3;OK\x1b\\"
    );
    assert!(t.visible_placements().is_empty());
}

#[test]
fn image_numbers_get_ids() {
    let mut t = term();
    t.advance(&apc("a=t,f=24,s=1,v=1,I=7", &[1, 2, 3]));
    let reply = replies(&mut t);
    let id: u32 = reply
        .strip_prefix("\x1b_Gi=")
        .and_then(|r| r.strip_suffix(",I=7;OK\x1b\\"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(t.image(id).is_some());
    t.advance(b"\x1b_Ga=p,I=7,p=3\x1b\\");
    assert_eq!(replies(&mut t), format!("\x1b_Gi={id},I=7,p=3;OK\x1b\\"));
    assert_eq!(placements(&t)[0].0, id);
}

#[test]
fn placement_keys() {
    let mut t = term();
    t.advance(&apc("a=t,f=24,s=40,v=40,i=5,q=1", &gradient_rgb(40, 40)));
    t.advance(b"\x1b[3;5H\x1b_Ga=p,i=5,p=1,x=10,y=0,w=20,h=40,X=3,Y=4,z=-2,C=1\x1b\\");
    let v = t.visible_placements()[0];
    let p = v.placement;
    assert_eq!((v.row, p.col, p.placement_id, p.z), (2, 4, 1, -2));
    assert_eq!((p.x_offset, p.y_offset), (3, 4));
    assert_eq!((p.source.x, p.source.width, p.source.height), (10, 20, 40));
    // 20+3 px wide is 3 cells, 40+4 px tall 3 rows; C=1 left the cursor alone.
    assert_eq!((p.cols, p.rows), (3, 3));
    assert_eq!(t.cursor_position(), Some((2, 4)));

    // Same placement id replaces; c and r size it; the cursor moves past it.
    t.advance(b"\x1b_Ga=p,i=5,p=1,c=6,r=2\x1b\\");
    assert_eq!(placements(&t), vec![(5, 1, 2, 4, 6, 2)]);
    assert_eq!(t.cursor_position(), Some((3, 10)));
    assert_eq!(
        replies(&mut t),
        "\x1b_Gi=5,p=1;OK\x1b\\\x1b_Gi=5,p=1;OK\x1b\\"
    );
}

#[test]
fn split_scroll_moves_kitty_images_with_their_anchor_cells() {
    let mut t = term();
    t.advance(&apc("a=t,f=32,s=1,v=1,i=7,q=2", &[0; 4]));
    t.advance(b"\x1b[?69h\x1b[3;8s\x1b[2;4r");
    t.advance(b"\x1b[4;1H\x1b_Ga=p,i=7,p=1,C=1,q=2\x1b\\");
    t.advance(b"\x1b[4;3H\x1b_Ga=p,i=7,p=2,r=2,c=1,q=2\x1b\\");
    assert_eq!(placements(&t), [(7, 1, 3, 0, 1, 1), (7, 2, 2, 2, 1, 2)]);
    assert_eq!(t.scrollback_len(), 0);
    t.advance(b"\x1b[T");
    assert_eq!(placements(&t)[1].2, 3);
}

#[test]
fn kitty_command_between_ed3_and_ed2_keeps_screen() {
    let mut t = term();
    t.advance(&apc("a=t,f=24,s=10,v=20,i=1,q=2", &gradient_rgb(10, 20)));
    t.advance(b"text\x1b[3J\x1b[2;1H\x1b_Ga=p,i=1,q=2\x1b\\\x1b[H\x1b[2J");
    assert!(placements(&t).is_empty());
    assert_eq!(t.scrollback_len(), 2);
    t.scroll_display(2);
    assert_eq!(placements(&t).len(), 1);
    assert_eq!(t.viewport_row(0).text(), "text");

    let mut t = term();
    t.advance(&apc("a=t,f=24,s=10,v=20,i=1,q=2", &gradient_rgb(10, 20)));
    t.advance(b"\x1b_Ga=p,i=1,q=2\x1b\\\r\ntext\x1b[3J\x1b_Ga=d,d=A\x1b\\\x1b[H\x1b[2J");
    assert!(t.image(1).is_none());
    assert_eq!(t.scrollback_len(), 2);
    t.scroll_display(2);
    assert_eq!(t.viewport_row(1).text(), "text");
}

#[test]
fn deletes() {
    let mut t = term();
    for id in 1..=3 {
        t.advance(&apc(
            &format!("a=t,f=24,s=10,v=20,i={id},q=2"),
            &gradient_rgb(10, 20),
        ));
    }
    let place = |t: &mut Terminal, at: &str, id: u32, z: i32| {
        t.advance(format!("\x1b[{at}H\x1b_Ga=p,i={id},z={z},q=2\x1b\\").as_bytes());
    };
    place(&mut t, "1;1", 1, 0);
    place(&mut t, "1;11", 2, 5);
    place(&mut t, "5;1", 3, 0);
    place(&mut t, "5;11", 1, 0);
    assert_eq!(placements(&t).len(), 4);

    t.advance(b"\x1b_Ga=d,d=z,z=5\x1b\\");
    assert!(placements(&t).iter().all(|p| p.0 != 2));
    assert!(t.image(2).is_some(), "lowercase keeps the image");

    t.advance(b"\x1b_Ga=d,d=p,x=11,y=5\x1b\\");
    assert_eq!(
        placements(&t).iter().map(|p| p.0).collect::<Vec<_>>(),
        vec![1, 3]
    );

    t.advance(b"\x1b[5;1H\x1b_Ga=d,d=C\x1b\\");
    assert!(
        t.image(3).is_none(),
        "uppercase frees an image left unplaced"
    );

    t.advance(b"\x1b_Ga=d,d=I,i=2\x1b\\");
    assert!(t.image(2).is_none());

    t.advance(b"\x1b_Ga=d,d=i,i=1\x1b\\");
    assert!(placements(&t).is_empty());
    assert!(t.image(1).is_some());
    place(&mut t, "1;1", 1, 0);
    t.advance(b"\x1b_Ga=d,d=A\x1b\\");
    assert!(placements(&t).is_empty() && t.image(1).is_none());
    assert_eq!(replies(&mut t), "");
}

#[test]
fn errors_and_quiet() {
    let mut t = term();
    t.advance(b"\x1b_Ga=p,i=9\x1b\\");
    assert_eq!(replies(&mut t), "\x1b_Gi=9;ENOENT:no such image\x1b\\");
    t.advance(&apc("a=t,f=24,s=2,v=2,i=1", &[0; 5]));
    assert_eq!(
        replies(&mut t),
        "\x1b_Gi=1;ENODATA:not enough image data\x1b\\"
    );
    t.advance(&apc("a=t,f=7,s=1,v=1,i=1", &[0; 4]));
    assert!(replies(&mut t).contains("EINVAL"));
    t.advance(b"\x1b_Ga=t,i=1,f=32,s=1,v=1;!!!!\x1b\\");
    assert_eq!(replies(&mut t), "\x1b_Gi=1;EINVAL:bad base64 payload\x1b\\");
    t.advance(&apc("a=t,f=100,i=1", b"not a png"));
    assert_eq!(replies(&mut t), "\x1b_Gi=1;EBADPNG:cannot decode PNG\x1b\\");
    t.advance(&apc("a=t,f=32,s=1,v=1,i=1,o=z", b"nope"));
    assert_eq!(replies(&mut t), "\x1b_Gi=1;EINVAL:bad zlib data\x1b\\");
    t.advance(&apc("a=t,s=1,v=1,i=1,t=s", b"/x"));
    assert!(replies(&mut t).contains("EINVAL"));
    t.advance(b"\x1b_Ga=x,i=1\x1b\\\x1b_Gi=1,i\x1b\\\x1b_Ga=p,i=1,I=2\x1b\\");
    assert_eq!(replies(&mut t).matches("EINVAL").count(), 3);
    assert!(t.image(1).is_none());

    t.advance(b"\x1b_Ga=p,i=9,q=1\x1b\\");
    assert!(replies(&mut t).contains("ENOENT"));
    t.advance(b"\x1b_Ga=p,i=9,q=2\x1b\\");
    t.advance(&apc("a=t,f=24,s=1,v=1,i=4,q=1", &[1, 2, 3]));
    assert_eq!(replies(&mut t), "");
}

#[test]
fn oversized_and_malformed_input_does_not_panic() {
    let mut t = term();
    // Too large in pixels, and a zlib bomb far bigger than s*v*4.
    t.advance(&apc("a=t,f=32,s=10001,v=1,i=1", &[0; 4]));
    assert!(replies(&mut t).contains("too large"));
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    std::io::Write::write_all(&mut z, &vec![0; 64 << 20]).unwrap();
    t.advance(&apc("a=t,f=32,o=z,s=2,v=2,i=1", &z.finish().unwrap()));
    assert_eq!(t.image(1).unwrap().pixels.len(), 16);
    replies(&mut t);

    // An over-long control part is not a command; the rest of the stream is still parsed.
    let mut long = b"\x1b_G".to_vec();
    long.extend(std::iter::repeat_n(b'a', 5000));
    long.extend_from_slice(b"\x1b\\ok");
    t.advance(&long);
    assert_eq!(t.viewport_row(0).cells[0].c, 'o');
    assert_eq!(replies(&mut t), "");

    let mut seed = 0x2545_f491_u32;
    for _ in 0..2000 {
        let mut bytes = b"\x1b_G".to_vec();
        for _ in 0..(seed % 64) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let alphabet = b"a=tTpdqfsvxyimIoz,;=1203C\x1b\\AZ+/";
            bytes.push(alphabet[seed as usize % alphabet.len()]);
        }
        bytes.extend_from_slice(b"\x1b\\");
        t.advance(&bytes);
    }
    // A transmission whose last chunk never comes is abandoned by the next command.
    let chunk = format!("\x1b_Gm=1;{}\x1b\\", "A".repeat(4096));
    t.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=7,m=1\x1b\\");
    t.advance(chunk.as_bytes());
    t.advance(b"\x1b_Ga=t,f=32,s=1,v=1,i=8,m=1\x1b\\");
    t.advance(chunk.as_bytes());
    t.advance(b"\x1b_Gm=0\x1b\\");
    assert!(t.image(7).is_none());
    assert!(t.image(8).is_some());
}

#[cfg(unix)]
#[test]
fn file_transmission() {
    let dir = std::env::temp_dir();
    let pid = std::process::id();
    let rgb = gradient_rgb(2, 2);
    let send = |t: &mut Terminal, medium: &str, path: &std::path::Path, id: u32| {
        let control = format!("a=t,f=24,s=2,v=2,t={medium},i={id}");
        t.advance(&apc(&control, path.as_os_str().as_encoded_bytes()));
        replies(t)
    };
    let mut t = term();

    let file = dir.join(format!("kan54-file-{pid}"));
    std::fs::write(&file, &rgb).unwrap();
    assert_eq!(send(&mut t, "f", &file, 1), "\x1b_Gi=1;OK\x1b\\");
    assert!(file.exists());
    // t=t only takes files meant for it, and deletes them.
    assert!(send(&mut t, "t", &file, 2).contains("EBADF"));
    assert!(file.exists());
    std::fs::remove_file(&file).unwrap();
    let temp = dir.join(format!("kan54-tty-graphics-protocol-{pid}"));
    std::fs::write(&temp, &rgb).unwrap();
    assert_eq!(send(&mut t, "t", &temp, 3), "\x1b_Gi=3;OK\x1b\\");
    assert!(!temp.exists());
    let outside = std::env::current_dir()
        .unwrap()
        .join(format!("tty-graphics-protocol-{pid}"));
    std::fs::write(&outside, &rgb).unwrap();
    assert!(send(&mut t, "t", &outside, 4).contains("EBADF"));
    assert!(outside.exists());
    std::fs::remove_file(&outside).unwrap();

    for special in [
        "/dev/zero",
        "/dev/null",
        "/proc/self/mem",
        "/sys/kernel/notes",
    ] {
        let reply = send(&mut t, "f", std::path::Path::new(special), 5);
        assert!(reply.starts_with("\x1b_Gi=5;E"), "{special}: {reply:?}");
        assert!(!reply.contains("OK"));
    }
    assert!(send(&mut t, "f", std::path::Path::new("relative"), 5).contains("EBADF"));
    assert!(send(&mut t, "f", &dir, 5).contains("EBADF"));
    let fifo = dir.join(format!("kan54-fifo-{pid}"));
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: c is a live NUL-terminated path; mkfifo does not retain its pointer.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(send(&mut t, "f", &fifo, 5).contains("EBADF"));
    std::fs::remove_file(&fifo).unwrap();
    assert!(t.image(5).is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn shm_temp_file_transmission() {
    let shm = std::path::Path::new("/dev/shm");
    let pid = std::process::id();
    let rgb = gradient_rgb(2, 2);
    let send = |t: &mut Terminal, medium: &str, path: &std::path::Path, id: u32| {
        let control = format!("a=t,f=24,s=2,v=2,t={medium},i={id}");
        t.advance(&apc(&control, path.as_os_str().as_encoded_bytes()));
        replies(t)
    };
    let mut t = term();

    // kitten's own temp file name, as icat creates it.
    let temp = shm.join(format!("kitty-tty-graphics-protocol-kan63-{pid}"));
    std::fs::write(&temp, &rgb).unwrap();
    assert_eq!(send(&mut t, "t", &temp, 1), "\x1b_Gi=1;OK\x1b\\");
    assert!(!temp.exists());
    assert_eq!(t.image(1).unwrap().pixels.len(), 2 * 2 * 4);

    // Only marked temp files: unmarked ones and t=f stay refused there.
    let unmarked = shm.join(format!("kan63-{pid}"));
    std::fs::write(&unmarked, &rgb).unwrap();
    assert!(send(&mut t, "t", &unmarked, 2).contains("EBADF"));
    assert!(unmarked.exists());

    // Unsupported a=f frames delete their temp file, but leave other files alone.
    let frame = |t: &mut Terminal, path: &std::path::Path| {
        let control = "a=f,f=24,s=2,v=2,t=t,i=1";
        t.advance(&apc(control, path.as_os_str().as_encoded_bytes()));
        replies(t)
    };
    assert!(frame(&mut t, &unmarked).contains("EINVAL:unsupported action"));
    assert!(unmarked.exists());
    std::fs::remove_file(&unmarked).unwrap();
    std::fs::write(&temp, &rgb).unwrap();
    assert!(frame(&mut t, &temp).contains("EINVAL:unsupported action"));
    assert!(!temp.exists());

    // kitten 0.39's unmarked kitty-icat-* frames are taken from /dev/shm only.
    let old = shm.join(format!("kitty-icat-kan63-{pid}"));
    std::fs::write(&old, &rgb).unwrap();
    assert_eq!(send(&mut t, "t", &old, 3), "\x1b_Gi=3;OK\x1b\\");
    assert!(!old.exists());
    let old = std::env::temp_dir().join(format!("kitty-icat-kan63-{pid}"));
    std::fs::write(&old, &rgb).unwrap();
    assert!(send(&mut t, "t", &old, 2).contains("EBADF"));
    assert!(old.exists());
    std::fs::remove_file(&old).unwrap();

    std::fs::write(&temp, &rgb).unwrap();
    assert!(send(&mut t, "f", &temp, 2).contains("EBADF"));
    assert!(temp.exists());

    // Other /dev paths stay refused, also through `..` or a marked symlink.
    let escape = shm.join("../zero");
    assert!(send(&mut t, "t", &escape, 2).contains("EBADF"));
    let link = std::env::temp_dir().join(format!("kan63-tty-graphics-protocol-{pid}"));
    std::os::unix::fs::symlink("/dev/zero", &link).unwrap();
    assert!(send(&mut t, "t", &link, 2).contains("EBADF"));
    std::fs::remove_file(&link).unwrap();
    std::fs::remove_file(&temp).unwrap();

    // A marked FIFO in /dev/shm is not read.
    let fifo = shm.join(format!("kan63-tty-graphics-protocol-fifo-{pid}"));
    let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: c is a live NUL-terminated path; mkfifo does not retain its pointer.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    assert!(send(&mut t, "t", &fifo, 2).contains("EBADF"));
    std::fs::remove_file(&fifo).unwrap();
    assert!(t.image(2).is_none());
}

#[test]
fn review_edge_cases() {
    let mut t = term();
    // An empty control part, and CAN / SUB aborting an APC, leave the stream intact.
    t.advance(b"\x1b_;\x1b\\a\x1b_Ga=t\x18b\x1b_Gq=1\x1ac");
    let text: String = t.viewport_row(0).cells[..3].iter().map(|c| c.c).collect();
    assert_eq!(text, "abc");

    // A huge r does not loop over billions of rows.
    t.advance(&apc(
        "a=T,f=32,s=1,v=1,i=1,r=4294967295,c=4294967295,q=2",
        &[0; 4],
    ));
    assert_eq!(t.cursor_position(), Some((23, 79)));

    // A tall image scrolls the screen so its bottom row is on screen.
    t.advance(b"\x1b[H\x1b_Ga=p,i=1,r=30,q=2\x1b\\");
    let tall = t
        .visible_placements()
        .into_iter()
        .find(|v| v.placement.rows == 30);
    assert_eq!(tall.map(|v| v.row), Some(23 - 29));
    assert_eq!(t.cursor_position().map(|p| p.0), Some(23));

    // On the alternate screen the cursor stops at the bottom instead of scrolling it away.
    t.advance(b"\x1b[?1049h\x1b[3H\x1b_Ga=p,i=1,r=30,q=2\x1b\\");
    let tall = t
        .visible_placements()
        .into_iter()
        .find(|v| v.placement.rows == 30);
    assert_eq!(tall.map(|v| v.row), Some(0));
    t.advance(b"\x1b[?1049l");

    // A failed upload under an image number keeps the number on the old image.
    t.advance(&apc("a=t,f=32,s=1,v=1,I=3,q=2", &[0; 4]));
    let old = t.kitty.numbers[&3];
    t.advance(&apc("a=t,f=32,s=1,v=1,I=3,q=2", &[0; 3]));
    assert_eq!(t.kitty.numbers[&3], old);

    // d=R frees unplaced images in the range too.
    t.advance(&apc("a=t,f=32,s=1,v=1,i=5,q=2", &[0; 4]));
    t.advance(b"\x1b_Ga=d,d=R,x=2,y=6\x1b\\");
    assert!(t.image(5).is_none() && t.image(1).is_some());
}

#[test]
fn padded_payload_fits_exact_limit() {
    let mut tr = super::Transmission {
        control: Default::default(),
        data: Vec::new(),
        carry: Vec::new(),
        limit: 4,
        error: None,
    };
    tr.push(b64(&[1, 2, 3, 4]).as_bytes());
    tr.finish();
    assert_eq!((tr.error, tr.data), (None, vec![1, 2, 3, 4]));
}
