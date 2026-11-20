# Frequently asked questions

Use these answers to troubleshoot configuration, shell integration and terminal input.

## Where do I put my settings?

Create `~/.config/trident/config.toml` on Linux or macOS. If you set `XDG_CONFIG_HOME`,
use `$XDG_CONFIG_HOME/trident/config.toml` instead. Start with the
[example config](configuration.md#config-file) and look up individual settings in the
[configuration reference](config-reference.md).

## Why didn’t a saved change take effect?

Check the error banner first. If Trident rejects an edit, it keeps the last valid config.
If you started Trident with `--config`, edit that file. Command-line options such as
`--font-size` take precedence on reload. For changes to `shell`, `term` or `[env]`,
launch Trident again.

## Why does an arrow appear as two characters?

For operators such as `->`, set `ligatures = true` at the top level of the config and
move the cursor outside the operator. Try the bundled JetBrains Mono if your font has no
programming ligatures. See [Fonts](configuration.md#fonts).

## Why doesn’t a new tab open in my current directory?

New tabs and windows use the directory reported by the shell. Without a report,
Trident checks the directory of a recognized local shell if it still owns the
foreground. It does not use a running job's directory or guess a remote path.
For reliable directory reporting, enable [shell integration](configuration.md#shell-integration).

## Why does a program report an unknown terminal over SSH?

SSH forwards `TERM`, but not the matching terminfo entry. If you use `xterm-trident`,
[install its entry on the remote host](usage.md#terminfo) or use the default
`xterm-256color` instead.

## How do I select text when an app uses the mouse?

Hold Shift while dragging to select in apps such as vim or tmux. Then press Cmd+C on
macOS or Ctrl+Shift+C on Linux to copy. See [Selection](usage.md#selection) for
rectangular selections and the Linux primary selection.
