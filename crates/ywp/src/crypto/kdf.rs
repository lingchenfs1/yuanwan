//! iv 派生（规格 §4.5）：prk = HKDF-Extract(salt = "ywp v0.1 iv", ikm = 握手哈希 h)，HMAC-BLAKE2s；
//! iv_c2s = Expand(prk, "c2s", 12)，iv_s2c = Expand(prk, "s2c", 12)。
//! blake2 0.10 的 BLAKE2s 没有实现 hkdf 要的 `Hmac` 所需的 trait，用 hkdf 的 SimpleHkdf（基于 SimpleHmac）。

use crate::consts::{IV_INFO_C2S, IV_INFO_S2C, IV_LEN, IV_SALT};
use blake2::Blake2s256;
use hkdf::SimpleHkdf;

/// 返回 (iv_c2s, iv_s2c)
pub fn derive_ivs(handshake_hash: &[u8]) -> ([u8; IV_LEN], [u8; IV_LEN]) {
    let hk = SimpleHkdf::<Blake2s256>::new(Some(IV_SALT), handshake_hash);
    let mut c2s = [0u8; IV_LEN];
    let mut s2c = [0u8; IV_LEN];
    // 12 B 远小于 HKDF 的上限（255 × 32 B）
    hk.expand(IV_INFO_C2S, &mut c2s).expect("HKDF 输出长度");
    hk.expand(IV_INFO_S2C, &mut s2c).expect("HKDF 输出长度");
    (c2s, s2c)
}
