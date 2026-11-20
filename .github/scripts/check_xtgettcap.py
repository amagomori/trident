"""Compare live PTY replies with ncurses reading the compiled, shipped profile."""

import curses
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import traceback
import tty

ST = b"\x1b\\"
STATUS = b"\x1b[0n"
NAMES = ["Co", "Tc", "Smulx", "setrgbf", "setrgbb", "Sync"]


def probe(directory):
    assert os.isatty(0) and os.isatty(1)
    tty.setraw(0)
    os.environ["TERMINFO"] = str(directory)
    curses.setupterm("xterm-trident", 1)

    values = {"Co": str(curses.tigetnum("colors")).encode(), "Tc": None}
    assert curses.tigetflag("Tc") == 1 and values["Co"] == b"256"
    for name in NAMES[2:]:
        values[name] = curses.tigetstr(name)
        assert values[name] is not None, name
    assert curses.tigetstr("Ms") is None

    failures = []
    for names in [[n] for n in NAMES] + [NAMES, ["Missing"], ["Tc", "Ms", "Co", "Tc"]]:
        keys = [n.encode().hex().encode() for n in names]
        request = b"\x1bP+q" + b";".join(keys) + ST + b"\x1b[5n"
        for i in range(0, len(request), 3):
            os.write(1, request[i:i + 3])
        actual = bytearray()
        while not actual.endswith(STATUS):
            chunk = os.read(0, 4096)
            assert chunk, "PTY closed before the trailing status report"
            actual.extend(chunk)
        expected = bytearray()
        for name, key in zip(names, keys):
            expected.extend((b"\x1bP1+r" if name in values else b"\x1bP0+r") + key.upper())
            if values.get(name) is not None:
                expected.extend(b"=" + values[name].hex().upper().encode())
            expected.extend(ST)
        if actual != expected + STATUS:
            failures.append(f"{names}: got {actual!r}, expected {expected + STATUS!r}")
    assert not failures, "\n".join(failures)


if __name__ == "__main__":
    if sys.argv[1] == "--child":
        directory = Path(sys.argv[2])
        try:
            probe(directory)
            result = "PASS"
        except Exception:
            result = traceback.format_exc()
        (directory / "result").write_text(result)
    else:
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(__file__).resolve().parents[2] / "crates/term-app/terminfo/xterm-trident.terminfo"
            subprocess.run(["/usr/bin/tic", "-x", "-o", tmp, source], check=True)
            subprocess.run([sys.argv[1], "-e", sys.executable, Path(__file__).resolve(),
                            "--child", tmp], check=True, timeout=45)
            result = (Path(tmp) / "result").read_text()
            if result != "PASS":
                sys.exit(result)
            print("PASS: all six capabilities, grouped replies, and negative controls")
