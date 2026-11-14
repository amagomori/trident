#!/usr/bin/env python3
"""KAN-95 functional check: long-command notifications and title updates are not delayed.

PR #61 leaves the long-command watcher (command_notify.rs, 200 ms poll) and title handling
alone, but both run beside the PTY reader whose sampling changed, so this measures them on
main and head, shuffled, from the moment the command acts to the moment the desktop sees it:

  title-idle    5 OSC 2 titles 300 ms apart at an idle prompt -> window title changes
  title-flood   OSC 2 title every 4 MiB inside 40 MiB of output -> window title changes
  notify-quiet  `sleep 2`, window unfocused, threshold 1 s      -> notification request
  notify-flood  2 s of continuous output, then exit, unfocused  -> notification request

Window title: Linux `xprop -spy`; macOS a JXA loop on System Events (needs Accessibility).
Notification: Linux the D-Bus Notify call (`dbus-monitor`, needs the desktop session bus and
a notification server, e.g. Plasma); macOS the app's `osascript display notification` child,
polled every 20 ms (unbundled binary). Times are wall clock on the same machine.

Fails (exit 1) when, for any scenario, an expected event never arrives on head, or head's
median latency exceeds main's by more than max(20 ms, 10%).

  python3 .github/scripts/pgrp-qa/timing.py --main ../t-main/target/release/terminal \
      --head ../t-head/target/release/terminal --runs 6 --out timing
"""
import argparse, json, os, platform, random, re, shutil, signal, statistics, subprocess
import sys, tempfile, threading, time
from pathlib import Path

MAC = platform.system() == "Darwin"
SCENARIOS = ["title-idle", "title-flood", "notify-quiet", "notify-flood"]
RUN_SH = r"""#!/bin/bash
set -m
cd "$1"
exec 9<>fifo
while [ ! -e go ]; do read -t 0.05 -u 9 _; done
python3 fixture.py "$2"
while :; do read -t 60 -u 9 _; done
"""
FIXTURE = r"""import json, os, sys, time
s = sys.argv[1]
out = os.fdopen(1, "wb", 0)
line = b"%07d KAN-95 timing flood line .................................................\n"
chunk = b"".join(line % i for i in range(4096))
events = []
def title(n):
    events.append(time.time())
    out.write(b"\x1b]2;K95-%d\x07" % n)
if s == "title-idle":
    time.sleep(1)
    for n in range(5):
        title(n); time.sleep(0.3)
elif s == "title-flood":
    for n in range(10):
        for _ in range(4 * 1024 * 1024 // len(chunk)):
            out.write(chunk)
        title(n)
elif s == "notify-quiet":
    time.sleep(2)
elif s == "notify-flood":
    end = time.monotonic() + 2
    while time.monotonic() < end:
        out.write(chunk)
if s.startswith("notify"):
    events.append(time.time())
json.dump(events, open("events.json", "w"))
"""


def wait_for(path, timeout, proc):
    end = time.monotonic() + timeout
    while not path.exists():
        if proc.poll() is not None:
            raise RuntimeError(f"app exited ({proc.returncode}) before {path.name}")
        if time.monotonic() > end:
            raise TimeoutError(path.name)
        time.sleep(0.01)


class Watch:
    """Background reader: (wall time, line) for every line a monitor process prints."""

    def __init__(self, cmd, env=None):
        self.lines = []
        self.proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
                                     bufsize=1, env=env)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.proc.stdout:
            self.lines.append((time.time(), line.rstrip("\n")))

    def stop(self):
        self.proc.terminate()
        try:
            self.proc.wait(3)
        except subprocess.TimeoutExpired:
            self.proc.kill()


class Linux:
    def __init__(self, env):
        if not env.get("DBUS_SESSION_BUS_ADDRESS"):
            sys.exit("DBUS_SESSION_BUS_ADDRESS is unset: run from the desktop session")
        owner = subprocess.run(["dbus-send", "--session", "--print-reply", "--dest=org.freedesktop.DBus", "/",
                                "org.freedesktop.DBus.GetNameOwner", "string:org.freedesktop.Notifications"],
                               env=env, capture_output=True)
        if owner.returncode:
            print("warning: no notification server on this bus; Notify calls are still timed", file=sys.stderr)
        self.env = env
        self.away = subprocess.Popen(["xmessage", "-geometry", "+0+0", "focus away"], env=env)
        self.notes = Watch(["dbus-monitor", "--session",
                            "type='method_call',interface='org.freedesktop.Notifications',member='Notify'"], env)
        time.sleep(0.5)

    def x(self, *a):
        return subprocess.run(["xdotool", *a], env=self.env, capture_output=True, text=True).stdout.split()

    def window(self, how):
        end = time.monotonic() + 10
        while time.monotonic() < end:
            wins = self.x("search", "--onlyvisible", *how)
            if wins:
                return wins[-1]
            time.sleep(0.1)
        raise TimeoutError(f"window {how}")

    def start(self, pid, unfocus):
        win = self.window(["--pid", str(pid)])
        if unfocus:
            self.x("windowactivate", "--sync", self.window(["--class", "xmessage"]))
        self.titles = Watch(["xprop", "-id", win, "-spy", "_NET_WM_NAME"], self.env)

    def seen(self):
        titles = [(t, m.group(1)) for t, l in self.titles.lines if (m := re.search(r'"(K95-\d+)"', l))]
        notes = []
        for _, l in self.notes.lines:
            m = re.search(r"method call time=([\d.]+)", l)
            if m:
                notes.append(float(m.group(1)))
        self.titles.stop()
        return titles, notes

    def close(self):
        self.notes.stop()
        self.away.terminate()


JXA = """
ObjC.import('Foundation');
const se = Application('System Events');
const p = se.processes.whose({unixId: %d})[0];
for (;;) {
  try { const n = p.windows[0].name(); $.NSFileHandle.fileHandleWithStandardOutput.writeData(
        $(Date.now() / 1000 + ' ' + n + '\\n').dataUsingEncoding($.NSUTF8StringEncoding)); } catch (e) {}
  delay(0.005);
}
"""


class Mac:
    def __init__(self, env):
        self.env = env

    def osa(self, script):
        subprocess.run(["osascript", "-e", script], capture_output=True, check=True)

    def start(self, pid, unfocus):
        self.osa(f'tell application "System Events" to set frontmost of (first process whose unix id is {pid}) to true')
        time.sleep(1)
        if unfocus:
            self.osa('tell application "Finder" to activate')
        self.titles = Watch(["osascript", "-l", "JavaScript", "-e", JXA % pid])
        self.notes, self.stop_poll = [], threading.Event()

        def poll():
            while not self.stop_poll.is_set():
                r = subprocess.run(["pgrep", "-f", "display notification"], capture_output=True)
                if r.returncode == 0 and (not self.notes or time.time() - self.notes[-1] > 1):
                    self.notes.append(time.time())
                time.sleep(0.02)
        threading.Thread(target=poll, daemon=True).start()
        time.sleep(0.5)

    def seen(self):
        self.stop_poll.set()
        titles = [(t, m.group(1)) for t, l in self.titles.lines if (m := re.search(r"(K95-\d+)", l))]
        self.titles.stop()
        return titles, self.notes

    def close(self):
        pass


def one_run(binary, scenario, env, desk, log):
    work = Path(tempfile.mkdtemp(prefix="k95timing-"))
    os.mkfifo(work / "fifo")
    (work / "run.sh").write_text(RUN_SH)
    (work / "run.sh").chmod(0o755)
    (work / "fixture.py").write_text(FIXTURE)
    proc = subprocess.Popen([str(binary), "-e", str(work / "run.sh"), str(work), scenario],
                            env=env, stdout=log, stderr=log)
    try:
        time.sleep(0.5)
        desk.start(proc.pid, scenario.startswith("notify"))
        (work / "go").touch()
        wait_for(work / "events.json", 180, proc)
        time.sleep(1.5)
        events = json.loads((work / "events.json").read_text())
        titles, notes = desk.seen()
        if scenario.startswith("title"):
            first = {}
            for t, name in titles:
                first.setdefault(name, t)
            lat = [first[f"K95-{n}"] - e for n, e in enumerate(events) if f"K95-{n}" in first]
            return {"latency_ms": [round(x * 1000, 1) for x in lat], "complete": f"K95-{len(events) - 1}" in first,
                    "titles_seen": len(first), "titles_sent": len(events)}
        after = [n for n in notes if n >= events[-1] - 0.05]
        return {"latency_ms": [round((after[0] - events[-1]) * 1000, 1)] if after else [],
                "complete": bool(after), "notifications": len(notes)}
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
    ap.add_argument("--runs", type=int, default=6, help="runs per scenario per build")
    ap.add_argument("--scenarios", default=",".join(SCENARIOS))
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    cfg = Path(tempfile.mkdtemp(prefix="k95cfg-"))
    (cfg / "terminal").mkdir()
    (cfg / "terminal" / "config.toml").write_text("notify_long_commands_after = 1\n")
    env = dict(os.environ, XDG_CONFIG_HOME=str(cfg), BASH_SILENCE_DEPRECATION_WARNING="1")
    desk = Mac(env) if MAC else Linux(env)
    builds = {"main": args.main.resolve(), "head": args.head.resolve()}
    scenarios = args.scenarios.split(",")
    jobs = [(s, b) for s in scenarios for b in builds for _ in range(args.runs)]
    random.shuffle(jobs)
    rows, log = [], open(args.out / "app.log", "w")
    try:
        for scenario, label in jobs:
            r = dict(scenario=scenario, build=label, **one_run(builds[label], scenario, env, desk, log))
            rows.append(r)
            print(f"{scenario:13s} {label:4s} complete={r['complete']!s:5s} latency_ms={r['latency_ms']}", flush=True)
    finally:
        log.close()
        desk.close()
    (args.out / "rows.json").write_text(json.dumps(rows, indent=1))
    failed = False
    print("\nscenario       main p50/max ms   head p50/max ms   head complete   verdict")
    for scenario in scenarios:
        stats = {}
        for label in builds:
            rs = [r for r in rows if r["scenario"] == scenario and r["build"] == label]
            lat = [x for r in rs for x in r["latency_ms"]]
            stats[label] = (statistics.median(lat) if lat else float("nan"), max(lat, default=float("nan")),
                            sum(r["complete"] for r in rs), len(rs))
        m, h = stats["main"], stats["head"]
        bad = h[2] < h[3] or not h[0] <= m[0] + max(20, 0.1 * m[0])
        failed |= bad
        print(f"{scenario:13s}  {m[0]:7.1f}/{m[1]:7.1f}   {h[0]:7.1f}/{h[1]:7.1f}   {h[2]}/{h[3]} (main {m[2]}/{m[3]})"
              f"   {'FAIL' if bad else 'ok'}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
