# Clipboard QA scripts

These scripts check that window lifetimes never disturb X11/Wayland clipboard ownership:
closing a window must neither drop a selection Trident still serves nor clobber one another
application owns. They need Python 3 with only the standard library on Linux. Run them from a
desktop session. Use `--xvfb` only for a smoke check.

| Script | Checks |
| --- | --- |
| `close_owner.py` | Selection contents after closing the window the clipboard is bound to and a window it is not bound to, under Trident ownership and under another application's, plus a fresh store after the rebind. Exits 1 on any mismatch. |

## Prerequisites

- `xdotool`, `xclip` and a running window manager in the session. For `--xvfb`, `Xvfb` and
  `openbox`.

## Run

```sh
cargo build --release -p term-app
python3 .github/scripts/clipboard-qa/close_owner.py --trident target/release/trident --xvfb
```
