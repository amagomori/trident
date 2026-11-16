//! Arbitrary bytes, split across `advance` calls, perform the same actions in the vendored vte
//! (with its CSI fast path) as in upstream vte 0.15.0.
#![no_main]

use libfuzzer_sys::fuzz_target;
#[path = "../../vendor/vte/tests/recorder/mod.rs"]
mod recorder;

fuzz_target!(|raw: &[u8]| {
    let (data, cuts) = recorder::split_input(raw);
    recorder::assert_parity(data, &cuts);
});
