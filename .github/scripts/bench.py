#!/usr/bin/env python3
"""Run the terminal-state benchmarks and compare a change against a baseline checkout.

    bench.py --head .                      # numbers for this checkout
    bench.py --base ../main --head .       # compare, exit 1 on a regression

The gate is the instruction count of each workload's measured region under callgrind,
which is deterministic, so it doesn't flake on noisy CI runners. Wall-clock medians are
reported alongside but never fail the check. The workloads are the `bench` module in
`src/term/tests.rs` (`term-core` once the crates are split), run as the ignored
`bench_workloads` test.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

TEST = "bench_workloads"


def build(checkout: Path, out_dir: Path, label: str) -> tuple[Path, str] | None:
    """Build the test binary that holds the benchmarks; None if the checkout has none."""
    result = subprocess.run(
        ["cargo", "test", "--release", "--workspace", "--no-run", "--message-format=json"],
        cwd=checkout,
        stdout=subprocess.PIPE,
        check=True,
        text=True,
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
                # A shared CARGO_TARGET_DIR means the next build overwrites this binary.
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
    return f"{entry['secs'] / entry['count'] * 1e6:,.2f} µs/key"


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
        f"Fails when a workload runs more than {threshold:g}% more instructions than the base "
        "branch. Wall clock is shared-runner time, for reference only.",
        "",
        "| Workload | Instructions (base → head) | Change | Wall clock (base → head) | Change | |",
        "| --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for name, h in head.items():
        b = base.get(name)
        if b is None:
            lines.append(f"| `{name}` | new: {h['instructions']:,} | | {rate(h)} | | new |")
            continue
        change = (h["instructions"] - b["instructions"]) / b["instructions"] * 100
        status = "regressed" if change > threshold else "faster" if change < -threshold else "ok"
        if status == "regressed":
            regressions.append(name)
        lines.append(
            f"| `{name}` | {b['instructions']:,} → {h['instructions']:,} | "
            f"{pct(h['instructions'], b['instructions'])} | {rate(b)} → {rate(h)} | "
            f"{wall_change(b, h)} | {status} |"
        )
    lines.append("")
    if regressions:
        lines.append(f"**Regressed:** {', '.join(f'`{n}`' for n in regressions)}")
    else:
        lines.append("No regressions.")
    return "\n".join(lines) + "\n", regressions


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--head", type=Path, required=True, help="checkout being tested")
    parser.add_argument("--base", type=Path, help="baseline checkout, e.g. main")
    parser.add_argument("--threshold", type=float, default=3.0, help="max %% instruction increase")
    parser.add_argument("--rounds", type=int, default=5, help="wall-clock rounds per checkout")
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

    text, regressions = report(results, args.threshold)
    print(text)
    if args.summary:
        with args.summary.open("a") as f:
            f.write(text)
    if args.json:
        args.json.write_text(json.dumps(results, indent=2))
    return 1 if regressions else 0


if __name__ == "__main__":
    sys.exit(main())
