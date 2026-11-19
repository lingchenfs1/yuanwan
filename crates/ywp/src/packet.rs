//! DATA 包的组装和拆解（A 组）：外层头 14 B | SendTS 4 B | 块…… | 标签 16 B，SendTS 和块是密文。

use crate::crypto::DirKey;
use crate::error::DropReason;
use crate::replay::ReplayWindow;
use crate::wire::{EncodeChunk, OuterHeader, VideoHeader};

/// 包里放不下了
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// 组一个 DATA 包：new → push…… → (push_video_tail) → seal
#[derive(Debug)]
pub struct PacketWriter<'b> {
    buf: &'b mut [u8],
    limit: usize,
    pos: usize,
}

impl<'b> PacketWriter<'b> {
    /// limit：整包上限（下行 1232、上行 400），含外层头和标签
    pub fn new(buf: &'b mut [u8], limit: usize, send_ts: u32) -> Self {
        todo!()
    }

    /// 还能放多少字节的块（已扣掉标签）
    pub fn remaining(&self) -> usize {
        todo!()
    }

    pub fn push(&mut self, c: &impl EncodeChunk) -> Result<(), Full> {
        todo!()
    }

    /// VIDEO 块一直延伸到包尾，必须是最后一块
    pub fn push_video_tail(&mut self, h: &VideoHeader, body: &[u8]) -> Result<(), Full> {
        todo!()
    }

    /// 写外层头、原地加密、追加标签，返回整包长度
    pub fn seal(self, hdr: &OuterHeader, pn: u64, key: &DirKey) -> usize {
        todo!()
    }
}

/// 一条路径收方向的状态：最大 PN 和重放窗口（都在 AEAD 通过之后才更新）
#[derive(Clone, Debug)]
pub struct PathRx {
    pub largest: Option<u64>,
    pub window: ReplayWindow,
}

impl PathRx {
    pub fn new() -> Self {
        todo!()
    }
}

/// open_data 的结果
#[derive(Clone, Copy, Debug)]
pub struct Opened<'a> {
    pub hdr: OuterHeader,
    pub pn: u64,
    pub send_ts: u32,
    /// 解密后的块（SendTS 之后、标签之前），已通过 validate_chunks
    pub chunks: &'a [u8],
}

/// 解头 → 还原 PN → 查重放 → 原地解密 → 校验通过后才提交重放窗口和 largest
pub fn open_data<'a>(pkt: &'a mut [u8], key: &DirKey, rx: &mut PathRx) -> Result<Opened<'a>, DropReason> {
    todo!()
}
