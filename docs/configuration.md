# Configuration

The optional config file is `$XDG_CONFIG_HOME/trident/config.toml`, or
`~/.config/trident/config.toml` when XDG_CONFIG_HOME is unset (on Linux and macOS).
A config at the pre-rename `terminal/config.toml` path is still read while the new one
doesn't exist.
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
clipboard_read = false      # true: let programs read the clipboard with OSC 52
confirm_close = true        # false: close the window at once even while a program runs
compare_runs = true         # false: don't mark output that changed since the last run (or use the [compare_runs] table below)
tab_bar = "always"          # Linux: "auto": only with two or more tabs; "never": hide it (tab shortcuts still work); macOS uses native tabs
desktop_notifications = true # false: ignore OSC 9/777/99 notification requests
vsync = true                # false: present without waiting for vblank (lower latency, may tear)
app_icon = "tide"           # tide, lagoon, dusk, reef, kelp, midnight, golden or sunset (see App icons in usage.md)
menu_bar_icon = true        # macOS: false hides the menu bar item
auto_secure_input = true    # macOS: false never turns on Secure Keyboard Entry at password prompts
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

`vsync = false` (opt-in, live-reloaded) presents frames without waiting for vblank, using
wgpu's `AutoNoVsync` present mode (Immediate, else Mailbox, else FIFO) on macOS and Linux.
It cut latency on a 120 Hz Mac, but tearing is unmeasured, so the default stays `true`.
Surfaces that only offer FIFO (e.g. OpenGL on Linux) keep vsync and log a diagnostic.

Actions: `new_window`, `new_tab`, `close_tab`, `next_tab`, `previous_tab`, `select_tab_1` through `select_tab_9`, `search`, `copy`, `paste`, `paste_primary` (primary selection where supported),
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

With OSC 133 marks, a command's output is compared with the last run of the same command
line: changed lines get an amber tint and a bar in the left margin, and a red tick marks
where lines were removed. Text that varies on every run is ignored, so a rerun whose only changes are
timings shows no marks while a changed test result does. The default patterns cover ISO
dates and times, clock and elapsed times (`07:02:11`), durations (`0.42s`, `12 ms`,
`1m3s`), PIDs (`pid 4242`, `process 31337`, a `[1] 12345` job notice) and temp paths
(`/tmp/…`, `/var/folders/…`, `/var/tmp/…`, `/dev/shm/…`, `.tmpXXXXXX`). Add your own
with the table form of `compare_runs`:

```toml
[compare_runs]
enabled = true              # same as compare_runs = true
default_ignore = true       # false: drop the built-in patterns above
ignore = ['build #\d+', 'seed=\w+'] # extra regular expressions, matched per logical line
```

Patterns use Rust `regex` syntax; each match counts as equal to any other match, so
`seed=1` and `seed=99` compare equal. An invalid pattern is a config error naming it.
Comparison is on by default: it runs only when a command ends, on at most 20,000 output
lines, and output is never altered, only marked. All three settings are live-reloaded;
changing the patterns forgets earlier runs (marks already shown stay), so the next run of
each command is compared from then on.

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

The snippets also report the current directory with OSC 7 at each prompt, so a new
window starts in the directory of the window it was opened from. Any shell or program
that sends `OSC 7 ; file://host/path` works the same way; directories on other hosts
(over ssh) and ones that no longer exist are ignored.

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

## Color themes

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
Tokyo Night. The files in [`themes/`](../themes/) are complete examples and are also
embedded in the binary, so bundled themes need no installation. Config `[colors]`
entries override the selected theme (both palettes in automatic mode).

Both files in an automatic pair are validated before applying either. Referenced
theme files are hot-reloaded, including atomic saves, missing files created later,
and symlinks. An invalid or unreadable theme shows a diagnostic banner and keeps
the last valid palettes, just like an invalid config.
