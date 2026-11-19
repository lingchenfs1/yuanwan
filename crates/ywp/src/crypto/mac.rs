//! 基于 BLAKE2s 的几个派生量（规格 §4.4、§4.5）。
//! BLAKE2s-128 一律按参数块方式计算（digest_length = 16），不是把 256 位结果截断。

use crate::consts::{GATEWAY_ID_LEN, HOST_ID_LABEL, HOST_ID_LEN, MAC1_LABEL};
use crate::wire::Cid;
use blake2::digest::consts::U16;
use blake2::digest::Mac;
use blake2::{Blake2s256, Blake2sMac, Digest};
use subtle::ConstantTimeEq;

/// BLAKE2s-256，依次喂入各段
pub fn blake2s256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Blake2s256::new();
    for p in parts {
        Digest::update(&mut h, p);
    }
    h.finalize().into()
}

/// 带密钥的 BLAKE2s-128：参数块里 key_length = 32、digest_length = 16
pub fn keyed_blake2s128(key: &[u8; 32], data: &[u8]) -> [u8; 16] {
    let mut m = <Blake2sMac<U16>>::new_with_salt_and_personal(key, &[], &[]).expect("32 B 密钥不超过 BLAKE2s 上限");
    Mac::update(&mut m, data);
    let r = m.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&r);
    out
}

/// mac1 的密钥 = BLAKE2s-256("mac1----" ‖ 对端静态公钥)
pub fn mac1_key(peer_pub: &[u8; 32]) -> [u8; 32] {
    blake2s256(&[MAC1_LABEL, peer_pub])
}

/// mac1 = 带密钥 BLAKE2s-128(key, data)；HS_INIT 覆盖 [0, L−32)
pub fn mac1(key: &[u8; 32], data: &[u8]) -> [u8; 16] {
    keyed_blake2s128(key, data)
}

/// 常量时间比较 mac1
pub fn verify_mac1(key: &[u8; 32], data: &[u8], mac: &[u8]) -> bool {
    mac.len() == 16 && bool::from(mac1(key, data).ct_eq(mac))
}

/// gateway_id = BLAKE2s-256(网关公钥) 的前 8 B
pub fn gateway_id(gw_pub: &[u8; 32]) -> [u8; 8] {
    let h = blake2s256(&[gw_pub]);
    let mut id = [0u8; GATEWAY_ID_LEN];
    id.copy_from_slice(&h[..GATEWAY_ID_LEN]);
    id
}

/// reset_token = 带密钥 BLAKE2s-128(reset_key, CID 的 8 B 大端)
pub fn reset_token(reset_key: &[u8; 32], cid: Cid) -> [u8; 16] {
    keyed_blake2s128(reset_key, &cid.0.to_be_bytes())
}

/// host_id = BLAKE2s-256("yw-host" ‖ gateway.conf 的 host 字符串) 的前 8 B
pub fn host_id(host: &str) -> [u8; 8] {
    let h = blake2s256(&[HOST_ID_LABEL, host.as_bytes()]);
    let mut id = [0u8; HOST_ID_LEN];
    id.copy_from_slice(&h[..HOST_ID_LEN]);
    id
}
