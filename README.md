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

### Viewing files

```
terminal show [--width N] [--height N] [--fit] FILE...

  --width N    Draw images N columns wide
  --height N   Draw images N rows tall (with --width: fit inside both)
  --fit        Scale images up or down to fill the window
```

Run it inside the terminal (or kitty, Ghostty, WezTerm) to see a file without
another tool. The type comes from the file's contents, not its name: PNG, JPEG,
GIF (first frame), WebP, BMP, TIFF, ICO, QOI, PNM, TGA and SVG are drawn with the
kitty graphics protocol, shrunk to fit the window; text files are printed with
control characters made visible; other files get a one-line summary such as
`ELF executable, 16.2 KiB`. A missing, unreadable or corrupt file prints an error
and the command exits with status 1. Sample files live in `crates/term-app/samples`.

Inside tmux, `terminal show` sends images through tmux's passthrough. tmux 3.3 and
later drop passthrough unless `allow-passthrough` is on, so, like `kitten icat`,
`terminal show` turns it on for the pane it runs in. To have it on everywhere, add this
to `~/.tmux.conf`:

```
set -g allow-passthrough on
```

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

### Measuring latency

On Wayland with presentation feedback and a known refresh period, redraws and flood
parsing latch just before the predicted vblank, allowing for measured frame cost and
adaptive compositor slack. Other platforms and missing feedback keep immediate redraws
and the fixed flood timeout. Idle windows do not schedule frame timers.

From a Linux or macOS desktop session, run `python3 .github/scripts/latency.py
--base ../baseline --head . --runs 5`, or use
`--app target/release/terminal`. It reports per-run p50/p99/mean input-to-present
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

## License

MIT. The bundled JetBrains Mono fonts are under the SIL Open Font License 1.1
(`crates/term-render/assets/fonts/OFL.txt`).
