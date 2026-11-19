//! CONTROL（0x28）与 ACK（0x29），规格 §5.5.5、附录 A.6。
//! CONTROL：stream u8 | msg_seq u16 | frag u8（高 4 位分片序号，低 4 位分片数 − 1）| 数据。
//! ACK：stream u8 | cum u16（初值 0xFFFF）| sack u32（第 i 位对应 msg_seq = cum + 1 + i）。

use super::{EncodeChunk, Writer};
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

impl<'a> ControlFrag<'a> {
    /// 解析 CONTROL 块的内容（≥ CONTROL_HDR）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

/// 解析 ACK 块的内容（7 B）
pub fn parse_ack(content: &[u8]) -> Result<Ack, DropReason> {
    todo!()
}

impl EncodeChunk for Ack {
    fn ty(&self) -> u8 {
        chunk_ty::ACK
    }
    fn content_len(&self) -> usize {
        chunk_len::ACK
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
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
        todo!()
    }
}
