# Scroll-region QA

These checks cover changes to `Row::reset`, which clears rows when a scroll region scrolls.
Run them on Linux and a native Mac against a frozen main and PR head pair. Every script exits 2
if its workload doesn't run, preventing a wrong path from passing as a fast run.

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
M=$(cd ../t-main && pwd)/target/release/trident
H=$(cd ../t-head && pwd)/target/release/trident
FX=$HOME/qa-fixtures; Q=$PWD/.github/scripts
```

Install vtebench and Alacritty:

- To install vtebench, run `git clone https://github.com/alacritty/vtebench ~/vtebench && cargo install --locked
  --path ~/vtebench`.
- On Linux, install Alacritty from the distribution or run `cargo install --locked alacritty`.
  Building requires the fontconfig, freetype, xcb and xkbcommon dev packages and cmake.
- On macOS, copy Alacritty from the release .dmg to `/Applications` because Gatekeeper
  can block the Homebrew cask.

On macOS, neither the Alacritty binary nor `~/.cargo/bin` is on `PATH` by default, so before the
commands, run:

```sh
export PATH=$HOME/.cargo/bin:/Applications/Alacritty.app/Contents/MacOS:$PATH
command -v alacritty vtebench   # both must print a path
```

`cfg/alacritty.toml` uses JetBrains Mono. On macOS, install it by copying the release zip's
`fonts/ttf` into `/Library/Fonts`. With the fallback font, Alacritty on macOS settles at 28 lines
and `vtebench_ab.py` fails its 100x30 check.

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

## Read the result

- The A/A bound is the larger absolute endpoint of the A/A 95% CI. Use ptyab's `A/A delta` line
  or the corresponding value per benchmark in `vtebench_ab.py`.
- A result is faster beyond the bound if the A/B CI's upper endpoint is below minus the bound.
  It is slower beyond the bound if the lower endpoint is above the bound.
- ptyab's last line, `A/A noise bound`, is the largest single A/A pair. It's informational, since
  one outlier pair sets it.
- `region_full`, full-width lines, is a control with little or no expected win. `region_bg`,
  colored erase, can also improve. Neither may regress.

`griddiff.py` is the check that catches a wrong reset bound, such as a row that leaves one occupied
cell unblanked. `vim_splits.py` is a visual smoke test for regions and splits. Vim's redraws don't
reach the bounded path.

## Platform notes

- On Linux, use an X11 display, `xdotool` and ImageMagick `import` for `vim_splits.py`. Xvfb with
  Mesa is sufficient, but report that software rendering makes the PTY A/B rendering-bound.
- On a native Mac, install Xcode command line tools, Rust and Python 3, and use `/usr/bin/vim`.
  The `vim_splits.py` script uses System Events and `screencapture`, so the shell running
  it needs **Accessibility** and **Screen Recording** permission. Before timing, run
  `xcrun simctl shutdown all` and `sudo mdutil -a -i off`. A macOS VM has a paravirtual Metal
  device and no Retina panel, so say so with the results.
- Keep the window at its 100x30 default. The `vtebench_ab.py` script fails if a terminal reports
  another size.
- `vtebench_ab.py` and `griddiff.py` run patched copies of vtebench's scripts
  (`vtebench_tty.py`), because upstream's tty lookup breaks on macOS and without a tty.
- `griddiff.py` keeps its dumper builds and failing streams in `--out` (default `~/qa-griddiff`).
- `vtebench_ab.py` writes its results to `--out` (default `~/qa-vtebench_ab.json`).
