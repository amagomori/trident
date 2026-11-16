# ASCII-run QA scripts

These scripts time a release binary against a baseline on large `cat` workloads. They are for QA
runners checking a PR that changes how PTY output is parsed or written to the grid. They need
Python 3 (stdlib only) on Linux or macOS.

| Script | Does |
| --- | --- |
| `fixtures.py` | Writes `dense.txt` (60 MB, all ASCII), `mixed.txt` (33 MB: box-drawing prompts, SGR, accented names) and `cjk.txt` (17 MB) into `--dir`, and prints their absolute paths. |
| `ptyab.py` | Times launch to exit of `trident -e cat FIXTURE` in interleaved A/A (A against a copy of A) and A/B pairs. Prints the median % change with a 95% bootstrap CI. |

## Run

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
for d in ../t-main ../t-head; do (cd $d && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app); done
python3 .github/scripts/ascii-run-qa/fixtures.py --dir ../qa-fixtures
for f in dense mixed cjk; do
    python3 .github/scripts/ascii-run-qa/ptyab.py --a ../t-main/target/release/trident \
        --b ../t-head/target/release/trident --fixture ../qa-fixtures/$f.txt --pairs 10
done
```

Pass fixture paths as absolute paths, because the app starts `-e` children in `$HOME`. `ptyab.py`
exits 2 if `cat` fails inside the app, so a run that only timed startup can't pass.

## Read the result

A change is beyond noise only when its A/B CI lies outside the A/A bound. A PR that targets plain
ASCII output passes when `dense` improves beyond the bound and `mixed` and `cjk` don't regress
beyond it.
