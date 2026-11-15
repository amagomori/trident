//! Arbitrary bytes through the parser and grid: no panics, and the heap the terminal holds
//! stays within the budget in `crates/term-core/tests/harness` for its grid, scrollback and input.
#![no_main]

use libfuzzer_sys::fuzz_target;
#[path = "../../crates/term-core/tests/harness/mod.rs"]
mod harness;

use harness::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fuzz_target!(|data: &[u8]| {
    let base = CountingAlloc::live();
    harness::run_with(data, |_, budget| {
        let held = CountingAlloc::live().saturating_sub(base);
        assert!(held <= budget, "terminal holds {held} bytes, budget {budget}");
    });
});
