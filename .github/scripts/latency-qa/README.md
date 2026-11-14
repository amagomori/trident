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
python3 .github/scripts/latency-qa/ab.py --main ../t-main/target/release/terminal \
    --head ../t-head/target/release/terminal --rounds 8 --out ab-out
python3 .github/scripts/latency-qa/vs_alacritty.py --main ../t-main/target/release/terminal \
    --head ../t-head/target/release/terminal --alacritty "$(command -v alacritty)" \
    --rounds 6 --probes 200 --load --out vs-out
```

Both write `rows.json` (every run) and `summary.json` into `--out`. Report the timing source
`ab.py` prints: only `wayland-presentation` is verified presentation; `x11-present`,
`metal-presented` and `present-return` are labelled unverified. `vs_alacritty.py` measures
until the pixels change in the captured screen, not photons at the panel.
