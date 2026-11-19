//! 客户端侧组帧（Assembler）。
//!
//! M2a 规则（计划 §3.1）：按 fid 顺序送解码，收完整就送；某帧不完整而后面已有完整帧并超过 50 ms、
//! 或自己挂了 1000 ms 判 Lost，发 IDR_REQUEST 并冻结到下一个完整关键帧；收到 KEY_PENDING 也冻结；
//! 每个关键帧前补上参数集，cfg_ver 变了 flush；cfg 对不上的帧先压着，最多 1000 ms；
//! W 帧照常送解码并单独计数。M2c 换成截止期、NEED 和 R_wait。

use crate::playout::Policy;
use std::sync::Arc;
use ywp::endpoint::RxMeta;
use ywp::time::Micros;
use ywp::wire::{FrameStatusEntry, Need, VideoChunk};

/// REPORT 偏移 2~19 的视频部分。定义在 ywp（ClientConn::set_video_report 要用，ywp 不能依赖本 crate），这里原样转出
pub use ywp::wire::ReportVideoFields;

/// 交给解码线程的一帧
#[derive(Clone, Debug)]
pub struct DecodeItem {
    /// 展开后的 fid
    pub fid: u64,
    /// 关键帧已在前面补上参数集
    pub au: Arc<[u8]>,
    pub key: bool,
    pub cfg: u8,
    /// 呈现时刻 P(f)
    pub p_us: Micros,
    /// 收完整的时刻
    pub complete_us: Micros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FreezeWhy {
    Lost,
    KeyPending,
    CfgTimeout,
    DecoderError,
}

/// 统计事件（客户端浮层和 JSONL 用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatEvent {
    FecRecovered { fid: u64 },
    WFrame { fid: u64 },
    Lost { fid: u64 },
    Late { fid: u64 },
}

#[derive(Clone, Debug)]
pub enum AsmEvent {
    Decode(DecodeItem),
    Need(Need),
    /// reason 见 ywp::consts::idr_reason
    IdrRequest(u8),
    Freeze(FreezeWhy),
    Unfreeze,
    Stat(StatEvent),
}

#[derive(Debug)]
pub struct Assembler {
    _priv: (),
}

impl Assembler {
    pub fn new(pol: Policy) -> Self {
        todo!()
    }

    pub fn on_video(&mut self, now: Micros, meta: &RxMeta, c: &VideoChunk, ev: &mut Vec<AsmEvent>) {
        todo!()
    }

    pub fn on_frame_status(&mut self, now: Micros, e: &FrameStatusEntry, ev: &mut Vec<AsmEvent>) {
        todo!()
    }

    /// 放行 cfg 对上了的压着的帧
    pub fn on_config(&mut self, now: Micros, cfg_ver: u8, params: &[u8], ev: &mut Vec<AsmEvent>) {
        todo!()
    }

    pub fn on_decoder_error(&mut self, now: Micros, fid: u64, ev: &mut Vec<AsmEvent>) {
        todo!()
    }

    pub fn on_timer(&mut self, now: Micros, ev: &mut Vec<AsmEvent>) {
        todo!()
    }

    pub fn next_timer(&self) -> Option<Micros> {
        todo!()
    }

    pub fn report(&self) -> ReportVideoFields {
        todo!()
    }
}
