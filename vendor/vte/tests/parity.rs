//! The CSI fast path must perform exactly what upstream vte 0.15.0 performs, for any input and
//! any split across `advance` calls.

mod recorder;

use recorder::assert_parity;

const PIECES: &[&[u8]] = &[
    b"\x1b[",
    b"\x1b[",
    b"\x1b[",
    b"\x1b",
    b"[",
    b"0",
    b"1",
    b"38",
    b"5",
    b"255",
    b"65535",
    b"65536",
    b"999999",
    b";",
    b";",
    b";",
    b":",
    b"?",
    b">",
    b"<",
    b"=",
    b" ",
    b"!",
    b"\"",
    b"$",
    b"m",
    b"H",
    b"J",
    b"K",
    b"A",
    b"r",
    b"h",
    b"l",
    b"q",
    b"@",
    b"~",
    b"`",
    b"p",
    b"\x7f",
    b"\n",
    b"\r",
    b"\x07",
    b"\x08",
    b"\x18",
    b"\x1a",
    b"\x1b]",
    b"\x1bP",
    b"\x1b\\",
    b"\x1b_",
    b"\x1bX",
    b"\x1b(",
    b"\x1b7",
    b"\x9b",
    b"\x9c",
    b"\xc3\xa9",
    b"\xe7\x95",
    b"\x8c",
    b"\xff",
    b"hello",
    b"x",
];

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

fn with_every_cut(data: &[u8]) {
    assert_parity(data, &[]);
    for cut in 1..data.len() {
        assert_parity(data, &[cut]);
    }
    assert_parity(data, &(1..data.len()).collect::<Vec<_>>());
}

#[test]
fn common_sequences() {
    for seq in [
        &b"\x1b[m"[..],
        b"\x1b[0m",
        b"\x1b[1;31m",
        b"\x1b[38;5;196mred\x1b[0m",
        b"\x1b[38;2;255;128;0mx",
        b"\x1b[12;40H\x1b[2J\x1b[K",
        b"\x1b[;H\x1b[;;m\x1b[5;m",
        b"\x1b[?1049h\x1b[?25l\x1b[>c\x1b[=1u",
        b"\x1b[4:3m\x1b[38:2::1:2:3m",
        b"\x1b[ q\x1b[2 q\x1b[!p\x1b[1$p",
        b"\x1b[1;\n2m\x1b[3\x1b[4m\x1b[5\x18m\x1b[6\x7fm",
        b"\x1b[65535m\x1b[65536;99999999m",
        b"\x1b\x1b[1m\x1b\n[2m",
        b"\x1b]0;title\x07\x1b[1m\x1bP1;2qdata\x1b\\\x1b[2m",
        b"\x1b_Ga=q\x1b\\\x1b[3m\x1bXsos\x1b\\\x1b[4m",
        b"\x1b[1\xc3\xa9m\x1b[\x9bm\x1b[\xffm",
    ] {
        with_every_cut(seq);
    }
}

#[test]
fn parameter_limit() {
    for n in 28..36 {
        for last in [&b"m"[..], b";m", b":1m", b"?m"] {
            let mut seq = b"\x1b[".to_vec();
            seq.extend(vec![&b"7"[..]; n].join(&b";"[..]));
            seq.extend_from_slice(last);
            seq.extend_from_slice(b"\x1b[1;2H");
            with_every_cut(&seq);
        }
    }
}

#[test]
fn random_streams() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..20_000 {
        let mut data = Vec::new();
        for _ in 0..rng.below(64) {
            data.extend_from_slice(PIECES[rng.below(PIECES.len())]);
        }
        let mut cuts: Vec<usize> = (0..rng.below(6)).map(|_| rng.below(data.len() + 1)).collect();
        cuts.sort_unstable();
        cuts.dedup();
        assert_parity(&data, &cuts);
    }
}

#[test]
fn random_bytes() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..5_000 {
        let raw: Vec<u8> = (0..rng.below(256)).map(|_| rng.next() as u8).collect();
        let (data, cuts) = recorder::split_input(&raw);
        assert_parity(data, &cuts);
    }
}
