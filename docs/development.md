# Development

This guide covers Trident's crate layout, builds, tests, releases and performance tools.

## Layout

The Cargo workspace has three crates.

| Crate | What it does |
| --- | --- |
| `crates/term-core` | Terminal state: grid, scrollback, cursor, modes, selection, colors, and the VT/xterm escape sequence handling, parsed with [`vte`](https://crates.io/crates/vte). No GPU or window dependencies, so it builds and tests headless |
| `crates/term-render` | wgpu renderer in `renderer.rs` and `shader.wgsl` that draws a frame as one instanced pass, plus glyph rasterization with [`ab_glyph`](https://crates.io/crates/ab_glyph), system font fallback (`fallback.rs`), color glyphs (`color.rs`) and the GPU glyph atlases. Takes any window with raw window and display handles |
| `crates/term-app` | The `trident` binary: window and event loop with [`winit`](https://crates.io/crates/winit), the shell on a pseudo-terminal with [`portable-pty`](https://crates.io/crates/portable-pty), key and mouse encoding, and the clipboard |

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt
```

CI runs the same checks on Linux and macOS for every pull request.

### Regenerate the configuration reference

When changing the config schema, update the field descriptions in
[`config/reference.rs`](../crates/term-app/src/config/reference.rs) and regenerate the
[reference](config-reference.md) from the workspace root:

```sh
cargo test -p term-app write_config_reference -- --ignored
```

Commit the generated Markdown. The workspace tests compare it with fresh output.

## Source documentation

Follow the [canonical source-documentation standard](https://github.com/amagomori/.github/blob/main/docs/rust_documentation.md)
when writing or reviewing Rust comments, rustdoc, Python docstrings and other first-party
source comments. The standard is maintained in `amagomori/.github`; do not copy it here.

Run the documentation gates locally before opening a PR:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p term-core -p term-render
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items -p term-core -p term-render -p term-app
cargo test --doc -p term-core -p term-render
```

Both CI platforms run these checks. The three first-party crates opt into
`doc_markdown`, `missing_safety_doc` and `undocumented_unsafe_blocks`; vendored code
does not inherit them. There is no blanket missing-documentation, error-heading,
panic-heading or example quota. Document non-obvious contracts and use examples
where they help, not to satisfy coverage counts. A green Linux run does not check
macOS-only code; run the same commands on macOS as well.

Linters check structure, links and examples, not whether prose is correct. Review comments
against the implementation, including ownership, units, bounds, errors, panics and unsafe
invariants. The PR checklist records that review.

## Release

Pushing a `v*` tag that matches the workspace version in `Cargo.toml` runs
`.github/workflows/release.yml`. Examples include `v1.0.0` for a stable release and
`v1.0.0-rc.1` for a prerelease.
This is the only workflow that runs on tags. It builds:

- `Trident-<version>-macos-universal.dmg`, a universal macOS binary inside `Trident.app`
- `Trident-<version>-linux-x86_64.tar.gz`, `Trident-<version>-linux-amd64.deb` and
  `Trident-<version>-x86_64.AppImage` for Linux x86_64

The workflow publishes these artifacts with `SHA256SUMS` and release notes listing the PRs merged
since the previous tag. The macOS app is ad-hoc signed to run on Apple silicon but has no
Developer ID signature or notarization.

The packaging lives in `.github/scripts/release/release.py`, so you can build and check the
artifacts locally before you tag. Updating the workspace version, building artifacts and
previewing notes do not publish anything. Until the release owner authorizes the
publication window, do not:

- Create or push a tag
- Publish a release
- Update the tap or AUR
- Change repository visibility

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
.github/scripts/release/pdfium.sh linux-x64 pdfium
python3 .github/scripts/release/release.py linux --pdfium pdfium --binary target/release/trident --version "$version" \
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
.github/scripts/release/pdfium.sh mac-universal pdfium
python3 .github/scripts/release/release.py macos --pdfium pdfium --binary trident-universal --version "$version" --out dist
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

### Update icons and images

- `assets/generate_icons.py` requires Pillow and generates icons from the source images in
  `assets/`. It writes `assets/icons/<name>.icns`, the macOS menu bar images
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
presentation feedback is verified. X11 Present, macOS Metal and present-return timings are
labeled unverified. None of them is an optical measurement of the display. For setup, flags and
how frames are matched, see the script's docstring.

## Compare with other terminals

`.github/scripts/compare/compare.py` runs the same black-box tests in Trident, Alacritty, kitty,
WezTerm and Ghostty on one Linux X display. Use `terminal` for Trident in `--terms`
and `--bin`. It measures:

- Startup measurements cover shell execution, window mapping and first paint.
- Keypress-to-framebuffer latency is measured through XTest.
- Throughput tests run `cat` on a 100 MiB file and the
  [vtebench](https://github.com/alacritty/vtebench) suite.
- Resource measurements cover RSS, PSS and idle CPU before and after a 50,000-line burst.

No terminal is instrumented. Each one gets the matching config in `compare/cfg/` (JetBrains Mono
14, 100x30, 10,000 lines of scrollback).

```sh
sudo apt install xvfb openbox python3-xlib xdotool fonts-jetbrains-mono
.github/scripts/compare/run.sh           # one pass; tables in target/compare/results.md
.github/scripts/compare/run.sh rounds    # two rounds with noise bounds
```

`run.sh` builds the release binary, starts Xvfb and openbox if there is no display, installs
vtebench under `target/compare` if needed, and runs every terminal found on `PATH`.

- Extra arguments go to `compare.py`. Use `--bin kitty=PATH` for a binary not on `PATH`, repeating
  it for each terminal. Use `--tests` or `--terms` to run a subset. In `rounds` mode, pass these
  arguments in `BINS`.
- Results merge into `target/compare/results.json`, so you can split runs.
- On Xvfb with llvmpipe, the numbers compare terminals on one machine rather than measuring
  real-display latency. Ghostty ignores XTest keys and spins its renderer there, so it gets no
  latency or `cat` numbers.

On macOS, `.github/scripts/compare/macos/run.sh` runs the same tests against Alacritty, Ghostty and
WezTerm, and runs Trident twice to establish noise bounds. Set `ARMS="... kitty"` to include kitty.
The shell running it needs **Screen Recording** and **Accessibility** permission.

```sh
brew install --cask ghostty wezterm font-jetbrains-mono
cargo install alacritty vtebench && git clone https://github.com/alacritty/vtebench ~/vtebench
cargo build --release && .github/scripts/compare/macos/run.sh   # writes target/compare-macos/RESULTS.md
```
