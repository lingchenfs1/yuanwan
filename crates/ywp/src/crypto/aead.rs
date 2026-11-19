//! 方向密钥：ChaCha20-Poly1305，nonce = (00 00 00 ‖ path_id ‖ PN64 大端) XOR iv，AD = 外层头 14 B。
//! Split() 出来的第一把是 c2s（发起方 → 应答方）。

use std::fmt;

/// AEAD 校验失败（不区分原因，计 DropReason::AeadFail）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AeadError;

/// 一个方向的密钥和 iv
pub struct DirKey {
    key: [u8; 32],
    iv: [u8; 12],
}

impl DirKey {
    pub fn new(key: [u8; 32], iv: [u8; 12]) -> Self {
        todo!()
    }

    /// 原地加密 buf，返回标签
    pub fn seal(&self, path_id: u8, pn: u64, ad: &[u8], buf: &mut [u8]) -> [u8; 16] {
        todo!()
    }

    /// 原地解密 buf。标签校验失败时 buf 保持不变（chacha20poly1305 0.10 先验标签、通过后才解密），
    /// 所以网关逐份试解握手候选时可以原地试下一份；A 组要加一条单测锁定这个行为
    pub fn open(&self, path_id: u8, pn: u64, ad: &[u8], buf: &mut [u8], tag: &[u8]) -> Result<(), AeadError> {
        todo!()
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
