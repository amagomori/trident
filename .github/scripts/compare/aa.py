#!/usr/bin/env python3
"""Tabulate Trident versus each competitor with two-round A/A bounds.

A/A bound of a terminal on a metric = |round A - round B| (shown as % of its mean).
'Trails beyond noise' = ours worse than theirs in both rounds AND the pooled gap exceeds
max(our A/A, their A/A, floor). All metrics lower-is-better. vtebench uses mean ms per sample (its medians are whole ms)."""
import json, statistics as st, sys
from pathlib import Path
D = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parents[3] / "target/compare/rounds")
A, B = (json.loads((D / f"round{r}.json").read_text()) for r in "AB")
T = ["terminal", "alacritty", "kitty", "wezterm", "ghostty"]
med = lambda xs: st.median(xs) if xs else None

def lst(k):
    return lambda r: med([x[k] for x in r if x.get(k) is not None]) if isinstance(r, list) else None
def dct(k):
    return lambda r: r.get(k) if isinstance(r, dict) and r.get("n") else None
M = {"startup": [("exec_ms", lst("exec_ms"), 0), ("mapped_ms", lst("mapped_ms"), 0), ("first_paint_ms", lst("painted_ms"), 0)],
     "memory": [("open_rss_mib", lst("open_rss_mib"), 0), ("peak_rss_mib", lst("peak_rss_mib"), 0), ("steady_rss_mib", lst("steady_rss_mib"), 0),
                ("open_idle_cpu_pct", lst("open_idle_cpu_pct"), 1.0), ("steady_idle_cpu_pct", lst("steady_idle_cpu_pct"), 1.0)],
     "latency": [("median_ms", dct("median"), 0), ("p90_ms", dct("p90"), 0)],
     "latency_load": [("median_ms", dct("median"), 0), ("p90_ms", dct("p90"), 0)],
     "cat": [("cat_s", lst("cat_s"), 0), ("rendered_s", lst("rendered_s"), 0), ("term_cpu_s", lst("term_cpu_s"), 0)]}
benches = sorted(set(A.get("vtebench", {}).get("terminal", {}) or {}) | set(B.get("vtebench", {}).get("terminal", {}) or {}))
M["vtebench"] = [(b, (lambda b: lambda r: r.get(b, {}).get("mean_ms") if isinstance(r, dict) else None)(b), 0) for b in benches if b != "error"]

def vals(test, t, fn):
    a, b = fn(A.get(test, {}).get(t, {})), fn(B.get(test, {}).get(t, {}))
    return (a, b) if a is not None and b is not None else None

f = lambda v, p=2: "n/a" if v is None else f"{v:.{p}f}"
out, trails, skipped = [], [], {}
for test, metrics in M.items():
    if test not in A:
        continue
    out += ["", f"### {test}", "| metric | terminal (A/A) | " + " | ".join(f"{c} (A/A) / gap" for c in T[1:]) + " |", "|---" * 5 + "|---|"]
    for name, fn, floor in metrics:
        us = vals(test, "terminal", fn)
        if not us:
            out.append(f"| {name} | n/a |"); continue
        um, uaa = st.fmean(us), abs(us[0] - us[1])
        row = [name, f"{f(um)} (±{f(100*uaa/um if um else 0,0)}%)"]
        for c in T[1:]:
            th = vals(test, c, fn)
            if not th:
                err = A.get(test, {}).get(c) or B.get(test, {}).get(c)
                skipped.setdefault(c, set()).add(test)
                row.append("n/a"); continue
            tm, taa = st.fmean(th), abs(th[0] - th[1])
            gap = (um / tm - 1) * 100 if tm else float("nan")
            beyond = us[0] > th[0] and us[1] > th[1] and (um - tm) > max(uaa, taa, floor)
            mark = " **TRAIL**" if beyond else ""
            row.append(f"{f(tm)} (±{f(100*taa/tm if tm else 0,0)}%) / {gap:+.0f}%{mark}")
            if beyond:
                trails.append(f"- {test} {name}: terminal {f(um)} vs {c} {f(tm)} ({gap:+.0f}%; A/A ours ±{f(uaa)}, theirs ±{f(taa)}; rounds {f(us[0])}/{f(us[1])} vs {f(th[0])}/{f(th[1])})")
        out.append("| " + " | ".join(row) + " |")
print("\n".join(out))
print("\n### Trails beyond noise\n" + ("\n".join(trails) or "- none"))
print("\n### Missing data\n" + "\n".join(f"- {c}: {', '.join(sorted(s))}" for c, s in skipped.items()))
