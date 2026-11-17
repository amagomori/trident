#!/usr/bin/env python3
"""Write dense ASCII, mixed-script and CJK fixtures for release PTY benchmarks.

Usage: fixtures.py [--dir DIR]
Print each fixture's absolute path. Files are approximately 10-60 MB each.
"""
import argparse
import os
import random

p = argparse.ArgumentParser()
p.add_argument("--dir", default=".")
o = p.parse_args()
d = os.path.abspath(o.dir)
os.makedirs(d, exist_ok=True)
random.seed(1)
with open(os.path.join(d, "dense.txt"), "w") as f:
    for i in range(400000):
        f.write(("%08d " % i) + "x" * 140 + "\n")
with open(os.path.join(d, "mixed.txt"), "w") as f:
    for i in range(300000):
        f.write("\x1b[34m╭─ jarett@host ~/projects/café ─╮\x1b[0m -rw-r--r-- 1 jarett staff %6d naïve_résumé_%d.txt\n" % (i, i))
with open(os.path.join(d, "cjk.txt"), "w") as f:
    for i in range(300000):
        f.write("日本語のテキスト%06d 漢字かな交じり文\n" % i)
for name in ("dense.txt", "mixed.txt", "cjk.txt"):
    print(os.path.join(d, name))
