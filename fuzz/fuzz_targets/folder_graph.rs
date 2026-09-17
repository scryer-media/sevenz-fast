//! The coder graph, over arbitrary folder bytes.
//!
//! The same entry point as the `header` target, with the fuzzer's bytes placed
//! where the folder definitions go: coder stream counts, bind pairs and packed
//! stream indices, which are the numbers the decode stack is built out of.
#![no_main]

mod common;

use libfuzzer_sys::fuzz_target;

#[global_allocator]
static ALLOC: common::CountingAllocator = common::CountingAllocator;

const K_END: u8 = 0x00;
const K_HEADER: u8 = 0x01;
const K_MAIN_STREAMS_INFO: u8 = 0x04;
const K_UNPACK_INFO: u8 = 0x07;
const K_FOLDER: u8 = 0x0B;

fuzz_target!(|data: &[u8]| {
    let mut header = vec![K_HEADER, K_MAIN_STREAMS_INFO, K_UNPACK_INFO, K_FOLDER];
    header.extend_from_slice(data);
    header.push(K_END);
    common::exercise(common::wrap(&header));
});
