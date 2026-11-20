"""Check negotiated and legacy keypad bytes on the default Xvfb US keymap.

Run: xvfb-run -a python3 .github/scripts/x11_kitty_keypad.py target/release/trident
Requires xdotool; no physical keypad or window manager is needed.
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tty


def capture(flags, path):
    """Negotiate in the app's PTY, then read through the F6 delimiter."""
    tty.setraw(0)
    os.write(1, f"\x1b=\x1b[>{flags}u\x1b[?u".encode())
    reply = bytearray()
    while not reply.endswith(b"u"):
        reply.extend(os.read(0, 64))
    assert reply == f"\x1b[?{flags}u".encode(), reply
    os.write(1, b"\x1b[?1004h\x1b]2;keypad-ready\x07")
    focus = bytearray()
    while not focus.endswith(b"\x1b[I"):
        focus.extend(os.read(0, 64))
    os.write(1, b"\x1b[?1004l\x1b]2;keypad-focused\x07")
    data = bytearray()
    while b"\x1b[17~" not in data:
        data.extend(os.read(0, 1024))
    path.write_bytes(data.split(b"\x1b[17~", 1)[0])


def check(binary, directory, flags, expected, keys):
    env = dict(os.environ, XDG_CONFIG_HOME=str(directory), XDG_STATE_HOME=str(directory))
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("WAYLAND_SOCKET", None)
    path = directory / f"raw-{flags}"
    app = subprocess.Popen([binary, "-e", sys.executable, str(Path(__file__).resolve()),
                            "--capture", str(flags), str(path)], env=env)
    try:
        search = ["xdotool", "search", "--sync", "--onlyvisible", "--all", "--pid", str(app.pid)]
        window = subprocess.check_output([*search, "--name", "^keypad-ready$"],
                                         timeout=15, text=True).splitlines()[-1]
        subprocess.run(["xdotool", "windowfocus", "--sync", window], check=True, timeout=10)
        subprocess.run([*search, "--name", "^keypad-focused$"], check=True,
                       stdout=subprocess.DEVNULL, timeout=10)
        subprocess.run(["xdotool", *keys, "key", "F6"], check=True, timeout=10)
        assert app.wait(timeout=10) == 0
        actual = path.read_bytes()
        assert actual == expected, f"flags={flags}: {actual!r} != {expected!r}"
    finally:
        if app.poll() is None:
            app.kill()
        app.wait()


if __name__ == "__main__":
    if sys.argv[1] == "--capture":
        capture(int(sys.argv[2]), Path(sys.argv[3]))
    else:
        binary = Path(sys.argv[1]).resolve()
        with tempfile.TemporaryDirectory() as tmp:
            keys = ["key", "Return", "KP_Enter", "1", "KP_Add", "KP_Home",
                    "keydown", "Control_L", "key", "KP_Enter", "keyup", "Control_L"]
            check(binary, Path(tmp), 0, b"\r\r1+\x1b[H\r", keys)
            check(binary, Path(tmp), 3, b"\r\x1b[57414u\x1b[57414;1:3u1+"
                  b"\x1b[57423u\x1b[57423;1:3u\x1b[57414;5u\x1b[57414;5:3u", keys)
            check(binary, Path(tmp), 10, b"\x1b[13u\x1b[13;1:3u\x1b[57414u\x1b[57414;1:3u"
                  b"\x1b[49u\x1b[49;1:3u\x1b[57413u\x1b[57413;1:3u"
                  b"\x1b[57423u\x1b[57423;1:3u\x1b[57442;5u\x1b[57414;5u\x1b[57414;5:3u"
                  b"\x1b[57442;1:3u", keys)
            # Xvfb keycode 87 is keypad End with Num Lock off, keypad 1 with it on.
            lock = b"\x1b[57360u\x1b[57360;1:3u"
            check(binary, Path(tmp), 26, b"\x1b[57424u\x1b[57424;1:3u" + lock
                  + b"\x1b[57400;;49u\x1b[57400;1:3u" + lock,
                  ["key", "87", "Num_Lock", "87", "Num_Lock"])
        print("PASS: legacy DECKPAM, disambiguation, report-all/events/text")
