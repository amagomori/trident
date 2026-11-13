# Terminal-state goldens

Run: `cargo test -p term-core golden` (headless, no extra dependencies).

Update intentionally: `UPDATE_GOLDEN=1 cargo test -p term-core golden`.
Review every changed snapshot against VT behavior before committing. Normal runs
fail on missing files or mismatches and print the case name and changed lines.

Every VT ticket adds regression cases to `cases()` in `../golden.rs`: a unique
name, initial columns/rows and raw input bytes. Optional resize runs after input.
Update to create the matching `<name>.snap`, inspect it, then run without update.
Do not bless incorrect behavior; report terminal bugs separately.

Format: zero-based cursor/row/cell coordinates; attribute ranges are half-open.
Quoted row text uses Rust string escaping, preserves trailing blanks, and has
one Unicode scalar per cell (including wide spacers). Named fg/bg and flags for
every cell are encoded in consecutive attribute runs. Both screens are always
included, even when empty, as is primary scrollback in oldest-first order.
Cursor template attributes and scroll region are included for diagnostic context.

Each case also checks all two-chunk splits and valid UTF-8 inputs bytewise against
the same snapshot. Invalid UTF-8 splits are only after ASCII bytes because vte
can consume invalid UTF-8 differently at other boundaries. No output is normalized
away. The baseline cases cover basic shell-era VT behavior; extended SGR, CJK,
alternate screen, regions and resize cases protect currently implemented features,
not a claim of complete VT conformance or resize reflow support.

`*-output.hex` captures interactive zsh 5.8.1, bash 5.1.16 and fish 3.3.1
with the repository's shell snippets on an 80x24 PTY (`TERM=xterm-256color`,
`LC_ALL=C.UTF-8`). Records are `case|expected UTF-8 hex|PTY output hex`;
`startup` initializes the terminal before the commands. Cases execute `printf
abc`, `printf 'abc\r'`, `printf 'abc\rx'`, CJK, 180 x characters, two lines,
empty output and `false`. The golden test replays whole chunks and bytewise,
asserting copied command output and exactly one completion with the right status.
