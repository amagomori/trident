"""Capture successive redraws from a real tmux client. The Rust test checks the pixels."""
import fcntl
import os
import pty
import select
import shlex
import struct
import subprocess
import sys
import termios
import time
from pathlib import Path

binary, png, directory = sys.argv[1:]
root = Path(directory)
tmux = ["tmux", "-L", f"trident-image-{os.getpid()}"]
os.environ.pop("TMUX", None)
os.environ.update(TERM="xterm-256color", LC_ALL="en_US.UTF-8", SHELL="/bin/sh")
args = [binary, "show", *(["--width", "158"] if "tall" in png else []), png]
script = (
    "printf 'BEFORE-IMAGE\\n'; while ! test -e start; do sleep .05; done; "
    + shlex.join(args)
    + "; echo $? > done; printf 'AFTER-IMAGE'; while ! test -e scroll; do sleep .05; done; "
    + "echo; i=1; while test $i -le 50; do echo FILL-$i; i=$((i+1)); done; "
    + "touch scrolled; exec sleep 120"
)
os.chdir(root)
pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(1, termios.TIOCSWINSZ, struct.pack("HHHH", 41, 158, 1264, 820))
    os.execvp("tmux", tmux + ["-f", "/dev/null", "new-session", "-s", "image", script])

data = bytearray()


def ctl(*args):
    return subprocess.check_output(tmux + list(args), text=True, timeout=5).strip()


def pump(timeout):
    end = time.monotonic() + timeout
    while (remaining := end - time.monotonic()) > 0 and select.select([fd], [], [], remaining)[0]:
        block = os.read(fd, 65536)
        if not block:
            raise EOFError("tmux client exited")
        data.extend(block)
        if b"\x1b[c" in block:
            os.write(fd, b"\x1b[?1;2c")


def until(path):
    end = time.monotonic() + 30
    while not Path(path).exists():
        if time.monotonic() >= end:
            raise TimeoutError(path)
        pump(.05)


def frame(name):
    pump(.35)
    hist, offset = ctl("display", "-p", "#{history_size}:#{scroll_position}").split(":")
    print(name, int(hist) - int(offset or 0), flush=True)
    Path(name).write_bytes(data)
    data.clear()


try:
    pump(.4)
    assert ctl("display", "-p", "#{pane_width}x#{pane_height}") == "158x40"
    # Reserve the last available IDs; wrapping to the start could corrupt an
    # earlier image still reachable in history.
    tiles = 2 if "tall" in Path(png).name else 1
    ctl("set", "-s", "@trident-image-sequence", str(240 * 255 - tiles))
    Path("start").touch()
    until("done")
    assert Path("done").read_text().strip() == "0"
    assert int(ctl("show", "-sv", "@trident-image-sequence")) == 240 * 255
    frame("live")
    Path("scroll").touch()
    until("scrolled")
    frame("out")
    ctl("copy-mode")
    for page in range(32):
        frame(f"copy{page}")
        if ctl("display", "-p", "#{scroll_position}") == ctl("display", "-p", "#{history_size}"):
            break
        ctl("send-keys", "-X", "page-up")
    else:
        raise AssertionError("did not reach the start of history")
    # The copy cursor can obscure the last cell of a full-width image with '$'.
    # Move it to the text sentinel without leaving this page; no pixel is exempted.
    ctl("send-keys", "-X", "history-top")
    ctl("send-keys", "-X", "start-of-line")
    frame("copy-top")
    ctl("send-keys", "-X", "cancel")
    frame("exit")
finally:
    subprocess.run(tmux + ["kill-server"], capture_output=True, timeout=5)
    os.close(fd)
    os.waitpid(pid, 0)
