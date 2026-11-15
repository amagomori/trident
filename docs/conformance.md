# Conformance: vttest and esctest

Trident's terminal core (`term-core`) is checked against
[vttest](https://invisible-island.net/vttest/) and
[esctest2](https://github.com/ThomasDickey/esctest2) with the `vtrun` example, which runs a
program on a pseudo-terminal with `term-core` as the terminal and sends its replies back.

## Running

```sh
cargo build --release -p term-core --example vtrun
VTRUN=target/release/examples/vtrun

# esctest (from an esctest2 checkout's esctest/ directory)
$VTRUN --idle 5000 -- python3 esctest.py --expected-terminal=xterm --max-vt-level=4 \
  --timeout=0.3 --logfile=esctest.log --v=2

# vttest menu N: --key is typed once output goes idle; screens are printed to stdout
$VTRUN --idle 400 --key '1\r' --key '\r' --key '\r' --key '0\r' -- vttest
```

`vtrun` turns on DECRQCRA checksums (`Terminal::set_checksum_reports`), which esctest uses to
read the screen. The app leaves them off: they let any program writing to the terminal read
the screen back. Keys typed by `vtrun` are raw bytes, not the app's key encoder.

## esctest results

xterm expectations, VT level 4: **294 passed**, 26 xterm known bugs skipped, 230 failed.
[esctest-pass.txt](conformance/esctest-pass.txt) is the pass list;
[esctest-fail.txt](conformance/esctest-fail.txt) lists every failure under its ticket.

| Failures | Cause | Ticket |
| --- | --- | --- |
| 73 | Left/right margins (DECLRMM, DECSLRM) | [KAN-136](https://jarettforzano.atlassian.net/browse/KAN-136) |
| 46 | VT420 rectangle and column ops, character protection (DECSCA, DECSED, DECSEL) | [KAN-137](https://jarettforzano.atlassian.net/browse/KAN-137) |
| 47 | OSC 4/10-19 color spec formats, special colors (OSC 5/105) | [KAN-138](https://jarettforzano.atlassian.net/browse/KAN-138) |
| 34 | XTWINOPS window ops, title stack, DECSLPP, DECCOLM 132 columns | [KAN-139](https://jarettforzano.atlassian.net/browse/KAN-139) |
| 26 | Reverse wraparound (45), more(1) fix (41), DECSCL, remaining DECRQM/DECRQSS settings | [KAN-140](https://jarettforzano.atlassian.net/browse/KAN-140) |
| 4 | DA1/DA2: Trident reports a VT220 (`CSI ? 62 ; 22 c`, `CSI > 0 ; 10 ; 1 c`), not xterm's VT420 | intentional |

Added with KAN-19: DECRQCRA, DEC private DSRs (DECXCPR, printer, UDK, keyboard, locator,
DECMSR, DECCKSR, data integrity, sessions), DECID, ANSI DECRQM, DECRQSS (SGR, DECSTBM,
DECSCUSR), XTSAVE/XTRESTORE, DECALN homing the cursor and clearing margins, and DECSTR
resetting the saved cursor, keypad mode and character sets.

## vttest results (menus 1-6)

| Menu | Result |
| --- | --- |
| 1 Cursor movements | Pass except the 132-column screen (DECCOLM, KAN-139) |
| 2 Screen features | Pass except 132-column screens (DECCOLM, KAN-139) |
| 3 Character sets | VT100 sets (US, UK, DEC Special Graphics) pass; NRCS and VT220 sets (G2/G3, SS2/SS3) are not supported (KAN-140) |
| 4 Double-sized characters | DECDHL/DECDWL are not supported; lines render single size (not planned) |
| 5 Keyboard | Covered by term-app key encoding tests (KAN-14); `vtrun` cannot exercise the app's encoder |
| 6 Terminal reports | Pass (DA, DSR 5/6, LNM); answerback is empty |
