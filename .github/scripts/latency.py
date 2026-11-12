"""Real-display latency/throughput comparison (Python 3 stdlib, Linux/macOS).

Run from a desktop session with cargo and the app's build dependencies installed:
  python3 .github/scripts/latency.py --app target/release/terminal --runs 5
  python3 .github/scripts/latency.py --base ../baseline --head . --json results.json
An uninstrumented baseline is skipped with a diagnostic; head must produce data.
Each checkout gets its own target directory.
Linux needs DISPLAY or WAYLAND_DISPLAY; macOS needs a logged-in GUI session.
--xvfb needs xvfb-run and is only a smoke test, not a real-display measurement.
Keep display refresh, window size, power mode and background load fixed. Runs are
interleaved; compare the reported min-max spread, not just a single percentile.
X11 Present, Wayland wp_presentation, or macOS Metal presentedTime timestamps are
used when available; present-return latency is reported separately and fallback
is labeled. A headless compositor is a protocol smoke test only; llvmpipe X11
may provide no completion feedback even when Present is advertised.
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
    frames, inputs, echoes, presented, refresh, source = {}, [], [], {}, 0, None
    for line in path.read_text().splitlines():
        fields = line.split()
        if fields[0] == "refresh_mhz":
            refresh = int(fields[1])
        elif fields[0] == "frame":
            seq, time, count = map(int, fields[1:])
            frames[seq] = (time, count)
        elif fields[0] == "input":
            inputs.append(int(fields[1]))
        elif fields[0] == "echo":
            echoes.append(tuple(map(int, fields[1:])))
        elif fields[0] == "presented":
            presented[int(fields[1])] = (int(fields[2]), fields[3])
        elif fields[0] == "source":
            source = fields[1]
    if len(frames) < 2 or (workload == "echo" and not echoes):
        raise RuntimeError("no usable samples: app must contain TERM_LATENCY_LOG instrumentation")
    feedback = bool(presented)
    successful = {seq: (time, frames[seq][1]) for seq, (time, mode) in presented.items()
                  if seq in frames and mode != "skip"} if feedback else frames
    ordered = [successful[seq] for seq in sorted(successful)]
    seconds = max(f[0] for f in [*frames.values(), *successful.values()]) / 1e6
    returns = [frames[seq][0] - time for time, seq in echoes] if workload == "echo" else []
    latencies = [successful[seq][0] - time for time, seq in echoes if seq in successful] if workload == "echo" else []
    if feedback and any(p[0] < 0 for p in presented.values()):
        raise RuntimeError("presentation clock precedes log epoch; check X11 clock mapping")
    timing = f"presented: {source or 'unknown'}" if feedback else "present-return (no feedback)"
    verified = feedback and source == "wayland-presentation"
    if source in ("metal-presented", "x11-present"):
        timing += " (unverified)"
    wasted = len(frames) - len(successful) if feedback else 0
    previous = 0
    for seq, (time, count) in sorted(frames.items()):
        if seq in successful and count == 0 and not any(previous < t <= time for t in inputs):
            wasted += 1
        previous = time
    return dict(p50_ms=percentile(latencies, .5) if latencies else None,
                p99_ms=percentile(latencies, .99) if latencies else None,
                mean_ms=statistics.mean(latencies) / 1000 if latencies else None,
                return_p50_ms=percentile(returns, .5) if returns else None,
                return_p99_ms=percentile(returns, .99) if returns else None,
                timing=timing, samples=len(latencies), frames=len(successful), fps=len(successful)/seconds,
                wasted=wasted,
                late=sum(a[1] > 0 and b[1] > 0 and b[0]-a[0] > 1.5e9/refresh
                         for a, b in zip(ordered, ordered[1:])) if refresh else None,
                bytes_s=sum(f[1] for f in frames.values())/seconds, refresh_mhz=refresh,
                verified=verified)


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
    unavailable = set()
    with tempfile.TemporaryDirectory(prefix="terminal-latency-") as temporary:
        tmp = Path(temporary)
        apps = {"app": args.app.resolve()} if args.app else {
            label: build(checkout.resolve(), checkout.resolve() / "target" / "latency")
            for label, checkout in ([('base', args.base)] if args.base else []) + [('head', args.head)]}
        workloads = ["echo", "load"] if args.workload == "both" else [args.workload]
        for workload in workloads:
            for run in range(1, args.runs + 1):
                for label, app in apps.items():
                    if label in unavailable:
                        continue
                    log = tmp / "samples.log"
                    log.unlink(missing_ok=True)
                    env = dict(os.environ, TERM_LATENCY_LOG=str(log),
                               TERM_LATENCY_PROBE=str(args.probes if workload == "echo" else 0),
                               TERM_LATENCY_SECONDS=str(args.duration if workload == "load" else 0))
                    command = [str(app), "-e"] + (["cat"] if workload == "echo" else
                        ["sh", "-c", "while :; do printf '%0150d\\n' 1; done"])
                    if args.xvfb:
                        env.pop("WAYLAND_DISPLAY", None)
                        env.pop("WAYLAND_SOCKET", None)
                        command = ["xvfb-run", "-a", *command]
                    try:
                        subprocess.run(command, env=env, check=True, stdout=subprocess.DEVNULL,
                                       timeout=30 + (args.probes * .2 if workload == "echo" else args.duration))
                        result = summarize(log, workload)
                    except (subprocess.TimeoutExpired, subprocess.CalledProcessError, OSError, RuntimeError) as error:
                        reason = "no latency log; checkout may lack latency instrumentation" if not log.exists() else str(error)
                        print(f"{label}: no latency data ({reason}); skipping", file=sys.stderr)
                        unavailable.add(label)
                        continue
                    results.append(dict(app=label, workload=workload, run=run, **result))
    print("| app | workload | run | p50 ms | p99 ms | mean ms | return p50 ms | return p99 ms | timing | samples | frames | fps | wasted | late | bytes/s | refresh mHz |")
    print("|" + " --- |" * 16)
    for row in results:
        print("| " + " | ".join("—" if v is None else f"{v:.2f}" if isinstance(v, float) else str(v) for k, v in row.items() if k != 'verified') + " |")
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
    return 0 if any(row['app'] != 'base' for row in results) else 1


if __name__ == "__main__":
    sys.exit(main())
