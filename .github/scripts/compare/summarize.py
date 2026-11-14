#!/usr/bin/env python3
"""Render compare.py results as Markdown tables (medians unless noted).

    python3 .github/scripts/compare/summarize.py [results.json]
"""
import json
import statistics as st
import sys
from pathlib import Path

DEFAULT = Path(__file__).resolve().parents[3] / "target/compare/results.json"
d = json.loads(Path(sys.argv[1] if len(sys.argv) > 1 else DEFAULT).read_text())
ORDER = ["terminal", "alacritty", "kitty", "wezterm", "ghostty"]
seen = {t for test in d.values() for t in test}
T = [t for t in ORDER if t in seen] + sorted(seen - set(ORDER))
med = lambda xs: st.median(xs) if xs else None
f = lambda v, p=1: "n/a" if v is None else f"{v:.{p}f}"

def rows(test):
    return {t: d[test][t] for t in T if t in d[test]}

def ok(r):
    return isinstance(r, list) and bool(r)

out = []
if "startup" in d:
    out += ["### Startup (ms, median of runs)", "| terminal | exec | window mapped | first paint | runs |", "|---|---|---|---|---|"]
    for t, r in rows("startup").items():
        if ok(r):
            out.append(f"| {t} | {f(med([x['exec_ms'] for x in r]))} | {f(med([x['mapped_ms'] for x in r]))} | {f(med([x['painted_ms'] for x in r if x.get('painted_ms')]))} | {len(r)} |")
        else:
            out.append(f"| {t} | {r} |")
if "memory" in d:
    out += ["", "### Memory and idle CPU (median of runs; steady = after 50k-line burst)",
            "| terminal | open RSS MiB | steady RSS MiB | open PSS MiB | steady PSS MiB | idle CPU % open/steady | procs |", "|---|---|---|---|---|---|---|"]
    for t, r in rows("memory").items():
        if ok(r):
            g = lambda k: med([x[k] for x in r if x.get(k) is not None])
            out.append(f"| {t} | {f(g('open_rss_mib'))} | {f(g('steady_rss_mib'))} | {f(g('open_pss_mib'))} | {f(g('steady_pss_mib'))} | {f(g('open_idle_cpu_pct'))} / {f(g('steady_idle_cpu_pct'))} | {r[0].get('procs')} |")
        else:
            out.append(f"| {t} | {r} |")
for test, title in (("latency", "Keypress to framebuffer change, idle (ms)"), ("latency_load", "Keypress latency under cat load (ms)")):
    if test in d:
        out += ["", f"### {title}", "| terminal | n | min | median | p90 | p99 | max | timeouts |", "|---|---|---|---|---|---|---|---|"]
        for t, r in rows(test).items():
            if isinstance(r, dict) and "median" in r:
                out.append(f"| {t} | {r['n']} | {f(r['min'])} | {f(r['median'])} | {f(r['p90'])} | {f(r['p99'])} | {f(r['max'])} | {r['timeouts']} |")
            else:
                out.append(f"| {t} | {r} |")
if "cat" in d:
    out += ["", "### cat 100 MiB (s, median of runs)", "| terminal | cat returns | rendered (cat + drain to idle) | MiB/s rendered | terminal CPU s | runs |", "|---|---|---|---|---|---|"]
    for t, r in rows("cat").items():
        if ok(r):
            rs = [x["rendered_s"] for x in r if x.get("rendered_s")]
            out.append(f"| {t} | {f(med([x['cat_s'] for x in r]),2)} | {f(med(rs),2)} | {f(100/med(rs)) if rs else 'n/a'} | {f(med([x['term_cpu_s'] for x in r]),2)} | {len(r)} |")
        else:
            out.append(f"| {t} | {r} |")
if "vtebench" in d:
    benches = sorted({b for r in d["vtebench"].values() if isinstance(r, dict) and "error" not in r for b in r})
    out += ["", "### vtebench (mean ms per sample, lower is better)", "| bench | " + " | ".join(T) + " |", "|---" * (len(T) + 1) + "|"]
    for b in benches:
        out.append(f"| {b} | " + " | ".join(f(d['vtebench'].get(t, {}).get(b, {}).get('mean_ms'), 2) if isinstance(d['vtebench'].get(t), dict) else "err" for t in T) + " |")
print("\n".join(out))
