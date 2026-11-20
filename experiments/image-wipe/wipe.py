"""Compare two local PNGs in a Trident tab; nothing is written to disk."""

import argparse
import base64
import curses
import fcntl
import io
import secrets
import struct
import sys
import termios
from pathlib import Path

from PIL import Image, ImageDraw


def load(path):
    if not path.is_file():
        raise ValueError("expected a local PNG file")
    with Image.open(path, formats=["PNG"]) as src:
        if src.width * src.height > 16_777_216:
            raise ValueError("input exceeds the experiment's 16-megapixel limit")
        return src.convert("RGBA")


def fit(pair, bounds):
    before, after = pair
    if before.size != after.size:
        raise ValueError("use screenshots with identical pixel dimensions")
    # Resize the whole screenshots, never the two visible slices.
    scale = min(1, bounds[0] / before.width, bounds[1] / before.height)
    if scale == 1:
        return pair
    size = (max(1, round(before.width * scale)), max(1, round(before.height * scale)))
    return tuple(image.resize(size, Image.Resampling.LANCZOS) for image in pair)


def frame(before, after, cut):
    result = after.copy()
    if cut:
        result.paste(before.crop((0, 0, cut, before.height)), (0, 0))
    if 0 < cut < before.width:
        ImageDraw.Draw(result).line((cut, 0, cut, before.height - 1), fill="magenta")
    return result


def display(image, image_id):
    png = io.BytesIO()
    image.save(png, "PNG", compress_level=1)
    data = base64.b64encode(png.getvalue())
    out = sys.stdout.buffer
    out.write(b"\x1b[4;1H")
    for pos in range(0, len(data), 4096):
        # Retransmitting our ID replaces only the previous frame.
        control = f"a=T,f=100,C=1,i={image_id}," if pos == 0 else ""
        more = int(pos + 4096 < len(data))
        out.write(f"\x1b_G{control}q=2,m={more};".encode() +
                  data[pos:pos + 4096] + b"\x1b\\")
    out.flush()


def view(screen, originals):
    curses.curs_set(0)
    image_id = 1 + secrets.randbelow(2**32 - 1)
    percent = 50
    size = None
    try:
        while True:
            rows, cols, px, py = struct.unpack(
                "HHHH", fcntl.ioctl(1, termios.TIOCGWINSZ, bytes(8)))
            if rows < 4 or cols < 2:
                raise ValueError("enlarge the terminal and try again")
            bounds = (px or cols * 8, (py or rows * 16) * (rows - 3) // rows)
            if bounds != size:
                before, after = fit(originals, bounds)
                size = bounds
            screen.erase()
            screen.addnstr(0, 0, f"BEFORE  |  AFTER     {percent}% before", cols - 1,
                           curses.A_BOLD)
            screen.addnstr(1, 0, "Left/Right: move   0/1: after/before   q: quit",
                           cols - 1)
            screen.refresh()
            display(frame(before, after, round(before.width * percent / 100)), image_id)
            while True:
                key = screen.getch()
                if key in (ord("q"), 27):
                    return
                if key == curses.KEY_RESIZE:
                    break
                next_percent = {curses.KEY_LEFT: max(0, percent - 5),
                                curses.KEY_RIGHT: min(100, percent + 5),
                                ord("0"): 0, ord("1"): 100}.get(key, percent)
                if next_percent != percent:
                    percent = next_percent
                    break
    finally:
        sys.stdout.buffer.write(f"\x1b_Ga=d,d=I,i={image_id},q=2;\x1b\\".encode())
        sys.stdout.buffer.flush()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    args = parser.parse_args()
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        parser.error("run inside an interactive Trident tab")
    try:
        pair = (load(args.before), load(args.after))
        curses.wrapper(view, pair)
    except KeyboardInterrupt:
        pass
    except (OSError, ValueError, Image.DecompressionBombError, curses.error) as exc:
        parser.exit(1, f"image wipe: {ascii(str(exc))}\n")


if __name__ == "__main__":
    main()
