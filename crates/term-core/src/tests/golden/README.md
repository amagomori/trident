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
