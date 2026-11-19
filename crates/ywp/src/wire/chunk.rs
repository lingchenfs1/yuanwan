//! 块的解析和编码（规格 §5.3）。
//!
//! 解析分两遍：先 [`validate_chunks`] 校验整包（遇到保留块或越界整包丢弃，前面的块也不生效），
//! 通过了再用 [`ChunkIter`] 逐块解码。单个块的字段不合法时只丢这一块（[`ChunkError`]）。

use super::control::parse_ack;
use super::misc::{parse_close, parse_path8, parse_ping, parse_pong};
use super::{
    Ack, AudioChunk, ControlFrag, FeedbackState, FrameStatusList, InputChunk, Nack, Need, Pong, Report, UpReport,
    VideoChunk, Writer,
};
use crate::consts::{chunk_ty, CHUNK_TLV_HDR};
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

/// 本端认识的、带长度的块类型（0x20 起）。HAPTICS、MIC 和各种「后」的类型在 v1 里按未知块处理
pub fn is_known_tlv_chunk(ty: u8) -> bool {
    matches!(
        ty,
        chunk_ty::PING
            | chunk_ty::PONG
            | chunk_ty::PATH_CHALLENGE
            | chunk_ty::PATH_RESPONSE
            | chunk_ty::CLOSE
            | chunk_ty::CONTROL
            | chunk_ty::ACK
            | chunk_ty::AUDIO
            | chunk_ty::INPUT
            | chunk_ty::FEEDBACK
            | chunk_ty::REPORT
            | chunk_ty::NACK
            | chunk_ty::NEED
            | chunk_ty::FRAME_STATUS
            | chunk_ty::UP_REPORT
    )
}

fn is_reserved(ty: u8) -> bool {
    (chunk_ty::RESERVED_FIRST..=chunk_ty::RESERVED_LAST).contains(&ty)
}

fn is_video(ty: u8) -> bool {
    ty & chunk_ty::VIDEO_MASK == chunk_ty::VIDEO
}

/// 第一遍：整包合法才继续。plain 是解密后的明文去掉 SendTS 之后的部分。
///
/// - 0x01~0x0F 保留块：返回 ReservedType，整包丢弃（前面的块也不生效）；
/// - 带长度的块越界：返回 Truncated，整包丢弃；
/// - PADDING、VIDEO 延伸到包尾，见到就结束（VIDEO 块自身字段的校验在第二遍，只丢这一块）。
pub fn validate_chunks(plain: &[u8]) -> Result<ChunkSummary, WireError> {
    let mut sum = ChunkSummary::default();
    let mut pos = 0usize;
    while pos < plain.len() {
        let ty = plain[pos];
        sum.count = sum.count.saturating_add(1);
        if ty == chunk_ty::PADDING {
            break;
        }
        if is_reserved(ty) {
            return Err(WireError::ReservedType);
        }
        if is_video(ty) {
            sum.video = true;
            break;
        }
        if plain.len() - pos < CHUNK_TLV_HDR {
            return Err(WireError::Truncated);
        }
        let len = u16::from_be_bytes([plain[pos + 1], plain[pos + 2]]) as usize;
        if plain.len() - pos - CHUNK_TLV_HDR < len {
            return Err(WireError::Truncated);
        }
        if !is_known_tlv_chunk(ty) {
            sum.unknown = sum.unknown.saturating_add(1);
        }
        pos += CHUNK_TLV_HDR + len;
    }
    Ok(sum)
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
        let (&ty, after) = self.rest.split_first()?;
        if ty == chunk_ty::PADDING {
            self.rest = &[];
            return Some(Ok(Chunk::Padding));
        }
        if is_reserved(ty) {
            self.rest = &[];
            return Some(Err(ChunkError { ty, reason: DropReason::ReservedChunk }));
        }
        if is_video(ty) {
            self.rest = &[];
            return Some(VideoChunk::parse(ty, after).map(Chunk::Video).map_err(|reason| ChunkError { ty, reason }));
        }
        // 第一遍已经保证不越界；没经过第一遍的输入在这里也不会 panic，只结束迭代
        if after.len() < CHUNK_TLV_HDR - 1 {
            self.rest = &[];
            return Some(Err(ChunkError { ty, reason: DropReason::MalformedChunk }));
        }
        let len = u16::from_be_bytes([after[0], after[1]]) as usize;
        let body = &after[2..];
        if body.len() < len {
            self.rest = &[];
            return Some(Err(ChunkError { ty, reason: DropReason::MalformedChunk }));
        }
        let (content, rest) = body.split_at(len);
        self.rest = rest;
        Some(parse_tlv_chunk(ty, content).map_err(|reason| ChunkError { ty, reason }))
    }
}

/// 解一个带长度的块（0x20 起）的内容。不认识的类型返回 Chunk::Unknown
fn parse_tlv_chunk(ty: u8, c: &[u8]) -> Result<Chunk<'_>, DropReason> {
    Ok(match ty {
        chunk_ty::PING => Chunk::Ping(parse_ping(c)?),
        chunk_ty::PONG => Chunk::Pong(parse_pong(c)?),
        chunk_ty::PATH_CHALLENGE => Chunk::PathChallenge(parse_path8(c)?),
        chunk_ty::PATH_RESPONSE => Chunk::PathResponse(parse_path8(c)?),
        chunk_ty::CLOSE => {
            let (code, reason) = parse_close(c)?;
            Chunk::Close { code, reason }
        }
        chunk_ty::CONTROL => Chunk::Control(ControlFrag::parse(c)?),
        chunk_ty::ACK => Chunk::Ack(parse_ack(c)?),
        chunk_ty::AUDIO => Chunk::Audio(AudioChunk::parse(c)?),
        chunk_ty::INPUT => Chunk::Input(InputChunk::parse(c)?),
        chunk_ty::FEEDBACK => Chunk::Feedback(FeedbackState::parse(c)?),
        chunk_ty::REPORT => Chunk::Report(Report::parse(c)?),
        chunk_ty::NACK => Chunk::Nack(Nack::parse(c)?),
        chunk_ty::NEED => Chunk::Need(Need::parse(c)?),
        chunk_ty::FRAME_STATUS => Chunk::FrameStatus(FrameStatusList::parse(c)?),
        chunk_ty::UP_REPORT => Chunk::UpReport(UpReport::parse(c)?),
        _ => Chunk::Unknown { ty },
    })
}

impl Chunk<'_> {
    /// 计数用的块类型字节（VIDEO 返回 0x10，标志位不算）
    pub fn ty(&self) -> u8 {
        match self {
            Chunk::Padding => chunk_ty::PADDING,
            Chunk::Video(_) => chunk_ty::VIDEO,
            Chunk::Ping(_) => chunk_ty::PING,
            Chunk::Pong(_) => chunk_ty::PONG,
            Chunk::PathChallenge(_) => chunk_ty::PATH_CHALLENGE,
            Chunk::PathResponse(_) => chunk_ty::PATH_RESPONSE,
            Chunk::Close { .. } => chunk_ty::CLOSE,
            Chunk::Control(_) => chunk_ty::CONTROL,
            Chunk::Ack(_) => chunk_ty::ACK,
            Chunk::Audio(_) => chunk_ty::AUDIO,
            Chunk::Input(_) => chunk_ty::INPUT,
            Chunk::Feedback(_) => chunk_ty::FEEDBACK,
            Chunk::Report(_) => chunk_ty::REPORT,
            Chunk::Nack(_) => chunk_ty::NACK,
            Chunk::Need(_) => chunk_ty::NEED,
            Chunk::FrameStatus(_) => chunk_ty::FRAME_STATUS,
            Chunk::UpReport(_) => chunk_ty::UP_REPORT,
            Chunk::Unknown { ty } => *ty,
        }
    }
}

/// 能写进包里的块（带长度的块由 PacketWriter 写 type 和 len，encode 只写内容，
/// 而且必须正好写 content_len 个字节）
pub trait EncodeChunk {
    fn ty(&self) -> u8;
    fn content_len(&self) -> usize;
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError>;
}
