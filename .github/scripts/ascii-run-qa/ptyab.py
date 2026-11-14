#!/usr/bin/env python3
"""ptyab.py --a MAIN_BIN --b HEAD_BIN --fixture dense.txt [--pairs 10]
Wall time from launch to exit for `terminal -e cat FIXTURE`, interleaved A/A and A/B pairs.

The app starts `-e` children in $HOME, so the fixture is passed as an absolute path. Every
run checks that `cat` exited 0 inside the app and exits 2 if it did not."""
import argparse
import os
import random
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument("--a", required=True)
p.add_argument("--b", required=True)
p.add_argument("--fixture", required=True)
p.add_argument("--pairs", type=int, default=10)
o = p.parse_args()
fixture = os.path.abspath(o.fixture)
if not os.path.isfile(fixture):
    sys.exit("fixture not found: %s" % fixture)
size = os.path.getsize(fixture)
work = tempfile.mkdtemp(prefix="ptyab-")
a2 = os.path.join(work, "terminal-a-copy")
shutil.copy2(o.a, a2)  # A/A uses a copy of A
marker = os.path.join(work, "status")
# cat writes straight to the app's PTY; the marker gets its exit status from inside the app.
script = 'cat -- "$1"; echo $? > "$2"'


def run(binary):
    if os.path.exists(marker):
        os.remove(marker)
    t = time.perf_counter()
    subprocess.run([os.path.abspath(binary), "-e", "/bin/sh", "-c", script, "ptyab", fixture, marker],
                   check=True, timeout=600)
    elapsed = time.perf_counter() - t
    try:
        with open(marker) as f:
            status = f.read().strip() or "empty"
    except OSError:
        status = "missing"
    if status != "0":
        print("workload did not run in %s: cat status %s after %.2f s" % (binary, status, elapsed), file=sys.stderr)
        sys.exit(2)
    return elapsed


def pairs(x, y):
    d = []
    for _ in range(o.pairs):
        order = [(x, 0), (y, 1)]
        random.shuffle(order)
        r = {i: run(b) for b, i in order}
        d.append((r[1] - r[0]) / r[0] * 100)
    return d


def ci(d):
    m = sorted(statistics.median(random.choices(d, k=len(d))) for _ in range(2000))
    return statistics.median(d), m[50], m[1949]


try:
    warm = [run(o.a), run(o.b)]  # warm caches
    aa, ab = pairs(o.a, a2), pairs(o.a, o.b)
finally:
    shutil.rmtree(work, ignore_errors=True)
print("fixture %s (%d bytes), warm-up runs %.2f s / %.2f s" % (fixture, size, *warm))
print("A/A delta %%: median %.1f, 95%% CI [%.1f, %.1f]" % ci(aa))
print("A/B delta %%: median %.1f, 95%% CI [%.1f, %.1f]" % ci(ab))
print("A/A noise bound: +/-%.1f%%" % max(abs(x) for x in aa))
