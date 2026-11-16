# Latency QA scripts

These scripts compare keypress latency of a PR's release binary with main and with Alacritty. They
are for QA runners. They need Python 3 (stdlib only unless noted). Run them from a desktop session;
Xvfb results don't count as evidence.

| Script | Measures | Platforms |
| --- | --- | --- |
| `ab.py` | Head against main through `../latency.py --app` (input, PTY echo, frame), with an identical main copy as A/A. Prints per-round paired deltas with 95% bootstrap CIs and exits 1 on a regression beyond A/A noise. | Linux X11, nested Weston, macOS |
| `vs_alacritty.py` | Keypress to changed screen pixels for head, main, a main copy and Alacritty in shuffled rounds, idle and with `--load`. Prints MET or NOT MET for "head no worse than Alacritty". | Linux X11 (`python3-xlib`, `xdotool`), macOS (`keylat`) |
| `keylat.swift` | macOS helper for `vs_alacritty.py`. Build it with `swiftc -O keylat.swift -o keylat`. | macOS |

## Run

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
(cd ../t-main && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app)
(cd ../t-head && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app)
python3 .github/scripts/latency-qa/ab.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --rounds 8 --out ab-out
python3 .github/scripts/latency-qa/vs_alacritty.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --rounds 6 --probes 200 --load --out vs-out
```

On macOS, the app that runs `keylat` needs **Accessibility** and **Screen Recording** permission.

`vs_alacritty.py` finds Alacritty on `PATH`, then on macOS in `/Applications` or `~/Applications`.
To use another one, pass `--alacritty` with a binary or an `Alacritty.app`. On macOS, install it
from the [Alacritty release](https://github.com/alacritty/alacritty/releases) .dmg, because
Gatekeeper can block the Homebrew cask.

## Read the result

- Both scripts write `rows.json` (every run) and `summary.json` into `--out`. A run that reaches no
  verdict leaves no `summary.json`.
- Report the timing source `ab.py` prints. Only `wayland-presentation` is verified presentation
  timing; `x11-present`, `metal-presented` and `present-return` are labeled unverified.
- `ab.py` exits 2 if it gets no echo, or if one workload's runs mix timing sources.
- `vs_alacritty.py` measures until captured screen pixels change, not light from the panel. On
  macOS, each `keylat` screen grab adds a few milliseconds of detection lag, the same for every
  terminal.
