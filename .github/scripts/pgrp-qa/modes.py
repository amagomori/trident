#!/usr/bin/env python3
"""KAN-95 functional check: shell-return marks still reset input modes a command left on.

When the shell takes the PTY foreground back, the app resets mouse reporting, focus events
and bracketed paste (`Terminal::reset_input_modes`, outside the alternate screen). PR #61
changes when that return is sampled, so this runs each scenario on main and head and checks,
from inside the app, which reports still arrive at the "prompt":

  run.sh (the app's shell, bash `set -m`): fixture.py SCENARIO (own job) -> prints a prompt
  and stays foreground 1 s (like waiting at a prompt) -> probe.py (raw tty, logs input).
  The driver then injects focus out/in, a click and a paste of a marker into the window.

Scenarios and what must hold on both builds (`report` = measured, not pass/fail):
  control     probe enables every mode itself: all reports arrive (proves injection works)
  idle        modes on, exit 0.3 s later            -> no mouse/focus/paste reports
  kill        modes on, SIGKILL 0.3 s later          -> none
  flood-kill  modes on, 20 MiB, SIGKILL right after  -> none (return seen while output queued)
  flood-exit  modes on, 20 MiB, exit right after     -> none
  prompt-2004 20 MiB, exit; prompt enables ?2004h    -> bracketed paste kept, no mouse/focus
  flood-tail  20 MiB, then modes, exit at once       -> report (PR notes: may keep modes)

Linux: X11 (xdotool, xclip; xmessage as the focus-away window). macOS: CoreGraphics events
through ctypes and osascript; the runner needs Accessibility permission.

  python3 .github/scripts/pgrp-qa/modes.py --main ../t-main/target/release/trident \
      --head ../t-head/target/release/trident --runs 5 --out modes
"""
import argparse, ctypes, ctypes.util, json, os, platform, random, shutil, signal, subprocess
import sys, tempfile, time
from pathlib import Path

MAC = platform.system() == "Darwin"
MODES = "\033[?1000h\033[?1002h\033[?1006h\033[?1004h\033[?2004h"
MARK = "K95PASTE"
SCENARIOS = {  # name: (expected leaks, prompt modes, pass/fail?)
    "control": ({"mouse", "focus", "paste"}, "", True),
    "idle": (set(), "", True),
    "kill": (set(), "", True),
    "flood-kill": (set(), "", True),
    "flood-exit": (set(), "", True),
    "prompt-2004": ({"paste"}, "\033[?2004h", True),
    "flood-tail": (set(), "", False),
}
RUN_SH = r"""#!/bin/bash
set -m
cd "$1"
exec 9<>fifo
python3 fixture.py "$2"
printf '%s$ ' "$3"
read -t 1 -u 9 _
python3 probe.py "$2"
while :; do read -t 60 -u 9 _; done
"""
FIXTURE = r"""import os, signal, sys, time
s = sys.argv[1]
out = os.fdopen(1, "wb", 0)
modes = b"MODES"
flood = b"".join(b"%07d flood line for the KAN-95 shell-return check ..........\n" % i
                 for i in range(4096))
def burst():
    for _ in range(20 * 1024 * 1024 // len(flood)):
        out.write(flood)
if s in ("idle", "kill"):
    out.write(modes); time.sleep(0.3)
elif s in ("flood-kill", "flood-exit"):
    out.write(modes); burst()
elif s in ("prompt-2004",):
    burst()
elif s == "flood-tail":
    burst(); out.write(modes)
if s.endswith("kill"):
    os.kill(os.getpid(), signal.SIGKILL)
""".replace("MODES", MODES.replace("\033", "\\x1b"))
PROBE = r"""import os, select, sys, termios, time, tty
fd = os.open("/dev/tty", os.O_RDWR)
old = termios.tcgetattr(fd)
tty.setraw(fd)
if sys.argv[1] == "control":
    os.write(fd, b"MODES")
os.write(fd, b"\r\nprobe ready\r\n")
open("ready", "w").close()
got = b""
end = time.monotonic() + 8
while not os.path.exists("stop") and time.monotonic() < end:
    if select.select([fd], [], [], 0.05)[0]:
        got += os.read(fd, 4096)
termios.tcsetattr(fd, termios.TCSADRAIN, old)
open("input.bin", "wb").write(got)
os.write(fd, b"probe got %r\r\n" % got[:200])
open("done", "w").close()
""".replace("MODES", MODES.replace("\033", "\\x1b"))


def wait_for(path, timeout, proc):
    end = time.monotonic() + timeout
    while not path.exists():
        if proc.poll() is not None:
            raise RuntimeError(f"app exited ({proc.returncode}) before {path.name}")
        if time.monotonic() > end:
            raise TimeoutError(path.name)
        time.sleep(0.01)


class X11:
    def __init__(self, env):
        self.env = env
        self.away = subprocess.Popen(["xmessage", "-geometry", "+0+0", "focus away"], env=env)
        time.sleep(0.5)

    def x(self, *a, check=True):
        return subprocess.run(["xdotool", *a], env=self.env, capture_output=True, text=True, check=check).stdout

    def focus(self, win):
        if subprocess.run(["xdotool", "windowactivate", "--sync", win], env=self.env,
                          capture_output=True, timeout=5).returncode:
            self.x("windowfocus", "--sync", win)

    def window(self, pid=None, cls=None):
        end = time.monotonic() + 10
        while time.monotonic() < end:
            how = ["--pid", str(pid)] if pid else ["--class", cls]
            wins = self.x("search", "--onlyvisible", *how, check=False).split()
            if wins:
                return wins[-1]
            time.sleep(0.1)
        raise TimeoutError("app window")

    def inject(self, pid):
        win = self.window(pid)
        away = self.window(cls="xmessage")
        self.focus(win); time.sleep(0.3)
        self.focus(away); time.sleep(0.3)
        self.focus(win); time.sleep(0.3)
        self.x("mousemove", "--window", win, "120", "120"); time.sleep(0.1)
        self.x("click", "1"); time.sleep(0.3)
        subprocess.run(["xclip", "-selection", "clipboard"], input=MARK.encode(), env=self.env, check=True)
        self.x("key", "--window", win, "--clearmodifiers", "ctrl+shift+v")
        time.sleep(0.5)

    def close(self):
        self.away.terminate()


class Mac:
    def __init__(self, env):
        self.cg = ctypes.CDLL(ctypes.util.find_library("ApplicationServices"))
        self.cg.CGEventCreateMouseEvent.restype = ctypes.c_void_p
        self.cg.CGEventCreateMouseEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint32, CGPoint, ctypes.c_uint32]
        self.cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
        self.cg.CGEventCreateKeyboardEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool]
        self.cg.CGEventSetFlags.argtypes = [ctypes.c_void_p, ctypes.c_uint64]
        self.cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        self.cf = ctypes.CDLL(ctypes.util.find_library("CoreFoundation"))
        self.cf.CFRelease.argtypes = [ctypes.c_void_p]

    def post(self, ev):
        self.cg.CGEventPost(0, ev)
        self.cf.CFRelease(ev)

    def osa(self, script):
        return subprocess.run(["osascript", "-e", script], capture_output=True, text=True, check=True).stdout

    def inject(self, pid):
        front = f'tell application "System Events" to set frontmost of (first process whose unix id is {pid}) to true'
        bounds = self.osa(f'tell application "System Events" to tell (first process whose unix id is {pid}) '
                          'to get {position, size} of window 1').replace(" ", "").split(",")
        x, y, w, h = map(int, bounds)
        self.osa(front); time.sleep(1.5)
        self.osa('tell application "Finder" to activate'); time.sleep(0.8)
        self.osa(front); time.sleep(1.5)
        p = CGPoint(x + w / 2, y + h / 2)
        self.post(self.cg.CGEventCreateMouseEvent(None, 1, p, 0)); time.sleep(0.05)  # left down
        self.post(self.cg.CGEventCreateMouseEvent(None, 2, p, 0)); time.sleep(0.3)   # left up
        subprocess.run(["pbcopy"], input=MARK.encode(), check=True)
        for down in (True, False):  # Cmd+V, kVK_ANSI_V = 9
            ev = self.cg.CGEventCreateKeyboardEvent(None, 9, down)
            self.cg.CGEventSetFlags(ev, 1 << 20)
            self.post(ev); time.sleep(0.05)
        time.sleep(0.5)

    def close(self):
        pass


class CGPoint(ctypes.Structure):
    _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]


def classify(got):
    leaks = set()
    if b"\x1b[<" in got or b"\x1b[M" in got:
        leaks.add("mouse")
    if b"\x1b[I" in got or b"\x1b[O" in got:
        leaks.add("focus")
    if b"\x1b[200~" in got:
        leaks.add("paste")
    return leaks, MARK.encode() in got


def one_run(binary, scenario, env, injector, log):
    work = Path(tempfile.mkdtemp(prefix="k95modes-"))
    os.mkfifo(work / "fifo")
    (work / "run.sh").write_text(RUN_SH)
    (work / "run.sh").chmod(0o755)
    (work / "fixture.py").write_text(FIXTURE)
    (work / "probe.py").write_text(PROBE)
    proc = subprocess.Popen([str(binary), "-e", str(work / "run.sh"), str(work), scenario, SCENARIOS[scenario][1] + scenario],
                            env=env, stdout=log, stderr=log)
    try:
        wait_for(work / "ready", 120, proc)
        time.sleep(0.3)
        injector.inject(proc.pid)
        (work / "stop").touch()
        wait_for(work / "done", 10, proc)
        return classify((work / "input.bin").read_bytes())
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill(); proc.wait()
        shutil.rmtree(work, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--main", type=Path, required=True)
    ap.add_argument("--head", type=Path, required=True)
    ap.add_argument("--runs", type=int, default=5, help="runs per scenario per build")
    ap.add_argument("--scenarios", default=",".join(SCENARIOS))
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    cfg = Path(tempfile.mkdtemp(prefix="k95cfg-"))
    env = dict(os.environ, XDG_CONFIG_HOME=str(cfg), BASH_SILENCE_DEPRECATION_WARNING="1")
    injector = Mac(env) if MAC else X11(env)
    builds = {"main": args.main.resolve(), "head": args.head.resolve()}
    jobs = [(s, b) for s in args.scenarios.split(",") for b in builds for _ in range(args.runs)]
    random.shuffle(jobs)
    rows, log = [], open(args.out / "app.log", "w")
    try:
        for scenario, label in jobs:
            leaks, pasted = one_run(builds[label], scenario, env, injector, log)
            rows.append({"scenario": scenario, "build": label, "leaks": sorted(leaks), "pasted": pasted})
            print(f"{scenario:12s} {label:4s} reports={','.join(sorted(leaks)) or '-':17s} paste_seen={pasted}", flush=True)
    finally:
        log.close()
        injector.close()
    (args.out / "rows.json").write_text(json.dumps(rows, indent=1))
    failed = False
    print("\nscenario     build  as-expected/runs  (expected reports)")
    for scenario in args.scenarios.split(","):
        want, _, gated = SCENARIOS[scenario]
        for label in builds:
            rs = [r for r in rows if r["scenario"] == scenario and r["build"] == label]
            ok = sum(set(r["leaks"]) == want and r["pasted"] for r in rs)
            unverified = sum(not r["pasted"] for r in rs)
            bad = gated and ok < len(rs)
            failed |= bad
            note = "FAIL" if bad else ("report" if not gated else "ok")
            print(f"{scenario:12s} {label:5s}  {ok}/{len(rs)}  ({','.join(sorted(want)) or 'none'})  {note}"
                  + (f"  [{unverified} runs: paste never arrived, injection unverified]" if unverified else ""))
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
