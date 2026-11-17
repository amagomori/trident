# Process-group QA scripts

These scripts check changes to Trident's foreground process-group tracking. They measure CPU cost
under output, terminal modes left on by a dead command, and notification and title latency.
They need Python 3 with only the standard library on Linux or macOS. Run them from a desktop
session. Use `--xvfb` only for a smoke check.

| Script | Measures |
| --- | --- |
| `cpu_ab.py` | App CPU (all app threads) and wall time of `cat` of 60 MiB up to a DA1 reply, head against main plus a byte-identical main copy as A/A, shuffled per round. Prints the median per-round % change with a 95% bootstrap CI and exits 1 on a wall-time regression beyond A/A. |
| `modes.py` | Whether mouse, focus and bracketed-paste reporting left on by a dead command still reach the next prompt. Covers idle, SIGKILL, a 20 MiB flood and a prompt that re-enables `?2004h`, main against head. |
| `timing.py` | Long-command notification and OSC 2 title latency, idle and during floods, main against head. Exits 1 when head misses a notification or title, or when head's median is slower than main's by more than max(20 ms, 10%), or 200 ms for notifications. |

## Prerequisites

- On Linux, use an X11 session with `xdotool`, `xprop`, `xmessage`, `xclip` and `dbus-monitor`, and the
  desktop session bus.
- On macOS, grant **Accessibility** permission to the app that runs the scripts. The stock
  `/usr/bin/python3` and `/bin/bash` are enough.

## Run

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
for d in ../t-main ../t-head; do (cd $d && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app); done
python3 .github/scripts/pgrp-qa/cpu_ab.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --rounds 16 --out cpu-ab
python3 .github/scripts/pgrp-qa/modes.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --runs 5 --out modes
python3 .github/scripts/pgrp-qa/timing.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --runs 6 --out timing
```

All three write `rows.json` (every run) into `--out`, and `cpu_ab.py` also writes `summary.json`
with binary hashes. On Linux, run `cpu_ab.py` both unpinned and with `--taskset 2`, which pins the
app to one core.
