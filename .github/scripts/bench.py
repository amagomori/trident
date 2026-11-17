#!/usr/bin/env python3
"""Run the terminal-state benchmarks and compare a change against a baseline checkout.

    bench.py --head .                      # numbers for this checkout
    bench.py --base ../main --head .       # compare, exit 1 on a regression

The gate is the instruction count of each workload's measured region under callgrind,
which avoids wall-clock scheduling noise on CI runners. Wall-clock medians are
reported alongside but never fail the check. The workloads are the `bench` module in
`crates/term-core/src/tests.rs`, run as the ignored
`bench_workloads` test.

The same check measures the real Trident app (see `app_bench.py`, Linux with Xvfb and
Mesa): RSS with the window open, peak and settled RSS after a scrollback burst, and idle
CPU in both states. It fails when RSS grows more than `--mem-threshold` percent or idle
CPU rises more than `--cpu-threshold` points of one core; `--no-app` skips it.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True
from app_bench import build_app, measure_app, report_app

TEST = "bench_workloads"


def cargo_env(label: str) -> dict[str, str] | None:
    if "CARGO_TARGET_DIR" in os.environ:
        # Both checkouts are the same package, so Cargo can hand back the other
        # checkout's binary as fresh. Give each its own target dir.
        return {**os.environ, "CARGO_TARGET_DIR": f"{os.environ['CARGO_TARGET_DIR']}/{label}"}
    return None


def build(checkout: Path, out_dir: Path, label: str) -> tuple[Path, str] | None:
    """Build the test binary that holds the benchmarks; None if the checkout has none."""
    result = subprocess.run(
        ["cargo", "test", "--release", "--workspace", "--no-run", "--message-format=json"],
        cwd=checkout,
        stdout=subprocess.PIPE,
        check=True,
        text=True,
        env=cargo_env(label),
    )
    for line in result.stdout.splitlines():
        msg = json.loads(line)
        exe = msg.get("executable")
        if msg.get("reason") != "compiler-artifact" or not exe or not msg["profile"]["test"]:
            continue
        listed = subprocess.run(
            [exe, "--list", "--ignored"], stdout=subprocess.PIPE, check=True, text=True
        ).stdout
        for entry in listed.splitlines():
            name = entry.removesuffix(": test")
            if name != entry and name.split("::")[-1] == TEST:
                # With a shared CARGO_TARGET_DIR the next build would overwrite it.
                copy = out_dir / f"{label}-{Path(exe).name}"
                shutil.copy2(exe, copy)
                return copy, name
    return None


def run(exe: Path, test: str, env: dict[str, str], wrapper: list[str] = ()) -> list[dict]:
    out = subprocess.run(
        [*wrapper, str(exe), "--ignored", "--exact", test, "--nocapture", "--test-threads=1"],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL if wrapper else None,
        check=True,
        text=True,
        env={**os.environ, **env},
    ).stdout
    # libtest prints "test <name> ... " on the same line as the first output.
    marker = "BENCH {"
    return [json.loads(line[line.index(marker) + 6 :]) for line in out.splitlines() if marker in line]


def instructions(exe: Path, test: str, name: str, tmp: Path) -> int:
    out_file = tmp / f"callgrind.{exe.name}.{name}"
    wrapper = [
        "valgrind",
        "--tool=callgrind",
        # Matches both the demangled and the v0-mangled (`5bench16measured_advance`) names.
        "--toggle-collect=*bench*measured_*",
        f"--callgrind-out-file={out_file}",
    ]
    run(exe, test, {"TERM_BENCH": name, "TERM_BENCH_ITERS": "1"}, wrapper)
    for line in out_file.read_text().splitlines():
        if line.startswith(("summary:", "totals:")):
            return int(line.split()[1])
    raise RuntimeError(f"no instruction total in {out_file}")


def measure(binaries: dict[str, tuple[Path, str]], rounds: int, tmp: Path) -> dict:
    results = {label: {} for label in binaries}
    # Alternate base and head each round so drift on the runner hits both equally.
    for _ in range(rounds):
        for label, (exe, test) in binaries.items():
            for r in run(exe, test, {"TERM_BENCH_ITERS": "3"}):
                entry = results[label].setdefault(r["name"], {**r, "secs": []})
                entry["secs"].append(r["secs"])
    for label, (exe, test) in binaries.items():
        for name, entry in results[label].items():
            entry["secs"] = sorted(entry["secs"])[len(entry["secs"]) // 2]
            entry["instructions"] = instructions(exe, test, name, tmp)
    return results


def rate(entry: dict) -> str:
    per_sec = entry["count"] / entry["secs"]
    if entry["unit"] == "bytes":
        return f"{per_sec / 2**20:,.1f} MiB/s"
    unit = "key" if entry["unit"] == "keystrokes" else entry["unit"].rstrip("s")
    return f"{entry['secs'] / entry['count'] * 1e6:,.2f} µs/{unit}"


def pct(new: float, old: float) -> str:
    return f"{(new - old) / old * 100:+.2f}%"


def wall_change(base: dict, head: dict) -> str:
    """Throughput change for byte workloads, latency change for keystrokes."""
    if base["unit"] == "bytes":
        return f"{pct(base['secs'], head['secs'])} speed"
    return f"{pct(head['secs'], base['secs'])} latency"


def report(results: dict, threshold: float) -> tuple[str, list[str]]:
    head, base = results["head"], results.get("base")
    lines = ["## Benchmarks", ""]
    regressions = []
    if base is None:
        lines += [
            "No baseline: the base branch has no `bench_workloads` yet, so nothing to compare.",
            "",
            "| Workload | Instructions | Wall clock |",
            "| --- | ---: | ---: |",
        ]
        for name, h in head.items():
            lines.append(f"| `{name}` | {h['instructions']:,} | {rate(h)} |")
        return "\n".join(lines) + "\n", regressions

    lines += [
        f"Fails when a workload runs more than {threshold:g}% more instructions per byte (or "
        "keystroke) than the base branch, or is removed or changes unit. Wall clock is shared-runner time, for "
        "reference only.",
        "",
        "| Workload | Instructions (base → head) | Change | Wall clock (base → head) | Change | |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for name, h in head.items():
        b = base.get(name)
        if b is None:
            lines.append(f"| `{name}` | new: {h['instructions']:,} | | {rate(h)} | | new |")
            continue
        if b["unit"] != h["unit"]:
            # Per-byte and per-keystroke counts don't compare, so this can't be gated as is.
            regressions.append(name)
            lines.append(
                f"| `{name}` | {b['instructions']:,} → {h['instructions']:,} | | | "
                f"| unit changed ({b['unit']} → {h['unit']}) |"
            )
            continue
        # Per byte or keystroke, so resizing a workload's input doesn't read as a change.
        b_per, h_per = b["instructions"] / b["count"], h["instructions"] / h["count"]
        change = (h_per - b_per) / b_per * 100
        status = "regressed" if change > threshold else "faster" if change < -threshold else "ok"
        if status == "regressed":
            regressions.append(name)
        per = "" if b["count"] == h["count"] else f" (per {b['unit'].rstrip('s')})"
        lines.append(
            f"| `{name}` | {b['instructions']:,} → {h['instructions']:,} | "
            f"{pct(h_per, b_per)}{per} | {rate(b)} → {rate(h)} | "
            f"{wall_change(b, h)} | {status} |"
        )
    for name in base.keys() - head.keys():
        # Dropping a workload must not be a way past the gate.
        regressions.append(name)
        lines.append(f"| `{name}` | {base[name]['instructions']:,} → removed | | | | removed |")
    lines.append("")
    if regressions:
        lines.append(f"**Regressed, removed or changed unit:** {', '.join(f'`{n}`' for n in regressions)}")
    else:
        lines.append("No regressions.")
    return "\n".join(lines) + "\n", regressions


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--head", type=Path, required=True, help="checkout being tested")
    parser.add_argument("--base", type=Path, help="baseline checkout, e.g. main")
    parser.add_argument("--threshold", type=float, default=3.0, help="max %% instruction increase")
    parser.add_argument("--rounds", type=int, default=5, help="wall-clock rounds per checkout")
    parser.add_argument("--mem-threshold", type=float, default=5.0, help="max %% RSS increase")
    parser.add_argument(
        "--cpu-threshold", type=float, default=1.0, help="max idle CPU increase, %% of a core"
    )
    parser.add_argument("--app-rounds", type=int, default=3, help="app runs per checkout")
    parser.add_argument("--no-app", action="store_true", help="skip memory and idle CPU")
    parser.add_argument("--summary", type=Path, help="append the markdown report here")
    parser.add_argument("--json", type=Path, help="write raw results here")
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        binaries = {}
        if args.base:
            base = build(args.base.resolve(), tmp, "base")
            if base:
                binaries["base"] = base
        head = build(args.head.resolve(), tmp, "head")
        if head is None:
            print(f"error: no `{TEST}` test in {args.head}", file=sys.stderr)
            return 2
        binaries["head"] = head
        results = measure(binaries, args.rounds, tmp)

        if not args.no_app:
            apps = {}
            if args.base:
                apps["base"] = build_app(args.base.resolve(), tmp, "base", cargo_env("base"))
            apps["head"] = build_app(args.head.resolve(), tmp, "head", cargo_env("head"))
            results["app"] = measure_app(apps, args.app_rounds)

    text, regressions = report(results, args.threshold)
    if "app" in results:
        app_lines, app_regressions = report_app(
            results["app"], args.mem_threshold, args.cpu_threshold
        )
        text += "\n" + "\n".join(app_lines)
        regressions += app_regressions
    print(text)
    if args.summary:
        with args.summary.open("a") as f:
            f.write(text)
    if args.json:
        args.json.write_text(json.dumps(results, indent=2))
    return 1 if regressions else 0


if __name__ == "__main__":
    sys.exit(main())
