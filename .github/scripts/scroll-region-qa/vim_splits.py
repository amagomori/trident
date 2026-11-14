#!/usr/bin/env python3
"""vim_splits.py --bin main=BIN --bin head=BIN [--out DIR]

Visual check of vim split windows, Linux (X11: xdotool + ImageMagick `import`) and macOS
(System Events + screencapture). Each build runs `vim -u NONE` as the terminal's own command on a
fixed corpus and scrolls its windows (`:split` scrolls through DECSTBM scroll regions, `:vsplit`
redraws) in three variants: default colors, a colored Normal background with cursorline, and
nowrap with `list`. At each checkpoint vim stops, the window is captured (`NAME-a.png`), vim runs
`:redraw!` (a full repaint from vim's own state) and the window is captured again (`NAME-b.png`).
Captures are the 100x30 cell grid only (cfg/terminal.toml, without the title bar, padding and
window corners), with the cursor hidden, so focus and cursor state cannot show up as a difference.

FAIL when a checkpoint's incremental capture differs from its full repaint by any pixel: a stale
or wrongly blanked cell from the bounded Row::reset shows up there. Differences between builds
at the same checkpoint are reported as WARN. Diff images (`NAME-diff.png`, red = differing
pixels) are written next to the captures. Exits 2 if vim never reached a checkpoint."""
import argparse
import os
import platform
import re
import shutil
import struct
import subprocess
import sys
import time
import zlib

MAC = platform.system() == "Darwin"
sys.stdout.reconfigure(line_buffering=True)
p = argparse.ArgumentParser()
p.add_argument("--bin", action="append", required=True, help="LABEL=PATH, e.g. main=../t-main/target/release/terminal")
p.add_argument("--out", default="vim-splits")
p.add_argument("--vim", default=shutil.which("vim") or "/usr/bin/vim")
p.add_argument("--settle", type=float, default=0.8, help="seconds to let a frame render before capture")
o = p.parse_args()
out = os.path.abspath(o.out)
CFG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "cfg", "terminal.toml")
PAD = int(re.search(r"^padding\s*=\s*(\d+)", open(CFG).read(), re.M).group(1))
os.makedirs(out, exist_ok=True)

CHECKPOINTS = ["plain", "bg", "nowrap"]
DRIVE = r'''
set nocompatible laststatus=2 ruler noshowmode shortmess+=I belloff=all t_vb=
" The cursor stays hidden from the first checkpoint on: empty t_ve and t_vs keep vim from showing it.
let &t_ve = ''
let &t_vs = ''
let s:dir = $QA_DIR
function! s:Wait(name)
  call writefile(['x'], s:dir . '/' . a:name)
  let n = 0
  while !filereadable(s:dir . '/' . a:name . '.ack') && n < 1200
    sleep 50m
    let n += 1
  endwhile
endfunction
function! s:Step(keys, times)
  for i in range(a:times)
    execute 'normal! ' . a:keys
    redraw
  endfor
endfunction
function! s:Exercise()
  for w in range(1, winnr('$'))
    execute w . 'wincmd w'
    call s:Step("\<C-E>", 120)
    call s:Step("\<C-Y>", 50)
    call s:Step("j", 40)
    call s:Step("\<C-D>", 4)
    call s:Step("\<C-U>", 2)
    call s:Step("3\<C-E>", 10)
  endfor
  1wincmd w
endfunction
function! s:Checkpoint(name)
  redraw
  call echoraw("\e[?25l")
  call s:Wait(a:name . '-a')
  redraw!
  call echoraw("\e[?25l")
  call s:Wait(a:name . '-b')
endfunction
split
vsplit
call s:Exercise()
call s:Checkpoint('plain')
highlight Normal ctermfg=230 ctermbg=24
highlight CursorLine cterm=NONE ctermbg=52
set cursorline
call s:Exercise()
call s:Checkpoint('bg')
highlight Normal NONE
set nocursorline nowrap list listchars=tab:>-,trail:~,extends:> sidescroll=1
call s:Exercise()
call s:Step("40zl", 1)
call s:Step("5zh", 6)
call s:Checkpoint('nowrap')
call writefile(['x'], s:dir . '/done')
qa!
'''


def corpus(path):
    words = ["alpha", "beta", "gamma", "delta", "日本語", "漢字", "café", "naïve", "\t", "x" * 30]
    with open(path, "w") as f:
        for i in range(600):
            n = (i * 37) % 23
            line = "%04d " % i + " ".join(words[(i + k) % len(words)] for k in range(n))
            if i % 7 == 0:
                line += " " * (i % 5)  # trailing spaces
            if i % 11 == 0:
                line = ""
            if i % 13 == 0:
                line = "%04d " % i + "-" * 180  # wraps in every window
            f.write(line + "\n")


def png_read(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    pos, idat, ihdr = 8, b"", None
    while pos < len(data):
        n, kind = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + n]
        if kind == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        pos += 12 + n
    w, h, depth, ctype, _, _, interlace = ihdr
    if depth != 8 or ctype not in (2, 6) or interlace:
        sys.exit("%s: unsupported PNG (depth %d, color type %d, interlace %d)" % (path, depth, ctype, interlace))
    bpp = 3 if ctype == 2 else 4
    raw, stride = zlib.decompress(idat), w * bpp
    rows, prev = [], bytearray(stride)
    for y in range(h):
        f, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b, c = prev[x], prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(b"".join(line[i:i + 3] for i in range(0, stride, bpp)))
        prev = line
    return w, h, rows


def png_write(path, w, h, rows):
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))
    raw = b"".join(b"\0" + r for r in rows)
    open(path, "wb").write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                           + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def diff(a, b, path):
    wa, ha, ra = png_read(a)
    wb, hb, rb = png_read(b)
    if (wa, ha) != (wb, hb):
        return "size %dx%d vs %dx%d" % (wa, ha, wb, hb)
    n, box, rows = 0, None, []
    for y in range(ha):
        if ra[y] == rb[y]:
            rows.append(bytes(v // 3 for v in ra[y]))
            continue
        line = bytearray(v // 3 for v in ra[y])
        for x in range(wa):
            if ra[y][3 * x:3 * x + 3] != rb[y][3 * x:3 * x + 3]:
                n += 1
                line[3 * x:3 * x + 3] = b"\xff\x00\x00"
                box = [min(box[0], x), min(box[1], y), max(box[2], x), max(box[3], y)] if box else [x, y, x, y]
        rows.append(bytes(line))
    if n:
        png_write(path, wa, ha, rows)
        return "%d pixels differ in x %d..%d, y %d..%d (%s)" % (n, box[0], box[2], box[1], box[3], path)
    return None


def window(pid):
    end = time.time() + 30
    while time.time() < end:
        if MAC:
            # The close button is centered in the title bar, so the bar is 2 * its inset + its height.
            r = subprocess.run(["osascript", "-e", 'tell application "System Events" to tell (first process whose unix id is %d)\n'
                                'set frontmost to true\nset w to window 1\n'
                                'set b to first button of w whose subrole is "AXCloseButton"\n'
                                'get {position, size} of w & {position, size} of b\nend tell' % pid],
                               capture_output=True, text=True)
            if r.returncode == 0 and r.stdout.strip():
                x, y, w, h, _, by, _, bh = [int(v) for v in r.stdout.replace(",", " ").split()]
                return x, y, w, h, 2 * (by - y) + bh
        else:
            r = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(pid)], capture_output=True, text=True)
            if r.stdout.split():
                wid = r.stdout.split()[-1]
                subprocess.run(["xdotool", "windowraise", wid], capture_output=True)
                return wid
        time.sleep(0.3)
    sys.exit("no window for pid %d" % pid)


def capture(win, path):
    """Captures the cell grid: the window's content area inside the configured padding. Vim always
    draws text, so a single-color capture is a frame the window had not presented yet: retried, then exit 2."""
    for _ in range(5):
        if not blank(grab(win, path)):
            return
        time.sleep(o.settle)
    print("%s: the window stayed blank" % path, file=sys.stderr)
    sys.exit(2)


def blank(rows):
    return len(set(rows)) == 1 and rows[0] == rows[0][:3] * (len(rows[0]) // 3)


def grab(win, path):
    if MAC:
        x, y, w, h, title = win
        subprocess.run(["screencapture", "-x", "-t", "png", "-R", "%d,%d,%d,%d" % (x, y + title, w, h - title), path],
                       check=True)
        scale = png_size(path)[0] / w
    else:
        subprocess.run(["import", "-window", win, "PNG24:" + path], check=True)
        scale = 1
    pad = round(PAD * scale)
    cw, ch, rows = png_read(path)
    rows = [r[3 * pad:3 * (cw - pad)] for r in rows[pad:ch - pad]]
    png_write(path, cw - 2 * pad, ch - 2 * pad, rows)
    return rows


def png_size(path):
    return struct.unpack(">II", open(path, "rb").read()[16:24])


def run(label, binary):
    d = os.path.join(out, label)
    shutil.rmtree(d, ignore_errors=True)
    os.makedirs(d)
    drive, text = os.path.join(d, "drive.vim"), os.path.join(out, "corpus.txt")
    open(drive, "w").write(DRIVE)
    corpus(text)  # one path for every build: vim's status lines show it
    env = dict(os.environ, QA_DIR=d)
    proc = subprocess.Popen([os.path.abspath(binary), "--config", CFG, "-e", o.vim, "-u", "NONE", "-i", "NONE", "-N", "-n",
                             "-S", drive, text], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    win = window(proc.pid)
    shots = {}
    try:
        for cp in CHECKPOINTS:
            for half in "ab":
                name = "%s-%s" % (cp, half)
                end = time.time() + 120
                while not os.path.exists(os.path.join(d, name)):
                    if time.time() > end or proc.poll() is not None:
                        print("%s: vim never reached checkpoint %s" % (label, name), file=sys.stderr)
                        sys.exit(2)
                    time.sleep(0.05)
                if MAC:
                    win = window(proc.pid)  # raising it may refocus and repaint it, so settle after
                time.sleep(o.settle)
                shots[name] = os.path.join(out, "%s-%s.png" % (label, name))
                capture(win, shots[name])
                open(os.path.join(d, name + ".ack"), "w").close()
        end = time.time() + 30
        while not os.path.exists(os.path.join(d, "done")) and time.time() < end:
            time.sleep(0.05)
        if not os.path.exists(os.path.join(d, "done")):
            print("%s: vim did not finish" % label, file=sys.stderr)
            sys.exit(2)
        proc.wait(30)
    finally:
        if proc.poll() is None:
            proc.kill()
    return shots


bins = [b.split("=", 1) for b in o.bin]
results, failed = {}, False
for label, binary in bins:
    results[label] = run(label, binary)
    for cp in CHECKPOINTS:
        a, b = results[label][cp + "-a"], results[label][cp + "-b"]
        r = diff(a, b, os.path.join(out, "%s-%s-diff.png" % (label, cp)))
        print("%-6s %-7s incremental vs :redraw!  %s" % (label, cp, "FAIL: " + r if r else "identical"))
        failed |= bool(r)
for (la, _), (lb, _) in zip(bins, bins[1:]):
    for cp in CHECKPOINTS:
        r = diff(results[la][cp + "-a"], results[lb][cp + "-a"], os.path.join(out, "%s-vs-%s-%s-diff.png" % (la, lb, cp)))
        print("%-6s vs %-6s %-7s %s" % (la, lb, cp, "WARN: " + r if r else "identical"))
print("captures in %s" % out)
sys.exit(1 if failed else 0)
