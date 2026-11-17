#!/usr/bin/env python3
"""Compare real-display keypress-to-pixel latency with Alacritty.

Use a real display, not Xvfb. The method is the same for every build, including terminals that
cannot carry our instrumentation.

Linux: XTEST key -> XGetImage poll of the glyph region (method of KAN-62/KAN-91 compare.py),
       on the session's X server (DISPLAY, must not be Xvfb). Needs python3-xlib, xdotool.
macOS: keylat (keylat.swift: CGEvent key -> CGWindowListCreateImage poll), logged-in GUI session,
       Accessibility + Screen Recording granted to the shell running this.
Every round launches head, main, an identical main copy (A/A) and Alacritty in a fresh
shuffled order; reports per-label median-of-medians and p99, and per-round paired deltas
with 95% bootstrap CIs.

  python3 .github/scripts/latency-qa/vs_alacritty.py --head H/release/trident --main M/release/trident \
      --rounds 6 --probes 200 --out ~/kan89/vs-x11
  Alacritty is found on PATH, else in /Applications or ~/Applications (macOS); --alacritty overrides.
  add --load to also run with `cat`-ing a 100 MiB file in the background of the same shell.
"""
import argparse, json, os, random, shlex, shutil, signal, statistics, subprocess, sys, tempfile, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ALACRITTY_TOML = """[window]
dimensions = { columns = 100, lines = 30 }
padding = { x = 6, y = 6 }
[font]
size = 14
normal = { family = "JetBrains Mono" }
[scrolling]
history = 10000
[cursor]
style = { blinking = "Never" }
"""
TERMINAL_TOML = "font_size = 14\npadding = 6\nscrollback = 10000\n"


MAC_ALACRITTY = ("/Applications/Alacritty.app", "~/Applications/Alacritty.app")


def require_executable(ap, flag, path):
    if not path.is_file() or not os.access(path, os.X_OK):
        what = "does not exist" if not path.exists() else "is not an executable file"
        ap.error(f"{flag} {path} {what}")


def find_alacritty(ap, given):
    if given:
        path = Path(given).expanduser()
        if path.suffix == ".app" and path.is_dir():
            path = path / "Contents/MacOS/alacritty"
        return path
    found = shutil.which("alacritty")
    if found:
        return Path(found)
    for app in MAC_ALACRITTY if sys.platform == "darwin" else ():
        path = Path(app).expanduser() / "Contents/MacOS/alacritty"
        if path.is_file():
            return path
    ap.error("no Alacritty found on PATH" + (" or in " + ", ".join(MAC_ALACRITTY) if sys.platform == "darwin" else "")
             + "; install it or pass --alacritty PATH")


def boot_ci(values, n=10000, seed=89):
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(n))
    return statistics.median(values), meds[int(.025 * n)], meds[int(.975 * n) - 1]


def summarize(v, timeouts):
    v = sorted(v)
    q = lambda p: v[min(len(v) - 1, int(p * len(v)))]
    return {"n": len(v), "timeouts": timeouts, "median": statistics.median(v) if v else None,
            "p90": q(.9) if v else None, "p99": q(.99) if v else None, "max": v[-1] if v else None}


def launch(args, label, binary, load):
    work = Path(tempfile.mkdtemp(prefix=f"kan89-{label}-"))
    body = "sleep 1; clear; "
    if load:
        body += f'(while :; do cat {shlex.quote(str(args.cat_file))}; done) >/dev/null & '
    body += f'touch {shlex.quote(str(work / "ready"))}; exec cat'
    probe = work / "probe.sh"
    probe.write_text(body + "\n")
    cmd = ["/bin/sh", str(probe)]
    if label == "alacritty":
        argv = [str(binary), "--config-file", str(args.out / "alacritty.toml"), "-e", *cmd]
    else:
        argv = [str(binary), "--config", str(args.out / "terminal.toml"), "-e", *cmd]
    env = dict(os.environ, XDG_CONFIG_HOME=str(work / "cfg"))
    proc = subprocess.Popen(argv, env=env, cwd=work, stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL, start_new_session=True)
    end = time.monotonic() + 60
    while not (work / "ready").exists():
        if time.monotonic() > end or proc.poll() is not None:
            raise RuntimeError(f"{label}: probe shell never started")
        time.sleep(0.01)
    time.sleep(args.settle)
    return proc, work


def close(proc, work):
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(proc.pid, sig)
            proc.wait(2)
            break
        except (ProcessLookupError, subprocess.TimeoutExpired):
            pass
    shutil.rmtree(work, ignore_errors=True)
    time.sleep(0.5)


# ---------- Linux / X11 ----------
def x11_measure(args, proc, before):
    from Xlib import X, XK, display as xdisplay
    from Xlib.ext import xtest
    d = xdisplay.Display()
    root = d.screen().root

    def tops():
        out = []
        def walk(w, depth):
            for c in w.query_tree().children:
                try:
                    a, g = c.get_attributes(), c.get_geometry()
                except Exception:
                    continue
                if a.map_state == X.IsViewable and g.width > 200 and g.height > 200 and a.win_class == X.InputOutput:
                    out.append(c)
                if depth < 1:
                    walk(c, depth + 1)
        walk(root, 0)
        return out

    w, end = None, time.monotonic() + 10
    while w is None and time.monotonic() < end:
        # the launched binary owns its window (_NET_WM_PID); this works under any reparenting WM
        ids = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(proc.pid)],
                             capture_output=True, text=True).stdout.split()
        cands = []
        for wid in ids:
            c = d.create_resource_object("window", int(wid))
            try:
                g = c.get_geometry()
            except Exception:
                continue
            if g.width > 200 and g.height > 200:
                cands.append((g.width * g.height, c))
        if not cands:  # fallback: largest new client window
            for c in tops():
                if c.id in before:
                    continue
                try:
                    if c.get_wm_class() is None and c.query_tree().children:
                        continue
                    g = c.get_geometry()
                except Exception:
                    continue
                cands.append((g.width * g.height, c))
        w = max(cands, key=lambda t: t[0])[1] if cands else None
        time.sleep(0.1)
    if w is None:
        return {"error": "no window"}
    g, t = w.get_geometry(), w.translate_coords(root, 0, 0)
    x, y, ww, wh = -t.x, -t.y, g.width, g.height
    subprocess.run(["xdotool", "windowactivate", "--sync", str(w.id)], stderr=subprocess.DEVNULL)
    xtest.fake_input(d, X.MotionNotify, x=x + ww // 2, y=y + wh // 2)
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.5)
    w.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    time.sleep(0.5)
    kx, kb = (d.keysym_to_keycode(XK.string_to_keysym(k)) for k in ("x", "BackSpace"))

    def press(code):
        xtest.fake_input(d, X.KeyPress, code)
        xtest.fake_input(d, X.KeyRelease, code)
        d.sync()

    grab = lambda gx, gy, gw, gh: root.get_image(gx, gy, gw, gh, X.ZPixmap, 0xFFFFFFFF).data
    base = grab(x, y, ww, wh)
    press(kx)
    time.sleep(1.0)
    after = grab(x, y, ww, wh)
    press(kb)
    time.sleep(1.0)
    if base == after:
        return {"error": "keypress produced no visible change (focus? capture permission?)"}
    bpp = len(base) // (ww * wh)
    diff = [i // bpp for i in range(0, len(base), bpp) if base[i:i + bpp] != after[i:i + bpp]]
    xs, ys = [i % ww for i in diff], [i // ww for i in diff]
    rx, ry = max(0, min(xs) - 4), max(0, min(ys) - 4)
    rw, rh = min(ww - rx, max(xs) - rx + 5), min(wh - ry, max(ys) - ry + 5)
    samples, timeouts = [], 0
    for i in range(args.probes):
        time.sleep(random.uniform(0.06, 0.14))
        cur = grab(x + rx, y + ry, rw, rh)
        t0 = time.perf_counter()
        press(kx if i % 2 == 0 else kb)
        while True:
            if grab(x + rx, y + ry, rw, rh) != cur:
                samples.append((time.perf_counter() - t0) * 1000)
                break
            if time.perf_counter() - t0 > 1.0:
                timeouts += 1
                break
    d.close()
    return summarize(samples, timeouts) | {"region": [rw, rh]}


def x11_windows():
    from Xlib import display as xdisplay
    d = xdisplay.Display()
    ids = {c.id for c in d.screen().root.query_tree().children}
    for c in d.screen().root.query_tree().children:
        try:
            ids |= {k.id for k in c.query_tree().children}
        except Exception:
            pass
    d.close()
    return ids


# ---------- macOS ----------
def mac_measure(args, proc, before):
    out = subprocess.run([str(args.keylat), "--pid", str(proc.pid), "--probes", str(args.probes)],
                         capture_output=True, text=True, timeout=600)
    try:
        r = json.loads(out.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"error": (out.stderr or out.stdout).strip()[-300:]}
    if "samples" not in r:
        return r
    return summarize(r["samples"], r["timeouts"]) | {"region": r.get("region")}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--head", type=Path, required=True)
    ap.add_argument("--main", type=Path, required=True)
    ap.add_argument("--alacritty", help="Alacritty binary or Alacritty.app (default: PATH, then /Applications)")
    ap.add_argument("--rounds", type=int, default=6)
    ap.add_argument("--probes", type=int, default=200)
    ap.add_argument("--settle", type=float, default=2.0)
    ap.add_argument("--load", action="store_true", help="also measure under background cat load")
    ap.add_argument("--cat-file", type=Path)
    ap.add_argument("--keylat", type=Path, default=HERE / "keylat", help="macOS: compiled keylat.swift")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    if args.rounds < 3:
        ap.error("--rounds must be at least 3 for paired-round CIs")
    require_executable(ap, "--head", args.head)
    require_executable(ap, "--main", args.main)
    args.alacritty = find_alacritty(ap, args.alacritty)
    require_executable(ap, "--alacritty", args.alacritty)
    args.out = args.out.resolve()  # the probe shell runs in a temp cwd
    mac = sys.platform == "darwin"
    if not mac:
        if not os.environ.get("DISPLAY"):
            sys.exit("needs DISPLAY (an X server you can see; Xvfb is smoke only)")
        xvfb = subprocess.run(["pgrep", "-a", "Xvfb"], capture_output=True, text=True).stdout.split()
        if os.environ["DISPLAY"] in xvfb:
            print("WARNING: this X server looks like Xvfb; result is a smoke test, not KAN-89 evidence", file=sys.stderr)
    elif not args.keylat.exists():
        sys.exit(f"build keylat first: swiftc -O {HERE / 'keylat.swift'} -o {args.keylat}")
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "summary.json").unlink(missing_ok=True)
    (args.out / "alacritty.toml").write_text(ALACRITTY_TOML)
    (args.out / "terminal.toml").write_text(TERMINAL_TOML)
    if args.load and not args.cat_file:
        args.cat_file = args.out / "100mib.txt"
        if not args.cat_file.exists():
            line = ("".join(chr(33 + i % 94) for i in range(99)) + "\n").encode()
            args.cat_file.write_bytes(line * (100 * 1024 * 1024 // len(line)))
    if args.load:
        args.cat_file = args.cat_file.resolve()
        if not args.cat_file.is_file():
            sys.exit(f"no such --cat-file {args.cat_file}")
    copy = args.out / "terminal-main-copy"
    shutil.copy2(args.main, copy)
    bins = {"head": args.head.resolve(), "main": args.main.resolve(), "copy": copy.resolve(),
            "alacritty": args.alacritty.resolve()}
    measure = mac_measure if mac else x11_measure
    rows = []
    for rnd in range(1, args.rounds + 1):
        for load in ([False, True] if args.load else [False]):
            order = list(bins)
            random.shuffle(order)
            for label in order:
                before = set() if mac else x11_windows()
                proc, work = launch(args, label, bins[label], load)
                try:
                    r = measure(args, proc, before)
                finally:
                    close(proc, work)
                rows.append(dict(round=rnd, label=label, load=load, order=",".join(order), **r))
                print(f"round {rnd} {'load' if load else 'idle'} {label}: {json.dumps(r)}", file=sys.stderr)
                (args.out / "rows.json").write_text(json.dumps(rows, indent=1))
    lines = ["| workload | label | median of run medians (ms) | worst run p99 (ms) | run-median spread (ms) | timeouts |",
             "| --- | --- | ---: | ---: | --- | ---: |"]
    deltas = []
    summary = {}
    for load in ([False, True] if args.load else [False]):
        wl = "load" if load else "idle"
        ok = [r for r in rows if r["load"] == load and r.get("median") is not None]
        by = {(r["round"], r["label"]): r for r in ok}
        for label in bins:
            rs = [r for r in ok if r["label"] == label]
            if not rs:
                lines.append(f"| {wl} | {label} | ERROR | | | |")
                continue
            meds = [r["median"] for r in rs]
            lines.append(f"| {wl} | {label} | {statistics.median(meds):.2f} | {max(r['p99'] for r in rs):.2f} | "
                         f"{min(meds):.2f}-{max(meds):.2f} | {sum(r['timeouts'] for r in rs)} |")
        for a, b in (("copy", "main"), ("head", "main"), ("head", "alacritty"), ("main", "alacritty")):
            rs = [n for n in range(1, args.rounds + 1) if (n, a) in by and (n, b) in by]
            if len(rs) >= 3:
                ci = boot_ci([by[n, a]["median"] - by[n, b]["median"] for n in rs])
                summary[f"{wl}.{a}-{b}"] = ci
                deltas.append(f"| {wl} | {a} - {b} | {ci[0]:+.2f} [{ci[1]:+.2f}, {ci[2]:+.2f}] |")
        aa = summary.get(f"{wl}.copy-main")
        ha = summary.get(f"{wl}.head-alacritty")
        head_timeouts = sum(r.get("timeouts", 0) for r in rows if r["load"] == load and r["label"] == "head")
        if head_timeouts:
            summary[f"{wl}.verdict"] = f"NOT MET (head had {head_timeouts} timed-out probes)"
        elif aa and ha:
            noise = max(abs(aa[1]), abs(aa[2]))
            summary[f"{wl}.verdict"] = "MET (head no worse than Alacritty)" if ha[0] <= noise else \
                "NOT MET (head slower than Alacritty beyond A/A noise)"
        else:
            summary[f"{wl}.verdict"] = "NO VERDICT (fewer than 3 complete rounds; see errors above)"
    print("\n".join(lines))
    print("\n| workload | paired per-round delta of medians | median [95% CI] ms |\n| --- | --- | --- |")
    print("\n".join(deltas))
    for k, v in summary.items():
        if k.endswith("verdict"):
            print(f"{k}: {v}")
    (args.out / "summary.json").write_text(json.dumps(summary, indent=1))
    verdicts = [v for k, v in summary.items() if k.endswith("verdict")]
    if any(v.startswith("NO VERDICT") for v in verdicts):
        return 2
    return 0 if all(v.startswith("MET") for v in verdicts) else 1


if __name__ == "__main__":
    sys.exit(main())
