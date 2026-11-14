# Scroll-region QA (KAN-90)

QA for occupancy-bounded `Row::reset`: run on Linux and on native macOS against a frozen
main / PR head pair. Every script takes absolute paths (or makes them absolute), and exits 2
when its workload did not run, so a wrong path cannot pass as a fast run.

| Step | Script | Pass |
|---|---|---|
| 1. Release PTY A/B, scroll-region fixtures | `fixtures.py` + `../ascii-run-qa/ptyab.py` | `region_bottom` head faster than main beyond the A/A bound; no fixture slower beyond it |
| 2. Dense ASCII non-regression | `../ascii-run-qa/fixtures.py` + `ptyab.py` | `dense`, `mixed`, `cjk` head not slower beyond the A/A bound |
| 3. vtebench vs Alacritty | `vtebench_ab.py` | `scrolling_bottom_small_region` head within A/A noise of Alacritty; no benchmark slower than main |
| 4. Vim split visual check | `vim_splits.py` | every incremental frame pixel-identical to vim's `:redraw!` of it |
| 5. Grid differential | `griddiff.py` | main and head state identical on every stream |

## Setup

```sh
git worktree add ../t-main <frozen main sha>
git worktree add ../t-head <frozen head sha>
for d in ../t-main ../t-head; do (cd $d && cargo build --release -p term-app); done
M=$(cd ../t-main && pwd)/target/release/terminal
H=$(cd ../t-head && pwd)/target/release/terminal
FX=$HOME/qa-fixtures; Q=$PWD/.github/scripts
```

vtebench and Alacritty: `git clone https://github.com/alacritty/vtebench ~/vtebench &&
cargo install --locked --path ~/vtebench`; Alacritty from the distro, `cargo install --locked alacritty`
(Linux, needs fontconfig/freetype/xcb/xkbcommon dev packages and cmake), or on macOS the
release `Alacritty-v0.17.0.dmg` copied to /Applications (the Homebrew cask can fail Gatekeeper),
binary `/Applications/Alacritty.app/Contents/MacOS/alacritty`. On macOS neither that directory nor
`~/.cargo/bin` (vtebench) is on PATH by default, so before the commands:

```sh
export PATH=$HOME/.cargo/bin:/Applications/Alacritty.app/Contents/MacOS:$PATH
command -v alacritty vtebench   # both must print a path
```

Fonts: `cfg/alacritty.toml` uses JetBrains Mono. Install it (the release zip's `fonts/ttf` into
`/Library/Fonts` on macOS); with the Menlo fallback, Alacritty 0.17.0 on macOS opens at 30 lines
and settles at 28, and `vtebench_ab.py` fails its 100x30 check.

## Commands

```sh
python3 $Q/scroll-region-qa/fixtures.py --dir $FX
python3 $Q/ascii-run-qa/fixtures.py --dir $FX
for f in region_bottom region_top region_full region_bg dense mixed cjk; do
  python3 $Q/ascii-run-qa/ptyab.py --a $M --b $H --fixture $FX/$f.txt --pairs 15
done
python3 $Q/scroll-region-qa/vtebench_ab.py --main $M --head $H --alacritty $(command -v alacritty) --rounds 8
python3 $Q/scroll-region-qa/vim_splits.py --bin main=$M --bin head=$H --out $HOME/qa-vim
python3 $Q/scroll-region-qa/griddiff.py --main ../t-main --head ../t-head --fixtures $FX
```

The A/A bound is the larger |end| of the A/A 95% CI (ptyab's `A/A delta` line; `vtebench_ab.py`
computes the same per benchmark). Faster beyond it: the A/B CI's upper end is below minus the
bound; slower beyond it: the A/B CI's lower end is above the bound. ptyab's last line
(`A/A noise bound`, the largest single A/A pair) is informational: one outlier pair sets it.

`region_full` (full-width lines) is a control where little or no win is expected. `region_bg`
(colored erase, full-reset path) is not a no-win control: on macOS (#64 at eea46c4) it was
-22% [-24, -12] against an A/A CI of [-1.3, 0.6]. Both must not regress; a `region_bg` win is
not a failure.

## What each check can catch

A mutant that leaves one occupied cell un-blanked in `Row::reset`
(`self.cells[..occupied.saturating_sub(1)]`) fails `griddiff.py` on 30 of 53 streams (40 seeds),
so the differential reaches `reset`. `vim_splits.py` passes on that mutant: vim's split windows
scroll inside DECSTBM regions and redraw, which does not reach the bounded path, so it is a visual
smoke test for regions and splits, not a detector for the reset bound.

## Platform notes

- Linux: an X11 display (Xvfb + Mesa is enough), `xdotool` and ImageMagick `import` for
  `vim_splits.py`. Software rendering makes the PTY A/B rendering-bound; report it.
- macOS: a native Mac (a macOS child, not Linux), Xcode command line tools, Rust, Python 3, vim
  (`/usr/bin/vim`). `vim_splits.py` uses System Events and `screencapture`, so the shell running
  it needs Accessibility and Screen Recording permission. Before timing: `xcrun simctl shutdown all`
  and `sudo mdutil -a -i off`. A macOS VM has a paravirtual Metal device and no Retina panel:
  say so with the results.
- Keep the window at its 100x30 default; `vtebench_ab.py` fails if a terminal reports another size.
- `vtebench_ab.py` and `griddiff.py` run patched copies of vtebench's scripts (`vtebench_tty.py`):
  upstream's `tty="/dev/$(ps -o tty= -p $$)"` breaks on macOS (BSD ps pads the name) and with no
  tty, and the setups then emit `ESC[1;0r` instead of their small region, still exiting 0.
- `griddiff.py` keeps its dumper builds and failing streams in `--out` (default `~/qa-griddiff`).
- `vtebench_ab.py` writes its results to `--out` (default `~/qa-vtebench_ab.json`).
