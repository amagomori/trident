//! Arbitrary bytes through the parser and grid: no panics, and the heap the terminal holds
//! stays within `term_core::fuzz`'s budget for its grid, scrollback and input.
#![no_main]

use libfuzzer_sys::fuzz_target;
use term_core::fuzz::CountingAlloc;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fuzz_target!(|data: &[u8]| {
    let base = CountingAlloc::live();
    term_core::fuzz::run_with(data, |_, budget| {
        let held = CountingAlloc::live().saturating_sub(base);
        assert!(held <= budget, "terminal holds {held} bytes, budget {budget}");
    });
});
