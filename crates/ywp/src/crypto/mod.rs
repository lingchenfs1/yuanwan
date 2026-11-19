//! 加密（A 组）：Noise IKpsk2、AEAD 方向密钥、mac1、HKDF 派生 iv、TAI64N。
//!
//! 脱敏：静态私钥、PSK、传输密钥、reset_key 不进日志也不进 Debug 输出，
//! 持有它们的类型一律手写 Debug，只打 `<redacted>`。

pub mod aead;
pub mod kdf;
pub mod mac;
pub mod noise;
pub mod tai64n;

pub use aead::{AeadError, DirKey, TransportKeys};
pub use kdf::derive_ivs;
pub use mac::{gateway_id, host_id, mac1, mac1_key, reset_token};
pub use tai64n::Tai64N;

use std::fmt;

/// 32 B 的秘密（PSK、reset_key、静态私钥）。Debug 打 `<redacted>`，drop 时清零
#[derive(Clone, PartialEq, Eq)]
pub struct Secret32([u8; 32]);

impl Secret32 {
    pub fn new(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// 取原始字节。只在交给加密库时用，不要写进日志
    pub fn expose(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Secret32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for Secret32 {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            // 防止被优化掉
            unsafe { std::ptr::write_volatile(b, 0) };
        }
    }
}

/// X25519 静态密钥对。Debug 打 `<redacted>`
#[derive(Clone)]
pub struct StaticKeypair {
    secret: Secret32,
    public: [u8; 32],
}

impl StaticKeypair {
    /// 用系统随机数生成
    pub fn generate() -> Self {
        todo!()
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// 从私钥恢复（算出公钥）
    pub fn from_secret(b: [u8; 32]) -> Self {
        todo!()
    }

    /// 私钥，只在交给 snow 时用
    pub(crate) fn secret(&self) -> &Secret32 {
        &self.secret
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticKeypair(<redacted>)")
    }
}
