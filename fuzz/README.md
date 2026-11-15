# Fuzzing

`terminal` feeds arbitrary bytes through `term_core::Terminal` (the parser and the grid),
mixed with resizes, scrolling, selections and searches, and fails on any panic, broken
grid invariant or heap growth past the budget in `crates/term-core/tests/harness/mod.rs`.

```sh
cargo install cargo-fuzz
cd fuzz
mkdir -p corpus/terminal && cp ../crates/term-core/tests/fuzz/* corpus/terminal/
cargo +nightly fuzz run terminal corpus/terminal -- -dict=terminal.dict -max_total_time=300 -max_len=16384
```

The `Fuzz` workflow runs this nightly for five minutes and keeps its corpus in the Actions
cache. To keep a crash as a regression test, copy the input from `artifacts/terminal/` into
`crates/term-core/tests/fuzz/`; `cargo test -p term-core --test fuzz` replays that folder
on stable.
