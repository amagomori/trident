"""Memory (RSS) and idle CPU of the real `terminal` app, compared base vs head by bench.py.

Each run opens the app on a private Xvfb display with software GL (llvmpipe), so it needs
Linux, Xvfb and Mesa but no GPU. The shell inside waits on a FIFO, which gives two phases:

1. window open, no output: RSS and idle CPU
2. after a burst that fills scrollback: peak RSS, RSS once settled, and idle CPU again

Every metric is the median over interleaved base/head rounds.
"""

import json
import os
import shutil
import signal
import subprocess
import tempfile
import time
from pathlib import Path

TICK = os.sysconf("SC_CLK_TCK")
IDLE_SECS = 5.0
# 50,000 lines of 150 columns wrap to 100,000 rows: well past the 10,000-line scrollback.
SHELL = 'cat "$0" >/dev/null; seq -f "%0150g" 1 50000; exec sleep 3600'

METRICS = [
    # key, label, kind
    ("open_rss_kib", "RSS, window open", "mem"),
    ("peak_rss_kib", "Peak RSS, scrollback burst", "mem"),
    ("steady_rss_kib", "RSS after burst", "mem"),
    ("open_idle_cpu", "Idle CPU, window open", "cpu"),
    ("steady_idle_cpu", "Idle CPU after burst", "cpu"),
]


def build_app(checkout: Path, out_dir: Path, label: str, env: dict[str, str] | None) -> Path:
    result = subprocess.run(
        ["cargo", "build", "--release", "--workspace", "--bins", "--message-format=json"],
        cwd=checkout,
        env=env,
        stdout=subprocess.PIPE,
        check=True,
        text=True,
    )
    for line in result.stdout.splitlines():
        msg = json.loads(line)
        if msg.get("reason") == "compiler-artifact" and msg["target"]["name"] in ("trident", "terminal"):
            copy = out_dir / f"{label}-terminal"
            shutil.copy2(msg["executable"], copy)
            return copy
    raise RuntimeError(f"no `trident` binary built in {checkout}")


def cpu_secs(pid: int) -> float:
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / TICK  # utime + stime, all threads


def status(pid: int, key: str) -> int:
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(key + ":"):
            return int(line.split()[1])
    raise RuntimeError(f"no {key} for pid {pid}")


def settle(proc: subprocess.Popen, timeout: float = 30.0) -> None:
    """Wait until the app stops using CPU (window drawn, output parsed). An app that never
    goes idle is measured anyway after `timeout`, so it shows up as idle CPU."""
    start = time.monotonic()
    prev = cpu_secs(proc.pid)
    while time.monotonic() - start < timeout:
        time.sleep(0.5)
        if proc.poll() is not None:
            raise RuntimeError(f"terminal exited with {proc.returncode}")
        now = cpu_secs(proc.pid)
        if now - prev < 2 / TICK and time.monotonic() - start > 1.5:
            time.sleep(1.0)
            return
        prev = now


def idle_cpu(pid: int) -> float:
    """Percent of one core used over IDLE_SECS with nothing happening."""
    c0, t0 = cpu_secs(pid), time.monotonic()
    time.sleep(IDLE_SECS)
    return (cpu_secs(pid) - c0) / (time.monotonic() - t0) * 100


def run_once(exe: Path, env: dict[str, str], tmp: Path) -> dict:
    fifo = tmp / "go"
    fifo.unlink(missing_ok=True)
    os.mkfifo(fifo)
    proc = subprocess.Popen(
        [str(exe), "-e", "sh", "-c", SHELL, str(fifo)],
        env=env,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    try:
        settle(proc)
        out = {"open_rss_kib": status(proc.pid, "VmRSS"), "open_idle_cpu": idle_cpu(proc.pid)}
        with open(fifo, "w"):
            pass
        settle(proc)
        out["steady_idle_cpu"] = idle_cpu(proc.pid)
        out["steady_rss_kib"] = status(proc.pid, "VmRSS")
        out["peak_rss_kib"] = status(proc.pid, "VmHWM")
        return out
    finally:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()


def start_xvfb() -> tuple[subprocess.Popen, str]:
    read, write = os.pipe()
    xvfb = subprocess.Popen(
        ["Xvfb", "-displayfd", str(write), "-screen", "0", "1600x1200x24", "-nolisten", "tcp"],
        pass_fds=[write],
        stderr=subprocess.DEVNULL,
    )
    os.close(write)
    with os.fdopen(read) as f:
        display = f.readline().strip()
    if not display:
        raise RuntimeError("Xvfb did not start")
    return xvfb, f":{display}"


def measure_app(binaries: dict[str, Path], rounds: int) -> dict:
    xvfb, display = start_xvfb()
    env = {k: v for k, v in os.environ.items() if k != "WAYLAND_DISPLAY"}
    env |= {"DISPLAY": display, "LIBGL_ALWAYS_SOFTWARE": "1", "WGPU_BACKEND": "gl"}
    samples = {label: [] for label in binaries}
    try:
        with tempfile.TemporaryDirectory() as tmp:
            for _ in range(rounds):
                for label, exe in binaries.items():
                    samples[label].append(run_once(exe, env, Path(tmp)))
    finally:
        xvfb.kill()
        xvfb.wait()
    return {
        label: {key: sorted(r[key] for r in runs)[len(runs) // 2] for key, _, _ in METRICS}
        for label, runs in samples.items()
    }


def fmt(value: float, kind: str) -> str:
    return f"{value / 1024:,.1f} MiB" if kind == "mem" else f"{value:.2f}%"


def report_app(
    results: dict, mem_threshold: float, cpu_threshold: float
) -> tuple[list[str], list[str]]:
    head, base = results["head"], results.get("base")
    lines = ["### Memory and idle CPU", ""]
    if base is None:
        lines += ["| Metric | Head |", "| --- | ---: |"]
        lines += [f"| {label} | {fmt(head[key], kind)} |" for key, label, kind in METRICS]
        return lines + [""], []

    lines += [
        (
            f"The `terminal` app on Xvfb with software GL. Fails when RSS grows more than "
            f"{mem_threshold:g}% or idle CPU rises more than {cpu_threshold:g} points of a core."
        ),
        "",
        "| Metric | Base → head | Change | |",
        "| --- | ---: | ---: | --- |",
    ]
    regressions = []
    for key, label, kind in METRICS:
        b, h = base[key], head[key]
        if kind == "mem":
            change = (h - b) / b * 100
            delta = f"{change:+.2f}%"
            status = "regressed" if change > mem_threshold else "ok"
        else:
            change = h - b
            delta = f"{change:+.2f} pts"
            status = "regressed" if change > cpu_threshold else "ok"
        if status == "regressed":
            regressions.append(label)
        lines.append(f"| {label} | {fmt(b, kind)} → {fmt(h, kind)} | {delta} | {status} |")
    return lines + [""], regressions
