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
    (tmp / "trident/config.toml").write_text('shell = "/bin/sh"\n')
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
        xdo("type", "--clearmodifiers", command)
        xdo("key", "Return")

    try:
        for role in ("original", "middle", "front"):
            if known:
                xdo("key", "--clearmodifiers", "ctrl+shift+n")
            window = wait_for("a new window in the same process", next_window)
            focus(window)
            if role == "original":
                original = window
            else:
                enter(window, f'role={role}; touch "$TEST_DIR/{role}-ready"')
                wait_for(f"{role} shell", (tmp / f"{role}-ready").is_file)
                if role == "middle":
                    middle = window
            known.add(window)

        shell = int((tmp / "pid").read_text())
        (tmp / "go").touch()
        wait_for("the background shell to exit", lambda: not Path(f"/proc/{shell}").exists())
        wait_for("the original native window to be destroyed", lambda: subprocess.run(
            ["xwininfo", "-id", original], capture_output=True).returncode != 0)
        # Do not refocus the front window: its existing input route must still work.
        enter(window, 'echo "$role" > "$TEST_DIR/front"')
        wait_for("front shell input", (tmp / "front").is_file)
        assert (tmp / "front").read_text().strip() == "front"
        enter(window, "exit")
        focus(middle)
        enter(middle, 'echo "$role" > "$TEST_DIR/middle"; exit')
        wait_for("middle shell input", (tmp / "middle").is_file)
        assert (tmp / "middle").read_text().strip() == "middle"
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
