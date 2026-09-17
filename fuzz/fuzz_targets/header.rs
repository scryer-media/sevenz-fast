//! The header parser, over arbitrary header bytes.
//!
//! Everything the parser allocates or walks is sized by a number in these
//! bytes, so this is the target that says whether those numbers are bounded:
//! no panic, no allocation above the ceiling, no run that does not end.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: common::CountingAllocator = common::CountingAllocator;

fuzz_target!(|data: &[u8]| {
    common::exercise(common::wrap(data));
});
