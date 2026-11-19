# Clipboard QA scripts

These scripts check that window lifetimes never disturb X11 clipboard ownership:
closing a window must neither drop a selection Trident still serves nor clobber one another
application owns. Run them on a dedicated X11 display: they change focus, type into
shells and replace the display's clipboard. Use `--xvfb` for a private smoke check,
not as a substitute for a native desktop QA pass.

| Script | Checks |
| --- | --- |
| `close_owner.py` | Checks the exact surviving window of the launched PID and selection contents after each unbound and bound close, under Trident and external ownership. The external helper serves the same plain text plus a custom `text/x-qa-payload` target, so losing ownership is detectable even when the text matches. Both bound-close survivors must accept a fresh OSC 52 store. |

## Prerequisites

- `xdotool`, `xclip`, Python 3 with `python3-xlib`, and an EWMH window manager.
  For `--xvfb`, also install `xvfb-run`, `Xvfb`, `xauth` and `openbox`.

## Run

```sh
cargo build --release -p term-app
python3 .github/scripts/clipboard-qa/close_owner.py --trident target/release/trident --xvfb
```
