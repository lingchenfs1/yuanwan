//! CONTROL（0x28）与 ACK（0x29），规格 §5.5.5。
//! CONTROL：stream u8 | msg_seq u16 | frag u8（高 4 位分片序号，低 4 位分片数 − 1）| 数据。
//! ACK：stream u8 | cum u16（初值 0xFFFF）| sack u32（第 i 位对应 msg_seq = cum + 1 + i）。
//!
//! 这里只管字节布局：stream 是否合法、单片限制、窗口由可靠层（reliable）判断。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub stream: u8,
    /// 已按序收齐的最大 msg_seq
    pub cum: u16,
    pub sack: u32,
}

/// CONTROL 块的只读视图（v1 只发单片，frag = 0x00）
#[derive(Clone, Copy, Debug)]
pub struct ControlFrag<'a> {
    pub stream: u8,
    pub msg_seq: u16,
    pub frag: u8,
    pub data: &'a [u8],
}

/// 发送端持有的 CONTROL 分片（StreamTx::poll 的输出）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlFragOwned {
    pub stream: u8,
    pub msg_seq: u16,
    pub frag: u8,
    pub data: Vec<u8>,
}

impl ControlFragOwned {
    /// 借成只读视图（单测「先编码再解析」、本地回环用）
    pub fn as_frag(&self) -> ControlFrag<'_> {
        ControlFrag { stream: self.stream, msg_seq: self.msg_seq, frag: self.frag, data: &self.data }
    }
}

impl<'a> ControlFrag<'a> {
    /// 解析 CONTROL 块的内容（≥ CONTROL_HDR）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::CONTROL_HDR {
            return Err(DropReason::ShortChunk);
        }
        let mut r = Reader::new(content);
        let stream = r.u8().map_err(|_| DropReason::ShortChunk)?;
        let msg_seq = r.u16().map_err(|_| DropReason::ShortChunk)?;
        let frag = r.u8().map_err(|_| DropReason::ShortChunk)?;
        Ok(Self { stream, msg_seq, frag, data: r.rest() })
    }

    /// 分片序号（frag 的高 4 位）
    pub fn frag_index(&self) -> u8 {
        self.frag >> 4
    }

    /// 分片数（frag 的低 4 位 + 1）
    pub fn frag_count(&self) -> u8 {
        (self.frag & 0x0F) + 1
    }
}

/// 解析 ACK 块的内容（7 B；更长只读前 7 B）
pub fn parse_ack(content: &[u8]) -> Result<Ack, DropReason> {
    if content.len() < chunk_len::ACK {
        return Err(DropReason::ShortChunk);
    }
    let mut r = Reader::new(content);
    let stream = r.u8().map_err(|_| DropReason::ShortChunk)?;
    let cum = r.u16().map_err(|_| DropReason::ShortChunk)?;
    let sack = r.u32().map_err(|_| DropReason::ShortChunk)?;
    Ok(Ack { stream, cum, sack })
}

impl EncodeChunk for Ack {
    fn ty(&self) -> u8 {
        chunk_ty::ACK
    }
    fn content_len(&self) -> usize {
        chunk_len::ACK
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        w.put_u8(self.stream)?;
        w.put_u16(self.cum)?;
        w.put_u32(self.sack)
    }
}

impl EncodeChunk for ControlFragOwned {
    fn ty(&self) -> u8 {
        chunk_ty::CONTROL
    }
    fn content_len(&self) -> usize {
        chunk_len::CONTROL_HDR + self.data.len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        w.put_u8(self.stream)?;
        w.put_u16(self.msg_seq)?;
        w.put_u8(self.frag)?;
        w.put_slice(&self.data)
    }
}
