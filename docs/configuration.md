# Configuration

This page is the reference for Trident's config file: every setting, key bindings, scrollback,
shell integration, live reload and color themes. For keys and everyday use, see
[Usage](usage.md).

## Config file

Trident reads `$XDG_CONFIG_HOME/trident/config.toml`, or `~/.config/trident/config.toml` if
`XDG_CONFIG_HOME` is unset, on both Linux and macOS. The file is optional, and so is every entry
in it.

- `--config <PATH>` reads another file instead.
- `--font`, `--font-size`, `--scrollback` and `-e` override their config values, including after
  a reload.
- If only an old `terminal/config.toml` exists, Trident reads that.

This example shows every setting with its default or an example value:

```toml
# All entries are optional; omitted colors use Tokyo Night.
# font = "/path/to/mono.ttf" # default: bundled JetBrains Mono
font_size = 14.0            # 4..=200 points
padding = 6.0               # 0..=1000 logical pixels
# window_columns = 100      # first window's width in cells; default: last closed window's, or 100
# window_rows = 30          # first window's height in cells; default: last closed window's, or 30
remember_window_size = true # false: always open at window_columns x window_rows
scrollback = 10000          # retained lines; 0 disables history (or use the [scrollback] table below)
drop_preview = true         # false: file drops insert only the shell-quoted path
paste_protection = true     # false: never confirm multi-line or control-character pastes
clipboard_read = false      # true: let programs read the clipboard with OSC 52
confirm_close = true        # false: close the window at once even while a program runs
visual_bell = false         # true: briefly flash the grid on BEL (background tabs always get a bell mark)
compare_runs = true         # false: don't mark output that changed since the last run (or use the [compare_runs] table below)
tab_bar = "always"          # Linux: "auto": only with two or more tabs; "never": hide it (tab shortcuts still work); macOS uses native tabs
desktop_notifications = true # false: ignore OSC 9/777/99 notification requests
vsync = true                # false: present without waiting for vblank (lower latency, may tear)
app_icon = "tide"           # tide, lagoon, dusk, reef, kelp, midnight, golden or sunset (see App icons in usage.md)
menu_bar_icon = true        # macOS: false hides the menu bar item
auto_secure_input = true    # macOS: false never turns on Secure Keyboard Entry at password prompts
hide_mouse_when_typing = true # false: keep the mouse pointer shown while you type
shell = ["/bin/zsh", "-l"]   # also accepts shell = "/bin/zsh"; default: login shell
term = "xterm-256color"     # "xterm-trident" once installed (falls back if not found)

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

[keybindings]              # layered over the platform defaults in usage.md
"ctrl+shift+c" = "copy"
"super+k" = "clear_scrollback"
"ctrl+shift+t" = { send = "\u001b[1;5T" }
"ctrl+shift+v" = "none"     # remove a default shortcut; pass key to the shell
```

`hide_mouse_when_typing` hides the mouse pointer over the window when a key press sends input to
the shell or the running program, including committed IME text and `{ send = "..." }` bindings.
Moving the mouse, clicking, scrolling or switching focus shows it again. Modifier keys and
shortcuts such as copy, tab switching and font size don't hide it, and mouse reporting to programs
works as before.

`window_columns` and `window_rows` set the first window's size in cells (1..=10000, then kept to
at least 20 by 5 and to what fits on the screen). Setting either one opens at that size every time,
with 100 or 30 for the other. Otherwise, with `remember_window_size = true`, the size in cells and
the maximized state of the last window to close are saved to `$XDG_STATE_HOME/trident/window.toml`
(`~/.local/state/trident/window.toml` when unset; `~/Library/Application Support/Trident/window.toml`
on macOS) and reused at the next launch. A missing or unreadable file means 100 by 30. Windows
opened with Ctrl+Shift+N (Cmd+N on macOS) take the size of the window they were opened from.

`vsync = false` shows frames without waiting for the display refresh. This can lower latency but
can cause tearing. Where the graphics driver can't turn vsync off, such as OpenGL on Linux, vsync
stays on and Trident logs a note.

## Key bindings

`[keybindings]` entries are added on top of the platform defaults listed in
[Usage](usage.md#keyboard-and-mouse).

A key chord combines `ctrl`, `shift`, `alt` (or `option`) and `super` (or `cmd`) with a
character or a named key, such as `pageup`, `home`, `insert`, `enter`, `escape`, `space`, an
arrow, or `f1` through `f35`.

A chord maps to one of these:

| Kind | Actions |
| --- | --- |
| Windows and tabs | `new_window`, `new_tab`, `close_tab`, `next_tab`, `previous_tab`, `select_tab_1` through `select_tab_9`, `toggle_fullscreen` |
| Clipboard and search | `copy`, `paste`, `paste_primary` (primary selection, where supported), `search` |
| Font size | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| Scrolling | `scroll_line_up`, `scroll_line_down`, `scroll_page_up`, `scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`, `clear_scrollback` |
| Shell integration | `jump_to_prompt_up`, `jump_to_prompt_down`, `select_command_output` |
| Other | `none` removes a default and passes the key to the program. `{ send = "..." }` sends text. |

## Scrollback

To limit how much history one command can take, use the table form of `scrollback`:

```toml
[scrollback]
lines = 10000               # same as scrollback = 10000
command_budget = true       # false: plain oldest-first eviction
command_share = 0.5         # 0 < share <= 1 of `lines` one command's output may use
```

With [shell integration](#shell-integration), a command whose output outgrows its share keeps its
first and last lines and drops the middle. One dim line, such as `… 48,213 lines of make output
dropped`, takes its place, so earlier commands stay in history. Without shell integration, the
oldest lines go first.

## Compare runs

With [shell integration](#shell-integration), Trident compares a command's output with the last
run of the same command line:

- Changed lines get an amber tint and a bar in the left margin.
- A red tick marks where lines were removed.

Text that varies on every run is ignored, so a rerun that only changes timings shows no marks,
but a changed test result does. The built-in patterns cover dates and times, durations (`0.42s`,
`1m3s`), process IDs and temporary paths (`/tmp/…`, `/var/folders/…`). To add your own, use the
table form of `compare_runs`:

```toml
[compare_runs]
enabled = true              # same as compare_runs = true
default_ignore = true       # false: drop the built-in patterns above
ignore = ['build #\d+', 'seed=\w+'] # extra regular expressions, matched per logical line
```

- Patterns use Rust `regex` syntax. Any two matches of a pattern compare equal, so `seed=1` and
  `seed=99` count as the same.
- Comparison runs when a command ends, on up to 20,000 lines of output. Output is only marked,
  never changed.
- Changing the patterns forgets earlier runs, so each command is compared from its next run.

## Shell integration

Shell integration lets your shell mark prompts and commands (OSC 133) and report its directory
(OSC 7). With it, Trident:

- Marks failed commands with a thin red bar beside their prompt.
- Pins the command you're scrolling through in a bar at the top, with its exit status and run
  time. Click the bar to jump to its prompt.
- Jumps between prompts with Cmd+Up/Down on macOS or Ctrl+Shift+Up/Down on Linux, and selects the
  latest command output with Cmd+Shift+A or Ctrl+Shift+A. To rebind these, use
  `jump_to_prompt_up`, `jump_to_prompt_down` and `select_command_output`.
- Keeps the screen in scrollback when you clear it at a prompt. `clear_scrollback` still erases
  it.
- Opens new windows and tabs in the current directory.
- Enables the per-command scrollback limit and [compare runs](#compare-runs).

To turn it on, source the snippet for your shell from your shell config, after your prompt is set
up. Trident installs the snippets in a `shell-integration` directory:

| Install | Directory |
| --- | --- |
| macOS app | `/Applications/Trident.app/Contents/Resources/shell-integration` |
| .deb or AUR | `/usr/share/trident/shell-integration` |
| Tarball in `~/.local` | `~/.local/share/trident/shell-integration` |
| Source checkout | `shell-integration` in the checkout |

For example, with the .deb:

```sh
# ~/.zshrc
source /usr/share/trident/shell-integration/terminal.zsh
# ~/.bashrc
source /usr/share/trident/shell-integration/terminal.bash
# ~/.config/fish/config.fish
source /usr/share/trident/shell-integration/terminal.fish
```

- The snippets keep your existing prompt and hooks. Don't source them if another integration
  already sends OSC 133.
- fish 4 and later sends these marks itself, so the fish snippet does nothing there.
- Bash before 4.4, including macOS `/bin/bash` 3.2, gets prompt marks only, without command
  start and exit status.
- Any program that sends `OSC 7 ; file://host/path` sets the directory too. Directories on other
  hosts and ones that no longer exist are ignored.

## Live reload

Trident watches the config file and applies changes about 100 ms after you save, including
saves through a symlink and a file created after launch.

- Fonts, padding, colors, scrollback, key bindings and the other settings apply at once. Your
  font-size adjustments survive an unrelated edit.
- `shell` and `[env]` changes apply to the next launch, and Trident prints a note on stderr.

## Config errors

If the file has an unknown key, a wrong type or value, or malformed TOML, Trident shows the file,
line, column and entry in a banner and on stderr. An error at startup uses the defaults; an error
on reload keeps the last valid config. When you fix the file, the banner clears.

## Color themes

The bundled themes are `tokyo-night` (the default), `tokyo-day`, `solarized-dark` and
`solarized-light`. To use one, set it at the top of the config:

```toml
theme = "solarized-dark"
```

To follow the OS light or dark appearance, set `theme = "auto"` for Tokyo Night and Tokyo Day, or
pick your own pair:

```toml
theme = { light = "solarized-light", dark = "solarized-dark" }
```

Trident switches open windows when the appearance changes, without restarting the shell. On
Linux, this needs a desktop that provides the `org.freedesktop.portal.Settings` portal; without
it, Trident uses the dark theme.

Programs can ask which scheme is active with `CSI ? 996 n`. Trident replies `CSI ? 997 ; 1 n` for
dark or `CSI ? 997 ; 2 n` for light. A program that sets mode 2031 (`CSI ? 2031 h`) gets the same
report whenever the scheme changes.

### Optional: Your own theme

A theme can also be a path to a TOML file, absolute or relative to the config's directory, for
example `theme = "themes/my-theme.toml"`.

- The file has a `[colors]` table with the same 20 keys as the config's `[colors]`. Omitted keys
  use Tokyo Night.
- The files in [`themes/`](../themes/) are complete examples.
- `[colors]` in the config overrides the theme, both themes of an automatic pair included.
- Theme files reload like the config. An invalid theme shows a banner and keeps the last valid
  colors.
