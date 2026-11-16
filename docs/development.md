# Development

This page is for contributors: how the code is laid out, how to test and release it, and how to
measure latency and compare Trident with other terminals.

## Layout

A Cargo workspace of three crates:

| Crate | What it does |
| --- | --- |
| `crates/term-core` | Terminal state: grid, scrollback, cursor, modes, selection, colors, and the VT/xterm escape sequence handling (parsed with [`vte`](https://crates.io/crates/vte)). No GPU or window dependencies, so it builds and tests headless |
| `crates/term-render` | wgpu renderer (`renderer.rs`, `shader.wgsl`) that draws a frame as one instanced pass, plus glyph rasterization ([`ab_glyph`](https://crates.io/crates/ab_glyph)), system font fallback (`fallback.rs`), color glyphs (`color.rs`) and the GPU glyph atlases. Takes any window with raw window and display handles |
| `crates/term-app` | The `trident` binary: window and event loop ([`winit`](https://crates.io/crates/winit)), the shell on a pseudo-terminal ([`portable-pty`](https://crates.io/crates/portable-pty)), key and mouse encoding, and the clipboard |

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt
```

CI runs the same checks on Linux and macOS for every pull request.

## Release

Pushing a `v*` tag that matches the workspace version in `Cargo.toml` (for example `v1.0.0`, or
`v1.0.0-rc.1` for a prerelease) runs `.github/workflows/release.yml`, the only workflow that runs
on tags. It builds a universal macOS binary into `Trident.app` inside
`Trident-<version>-macos-universal.dmg`, and a Linux x86_64 `Trident-<version>-linux-x86_64.tar.gz`,
`Trident-<version>-linux-amd64.deb` and `Trident-<version>-x86_64.AppImage`, then publishes them
with `SHA256SUMS` and release notes that list the PRs merged since the previous tag. Builds aren't
signed or notarized. The app is ad-hoc signed so that it runs on Apple silicon.

The packaging lives in `.github/scripts/release/release.py`, so you can build and check the
artifacts locally before you tag. Updating the workspace version, building artifacts and
previewing notes do not publish anything. Do not create or push a tag, publish a release,
update the tap/AUR, or change repository visibility until the release owner authorizes the
publication window.

Start both native builds from the same frozen commit, with a clean `dist` directory on each
machine. Validate the intended tag without creating it:

```sh
tag=v1.0.0
version=$(python3 .github/scripts/release/release.py version --tag "$tag")
export SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)
python3 -m unittest discover -s .github/scripts/release
```

Build Linux on Ubuntu 22.04 (glibc 2.35), as the workflow does. Download appimagetool and its
runtime using the URLs and SHA-256 pins in `release.yml`, verify both checksums, and make
`appimagetool` executable before running:

```sh
# Linux
cargo build --release --locked -p term-app
python3 .github/scripts/release/release.py linux --binary target/release/trident --version "$version" \
  --out dist --appimagetool ./appimagetool --runtime ./appimage-runtime
./dist/*.AppImage --appimage-extract-and-run --version
```

```sh
# macOS: a universal binary, as release.yml builds it
rustup target add aarch64-apple-darwin x86_64-apple-darwin
export MACOSX_DEPLOYMENT_TARGET=11.0
cargo build --release --locked -p term-app --target aarch64-apple-darwin
cargo build --release --locked -p term-app --target x86_64-apple-darwin
lipo -create -output trident-universal \
  target/aarch64-apple-darwin/release/trident target/x86_64-apple-darwin/release/trident
lipo trident-universal -verify_arch arm64 x86_64
python3 .github/scripts/release/release.py macos --binary trident-universal --version "$version" --out dist
hdiutil verify "dist/Trident-$version-macos-universal.dmg"
```

Combine only the four distributable files from those builds in a clean `dist` directory:
the .dmg, .tar.gz, .deb and .AppImage. Keep `Trident.app`, tools, test outputs and preview notes
outside that directory. On Linux, prepare the final checksums, notes and package definitions:

```sh
python3 .github/scripts/release/release.py checksums dist
(cd dist && sha256sum -c SHA256SUMS)
python3 .github/scripts/release/release.py changelog --tag "$tag" > notes.md
python3 .github/scripts/release/release.py packages --version "$version" --sums dist/SHA256SUMS \
  --out packages
```

- If the tag doesn't exist, `changelog` writes the notes for `HEAD` as if it were tagged.
- The .deb needs `dpkg-deb` and `tic`. The .dmg needs macOS.
- `packages` writes the cask when `SHA256SUMS` lists the .dmg, and the AUR files when it lists the
  tarball, so a `dist` from either platform works.
- `release.yml` pins appimagetool and its runtime by SHA-256.

### When hosted Actions is unavailable

The native steps above also prepare a release without GitHub-hosted runners. Keep the frozen
commit ID with the artifacts and record native macOS/Linux install and upgrade results. A
local cask or PKGBUILD can use `--base-url "file://$PWD/dist"`; regenerate `packages` without
that override before publication so no local download URL reaches the tap or AUR. Do not
substitute a Linux binary for the Mac build or label a single-architecture Mac build universal.

At the authorized publication window, the release owner can tag the verified commit and
upload the four artifacts plus `SHA256SUMS` with `gh release create --verify-tag`, using
`notes.md`. This is a publishing action, not part of preparation. Do not also run the automatic
publisher for the same tag. Updating the tap and AUR afterward is a separate publishing step:
use the generated package files, the existing `push_package.sh` and the same stable/newest
release checks as the workflow, with the already provisioned credentials. Never create new
credentials just to rehearse a release. If credentials are unavailable locally, retain the
prepared files for the authorized release owner rather than exporting Actions secrets.

Before calling the release complete, check anonymous download access and perform real
Homebrew/AUR installs and upgrades against the published URLs. A private source repository,
an unpublished tag, or missing package credentials cannot be validated by offline tests.

### Homebrew and the AUR

When a stable (non-prerelease) tag is published, the `packages` job writes the Homebrew cask
(`trident.rb`) and the AUR `trident-bin` `PKGBUILD` and `.SRCINFO` from the release's
`SHA256SUMS`. It uploads them as the `packages` artifact and pushes them:

- `Casks/trident.rb` to [amagomori/homebrew-tap](https://github.com/amagomori/homebrew-tap), when
  the `HOMEBREW_TAP_TOKEN` secret holds a token that can push there
  (`brew install --cask amagomori/tap/trident`).
- `PKGBUILD` and `.SRCINFO` to `ssh://aur@aur.archlinux.org/trident-bin.git`, when the
  `AUR_SSH_KEY` secret holds the private key of the AUR account that maintains `trident-bin`.

Each push is skipped while its secret is unset, and re-running the job for a published release
pushes nothing new. To try the files before you tag, point them at local artifacts with
`--base-url`, then run `makepkg` in the `aur` directory, or on macOS `brew install --cask` from a
local tap.

### Icons and images

- `assets/generate_icons.py` (needs Pillow) writes every app icon variant from the source images
  in `assets/`: `assets/icons/<name>.icns`, the macOS menu bar images
  (`<name>-menubar.png`, `<name>-menubar@2x.png`) and the Linux `hicolor` icons
  (`tide-*.png`). After you edit a source image, run `python3 assets/generate_icons.py`.
- The tarball, .deb and AppImage install the icons with the `.desktop` entry,
  `packaging/linux/trident.desktop`.
- `assets/social-preview.png` (1280x640) is the repository's social preview image. GitHub shows it
  only for public repositories, so upload it under **Settings** > **General** > **Social preview**
  when the repository is public.

## Measure latency

`.github/scripts/latency.py` measures input-to-present latency and throughput of a real window.
Run it from a Linux or macOS desktop session, not Xvfb:

```sh
python3 .github/scripts/latency.py --base ../baseline --head . --runs 5
python3 .github/scripts/latency.py --app target/release/trident --runs 5
```

It reports p50, p99 and mean latency per run with their spread, plus presented, wasted and late
frames and bytes per second under load. Each run names its timing source. Only Wayland
presentation feedback is verified; X11 Present, macOS Metal and present-return timings are
labeled unverified. None of them is an optical measurement of the display. For setup, flags and
how frames are matched, see the script's docstring.

## Compare with other terminals

`.github/scripts/compare/compare.py` runs the same black-box tests in Trident, Alacritty, kitty,
WezTerm and Ghostty on one Linux X display. The script calls Trident `terminal`, in `--terms`
and `--bin`. It measures:

- Startup: shell exec, window mapped and first paint.
- Keypress-to-framebuffer latency through XTest.
- `cat` of a 100 MiB file and the [vtebench](https://github.com/alacritty/vtebench) suite.
- RSS, PSS and idle CPU before and after a 50,000-line burst.

No terminal is instrumented. Each one gets the matching config in `compare/cfg/` (JetBrains Mono
14, 100x30, 10,000 lines of scrollback).

```sh
sudo apt install xvfb openbox python3-xlib xdotool fonts-jetbrains-mono
.github/scripts/compare/run.sh           # one pass; tables in target/compare/results.md
.github/scripts/compare/run.sh rounds    # two rounds with noise bounds, as in benchmarks.md
```

`run.sh` builds the release binary, starts Xvfb and openbox if there is no display, installs
vtebench under `target/compare` if needed, and runs every terminal found on `PATH`.

- Extra arguments go to `compare.py`: `--bin kitty=PATH` (repeatable) for a binary not on `PATH`,
  such as a Ghostty AppImage, and `--tests`/`--terms` for a subset. In `rounds` mode, pass them
  in `BINS`.
- Results merge into `target/compare/results.json`, so you can split runs.
- On Xvfb with llvmpipe, the numbers compare terminals on one machine; they aren't real-display
  latency. Ghostty ignores XTest keys and spins its renderer there, so it gets no latency or `cat`
  numbers.

On macOS, `.github/scripts/compare/macos/run.sh` runs the same tests against Alacritty, Ghostty and
WezTerm (add kitty with `ARMS="... kitty"`), with Trident run twice for noise bounds. See
[benchmarks.md](benchmarks.md#run-it) for setup.
