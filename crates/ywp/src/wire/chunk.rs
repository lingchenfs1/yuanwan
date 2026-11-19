//! 块的解析和编码（规格 §5.3）。
//!
//! 解析分两遍：先 [`validate_chunks`] 校验整包（遇到保留块或越界整包丢弃，前面的块也不生效），
//! 通过了再用 [`ChunkIter`] 逐块解码。单个块的字段不合法时只丢这一块（[`ChunkError`]）。

use super::{
    Ack, AudioChunk, ControlFrag, FeedbackState, FrameStatusList, InputChunk, Nack, Need, Pong, Report, UpReport,
    VideoChunk, Writer,
};
use crate::error::{DropReason, WireError};

/// 解出来的一个块。PADDING 和 VIDEO 一直延伸到包尾，必须是最后一块
#[derive(Clone, Copy, Debug)]
pub enum Chunk<'a> {
    Padding,
    Video(VideoChunk<'a>),
    Ping(u16),
    Pong(Pong),
    PathChallenge([u8; 8]),
    PathResponse([u8; 8]),
    Close { code: u16, reason: &'a [u8] },
    Control(ControlFrag<'a>),
    Ack(Ack),
    Audio(AudioChunk<'a>),
    Input(InputChunk<'a>),
    Feedback(FeedbackState),
    Report(Report<'a>),
    Nack(Nack<'a>),
    Need(Need),
    FrameStatus(FrameStatusList<'a>),
    UpReport(UpReport<'a>),
    /// 类型 ≥ 0x20 的未知块，已按 len 跳过（计 DropReason::UnknownChunk）
    Unknown { ty: u8 },
}

/// 第一遍校验的结果
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct ChunkSummary {
    /// 块数（含 PADDING 和未知块）
    pub count: u16,
    /// 有没有 VIDEO 块
    pub video: bool,
    /// 未知块数
    pub unknown: u16,
}

/// 单个块的字段不合法：只丢这一块，按 reason 计数
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChunkError {
    pub ty: u8,
    pub reason: DropReason,
}

/// 第一遍：整包合法才继续。plain 是解密后的明文去掉 SendTS 之后的部分
pub fn validate_chunks(plain: &[u8]) -> Result<ChunkSummary, WireError> {
    todo!()
}

/// 第二遍：逐块解码（只对通过了 validate_chunks 的明文用）
#[derive(Clone, Debug)]
pub struct ChunkIter<'a> {
    rest: &'a [u8],
}

impl<'a> ChunkIter<'a> {
    pub fn new(plain: &'a [u8]) -> Self {
        Self { rest: plain }
    }
}

impl<'a> Iterator for ChunkIter<'a> {
    type Item = Result<Chunk<'a>, ChunkError>;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// 能写进包里的块（带长度的块由 PacketWriter 写 type 和 len，encode 只写内容）
pub trait EncodeChunk {
    fn ty(&self) -> u8;
    fn content_len(&self) -> usize;
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError>;
}
