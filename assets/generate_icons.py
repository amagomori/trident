#!/usr/bin/env python3
"""Builds assets/icons/ from the 1024x1024 full-bleed sources in assets/.

For each icon it writes:
  <name>.icns           macOS app icon: the body scaled to 824/1024 on a transparent
                        canvas (Apple's icon grid) at 16-1024 px, 1x and 2x.
  <name>-menubar.png    18x18 menu bar image (1x), body at 16 px with 1 px margin.
  <name>-menubar@2x.png 36x36 menu bar image (2x).
and, for the default icon only, the Linux hicolor set:
  tide-<size>.png       the body scaled to 824/1024 on a transparent <size> canvas.

Needs Pillow. Run from anywhere: python3 assets/generate_icons.py
"""
import io
import struct
from pathlib import Path

from PIL import Image

ASSETS = Path(__file__).resolve().parent
OUT = ASSETS / "icons"
NAMES = ["tide", "lagoon", "dusk", "reef", "kelp", "midnight", "golden", "sunset"]

# ICNS chunk type -> pixel size, as iconutil writes them.
ICNS_TYPES = [
    (b"icp4", 16),
    (b"ic11", 32),
    (b"icp5", 32),
    (b"ic12", 64),
    (b"ic07", 128),
    (b"ic13", 256),
    (b"ic08", 256),
    (b"ic14", 512),
    (b"ic09", 512),
    (b"ic10", 1024),
]
GRID = 1024
BODY = 824
MENU_POINTS = 18
MENU_BODY_POINTS = 16
LINUX_ICON = "tide"
LINUX_SIZES = [16, 24, 32, 48, 64, 128, 256, 512]


def png(image: Image.Image) -> bytes:
    buf = io.BytesIO()
    image.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def fit(source: Image.Image, canvas: int, body: int) -> Image.Image:
    out = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
    scaled = source.resize((body, body), Image.Resampling.LANCZOS)
    offset = (canvas - body) // 2
    out.alpha_composite(scaled, (offset, offset))
    return out


def icns(source: Image.Image) -> bytes:
    dock = fit(source, GRID, BODY)
    by_size = {}
    for _, size in ICNS_TYPES:
        if size not in by_size:
            by_size[size] = png(dock.resize((size, size), Image.Resampling.LANCZOS))
    chunks = b"".join(
        kind + struct.pack(">I", 8 + len(by_size[size])) + by_size[size]
        for kind, size in ICNS_TYPES
    )
    return b"icns" + struct.pack(">I", 8 + len(chunks)) + chunks


def main() -> None:
    OUT.mkdir(exist_ok=True)
    for name in NAMES:
        source = Image.open(ASSETS / f"{name}.png").convert("RGBA")
        (OUT / f"{name}.icns").write_bytes(icns(source))
        for scale, suffix in ((1, ""), (2, "@2x")):
            menu = fit(source, MENU_POINTS * scale, MENU_BODY_POINTS * scale)
            (OUT / f"{name}-menubar{suffix}.png").write_bytes(png(menu))
        if name == LINUX_ICON:
            for size in LINUX_SIZES:
                image = fit(source, size, round(size * BODY / GRID))
                (OUT / f"{name}-{size}.png").write_bytes(png(image))


if __name__ == "__main__":
    main()
