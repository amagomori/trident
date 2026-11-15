//! Replays the fuzz harness on stable: every input under `tests/fuzz/` (past crashes and
//! seeds) plus seeded random escape-sequence soup, with the fuzz target's memory check.
//! `TERM_FUZZ_CASES` and `TERM_FUZZ_SEED` widen the random part.

mod harness;

use harness::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn run(name: &str, data: &[u8]) {
    let base = CountingAlloc::live();
    harness::run_with(data, |_, budget| {
        let held = CountingAlloc::live().saturating_sub(base);
        assert!(
            held <= budget,
            "{name}: terminal holds {held} bytes, budget {budget}"
        );
    });
}

const PIECES: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b]",
    b"\x1bP",
    b"\x1b_G",
    b"\x1b\\",
    b"\x07",
    b"\x1b",
    b";",
    b":",
    b"?",
    b"0",
    b"1",
    b"9",
    b"65535",
    b"99999",
    b"m",
    b"H",
    b"J",
    b"K",
    b"r",
    b"h",
    b"l",
    b"@",
    b"L",
    b"M",
    b"P",
    b"S",
    b"T",
    b"X",
    b"b",
    b"t",
    b"n",
    b"\r\n",
    b"\n",
    b"\t",
    b"\x08",
    b"\x0e",
    b"\x0f",
    b"\x1b(0",
    b"\x1b#8",
    b"\x1bM",
    b"\x1bc",
    b"\x1b7",
    b"\x1b8",
    b"?1049h",
    b"?1049l",
    b"?2026h",
    b"?2026l",
    b"?6h",
    b"?7l",
    b"8;;http://x\x07",
    b"133;A\x07",
    b"133;B\x07",
    b"133;C\x07",
    b"133;D;1\x07",
    b"a=T,f=24,s=1,v=1;AAAA",
    b"\xe6\x97\xa5",
    b"\xf0\x9f\x98\x80",
    b"\xcc\x81",
    b"\xe2\x80\x8d",
    b"\xff",
    b"\xc3",
    b"abc",
    b"x",
];

#[test]
fn fuzz_inputs_hold() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fuzz");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    assert!(!entries.is_empty());
    for path in entries {
        run(&path.display().to_string(), &std::fs::read(&path).unwrap());
    }

    let cases: u64 = std::env::var("TERM_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let seed: u64 = std::env::var("TERM_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut state = seed;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for case in 0..cases {
        let mut data: Vec<u8> = (0..4).map(|_| next() as u8).collect();
        let len = 1 + next() % 2000;
        while (data.len() as u64) < len {
            match next() % 4 {
                0 => data.push(next() as u8),
                _ => data.extend_from_slice(PIECES[(next() % PIECES.len() as u64) as usize]),
            }
        }
        run(&format!("seed {seed} case {case}"), &data);
    }
}
