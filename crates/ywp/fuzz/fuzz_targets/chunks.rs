//! 块解析：第一遍校验通过的明文，第二遍逐块解码不能 panic。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::wire::{validate_chunks, ChunkIter};

fuzz_target!(|data: &[u8]| {
    if validate_chunks(data).is_ok() {
        for c in ChunkIter::new(data) {
            let _ = c;
        }
    }
});
