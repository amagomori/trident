<p align="center">
  <img src="assets/tide.png" alt="Trident icon" width="128">
</p>

<h1 align="center">Trident</h1>

<p align="center">Trident is a GPU-rendered terminal emulator for macOS and Linux, written in Rust.</p>

<p align="center">
  <img src="docs/images/screenshot.png" alt="Trident showing git log, an inline image, CJK, emoji, text styles and 256 colors" width="600">
</p>

## Install

### macOS

Trident runs on macOS 11 or later, on Apple silicon and Intel.

- With [Homebrew](https://brew.sh), run `brew install --cask amagomori/tap/trident`. To update, run
  `brew upgrade`.
- From [Releases](https://github.com/amagomori/trident/releases), download
  `Trident-<version>-macos-universal.dmg` and `SHA256SUMS` into `~/Downloads`.

Builds are unsigned and not notarized. For a browser download, replace `<version>` below and
compare `shasum -a 256 "$HOME/Downloads/Trident-<version>-macos-universal.dmg"` with the DMG's
entry in `SHA256SUMS`. If they match, run this in Terminal before opening the DMG:

```sh
xattr -d com.apple.quarantine "$HOME/Downloads/Trident-<version>-macos-universal.dmg"
```

Then open the DMG and drag Trident to Applications. Before opening Trident the first time, run
`xattr -dr com.apple.quarantine /Applications/Trident.app`. These steps apply only to this
download and app. The Homebrew cask handles quarantine for you.

### Linux

Download the package for your distribution from
[Releases](https://github.com/amagomori/trident/releases):

- On Arch Linux, install [`trident-bin`](https://aur.archlinux.org/packages/trident-bin) from the
  AUR, for example `yay -S trident-bin`.
- On Debian and Ubuntu, run `sudo apt install ./Trident-<version>-linux-amd64.deb`. To upgrade,
  install a newer .deb the same way.
- On other distributions with glibc 2.35 or later, run `Trident-<version>-x86_64.AppImage` after
  `chmod +x`, or unpack the tarball into `~/.local`:
  `tar xzf Trident-<version>-linux-x86_64.tar.gz -C ~/.local --strip-components=1`.
  The app menu entry runs `trident` from your login `PATH`, so if `~/.local/bin` wasn't on it,
  log out and back in.

To check a download, run `sha256sum -c SHA256SUMS` with the release's `SHA256SUMS` file.

### Build from source

You need a [Rust toolchain](https://rustup.rs) 1.90 or later and a graphics driver. On Linux, you
also need Mesa or a vendor Vulkan or GL driver, and an X11 or Wayland session.

```sh
git clone https://github.com/amagomori/trident
cd trident
cargo run --release
```

To install the `trident` binary into `~/.cargo/bin`, run `cargo install --path crates/term-app`.

## Quick start

```sh
trident                        # your login shell
trident -e htop                # run a command instead
trident show photo.png         # view an image or file inline
trident --install-terminfo     # optional: install the xterm-trident terminfo entry
```

## Features

- Trident renders with [wgpu](https://wgpu.rs) through Metal on macOS or Vulkan or OpenGL on Linux.
  It prefers the integrated GPU and uses no CPU or GPU while idle.
- It supports xterm features including truecolor, mouse reporting, alternate screen, bracketed paste
  and wide characters. It runs vim, tmux, htop and less.
- Tabs support scrollback search and selection, including rectangular selection and the Linux primary
  selection.
- Shell integration through OSC 133 lets you jump between prompts and marks output that changed
  since the last run.
- You can display inline images with kitty graphics or iTerm2’s `imgcat`, receive desktop notifications
  and drop files.
- Trident bundles JetBrains Mono and supports color emoji and font fallback.
- Configuration reloads when you save, and bundled themes can follow the OS appearance.

## Configuration

Trident reads `~/.config/trident/config.toml`, or `$XDG_CONFIG_HOME/trident/config.toml` when set. Every
entry is optional. For all settings, see [Configuration](docs/configuration.md).

## Documentation

- [Usage](docs/usage.md) covers commands, shortcuts and everyday tasks.
- [Configuration](docs/configuration.md) describes settings, key bindings, shell integration and themes.
- [Development](docs/development.md) covers building, testing, releases and performance measurement.
- [Conformance](docs/conformance.md) lists vttest and esctest results.

## License

Trident uses the MIT license. The bundled JetBrains Mono fonts use the SIL Open Font License 1.1
(`crates/term-render/assets/fonts/OFL.txt`).
