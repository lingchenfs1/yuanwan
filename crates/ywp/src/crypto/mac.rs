//! 基于 BLAKE2s 的几个派生量（附录 A.1、A.2）。
//! BLAKE2s-128 一律按参数块方式计算（digest_length = 16），不是把 256 位结果截断。

use crate::wire::Cid;

/// mac1 的密钥 = BLAKE2s-256("mac1----" ‖ 对端静态公钥)
pub fn mac1_key(peer_pub: &[u8; 32]) -> [u8; 32] {
    todo!()
}

/// mac1 = 带密钥 BLAKE2s-128(key, data)；HS_INIT 覆盖 [0, L−32)
pub fn mac1(key: &[u8; 32], data: &[u8]) -> [u8; 16] {
    todo!()
}

/// gateway_id = BLAKE2s-256(网关公钥) 的前 8 B
pub fn gateway_id(gw_pub: &[u8; 32]) -> [u8; 8] {
    todo!()
}

/// reset_token = 带密钥 BLAKE2s-128(reset_key, CID 的 8 B 大端)
pub fn reset_token(reset_key: &[u8; 32], cid: Cid) -> [u8; 16] {
    todo!()
}

/// host_id = BLAKE2s("yw-host" ‖ gateway.conf 的 host 字符串) 的前 8 B
pub fn host_id(host: &str) -> [u8; 8] {
    todo!()
}
