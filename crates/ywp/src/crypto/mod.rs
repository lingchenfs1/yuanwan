//! 加密（A 组）：Noise IKpsk2、AEAD 方向密钥、mac1、HKDF 派生 iv、TAI64N。
//!
//! 脱敏：静态私钥、PSK、传输密钥、reset_key 不进日志也不进 Debug 输出，
//! 持有它们的类型一律手写 Debug，只打 `<redacted>`。

// lib.rs 为第 0 步骨架整体放宽了这几项；本模块已实现完，重新打开
#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod aead;
pub mod kdf;
pub mod mac;
pub mod noise;
pub mod tai64n;

pub use aead::{AeadError, DirKey, TransportKeys};
pub use kdf::derive_ivs;
pub use mac::{gateway_id, host_id, mac1, mac1_key, reset_token, verify_mac1};
pub use noise::{accept_hs_init, hs_init_ephemeral, HsInitAccepted, HsPeer, InitiatorAttempt};
pub use tai64n::Tai64N;

use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::fmt;

/// 系统随机数。失败只可能是系统随机源坏了，这时继续运行没有意义
pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("系统随机数不可用");
    b
}

/// 32 B 的秘密（PSK、reset_key、静态私钥）。Debug 打 `<redacted>`，drop 时清零
#[derive(Clone, PartialEq, Eq)]
pub struct Secret32([u8; 32]);

impl Secret32 {
    pub fn new(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// 用系统随机数生成（PSK、reset_key）
    pub fn random() -> Self {
        Self(random_bytes())
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
        wipe(&mut self.0);
    }
}

/// 清零（volatile 写，防止被优化掉）
pub(crate) fn wipe(b: &mut [u8]) {
    for x in b.iter_mut() {
        // SAFETY: x 是有效的可变引用
        unsafe { std::ptr::write_volatile(x, 0) };
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
        let mut b = random_bytes::<32>();
        let kp = Self::from_secret(b);
        wipe(&mut b);
        kp
    }

    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// 从私钥恢复（算出公钥；X25519 的私钥钳位在标量乘时做，存的是原始 32 B）
    pub fn from_secret(b: [u8; 32]) -> Self {
        let mut dh = DefaultResolver.resolve_dh(&DHChoice::Curve25519).expect("snow 默认实现总有 Curve25519");
        dh.set(&b);
        let mut public = [0u8; 32];
        public.copy_from_slice(dh.pubkey());
        Self { secret: Secret32::new(b), public }
    }

    /// 私钥，只在交给 snow 和写 keys/ 文件时用
    pub(crate) fn secret(&self) -> &Secret32 {
        &self.secret
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StaticKeypair(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_public_matches_rfc7748() {
        // RFC 7748 §6.1 的 Alice
        let sk = hex32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let pk = hex32("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert_eq!(StaticKeypair::from_secret(sk).public(), pk);
        assert_eq!(format!("{:?}", StaticKeypair::from_secret(sk)), "StaticKeypair(<redacted>)");
        assert_eq!(format!("{:?}", Secret32::new(sk)), "<redacted>");
        assert_ne!(StaticKeypair::generate().public(), StaticKeypair::generate().public());
    }

    fn hex32(s: &str) -> [u8; 32] {
        let mut b = [0u8; 32];
        for (i, x) in b.iter_mut().enumerate() {
            *x = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        b
    }
}
