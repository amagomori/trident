# Usage

Use Trident's commands, shortcuts, tabs, search, selection and built-in file viewer. For settings, see
[Configuration](configuration.md).

## Command line

```text
trident [OPTIONS] [-e COMMAND [ARGS...]]

  --config <PATH>     Override the TOML config file location
  --font <FONT>       Font family name or TrueType/OpenType file (default: bundled JetBrains Mono)
  --font-size <SIZE>  Font size in points (default: 14)
  --scrollback <LINES> History lines (default: 10000; 0 disables; maximum: 1000000)
  --option-as-alt     On macOS, make Option act as Alt/Meta
  --copy-on-select    Copy selected text to the clipboard on mouse release
  -e, --command ...   Run this command instead of your shell
  --install-terminfo  Install the xterm-trident terminfo entry with tic
```

## Terminfo

Trident starts your login shell (`$SHELL`) with `TERM=xterm-256color`. It also ships an
`xterm-trident` terminfo entry, which adds truecolor, styled underlines, bracketed paste,
synchronized output and the window title, and drops xterm features Trident lacks. To use it:

1. Run `trident --install-terminfo`. On macOS, this uses `/usr/bin/tic` because an entry
   compiled by Homebrew's ncurses can't be read by the system tools.
2. Set `term = "xterm-trident"` in the config.

If the system can't find the entry, Trident falls back to `xterm-256color`. The old name
`xterm-terminal` works as an alias.

ssh passes `TERM` to the remote host, so install the entry there too:

```sh
infocmp -x xterm-trident | ssh host -- tic -x /dev/stdin
```

For the full capability list, see
[xterm-trident.terminfo](../crates/term-app/terminfo/xterm-trident.terminfo).

## Keyboard and mouse

| Action | macOS | Linux |
| --- | --- | --- |
| Copy selection | Cmd+C | Ctrl+Shift+C (selecting also sets the primary selection) |
| Search scrollback | Cmd+F | Ctrl+Shift+F |
| Select a rectangle | Option-drag | Alt-drag |
| Extend the current selection | Shift-click or Shift-drag | same |
| Move the cursor in the command line | Click (with shell integration) | same |
| Paste | Cmd+V | Ctrl+Shift+V, Shift+Insert, middle click |
| New window | Cmd+N | Ctrl+Shift+N |
| New tab / close tab | Cmd+T / Cmd+W | Ctrl+Shift+T / Ctrl+Shift+W |
| Reopen the last closed tab | Cmd+Shift+T | Ctrl+Shift+Alt+T |
| Close window (every tab in it) | Cmd+Shift+W | — |
| Next / previous tab | Cmd+Shift+] / Cmd+Shift+[, Ctrl+Tab / Ctrl+Shift+Tab | Ctrl+PageDown / Ctrl+PageUp, Ctrl+Tab / Ctrl+Shift+Tab |
| Go to tab 1-8 / last tab | Cmd+1..8 / Cmd+9 | Alt+1..8 / Alt+9 |
| Open the last command's output in a pager (needs shell integration) | Cmd+Shift+G | Ctrl+Shift+G |
| Scroll one line up / down | Cmd+Option+Up/Down | Ctrl+Alt+Shift+Up/Down |
| Bigger / smaller / reset font | Cmd+= / Cmd+- / Cmd+0 | Ctrl+Shift+= / Ctrl+Shift+- / Ctrl+Shift+0 |
| Scroll back | Shift+PageUp/PageDown, Shift+Home/End, mouse wheel | same |
| Clear scrollback | Cmd+K | — |
| Toggle fullscreen | Ctrl+Cmd+F (native fullscreen, also Window > Enter Full Screen) | F11 (borderless) |

## Tabs

Every tab runs its own shell, which keeps running in the background. A new tab starts in the
active tab's directory when the shell reports it with OSC 7.

- To switch tabs, click a tab. To close one, click its **×** or middle-click it. To open one,
  click **+**.
- Closing a tab that's running a program asks first (see `confirm_close`). Closing the last tab
  closes the window.
- On Linux, to show the tab bar only while a window has two or more tabs, set `tab_bar = "auto"`.

On macOS, tabs are native window tabs, like Terminal.app's. The system shows the tab bar once a
window has two tabs, and you can drag tabs to reorder them or into their own window. The
**Window** menu has the usual tab commands, such as **Move Tab to New Window** and **Merge All
Windows**.

## Links

Hold Cmd (macOS) or Ctrl (Linux) over a link to underline it, and click to open it. Trident links:

- OSC 8 hyperlinks and `http`, `https`, `ftp`, `file` and `mailto` URLs
- File references printed by compilers, linters, test runners and grep, such as `src/main.rs:10:5`,
  `--> src/lib.rs:7:13`, `main.go:5:2:`, `Makefile:12:`, `at f (/srv/app.js:12:9)` and Python's
  `File "app.py", line 12`

- While you hold the key over an OSC 8 hyperlink, its real target shows at the bottom-left by
  default. It moves if needed to leave Find, the pinned command header and linked text readable.
  Characters outside printable ASCII show percent-encoded, and a long target is cut in the middle.
- A file reference is a link only when the file exists. Relative paths are tried against the
  shell's directory (OSC 7), then the foreground program's working directory. `~/` is your home
  directory.
- Clicking a file reference opens it at that line in `editor` from the config file, filling in its
  `{file}`, `{line}` and `{column}` arguments. If no argument names the file, it's appended.
- Without `editor`, Trident uses `$VISUAL` or `$EDITOR` (from `[env]` first) with that editor's
  own line syntax:
  - Supported editors: vim, nvim, nano, emacs, helix, micro, kakoune, VS Code, Cursor, Sublime Text,
    Zed and TextMate.
  - `ed` opens at the top of the file, and `red` starts in the file's directory because it
    restricts editing to that directory. Any other editor gets `+line file`.
  - Quoted words in the variable, such as an editor path with spaces, stay one argument.
- Terminal editors such as vim, nano and `emacs -nw` open in a new Trident window. GUI editors run
  directly.
- With no editor set at all, the file goes to `open` or `xdg-open`, which can't jump to the line.

## Search

Search updates as you type, across scrollback and the screen, and matches across soft-wrapped
lines. It ignores case unless the query has an uppercase letter.

- Enter or Up moves to the next older match. Shift+Enter or Down moves to the next newer one.
  Search wraps at either end.
- Alt+R or Ctrl+R toggles regular expressions.
- Escape closes search and clears highlights without changing the scroll position.

## Selection

- While a program such as vim, tmux or htop uses the mouse, hold Shift to select text. For a
  rectangle, hold Shift+Alt (Shift+Option on macOS).
- Shift-click extends the current selection, keeping its anchor and shape.
- Rectangular copies keep spaces and put each screen row on its own line.
- Selecting doesn't copy to the clipboard unless you launch with `--copy-on-select`. On Linux,
  selecting always sets the primary selection.

## Click to move the cursor

With shell integration, a click without a drag in the command line you're editing moves the
shell's cursor there by sending Left/Right arrow keys, as in Ghostty and kitty. Only the
soft-wrapped line holding the cursor counts, and a click past the end of the text moves to its
end. Greyed autosuggestion text (zsh-autosuggestions, fish) past the input isn't part of it, so a
click there leaves the suggestion unaccepted. Single-click movement waits briefly for a possible
double/triple-click, so selecting a word or line leaves the shell's cursor unchanged. Typing
after a single click moves first, without waiting for that interval.

## Paste

Without bracketed paste, a pasted line break runs the command at once. Trident therefore asks for
confirmation before pasting a line break or another control character other than tab.
Press Enter to paste or Escape to cancel. With bracketed paste on, text pastes directly
with every ESC removed. To always paste immediately, set `paste_protection = false`.

A single-line paste loses its trailing whitespace, so a copied command ending in a newline pastes
without the banner and without running. Pastes of several lines keep every newline. To paste
text exactly as copied, set `trim_paste = false`.

### Clipboard from programs

Programs can copy to the clipboard with OSC 52, including through ssh and tmux
(`set -g set-clipboard on`):

```sh
printf '\e]52;c;%s\a' "$(printf hello | base64)"
```

On Linux, use `p` instead of `c` for the primary selection. Programs can't read the clipboard
back unless you set `clipboard_read = true`, because any program, including one on a remote host,
could then read what you copied.

## Close confirmation

If a program such as vim, ssh or a build is running, closing the window shows a confirmation banner
with the program's name. Press Enter to close the window or Escape to keep it open.
Closing again also confirms for the last window in a Trident process. On Linux, if another window
in the same process is open, closing again repeats the prompt instead.
Commands started with `-e` also require confirmation unless the command is a shell at its prompt.
At an idle shell prompt, the window closes without confirmation.

On macOS, **Close Window** and quitting the app ask the same way. Quitting closes every window
opened with **New Window**, with a separate confirmation for each running program. To always close
immediately, set `confirm_close = false`.

## Secure Keyboard Entry

On macOS, Trident turns on Secure Keyboard Entry while a program such as sudo, ssh, passwd or
`read -s` asks for a password, so other apps can't watch your keystrokes. It's on only while the
window is focused, and it turns off when the prompt ends. It doesn't cover password prompts inside
an ssh session or other full-screen programs. To turn it off, set `auto_secure_input = false`.

## Desktop notifications

Programs can show a desktop notification with OSC 9, OSC 777 or OSC 99, the
[kitty notification protocol](https://sw.kovidgoyal.net/kitty/desktop-notifications/):

```sh
printf '\e]9;Build done\a'
printf '\e]777;notify;Title;Body\a'
```

Trident shows at most five notifications per ten seconds, as plain text up to 1,024 characters.
To ignore them, set `desktop_notifications = false`.

## Bell

A BEL (`printf '\a'`) in an unfocused window requests attention through the taskbar or Dock icon.
A bell in a background tab adds a `●` before its title on Linux or a `🔔` on macOS until you view it.
Set `visual_bell = true` to also flash the active tab's grid for about 120 ms.
A stream of bells (`yes $'\a'`) flashes once until it pauses for a quarter second.

## App icons

`app_icon` sets the Dock icon on macOS or the window icon on X11 (including XWayland).
Native Wayland uses the installed desktop icon instead. The eight variants are `tide` (the
default), `lagoon`, `dusk`, `reef`, `kelp`, `midnight`, `golden` and `sunset`, all
case-insensitive. On macOS, the same icon also shows in the menu bar, with a menu that brings
the window forward or quits. To hide it, set `menu_bar_icon = false`. Both settings apply when the
config reloads. An unknown name logs a warning and uses `tide`.

## Drop files

Drop a local file onto the window to type its shell-quoted path followed by a space. The command
doesn't run.

- Images that `trident show` can draw also get a thumbnail above the prompt, up to 32 columns by 6
  rows. To insert only the path, set `drop_preview = false`.
- Other files, directories, and files over 16 MiB get only the path.
- Full-screen programs such as vim and tmux get only the path.
- With a multi-line prompt or a long command that wraps, the thumbnail can appear in the middle of
  the command. Your typed text and the path are kept.

On Linux, drops work from X11 apps. On a Wayland desktop, run `env -u WAYLAND_DISPLAY trident` to
accept drops from X11 apps. Drops from native Wayland apps aren't supported.

## Wayland notes

Trident runs natively on Wayland and on X11 (including XWayland). On Wayland:

- When `theme` follows the OS appearance (`theme = "auto"` or a `light`/`dark`
  pair), client-side decorations and the X11 `_GTK_THEME_VARIANT` hint track the
  desktop's color scheme through the `org.freedesktop.appearance` portal. A window
  opened with a fixed theme uses the platform's default decorations. Switching from
  automatic to fixed on config reload changes the terminal palette but does not
  reset the existing window's decorations.
- Fractional scaling works; moving the window between monitors re-renders at the new
  scale.
- The window icon comes from the installed `trident.desktop` entry and icon theme, not
  the `app_icon` setting.
- Primary selection (select-to-copy, middle-click paste) uses the
  `zwp_primary_selection_v1` protocol, which GNOME, KDE and wlroots compositors
  provide. On a compositor without it, use Ctrl+Shift+C/V and the regular clipboard.
- The compositor owns window position: Trident can restore its size, not its place.
- Drops from native Wayland apps aren't supported yet (see above).

## Hover previews

With `hover_preview = true` in the config, hold Ctrl (Cmd on macOS) with the pointer over a file
path in the output to see a small card next to it:

- Images show a thumbnail, and PDFs show their first page.
- Text files show their first lines, or the lines around `path:line` when a line is given.
- Directories list their entries.

Paths are found in `ls` columns, `git status` and `git diff` output, compiler and traceback
locations, and quoted names. Ctrl-click or Cmd-click opens links. Relative paths use the directory
the shell last reported with OSC 7, falling back to the foreground program's or shell's working
directory. Trident skips previews while the shell reports a directory on another host, as over ssh.
PDF previews require `pdftoppm` from poppler-utils on Linux and use Quick Look on macOS.

## View files

```text
trident show [--width N] [--height N] [--fit] FILE...

  --width N    Draw images N columns wide
  --height N   Draw images N rows tall (with --width: fit inside both)
  --fit        Scale images up or down to fill the window
```

`trident show` displays a file in Trident, kitty, Ghostty or WezTerm. It
picks the type from the file's contents, not its name:

- Images (PNG, JPEG, GIF, WebP, BMP, TIFF, ICO, QOI, PNM, TGA and SVG) are drawn shrunk to fit
  the window. GIFs show their first frame.
- Text files are printed with control characters made visible.
- Other files get a one-line summary, such as `ELF executable, 16.2 KiB`.

A missing, unreadable or corrupt file prints an error, and the command exits with status 1. For
sample files, see `crates/term-app/samples`.

Inside tmux, `trident show` turns on `allow-passthrough` for its pane, which tmux 3.3 and later
need for images. To turn it on for every pane, add this line to `~/.tmux.conf`:

```text
set -g allow-passthrough on
```
