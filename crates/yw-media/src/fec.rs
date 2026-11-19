//! RS 封装和均分切片（M2a）：k = ⌈L / 1186⌉ ≤ 1024，d = ⌈L / k⌉；符号长度 S = 2 + d，向上取偶数。
//! 编码时 `[len][数据]` 用 0 补齐到 S。用 reed-solomon-simd 3.1.0（S 必须是偶数）。

use ywp::consts::{FEC_K_MAX, MAX_SYMBOL_DATA, VIDEO_SRC_LEN_LEN};

#[derive(Debug)]
#[non_exhaustive]
pub enum FecError {
    /// 空帧
    Empty,
    /// k 会超过 1024
    FrameTooLarge { len: usize },
    /// k、R、S 不合法
    BadParams(&'static str),
    /// 收到的符号不够 k 个
    NotEnough { have: usize, k: usize },
    Rs(reed_solomon_simd::Error),
}

impl std::fmt::Display for FecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FecError::Empty => f.write_str("空帧"),
            FecError::FrameTooLarge { len } => write!(f, "帧太大：{len} B"),
            FecError::BadParams(what) => write!(f, "参数不对：{what}"),
            FecError::NotEnough { have, k } => write!(f, "符号不够：{have}/{k}"),
            FecError::Rs(e) => write!(f, "RS：{e}"),
        }
    }
}

impl std::error::Error for FecError {}

/// 返回 (k, d)：k 个源符号，每个最多 d 字节数据
pub fn split_frame(len: usize) -> Result<(u16, usize), FecError> {
    if len == 0 {
        return Err(FecError::Empty);
    }
    let k = len.div_ceil(MAX_SYMBOL_DATA);
    if k > FEC_K_MAX as usize {
        return Err(FecError::FrameTooLarge { len });
    }
    Ok((k as u16, len.div_ceil(k)))
}

/// 2 + d，向上取偶数
pub fn shard_len(d: usize) -> usize {
    (VIDEO_SRC_LEN_LEN + d + 1) & !1
}

#[derive(Debug)]
pub struct RsEnc {
    _priv: (),
}

impl RsEnc {
    pub fn new() -> Self {
        todo!()
    }

    /// src：k × S 的源符号（已按 `[len][数据][补 0]` 排好）；out：R × S
    pub fn encode(&mut self, k: usize, r: usize, s: usize, src: &[u8], out: &mut [u8]) -> Result<(), FecError> {
        todo!()
    }
}

#[derive(Debug)]
pub struct RsDec {
    _priv: (),
}

impl RsDec {
    pub fn new() -> Self {
        todo!()
    }

    /// have：(sym_idx, 符号)；out：k × S，恢复出的全部源符号
    pub fn recover<'a>(
        &mut self,
        k: usize,
        r: usize,
        s: usize,
        have: impl Iterator<Item = (u16, &'a [u8])>,
        out: &mut [u8],
    ) -> Result<(), FecError> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_and_shard() {
        assert!(matches!(split_frame(0), Err(FecError::Empty)));
        assert_eq!(split_frame(1).unwrap(), (1, 1));
        assert_eq!(split_frame(1186).unwrap(), (1, 1186));
        assert_eq!(split_frame(1187).unwrap(), (2, 594));
        assert_eq!(split_frame(1024 * 1186).unwrap(), (1024, 1186));
        assert!(matches!(split_frame(1024 * 1186 + 1), Err(FecError::FrameTooLarge { .. })));
        assert_eq!(shard_len(1), 4);
        assert_eq!(shard_len(594), 596);
        assert_eq!(shard_len(1186), 1188);
    }
}
