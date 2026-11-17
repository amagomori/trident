#!/usr/bin/env python3
"""Exercise the installed fish/zsh query binding through a real 44-column PTY.

No display is needed. Example (test fish 3 and 4 separately):
  python3 .github/scripts/prompt_input_test.py --fish /usr/bin/fish --fish /opt/fish/bin/fish --zsh /usr/bin/zsh
"""
import argparse
import base64
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import tempfile
import termios
import time

ROOT = Path(__file__).resolve().parents[2]
QUERY = b"\x1b[1114109u"
READY = b"\x1b]777;trident;prompt-input;ready\x07"
REPLY = re.compile(rb"\x1b]777;trident;prompt-input;data;(\d+);([A-Za-z0-9+/=]*);(scalar|glyph)\x07")


def exercise(shell, kind):
    with tempfile.TemporaryDirectory(prefix="trident-prompt-input-") as directory:
        integration = ROOT / "shell-integration" / f"terminal.{kind}"
        if kind == "zsh":
            Path(directory, ".zshrc").write_text(
                'bindkey -e\nsetopt COMBINING_CHARS\nPS1=\"> \"\n'
                f'source "{integration}"\n'
                '_terminal_test_clear() { BUFFER=; CURSOR=0; }\n'
                'zle -N _terminal_test_clear\n'
                "bindkey $'\\e[1114108u' _terminal_test_clear\n"
            )
            args = [shell, "-d", "-i"]
        else:
            args = [shell, "-i", "-C",
                    'set -g fish_greeting; function fish_prompt; printf "> "; end; '
                    f'source "{integration}"; '
                    'function _terminal_test_clear; commandline --replace ""; end; '
                    r'bind \e\[1114108u _terminal_test_clear']
        pid, fd = pty.fork()
        if pid == 0:
            os.environ.update(TERM="xterm-256color", LANG="C.UTF-8", LC_ALL="C.UTF-8",
                              ZDOTDIR=directory, XDG_CONFIG_HOME=directory,
                              XDG_DATA_HOME=directory)
            os.execv(shell, args)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 20, 44, 0, 0))

        def until(predicate, timeout=5):
            output = b""
            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if not select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                chunk = os.read(fd, 65536)
                output += chunk
                # Answer the startup queries just as the terminal does. Match across reads.
                for query, response in [(b"\x1b[0c", b"\x1b[?62;22c"),
                                        (b"\x1b[6n", b"\x1b[1;3R")]:
                    if query in output:
                        os.write(fd, response)
                        output = output.replace(query, b"")
                if predicate(output):
                    return output
            raise AssertionError(f"{kind} timed out; terminal output={output!r}")

        try:
            until(lambda output: READY in output and b"\x1b]133;B\x07" in output)
            cases = ["", ": abc", ": 日本😀e\u0301    ", ": " + "a" * 39 + "日本😀bc", ": a\nb"]
            for text in cases:
                # Clear the entire editor (Ctrl-U is line-local in fish). Paste
                # keeps newlines literal, without executing a command.
                os.write(fd, b"\x1b[1114108u\x1b[200~" + text.encode() + b"\x1b[201~" + QUERY)
                match = REPLY.search(until(lambda output: REPLY.search(output) is not None))
                cursor = int(match[1])
                payload = base64.b64decode(match[2], validate=True).decode()
                assert payload == text + "\n", (kind, text, payload)
                assert cursor == len(text), (kind, text, cursor, len(text))
                assert match[3] == (b"scalar" if kind == "fish" else b"glyph"), match[3]
            print(f"PASS {kind} ({shell}): {len(cases)} exact buffer/cursor replies at 44 columns")
        finally:
            os.close(fd)
            os.kill(pid, signal.SIGKILL)
            os.waitpid(pid, 0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fish", action="append", default=[])
    parser.add_argument("--zsh", action="append", default=[])
    args = parser.parse_args()
    if not args.fish and not args.zsh:
        parser.error("pass at least one --fish or --zsh executable")
    for kind in ["fish", "zsh"]:
        for shell in getattr(args, kind):
            exercise(str(Path(shell).resolve()), kind)


if __name__ == "__main__":
    main()
