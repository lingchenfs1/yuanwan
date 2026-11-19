//! VIDEO 块（0x10~0x1F，规格 §5.5.1）和 FRAME_STATUS（0x43，§5.5.11）。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{
    chunk_len, chunk_ty, video_flags, FEC_11_MASK, FEC_KR_MAX, FEC_K_MAX, FEC_K_SHIFT, FEC_R_MASK, FEC_SYM_SHIFT,
    META_CFG_MASK, META_CFG_SHIFT, META_N_MASK, META_N_SHIFT, META_PROFILE_MASK, META_PROFILE_SHIFT, META_W_BIT,
    VIDEO_HDR_LEN, VIDEO_SRC_LEN_LEN,
};
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
    /// W：网关这边的 PS5 会话库改写过参考
    pub w: bool,
}

impl VideoHeader {
    /// 类型字节：0001 KPLX
    pub fn type_byte(&self) -> u8 {
        chunk_ty::VIDEO
            | if self.key { video_flags::KEY } else { 0 }
            | if self.parity { video_flags::PARITY } else { 0 }
            | if self.last_src { video_flags::LAST_SRC } else { 0 }
            | if self.repair { video_flags::REPAIR } else { 0 }
    }

    /// FEC 字：sym_idx(31..21) | k−1(20..10) | R(9..0)。k 必须是 1..=1024（调用方保证）
    pub fn fec_word(&self) -> u32 {
        debug_assert!((1..=FEC_K_MAX).contains(&self.k), "k 越界");
        ((self.sym_idx as u32 & FEC_11_MASK) << FEC_SYM_SHIFT)
            | ((self.k.wrapping_sub(1) as u32 & FEC_11_MASK) << FEC_K_SHIFT)
            | (self.r as u32 & FEC_R_MASK)
    }

    /// meta：profile(7..5) | n−1(4..3) | cfg(2..1) | W(0)。n 必须是 1..=4（调用方保证）
    pub fn meta(&self) -> u8 {
        debug_assert!((1..=4).contains(&self.n), "n 越界");
        ((self.profile & META_PROFILE_MASK) << META_PROFILE_SHIFT)
            | ((self.n.wrapping_sub(1) & META_N_MASK) << META_N_SHIFT)
            | ((self.cfg & META_CFG_MASK) << META_CFG_SHIFT)
            | if self.w { META_W_BIT } else { 0 }
    }

    /// 写 10 B 块头（含类型字节）
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        if w.remaining() < VIDEO_HDR_LEN {
            return Err(WireError::NoRoom);
        }
        w.put_u8(self.type_byte())?;
        w.put_u16(self.fid)?;
        w.put_u32(self.fec_word())?;
        w.put_u16(self.age_100us)?;
        w.put_u8(self.meta())
    }

    /// 块从类型字节算起的总长：源包 12 + len，校验包 10 + S
    pub fn chunk_len(&self, body_len: usize) -> usize {
        VIDEO_HDR_LEN + if self.parity { 0 } else { VIDEO_SRC_LEN_LEN } + body_len
    }
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

impl FrameStatusKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => FrameStatusKind::AbortLan,
            2 => FrameStatusKind::RefOk,
            3 => FrameStatusKind::Gone,
            4 => FrameStatusKind::KeyPending,
            5 => FrameStatusKind::CatchUp,
            _ => return None,
        })
    }
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

    /// 逐条取；不认识的 status 跳过（调用方按 [`Self::bad_kinds`] 计 DropReason::FrameStatusBadKind）
    pub fn iter(&self) -> FrameStatusIter<'a> {
        FrameStatusIter { rest: self.raw }
    }

    /// status 不认识的条目数
    pub fn bad_kinds(&self) -> usize {
        self.raw
            .as_chunks::<{ chunk_len::FRAME_STATUS_ENTRY }>()
            .0
            .iter()
            .filter(|e| FrameStatusKind::from_u8(e[2]).is_none())
            .count()
    }
}

#[derive(Clone, Debug)]
pub struct FrameStatusIter<'a> {
    rest: &'a [u8],
}

impl Iterator for FrameStatusIter<'_> {
    type Item = FrameStatusEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.rest.len() < chunk_len::FRAME_STATUS_ENTRY {
                self.rest = &[];
                return None;
            }
            let (e, rest) = self.rest.split_at(chunk_len::FRAME_STATUS_ENTRY);
            self.rest = rest;
            if let Some(kind) = FrameStatusKind::from_u8(e[2]) {
                return Some(FrameStatusEntry { fid: u16::from_be_bytes([e[0], e[1]]), kind, arg: e[3] });
            }
        }
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
        let (&n, rest) = content.split_first().ok_or(DropReason::ShortChunk)?;
        let need = chunk_len::FRAME_STATUS_ENTRY * n as usize;
        if rest.len() < need {
            return Err(DropReason::ShortChunk);
        }
        // 比定义长：只读定义的部分
        Ok(Self { n, raw: &rest[..need] })
    }
}

impl<'a> VideoChunk<'a> {
    /// 解析 VIDEO 块（ty 是 0x10~0x1F 的块类型字节，content 是类型字节之后、到包尾的全部字节）。
    ///
    /// 收端校验（规格 §5.5.1）：k 1..=1024；P = 1 当且仅当 sym_idx ≥ k；校验包 k + R ≤ 2048、
    /// sym_idx < k + R、S 是不小于 2 的偶数；源包 12 + len 正好到包尾。不满足只丢这一块。
    /// 同一帧前后 S、k 是否一致由组帧方判断（yw-media）。
    pub fn parse(ty: u8, content: &'a [u8]) -> Result<Self, DropReason> {
        let mut r = Reader::new(content);
        let (fid, fec, age, meta) = match (r.u16(), r.u32(), r.u16(), r.u8()) {
            (Ok(a), Ok(b), Ok(c), Ok(d)) => (a, b, c, d),
            _ => return Err(DropReason::VideoBadLen),
        };
        let sym_idx = ((fec >> FEC_SYM_SHIFT) & FEC_11_MASK) as u16;
        let k = ((fec >> FEC_K_SHIFT) & FEC_11_MASK) as u16 + 1;
        let rr = (fec & FEC_R_MASK) as u16;
        if k > FEC_K_MAX {
            return Err(DropReason::VideoBadFec);
        }
        let parity = ty & video_flags::PARITY != 0;
        if parity != (sym_idx >= k) {
            return Err(DropReason::VideoParityMismatch);
        }
        let hdr = VideoHeader {
            key: ty & video_flags::KEY != 0,
            parity,
            last_src: ty & video_flags::LAST_SRC != 0,
            repair: ty & video_flags::REPAIR != 0,
            fid,
            sym_idx,
            k,
            r: rr,
            age_100us: age,
            profile: (meta >> META_PROFILE_SHIFT) & META_PROFILE_MASK,
            n: ((meta >> META_N_SHIFT) & META_N_MASK) + 1,
            cfg: (meta >> META_CFG_SHIFT) & META_CFG_MASK,
            w: meta & META_W_BIT != 0,
        };
        let body = if parity {
            if k + rr > FEC_KR_MAX || sym_idx >= k + rr {
                return Err(DropReason::VideoBadFec);
            }
            let s = r.rest();
            if s.len() < VIDEO_SRC_LEN_LEN {
                return Err(DropReason::VideoBadLen);
            }
            if !s.len().is_multiple_of(2) {
                return Err(DropReason::VideoOddShard);
            }
            s
        } else {
            let len = r.u16().map_err(|_| DropReason::VideoBadLen)? as usize;
            if r.remaining() != len {
                return Err(DropReason::VideoBadLen);
            }
            r.rest()
        };
        Ok(Self { hdr, body })
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
        let n = u8::try_from(self.entries.len()).map_err(|_| WireError::Malformed("FRAME_STATUS 条目超过 255"))?;
        w.put_u8(n)?;
        for e in &self.entries {
            w.put_u16(e.fid)?;
            w.put_u8(e.kind as u8)?;
            w.put_u8(e.arg)?;
        }
        Ok(())
    }
}
