"""Check background shell teardown on an X11 desktop with xdotool and xwininfo.

Usage: python3 x11_background_exit.py <trident-binary>
"""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def wait_for(description, probe):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"timed out waiting for {description}")


def xdo(*args):
    return subprocess.check_output(["xdotool", *args], text=True, timeout=10).strip()


def exercise(binary, tmp):
    (tmp / "trident").mkdir()
    (tmp / "trident/config.toml").write_text(
        """shell = ["/bin/sh", "-c", 'echo $$ >> "$TEST_DIR/started"; exec /bin/sh -i']\n"""
    )
    env = dict(os.environ, TEST_DIR=str(tmp),
               XDG_CONFIG_HOME=str(tmp), XDG_STATE_HOME=str(tmp))
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("WAYLAND_SOCKET", None)
    # The original window (the clipboard's initial window) exits only when released.
    command = 'echo $$ > "$TEST_DIR/pid"; while [ ! -f "$TEST_DIR/go" ]; do sleep .05; done'
    app = subprocess.Popen([binary, "-e", "/bin/sh", "-c", command], env=env)
    known = set()

    def next_window():
        result = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(app.pid)],
                                text=True, capture_output=True, check=False)
        return next(iter(set(result.stdout.split()) - known), None)

    def focus(window):
        xdo("windowactivate", "--sync", window)
        wait_for(f"focus on {window}", lambda: xdo("getactivewindow") == window)

    def enter(window, command):
        assert xdo("getactivewindow") == window
        # Focus can move before the new-window shortcut's final key is released.
        xdo("key", "--clearmodifiers", "ctrl+u")
        xdo("type", "--clearmodifiers", command)
        xdo("key", "Return")

    def read_pids(name, count):
        path = tmp / name

        def probe():
            values = path.read_text().split() if path.exists() else []
            return values if len(values) == count and all(v.isdecimal() for v in values) else None

        return wait_for(f"{count} shell PID(s) in {name}", probe)

    def destroyed(window):
        wait_for(f"native window {window} to be destroyed", lambda: subprocess.run(
            ["xwininfo", "-id", window], capture_output=True).returncode != 0)

    try:
        for role in ("original", "middle", "front"):
            if known:
                xdo("key", "--clearmodifiers", "ctrl+shift+n")
            window = wait_for("a new window in the same process", next_window)
            focus(window)
            if role == "original":
                original = window
            elif role == "middle":
                middle = window
                read_pids("started", 1)
            known.add(window)

        middle_pid, front_pid = read_pids("started", 2)
        assert middle_pid != front_pid
        shell = int((tmp / "pid").read_text())
        (tmp / "go").touch()
        wait_for("the background shell to exit", lambda: not Path(f"/proc/{shell}").exists())
        destroyed(original)
        # Do not refocus the front window: its existing input route must still work.
        enter(window, 'echo $$ > "$TEST_DIR/front"')
        assert read_pids("front", 1) == [front_pid]
        enter(window, "exit")
        destroyed(window)
        focus(middle)
        enter(middle, 'echo $$ > "$TEST_DIR/middle"; exit')
        assert read_pids("middle", 1) == [middle_pid]
        assert app.wait(timeout=10) == 0
    finally:
        (tmp / "go").touch()
        if app.poll() is None:
            app.kill()
        app.wait()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory() as directory:
        exercise(sys.argv[1], Path(directory))
    print("PASS: background window destroyed, both surviving shells received input, last exit quit")
