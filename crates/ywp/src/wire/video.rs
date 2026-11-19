//! VIDEO 块（0x10~0x1F，规格 §5.5.1、附录 A.7）和 FRAME_STATUS（0x43，§5.5.11）。

use super::{EncodeChunk, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

/// VIDEO 块头（10 B）的各字段，已按位拆开
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoHeader {
    /// K：关键帧
    pub key: bool,
    /// P：校验符号（当且仅当 sym_idx ≥ k）
    pub parity: bool,
    /// L：本帧初次发送中的最后一个源符号
    pub last_src: bool,
    /// X：修复包
    pub repair: bool,
    pub fid: u16,
    /// 0..k−1 是源符号，k..k+R−1 是校验符号
    pub sym_idx: u16,
    /// 本帧源符号数 1..=1024（线上存 k − 1）
    pub k: u16,
    /// 本帧编码时生成的校验符号总数，只有校验包里的值有效，源包填 0
    pub r: u16,
    /// 发送时刻 − 本帧 t0，单位 100 µs
    pub age_100us: u16,
    /// PS5 的 adaptive_stream_index（目前恒为 0）
    pub profile: u8,
    /// 每个 PS5 unit 切成几个符号（线上存 n − 1；v1 整帧路线恒为 1）
    pub n: u8,
    /// VIDEO_CONFIG 版本号的低 2 位
    pub cfg: u8,
    /// W：chiaki 改写过参考（recovered）
    pub w: bool,
}

/// 一个 VIDEO 块：源包的 body 是数据（不含 len 字段），校验包的 body 是 S 字节
#[derive(Clone, Copy, Debug)]
pub struct VideoChunk<'a> {
    pub hdr: VideoHeader,
    pub body: &'a [u8],
}

/// FRAME_STATUS 的 status（v1 只发 Gone 和 KeyPending）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameStatusKind {
    AbortLan = 1,
    RefOk = 2,
    Gone = 3,
    KeyPending = 4,
    CatchUp = 5,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameStatusEntry {
    pub fid: u16,
    pub kind: FrameStatusKind,
    pub arg: u8,
}

/// FRAME_STATUS 块的只读视图：n u8，之后每条 4 B
#[derive(Clone, Copy, Debug)]
pub struct FrameStatusList<'a> {
    n: u8,
    raw: &'a [u8],
}

impl<'a> FrameStatusList<'a> {
    pub fn len(&self) -> usize {
        self.n as usize
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// 逐条取；不认识的 status 跳过（计 DropReason::FrameStatusBadKind）
    pub fn iter(&self) -> FrameStatusIter<'a> {
        FrameStatusIter { rest: self.raw }
    }
}

#[derive(Clone, Debug)]
pub struct FrameStatusIter<'a> {
    rest: &'a [u8],
}

impl Iterator for FrameStatusIter<'_> {
    type Item = FrameStatusEntry;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// 发送端组好的 FRAME_STATUS（网关）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameStatusOwned {
    pub entries: Vec<FrameStatusEntry>,
}

impl<'a> FrameStatusList<'a> {
    /// 解析 FRAME_STATUS 块的内容（FRAME_STATUS_HDR + FRAME_STATUS_ENTRY × n）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl<'a> VideoChunk<'a> {
    /// 解析 VIDEO 块（ty 是 0x10~0x1F 的块类型字节，content 到包尾）
    pub fn parse(ty: u8, content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl EncodeChunk for FrameStatusOwned {
    fn ty(&self) -> u8 {
        chunk_ty::FRAME_STATUS
    }
    fn content_len(&self) -> usize {
        chunk_len::FRAME_STATUS_HDR + chunk_len::FRAME_STATUS_ENTRY * self.entries.len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}
