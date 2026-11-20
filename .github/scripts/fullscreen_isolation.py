#!/usr/bin/env python3
"""Check that changing native fullscreen Spaces preserves the other group's frame and PTY.

Run on a macOS desktop with Accessibility permission and English Window menu items:
    python3 .github/scripts/fullscreen_isolation.py target/release/trident
Pass --new-window to create the second group with Cmd+N instead of detaching a tab.
Both groups must be visible to Accessibility under the original Trident PID.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


def monitor_pty(directory):
    """Identify this tab by its shell PID and log every observed terminal size change."""
    import signal

    name = f"geometry-{os.getpid()}"
    print(f"\033]2;{name}\007", end="", flush=True)
    with (directory / name).open("w", buffering=1) as log:
        def record(*_):
            cols, rows = os.get_terminal_size(0)
            print(cols, rows, file=log)

        signal.signal(signal.SIGWINCH, record)
        record()
        while True:
            signal.pause()


def until(read, accept, message):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            value = read()
        except subprocess.CalledProcessError as exc:
            # AX windows can disappear briefly when AppKit changes Spaces.
            value = exc.stderr or str(exc)
        else:
            if accept(value):
                return value
        time.sleep(0.1)
    raise AssertionError(f"{message}: last saw {value!r}")


def exercise(process, directory, new_window):
    def ax(command):
        script = (f'tell application "System Events" to tell (first application process '
                  f'whose unix id is {process.pid})\n{command}\nend tell')
        return subprocess.check_output(["osascript", "-e", script], text=True, timeout=8,
                                       stderr=subprocess.PIPE).strip()

    def menu(item):
        ax(f'click menu item "{item}" of menu 1 of menu bar item "Window" of menu bar 1')

    def menu_enabled(item):
        return ax(f'get enabled of menu item "{item}" of menu 1 of menu bar item '
                  '"Window" of menu bar 1') == "true"

    def names():
        return ax("get name of every window").split(", ")

    def grid(name):
        return (directory / name).read_text().splitlines()[-1]

    def frame(name):
        return ax(f'get {{position, size}} of window "{name}"')

    def new_pane(key="t"):
        before = set(directory.glob("geometry-*"))
        ax(f'keystroke "{key}" using command down')
        new = until(lambda: set(directory.glob("geometry-*")) - before, bool, "new PTY")
        name = new.pop().name
        until(names, lambda n: n[0] == name, "new tab did not become key")
        return name

    def place(name, x, y, width, height):
        menu(name)
        until(names, lambda n: n[0] == name, "group did not become key")
        ax(f'set size of window "{name}" to {{{width}, {height}}}\n'
           f'set position of window "{name}" to {{{x}, {y}}}')
        until(lambda: frame(name), lambda f: f == f"{x}, {y}, {width}, {height}",
              "test requires room for two distinct frames")

    def fullscreen(name):
        return ax(f'get value of attribute "AXFullScreen" of window "{name}"') == "true"

    def cycle(active, inactive, group):
        before = frame(inactive)
        logs = {name: (grid(name), (directory / name).stat().st_size) for name in group}
        print(f"Active {active}, inactive {inactive}: {before}, {grid(inactive)} cells", flush=True)
        menu("Enter Full Screen")
        until(lambda: fullscreen(active), bool, "did not enter fullscreen")
        until(lambda: menu_enabled("Exit Full Screen"), bool, "fullscreen transition not ready")
        created = new_pane()
        until(lambda: fullscreen(created), bool, "new tab did not join fullscreen")
        menu("Exit Full Screen")
        until(names, lambda n: set(n) == {created, inactive}, "expected two normal groups")
        until(lambda: fullscreen(created), lambda f: not f, "did not exit fullscreen")
        until(lambda: menu_enabled("Enter Full Screen"), bool, "normal window not ready")
        after = frame(inactive)
        errors = [f"{inactive}: frame {before} -> {after}"] if before != after else []
        for name, (cells, offset) in logs.items():
            with (directory / name).open() as out:
                out.seek(offset)
                changes = out.read().splitlines()
            if any(size != cells for size in changes):
                errors.append(f"{name}: grid {cells} -> {changes}")
        return created, errors

    a = until(lambda: list(directory.glob("geometry-*")), bool, "first PTY")[0].name
    until(names, lambda n: n == [a], "expected one normal group")
    ax("set frontmost to true")
    if new_window:
        b = new_pane("n")
    else:
        b = new_pane()
        menu("Move Tab to New Window")
    until(names, lambda n: set(n) == {a, b}, "expected two windows in the original process")
    place(b, 480, 260, 600, 400)
    place(a, 80, 100, 812, 554)
    until(lambda: grid(a) != grid(b), bool, "precondition: distinct PTY sizes")
    new_a, errors = cycle(a, b, [b])
    place(new_a, 80, 100, 812, 590)
    place(b, 480, 260, 600, 400)
    until(lambda: grid(new_a) != grid(b), bool, "reverse precondition: distinct PTY sizes")
    new_b, reverse_errors = cycle(b, new_a, [a, new_a])
    errors.extend(reverse_errors)
    if errors:
        raise AssertionError("\n".join(errors))
    if new_window:
        menu(new_b)
        before_zoom = {name: grid(name) for name in (a, new_a, new_b)}
        ax('keystroke "=" using command down')
        until(lambda: grid(new_b), lambda size: size != before_zoom[new_b], "zoom had no effect")
        ax('keystroke "w" using {command down, shift down}')
        until(names, lambda n: n == [new_a], "Close Window did not leave the other group intact")
        if any(grid(name) != before_zoom[name] for name in (a, new_a)):
            raise AssertionError("zoom resized a PTY in the other group")
        ax('keystroke "w" using command down')
        until(names, lambda n: n == [a], "surviving tab did not become key")
        ax('keystroke "w" using command down')
        if process.wait(timeout=10) != 0:
            raise AssertionError("closing the final window did not exit cleanly")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--new-window", action="store_true")
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("requires a native macOS desktop")
    with tempfile.TemporaryDirectory() as temp:
        directory = Path(temp)
        config = directory / "config.toml"
        shell = [sys.executable, str(Path(__file__).resolve()), "--monitor", temp]
        config.write_text(f"shell = {json.dumps(shell)}\nremember_window_size = false\n")
        with subprocess.Popen([args.binary.resolve(), "--config", config],
                              env={**os.environ, "HOME": temp, "XDG_CONFIG_HOME": temp}) as process:
            try:
                exercise(process, directory, args.new_window)
            finally:
                process.terminate()
        print("PASS: both inactive groups retained their frame and grid")


if __name__ == "__main__":
    if sys.argv[1:2] == ["--monitor"]:
        monitor_pty(Path(sys.argv[2]))
    else:
        main()
