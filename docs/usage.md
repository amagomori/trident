# Usage

## Command line

```
trident [OPTIONS] [-e COMMAND [ARGS...]]

  --config <PATH>     Override the TOML config file location
  --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)
  --font-size <SIZE>  Font size in points (default: 14)
  --scrollback <LINES> History lines (default: 10000; 0 disables; maximum: 1000000)
  --option-as-alt     On macOS, make Option act as Alt/Meta
  --copy-on-select    Copy selected text to the clipboard on mouse release
  -e, --command ...   Run this command instead of your shell
  --install-terminfo  Install the xterm-trident terminfo entry with tic
```

## Terminfo

It starts your login shell (`$SHELL`) with `TERM=xterm-256color`. Trident also
ships an `xterm-trident` terminfo entry
([crates/term-app/terminfo/xterm-trident.terminfo](../crates/term-app/terminfo/xterm-trident.terminfo))
that extends xterm-256color with truecolor (`Tc`, `setrgbf`/`setrgbb`), styled
underlines (`Smulx`), bracketed paste (`BE`/`BD`/`PS`/`PE`), synchronized
output (`Sync`) and the window title (`tsl`/`fsl`), and drops the xterm
features Trident lacks. To use it, install it once with
`trident --install-terminfo` (or `/usr/bin/tic -x` on that file) and set
`term = "xterm-trident"` in the config. The pre-rename name `xterm-terminal`
stays an alias, and installing over an entry compiled under that name replaces it.
Trident then sets that TERM when the
system ncurses (`/usr/bin/infocmp` when present) finds the entry, and falls back
to `xterm-256color` when it does not. On macOS, install with `/usr/bin/tic`: an
entry compiled by Homebrew's ncurses can't be read by the system tools, so
Trident falls back. ssh passes TERM to the remote host, where curses programs
fail unless the entry is installed there too; copy it with
`infocmp -x xterm-trident | ssh host -- tic -x /dev/stdin`.

## Keyboard and mouse

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

## Pasting

When the running program has not enabled bracketed paste, a pasted line break acts as
Enter and runs the command at once. Such pastes, and pastes containing other control
characters (tab excepted), first show a banner: Enter pastes, Escape cancels, and other
keys are ignored while it is shown. With bracketed paste on, text pastes directly with
every ESC stripped. Set `paste_protection = false` to always paste immediately.

Programs can copy to the clipboard (`c`) or, on Linux, the primary selection (`p`) with
OSC 52, which also works through ssh and tmux (`set -g set-clipboard on`), for example
`printf '\e]52;c;%s\a' "$(printf hello | base64)"`. Reading the clipboard back with an
`OSC 52 ; c ; ?` query is off by default because any program, including one on a remote
host, could read what you copied; set `clipboard_read = true` to answer such queries.
On X11 a copy larger than the X server's maximum request size (about 16 MiB with
BIG-REQUESTS) is ignored and the clipboard keeps its previous contents.

Closing the window at an idle shell prompt quits at once. While another program holds the
Trident's foreground (vim, ssh, a build), a banner names it and asks first: Enter (or a
second close) closes the window, Escape keeps it, and other keys are ignored while it is
shown. A command started with `-e` (`trident -e vim`, `trident -e sh -c make`) counts as a
running program too, unless it is a shell at its prompt. On macOS, Cmd-W and every app quit
(Cmd-Q, the Quit menu, Dock > Quit, logging out) ask the same way. Set `confirm_close = false`
to always close immediately.

## Desktop notifications

Programs can raise a desktop notification with OSC 9 (`printf '\e]9;Build done\a'`), OSC 777
(`printf '\e]777;notify;Title;Body\a'`), or the kitty OSC 99 protocol (title and body
parts, chunking with `d=0`, base64 with `e=1`, urgency `u=0..2`, occasion `o=always`,
`unfocused` or `invisible`, `p=?` capability queries, and `i=` ids: a notification with the
same id replaces the earlier one; on macOS only when run from an app bundle, since a bare binary
posts through `osascript`, which can't replace). Notifications are shown in the order they were sent;
`invisible` counts a window as hidden when it is minimized, occluded, or on macOS fully covered by
other apps' windows. ConEmu's numeric OSC 9 commands
(`9;4;…` progress and the like) are not notifications. At most five are shown per ten
seconds; title and body are capped at 1024 characters and shown as plain text. Set
`desktop_notifications = false` to ignore them.

## App icons

`app_icon` picks the Dock icon on macOS and the window icon on Linux from eight variants:
`tide` (default), `lagoon`, `dusk`, `reef`, `kelp`, `midnight`, `golden` and `sunset`
(case-insensitive). On macOS the same icon also shows as a menu bar item whose menu brings the
window forward or quits; set `menu_bar_icon = false` to hide it. Both apply on config reload. An
unknown name logs a warning and uses `tide` instead of rejecting the file.

Each variant ships as `assets/icons/<name>.icns` (16 to 1024 px, 1x and 2x, the artwork on
Apple's 824/1024 icon grid) and 18 pt menu bar images at 1x and 2x
(`<name>-menubar.png`, `<name>-menubar@2x.png`). After editing a source image in `assets/`,
regenerate them with `python3 assets/generate_icons.py` (needs Pillow).

## Dropping files

Drop a local file from your file manager onto the window to insert its POSIX-shell-quoted
path plus a space, without executing it. Images supported by `trident show` (including
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
event via AppKit. On a Wayland desktop, running `env -u WAYLAND_DISPLAY trident`
selects XWayland; drops from X11/XWayland file managers are supported there. Drops
from native Wayland apps are not supported yet, even into an XWayland terminal
(verified with native Wayland Dolphin under Sway). A native Wayland terminal cannot
receive file drops through winit 0.30 either.

## Viewing files

```
trident show [--width N] [--height N] [--fit] FILE...

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

Inside tmux, `trident show` sends images through tmux's passthrough. tmux 3.3 and
later drop passthrough unless `allow-passthrough` is on, so, like `kitten icat`,
`trident show` turns it on for the pane it runs in. To have it on everywhere, add this
to `~/.tmux.conf`:

```
set -g allow-passthrough on
```
