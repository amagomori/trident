#!/usr/bin/env python3
"""fixtures.py [--dir DIR] [--lines 30]
Write the KAN-90 scroll-region PTY A/B fixtures and print their absolute paths.

`--lines` is the window height the fixtures assume (the app opens 100x30 by default); the
regions mirror vtebench `scrolling_bottom_small_region` / `scrolling_top_small_region`."""
import argparse
import os

p = argparse.ArgumentParser()
p.add_argument("--dir", default=".")
p.add_argument("--lines", type=int, default=30)
o = p.parse_args()
d = os.path.abspath(o.dir)
os.makedirs(d, exist_ok=True)
half = o.lines // 2
bottom = "\x1b[?1049h\x1b[%d;%dr" % (half, o.lines)
top = "\x1b[?1049h\x1b[1;%dr" % half
leave = "\x1b[r\x1b[?1049l"
fixtures = {
    # The KAN-90 case: one-character lines scrolling a small bottom region (6 MB).
    "region_bottom.txt": (bottom, "y\n", 3_000_000),
    "region_top.txt": (top, "y\n", 3_000_000),
    # Full-width lines in the region: the bound covers the whole row, so no win expected (30 MB).
    "region_full.txt": (bottom, "%s\n" % ("x" * 99), 300_000),
    # Colored erase template: rows' trailing blanks no longer match it, so reset takes the full path.
    "region_bg.txt": (bottom + "\x1b[44m", "y\n", 3_000_000),
}
for name, (setup, line, n) in fixtures.items():
    with open(os.path.join(d, name), "w") as f:
        f.write(setup)
        for _ in range(n // 1000):
            f.write(line * 1000)
        f.write("\x1b[0m" + leave)
for name in fixtures:
    print(os.path.join(d, name))
