#!/usr/bin/env python3
"""griddiff.py --main WORKTREE --head WORKTREE [--fixtures DIR] [--seeds 200] [--out DIR]

Headless differential for KAN-90: builds a tiny dumper against each worktree's term-core, feeds
the same byte streams through `Terminal::advance` in uneven chunks and compares the full state
(every scrollback and viewport row's cells, public RowFlags and wrap bit, cursor, alt-screen) after each
chunk. Any difference between main and head FAILs (exit 1) with the first differing row.

Streams: the fixtures.py files in --fixtures (absolute paths), vtebench's scrolling benchmarks'
output when --vtebench-dir has them, and --seeds random streams mixing DECSTBM regions, scrolls
(LF/RI/SU/SD/IL/DL), erases (EL/ED/ECH/DCH/ICH), SGR backgrounds (BCE), wide chars, wraps,
alt-screen switches and resizes. Exits 2 if a dumper did not process a stream."""
import argparse
import hashlib
import os
import random
import re
import subprocess
import sys
import tempfile

sys.stdout.reconfigure(line_buffering=True)
p = argparse.ArgumentParser()
p.add_argument("--main", required=True)
p.add_argument("--head", required=True)
p.add_argument("--fixtures", default="")
p.add_argument("--vtebench-dir", default=os.path.expanduser("~/vtebench"))
p.add_argument("--seeds", type=int, default=200)
p.add_argument("--out", default="griddiff")
o = p.parse_args()
out = os.path.abspath(o.out)
os.makedirs(out, exist_ok=True)

DUMPER = r'''
use std::io::{Read, Write};
use term_core::{RowFlags, Terminal};
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (cols, rows): (usize, usize) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let mut bytes = Vec::new();
    std::fs::File::open(&a[1]).unwrap().read_to_end(&mut bytes).unwrap();
    let mut t = Terminal::new(cols, rows);
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    let (mut pos, mut step, mut n) = (0usize, 1usize, 0u64);
    // \x00RESIZE c r\x00 markers are test-harness commands, not terminal input.
    let find = |from: usize| bytes[from..].windows(8).position(|w| w == b"\x00RESIZE ").map(|i| from + i);
    let mut marker = find(0);
    while pos < bytes.len() {
        if marker == Some(pos) {
            let rest = &bytes[pos + 8..];
            let close = rest.iter().position(|&b| b == 0).unwrap();
            let s = std::str::from_utf8(&rest[..close]).unwrap();
            let mut it = s.split(' ').map(|v| v.parse::<usize>().unwrap());
            t.resize(it.next().unwrap(), it.next().unwrap());
            pos += 8 + close + 1;
            marker = find(pos);
        } else {
            let end = (pos + step).min(marker.unwrap_or(bytes.len()));
            t.advance(&bytes[pos..end]);
            pos = end;
        }
        step = step * 7 % 4093 + 1;
        n += 1;
        if n % 16 == 0 || pos >= bytes.len() {
            let mut s = format!("@{} cur={:?} alt={} sb={} rows={} cols={}\n", pos, t.cursor_position(),
                t.is_alt_screen(), t.scrollback_len(), t.rows(), t.cols());
            for r in -(t.scrollback_len() as isize)..t.rows() as isize {
                let row = t.viewport_row_signed(r);
                let f = [RowFlags::PROMPT, RowFlags::PROMPT_START, RowFlags::OUTPUT_START, RowFlags::FAILED,
                    RowFlags::SUCCEEDED].map(|b| row.flags.contains(b) as u8);
                s.push_str(&format!("{} w={} f={:?} {:?}\n", r, WRAPPED(row), f, row.cells));
            }
            out.write_all(s.as_bytes()).unwrap();
        }
    }
    writeln!(out, "END {}", bytes.len()).unwrap();
}
'''


def build(label, wt):
    wt = os.path.abspath(wt)
    core = os.path.join(wt, "crates", "term-core")
    cell = open(os.path.join(core, "src", "cell.rs")).read()
    wrapped = "row.wrapped()" if re.search(r"pub fn wrapped\(&self\)", cell) else "row.wrapped"
    d = os.path.join(out, "dumper-" + label)
    os.makedirs(os.path.join(d, "src"), exist_ok=True)
    open(os.path.join(d, "Cargo.toml"), "w").write(
        '[package]\nname = "griddump"\nversion = "0.0.0"\nedition = "2021"\n[workspace]\n'
        '[dependencies]\nterm-core = { path = "%s" }\n[profile.release]\ndebug-assertions = true\n' % core)
    open(os.path.join(d, "src", "main.rs"), "w").write(DUMPER.replace("WRAPPED(row)", wrapped))
    lock = os.path.join(wt, "Cargo.lock")
    if os.path.exists(lock):
        open(os.path.join(d, "Cargo.lock"), "w").write(open(lock).read())
    subprocess.run(["cargo", "build", "-q", "--release", "--manifest-path", os.path.join(d, "Cargo.toml")], check=True)
    return os.path.join(d, "target", "release", "griddump")


def fuzz(seed):
    r = random.Random(seed)
    cols, rows = r.choice([(100, 30), (80, 24), (37, 11), (255, 40), (300, 50)])
    s = ["\x00RESIZE %d %d\x00" % (cols, rows)]
    for _ in range(r.randint(200, 1500)):
        k = r.random()
        if k < 0.15:
            top = r.randint(1, rows)
            s.append("\x1b[%d;%dr" % (top, r.randint(top, rows)))
        elif k < 0.22:
            s.append("\x1b[%d;%dH" % (r.randint(1, rows), r.randint(1, cols)))
        elif k < 0.40:
            s.append(r.choice(["\n", "\r\n", "\x1bM", "\x1bD", "\x1bE"]) * r.randint(1, 60))
        elif k < 0.48:
            s.append("\x1b[%d%s" % (r.randint(0, rows), r.choice("STLM")))
        elif k < 0.56:
            s.append("\x1b[%d%s" % (r.randint(0, 2), r.choice("KJ")))
        elif k < 0.62:
            s.append("\x1b[%d%s" % (r.randint(1, cols), r.choice("XP@")))
        elif k < 0.70:
            s.append(r.choice(["\x1b[0m", "\x1b[44m", "\x1b[48;5;%dm" % r.randint(0, 255),
                               "\x1b[48;2;1;2;3m", "\x1b[7m", "\x1b[49m", "\x1b[1;31m"]))
        elif k < 0.73:
            s.append(r.choice(["\x1b[?1049h", "\x1b[?1049l", "\x1b[?47h", "\x1b[?47l", "\x1b[?6h", "\x1b[?6l",
                               "\x1b[?7l", "\x1b[?7h", "\x1b7", "\x1b8"]))
        elif k < 0.745:
            cols, rows = r.choice([(100, 30), (80, 24), (37, 11), (255, 40), (300, 50), (5, 3)])
            s.append("\x00RESIZE %d %d\x00" % (cols, rows))
        else:
            alphabet = "y" * 8 + "abc xyz 漢字日本語é" + "\t"
            s.append("".join(r.choice(alphabet) for _ in range(r.choice([1, 2, 5, cols - 1, cols, cols + 3, 3 * cols]))))
    return "".join(s), 100, 30


def streams():
    if o.fixtures:
        fx = os.path.abspath(o.fixtures)
        for name in sorted(os.listdir(fx)):
            if name.endswith(".txt"):
                data = open(os.path.join(fx, name), "rb").read()
                yield name, data[: 2 << 20], 100, 30  # 2 MB is far past steady state
    bdir = os.path.join(os.path.abspath(o.vtebench_dir), "benchmarks")
    for name in ("scrolling_bottom_small_region", "scrolling_top_small_region", "scrolling_bottom_region",
                 "scrolling_top_region", "scrolling_fullscreen", "scrolling"):
        d = os.path.join(bdir, name)
        if not os.path.isdir(d):
            continue
        env = dict(os.environ, LINES="30", COLUMNS="100")
        setup = subprocess.run([os.path.join(d, "setup")], capture_output=True, env=env).stdout if os.path.exists(os.path.join(d, "setup")) else b""
        bench = subprocess.run([os.path.join(d, "benchmark")], capture_output=True, env=env).stdout
        if not bench:
            sys.exit("vtebench benchmark %s produced no output" % name)
        yield "vtebench-" + name, setup + bench * max(1, 200_000 // len(bench)), 100, 30
    for seed in range(o.seeds):
        text, c, r = fuzz(seed)
        yield "fuzz-%d" % seed, text.encode(), c, r


dumpers = {"main": build("main", o.main), "head": build("head", o.head)}
failed, n = False, 0
with tempfile.TemporaryDirectory() as tmp:
    for name, data, cols, rows in streams():
        path = os.path.join(tmp, "stream.bin")
        open(path, "wb").write(data)
        dumps = {}
        for label, exe in dumpers.items():
            r = subprocess.run([exe, path, str(cols), str(rows)], capture_output=True)
            tail = r.stdout[-40:].split(b"\n")
            if r.returncode != 0 or b"END %d" % len(data) not in tail:
                print("%s: %s dumper did not process the stream (exit %d): %s"
                      % (name, label, r.returncode, r.stderr.decode()[-400:]), file=sys.stderr)
                sys.exit(2)
            dumps[label] = r.stdout
        n += 1
        if dumps["main"] != dumps["head"]:
            failed = True
            a, b = dumps["main"].split(b"\n"), dumps["head"].split(b"\n")
            i = next(i for i, (x, y) in enumerate(zip(a + [b""], b + [b""])) if x != y)
            check = next((a[j] for j in range(i, -1, -1) if a[j].startswith(b"@")), b"?")
            open(os.path.join(out, name + ".bin"), "wb").write(data)
            print("FAIL %s at %s\n  main: %s\n  head: %s" % (name, check.decode()[:60], a[i].decode()[:300],
                                                            b[i].decode()[:300] if i < len(b) else "<end>"))
        else:
            print("ok   %s  %s" % (name, hashlib.sha256(dumps["main"]).hexdigest()[:12]))
print("%d streams, %s" % (n, "DIFFERENCES (failing streams saved in %s)" % out if failed else "identical"))
sys.exit(1 if failed else 0)
