//! Benchmark workloads for terminal state, run by `.github/scripts/bench.py` to compare a
//! change against main. Ignored by `cargo test`; run them directly with
//! `cargo test --release --workspace -- --ignored --nocapture bench_workloads`.
//!
//! Each workload prints one `BENCH {json}` line. `TERM_BENCH` picks workloads by name
//! (comma-separated, default all) and `TERM_BENCH_ITERS` sets the timed repetitions.
//! Only the `measured_*` functions are counted when CI runs this under callgrind.

use super::Terminal;
use std::hint::black_box;
use std::time::Instant;

const COLS: usize = 200;
const ROWS: usize = 50;
const INPUT_BYTES: usize = 2 << 20;
const KEYSTROKES: usize = 2_000;

/// xorshift64, so every run feeds identical input.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn fill(mut chunk: impl FnMut(&mut Rng, &mut Vec<u8>)) -> Vec<u8> {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut out = Vec::with_capacity(INPUT_BYTES + 4096);
    while out.len() < INPUT_BYTES {
        chunk(&mut rng, &mut out);
    }
    out
}

fn printable(rng: &mut Rng) -> u8 {
    b' ' + rng.below(95) as u8
}

/// Long lines of plain ASCII that soft-wrap, like `cat` of a log file.
fn dense_ascii() -> Vec<u8> {
    fill(|rng, out| {
        let len = 40 + rng.below(400);
        out.extend((0..len).map(|_| printable(rng)));
        out.extend_from_slice(b"\r\n");
    })
}

/// Short lines that scroll the screen and fill scrollback, like `seq` or build output.
fn scrolling() -> Vec<u8> {
    fill(|rng, out| {
        let len = rng.below(30);
        out.extend((0..len).map(|_| printable(rng)));
        out.extend_from_slice(b"\r\n");
    })
}

/// A color change every few characters: 16, 256 and truecolor SGR plus bold/italic/reset.
fn colored_text() -> Vec<u8> {
    fill(|rng, out| {
        match rng.below(5) {
            0 => out.extend(format!("\x1b[{}m", 30 + rng.below(8)).bytes()),
            1 => out.extend(format!("\x1b[38;5;{}m", rng.below(256)).bytes()),
            2 => out.extend(
                format!(
                    "\x1b[38;2;{};{};{}m",
                    rng.below(256),
                    rng.below(256),
                    rng.below(256)
                )
                .bytes(),
            ),
            3 => out.extend_from_slice(b"\x1b[1;3;48;5;236m"),
            _ => out.extend_from_slice(b"\x1b[0m"),
        }
        let len = 1 + rng.below(8);
        out.extend((0..len).map(|_| printable(rng)));
        if rng.below(20) == 0 {
            out.extend_from_slice(b"\r\n");
        }
    })
}

/// Absolute cursor moves followed by short writes and erases, like htop or a vim redraw.
fn cursor_motion() -> Vec<u8> {
    fill(|rng, out| {
        let row = 1 + rng.below(ROWS);
        let col = 1 + rng.below(COLS);
        out.extend(format!("\x1b[{row};{col}H").bytes());
        let len = 1 + rng.below(16);
        out.extend((0..len).map(|_| printable(rng)));
        if rng.below(4) == 0 {
            out.extend_from_slice(b"\x1b[K");
        }
    })
}

/// Full-screen repaints on the alternate screen inside a scroll region, like `less` or tmux.
fn alt_screen() -> Vec<u8> {
    let mut first = true;
    fill(|rng, out| {
        if first {
            out.extend(format!("\x1b[?1049h\x1b[2;{}r", ROWS - 1).bytes());
            first = false;
        }
        out.extend_from_slice(b"\x1b[H\x1b[2J");
        for row in 1..=ROWS {
            out.extend(format!("\x1b[{row};1H\x1b[7m{row:>4}\x1b[27m ").bytes());
            let len = rng.below(COLS - 5);
            out.extend((0..len).map(|_| printable(rng)));
        }
        out.extend(format!("\x1b[{};1H\x1bM\x1b[3S\x1b[2T", ROWS / 2).bytes());
    })
}

/// Wide CJK characters, emoji and accented Latin mixed with ASCII.
fn unicode() -> Vec<u8> {
    const WORDS: &[&str] = &[
        "日本語",
        "中文字符",
        "한국어",
        "🦀",
        "✓",
        "naïve",
        "über",
        "→",
        "😀😀",
    ];
    fill(|rng, out| {
        for _ in 0..12 {
            out.extend_from_slice(WORDS[rng.below(WORDS.len())].as_bytes());
            out.push(b' ');
        }
        out.extend_from_slice(b"\r\n");
    })
}

#[inline(never)]
fn measured_advance(term: &mut Terminal, input: &[u8]) {
    term.advance(black_box(input));
}

/// One keystroke echoed by the shell, then the walk over every visible cell that the
/// renderer does to build the next frame: the CPU side of keypress-to-frame latency.
#[inline(never)]
fn measured_keystrokes(term: &mut Terminal, count: usize) {
    for i in 0..count {
        if i % 80 == 79 {
            term.advance(black_box(b"\r\n$ "));
        } else {
            term.advance(black_box(b"x"));
        }
        let mut visible = 0usize;
        for row in 0..term.rows() {
            for cell in &term.viewport_row(row).cells {
                visible = visible.wrapping_add(cell.c as usize);
            }
        }
        black_box(visible);
        black_box(term.cursor_position());
    }
}

enum Kind {
    Throughput(fn() -> Vec<u8>),
    Keystrokes,
}

const WORKLOADS: &[(&str, Kind)] = &[
    ("dense_ascii", Kind::Throughput(dense_ascii)),
    ("scrolling", Kind::Throughput(scrolling)),
    ("colored_text", Kind::Throughput(colored_text)),
    ("cursor_motion", Kind::Throughput(cursor_motion)),
    ("alt_screen", Kind::Throughput(alt_screen)),
    ("unicode", Kind::Throughput(unicode)),
    ("keystroke_latency", Kind::Keystrokes),
];

fn median(secs: &mut [f64]) -> f64 {
    secs.sort_by(f64::total_cmp);
    secs[secs.len() / 2]
}

#[test]
#[ignore = "benchmark; run by .github/scripts/bench.py"]
fn bench_workloads() {
    let selected = std::env::var("TERM_BENCH").unwrap_or_default();
    let iters: usize = std::env::var("TERM_BENCH_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5)
        .max(1);
    for (name, kind) in WORKLOADS {
        if !selected.is_empty() && !selected.split(',').any(|s| s == *name) {
            continue;
        }
        let (unit, count, mut secs) = match kind {
            Kind::Throughput(generate) => {
                let input = generate();
                let secs: Vec<f64> = (0..iters)
                    .map(|_| {
                        let mut term = Terminal::new(COLS, ROWS);
                        let start = Instant::now();
                        measured_advance(&mut term, &input);
                        start.elapsed().as_secs_f64()
                    })
                    .collect();
                ("bytes", input.len(), secs)
            }
            Kind::Keystrokes => {
                let screen = dense_ascii();
                let secs: Vec<f64> = (0..iters)
                    .map(|_| {
                        let mut term = Terminal::new(COLS, ROWS);
                        term.advance(&screen);
                        let start = Instant::now();
                        measured_keystrokes(&mut term, KEYSTROKES);
                        start.elapsed().as_secs_f64()
                    })
                    .collect();
                ("keystrokes", KEYSTROKES, secs)
            }
        };
        println!(
            "BENCH {{\"name\":\"{name}\",\"unit\":\"{unit}\",\"count\":{count},\"secs\":{:.9}}}",
            median(&mut secs)
        );
    }
}
