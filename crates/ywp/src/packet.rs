//! DATA 包的组装和拆解（A 组，规格 §5.2）：外层头 14 B | SendTS 4 B | 块…… | 标签 16 B，SendTS 和块是密文。
//! 另有 RESET（规格 §4.4，38 B）的编码和校验。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::consts::{
    chunk_ty, CHUNK_TLV_HDR, DATA_OVERHEAD, FLAGS_RESET, HDR_LEN, RECV_MAX, RECV_MIN, RESET_LEN, RESET_RANDOM_LEN,
    RESET_TOKEN_LEN, SEND_TS_LEN, TAG_LEN,
};
use crate::crypto::noise::header_reason;
use crate::crypto::DirKey;
use crate::error::{DropReason, WireError};
use crate::pn::decode_pn;
use crate::replay::ReplayWindow;
use crate::wire::{validate_chunks, Cid, EncodeChunk, OuterHeader, PacketType, VideoHeader, Writer};
use subtle::ConstantTimeEq;

/// 包里放不下了
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

/// 块明文的起点：外层头 + SendTS
const CHUNKS_AT: usize = HDR_LEN + SEND_TS_LEN;

/// 组一个 DATA 包：new → push…… → (push_video_tail) → seal
#[derive(Debug)]
pub struct PacketWriter<'b> {
    buf: &'b mut [u8],
    limit: usize,
    pos: usize,
    /// 已经放了 VIDEO 块（延伸到包尾），之后不能再放
    tail: bool,
}

impl<'b> PacketWriter<'b> {
    /// limit：整包上限（下行 1232、上行 400），含外层头和标签；超过 buf 长度时按 buf 长度算。
    /// buf 或 limit 小于 34 B（最短的 DATA）是编程错误，会 panic
    pub fn new(buf: &'b mut [u8], limit: usize, send_ts: u32) -> Self {
        let limit = limit.min(buf.len());
        assert!(limit >= DATA_OVERHEAD, "DATA 包缓冲至少 {DATA_OVERHEAD} B");
        buf[HDR_LEN..CHUNKS_AT].copy_from_slice(&send_ts.to_be_bytes());
        Self { buf, limit, pos: CHUNKS_AT, tail: false }
    }

    /// 还能放多少字节的块（已扣掉标签）
    pub fn remaining(&self) -> usize {
        if self.tail {
            return 0;
        }
        self.limit - TAG_LEN - self.pos
    }

    /// 已放进去的块的总字节数
    pub fn chunks_len(&self) -> usize {
        self.pos - CHUNKS_AT
    }

    /// 还没放任何块
    pub fn is_empty(&self) -> bool {
        self.pos == CHUNKS_AT
    }

    /// 放一个带长度的块（类型 ≥ 0x20）。放不下返回 Full，什么也不写；
    /// 块自身编码失败（内容不合法，比如 NACK 段数超过 16）也按放不下处理
    pub fn push(&mut self, c: &impl EncodeChunk) -> Result<(), Full> {
        let ty = c.ty();
        if ty < chunk_ty::TLV_FIRST {
            debug_assert!(false, "push 只放带长度的块，VIDEO 用 push_video_tail");
            return Err(Full);
        }
        let len = c.content_len();
        let total = CHUNK_TLV_HDR + len;
        if len > u16::MAX as usize || total > self.remaining() {
            return Err(Full);
        }
        let start = self.pos;
        self.buf[start] = ty;
        self.buf[start + 1..start + CHUNK_TLV_HDR].copy_from_slice(&(len as u16).to_be_bytes());
        let mut w = Writer::new(&mut self.buf[start + CHUNK_TLV_HDR..start + total]);
        match c.encode(&mut w) {
            Ok(()) if w.pos() == len => {
                self.pos += total;
                Ok(())
            }
            Ok(()) => {
                debug_assert!(false, "块 0x{ty:02x} 的 encode 写的字节数和 content_len 不一致");
                Err(Full)
            }
            Err(_) => Err(Full),
        }
    }

    /// VIDEO 块一直延伸到包尾，必须是最后一块。源包 body 是数据（这里写 len 字段），校验包 body 是 S 字节
    pub fn push_video_tail(&mut self, h: &VideoHeader, body: &[u8]) -> Result<(), Full> {
        let total = h.chunk_len(body.len());
        if total > self.remaining() || (!h.parity && body.len() > u16::MAX as usize) {
            return Err(Full);
        }
        let mut w = Writer::new(&mut self.buf[self.pos..self.pos + total]);
        let r: Result<(), WireError> = (|| {
            h.encode(&mut w)?;
            if !h.parity {
                w.put_u16(body.len() as u16)?;
            }
            w.put_slice(body)
        })();
        r.map_err(|_| Full)?;
        self.pos += total;
        self.tail = true;
        Ok(())
    }

    /// 写外层头、原地加密、追加标签，返回整包长度。
    /// 外层头的 PN 字段以 `pn` 的低 32 位为准（`hdr.pn_trunc` 不用），免得和 nonce 用的 PN 不一致
    pub fn seal(self, hdr: &OuterHeader, pn: u64, key: &DirKey) -> usize {
        let mut h = *hdr;
        h.pn_trunc = pn as u32;
        h.encode(&mut self.buf[..HDR_LEN]);
        let (head, rest) = self.buf.split_at_mut(HDR_LEN);
        let plain_len = self.pos - HDR_LEN;
        let tag = key.seal(h.path_id, pn, head, &mut rest[..plain_len]);
        rest[plain_len..plain_len + TAG_LEN].copy_from_slice(&tag);
        self.pos + TAG_LEN
    }
}

/// 一条路径收方向的状态：最大 PN 和重放窗口（都在 AEAD 通过之后才更新）
#[derive(Clone, Debug, Default)]
pub struct PathRx {
    pub largest: Option<u64>,
    pub window: ReplayWindow,
}

impl PathRx {
    pub fn new() -> Self {
        Self::default()
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
    /// 这个包的 PN 比之前认证通过的都大（NAT 重绑定时「PN 是新的」用这个判断，规格 §3.3）
    pub new_largest: bool,
}

/// 外层头解析错误对应的丢弃原因（Short → TooShort、BadVersion、ReservedType，其余 BadHeader）
pub fn header_drop_reason(e: WireError) -> DropReason {
    header_reason(e)
}

/// 解头 → 还原 PN → 查重放 → 原地解密 → 校验通过后才提交重放窗口和 largest → 第一遍块校验。
///
/// - 包长不在 34~1500 B：TooShort / TooLong；外层头错：见 [`header_drop_reason`]；不是 DATA：BadHeader；
///   K = 1：KeyPhase（v1 不做密钥更新）；
/// - 重放：Replay / TooOld；AEAD 失败：AeadFail，这时 pkt 和 rx 都不变（可以换一把密钥再试）；
/// - AEAD 通过后提交 PN；之后块校验不过（保留块、越界）整包丢弃：ReservedChunk / MalformedChunk。
pub fn open_data<'a>(pkt: &'a mut [u8], key: &DirKey, rx: &mut PathRx) -> Result<Opened<'a>, DropReason> {
    if pkt.len() < RECV_MIN {
        return Err(DropReason::TooShort);
    }
    if pkt.len() > RECV_MAX {
        return Err(DropReason::TooLong);
    }
    let hdr = OuterHeader::decode(pkt).map_err(header_reason)?;
    if hdr.ptype != PacketType::Data {
        return Err(DropReason::BadHeader);
    }
    if hdr.key_phase {
        return Err(DropReason::KeyPhase);
    }
    let pn = decode_pn(rx.largest, hdr.pn_trunc);
    rx.window.check_reason(pn)?;

    let tag_at = pkt.len() - TAG_LEN;
    let (head, rest) = pkt.split_at_mut(HDR_LEN);
    let (body, tag) = rest.split_at_mut(tag_at - HDR_LEN);
    key.open(hdr.path_id, pn, head, body, tag).map_err(|_| DropReason::AeadFail)?;

    let new_largest = rx.largest.is_none_or(|l| pn > l);
    rx.window.commit(pn);
    if new_largest {
        rx.largest = Some(pn);
    }

    let pkt: &'a [u8] = pkt;
    let send_ts = u32::from_be_bytes([pkt[HDR_LEN], pkt[HDR_LEN + 1], pkt[HDR_LEN + 2], pkt[HDR_LEN + 3]]);
    let chunks = &pkt[CHUNKS_AT..tag_at];
    validate_chunks(chunks).map_err(|e| match e {
        WireError::ReservedType => DropReason::ReservedChunk,
        _ => DropReason::MalformedChunk,
    })?;
    Ok(Opened { hdr, pn, send_ts, chunks, new_largest })
}

// ======================================================================
// RESET（38 B）：外层头 0x70（CID、path_id 原样回显，PN 随机）| 随机 8 B | reset_token 16 B
// ======================================================================

/// 组 RESET，随机部分（PN 4 B + 8 B）取系统随机数。返回包长 38；out 不足 38 B 会 panic（编程错误）
pub fn encode_reset(cid: Cid, path_id: u8, token: &[u8; RESET_TOKEN_LEN], out: &mut [u8]) -> usize {
    encode_reset_with(cid, path_id, token, &crate::crypto::random_bytes(), out)
}

/// 同 [`encode_reset`]，随机部分由调用方给（金标向量用）：前 4 B 是 PN，后 8 B 是随机字节
pub fn encode_reset_with(
    cid: Cid,
    path_id: u8,
    token: &[u8; RESET_TOKEN_LEN],
    rand: &[u8; 4 + RESET_RANDOM_LEN],
    out: &mut [u8],
) -> usize {
    let out = &mut out[..RESET_LEN];
    let pn = u32::from_be_bytes([rand[0], rand[1], rand[2], rand[3]]);
    OuterHeader::new(PacketType::Reset, cid, path_id, pn).encode(out);
    out[HDR_LEN..HDR_LEN + RESET_RANDOM_LEN].copy_from_slice(&rand[4..]);
    out[HDR_LEN + RESET_RANDOM_LEN..].copy_from_slice(token);
    RESET_LEN
}

/// 客户端校验 RESET：包长必须正好 38 B（ResetBadLen）、Flags 必须是 0x70（BadHeader）、
/// CID 必须是自己当前的 CID（ResetBadCid）、令牌常量时间比较（ResetBadToken）
pub fn check_reset(pkt: &[u8], cid: Cid, expected: &[u8; RESET_TOKEN_LEN]) -> Result<(), DropReason> {
    if pkt.len() != RESET_LEN {
        return Err(DropReason::ResetBadLen);
    }
    if pkt[0] != FLAGS_RESET {
        return Err(DropReason::BadHeader);
    }
    if OuterHeader::peek_cid(pkt) != Some(cid) {
        return Err(DropReason::ResetBadCid);
    }
    let token = &pkt[RESET_LEN - RESET_TOKEN_LEN..];
    if bool::from(token.ct_eq(expected)) {
        Ok(())
    } else {
        Err(DropReason::ResetBadToken)
    }
}
