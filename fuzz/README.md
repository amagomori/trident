# Fuzzing

The fuzz target feeds arbitrary bytes through `term_core::Terminal` (the parser and the grid),
mixed with resizes, scrolling, selections and searches. It fails on any panic, broken grid
invariant, or heap growth past the budget in `crates/term-core/tests/harness/mod.rs`. The target,
its corpus and its dictionary are named `terminal` in `fuzz/Cargo.toml`.

## Run

You need `cargo-fuzz` and a nightly toolchain.

```sh
cargo install cargo-fuzz
cd fuzz
mkdir -p corpus/terminal && cp ../crates/term-core/tests/fuzz/* corpus/terminal/
cargo +nightly fuzz run terminal corpus/terminal -- -dict=terminal.dict -max_total_time=300 -max_len=16384
```

The `Fuzz` workflow runs the target nightly for five minutes and keeps its corpus in the Actions
cache.

## Keep a crash as a regression test

1. Copy the crashing input from `fuzz/artifacts/terminal/` into `crates/term-core/tests/fuzz/`.
2. Run `cargo test -p term-core --test fuzz`, which replays that folder on stable.
