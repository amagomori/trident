# terminal

A fast GPU-rendered terminal emulator written from scratch in Rust, for macOS and Linux.

- **GPU rendering** with [wgpu](https://wgpu.rs): Metal on macOS, Vulkan or OpenGL on Linux.
  Every frame is one instanced draw call of cell backgrounds and glyphs from a texture atlas.
- **Easy on low-end hardware**: it asks for the low-power (integrated) GPU, so dual-GPU
  MacBooks stay on the integrated one, and only redraws when something on screen changes.
  Idle, it uses no CPU or GPU.
- **xterm-compatible**: 256 colors and truecolor, bold/italic/underline/strikethrough/dim,
  alternate screen, scroll regions, bracketed paste, focus events, mouse reporting (X10,
  click, drag and motion tracking; SGR and legacy encodings), window titles, and wide (CJK)
  characters. Runs vim, tmux, htop, less and friends.
- **Configurable scrollback** (10,000 lines by default), pixel-smooth trackpad scrolling,
  mouse selection (double-click for words, triple-click for
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

  --config <PATH>     Override the TOML config file location
  --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)
  --font-size <SIZE>  Font size in points (default: 14)
  --scrollback <LINES> History lines (default: 10000; 0 disables; maximum: 1000000)
  --option-as-alt     On macOS, make Option act as Alt/Meta
  --copy-on-select    Copy selected text to the clipboard on mouse release
  -e, --command ...   Run this command instead of your shell
```

It starts your login shell (`$SHELL`) with `TERM=xterm-256color`.

| Action | macOS | Linux |
| --- | --- | --- |
| Copy selection | Cmd+C | Ctrl+Shift+C (selecting also sets the primary selection) |
| Search scrollback | Cmd+F | Ctrl+Shift+F |
| Select a rectangle | Option-drag | Alt-drag |
| Extend the current selection | Shift-click or Shift-drag | same |
| Paste | Cmd+V | Ctrl+Shift+V, Shift+Insert, middle click |
| New window | Cmd+N | Ctrl+Shift+N |
| Scroll one line up / down | Cmd+Option+Up/Down | Ctrl+Alt+Shift+Up/Down |
| Bigger / smaller / reset font | Cmd+= / Cmd+- / Cmd+0 | Ctrl+Shift+= / Ctrl+Shift+- / Ctrl+Shift+0 |
| Scroll back | Shift+PageUp/PageDown, Shift+Home/End, mouse wheel | same |
| Clear scrollback | Cmd+K | — |

Search updates as you type across scrollback and the active screen, joining soft wraps.
It ignores case unless the query contains an uppercase character. Enter or Up moves
to the next older match; Shift+Enter or Down moves newer, wrapping at either end.
Alt+R (or Ctrl+R) toggles regular expressions. Backspace edits the query; Escape
closes search and clears highlights without changing the scroll position.

Hold Shift to select text while a program (vim, tmux, htop) is using the mouse.
For a rectangle in these programs, hold Shift+Alt (Shift+Option on macOS).
Shift-click keeps the original anchor and selection shape; without an existing
selection, it starts a normal drag. Rectangular copies keep spaces and put each
screen row on its own line, even across soft wraps. Wide characters are copied whole.

Clipboard copy-on-select is off by default; launch with `--copy-on-select` to copy
on mouse release as well as with the copy shortcut. Linux's primary selection is
always updated on release, independently of this option. Option-drag works on
macOS whether or not `--option-as-alt` is set.

### Configuration

The optional config file is `$XDG_CONFIG_HOME/terminal/config.toml`, or
`~/.config/terminal/config.toml` when XDG_CONFIG_HOME is unset (on Linux and macOS).
`--config <PATH>` overrides this location. A missing file uses defaults.
`--font`, `--font-size`, `--scrollback`, and `-e` override the corresponding config
values, including on reload.

```toml
# All entries are optional; omitted colors use Tokyo Night.
# font = "/path/to/mono.ttf" # default: bundled JetBrains Mono
font_size = 14.0            # 4..=200 points
padding = 6.0               # 0..=1000 logical pixels
scrollback = 10000          # retained lines; 0 disables history (or use the [scrollback] table below)
drop_preview = true         # false: file drops insert only the shell-quoted path
paste_protection = true     # false: never confirm multi-line or control-character pastes
confirm_close = true        # false: close the window at once even while a program runs
desktop_notifications = true # false: ignore OSC 9/777/99 notification requests
vsync = true                # false: present without waiting for vblank (lower latency, may tear)
shell = ["/bin/zsh", "-l"]   # also accepts shell = "/bin/zsh"; default: login shell

[env]                      # applied after TERM/COLORTERM; overrides inherited values
EDITOR = "vim"

[colors]                   # exactly "#rrggbb"
foreground = "#c0caf5"
background = "#1a1b26"
cursor = "#c0caf5"
selection = "#33467c"
black = "#15161e"
red = "#f7768e"
green = "#9ece6a"
yellow = "#e0af68"
blue = "#7aa2f7"
magenta = "#bb9af7"
cyan = "#7dcfff"
white = "#a9b1d6"
bright_black = "#414868"
bright_red = "#ff899d"
bright_green = "#9fe044"
bright_yellow = "#faba4a"
bright_blue = "#8db0ff"
bright_magenta = "#c7a9ff"
bright_cyan = "#a4daff"
bright_white = "#c0caf5"

[keybindings]              # layered over the platform defaults listed above
"ctrl+shift+c" = "copy"
"super+k" = "clear_scrollback"
"ctrl+shift+t" = { send = "\u001b[1;5T" }
"ctrl+shift+v" = "none"     # remove a default shortcut; pass key to the shell
```

`vsync = false` (opt-in, live-reloaded) presents frames without waiting for vblank, using
wgpu's `AutoNoVsync` present mode (Immediate, else Mailbox, else FIFO) on macOS and Linux.
It cut latency on a 120 Hz Mac, but tearing is unmeasured, so the default stays `true`.
Surfaces that only offer FIFO (e.g. OpenGL on Linux) keep vsync and log a diagnostic.

Actions: `new_window`, `search`, `copy`, `paste`, `paste_primary` (primary selection where supported),
`increase_font_size`, `decrease_font_size`, `reset_font_size`, `clear_scrollback`,
`scroll_line_up`, `scroll_line_down`, `scroll_page_up`, `scroll_page_down`,
`scroll_to_top`, `scroll_to_bottom`, `jump_to_prompt_up`, `jump_to_prompt_down`,
`select_command_output`, `none`,
or `{ send = "..." }`. Chords use `ctrl`, `shift`, `alt` (alias `option`), and
`super` (alias `cmd`), plus a character or named key such as `pageup`, `home`,
`insert`, `enter`, `escape`, `space`, arrows, or `f1` through `f35`.

To cap how much history one command can take, use the table form of `scrollback`:

```toml
[scrollback]
lines = 10000               # same as scrollback = 10000
command_budget = true       # false: plain oldest-first eviction
command_share = 0.5         # 0 < share <= 1 of `lines` one command's output may use
```

With OSC 133 marks, a command whose output outgrows its share keeps its first and last
lines and drops the middle, leaving one dim line such as `… 48,213 lines of make output
dropped`, so earlier commands stay in history. Without marks, eviction is oldest-first.
Both settings are live-reloaded.

Shell integration uses OSC 133 marks emitted by your shell. With integration enabled,
clearing at a prompt preserves the screen in scrollback (explicit scrollback clears
still wipe it). Failed commands have a thin red marker beside their prompt.
While you scroll back through a command's output, that command stays pinned in a bar at
the top with its exit status and run time; click the bar to jump to its prompt.
Cmd+Up/Down on macOS or Ctrl+Shift+Up/Down on Linux jumps between prompts.
Cmd+Shift+A on macOS or Ctrl+Shift+A on Linux selects the latest command output
starting in or above the viewport. These shortcuts can be rebound with
`jump_to_prompt_up`, `jump_to_prompt_down`, and `select_command_output`.

To enable marks, source the matching optional snippet from your interactive shell
configuration (replace `/path/to/terminal` with this checkout):

```sh
# ~/.zshrc
source /path/to/terminal/shell-integration/terminal.zsh
# ~/.bashrc
source /path/to/terminal/shell-integration/terminal.bash
# ~/.config/fish/config.fish
source /path/to/terminal/shell-integration/terminal.fish
```

Source after setting up your prompt. The snippets retain existing prompts and
hooks; don't source them if another integration already emits OSC 133. The fish
snippet skips fish 4+ because it emits these marks natively.
Bash 4.4+ is required for command-start marks via `PS0` and command completion
tracking. Older Bash (including macOS `/bin/bash` 3.2) gets prompt marks only,
not command C/D tracking. Existing `PROMPT_COMMAND` arrays are retained as arrays
on Bash 5.1+; string hooks and older Bash use a prepended string hook.

New windows launch an independent process with the same command-line options.
Line-scroll shortcuts leave the prompt-jump chords Cmd+Up/Down and
Ctrl+Shift+Up/Down available for shell integration.

Changes are watched in the file's parent directory, including atomic editor saves,
and applied after about 100 ms of quiet. If the config directory (or a symlink
target's directory) doesn't exist yet, the nearest existing ancestor is watched
instead, so creating it and the file later is picked up without a restart.
Symlinked configs are followed through every hop, including retargeted links.
Fonts, padding, colors, scrollback, and bindings apply live. An unrelated edit
preserves interactive font-size adjustments. Shell and env changes take effect at
the next launch, with a note on stderr.

Unknown keys, invalid types/values, and malformed TOML are errors with the file,
line, column, and entry printed to stderr and shown in a banner (alongside the
output-paused notice when applicable). Startup errors use defaults; reload errors
keep the last valid config. Fixing the config clears the banner.

#### Color themes

Bundled themes are `tokyo-night` (the unchanged default), `tokyo-day`,
`solarized-dark`, and `solarized-light`. Set one at the top of the config:

```toml
theme = "solarized-dark"
```

To follow the OS appearance, use `theme = "auto"` (Tokyo Night / Tokyo Day),
or select your own pair:

```toml
theme = { light = "solarized-light", dark = "solarized-dark" }
```

Automatic mode reads the initial appearance and switches the existing window
live, without restarting the shell or losing scrollback. On macOS this uses native
appearance events; on Linux (X11 and Wayland) it uses the desktop's
`org.freedesktop.portal.Settings` appearance portal. A desktop without this portal,
an unavailable appearance, or “no preference” uses the dark theme. Fixed themes
ignore OS appearance changes. No appearance listener is started for fixed themes.

Programs can follow the theme: `CSI ? 996 n` asks for the current color scheme and
gets `CSI ? 997 ; 1 n` (dark) or `CSI ? 997 ; 2 n` (light) back, judged by the
theme's background. With mode 2031 set (`CSI ? 2031 h`), the same report is sent
whenever a switch (OS appearance or config reload) changes dark to light or back.

A theme name may instead be an absolute path or a path relative to the config's
directory, for example `theme = "themes/my-theme.toml"`. Theme files have a
`[colors]` table with the same 20 keys and `"#rrggbb"` values shown above (16 ANSI
colors plus foreground, background, cursor, selection); omitted keys inherit
Tokyo Night. The files in [`themes/`](themes/) are complete examples and are also
embedded in the binary, so bundled themes need no installation. Config `[colors]`
entries override the selected theme (both palettes in automatic mode).

Both files in an automatic pair are validated before applying either. Referenced
theme files are hot-reloaded, including atomic saves, missing files created later,
and symlinks. An invalid or unreadable theme shows a diagnostic banner and keeps
the last valid palettes, just like an invalid config.

### Pasting

When the running program has not enabled bracketed paste, a pasted line break acts as
Enter and runs the command at once. Such pastes, and pastes containing other control
characters (tab excepted), first show a banner: Enter pastes, Escape cancels, and other
keys are ignored while it is shown. With bracketed paste on, text pastes directly with
every ESC stripped. Set `paste_protection = false` to always paste immediately.

Closing the window at an idle shell prompt quits at once. While another program holds the
terminal's foreground (vim, ssh, a build), a banner names it and asks first: Enter (or a
second close) closes the window, Escape keeps it, and other keys are ignored while it is
shown. A command started with `-e` (`terminal -e vim`, `terminal -e sh -c make`) counts as a
running program too, unless it is a shell at its prompt. On macOS, Cmd-W and every app quit
(Cmd-Q, the Quit menu, Dock > Quit, logging out) ask the same way. Set `confirm_close = false`
to always close immediately.

### Desktop notifications

Programs can raise a desktop notification with OSC 9 (`printf '\e]9;Build done\a'`), OSC 777
(`printf '\e]777;notify;Title;Body\a'`), or the kitty OSC 99 protocol (title and body
parts, chunking with `d=0`, base64 with `e=1`, urgency `u=0..2`, occasion `o=always`,
`unfocused` or `invisible`, `p=?` capability queries, and `i=` ids: a notification with the
same id replaces the earlier one). Notifications are shown in the order they were sent;
`invisible` counts a window as hidden when it is minimized, occluded, or on macOS fully covered by
other apps' windows. ConEmu's numeric OSC 9 commands
(`9;4;…` progress and the like) are not notifications. At most five are shown per ten
seconds; title and body are capped at 1024 characters and shown as plain text. Set
`desktop_notifications = false` to ignore them.

### Dropping files

Drop a local file from your file manager onto the window to insert its POSIX-shell-quoted
path plus a space, without executing it. Images supported by `terminal show` (including
SVG) also get a thumbnail above the current prompt, at most 32 columns by 6 rows.
Set `drop_preview = false` in the live-reloaded config for path-only drops. Text, PDFs,
unreadable/corrupt files, directories, and files over 16 MiB always get just the path.
Names containing line-editor control characters are rejected. Preview decoding is
synchronous; previews are skipped in alternate-screen/mouse-reporting
apps, and prompt relocation assumes a simple single-line shell prompt, not a TUI/tmux pane.
With a wrapped command, including Bash readline's eager wrap at the right margin, the
preview can appear between the command's rows instead of above the whole command.
The typed text and dropped path are preserved; use `drop_preview = false` for path-only
drops without this layout limitation.

Linux X11 uses winit's Xdnd file-drop event. macOS Finder uses the same `DroppedFile`
event via AppKit. On a Wayland desktop, running `env -u WAYLAND_DISPLAY terminal`
selects XWayland; drops from X11/XWayland file managers are supported there. Drops
from native Wayland apps are not supported yet, even into an XWayland terminal
(verified with native Wayland Dolphin under Sway). A native Wayland terminal cannot
receive file drops through winit 0.30 either.

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

### Comparing against other terminals

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

## License

MIT. The bundled JetBrains Mono fonts are under the SIL Open Font License 1.1
(`crates/term-render/assets/fonts/OFL.txt`).
