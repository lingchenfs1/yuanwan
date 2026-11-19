//! REPORT（0x40）、UP_REPORT（0x44）、NACK（0x41）、NEED（0x42），规格 §5.5.6、§5.5.7、§5.5.9、§5.5.10，
//! 附录 A.7。所有计数都是累计值，丢掉任何一个 REPORT 都不影响网关算差值。

use super::{EncodeChunk, Writer};
use crate::consts::{chunk_len, chunk_ty};
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

/// REPORT 块的只读视图（GwEvent::Report 和 VideoTx::on_report 共用）
#[derive(Clone, Copy, Debug)]
pub struct Report<'a> {
    pub rep_seq: u16,
    /// 路径数 1~4（偏移 2 的低 3 位）
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
        todo!()
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
}

#[derive(Clone, Debug)]
pub struct UpPathReports<'a> {
    rest: &'a [u8],
}

impl Iterator for UpPathReports<'_> {
    type Item = UpPathReport;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
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
}

#[derive(Clone, Debug)]
pub struct NackSegments<'a> {
    rest: &'a [u8],
}

impl Iterator for NackSegments<'_> {
    type Item = (u32, u8);

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
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

/// 发送端组好的 REPORT（ClientConn 每 50 ms 组一份）
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
        todo!()
    }
}

impl<'a> UpReport<'a> {
    /// 解析 UP_REPORT 块的内容（UP_REPORT_HDR + UP_REPORT_PATH × n）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl<'a> Nack<'a> {
    /// 解析 NACK 块的内容（NACK_HDR + NACK_SEG × n，n = 1~16）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl Need {
    /// 解析 NEED 块的内容（4 B）
    pub fn parse(content: &[u8]) -> Result<Self, DropReason> {
        todo!()
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
        todo!()
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
        todo!()
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
        todo!()
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
        todo!()
    }
}
