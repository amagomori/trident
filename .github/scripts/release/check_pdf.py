"""Exercise the release CLI with the real bundled PDFium (no display required)."""
import base64
import os
from pathlib import Path
import re
import subprocess
import sys
import zlib

binary = Path(sys.argv[1]).resolve()
samples = Path(__file__).resolve().parents[3] / "crates/term-app/samples"


def show(*args, error=None):
    env = os.environ.copy()
    env.pop("TMUX", None)
    result = subprocess.run([binary, "show", *args], env=env, capture_output=True,
                            stdin=subprocess.DEVNULL, start_new_session=True, timeout=20)
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
print("PDF content, dimensions, page ranges and errors passed")
