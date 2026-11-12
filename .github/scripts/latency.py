"""Real-display latency/throughput comparison (Python 3 stdlib, Linux/macOS).

Run from a desktop session with cargo and the app's build dependencies installed:
  python3 .github/scripts/latency.py --app target/release/terminal --runs 5
  python3 .github/scripts/latency.py --base ../baseline --head . --json results.json
Checkouts must contain the instrumentation; first land this harness on main, then
compare scheduling branches against it. Each checkout gets its own target directory.
Linux needs DISPLAY or WAYLAND_DISPLAY; macOS needs a logged-in GUI session.
--xvfb needs xvfb-run and is only a smoke test, not a real-display measurement.
Keep display refresh, window size, power mode and background load fixed. Runs are
interleaved; compare the reported min-max spread, not just a single percentile.
The timestamp is after queue.present returns, NOT compositor presentation feedback:
GPU/compositor queuing and up to a refresh of scanout are additional, unmeasured delay.
Inputs are injected through write_input (no OS keyboard permission required).
Load runs for --duration seconds without input probes and reports throughput only.
Late intervals exceed 1.5 refresh periods and both adjacent frames parsed new bytes;
idle gaps between echo probes do not count.
"""

import argparse
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)] / 1000


def summarize(path, workload):
    frames, latencies, refresh = [], [], 0
    for line in path.read_text().splitlines():
        fields = line.split()
        if fields[0] == "refresh_mhz":
            refresh = int(fields[1])
        elif fields[0] == "frame":
            frames.append(tuple(map(int, fields[1:])))
        elif fields[0] == "latency":
            latencies.append(int(fields[1]))
    if len(frames) < 2 or (workload == "echo" and not latencies):
        raise RuntimeError("no usable samples: app must contain TERM_LATENCY_LOG instrumentation")
    seconds = (frames[-1][0] - frames[0][0]) / 1e6
    if workload == "load":
        latencies = []
    return dict(p50_ms=percentile(latencies, .5) if latencies else None,
                p99_ms=percentile(latencies, .99) if latencies else None,
                mean_ms=statistics.mean(latencies) / 1000 if latencies else None,
                samples=len(latencies), frames=len(frames), fps=(len(frames)-1)/seconds,
                wasted=sum(b == 0 and i == 0 for _, b, i in frames),
                late=sum(a[1] > 0 and b[1] > 0 and b[0]-a[0] > 1.5e9/refresh
                         for a, b in zip(frames, frames[1:])) if refresh else None,
                bytes_s=sum(f[1] for f in frames[1:])/seconds, refresh_mhz=refresh)


def build(checkout, target):
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    subprocess.run(["cargo", "build", "--release", "-p", "term-app"],
                   cwd=checkout, env=env, check=True, stdout=sys.stderr)
    return target / "release" / "terminal"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--app", type=Path)
    parser.add_argument("--base", type=Path)
    parser.add_argument("--head", type=Path, default=Path.cwd())
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--probes", type=int, default=100)
    parser.add_argument("--duration", type=int, default=10, help="load duration in seconds")
    parser.add_argument("--workload", choices=["echo", "load", "both"], default="both")
    parser.add_argument("--json", type=Path)
    parser.add_argument("--xvfb", action="store_true")
    args = parser.parse_args()
    if args.runs < 1 or args.probes < 1 or args.duration < 1 or (args.app and args.base):
        parser.error("runs/probes/duration must be positive; --app and --base are exclusive")
    if sys.platform != "darwin" and not args.xvfb and not any(os.environ.get(k) for k in ("DISPLAY", "WAYLAND_DISPLAY")):
        parser.error("run in a desktop session or use --xvfb for smoke testing")
    results = []
    with tempfile.TemporaryDirectory(prefix="terminal-latency-") as temporary:
        tmp = Path(temporary)
        apps = {"app": args.app.resolve()} if args.app else {
            label: build(checkout.resolve(), checkout.resolve() / "target" / "latency")
            for label, checkout in ([('base', args.base)] if args.base else []) + [('head', args.head)]}
        workloads = ["echo", "load"] if args.workload == "both" else [args.workload]
        for workload in workloads:
            for run in range(1, args.runs + 1):
                for label, app in apps.items():
                    log = tmp / "samples.log"
                    log.unlink(missing_ok=True)
                    env = dict(os.environ, TERM_LATENCY_LOG=str(log),
                               TERM_LATENCY_PROBE=str(args.probes if workload == "echo" else 0),
                               TERM_LATENCY_SECONDS=str(args.duration if workload == "load" else 0))
                    command = [str(app), "-e"] + (["cat"] if workload == "echo" else
                        ["sh", "-c", "while :; do printf '%0150d\\n' 1; done"])
                    if args.xvfb:
                        command = ["xvfb-run", "-a", *command]
                    subprocess.run(command, env=env, check=True, stdout=subprocess.DEVNULL,
                                   timeout=30 + (args.probes * .2 if workload == "echo" else args.duration))
                    results.append(dict(app=label, workload=workload, run=run, **summarize(log, workload)))
    print("| app | workload | run | p50 ms | p99 ms | mean ms | samples | frames | fps | wasted | late | bytes/s | refresh mHz |")
    print("|" + " --- |" * 13)
    for row in results:
        print("| " + " | ".join("—" if v is None else f"{v:.2f}" if isinstance(v, float) else str(v) for v in row.values()) + " |")
    for label in apps:
        for workload in workloads:
            rows = [r for r in results if r['app'] == label and r['workload'] == workload]
            spread = []
            for key in ('p50_ms', 'p99_ms'):
                values = [r[key] for r in rows if r[key] is not None]
                if values:
                    spread.append(f"{key}: {min(values):.2f}–{max(values):.2f}")
            if spread:
                print(f"{label} {workload} spread: " + ", ".join(spread))
    if args.json:
        args.json.write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
