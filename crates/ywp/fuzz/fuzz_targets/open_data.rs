//! open_data：任意字节都不能 panic，解不开时不能动重放窗口。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::crypto::DirKey;
use ywp::packet::{open_data, PathRx};

fuzz_target!(|data: &[u8]| {
    let key = DirKey::new([7u8; 32], [9u8; 12]);
    let mut rx = PathRx::new();
    let mut pkt = data.to_vec();
    let _ = open_data(&mut pkt, &key, &mut rx);
});
