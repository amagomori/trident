# Development

## Layout

A Cargo workspace of three crates:

| Crate | What it does |
| --- | --- |
| `crates/term-core` | Terminal state: grid, scrollback, cursor, modes, selection, colors, and the VT/xterm escape sequence handling (parsed with [`vte`](https://crates.io/crates/vte)). No GPU or window dependencies, so it builds and tests headless |
| `crates/term-render` | wgpu renderer (`renderer.rs`, `shader.wgsl`) that draws a frame as one instanced pass, plus glyph rasterization ([`ab_glyph`](https://crates.io/crates/ab_glyph)), system font fallback (`fallback.rs`), color glyphs (`color.rs`) and the GPU glyph atlases. Takes any window with raw window and display handles |
| `crates/term-app` | The `trident` binary: window and event loop ([`winit`](https://crates.io/crates/winit)), the shell on a pseudo-terminal ([`portable-pty`](https://crates.io/crates/portable-pty)), key and mouse encoding, and the clipboard |

## Building and testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt
```

CI runs these on Linux and macOS for every pull request.

## Releasing

Pushing a `v*` tag that matches the workspace version in `Cargo.toml` (for example `v0.1.0`, or
`v0.1.0-rc.1` for a prerelease) runs `.github/workflows/release.yml`, the only workflow that runs
on tags. It builds a universal macOS binary into `Trident.app` inside
`Trident-<version>-macos-universal.dmg`, and a Linux x86_64 `Trident-<version>-linux-x86_64.tar.gz`,
`Trident-<version>-linux-amd64.deb` and `Trident-<version>-x86_64.AppImage`, then publishes them
with `SHA256SUMS` and release notes listing the PRs merged since the previous tag. Builds are not
signed or notarized; the app is only ad-hoc signed so it runs on Apple silicon.

The packaging lives in `.github/scripts/release/release.py`, so artifacts can be built and checked
locally before tagging:

```sh
cargo build --release -p term-app
python3 .github/scripts/release/release.py linux --binary target/release/trident --version 0.1.0 \
  --out dist [--appimagetool appimagetool-x86_64.AppImage --runtime runtime-x86_64]
python3 .github/scripts/release/release.py macos --binary trident-universal --version 0.1.0 --out dist
python3 .github/scripts/release/release.py checksums dist
python3 .github/scripts/release/release.py changelog --tag v0.1.0
python3 .github/scripts/release/release.py packages --version 0.1.0 --sums dist/SHA256SUMS \
  --out packages [--base-url file://$PWD/dist]
python3 -m unittest discover -s .github/scripts/release
```

The .deb needs `dpkg-deb` and `tic`; the .dmg needs macOS. `release.yml` pins appimagetool and its
runtime by SHA-256.

### Homebrew and the AUR

After a stable (non-prerelease) tag is published, the `packages` job writes the Homebrew cask
(`trident.rb`) and the AUR `trident-bin` `PKGBUILD` and `.SRCINFO` from the release's
`SHA256SUMS`, uploads them as the `packages` artifact, and pushes them:

- `Casks/trident.rb` to [amagomori/homebrew-tap](https://github.com/amagomori/homebrew-tap), when
  the `HOMEBREW_TAP_TOKEN` secret holds a token that can push there
  (`brew install --cask amagomori/tap/trident`).
- `PKGBUILD` and `.SRCINFO` to `ssh://aur@aur.archlinux.org/trident-bin.git`, when the
  `AUR_SSH_KEY` secret holds the private key of the AUR account that maintains `trident-bin`.

Each push is skipped until its secret is set, and re-running the job for a published release
pushes nothing new. `--base-url` points the files at local artifacts, to try them before tagging
(`makepkg` in the `aur` directory, or `brew install --cask` from a local tap on macOS).

The Linux `.desktop` entry is `packaging/linux/trident.desktop`; the tarball, .deb and AppImage
install it with the `hicolor` icons `assets/generate_icons.py` writes to `assets/icons/tide-*.png`.

## Measuring latency

On Wayland with presentation feedback and a known refresh period, redraws and flood
parsing latch just before the predicted vblank, allowing for measured frame cost and
adaptive compositor slack. Other platforms and missing feedback keep immediate redraws
and the fixed flood timeout. Idle windows do not schedule frame timers.

From a Linux or macOS desktop session, run `python3 .github/scripts/latency.py
--base ../baseline --head . --runs 5`, or use
`--app target/release/trident`. It reports per-run p50/p99/mean input-to-present
latency and their min–max spread, plus presented/wasted/late frames and bytes/s
under sustained output. Load mode runs without probes for `--duration` seconds
(default 10) and reports throughput only. Late intervals exceed 1.5 refresh periods
and require both adjacent frames to have parsed new bytes; idle gaps do not count.
`--json results.json` saves samples; `--xvfb` is for smoke
tests only. See the script docstring for setup and comparison controls.
An uninstrumented baseline is skipped with a diagnostic; the head must produce
latency data. Meaningful comparisons require instrumentation in both checkouts.

Probes enter through the app's normal PTY input path. Output is matched to the
frame carrying parsed output. On X11, Present CompleteNotify supplies the
presentation timestamp (`x11-present`). On Wayland, `wp_presentation` feedback
(`wayland-presentation`) is requested before
each surface commit and converted from the compositor's advertised clock; the mode
records vsync/hardware-clock/hardware-completion/zero-copy flags. Skipped or missing
completions count as wasted frames.
Present-return latency is reported separately. Each run labels its timing source;
without feedback it explicitly reports `present-return (no feedback)` instead.
Wayland logs also include `clock_ns` with final frame cost, latch margin and refresh
period in nanoseconds (the comparison table's refresh comes from the monitor API).
On macOS, Metal drawable presented handlers use `presentedTime` (`metal-presented`).
The opt-in harness hooks the concrete command-buffer `presentDrawable:` method;
the direct drawable `present` path used with `presentsWithTransaction` is not
hooked. The app does not enable that setting (CAMetalLayer defaults to false).
Only Wayland presentation timing is verified, through headless Weston protocol
testing. Metal and X11 timing are **unverified**, and the table and JSON label
them accordingly. QA's paravirtual Metal run returned `presentedTime == 0` for all
681 callbacks (skipped); real-Mac timing remains unverified. X11 completion
matching is untested because no DRI3/GPU X server was available.
These timestamps are not an optical
measurement of pixel scanout; compositor/display processing may add delay.
For a Wayland smoke test, start Weston headless and run with its `WAYLAND_DISPLAY`
and `XDG_RUNTIME_DIR`, with `DISPLAY` unset. Headless feedback validates protocol
matching, not physical-display latency.
Likewise, llvmpipe on an X server can produce no Present completion events even
when the Present extension is advertised; such runs explicitly use the labeled
present-return fallback and cannot prove real-display scheduling gains.

## Comparing against other terminals

`.github/scripts/compare/compare.py` runs the same black-box tests in `terminal`,
Alacritty, kitty, WezTerm and Ghostty on one Linux X display: startup (shell exec,
window mapped, first paint), keypress-to-framebuffer latency through XTest, `cat` of a
100 MiB file, the [vtebench](https://github.com/alacritty/vtebench) suite, and RSS/PSS
plus idle CPU before and after a 50,000-line burst. Nothing in the terminals is
instrumented, and each one gets the matching config in `compare/cfg/` (JetBrains Mono
14, 100x30, 10k scrollback).

```sh
sudo apt install xvfb openbox xdotool python3-xlib fonts-jetbrains-mono
Xvfb :99 -screen 0 1920x1080x24 & DISPLAY=:99 openbox &
cargo build --release
DISPLAY=:99 python3 .github/scripts/compare/compare.py --terms terminal,alacritty
python3 .github/scripts/compare/summarize.py   # Markdown tables
```

Other terminals are looked up on `PATH`; pass `--bin kitty=PATH` (repeatable) for
others, such as an extracted kitty bundle or a Ghostty AppImage. vtebench needs its
binary (`cargo install --git https://github.com/alacritty/vtebench`) and a checkout
(`--vtebench-dir`, default `~/vtebench`). `--tests` and `--terms` pick a subset;
results merge into `target/compare/results.json`, so runs can be split. On Xvfb with
llvmpipe the numbers are relative comparisons on one box, not real-display latency.
Ghostty needs a Mesa GL version override there (set by the script), then spins its
renderer and ignores XTest keys, so only its vtebench numbers are usable.
