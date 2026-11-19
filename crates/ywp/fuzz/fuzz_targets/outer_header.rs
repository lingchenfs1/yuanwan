//! 外层头：任意字节都不能 panic；能解开的，编回去要逐字节一致。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::wire::OuterHeader;

fuzz_target!(|data: &[u8]| {
    let _ = OuterHeader::peek_cid(data);
    if let Ok(h) = OuterHeader::decode(data) {
        let mut out = [0u8; ywp::consts::HDR_LEN];
        h.encode(&mut out);
        assert_eq!(&out[..], &data[..ywp::consts::HDR_LEN]);
    }
});
