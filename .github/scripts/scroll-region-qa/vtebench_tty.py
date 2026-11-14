"""Copies of vtebench's benchmarks that find the terminal size on macOS and without a tty.

Upstream scripts read the size with `tty="/dev/$(ps -o tty= -p $$)"; tput lines < $tty`.
BSD ps pads the name with a space (no such device), ps prints `??`/`?` with no controlling tty,
and macOS tput sizes from stderr, which vtebench captures. Each failure still exits 0, so a setup
emits `ESC[1;0r` instead of a small region. The copies read `stty size` from $VTEBENCH_TTY
(default /dev/tty); check_setup / check_benchmark verify what a script emitted at rows x cols, and
run_sized runs a script outside any terminal against a pty of that size."""
import fcntl
import os
import pty
import re
import shutil
import struct
import subprocess
import termios

TTY = '"${VTEBENCH_TTY:-/dev/tty}"'
SUBS = [
    (re.escape('tty="/dev/$(ps -o tty= -p $$)"'), 'tty=%s; size=$(stty size < "$tty")' % TTY),
    (re.escape("$(tput lines < $tty)"), "${size%% *}"),
    (re.escape("$(tput cols < $tty)"), "${size##* }"),
    (re.escape("$(tput lines)"), "$(stty size < %s | cut -d' ' -f1)" % TTY),
]
# What each setup must emit at a rows-line terminal; setups not listed must exit 0 silently on stderr.
SETUPS = {
    "scrolling_bottom_region": lambda rows: "\x1b[?1049h\x1b[1;%dr" % (rows - 1),
    "scrolling_bottom_small_region": lambda rows: "\x1b[?1049h\x1b[1;%dr" % (rows // 2),
    "scrolling_top_region": lambda rows: "\x1b[?1049h\x1b[2;%dr" % rows,
    "scrolling_top_small_region": lambda rows: "\x1b[?1049h\x1b[%d;%dr" % (rows // 2, rows),
    "dense_cells": lambda rows: "\x1b[?1049h",
    "light_cells": lambda rows: "\x1b[?1049h",
    "unicode": lambda rows: "\x1b[?1049h",
}
# A property of each size-dependent benchmark's output that only holds at the right size.
BENCHMARKS = {
    "cursor_motion": lambda out, rows, cols: b"\x1b[%d;%dHA" % (rows, cols) in out
                                             and b"\x1b[%d;%dHA" % (rows + 1, cols) not in out,
    "dense_cells": lambda out, rows, cols: out.count(b"mA") == rows * cols,
    "light_cells": lambda out, rows, cols: out.startswith(b"\x1b[H" + b"A" * (rows * cols) + b"\x1b[H"),
    "scrolling_fullscreen": lambda out, rows, cols: out.startswith(b"A" * cols + b"\n"),
}


def patch(src_root, names, dst_root):
    """Copies src_root/<name> for each name into dst_root and rewrites the size lookups."""
    for n in names:
        d = os.path.join(dst_root, n)
        shutil.copytree(os.path.join(src_root, n), d)
        for s in ("setup", "benchmark"):
            f = os.path.join(d, s)
            if not os.path.isfile(f):
                continue
            text = open(f).read()
            for pat, rep in SUBS:
                text = re.sub(pat, lambda _m, rep=rep: rep, text)
            if re.search(r"\btput\b|ps -o tty", text):
                raise SystemExit("vtebench %s/%s has a size lookup vtebench_tty.py does not rewrite" % (n, s))
            open(f, "w").write(text)
    return [os.path.join(dst_root, n) for n in names]


def check_setup(name, rc, out, err, rows):
    """Returns why a setup's run is wrong, or None."""
    if rc != 0 or err:
        return "%s/setup exited %s: %r" % (name, rc, err[-200:])
    want = SETUPS.get(name)
    if want and out != want(rows).encode():
        return "%s/setup emitted %r, not %r at %d rows" % (name, out[:60], want(rows).encode(), rows)
    return None


def check_benchmark(name, rc, out, err, rows, cols):
    if rc != 0 or err or not out:
        return "%s/benchmark exited %s with %d bytes: %r" % (name, rc, len(out), err[-200:])
    ok = BENCHMARKS.get(name)
    if ok and not ok(out, rows, cols):
        return "%s/benchmark output is not sized %dx%d" % (name, rows, cols)
    return None


def run_sized(path, rows, cols):
    """Runs a patched script with stdin closed and a rows x cols pty as $VTEBENCH_TTY: (rc, out, err)."""
    master, slave = pty.openpty()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        r = subprocess.run([path], stdin=subprocess.DEVNULL, capture_output=True,
                           env=dict(os.environ, VTEBENCH_TTY=os.ttyname(slave)))
    finally:
        os.close(slave)
        os.close(master)
    return r.returncode, r.stdout, r.stderr
