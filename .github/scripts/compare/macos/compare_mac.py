"""Black-box vtebench / 100 MiB cat / RSS+idle CPU on macOS for Trident (+ an A/A arm: the same
binary again) vs other terminals. Driven by run.sh; nothing in any terminal is instrumented.
usage: compare_mac.py <vtebench|cat|memory> <rounds> <out.json> [arms,...]. Never redirect vtebench stdout."""
import json, os, signal, subprocess, sys, time, statistics as st
from pathlib import Path
HERE = Path(__file__).resolve().parent; REPO = HERE.parents[3]; C = HERE.parent / "cfg"
OUT = Path(os.environ.get("COMPARE_OUT", REPO / "target/compare-macos")); W = OUT / "work"; W.mkdir(parents=True, exist_ok=True)
V = Path(os.environ.get("VTEBENCH_DIR", Path.home() / "vtebench")); VB = os.environ.get("VTEBENCH", str(Path.home() / ".cargo/bin/vtebench"))
DATA = OUT / "100mib.txt"
TRIDENT = os.environ.get("TRIDENT", str(Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target")) / "release/trident"))
ALACRITTY = os.environ.get("ALACRITTY", str(Path.home() / ".cargo/bin/alacritty"))
ARMS = {
 "trident": lambda c: [TRIDENT, "--config", str(C / "terminal.toml"), "-e", *c],
 "trident-aa": lambda c: [TRIDENT, "--config", str(C / "terminal.toml"), "-e", *c],
 "alacritty": lambda c: [ALACRITTY, "--config-file", str(C / "alacritty.toml"), "-e", *c],
 "ghostty": lambda c: ["/Applications/Ghostty.app/Contents/MacOS/ghostty", "--config-default-files=false", f"--config-file={C / 'ghostty.conf'}", "-e", *c],
 "kitty": lambda c: ["/Applications/kitty.app/Contents/MacOS/kitty", "--config", str(C / "kitty.conf"), *c],
 "wezterm": lambda c: ["/Applications/WezTerm.app/Contents/MacOS/wezterm-gui", "--config-file", str(HERE / "wezterm.lua"), "start", "--always-new-process", "--", *c],
}
def ps_all():
    rows = {}
    for l in subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss=,time=,comm="], capture_output=True, text=True).stdout.splitlines():
        f = l.split(None, 4)
        if len(f) < 5: continue
        p = f[3].replace("-", ":").split(":"); secs = float(p[-1]) + 60 * int(p[-2]) + (3600 * int(p[-3]) if len(p) > 2 else 0) + (86400 * int(p[-4]) if len(p) > 3 else 0)
        rows[int(f[0])] = (int(f[1]), int(f[2]), secs, f[4])
    return rows
SHELLS = ("sh", "bash", "zsh", "cat", "sleep", "seq", "vtebench", "login")
def tree(root):
    """terminal-owned processes under root (shell/workload children excluded)."""
    rows = ps_all(); out = [root] if root in rows else []; todo = [root]
    while todo:
        p = todo.pop()
        for c, r in rows.items():
            if r[0] == p and os.path.basename(r[3]) not in SHELLS: out.append(c); todo.append(c)
    return out, rows
def usage(root):
    pids, rows = tree(root); return sum(rows[p][1] for p in pids), sum(rows[p][2] for p in pids)
def launch(arm, sh):
    return subprocess.Popen(ARMS[arm](["/bin/sh", "-c", sh]), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
def close(p):
    for f in (lambda: os.killpg(p.pid, signal.SIGKILL), lambda: p.kill()):
        try: f()
        except (ProcessLookupError, PermissionError): pass
    p.wait(); time.sleep(1.5)
def wait(f, t):
    t0 = time.monotonic()
    while not f.exists() and time.monotonic() - t0 < t: time.sleep(0.1)
    return f.exists()
def settle(p, timeout=30, peak=None):
    t0 = time.monotonic(); prev = usage(p.pid)[1]; last = t0
    while time.monotonic() - t0 < timeout:
        time.sleep(0.1); rss, cpu = usage(p.pid)
        if peak is not None: peak[0] = max(peak[0], rss)
        if time.monotonic() - last >= 0.5:
            if cpu - prev < 0.02 and time.monotonic() - t0 > 1.0: return time.monotonic() - t0
            prev, last = cpu, time.monotonic()
    return None
def idle(p, secs=5.0):
    c0 = usage(p.pid)[1]; time.sleep(secs); return (usage(p.pid)[1] - c0) / secs * 100
def parse(path):
    lines = [l.split() for l in Path(path).read_text().splitlines() if l.strip()]
    return {b: st.median(v) for i, b in enumerate(lines[0]) if (v := [float(r[i]) for r in lines[1:] if i < len(r) and r[i].replace(".", "", 1).isdigit()])}
def vtebench(arm, r):
    d = W / f"{arm}-{r}.dat"; done = W / "done"; done.unlink(missing_ok=True); d.unlink(missing_ok=True)
    p = launch(arm, f'sleep 2; cd "{V}" && "{VB}" -s --max-secs {os.environ.get("VSECS", "6")} --dat "{d}" 2> "{W}/vlog"; touch "{done}"; exec sleep 600')
    ok = wait(done, 1800); close(p); return parse(d) if ok and d.exists() else {"error": "no result"}
def cat(arm, r):
    f = W / "t"; f.unlink(missing_ok=True)
    p = launch(arm, f'sleep 2; s=$(python3 -c "import time;print(time.time())"); cat "{DATA}"; e=$(python3 -c "import time;print(time.time())"); echo $s $e > "{f}.tmp"; mv "{f}.tmp" "{f}"; exec sleep 600')
    if not wait(f, 600): close(p); return {"error": "timeout"}
    a, b = map(float, f.read_text().split()); drain = settle(p, 120); cpu = usage(p.pid)[1]; close(p)
    return {"cat_s": b - a, "drain_s": drain, "rendered_s": None if drain is None else b - a + drain, "term_cpu_s": cpu}
def memory(arm, r):
    fifo = W / "go"; fifo.unlink(missing_ok=True); os.mkfifo(fifo); rd = W / "ready"; rd.unlink(missing_ok=True)
    p = launch(arm, f'touch "{rd}"; read _ < "{fifo}"; seq -f "%0150g" 1 50000; exec sleep 600')
    try:
        if not wait(rd, 30): return {"error": "no ready"}
        time.sleep(1); settle(p); o = {"open_rss_mib": usage(p.pid)[0] / 1024, "open_idle_cpu": idle(p)}
        peak = [0]
        with open(fifo, "w") as f: f.write("go\n")
        settle(p, 60, peak); time.sleep(1)
        o["steady_rss_mib"] = usage(p.pid)[0] / 1024; o["peak_rss_mib"] = max(peak[0] / 1024, o["steady_rss_mib"]); o["steady_idle_cpu"] = idle(p)
        o["procs"] = len(tree(p.pid)[0]); return o
    finally: close(p)
test, R, out = sys.argv[1], int(sys.argv[2]), Path(sys.argv[3])
arms = sys.argv[4].split(",") if len(sys.argv) > 4 else list(ARMS)
res = json.loads(out.read_text()) if out.exists() else {}
fn = {"vtebench": vtebench, "cat": cat, "memory": memory}[test]
for r in range(R):
    for a in (arms if r % 2 == 0 else arms[::-1]):
        v = fn(a, r); res.setdefault(a, []).append(v); print(test, a, r, json.dumps(v)[:300], flush=True)
        out.write_text(json.dumps(res, indent=1))
