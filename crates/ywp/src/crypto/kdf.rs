//! iv 派生（附录 A.2）：prk = HKDF-Extract(salt = "ywp v0.1 iv", ikm = 握手哈希 h)，HMAC-BLAKE2s；
//! iv_c2s = Expand(prk, "c2s", 12)，iv_s2c = Expand(prk, "s2c", 12)。
//! 注：blake2 0.10 的 BLAKE2s 要配 hkdf 的 SimpleHkdf（基于 SimpleHmac）。

/// 返回 (iv_c2s, iv_s2c)
pub fn derive_ivs(handshake_hash: &[u8]) -> ([u8; 12], [u8; 12]) {
    todo!()
}
