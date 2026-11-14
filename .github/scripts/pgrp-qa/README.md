# KAN-95 process-group QA scripts

Release-binary checks for PR #61 (sample the shell's foreground process group at most every
5 ms while output is queued). Python 3 stdlib, Linux and macOS. Run from a desktop session
for anything that counts as evidence; `--xvfb` is a smoke check only.

| script | measures |
| --- | --- |
| `cpu_ab.py` | Merge number: app CPU (all app threads, not the shell or `cat`) and wall time of `cat` 60 MiB up to a DA1 reply, head vs main plus a byte-identical main copy for A/A, shuffled per round. Median per-round % change with 95% bootstrap CI; exit 1 on a wall-time regression beyond A/A. |
| `modes.py` | Shell-return marks: whether mouse, focus and bracketed-paste reports a dead command left on still reach the next prompt, per scenario (idle, SIGKILL, during a 20 MiB flood, prompt re-enabling `?2004h`, plus the documented flood-tail case), main vs head. Needs `xdotool xclip xmessage` on Linux X11; Accessibility for the runner on macOS. |
| `timing.py` | Long-command notification and OSC 2 title latency (idle and during floods), main vs head, shuffled. Exit 1 when a notification or final title is missing on head, or head's median is above main's by more than max(20 ms, 10%), at least 100 ms for the notify cases (200 ms watcher poll). Linux: `xdotool xprop xmessage dbus-monitor` and the desktop session bus; macOS: Accessibility for the runner. |

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
for d in ../t-main ../t-head; do (cd $d && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app); done
python3 .github/scripts/pgrp-qa/cpu_ab.py --main ../t-main/target/release/terminal \
    --head ../t-head/target/release/terminal --rounds 16 --out cpu-ab
python3 .github/scripts/pgrp-qa/modes.py --main ../t-main/target/release/terminal \
    --head ../t-head/target/release/terminal --runs 5 --out modes
python3 .github/scripts/pgrp-qa/timing.py --main ../t-main/target/release/terminal \
    --head ../t-head/target/release/terminal --runs 6 --out timing
```

All three write `rows.json` (every run) into `--out`; `cpu_ab.py` also writes `summary.json`
with binary hashes. On Linux, `cpu_ab.py --taskset 2` pins the app to one core, as in the
PR's own A/B; run it pinned and unpinned.

macOS: the stock `/usr/bin/python3` (3.9) and `/bin/bash` (3.2) are enough.
