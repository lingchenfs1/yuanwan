//! open_data：任意字节都不能 panic，解不开时不能动重放窗口和 largest，也不能改缓冲。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::crypto::DirKey;
use ywp::packet::{open_data, PathRx};
use ywp::DropReason;

fuzz_target!(|data: &[u8]| {
    let key = DirKey::new([7u8; 32], [9u8; 12]);
    let mut rx = PathRx::new();
    let mut pkt = data.to_vec();
    if let Err(r) = open_data(&mut pkt, &key, &mut rx) {
        // AEAD 通过之后才提交；在那之前失败的，状态和缓冲都不变
        if !matches!(r, DropReason::ReservedChunk | DropReason::MalformedChunk) {
            assert!(rx.largest.is_none());
            assert_eq!(&pkt[..], data);
        }
    }
});
