//! PING / PONG / PATH_CHALLENGE / PATH_RESPONSE / CLOSE（规格 §5.5.8）。
//! PING = ping_id u16；PATH_* = 8 B 随机数原样回显；CLOSE = code u16 | 原因（UTF-8 ≤ 64 B）。

use super::{EncodeChunk, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

/// PONG：RTT = 收到 PONG 的时刻 − echo_ts − hold
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pong {
    pub ping_id: u16,
    /// PING 所在包的 SendTS
    pub echo_ts: u32,
    /// 从收到 PING 到发出 PONG 的时间，单位 10 µs
    pub hold_10us: u16,
}

/// PING（发送端）：ping_id
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ping(pub u16);

/// PATH_CHALLENGE（发送端）：8 B 随机数
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathChallenge(pub [u8; 8]);

/// PATH_RESPONSE（发送端）：原样回显收到的 8 B
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PathResponse(pub [u8; 8]);

/// CLOSE（发送端）。reason 编码时按字符边界截到 CLOSE_REASON_MAX 字节（见 [`close_reason_bytes`]）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloseOwned {
    pub code: u16,
    pub reason: String,
}

/// CLOSE 原因实际写出的字节：UTF-8，按字符边界截到 CLOSE_REASON_MAX 字节
pub fn close_reason_bytes(reason: &str) -> &[u8] {
    let max = chunk_len::CLOSE_REASON_MAX;
    if reason.len() <= max {
        return reason.as_bytes();
    }
    let mut end = max;
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason.as_bytes()[..end]
}

impl EncodeChunk for Ping {
    fn ty(&self) -> u8 {
        chunk_ty::PING
    }
    fn content_len(&self) -> usize {
        chunk_len::PING
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}

impl EncodeChunk for Pong {
    fn ty(&self) -> u8 {
        chunk_ty::PONG
    }
    fn content_len(&self) -> usize {
        chunk_len::PONG
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}

impl EncodeChunk for PathChallenge {
    fn ty(&self) -> u8 {
        chunk_ty::PATH_CHALLENGE
    }
    fn content_len(&self) -> usize {
        chunk_len::PATH_CHALLENGE
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}

impl EncodeChunk for PathResponse {
    fn ty(&self) -> u8 {
        chunk_ty::PATH_RESPONSE
    }
    fn content_len(&self) -> usize {
        chunk_len::PATH_RESPONSE
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}

impl EncodeChunk for CloseOwned {
    fn ty(&self) -> u8 {
        chunk_ty::CLOSE
    }
    fn content_len(&self) -> usize {
        chunk_len::CLOSE_MIN + close_reason_bytes(&self.reason).len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}

/// 解析 PONG 的内容（8 B）
pub fn parse_pong(content: &[u8]) -> Result<Pong, DropReason> {
    todo!()
}
