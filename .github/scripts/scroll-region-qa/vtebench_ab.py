#!/usr/bin/env python3
"""vtebench_ab.py --main BIN --head BIN --alacritty BIN [--vtebench BIN] [--vtebench-dir DIR]
                  [--rounds 8] [--max-secs 3] [--benchmarks a,b] [--out results.json]

Runs the vtebench suite inside main, a byte copy of main (A/A), head and Alacritty, in shuffled
rounds, all at 100x30 with the configs in cfg/. Per round and benchmark it takes the median
sample (ms); deltas are per-round paired % deltas, reported as median and 95% bootstrap CI.

KAN-90 verdicts:
  * scrolling_bottom_small_region: head within noise of Alacritty (median head-vs-Alacritty delta
    no larger than the A/A noise, the largest |end| of the A/A CI), MET / NOT MET.
  * every benchmark: head not slower than main (REGRESSION when the head-vs-main CI lies wholly
    above the A/A noise).
Exits 2 when a run did not produce samples for every benchmark (vtebench failed, the
terminal exited early, or the path was wrong), 1 when a verdict failed, 0 otherwise."""
import argparse
import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = "scrolling_bottom_small_region"

sys.stdout.reconfigure(line_buffering=True)
p = argparse.ArgumentParser()
p.add_argument("--main", required=True)
p.add_argument("--head", required=True)
p.add_argument("--alacritty", required=True)
p.add_argument("--vtebench", default=shutil.which("vtebench") or "vtebench")
p.add_argument("--vtebench-dir", default=os.path.expanduser("~/vtebench"))
p.add_argument("--rounds", type=int, default=8)
p.add_argument("--max-secs", type=int, default=3)
p.add_argument("--min-samples", type=int, default=3)
p.add_argument("--benchmarks", default="", help="comma list; default: every vtebench benchmark")
p.add_argument("--out", default="vtebench_ab.json")
o = p.parse_args()

vtebench = shutil.which(o.vtebench) or o.vtebench
bench_root = os.path.join(os.path.abspath(os.path.expanduser(o.vtebench_dir)), "benchmarks")
if not os.access(vtebench, os.X_OK) or not os.path.isdir(bench_root):
    sys.exit("need the vtebench binary (--vtebench) and a vtebench checkout (--vtebench-dir)")
names = [n for n in o.benchmarks.split(",") if n] or sorted(
    n for n in os.listdir(bench_root) if os.path.isfile(os.path.join(bench_root, n, "benchmark")))
dirs = [os.path.join(bench_root, n) for n in names]
for d in dirs:
    if not os.path.isfile(os.path.join(d, "benchmark")):
        sys.exit("no such vtebench benchmark: %s" % d)

work = tempfile.mkdtemp(prefix="vteab-")
main_copy = os.path.join(work, "terminal-main-copy")
shutil.copy2(o.main, main_copy)
term_cfg = os.path.join(HERE, "cfg", "terminal.toml")
alac_cfg = os.path.join(HERE, "cfg", "alacritty.toml")
TERMS = {
    "main": [os.path.abspath(o.main), "--config", term_cfg],
    "main_copy": [main_copy, "--config", term_cfg],
    "head": [os.path.abspath(o.head), "--config", term_cfg],
    "alacritty": [os.path.abspath(o.alacritty), "--config-file", alac_cfg],
}
# Runs inside the terminal; the marker gets vtebench's exit status and the PTY size it saw.
script = ('vb=$1; dat=$2; mk=$3; secs=$4; shift 4; "$vb" -s --max-secs "$secs" --dat "$dat" "$@"; '
          'rc=$?; echo "$rc $(stty size)" > "$mk"')
timeout = len(dirs) * (o.max_secs + 2) * 4 + 120


def parse_dat(path):
    lines = [l.split() for l in open(path).read().splitlines() if l.strip()]
    head, rows = lines[0], lines[1:]
    res = {}
    for i, bench in enumerate(head):
        vals = []
        for r in rows:
            try:
                vals.append(float(r[i]))
            except (IndexError, ValueError):
                pass
        res[bench] = vals
    return res


def run(label):
    dat, mk = os.path.join(work, label + ".dat"), os.path.join(work, label + ".status")
    for f in (dat, mk):
        if os.path.exists(f):
            os.remove(f)
    args = [a for d in dirs for a in ("-b", d)]
    t = time.perf_counter()
    subprocess.run(TERMS[label] + ["-e", "/bin/sh", "-c", script, "vteab", vtebench, dat, mk,
                                   str(o.max_secs), *args],
                   timeout=timeout, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    status = open(mk).read().split() if os.path.exists(mk) else ["missing"]
    samples = parse_dat(dat) if os.path.exists(dat) else {}
    short = [n for n in names if len(samples.get(n, [])) < o.min_samples]
    if status[0] != "0" or short:
        print("workload did not run in %s: vtebench status %s, too few samples for %s (%.1f s)"
              % (label, " ".join(status), ",".join(short) or "-", time.perf_counter() - t), file=sys.stderr)
        sys.exit(2)
    if status[1:] != ["30", "100"]:
        print("%s ran at %s rows x cols, not 30 x 100" % (label, "x".join(status[1:])), file=sys.stderr)
        sys.exit(2)
    return {n: statistics.median(samples[n]) for n in names}


def ci(d):
    m = sorted(statistics.median(random.choices(d, k=len(d))) for _ in range(4000))
    return statistics.median(d), m[100], m[3899]


try:
    for label in TERMS:  # warm-up, also proves every terminal runs the suite
        run(label)
    rounds = []
    for i in range(o.rounds):
        order = list(TERMS)
        random.shuffle(order)
        rounds.append({label: run(label) for label in order})
        print("round %d/%d done" % (i + 1, o.rounds), file=sys.stderr)
finally:
    shutil.rmtree(work, ignore_errors=True)

failed = False
res = {"benchmarks": {}, "rounds": rounds}
print("%-32s %9s %9s %9s %18s %18s %18s" % ("benchmark (median ms)", "main", "head", "alacritty",
                                             "A/A %", "head vs main %", "head vs alac %"))
for n in names:
    def delta(x, y):
        return ci([(r[x][n] - r[y][n]) / r[y][n] * 100 for r in rounds])
    aa, hm, ha, ma = delta("main_copy", "main"), delta("head", "main"), delta("head", "alacritty"), delta("main", "alacritty")
    noise = max(abs(aa[1]), abs(aa[2]))
    med = {k: statistics.median(r[k][n] for r in rounds) for k in TERMS}
    verdicts = []
    if hm[1] > noise:
        verdicts.append("REGRESSION vs main")
        failed = True
    if n == TARGET:
        met = ha[0] <= noise
        verdicts.append("within noise of Alacritty: %s" % ("MET" if met else "NOT MET"))
        failed |= not met
    fmt = lambda c: "%+.1f [%+.1f,%+.1f]" % c
    print("%-32s %9.2f %9.2f %9.2f %18s %18s %18s  %s" % (n, med["main"], med["head"], med["alacritty"],
                                                         fmt(aa), fmt(hm), fmt(ha), "; ".join(verdicts)))
    res["benchmarks"][n] = {"median_ms": med, "aa": aa, "head_vs_main": hm, "head_vs_alacritty": ha,
                            "main_vs_alacritty": ma, "noise": noise, "verdicts": verdicts}
json.dump(res, open(o.out, "w"), indent=1)
print("results: %s" % os.path.abspath(o.out))
sys.exit(1 if failed else 0)
