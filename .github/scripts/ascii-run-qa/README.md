# KAN-87 ASCII-run QA scripts

Release-binary PTY A/B for PR #55 (write plain ASCII runs in one pass), from the KAN-87 QA plan.
Python 3 stdlib, Linux and macOS.

| script | does |
| --- | --- |
| `fixtures.py` | Writes `dense.txt` (60 MB, all ASCII), `mixed.txt` (33 MB, box-drawing prompts, SGR, accented names) and `cjk.txt` (17 MB) into `--dir` and prints their absolute paths. |
| `ptyab.py` | Merge number: wall time from launch to exit of `terminal -e cat FIXTURE`, interleaved A/A (a copy of A) and A/B pairs, median % delta with 95% bootstrap CI. |

The app starts `-e` children in `$HOME`, not in the caller's directory, so `ptyab.py` passes the
fixture as an absolute path. It also checks `cat`'s exit status from inside the app on every run
and exits 2 when the workload did not run; the app itself exits 0 either way, so without that
check a failed `cat` times startup only (about 0.1 s on the Linux VM instead of 0.5 s for `dense`).

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
for d in ../t-main ../t-head; do (cd $d && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app); done
python3 .github/scripts/ascii-run-qa/fixtures.py --dir ../k87-fixtures
for f in dense mixed cjk; do
    python3 .github/scripts/ascii-run-qa/ptyab.py --a ../t-main/target/release/terminal \
        --b ../t-head/target/release/terminal --fixture ../k87-fixtures/$f.txt --pairs 10
done
```

Pass: `dense` improves outside the A/A bound; `mixed` and `cjk` do not regress beyond it.
