# terminal

A fast GPU-rendered terminal emulator written from scratch in Rust, for macOS and Linux.

- **GPU rendering** with [wgpu](https://wgpu.rs): Metal on macOS, Vulkan or OpenGL on Linux.
  Every frame is one instanced draw call of cell backgrounds and glyphs from a texture atlas.
- **Easy on low-end hardware**: it asks for the low-power (integrated) GPU, so dual-GPU
  MacBooks stay on the integrated one, and only redraws when something on screen changes.
  Idle, it uses no CPU or GPU.
- **xterm-compatible**: 256 colors and truecolor, bold/italic/underline/strikethrough/dim,
  alternate screen, scroll regions, bracketed paste, focus events, mouse reporting (SGR and
  legacy), window titles, and wide (CJK) characters. Runs vim, tmux, htop, less and friends.
- **Scrollback** of 10,000 lines, mouse selection (double-click for words, triple-click for
  lines), and clipboard support, including the primary selection on Linux.
- Bundled [JetBrains Mono](https://www.jetbrains.com/lp/mono/), with fallback to system fonts
  (found through fontconfig on Linux, Core Text on macOS) for symbols and CJK, and color emoji
  (COLR, sbix and CBDT fonts such as Apple Color Emoji and Noto Color Emoji).

## Install and run

You need a [Rust toolchain](https://rustup.rs) (1.90 or newer).

```sh
git clone https://github.com/amagomori/terminal
cd terminal
cargo run --release
```

Or install the `terminal` binary into `~/.cargo/bin`:

```sh
cargo install --path crates/term-app
```

No system libraries are needed beyond a graphics driver (on Linux: Mesa or a vendor Vulkan/GL
driver, and an X11 or Wayland session).

## Usage

```
terminal [OPTIONS] [-e COMMAND [ARGS...]]

  --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)
  --font-size <SIZE>  Font size in points (default: 14)
  --option-as-alt     On macOS, make Option act as Alt/Meta
  -e, --command ...   Run this command instead of your shell
```

It starts your login shell (`$SHELL`) with `TERM=xterm-256color`.

| Action | macOS | Linux |
| --- | --- | --- |
| Copy selection | Cmd+C | Ctrl+Shift+C (selecting also sets the primary selection) |
| Paste | Cmd+V | Ctrl+Shift+V, Shift+Insert, middle click |
| Bigger / smaller / reset font | Cmd+= / Cmd+- / Cmd+0 | Ctrl+Shift+= / Ctrl+Shift+- / Ctrl+Shift+0 |
| Scroll back | Shift+PageUp/PageDown, Shift+Home/End, mouse wheel | same |
| Clear scrollback | Cmd+K | — |

Hold Shift to select text while a program (vim, tmux, htop) is using the mouse.

## Layout

A Cargo workspace of three crates:

| Crate | What it does |
| --- | --- |
| `crates/term-core` | Terminal state: grid, scrollback, cursor, modes, selection, colors, and the VT/xterm escape sequence handling (parsed with [`vte`](https://crates.io/crates/vte)). No GPU or window dependencies, so it builds and tests headless |
| `crates/term-render` | wgpu renderer (`renderer.rs`, `shader.wgsl`) that draws a frame as one instanced pass, plus glyph rasterization ([`ab_glyph`](https://crates.io/crates/ab_glyph)), system font fallback (`fallback.rs`), color glyphs (`color.rs`) and the GPU glyph atlases. Takes any window with raw window and display handles |
| `crates/term-app` | The `terminal` binary: window and event loop ([`winit`](https://crates.io/crates/winit)), the shell on a pseudo-terminal ([`portable-pty`](https://crates.io/crates/portable-pty)), key and mouse encoding, and the clipboard |

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt
```

CI runs these on Linux and macOS for every pull request.

## License

MIT. The bundled JetBrains Mono fonts are under the SIL Open Font License 1.1
(`crates/term-render/assets/fonts/OFL.txt`).
