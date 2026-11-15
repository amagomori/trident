# Latency QA scripts

Release-binary latency checks used for KAN-89 (keypress-to-frame) and the KAN-64 real-display
comparison against Alacritty. Python 3 stdlib unless noted. Run from a desktop session, never
Xvfb, for anything that counts as evidence.

| script | measures | platforms |
| --- | --- | --- |
| `ab.py` | Merge number: head vs main through `../latency.py --app` (write_input -> PTY echo -> frame, with presentation feedback where the platform gives it), plus an identical main copy as A/A. Per-round paired deltas with 95% bootstrap CIs; exit 1 on a regression beyond A/A noise. | Linux X11, nested Weston, macOS |
| `vs_alacritty.py` | Black-box keypress-to-pixel (OS key event -> captured screen pixels) for head, main, main copy and Alacritty in shuffled rounds, idle and with `--load`. Prints a MET / NOT MET verdict for "head no worse than Alacritty". | Linux X11 (`python3-xlib`, `xdotool`), macOS (`keylat`) |
| `keylat.swift` | macOS helper for `vs_alacritty.py`: CGEvent keypress, CGWindowListCreateImage poll. `swiftc -O keylat.swift -o keylat`; needs Accessibility and Screen Recording for the app running it. | macOS |

```sh
git worktree add ../t-main origin/main && git worktree add ../t-head <head-sha>
(cd ../t-main && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app)
(cd ../t-head && CARGO_TARGET_DIR=$PWD/target cargo build --release -p term-app)
python3 .github/scripts/latency-qa/ab.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --rounds 8 --out ab-out
python3 .github/scripts/latency-qa/vs_alacritty.py --main ../t-main/target/release/trident \
    --head ../t-head/target/release/trident --rounds 6 --probes 200 --load --out vs-out
```

Both write `rows.json` (every run) and `summary.json` into `--out`; a reused `--out` loses its old
`summary.json` at the start of the run, so a run with no verdict never leaves a stale one. Report the timing source
`ab.py` prints: only `wayland-presentation` is verified presentation; `x11-present`,
`metal-presented` and `present-return` are labelled unverified. Where every presentation
callback is a skip (the macOS VM's paravirtual Metal), `ab.py` reports present-return and
marks fps/wasted n/a. It exits 2 if it gets no echo p50 at all, and exits 2 with no verdict table
and no `summary.json` if one workload's runs mix timing sources (raw runs stay in `rows.json`). `vs_alacritty.py`
finds Alacritty on `PATH`, then (macOS) in `/Applications` or `~/Applications`; `--alacritty` takes
a binary or an `Alacritty.app`. Both scripts check every binary path before the first run. `vs_alacritty.py` measures
until the pixels change in the captured screen, not photons at the panel.

macOS Alacritty: the Homebrew cask is disabled (Gatekeeper), so use the release DMG:

```sh
curl -fsSLo /tmp/alacritty.dmg https://github.com/alacritty/alacritty/releases/download/v0.17.0/Alacritty-v0.17.0.dmg
hdiutil attach -nobrowse -mountpoint /tmp/alacritty /tmp/alacritty.dmg
cp -R /tmp/alacritty/Alacritty.app /Applications/ && hdiutil detach /tmp/alacritty
/Applications/Alacritty.app/Contents/MacOS/alacritty --version   # alacritty 0.17.0
```

`keylat` polls with CGWindowListCreateImage, which costs about 5-7 ms per grab on the macOS VM, so
its samples carry that much detection lag; it is the same for every label.
