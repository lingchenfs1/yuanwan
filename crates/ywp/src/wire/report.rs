//! REPORT（0x40）、UP_REPORT（0x44）、NACK（0x41）、NEED（0x42），规格 §5.5.6、§5.5.7、§5.5.9、§5.5.10。
//! 所有计数都是累计值，丢掉任何一个 REPORT 都不影响网关算差值。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{
    chunk_len, chunk_ty, NACK_MAX_SEGS, NEED_FLAG_KEY, NEED_TRIES_MASK, REPORT_MODE_SHIFT, REPORT_PATHS_MASK,
};
use crate::error::{DropReason, WireError};

/// REPORT 偏移 2~19 的视频部分（客户端 Assembler::report 产出，ClientConn::set_video_report 填进 REPORT）。
/// yw-media 的 video_rx 原样转出这个类型（ywp 不能依赖 yw-media，所以定义在这里）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportVideoFields {
    /// 0 竞技 / 1 均衡 / 2 稳定（consts::mode）
    pub mode: u8,
    /// 偏移 3：S_b90（ms，帧展开余量）
    pub sb90_ms: u8,
    /// 偏移 4：J（ms，播放余量）
    pub j_ms: u8,
    pub frames_ok: u16,
    pub frames_late: u16,
    pub frames_repair: u16,
    pub frames_lost: u16,
    pub fec_recovered: u16,
    pub oldest_needed_fid: u16,
    /// blocked_need == 0 时填 0xFFFF，只作提示
    pub blocked_fid: u16,
    /// 0 表示没有阻塞
    pub blocked_need: u8,
}

/// REPORT 的每路径块（22 B）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathReport {
    pub path_id: u8,
    /// 低 32 位
    pub largest_pn: u32,
    pub recv_cnt: u32,
    pub lost_cnt: u32,
    /// 落在「连丢 ≤ 3 包」的段里的判丢数（模 2¹⁶）
    pub lost_short: u16,
    /// 最近 1 s 最长的一次连丢（封顶 255）
    pub burst_max: u8,
    /// 排队时延，0.5 ms（M2a~M2d 填 0）
    pub q_now: u8,
    pub q_p95: u8,
    /// 最近 200 ms 接收速率，16 kbps（M2a~M2d 填 0）
    pub rx_rate: u16,
    /// 容量估计，16 kbps（v1 恒为 0）
    pub c_est: u16,
}

impl PathReport {
    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        Ok(Self {
            path_id: r.u8()?,
            largest_pn: r.u32()?,
            recv_cnt: r.u32()?,
            lost_cnt: r.u32()?,
            lost_short: r.u16()?,
            burst_max: r.u8()?,
            q_now: r.u8()?,
            q_p95: r.u8()?,
            rx_rate: r.u16()?,
            c_est: r.u16()?,
        })
    }

    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        w.put_u8(self.path_id)?;
        w.put_u32(self.largest_pn)?;
        w.put_u32(self.recv_cnt)?;
        w.put_u32(self.lost_cnt)?;
        w.put_u16(self.lost_short)?;
        w.put_u8(self.burst_max)?;
        w.put_u8(self.q_now)?;
        w.put_u8(self.q_p95)?;
        w.put_u16(self.rx_rate)?;
        w.put_u16(self.c_est)
    }
}

/// REPORT 块的只读视图（GwEvent::Report 和 VideoTx::on_report 共用）
#[derive(Clone, Copy, Debug)]
pub struct Report<'a> {
    pub rep_seq: u16,
    /// 路径数（偏移 2 的低 3 位；规格定义 1~4，解析时不强求，只按它算长度）
    pub n_paths: u8,
    /// 偏移 2~19 的视频部分（mode 取偏移 2 的高 2 位）
    pub video: ReportVideoFields,
    raw_paths: &'a [u8],
}

impl<'a> Report<'a> {
    pub fn paths(&self) -> PathReports<'a> {
        PathReports { rest: self.raw_paths }
    }
}

#[derive(Clone, Debug)]
pub struct PathReports<'a> {
    rest: &'a [u8],
}

impl Iterator for PathReports<'_> {
    type Item = PathReport;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < chunk_len::REPORT_PATH {
            self.rest = &[];
            return None;
        }
        let (one, rest) = self.rest.split_at(chunk_len::REPORT_PATH);
        self.rest = rest;
        PathReport::decode(&mut Reader::new(one)).ok()
    }
}

/// UP_REPORT 的每路径块（12 B）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpPathReport {
    pub path_id: u8,
    pub in_recv: u32,
    /// 按 PN 缺口算
    pub in_lost: u32,
    /// 0.5 ms（M2a~M2d 填 0）
    pub q_up: u8,
    /// 最近 1 s（M2a~M2d 填 0）
    pub up_loss_ppm: u16,
}

/// UP_REPORT 块的只读视图：n u8，之后每条路径 12 B
#[derive(Clone, Copy, Debug)]
pub struct UpReport<'a> {
    n: u8,
    raw: &'a [u8],
}

impl<'a> UpReport<'a> {
    pub fn paths(&self) -> UpPathReports<'a> {
        UpPathReports { rest: self.raw }
    }

    pub fn n_paths(&self) -> usize {
        self.n as usize
    }
}

#[derive(Clone, Debug)]
pub struct UpPathReports<'a> {
    rest: &'a [u8],
}

impl Iterator for UpPathReports<'_> {
    type Item = UpPathReport;

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < chunk_len::UP_REPORT_PATH {
            self.rest = &[];
            return None;
        }
        let (one, rest) = self.rest.split_at(chunk_len::UP_REPORT_PATH);
        self.rest = rest;
        let mut r = Reader::new(one);
        Some(UpPathReport {
            path_id: r.u8().ok()?,
            in_recv: r.u32().ok()?,
            in_lost: r.u32().ok()?,
            q_up: r.u8().ok()?,
            up_loss_ppm: r.u16().ok()?,
        })
    }
}

/// NACK 块的只读视图：path_id u8 | n u8 | n × (起始 PN 低 32 位 u32, count u8)，1~16 段。
/// 网关以自己已发出的最大 PN 为基准还原成 PN64
#[derive(Clone, Copy, Debug)]
pub struct Nack<'a> {
    pub path_id: u8,
    n: u8,
    raw: &'a [u8],
}

impl<'a> Nack<'a> {
    /// (start_pn_lo32, count)
    pub fn segments(&self) -> NackSegments<'a> {
        NackSegments { rest: self.raw }
    }

    pub fn n_segments(&self) -> usize {
        self.n as usize
    }
}

#[derive(Clone, Debug)]
pub struct NackSegments<'a> {
    rest: &'a [u8],
}

impl Iterator for NackSegments<'_> {
    type Item = (u32, u8);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.len() < chunk_len::NACK_SEG {
            self.rest = &[];
            return None;
        }
        let (s, rest) = self.rest.split_at(chunk_len::NACK_SEG);
        self.rest = rest;
        Some((u32::from_be_bytes([s[0], s[1], s[2], s[3]]), s[4]))
    }
}

/// 发送端组好的 NACK（NackQueue::take 的输出）
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NackOwned {
    pub path_id: u8,
    /// (start_pn_lo32, count)，1~16 段
    pub segs: Vec<(u32, u8)>,
}

/// NEED（4 B）：fid u16 | need u8 | flags u8（位 7 K，位 1–0 尝试次数）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    pub fid: u16,
    /// 还缺几个符号；0xFF 表示不知道 k
    pub need: u8,
    /// 同时请网关请求关键帧
    pub key: bool,
    /// 尝试次数 0~3
    pub tries: u8,
}

/// 发送端组好的 REPORT（ClientConn 定时组一份）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportOwned {
    pub rep_seq: u16,
    pub video: ReportVideoFields,
    /// 1~4 条
    pub paths: Vec<PathReport>,
}

/// 发送端组好的 UP_REPORT（网关每 100 ms 组一份）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpReportOwned {
    pub paths: Vec<UpPathReport>,
}

impl<'a> Report<'a> {
    /// 解析 REPORT 块的内容（REPORT_FIXED + REPORT_PATH × 路径数）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::REPORT_FIXED {
            return Err(DropReason::ShortChunk);
        }
        let mut r = Reader::new(content);
        let e = |_| DropReason::ShortChunk;
        let rep_seq = r.u16().map_err(e)?;
        let b2 = r.u8().map_err(e)?;
        let n_paths = b2 & REPORT_PATHS_MASK;
        let video = ReportVideoFields {
            mode: b2 >> REPORT_MODE_SHIFT,
            sb90_ms: r.u8().map_err(e)?,
            j_ms: r.u8().map_err(e)?,
            frames_ok: r.u16().map_err(e)?,
            frames_late: r.u16().map_err(e)?,
            frames_repair: r.u16().map_err(e)?,
            frames_lost: r.u16().map_err(e)?,
            fec_recovered: r.u16().map_err(e)?,
            oldest_needed_fid: r.u16().map_err(e)?,
            blocked_fid: r.u16().map_err(e)?,
            blocked_need: r.u8().map_err(e)?,
        };
        let raw_paths = r.bytes(chunk_len::REPORT_PATH * n_paths as usize).map_err(e)?;
        Ok(Self { rep_seq, n_paths, video, raw_paths })
    }
}

impl<'a> UpReport<'a> {
    /// 解析 UP_REPORT 块的内容（UP_REPORT_HDR + UP_REPORT_PATH × n）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        let (&n, rest) = content.split_first().ok_or(DropReason::ShortChunk)?;
        let need = chunk_len::UP_REPORT_PATH * n as usize;
        if rest.len() < need {
            return Err(DropReason::ShortChunk);
        }
        Ok(Self { n, raw: &rest[..need] })
    }
}

impl<'a> Nack<'a> {
    /// 解析 NACK 块的内容（NACK_HDR + NACK_SEG × n，n = 1~16，每段 count ≥ 1）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::NACK_HDR {
            return Err(DropReason::ShortChunk);
        }
        let path_id = content[0];
        let n = content[1];
        if n == 0 || n as usize > NACK_MAX_SEGS {
            return Err(DropReason::NackBadSegments);
        }
        let need = chunk_len::NACK_SEG * n as usize;
        let rest = &content[chunk_len::NACK_HDR..];
        if rest.len() < need {
            return Err(DropReason::ShortChunk);
        }
        let raw = &rest[..need];
        if raw.as_chunks::<{ chunk_len::NACK_SEG }>().0.iter().any(|s| s[4] == 0) {
            return Err(DropReason::NackBadSegments);
        }
        Ok(Self { path_id, n, raw })
    }
}

impl Need {
    /// 解析 NEED 块的内容（4 B）
    pub fn parse(content: &[u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::NEED {
            return Err(DropReason::ShortChunk);
        }
        let flags = content[3];
        Ok(Self {
            fid: u16::from_be_bytes([content[0], content[1]]),
            need: content[2],
            key: flags & NEED_FLAG_KEY != 0,
            tries: flags & NEED_TRIES_MASK,
        })
    }
}

impl EncodeChunk for ReportOwned {
    fn ty(&self) -> u8 {
        chunk_ty::REPORT
    }
    fn content_len(&self) -> usize {
        chunk_len::REPORT_FIXED + chunk_len::REPORT_PATH * self.paths.len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        if self.paths.len() > REPORT_PATHS_MASK as usize {
            return Err(WireError::Malformed("REPORT 路径数超过 7"));
        }
        let v = &self.video;
        w.put_u16(self.rep_seq)?;
        w.put_u8((v.mode << REPORT_MODE_SHIFT) | self.paths.len() as u8)?;
        w.put_u8(v.sb90_ms)?;
        w.put_u8(v.j_ms)?;
        w.put_u16(v.frames_ok)?;
        w.put_u16(v.frames_late)?;
        w.put_u16(v.frames_repair)?;
        w.put_u16(v.frames_lost)?;
        w.put_u16(v.fec_recovered)?;
        w.put_u16(v.oldest_needed_fid)?;
        w.put_u16(v.blocked_fid)?;
        w.put_u8(v.blocked_need)?;
        for p in &self.paths {
            p.encode(w)?;
        }
        Ok(())
    }
}

impl EncodeChunk for UpReportOwned {
    fn ty(&self) -> u8 {
        chunk_ty::UP_REPORT
    }
    fn content_len(&self) -> usize {
        chunk_len::UP_REPORT_HDR + chunk_len::UP_REPORT_PATH * self.paths.len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        let n = u8::try_from(self.paths.len()).map_err(|_| WireError::Malformed("UP_REPORT 路径数超过 255"))?;
        w.put_u8(n)?;
        for p in &self.paths {
            w.put_u8(p.path_id)?;
            w.put_u32(p.in_recv)?;
            w.put_u32(p.in_lost)?;
            w.put_u8(p.q_up)?;
            w.put_u16(p.up_loss_ppm)?;
        }
        Ok(())
    }
}

impl EncodeChunk for NackOwned {
    fn ty(&self) -> u8 {
        chunk_ty::NACK
    }
    fn content_len(&self) -> usize {
        chunk_len::NACK_HDR + chunk_len::NACK_SEG * self.segs.len()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        if self.segs.is_empty() || self.segs.len() > NACK_MAX_SEGS {
            return Err(WireError::Malformed("NACK 段数不在 1~16"));
        }
        if self.segs.iter().any(|s| s.1 == 0) {
            return Err(WireError::Malformed("NACK 段的 count 为 0"));
        }
        w.put_u8(self.path_id)?;
        w.put_u8(self.segs.len() as u8)?;
        for &(start, count) in &self.segs {
            w.put_u32(start)?;
            w.put_u8(count)?;
        }
        Ok(())
    }
}

impl EncodeChunk for Need {
    fn ty(&self) -> u8 {
        chunk_ty::NEED
    }
    fn content_len(&self) -> usize {
        chunk_len::NEED
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        w.put_u16(self.fid)?;
        w.put_u8(self.need)?;
        w.put_u8(if self.key { NEED_FLAG_KEY } else { 0 } | (self.tries & NEED_TRIES_MASK))
    }
}
