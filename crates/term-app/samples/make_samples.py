#!/usr/bin/env python3
"""Write the raster-format and failure-case samples for `trident show`.

Create a 48x32 image split into red, green, blue and white quadrants in every
raster format, plus files for the failure cases. Requires Pillow.
"""

import io
import struct

from PIL import Image

W, H = 48, 32
QUADRANTS = [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255)]


def pattern(w=W, h=H, mode="RGB"):
    img = Image.new("RGB", (w, h))
    for y in range(h):
        for x in range(w):
            img.putpixel((x, y), QUADRANTS[(y >= h // 2) * 2 + (x >= w // 2)])
    return img.convert(mode)


img = pattern()
img.save("sample.png")
img.save("sample.jpg", quality=95)
img.save("sample.webp", quality=95)
img.save("sample.bmp")
img.save("sample.tiff")
img.save("sample.ppm")
img.save("sample.tga")
# Classic BMP-in-ICO; image also reads PNG-in-ICO, but only RGBA ones.
pattern(32, 32).convert("RGBA").save("sample.ico", sizes=[(32, 32)], bitmap_format="bmp")
# Two frames with different pixels, for playback and loop-count tests.
black = Image.new("RGB", (W, H))
img.save("sample.gif", save_all=True, append_images=[black], duration=100, loop=0)
img.save("animated.webp", save_all=True, append_images=[black], duration=[60, 90], loop=1, lossless=True)

# A blended lossless patch: unchanged red pixels are encoded as transparent,
# while the opaque replacement has nonzero channels near both ends of the range.
patch = Image.new("RGB", img.size, (1, 127, 255))
patch.paste(img.crop((2, 0, 8, 16)), (2, 0))
img.save("blended.webp", save_all=True, append_images=[patch], duration=[60, 90], loop=1, lossless=True)

# QOI by hand: Pillow only reads it. One QOI_OP_RGB per pixel is valid, if not compact.
qoi = bytearray(b"qoif" + struct.pack(">IIBB", W, H, 3, 0))
for r, g, b in zip(*[iter(img.tobytes())] * 3):
    qoi += bytes([0xFE, r, g, b])
qoi += bytes(7) + b"\x01"
open("sample.qoi", "wb").write(qoi)

# Stored sideways with EXIF orientation 6 (rotate 90 degrees clockwise to view).
side = Image.new("RGB", (16, 8), (0, 0, 255)); side.paste((255, 0, 0), (0, 0, 8, 8))
exif = Image.Exif(); exif[0x0112] = 6
side.save("rotated.jpg", quality=95, subsampling=0, exif=exif)

# Same file, misleading names: the type comes from the bytes.
open("png-named.jpg", "wb").write(open("sample.png", "rb").read())

png = open("sample.png", "rb").read()
open("corrupt.png", "wb").write(png[: len(png) // 2])
