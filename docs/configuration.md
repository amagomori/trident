# Configuration

Configure Trident's appearance, key bindings, scrollback and shell integration. For everyday tasks, see
[Usage](usage.md).

## Config file

Trident reads `$XDG_CONFIG_HOME/trident/config.toml`, or `~/.config/trident/config.toml` if
`XDG_CONFIG_HOME` is unset, on both Linux and macOS. The file and its entries are optional.

- `--config <PATH>` reads another file instead.
- `--font`, `--font-size`, `--scrollback` and `-e` override their config values, including after
  a reload.
- If only an old `terminal/config.toml` exists, Trident reads that.

This example shows every setting with its default or an example value:

```toml
# All entries are optional; omitted colors use Tokyo Night.
# font = "Fira Code"       # family name or font file; default: bundled JetBrains Mono
# font_bold = "/path/to/mono-bold.ttf" # optional per-style faces: family names or files
# font_italic = "Fira Code"
# font_bold_italic = "Fira Code"
font_size = 14.0            # 4..=200 points
padding = 6.0               # 0..=1000 logical pixels
# window_columns = 100      # first window's width in cells; default: last closed window's, or 100
# window_rows = 30          # first window's height in cells; default: last closed window's, or 30
remember_window_size = true # false: always open at window_columns x window_rows
scrollback = 10000          # retained lines; 0 disables history (or use the [scrollback] table below)
drop_preview = true         # false: file drops insert only the shell-quoted path
hover_preview = false       # true: hold Ctrl (Cmd on macOS) over a file path to preview it
paste_protection = true     # false: never confirm multi-line or control-character pastes
trim_paste = true           # false: keep trailing whitespace and newlines on single-line pastes
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
pager = ["less", "-R", "+G"] # opens the last command's output (file path appended); also accepts a string
term = "xterm-256color"     # "xterm-trident" once installed (falls back if not found)
editor = ["code", "--goto", "{file}:{line}:{column}"] # path:line links; default: $VISUAL/$EDITOR (see usage.md)

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
shortcuts such as copy, tab switching and font size don't hide it.

`window_columns` and `window_rows` accept 1 to 10,000 cells. Trident limits the window to at least
20 by 5 cells and to what fits on the screen. Setting either value opens at that size every time,
with 100 columns or 30 rows for the other. Otherwise, `remember_window_size = true` restores the
last closed window's size and maximized state.

On Linux, Trident saves these to
`$XDG_STATE_HOME/trident/window.toml`, defaulting to `~/.local/state/trident/window.toml`.
On macOS, it uses `~/Library/Application Support/Trident/window.toml`.
A missing or unreadable file means 100 by 30. Windows opened with Ctrl+Shift+N (Cmd+N on macOS)
take the size of the window they were opened from.

`vsync = false` shows frames without waiting for the display refresh. This can lower latency but
can cause tearing. Where the graphics driver can't turn vsync off, such as OpenGL on Linux, vsync
stays on and Trident logs a note.

## Fonts

`font` takes a family name, such as `"Fira Code"` or `"Menlo"`, or the path to a
TrueType/OpenType file or `.ttc` collection. A value that contains a `/` or ends in `.ttf`,
`.otf`, `.ttc` or `.otc` is a path; anything else is a family name, looked up with fontconfig on
Linux and Core Text on macOS (a full face name such as `"Fira Code Retina"` works too).

- Bold, italic and bold-italic text use the family's own faces, picked by weight and slant
  (the faces in a collection file count as one family).
- A style the font has no face for is synthesized: bold is drawn heavier and italic is slanted,
  rather than drawn as regular text.
- `font_bold`, `font_italic` and `font_bold_italic` choose a style's face from another family or
  file. They are ignored when `--font` is given.
- A family that is not installed is a warning naming it, and Trident uses the bundled
  JetBrains Mono; a missing style override falls back to the style from `font`. A font file that
  cannot be read is a [config error](#config-errors).
- Font changes apply on [live reload](#live-reload), at the current font size.

## Key bindings

`[keybindings]` entries are added on top of the platform defaults listed in
[Usage](usage.md#keyboard-and-mouse).

A key chord combines `ctrl`, `shift`, `alt` (or `option`) and `super` (or `cmd`) with a
character or a named key, such as `pageup`, `home`, `insert`, `enter`, `escape`, `space`, an
arrow, or `f1` through `f35`.

A chord maps to one of the following actions.

| Kind | Actions |
| --- | --- |
| Windows and tabs | `new_window`, `new_tab`, `close_tab`, `next_tab`, `previous_tab`, `select_tab_1` through `select_tab_9`, `toggle_fullscreen` |
| Clipboard and search | `copy`, `paste`, `paste_primary` (primary selection, where supported), `search` |
| Font size | `increase_font_size`, `decrease_font_size`, `reset_font_size` |
| Scrolling | `scroll_line_up`, `scroll_line_down`, `scroll_page_up`, `scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`, `clear_scrollback` |
| Shell integration | `jump_to_prompt_up`, `jump_to_prompt_down`, `select_command_output`, `show_last_command_output` |
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
first and last lines and drops the middle. A dim line such as `… 48,213 lines of make output
dropped` replaces the omitted output, keeping earlier commands in history. Without shell
integration, the oldest lines go first.

## Compare runs

With [shell integration](#shell-integration), Trident compares a command's output with the last
run of the same command line:

- Changed lines get an amber tint and a bar in the left margin.
- A red tick marks where lines were removed.

Trident ignores text that varies on every run. A rerun that only changes timings shows no marks,
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
- Opens the latest command output that isn't empty, wherever you're scrolled, in a new window
  running `pager`, with Cmd+Shift+G on macOS or Ctrl+Shift+G on Linux. Colors are kept as SGR
  sequences, so `less` needs `-R`. Quitting the pager closes the window. To rebind it, use
  `show_last_command_output`. The pager gets a UTF-8 `LC_CTYPE` when its environment names no
  locale (an app opened from Finder has none). If `pager` isn't on `PATH`, Trident shows a banner
  instead of a window, so on macOS give Homebrew pagers by full path. A pager that exits with a
  nonzero status also shows a banner in the window that opened it.
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

- The snippets keep your existing prompt and hooks. Don't combine them with another prompt
  integration, except fish's built-in marks.
- fish 4 and later supplies its own command marks unless `fish_features` includes
  `no-mark-prompt`; the fish snippet then writes them itself, as it does for fish 3. It also
  adds the input boundary and the same click-to-move adapter used by the zsh snippet.
- With these fish/zsh snippets, a prompt click queries the editor's current buffer once,
  then checks it against the screen before moving. This handles wide characters on rows
  redrawn by the shell and distinguishes real spaces from painted margin padding. The
  snippets use `base64`; restart the shell after updating them. Tabs, literal newlines,
  partially scrolled-away input and text that does not match the screen do not move.
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
line, column and entry in a banner and on stderr. At startup, Trident falls back to the defaults.
On reload, it keeps the last valid config. Fixing the file clears the banner.

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
Linux, this needs a desktop that provides the `org.freedesktop.portal.Settings` portal. Without
it, Trident uses the dark theme.

Programs can ask which scheme is active with `CSI ? 996 n`. Trident replies `CSI ? 997 ; 1 n` for
dark or `CSI ? 997 ; 2 n` for light. A program that sets mode 2031 (`CSI ? 2031 h`) gets the same
report whenever the scheme changes.

### Use your own theme

A theme can also be a path to a TOML file, absolute or relative to the config's directory, for
example `theme = "themes/my-theme.toml"`.

- The file has a `[colors]` table with the same 20 keys as the config's `[colors]`. Omitted keys
  use Tokyo Night.
- The files in [`themes/`](../themes/) are complete examples.
- `[colors]` in the config overrides the theme, both themes of an automatic pair included.
- Theme files reload like the config. An invalid theme shows a banner and keeps the last valid
  colors.
