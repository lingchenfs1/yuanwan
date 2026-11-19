//! 网关端点（B 组）。v1 只有一个控制客户端。
//!
//! - 同一 CID、同一客户端公钥算新的尝试，加进候选（最多 5 份，5 s 过期）；公钥不同才回 CID_COLLISION；
//!   收到 DATA 时逐份试解，先解开的那份确认；
//! - 接管发生在密钥确认的时刻；`TakeoverPolicy::RequireFlag`（默认）时另一台客户端没带接管位就回 BUSY；
//! - 半开会话 3 s，已确认会话 60 s 没有任何包就删除；RESET 全局每秒 ≤ 50 个；
//! - 密钥确认之后、并且不在 Held 时才发媒体。

use super::{ConnStats, RxMeta, Transmit};
use crate::consts::BASE_PAYLOAD;
use crate::creds::{ClientEntry, ClientId, Keys};
use crate::error::DropReason;
use crate::msg::ControlMsg;
use crate::reliable::CtlError;
use crate::rtt::RttEstimator;
use crate::sched::{SymbolSource, VideoCarrier};
use crate::time::Micros;
use crate::wire::{Cid, FeedbackState, FrameStatusEntry, HsInitPayload, InputChunk, Need, Report, SessionInfo};
use std::net::SocketAddr;

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// 来自 creds::load_keys
    pub keys: Keys,
    pub takeover: TakeoverPolicy,
}

/// 第二台客户端连上时怎么办（gateway.conf 的 ywp_takeover）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TakeoverPolicy {
    /// 默认（ywp_takeover=0）：没带接管位就回 BUSY
    #[default]
    RequireFlag,
    /// ywp_takeover=1
    Auto,
}

/// 上行存活状态（M2d）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// 500 ms 没有任何包：盲发档
    Blind,
    /// 10 s 没有任何包或收到 CLOSE：停发媒体，每 1 s 心跳，最多 60 s
    Held,
    /// 恢复
    Back,
}

#[derive(Debug)]
pub enum GwEvent<'a> {
    Connected { cid: Cid, client: ClientId, init: HsInitPayload },
    Replaced { old: Cid, new: Cid },
    Input(InputChunk<'a>, RxMeta),
    /// NACK 换算出的丢失符号 (fid, sym)
    Lost(&'a [(u16, u16)]),
    Need(Need),
    Report(Report<'a>),
    Control(ControlMsg),
    Liveness(Liveness),
    Closed(u16),
    PathChanged(SocketAddr),
    Dropped(DropReason),
}

#[derive(Debug)]
pub struct Gateway {
    _priv: (),
}

impl Gateway {
    pub fn new(cfg: GatewayConfig) -> Self {
        todo!()
    }

    /// 被删掉的客户端，会话立即删除
    pub fn reload_clients(&mut self, clients: Vec<ClientEntry>) {
        todo!()
    }

    /// 填进 HS_RESP 的 TLV 0x12
    pub fn set_session_info(&mut self, s: SessionInfo) {
        todo!()
    }

    pub fn handle_datagram(&mut self, now: Micros, from: SocketAddr, pkt: &mut [u8], sink: &mut dyn FnMut(GwEvent<'_>)) {
        todo!()
    }

    pub fn handle_timeout(&mut self, now: Micros, sink: &mut dyn FnMut(GwEvent<'_>)) {
        todo!()
    }

    pub fn poll_transmit(
        &mut self,
        now: Micros,
        video: &dyn SymbolSource,
        out: &mut [u8; BASE_PAYLOAD],
    ) -> Option<Transmit> {
        todo!()
    }

    pub fn next_timeout(&self) -> Option<Micros> {
        todo!()
    }

    /// v1 只有一个控制客户端
    pub fn conn(&mut self) -> Option<&mut GwConn> {
        todo!()
    }

    pub fn kick(&mut self, client: &ClientId, code: u16) {
        todo!()
    }
}

/// 网关上一个已建立的会话
#[derive(Debug)]
pub struct GwConn {
    _priv: (),
}

impl GwConn {
    /// 密钥已确认，并且不在 Held
    pub fn media_allowed(&self) -> bool {
        todo!()
    }

    pub fn queue_video(&mut self, v: VideoCarrier) {
        todo!()
    }

    pub fn queue_audio(&mut self, opus: Option<&[u8]>, arrival: Micros, now: Micros) {
        todo!()
    }

    pub fn set_feedback(&mut self, s: FeedbackState, now: Micros) {
        todo!()
    }

    /// 连续 3 个包各带一次（5 ms 内没有包就单独发）
    pub fn frame_status(&mut self, e: FrameStatusEntry) {
        todo!()
    }

    pub fn send_control(&mut self, m: &ControlMsg) -> Result<(), CtlError> {
        todo!()
    }

    pub fn rtt(&self) -> &RttEstimator {
        todo!()
    }

    /// 距上次收到任何包多久
    pub fn silence(&self, now: Micros) -> Micros {
        todo!()
    }

    pub fn stats(&self) -> ConnStats {
        todo!()
    }
}
