"""Markdown tables for run.sh results: Trident vs each competitor, with A/A (Trident run again as its own arm).
Lower is better everywhere. Trail flag: Trident worse than a competitor by more than max(2*|A/A|, 3%) (CPU: 1 pt).
    python3 .github/scripts/compare/macos/summarize.py [results dir]"""
import json, glob, os, sys, statistics as st
from pathlib import Path
R = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ.get("COMPARE_OUT", Path(__file__).resolve().parents[4] / "target/compare-macos")) / "res"; COMP = ["alacritty", "ghostty", "wezterm"]; ARMS = ["trident", "trident-aa"] + COMP
def pct(v, p): v = sorted(v); return v[min(len(v) - 1, int(len(v) * p))] if v else None
rows, trails = [], []
def add(group, metric, vals, unit="ms", pts=False):
    t, aa = vals.get("trident"), vals.get("trident-aa")
    if t is None or aa is None: return
    if pts: aad = aa - t; bound = max(2 * abs(aad), 1.0)
    else: aad = (aa - t) / t * 100 if t else 0; bound = max(2 * abs(aad), 3.0)
    cells = []
    for c in COMP:
        v = vals.get(c)
        if v is None: cells.append("n/a"); continue
        if pts: d = t - v; s = f"{v:.2f}{unit} ({d:+.2f} pts)"; worse = d > bound
        else: d = (t - v) / v * 100 if v else 0; s = f"{v:.2f}{unit} ({d:+.1f}%)"; worse = d > bound
        if worse: s = f"**{s}**"; trails.append(f"{group} / {metric}: trident {t:.2f}{unit} vs {c} {v:.2f}{unit} ({'%+.2f pts' % d if pts else '%+.1f%%' % d}; A/A {'%+.2f pts' % aad if pts else '%+.1f%%' % aad}, bound ±{bound:.1f}{' pts' if pts else '%'})")
        cells.append(s)
    rows.append((group, f"| {metric} | {t:.2f}{unit} | {aa:.2f}{unit} | {'%+.2f pts' % aad if pts else '%+.1f%%' % aad} | ±{bound:.1f}{' pts' if pts else '%'} | " + " | ".join(cells) + " |"))
def load(p): return json.loads(Path(p).read_text()) if Path(p).exists() else {}
# startup
st_ = {}; mp = {}
for a in ARMS:
    v = [x["painted_ms"] for f in sorted(glob.glob(str(R / f"{a}-startup*.json"))) for x in load(f)[1:] if isinstance(x.get("painted_ms"), (int, float)) and x["painted_ms"] > 0]
    if v: st_[a] = st.median(v)
add("Startup", "launch to first painted frame, p50 of warm runs (first launch per round dropped)", st_)
# latency
lat = {a: [x for f in sorted(glob.glob(str(R / f"{a}-latency-*.json"))) for row in load(f) for x in row.get("latency_ms", [])] for a in ARMS}
miss = {a: sum(row.get("missed", 0) for f in glob.glob(str(R / f"{a}-latency-*.json")) for row in load(f)) for a in ARMS}
for p, n in ((.5, "p50"), (.9, "p90"), (.99, "p99")):
    add("Keypress latency", f"key to composited frame {n} (ScreenCaptureKit, 4x100)", {a: pct(v, p) for a, v in lat.items() if v})
# memory
m = load(R / "memory.json")
for k, n in (("open_rss_mib", "RSS, window open"), ("peak_rss_mib", "peak RSS, 50k x 150 burst"), ("steady_rss_mib", "RSS after burst")):
    add("Memory", n, {a: st.median([x[k] for x in v if k in x]) for a, v in m.items() if any(k in x for x in v)}, " MiB")
for k, n in (("open_idle_cpu", "idle CPU, window open"), ("steady_idle_cpu", "idle CPU after burst")):
    add("Memory", n, {a: st.median([x[k] for x in v if k in x]) for a, v in m.items() if any(k in x for x in v)}, "%", pts=True)
# cat
c = load(R / "cat.json")
for k, n in (("cat_s", "cat 100 MiB, shell-timed"), ("rendered_s", "cat + drain to idle"), ("term_cpu_s", "terminal CPU during cat")):
    add("100 MiB cat", n, {a: st.median([x[k] for x in v if x.get(k) is not None]) for a, v in c.items() if any(x.get(k) is not None for x in v)}, " s")
# vtebench
vb = load(R / "vtebench.json")
benches = sorted({b for v in vb.values() for x in v for b in x if b != "error"})
for b in benches:
    add("vtebench (ms per sample, median)", b, {a: st.median([x[b] for x in v if b in x]) for a, v in vb.items() if any(b in x for x in v)})
out = ["Black-box numbers from one Mac (VM runs: paravirtual Metal, simulated 60 Hz display, not photon latency). Lower is better. Cells: competitor value (Trident vs competitor; + means we are slower/bigger). Bold = we trail beyond noise.", ""]
g0 = None
for g, line in rows:
    if g != g0:
        out += ["", f"### {g}", "", "| Metric | trident | A/A (trident again) | A/A delta | noise bound | " + " | ".join(COMP) + " |", "|---|---:|---:|---:|---:|" + "---:|" * len(COMP)]; g0 = g
    out.append(line)
out += ["", f"Latency probes missed (no frame within 1 s): " + ", ".join(f"{a} {miss[a]}" for a in ARMS), "", "### Where we trail beyond noise", ""] + ([f"- {t}" for t in trails] or ["- none"])
print("\n".join(out))
