//! 方向密钥：ChaCha20-Poly1305，nonce = (00 00 00 ‖ path_id ‖ PN64 大端) XOR iv，AD = 外层头 14 B（规格 §4.5）。
//! Split() 出来的第一把是 c2s（发起方 → 应答方）。

use super::wipe;
use chacha20poly1305::aead::AeadInPlace;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, Tag};
use std::fmt;

/// AEAD 校验失败（不区分原因，计 DropReason::AeadFail）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AeadError;

/// 一个方向的密钥和 iv。drop 时清零
pub struct DirKey {
    key: [u8; 32],
    iv: [u8; 12],
}

impl DirKey {
    pub fn new(key: [u8; 32], iv: [u8; 12]) -> Self {
        Self { key, iv }
    }

    /// nonce = (00 00 00 ‖ path_id ‖ PN64 大端) XOR iv
    fn nonce(&self, path_id: u8, pn: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[3] = path_id;
        n[4..].copy_from_slice(&pn.to_be_bytes());
        for (a, b) in n.iter_mut().zip(self.iv) {
            *a ^= b;
        }
        n
    }

    fn cipher(&self) -> ChaCha20Poly1305 {
        ChaCha20Poly1305::new(Key::from_slice(&self.key))
    }

    /// 原地加密 buf，返回标签
    pub fn seal(&self, path_id: u8, pn: u64, ad: &[u8], buf: &mut [u8]) -> [u8; 16] {
        let n = self.nonce(path_id, pn);
        let tag = self
            .cipher()
            .encrypt_in_place_detached(Nonce::from_slice(&n), ad, buf)
            // 只有明文超过 2³⁸ B 才会失败，包长到不了
            .expect("AEAD 明文超长");
        tag.into()
    }

    /// 原地解密 buf。标签校验失败时 buf 保持不变（chacha20poly1305 0.10 先验标签、通过后才解密），
    /// 所以网关逐份试解握手候选时可以原地试下一份（有单测锁定这个行为）
    pub fn open(&self, path_id: u8, pn: u64, ad: &[u8], buf: &mut [u8], tag: &[u8]) -> Result<(), AeadError> {
        if tag.len() != 16 {
            return Err(AeadError);
        }
        let n = self.nonce(path_id, pn);
        self.cipher().decrypt_in_place_detached(Nonce::from_slice(&n), ad, buf, Tag::from_slice(tag)).map_err(|_| AeadError)
    }

    /// 测试和金标向量用：取出原始密钥和 iv。正式代码不要调用
    #[cfg(any(test, feature = "testkit"))]
    #[doc(hidden)]
    pub fn raw_for_testing(&self) -> ([u8; 32], [u8; 12]) {
        (self.key, self.iv)
    }
}

impl Drop for DirKey {
    fn drop(&mut self) {
        wipe(&mut self.key);
        wipe(&mut self.iv);
    }
}

impl fmt::Debug for DirKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DirKey(<redacted>)")
    }
}

/// 一次会话的两把方向密钥
#[derive(Debug)]
pub struct TransportKeys {
    pub tx: DirKey,
    pub rx: DirKey,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonce_layout() {
        let k = DirKey::new([0; 32], [0; 12]);
        assert_eq!(k.nonce(0x05, 0x0102_0304_0506_0708), [0, 0, 0, 5, 1, 2, 3, 4, 5, 6, 7, 8]);
        let k = DirKey::new([0; 32], [0xFF; 12]);
        assert_eq!(k.nonce(0, 0), [0xFF; 12]);
    }

    #[test]
    fn failed_open_leaves_buffer() {
        let k = DirKey::new([1; 32], [2; 12]);
        let mut buf = *b"hello, ywp";
        let tag = k.seal(0, 7, b"ad", &mut buf);
        let ct = buf;
        let mut bad = tag;
        bad[0] ^= 1;
        assert_eq!(k.open(0, 7, b"ad", &mut buf, &bad), Err(AeadError));
        assert_eq!(buf, ct, "校验失败时缓冲不变");
        k.open(0, 7, b"ad", &mut buf, &tag).unwrap();
        assert_eq!(&buf, b"hello, ywp");
    }
}
