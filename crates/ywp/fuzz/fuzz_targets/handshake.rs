//! 握手 TLV：任意字节都不能 panic。
//! TODO(A 组)：再加整包 HS_INIT 进网关端点（固定密钥）的目标。
#![no_main]

use libfuzzer_sys::fuzz_target;
use ywp::wire::{HsInitPayload, HsRespPayload};

fuzz_target!(|data: &[u8]| {
    let _ = HsInitPayload::decode(data);
    let _ = HsRespPayload::decode(data);
});
