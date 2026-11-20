"""Exercise the release CLI with the real bundled PDFium (no display required)."""
import base64
import fcntl
import hashlib
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import termios
import zlib

binary = Path(sys.argv[1]).resolve()
samples = Path(__file__).resolve().parents[3] / "crates/term-app/samples"
fixtures = samples.parent / "tests/fixtures"


def show(*args, error=None, stdin=subprocess.DEVNULL):
    env = os.environ.copy()
    env.pop("TMUX", None)
    result = subprocess.run([binary, "show", *args], env=env, capture_output=True,
                            stdin=stdin, start_new_session=True, timeout=20)
    if error:
        assert result.returncode == 1 and not result.stdout and error in result.stderr, result
        return
    assert result.returncode == 0, result.stderr
    pages = []
    for line in result.stdout.splitlines():
        chunks = re.findall(rb"\x1b_G([^;]+);([^\x1b]+)\x1b\\", line)
        width, height = map(int, re.search(rb"s=(\d+),v=(\d+)", chunks[0][0]).groups())
        rgba = zlib.decompress(base64.b64decode(b"".join(data for _, data in chunks)))
        assert len(rgba) == width * height * 4
        pages.append((width, height, rgba))
    return pages


sample = samples / "sample.pdf"
pages = show(sample)
assert len(pages) == 3 and all(p[:2] == (640, 256) for p in pages)
assert show("--page", "2-3", sample) == pages[1:]
assert show("--page", "1", sample) == pages[:1]
assert show("--width", "10", "--page", "2", sample)[0][:2] == (80, 32)
assert pages[0][2].count(b"\xff\xff\xff\xff") > 40  # white text on a dark background
assert pages[1][2].count(bytes([13, 128, 122, 255])) > 100  # filled vector bars
for x, y, rgb in [(100, 64, (255, 0, 0)), (480, 64, (0, 255, 0)),
                  (100, 192, (0, 0, 255)), (480, 192, (255, 255, 255))]:
    pos = (y * 640 + x) * 4
    assert pages[2][2][pos:pos + 4] == bytes((*rgb, 255))  # embedded raster quadrants
show("--page", "2-4", sample, error=b"pages 1-3")
show(samples / "locked.pdf", error=b"password-protected PDF")


def check_tall(width, height, *options):
    master, slave = os.openpty()
    try:
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 158, width, height))
        bands = show(*options, fixtures / "tall.pdf", stdin=slave)
    finally:
        os.close(master)
        os.close(slave)
    cell_h = height // 40
    page_h = cell_h * 39 if options else width * 5
    page_w = round(page_h / 5)
    top = 0
    for w, h, rgba in bands:
        assert w == page_w and w * h <= 16 * 1024 * 1024
        assert top % cell_h == 0, "a band would leave a gap before the next one"
        # The left edge contains full-width TOP, MIDDLE and BOTTOM color bars.
        for y in range(h):
            pt = 2000 * (top + y + 0.5) / page_h
            if 1 < pt < 179 or 911 < pt < 1089 or 1821 < pt < 1999:
                rgb = (255, 0, 0) if pt < 180 else (0, 255, 0) if pt < 1090 else (0, 0, 255)
                assert rgba[4*w*y:4*w*y+4] == bytes((*rgb, 255)), (top, y, rgb)
        top += h
    assert top == page_h
    digest = hashlib.sha256()
    for _, _, rgba in bands:
        digest.update(rgba)
    return digest.digest()


def check_wide_tmux(expected):
    """Check every band reaches the outer PTY despite tmux redraws."""
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run([sys.executable, fixtures / "tmux_image_history.py", binary,
                        fixtures / "tall.pdf", tmp], check=True, stdout=subprocess.DEVNULL)
        live = (Path(tmp) / "live").read_bytes()
    digest = hashlib.sha256()
    encoded = bytearray()
    rows_left = 9480
    for header, chunk in re.findall(rb"\x1b_G([^;]+);([^\x1b]+)\x1b\\", live):
        if not encoded:
            width, height = map(int, re.search(rb"s=(\d+),v=(\d+)", header).groups())
            assert width == 1896
        encoded.extend(chunk)
        if b"m=0" in header:
            rgba = zlib.decompress(base64.b64decode(encoded))
            assert len(rgba) == width * height * 4
            rows = min(height, rows_left)
            digest.update(memoryview(rgba)[:rows * width * 4])
            rows_left -= rows
            encoded.clear()
    assert not encoded and not rows_left and digest.digest() == expected


check_tall(1264, 760)
wide = check_tall(1896, 1080)
if "--tmux" in sys.argv[2:]:
    check_wide_tmux(wide)
check_tall(1896, 1080, "--fit")
print("PDF content, dimensions, tall-page bands, page ranges and errors passed")
