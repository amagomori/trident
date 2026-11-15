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


def require_executable(ap, flag, path):
    if not path.is_file() or not os.access(path, os.X_OK):
        what = "does not exist" if not path.exists() else "is not an executable file"
        ap.error(f"{flag} {path} {what}")


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
    r = json.loads(out.read_text())[0]
    if r["frames"] == 0 and r["timing"].startswith("presented"):
        # Every presentation callback was a skip (the macOS VM's paravirtual Metal reports
        # presentedTime 0), so latency.py has no presented frames: fall back to present-return.
        source = r["timing"].split(": ", 1)[1].split(" ")[0]
        r.update(p50_ms=r["return_p50_ms"], p99_ms=r["return_p99_ms"], fps=None, wasted=None,
                 timing=f"present-return ({source} gave no timestamps)")
    return r


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
    if args.rounds < 3:
        ap.error("--rounds must be at least 3 for paired-round CIs")
    require_executable(ap, "--main", args.main)
    require_executable(ap, "--head", args.head)
    if not args.latency.is_file():
        ap.error(f"--latency {args.latency} does not exist")
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "summary.json").unlink(missing_ok=True)
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
                fps = "n/a" if r["fps"] is None else f"{r['fps']:.1f}"
                print(f"round {rnd} {workload} {label}: p50 {r['p50_ms']} p99 {r['p99_ms']} "
                      f"fps {fps} wasted {r['wasted']} [{r['timing']}]", file=sys.stderr)
    (args.out / "rows.json").write_text(json.dumps(rows, indent=1))
    timings = {w: sorted({r["timing"] for r in rows if r["workload"] == w}) for w in workloads}
    mixed = {w: t for w, t in timings.items() if len(t) > 1}
    if mixed:
        # Deltas are paired within a workload, so each workload must use one timing source.
        print("ERROR: mixed timing sources within a workload ("
              + "; ".join(f"{w}: {', '.join(t)}" for w, t in mixed.items())
              + "); deltas would compare different endpoints, so no verdict. Rerun; raw runs are in "
              + str(args.out / "rows.json"), file=sys.stderr)
        return 2
    summary = {"timing": {w: t[0] for w, t in timings.items()}, "rounds": args.rounds, "metrics": {}}
    lines = ["timing source: " + "; ".join(f"{w} {t[0]}" for w, t in timings.items()),
             "| workload | metric | main median | copy median | head median | A/A copy-main [95% CI] | head-main [95% CI] | verdict |",
             "| --- | --- | ---: | ---: | ---: | --- | --- | --- |"]
    for workload in workloads:
        for key, better in METRICS[workload]:
            by = {(r["round"], r["label"]): r[key] for r in rows if r["workload"] == workload and r[key] is not None}
            rs = [n for n in range(1, args.rounds + 1) if all((n, l) in by for l in bins)]
            if len(rs) < 3:
                lines.append(f"| {workload} | {key} | n/a | n/a | n/a | | | no data ({len(rs)} complete rounds) |")
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
    (args.out / "summary.json").write_text(json.dumps(summary, indent=1))
    print("\n".join(lines))
    missing = [w for w, k in (("echo", "echo.p50_ms"), ("load", "load.bytes_s"))
               if w in workloads and k not in summary["metrics"]]
    if missing:
        print(f"ERROR: no {', '.join(missing)} result for at least 3 complete rounds", file=sys.stderr)
        return 2
    return 1 if any(m["verdict"] == "REGRESSION" for m in summary["metrics"].values()) else 0


if __name__ == "__main__":
    sys.exit(main())
