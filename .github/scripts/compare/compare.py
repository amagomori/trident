#!/usr/bin/env python3
"""Black-box comparison of `terminal` against other terminal emulators (Linux, X11).

    Xvfb :99 -screen 0 1920x1080x24 & DISPLAY=:99 openbox &
    cargo build --release
    python3 .github/scripts/compare/compare.py --terms terminal,alacritty
    python3 .github/scripts/compare/summarize.py

Every test launches a fresh terminal running a probe shell script, so nothing in any
terminal is instrumented. Tests:
  startup       launch -> shell exec, window mapped, first painted frame
  cat           wall time for `cat` of a 100 MiB text file, plus drain to idle
  vtebench      alacritty/vtebench suite, run inside the terminal
  latency       XTEST keypress -> pixels change on the X framebuffer (typometer-style)
  latency_load  the same with a background `cat` loop hogging the CPU (not run by default)
  memory        RSS/PSS of the terminal process tree (shell excluded) + idle CPU, before
                and after a 50,000 x 150-col scrollback burst (same burst as app_bench.py)

Results merge into `--json` (default target/compare/results.json), so tests and
terminals can be run separately. Each terminal's binary is found on PATH unless given
with `--bin NAME=PATH`; `terminal` defaults to this checkout's release build. The
configs in cfg/ give every terminal JetBrains Mono 14, 100x30, 10k scrollback, no
blink and ~6 px padding. On Xvfb with llvmpipe the numbers only compare terminals on
the same box; they are not keypress-to-photon latency (see latency.py for that).
"""

import argparse
import hashlib
import json
import os
import random
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    from Xlib import XK, X
    from Xlib import display as xdisplay
    from Xlib.ext import xtest
except ImportError:
    sys.exit("compare.py needs python-xlib (apt install python3-xlib, or pip install python-xlib)")

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]
CFG = HERE / "cfg"
TICK = os.sysconf("SC_CLK_TCK")
LIVE = set()


def default_bins():
    target = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
    return {
        "terminal": str(target / "release/terminal"),
        "alacritty": "alacritty",
        "kitty": "kitty",
        "wezterm": "wezterm",
        "ghostty": "ghostty",
    }


TERMS = {
    "terminal": lambda b, cmd: [b, "--font-size", "14", "--scrollback", "10000", "-e", *cmd],
    "alacritty": lambda b, cmd: [b, "--config-file", str(CFG / "alacritty.toml"), "-e", *cmd],
    "kitty": lambda b, cmd: [b, "--config", str(CFG / "kitty.conf"), *cmd],
    "wezterm": lambda b, cmd: [b, "--config-file", str(CFG / "wezterm.lua"), "start",
                               "--always-new-process", "--", *cmd],
    "ghostty": lambda b, cmd: [b, "--config-default-files=false",
                               f"--config-file={CFG / 'ghostty.conf'}", "-e", *cmd],
}


# Ghostty's bundled Mesa refuses its GL 4.3 core context on Xvfb (BadMatch) unless the
# advertised GL/GLSL version is overridden; it then presents through a CPU copy.
TERM_ENV = {"ghostty": {"MESA_GL_VERSION_OVERRIDE": "4.6", "MESA_GLSL_VERSION_OVERRIDE": "460",
                        "GTK_A11Y": "none"}}


def env(name):
    e = {k: v for k, v in os.environ.items() if k not in ("WAYLAND_DISPLAY",)}
    e.update(DISPLAY=ARGS.display, LIBGL_ALWAYS_SOFTWARE="1")
    e.update(TERM_ENV.get(name, {}))
    return e


# ---------- process helpers ----------
def children_map():
    kids = {}
    for p in Path("/proc").iterdir():
        if p.name.isdigit():
            try:
                ppid = int((p / "stat").read_text().rsplit(")", 1)[1].split()[1])
            except (OSError, IndexError):
                continue
            kids.setdefault(ppid, []).append(int(p.name))
    return kids


def tree(pid, exclude=()):
    kids, out, stack = children_map(), [], [pid]
    while stack:
        p = stack.pop()
        if p in exclude:
            continue
        out.append(p)
        stack.extend(kids.get(p, []))
    return out


def shell_pids(pidfile):
    try:
        shell = int(Path(pidfile).read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return set()
    return {shell}


def cpu_secs(pids):
    t = 0
    for p in pids:
        try:
            f = Path(f"/proc/{p}/stat").read_text().rsplit(")", 1)[1].split()
            t += int(f[11]) + int(f[12])
        except OSError:
            pass
    return t / TICK


def pss_kib(pids):
    t = 0
    for p in pids:
        try:
            for line in Path(f"/proc/{p}/smaps_rollup").read_text().splitlines():
                if line.startswith("Pss:"):
                    t += int(line.split()[1])
        except OSError:
            pass
    return t


def rss_kib(pids):
    t = 0
    for p in pids:
        try:
            for line in Path(f"/proc/{p}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    t += int(line.split()[1])
        except OSError:
            pass
    return t


class Term:
    def __init__(self, name, script, *args):
        self.name = name
        self.dir = Path(tempfile.mkdtemp(prefix=f"tb-{name}-"))
        self.pidfile = self.dir / "pid"
        cmd = ["/bin/sh", str(script), str(self.dir), *map(str, args)]
        self.t0 = time.monotonic_ns()
        self.proc = subprocess.Popen(TERMS[name](ARGS.bins[name], cmd), env=env(name), stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True,
                                     cwd=self.dir)
        LIVE.add(self)

    def wait_file(self, name, timeout=120):
        f = self.dir / name
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if f.exists() and f.stat().st_size:
                return f.read_text()
            time.sleep(0.005)
        raise TimeoutError(f"{self.name}: no {name}")

    def pids(self):
        return tree(self.proc.pid, exclude=shell_pids(self.pidfile))

    def settle(self, timeout=30):
        start, prev = time.monotonic(), cpu_secs(self.pids())
        while time.monotonic() - start < timeout:
            time.sleep(0.5)
            now = cpu_secs(self.pids())
            if now - prev < 2 / TICK and time.monotonic() - start > 1.5:
                time.sleep(1.0)
                return time.monotonic() - start
            prev = now
        return None

    def drain(self, idle=1.5, timeout=120):
        """Seconds from now until the terminal's last busy CPU sample before `idle` s of quiet."""
        start = time.monotonic()
        prev, last_busy = cpu_secs(self.pids()), start
        while time.monotonic() - start < timeout:
            time.sleep(0.05)
            now = cpu_secs(self.pids())
            if now - prev >= 2 / TICK:
                last_busy = time.monotonic()
            elif time.monotonic() - last_busy >= idle:
                return last_busy - start
            prev = now
        return None

    def close(self):
        LIVE.discard(self)
        # snapshot first: helpers outside the launcher's group reparent to init once it exits
        procs = tree(self.proc.pid)
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(self.proc.pid, sig)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(2)
                break
            except subprocess.TimeoutExpired:
                pass
        # AppImage/wezterm may leave helpers in their own groups: kill whole tree
        for p in set(procs) | set(tree(self.proc.pid)):
            try:
                os.kill(p, signal.SIGKILL)
            except ProcessLookupError:
                pass
        shutil.rmtree(self.dir, ignore_errors=True)
        time.sleep(0.5)


def script(body):
    f = ARGS.work / "probes" / (hashlib.sha1(body.encode()).hexdigest()[:12] + ".sh")
    f.parent.mkdir(parents=True, exist_ok=True)
    f.write_text('D="$1"; shift; echo $$ > "$D/pid"\n' + body + "\n")
    return f


# ---------- X helpers ----------
def top_windows(d):
    out = []
    def walk(w, depth):
        try:
            for c in w.query_tree().children:
                a = c.get_attributes()
                g = c.get_geometry()
                if a.map_state == X.IsViewable and g.width > 200 and g.height > 200 and \
                        a.win_class == X.InputOutput:
                    out.append(c)
                if depth < 1:
                    walk(c, depth + 1)
        except Exception:
            pass
    walk(d.screen().root, 0)
    return out


def app_window(d, before=()):
    """Largest client window (openbox reparents into a frame; look one level down)."""
    best = None
    for w in top_windows(d):
        if w.id in before:
            continue
        try:
            if w.get_wm_class() is None and w.query_tree().children:
                continue
            g = w.get_geometry()
        except Exception:
            continue
        if best is None or g.width * g.height > best[1]:
            best = (w, g.width * g.height)
    return best[0] if best else None


def abs_geom(d, w):
    g = w.get_geometry()
    t = w.translate_coords(d.screen().root, 0, 0)
    return -t.x, -t.y, g.width, g.height


# ---------- tests ----------
def t_startup(name):
    s = script('date +%s%N > "$D/exec.tmp"; mv "$D/exec.tmp" "$D/exec"; exec sleep 600')
    d = xdisplay.Display(ARGS.display)
    res = []
    for _ in range(ARGS.startup_runs):
        before = {w.id for w in top_windows(d)}
        wall0 = time.time_ns()
        t = Term(name, s)
        mapped = None
        end = time.monotonic() + 30
        while time.monotonic() < end and mapped is None:
            if any(w.id not in before for w in top_windows(d)):
                mapped = (time.monotonic_ns() - t.t0) / 1e6
            else:
                time.sleep(0.002)
        # first frame with anything drawn on it (cursor or text), on the client window
        painted, w = None, None
        root = d.screen().root
        while time.monotonic() < end and painted is None:
            w = w or app_window(d, before)
            if w is not None:
                try:
                    x, y, ww, wh = abs_geom(d, w)
                    img = grab(root, x, y, ww, wh)
                    if len(set(img[i:i + 4] for i in range(0, len(img), 4 * 97))) > 1:
                        painted = (time.monotonic_ns() - t.t0) / 1e6
                except Exception:
                    w = None
            if painted is None:
                time.sleep(0.002)
        exec_ms = (int(t.wait_file("exec")) - wall0) / 1e6
        res.append({"exec_ms": exec_ms, "mapped_ms": mapped, "painted_ms": painted})
        t.close()
    return res


def t_cat(name):
    s = script('sleep 2; s=$(date +%s%N); cat "$1"; e=$(date +%s%N); '
               'echo $s $e > "$D/t.tmp"; mv "$D/t.tmp" "$D/t"; exec sleep 600')
    res = []
    for _ in range(ARGS.cat_runs):
        t = Term(name, s, ARGS.cat_file)
        a, b = map(int, t.wait_file("t", 600).split())
        cat_s = (b - a) / 1e9
        drain = t.drain()
        res.append({"cat_s": cat_s, "drain_after_s": drain,
                    "rendered_s": None if drain is None else cat_s + drain,
                    "term_cpu_s": cpu_secs(t.pids())})
        t.close()
    return res


def t_vtebench(name):
    out = ARGS.work / f"vtebench-{name}.dat"
    s = script(f'sleep 2; cd "{ARGS.vtebench_dir}" && "{ARGS.vtebench}" -s '
               f'--max-secs {ARGS.vte_secs} --dat "$D/out.dat" 2> "$D/log"; '
               'cp "$D/out.dat" "$1"; touch "$D/done"; echo x > "$D/done"; exec sleep 600')
    t = Term(name, s, out)
    t.wait_file("done", 60 * 60)
    t.close()
    return parse_dat(out)


def parse_dat(path):
    lines = [l.split() for l in Path(path).read_text().splitlines() if l.strip()]
    head, rows = lines[0], lines[1:]
    res = {}
    for i, bench in enumerate(head):
        vals = [float(r[i]) for r in rows if i < len(r) and r[i].replace(".", "", 1).isdigit()]
        if vals:
            res[bench] = {"mean_ms": statistics.fmean(vals), "median_ms": statistics.median(vals),
                          "n": len(vals)}
    return res


def keycode(d, ks):
    return d.keysym_to_keycode(XK.string_to_keysym(ks))


def press(d, code):
    xtest.fake_input(d, X.KeyPress, code)
    xtest.fake_input(d, X.KeyRelease, code)
    d.sync()


def grab(root, x, y, w, h):
    return root.get_image(x, y, w, h, X.ZPixmap, 0xFFFFFFFF).data


def t_latency(name, load=False):
    body = 'sleep 1; clear; '
    if load:
        body += '(while :; do cat "$1"; done) >/dev/null & '
    body += 'touch "$D/ready"; echo x > "$D/ready"; exec cat'
    d = xdisplay.Display(ARGS.display)
    root = d.screen().root
    before = {w.id for w in top_windows(d)}
    t = Term(name, script(body), ARGS.cat_file)
    t.wait_file("ready", 60)
    t.settle(20)
    w = None
    end = time.monotonic() + 10
    while w is None and time.monotonic() < end:
        w = app_window(d, before)
        time.sleep(0.1)
    x, y, ww, wh = abs_geom(d, w)
    subprocess.run(["xdotool", "windowactivate", str(w.id)], env=env(name),
                   stderr=subprocess.DEVNULL)
    xtest.fake_input(d, X.MotionNotify, x=x + ww // 2, y=y + wh // 2)
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.5)
    w.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    time.sleep(0.5)
    kx, kb = keycode(d, "x"), keycode(d, "BackSpace")
    # calibrate: find the region a typed character changes
    base = grab(root, x, y, ww, wh)
    press(d, kx)
    time.sleep(1.0)
    after = grab(root, x, y, ww, wh)
    press(d, kb)
    time.sleep(1.0)
    if base == after:
        t.close()
        return {"error": "keypress produced no visible change"}
    bpp = len(base) // (ww * wh)
    diff = [i for i in range(0, len(base), bpp) if base[i:i + bpp] != after[i:i + bpp]]
    ys = [(i // bpp) // ww for i in diff]
    xs = [(i // bpp) % ww for i in diff]
    rx, ry = max(0, min(xs) - 4), max(0, min(ys) - 4)
    rw, rh = min(ww - rx, max(xs) - rx + 5), min(wh - ry, max(ys) - ry + 5)
    samples, timeouts = [], 0
    for i in range(ARGS.latency_probes):
        time.sleep(random.uniform(0.06, 0.14))
        code = kx if i % 2 == 0 else kb
        cur = grab(root, x + rx, y + ry, rw, rh)
        t0 = time.perf_counter()
        press(d, code)
        while True:
            if grab(root, x + rx, y + ry, rw, rh) != cur:
                samples.append((time.perf_counter() - t0) * 1000)
                break
            if time.perf_counter() - t0 > 1.0:
                timeouts += 1
                break
    t.close()
    return summarize(samples) | {"timeouts": timeouts, "region": [rw, rh]}


def summarize(v):
    if not v:
        return {"n": 0}
    v = sorted(v)
    q = lambda p: v[min(len(v) - 1, int(p * len(v)))]
    return {"n": len(v), "min": v[0], "median": statistics.median(v), "p90": q(.9),
            "p99": q(.99), "max": v[-1], "mean": statistics.fmean(v),
            "stdev": statistics.pstdev(v)}


def t_memory(name):
    s = script('touch "$D/ready"; echo x > "$D/ready"; read _ < "$D/go"; '
               'seq -f "%0150g" 1 50000; touch "$D/burst"; echo x > "$D/burst"; exec sleep 600')
    res = []
    for _ in range(ARGS.mem_runs):
        t = Term(name, s)
        os.mkfifo(t.dir / "go")
        t.wait_file("ready", 60)
        t.settle()
        open_rss, open_pss = rss_kib(t.pids()), pss_kib(t.pids())
        c0 = cpu_secs(t.pids()); time.sleep(ARGS.idle_secs); c1 = cpu_secs(t.pids())
        with open(t.dir / "go", "w") as f:
            f.write("go\n")
        peak, burst_t0 = open_rss, time.monotonic()
        while not (t.dir / "burst").exists() or time.monotonic() - burst_t0 < 0.2:
            peak = max(peak, rss_kib(t.pids()))
            time.sleep(0.02)
        for _ in range(4):  # a terminal that never idles (Ghostty on Xvfb) is measured anyway
            if t.settle(5) is not None:
                break
            peak = max(peak, rss_kib(t.pids()))
        peak = max(peak, rss_kib(t.pids()))
        steady, steady_pss = rss_kib(t.pids()), pss_kib(t.pids())
        c2 = cpu_secs(t.pids()); time.sleep(ARGS.idle_secs); c3 = cpu_secs(t.pids())
        res.append({"open_rss_mib": open_rss / 1024, "peak_rss_mib": peak / 1024,
                    "steady_rss_mib": steady / 1024, "open_pss_mib": open_pss / 1024,
                    "steady_pss_mib": steady_pss / 1024,
                    "open_idle_cpu_pct": 100 * (c1 - c0) / ARGS.idle_secs,
                    "steady_idle_cpu_pct": 100 * (c3 - c2) / ARGS.idle_secs,
                    "procs": len(t.pids())})
        t.close()
    return res


TESTS = {"startup": t_startup, "cat": t_cat, "vtebench": t_vtebench, "latency": t_latency,
         "latency_load": lambda n: t_latency(n, load=True), "memory": t_memory}


def make_cat_file(path, mib=100):
    """Deterministic printable ASCII, lines of 1-150 columns."""
    path.parent.mkdir(parents=True, exist_ok=True)
    rng = random.Random(0)
    alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,:;-_=+/()[]{}"
    lines = ["".join(rng.choice(alphabet) for _ in range(rng.randint(1, 150))) for _ in range(4096)]
    block = ("\n".join(lines) + "\n").encode()
    tmp = path.with_suffix(".tmp")
    with open(tmp, "wb") as f:
        left = mib << 20
        while left:
            f.write(block[:left])
            left -= min(left, len(block))
    tmp.rename(path)


def resolve_bins(names, overrides):
    bins = default_bins()
    for item in overrides:
        name, sep, path = item.partition("=")
        if not sep or name not in TERMS:
            sys.exit(f"--bin wants NAME=PATH with NAME in {', '.join(TERMS)}: {item}")
        bins[name] = path
    for name in names:
        found = shutil.which(os.path.expanduser(bins[name]))
        if not found:
            sys.exit(f"{name}: {bins[name]} not found (pass --bin {name}=PATH)")
        bins[name] = os.path.abspath(found)
    return bins


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--display", default=os.environ.get("DISPLAY", ":99"))
    p.add_argument("--terms", default=",".join(TERMS))
    p.add_argument("--tests", default="startup,memory,latency,cat,vtebench")
    p.add_argument("--bin", action="append", default=[], metavar="NAME=PATH",
                   help="binary for one terminal (repeatable)")
    p.add_argument("--work", type=Path, default=REPO / "target/compare",
                   help="probe scripts, generated data and vtebench output")
    p.add_argument("--json", type=Path, help="results file (default WORK/results.json)")
    p.add_argument("--cat-file", type=Path, help="default WORK/100mib.txt, generated if missing")
    p.add_argument("--vtebench", default="vtebench", help="vtebench binary")
    p.add_argument("--vtebench-dir", type=Path, default=Path("~/vtebench").expanduser(),
                   help="vtebench checkout (its benchmarks/ are run)")
    p.add_argument("--startup-runs", type=int, default=10)
    p.add_argument("--cat-runs", type=int, default=5)
    p.add_argument("--mem-runs", type=int, default=3)
    p.add_argument("--latency-probes", type=int, default=200)
    p.add_argument("--idle-secs", type=float, default=5.0)
    p.add_argument("--vte-secs", type=int, default=10)
    ARGS = p.parse_args()
    names, tests = ARGS.terms.split(","), ARGS.tests.split(",")
    for test in tests:
        if test not in TESTS:
            sys.exit(f"unknown test {test}; choose from {', '.join(TESTS)}")
    ARGS.bins = resolve_bins(names, ARGS.bin)
    ARGS.work = ARGS.work.resolve()
    ARGS.work.mkdir(parents=True, exist_ok=True)
    if ARGS.cat_file is None:
        ARGS.cat_file = ARGS.work / "100mib.txt"
        if not ARGS.cat_file.exists() and {"cat", "latency_load"} & set(tests):
            make_cat_file(ARGS.cat_file)
    ARGS.cat_file = str(ARGS.cat_file.resolve())
    if "vtebench" in tests:
        found = shutil.which(os.path.expanduser(ARGS.vtebench))
        if not found or not (ARGS.vtebench_dir / "benchmarks").is_dir():
            sys.exit("vtebench needs the vtebench binary and a checkout with benchmarks/ "
                     "(--vtebench, --vtebench-dir)")
        ARGS.vtebench = found
    path = ARGS.json or ARGS.work / "results.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    results = json.loads(path.read_text()) if path.exists() else {}
    for test in tests:
        for name in names:
            print(f"[{time.strftime('%H:%M:%S')}] {test} {name}", flush=True)
            try:
                r = TESTS[test](name)
            except Exception as e:
                r = {"error": repr(e)}
            finally:
                for t in list(LIVE):
                    t.close()
            print("   ", json.dumps(r)[:400], flush=True)
            results.setdefault(test, {})[name] = r
            path.write_text(json.dumps(results, indent=1))
    print(f"results: {path}")
