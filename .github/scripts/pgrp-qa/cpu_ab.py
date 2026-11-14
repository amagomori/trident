#!/usr/bin/env python3
"""KAN-95 merge number: release-binary PTY A/B of app CPU and wall time under output load.

Runs main, a byte-identical copy of main (A/A) and head in a freshly shuffled order every
round. Each run opens the app with a bash script as its shell (`set -m`, so `cat` is its
own foreground job like at an interactive prompt), waits until the window is up and idle,
then times `cat FILE` up to a DA1 reply (so all output has been parsed, not only written)
and samples the app's own CPU (all threads, not the shell or cat) across that span.
Reports the median of per-round paired changes (head vs main, copy vs main) with 95%
bootstrap CIs; exit 1 on a wall-time regression beyond A/A noise. Linux and macOS.

  python3 .github/scripts/pgrp-qa/cpu_ab.py --main ../t-main/target/release/terminal \
      --head ../t-head/target/release/terminal --rounds 16 --out cpu-ab
"""
import argparse, ctypes, ctypes.util, hashlib, json, os, platform, random, shutil, signal
import statistics, subprocess, sys, tempfile, time
from pathlib import Path

MAC = platform.system() == "Darwin"
RUN_SH = r"""#!/bin/bash
set -m
cd "$1"
exec 9<>fifo
: > ready
while [ ! -e go ]; do read -t 1 -u 9 _; done
cat "$2" || : > cat-failed
stty -icanon -echo
printf '\033[c'
IFS= read -rs -d c _
: > done
while :; do read -t 60 -u 9 _; done
"""


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def boot_ci(values, n=10000, seed=95):
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(n))
    return statistics.median(values), meds[int(.025 * n)], meds[int(.975 * n) - 1]


if MAC:
    class _Timebase(ctypes.Structure):
        _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]

    _libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    _tb = _Timebase()
    _libc.mach_timebase_info(ctypes.byref(_tb))

    def cpu_secs(pid):
        # rusage_info_v0: 16-byte uuid, then ri_user_time, ri_system_time in Mach ticks.
        buf = (ctypes.c_uint64 * 32)()
        if _libc.proc_pid_rusage(pid, 0, buf) != 0:
            raise OSError(ctypes.get_errno(), f"proc_pid_rusage({pid})")
        return (buf[2] + buf[3]) * _tb.numer / _tb.denom / 1e9
else:
    TICK = os.sysconf("SC_CLK_TCK")

    def cpu_secs(pid):
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return (int(fields[11]) + int(fields[12])) / TICK  # utime + stime, all threads


def wait_for(path, timeout, proc):
    end = time.monotonic() + timeout
    while not path.exists():
        if proc.poll() is not None:
            raise RuntimeError(f"app exited ({proc.returncode}) before {path.name}")
        if time.monotonic() > end:
            raise TimeoutError(path.name)
        time.sleep(0.001)
    return time.monotonic()


def one_run(args, binary, data, env, log):
    work = Path(tempfile.mkdtemp(prefix="k95cpu-"))
    os.mkfifo(work / "fifo")
    script = work / "run.sh"
    script.write_text(RUN_SH)
    script.chmod(0o755)
    cmd = ([] if not args.taskset else ["taskset", "-c", args.taskset]) + [
        str(binary), "-e", str(script), str(work), str(data)]
    proc = subprocess.Popen(cmd, env=env, stdout=log, stderr=log)
    try:
        wait_for(work / "ready", 30, proc)
        time.sleep(args.settle)
        cpu0, t0 = cpu_secs(proc.pid), time.monotonic()
        (work / "go").touch()
        # Wake the shell's `read -t 1` now, so its poll period stays out of the timed span.
        fd = os.open(work / "fifo", os.O_WRONLY | os.O_NONBLOCK)
        os.write(fd, b"\n")
        os.close(fd)
        t1 = wait_for(work / "done", args.timeout, proc)
        cpu1 = cpu_secs(proc.pid)
        if (work / "cat-failed").exists():
            raise RuntimeError(f"cat {data} failed inside the app's shell")
        return {"wall_s": t1 - t0, "cpu_s": cpu1 - cpu0}
    finally:
        proc.send_signal(signal.SIGTERM)
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        shutil.rmtree(work, ignore_errors=True)


def make_data(path, mib):
    line = b"".join(bytes([33 + (i * 7) % 94]) for i in range(99)) + b"\n"
    with open(path, "wb") as f:
        for i in range(mib * 1024 * 1024 // len(line)):
            f.write(b"%08d " % i + line[9:])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--main", type=Path, required=True, help="main release binary")
    ap.add_argument("--head", type=Path, required=True, help="head release binary")
    ap.add_argument("--rounds", type=int, default=16)
    ap.add_argument("--mib", type=int, default=60, help="size of the cat file (default 60 MiB)")
    ap.add_argument("--settle", type=float, default=1.0, help="idle seconds before timing")
    ap.add_argument("--timeout", type=float, default=300)
    ap.add_argument("--taskset", help="Linux: pin the app to these CPUs (e.g. 2), as in PR #61")
    ap.add_argument("--xvfb", action="store_true", help="Linux smoke only; not the merge number")
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    # run.sh cds into its work dir, so the data path handed to it must be absolute.
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    copy = args.out / "main-copy" / args.main.name
    copy.parent.mkdir(exist_ok=True)
    shutil.copy2(args.main, copy)
    builds = {"main": args.main.resolve(), "copy": copy.resolve(), "head": args.head.resolve()}
    hashes = {k: sha(v) for k, v in builds.items()}
    assert hashes["main"] == hashes["copy"]
    if hashes["main"] == hashes["head"]:
        print("warning: head and main binaries are identical", file=sys.stderr)
    data = args.out / f"data-{args.mib}m.txt"
    if not data.exists():
        make_data(data, args.mib)
    cfg = Path(tempfile.mkdtemp(prefix="k95cfg-"))
    env = dict(os.environ, XDG_CONFIG_HOME=str(cfg), BASH_SILENCE_DEPRECATION_WARNING="1")
    xvfb = None
    if args.xvfb:
        xvfb = subprocess.Popen(["Xvfb", ":95", "-screen", "0", "1600x1200x24", "-nolisten", "tcp"],
                                stderr=subprocess.DEVNULL)
        env.update(DISPLAY=":95")
        env.pop("WAYLAND_DISPLAY", None)
        time.sleep(1)
    rows = []
    log = open(args.out / "app.log", "w")
    try:
        for rnd in range(args.rounds):
            order = list(builds)
            random.shuffle(order)
            for label in order:
                r = one_run(args, builds[label], data, env, log)
                r.update(round=rnd, label=label)
                rows.append(r)
                print(f"round {rnd:2d} {label:4s} wall {r['wall_s']:.3f}s cpu {r['cpu_s']:.3f}s", flush=True)
    finally:
        log.close()
        if xvfb:
            xvfb.terminate()
            xvfb.wait()
    (args.out / "rows.json").write_text(json.dumps(rows, indent=1))

    by = {(r["round"], r["label"]): r for r in rows}
    summary = {"hashes": hashes, "rounds": args.rounds, "mib": args.mib, "taskset": args.taskset,
               "xvfb": args.xvfb, "platform": platform.platform(), "metrics": {}}
    failed = False
    print(f"\n{args.rounds} rounds, cat {args.mib} MiB, median of per-round change vs main [95% CI]")
    for metric in ("cpu_s", "wall_s"):
        med = {k: statistics.median(by[(i, k)][metric] for i in range(args.rounds)) for k in builds}
        d = {k: [100 * (by[(i, k)][metric] / by[(i, "main")][metric] - 1) for i in range(args.rounds)]
             for k in ("head", "copy")}
        head, aa = boot_ci(d["head"]), boot_ci(d["copy"])
        bound = max(abs(aa[1]), abs(aa[2]))
        if head[2] < -bound:
            verdict = "LOWER"
        elif head[1] > bound:
            verdict = "REGRESSION"
        else:
            verdict = "within A/A noise"
        if metric == "wall_s" and verdict == "REGRESSION":
            failed = True
        summary["metrics"][metric] = {"median": med, "head_pct": head, "aa_pct": aa,
                                      "aa_bound_pct": bound, "verdict": verdict}
        print(f"  {metric:6s} medians main {med['main']:.3f} copy {med['copy']:.3f} head {med['head']:.3f}"
              f" | head {head[0]:+.1f}% [{head[1]:+.1f}, {head[2]:+.1f}]"
              f" | A/A {aa[0]:+.1f}% [{aa[1]:+.1f}, {aa[2]:+.1f}] -> {verdict}")
    (args.out / "summary.json").write_text(json.dumps(summary, indent=1))
    if args.xvfb:
        print("(Xvfb run: smoke only, not the merge number)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
