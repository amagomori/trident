#!/usr/bin/env python3
"""KAN-89 merge number: release-binary PTY A/B of head vs main with an A/A noise bound.

Runs .github/scripts/latency.py --app (input through write_input -> PTY `cat` echo ->
frame) on main, a byte-identical copy of main, and head, in a freshly shuffled order every
round. Reports the median of per-round paired deltas (head-main, copy-main) with a 95%
bootstrap CI. Linux (X11 :0, nested Weston, --xvfb smoke) and macOS.

  python3 .github/scripts/latency-qa/ab.py \
      --main ~/kan89/t-main/release/terminal --head ~/kan89/t-head/release/terminal \
      --rounds 8 --out ~/kan89/ab-x11
"""
import argparse, hashlib, json, os, random, shutil, statistics, subprocess, sys, tempfile
from pathlib import Path

METRICS = {"echo": [("p50_ms", "lower"), ("p99_ms", "lower"), ("wasted", "lower")],
           "load": [("fps", "higher"), ("bytes_s", "higher")]}


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def boot_ci(values, n=10000, seed=89):
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(n))
    return statistics.median(values), meds[int(.025 * n)], meds[int(.975 * n) - 1]


def run(args, label, binary, workload, rnd, cfg_home):
    out = args.out / f"r{rnd:02d}-{label}-{workload}.json"
    cmd = [sys.executable, str(args.latency), "--app", str(binary), "--runs", "1",
           "--probes", str(args.probes), "--duration", str(args.duration),
           "--workload", workload, "--json", str(out)] + (["--xvfb"] if args.xvfb else [])
    env = dict(os.environ, XDG_CONFIG_HOME=str(cfg_home))
    p = subprocess.run(cmd, env=env, capture_output=True, text=True)
    (args.out / f"r{rnd:02d}-{label}-{workload}.log").write_text(p.stdout + p.stderr)
    if p.returncode or not out.exists():
        sys.exit(f"{label} {workload} round {rnd} failed; see {out.with_suffix('.log')}")
    return json.loads(out.read_text())[0]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--latency", type=Path, default=Path(__file__).resolve().parent.parent / "latency.py",
                    help="latency.py to drive (default: this checkout's .github/scripts/latency.py)")
    ap.add_argument("--main", type=Path, required=True, help="main release binary")
    ap.add_argument("--head", type=Path, required=True, help="head release binary")
    ap.add_argument("--rounds", type=int, default=8)
    ap.add_argument("--probes", type=int, default=100)
    ap.add_argument("--duration", type=int, default=10)
    ap.add_argument("--workload", choices=["echo", "load", "both"], default="both")
    ap.add_argument("--xvfb", action="store_true", help="smoke only; not the merge number")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    copy = args.out / "terminal-main-copy"
    shutil.copy2(args.main, copy)
    assert sha(copy) == sha(args.main)
    bins = {"main": args.main.resolve(), "copy": copy.resolve(), "head": args.head.resolve()}
    print("sha256", {k: sha(v)[:12] for k, v in bins.items()}, file=sys.stderr)
    cfg_home = Path(tempfile.mkdtemp(prefix="kan89-cfg-"))  # empty config: defaults for every build
    workloads = ["echo", "load"] if args.workload == "both" else [args.workload]
    rows = []
    for rnd in range(1, args.rounds + 1):
        for workload in workloads:
            order = list(bins)
            random.shuffle(order)
            for label in order:
                r = run(args, label, bins[label], workload, rnd, cfg_home)
                rows.append(dict(round=rnd, label=label, order="".join(o[0] for o in order), **r))
                print(f"round {rnd} {workload} {label}: p50 {r['p50_ms']} p99 {r['p99_ms']} "
                      f"fps {r['fps']:.1f} wasted {r['wasted']} [{r['timing']}]", file=sys.stderr)
    timings = {r["timing"] for r in rows}
    summary = {"timing": sorted(timings), "rounds": args.rounds, "metrics": {}}
    lines = [f"timing source: {', '.join(sorted(timings))}" + (" (MIXED: rerun)" if len(timings) > 1 else ""),
             "| workload | metric | main median | copy median | head median | A/A copy-main [95% CI] | head-main [95% CI] | verdict |",
             "| --- | --- | ---: | ---: | ---: | --- | --- | --- |"]
    for workload in workloads:
        for key, better in METRICS[workload]:
            by = {(r["round"], r["label"]): r[key] for r in rows if r["workload"] == workload and r[key] is not None}
            rs = [n for n in range(1, args.rounds + 1) if all((n, l) in by for l in bins)]
            if len(rs) < 3:
                continue
            aa = boot_ci([by[n, "copy"] - by[n, "main"] for n in rs])
            ab = boot_ci([by[n, "head"] - by[n, "main"] for n in rs])
            noise = max(abs(aa[1]), abs(aa[2]))
            worse = ab[1] > noise if better == "lower" else ab[2] < -noise
            better_ = ab[2] < -noise if better == "lower" else ab[1] > noise
            verdict = "REGRESSION" if worse else "improved" if better_ else "within noise"
            med = {l: statistics.median(by[n, l] for n in rs) for l in bins}
            summary["metrics"][f"{workload}.{key}"] = dict(medians=med, aa=aa, ab=ab, noise=noise, verdict=verdict)
            f = lambda c: f"{c[0]:+.2f} [{c[1]:+.2f}, {c[2]:+.2f}]"
            lines.append(f"| {workload} | {key} | {med['main']:.2f} | {med['copy']:.2f} | {med['head']:.2f} | "
                         f"{f(aa)} | {f(ab)} | {verdict} |")
    (args.out / "rows.json").write_text(json.dumps(rows, indent=1))
    (args.out / "summary.json").write_text(json.dumps(summary, indent=1))
    print("\n".join(lines))
    return 1 if any(m["verdict"] == "REGRESSION" for m in summary["metrics"].values()) else 0


if __name__ == "__main__":
    sys.exit(main())
