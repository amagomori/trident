# Terminal-state goldens

Golden tests feed raw input bytes to `term_core::Terminal` and compare the resulting state with a
checked-in `<name>.snap`. Run them with `cargo` when changing VT behavior. They don't need a display.

## Run

```sh
cargo test -p term-core golden
```

A missing snapshot or a mismatch fails the test and prints the case name and the changed lines.

## Add a case

1. Add a case to `cases()` in `../golden.rs` with a unique name, initial columns and rows, and raw
   input bytes. You can also specify a resize that runs after the input.
2. Run `UPDATE_GOLDEN=1 cargo test -p term-core golden` to write its snapshot.
3. Check the new `<name>.snap` against the expected VT behavior, then run without
   `UPDATE_GOLDEN` to confirm it passes.

Add regression cases for every VT change. If a snapshot shows wrong behavior, file the terminal
bug separately rather than committing it. Review changed snapshots the same way when updating
existing cases.

## Snapshot format

- Cursor, row and cell coordinates are zero-based. Attribute ranges are half-open.
- Row text is quoted with Rust string escaping, keeps trailing blanks and has one Unicode scalar
  per cell, including wide-character spacers.
- Foreground, background and flags for every cell are encoded as consecutive attribute runs.
- Both screens are always included, even when empty, plus primary scrollback oldest first.
- The cursor template attributes and scroll region are included for diagnosis.
- No output is normalized away.

## Split input

Each case also replays every two-chunk split of its input, and valid UTF-8 input byte by byte,
against the same snapshot. Invalid UTF-8 is split only after ASCII bytes, because the parser can
consume invalid UTF-8 differently at other boundaries.

## Shell output captures

`*-output.hex` files capture interactive zsh 5.8.1, bash 5.1.16 and fish 3.3.1 running the
repository's shell integration on an 80x24 PTY (`TERM=xterm-256color`, `LC_ALL=C.UTF-8`).

- Each record is `case|expected UTF-8 hex|PTY output hex`. The `startup` record initializes the
  terminal before the commands.
- The cases run `printf abc`, `printf 'abc\r'`, `printf 'abc\rx'`, CJK text, 180 `x` characters,
  two lines, empty output and `false`.
- The test replays each capture in whole chunks and byte by byte. It checks the copied command
  output and that exactly one command completes, with the right exit status.

These cases don't establish complete VT conformance.
