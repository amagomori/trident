# Terminal conformance

Run [vttest](https://invisible-island.net/vttest/) and
[esctest2](https://github.com/ThomasDickey/esctest2) against Trident's `term-core` crate and
review the failures below. The `vtrun` example runs a program on a
pseudo-terminal with `term-core` as the terminal and sends its replies back.

## Run the suites

```sh
cargo build --release -p term-core --example vtrun
VTRUN=target/release/examples/vtrun

# esctest (from an esctest2 checkout's esctest/ directory)
$VTRUN --idle 5000 -- python3 esctest.py --expected-terminal=xterm --options allowC2Controls --max-vt-level=4 \
  --xterm-reverse-wrap=383 --timeout=0.3 --logfile=esctest.log --v=2

# vttest menu N: --key is typed once output goes idle; screens are printed to stdout
$VTRUN --idle 400 --key '1\r' --key '\r' --key '\r' --key '0\r' -- vttest
```

- `vtrun` turns on DECRQCRA checksums (`Terminal::set_checksum_reports`), which esctest uses to
  read the screen. The app leaves them off, because they let any program that writes to the
  terminal read the screen back.
- Keys that `vtrun` types are raw bytes. They don't go through the app's key encoder.

## esctest results

With xterm expectations at VT level 4 and `allowC2Controls`, 467 tests pass and 60 fail;
the suite classifies 40 as known bugs or missing options. Rectangle edits, column edits
and character protection have no unexpected failures. As in xterm, DECSED and DECSEL honor
both DEC and ISO protection, while DECSERA honors DEC protection only. The remaining failures are:

| Failures | Cause | Ticket |
| --- | --- | --- |
| 47 | OSC 4/10-19 color spec formats, special colors (OSC 5/105) | [KAN-138](https://jarettforzano.atlassian.net/browse/KAN-138) |
| 2 | DECRQSS for DECSLPP; VT level 4 honors DECNCSM instead of clearing on DECCOLM | [KAN-139](https://jarettforzano.atlassian.net/browse/KAN-139) |
| 7 | XTWINOPS window-manager ops refused by policy (table below) | intentional |
| 4 | DA1/DA2: Trident reports a VT220 (`CSI ? 62 ; 22 c`, `CSI > 0 ; 10 ; 1 c`), not xterm's VT420 | intentional |

The left/right margin cases pass, including origin-relative movement, reverse wraparound,
bounded scrolling, and margin reports. The two DECSLPP and DECNCSM failures above also occur
without horizontal margins. The suite counts DECELF/DECLFKC/DECSMKR as known xterm bugs.

## Saving and restoring text styles

An application can push its current text style with `CSI # {` (XTPUSHSGR), change
or reset the style, then pop it with `CSI # }` (XTPOPSGR). Without selectors,
the push saves all SGR attributes, including underline style and color. To save
only specific attributes, put their numbers before the `#`, separated by
semicolons: for example, `CSI 30 ; 31 # {` saves only foreground and background.

| Selector | Attribute |
| --- | --- |
| 1, 2, 3 | Bold, faint, italic, respectively |
| 4, 21 | Single/extended underline styles, double underline, respectively |
| 5, 7, 8, 9 | Blink, inverse, conceal, strike, respectively |
| 30, 31 | Foreground, background, respectively (not SGR setter numbers) |

A selective pop leaves other attributes, including underline color, alone.
Unknown selectors are ignored; even an unsupported-only list pushes an entry
so its pop cannot accidentally unwind an outer style. Neither operation moves
the cursor or changes hyperlinks or character protection.

The primary and alternate screens share one stack of ten entries. An eleventh
push discards the oldest entry; popping an empty stack does nothing. SGR 0
resets the active style without clearing the stack. A full reset (RIS) clears it.

## XTWINOPS and DECCOLM decisions

`CSI Ps t` (XTWINOPS) lets a program drive the host window manager. Trident follows xterm's
`allowWindowOps`-off posture: ops that move, minimize or restyle the real window are refused
(silently ignored), while queries and text-area resizes are honored.

| Op | Decision |
| --- | --- |
| 1-3, 5-7, 9-10 (de-iconify, iconify, move, raise, lower, refresh, maximize, fullscreen) | Refused: a program must not move, hide or restyle the user's window |
| 4, 8 (resize text area in pixels/characters) | Supported: resizes the grid; the app mirrors it on the real window with `request_inner_size` |
| 11, 13-16, 18-19 (state, position, pixel and character size reports) | Supported, answered from the window geometry the app feeds in |
| 20, 21 (icon/window title reports) | Supported |
| 22, 23 (push/pop title stack, slots 1-10) | Supported |
| >= 24 (DECSLPP, set lines) | Supported like op 8 |
| `CSI > Pm t` (XTSMTITLE hex/UTF-8 title modes) | Supported |
| `CSI ? 3 h/l` (DECCOLM 80/132 columns) | Supported only when mode 40 (Allow80To132) is set, as in xterm; the switch erases the screen unless mode 95 (DECNCSM) is set, homes the cursor and resets the scrolling region. RIS resets to 80 columns. |

## vttest results (menus 1-6)

| Menu | Result |
| --- | --- |
| 1 Cursor movements | Pass except the 132-column screen (DECCOLM, KAN-139) |
| 2 Screen features | Pass except 132-column screens (DECCOLM, KAN-139) |
| 3 Character sets | VT100 sets (US, UK, DEC Special Graphics) pass. NRCS and VT220 sets (G2/G3, SS2/SS3) are not supported. |
| 4 Double-sized characters | DECDHL/DECDWL are not supported or planned. Lines render at single size. |
| 5 Keyboard | The term-app key encoding tests cover keyboard behavior (KAN-14). The `vtrun` example cannot exercise the app's encoder. |
| 6 Terminal reports | DA, DSR 5/6 and LNM pass. Answerback is empty. |
