//! Replay the outer PTY (not capture-pane text) and compare each visible source
//! pixel to the original PNG at its expected position in tmux's pane history.
#![cfg(unix)]

use std::{fs, path::Path, process::Command};
use term_core::Terminal;

#[test]
#[ignore = "requires python3 and tmux; run explicitly on the Linux and macOS CI matrix"]
fn real_tmux_restores_inline_pixels() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for kind in ["tall", "fit"] {
        let png = fixtures.join(format!("direct-{kind}-pdf.png"));
        let original = image::open(&png).unwrap().to_rgba8();
        let tmp = tempfile::tempdir().unwrap();
        let capture = Command::new("python3")
            .arg(fixtures.join("tmux_image_history.py"))
            .arg(env!("CARGO_BIN_EXE_trident"))
            .arg(png)
            .arg(tmp.path())
            .output()
            .unwrap();
        assert!(
            capture.status.success(),
            "{}",
            String::from_utf8_lossy(&capture.stderr)
        );
        let mut term = Terminal::new(158, 41);
        term.set_cell_pixel_size(8., 20.);
        let mut recovered = vec![false; original.len() / 4];
        for frame in std::str::from_utf8(&capture.stdout).unwrap().lines() {
            let (name, top) = frame.split_once(' ').unwrap();
            let top: i32 = top.parse().unwrap();
            term.advance(&fs::read(tmp.path().join(name)).unwrap());
            let runs = term.visible_placements();
            if matches!(name, "out" | "exit") {
                assert!(runs.is_empty(), "image over unrelated text: {kind} {name}");
                assert!((0..40).any(|row| {
                    term.viewport_row(row)
                        .cells
                        .iter()
                        .map(|c| term.base_char(c))
                        .collect::<String>()
                        .starts_with("FILL-50")
                }));
                continue;
            }
            let mut live_pixels = 0;
            let mut markers = [false; 3];
            for run in runs {
                assert!((0..40).contains(&run.row), "image outside pane");
                let p = run.placement;
                let slice = run.slice.expect("not a cell-backed image");
                let [l, t, r, b] = slice.destination;
                let [x0, y0, x1, y1] = slice.source;
                let tile = term.image(p.image_id).unwrap();
                assert_eq!((x1 - x0, y1 - y0), ((r - l) * 8., (b - t) * 20.));
                // Image starts on pane history line 1, after BEFORE-IMAGE.
                let left = p.col as i32 * 8 + (l * 8.) as i32;
                let y = (top + run.row as i32 - 1) * 20 + (t * 20.) as i32;
                for dy in 0..(y1 - y0) as i32 {
                    for dx in 0..(x1 - x0) as i32 {
                        let i = ((y0 as usize + dy as usize) * tile.width as usize
                            + x0 as usize
                            + dx as usize)
                            * 4;
                        let rgba = &tile.pixels[i..i + 4];
                        let (x, y) = (left + dx, y + dy);
                        if x < 0
                            || y < 0
                            || x >= original.width() as i32
                            || y >= original.height() as i32
                        {
                            assert_eq!(rgba[3], 0, "stray pixels: {kind} {name}");
                            continue;
                        }
                        assert_eq!(
                            rgba,
                            original.get_pixel(x as u32, y as u32).0,
                            "{kind} {name} at {x},{y}"
                        );
                        for (i, rgb) in [[255, 0, 0], [0, 255, 0], [0, 0, 255]].iter().enumerate() {
                            markers[i] |= rgba[..3] == *rgb;
                        }
                        if name == "live" {
                            live_pixels += 1;
                        }
                        if name.starts_with("copy") {
                            recovered[y as usize * original.width() as usize + x as usize] = true;
                        }
                    }
                }
            }
            if name == "live" {
                let first_y = ((top - 1) * 20).max(0) as u32;
                let end_y = (((top + 40 - 1) * 20) as u32).min(original.height());
                assert_eq!(
                    live_pixels,
                    original.width() * (end_y - first_y),
                    "{kind} live coverage"
                );
                assert_eq!(
                    markers,
                    [kind == "fit", kind == "fit", true],
                    "{kind} live markers"
                );
            }
        }
        assert!(
            recovered.iter().all(|p| *p),
            "unrecovered {kind} source pixels"
        );
    }
}
