//! 客户端端点（B 组）。
//!
//! 握手：首包双发（间隔 10 ms），之后 250 ms~2 s 重传；同一次连接的多次尝试共用 CID，PN = 尝试序号；
//! 保留最近 5 次尝试的状态，5 s 过期。发出第一个 DATA 后 1 s 没回音就重握；30 s 没有任何包放弃旧密钥；
//! 收到有效 RESET 立即重握。只接收当前对端地址的包。

use super::{ConnStats, RxMeta, Transmit};
use crate::consts::BASE_PAYLOAD;
use crate::creds::ClientCreds;
use crate::error::DropReason;
use crate::msg::ControlMsg;
use crate::reliable::CtlError;
use crate::time::Micros;
use crate::wire::{
    AudioChunk, FeedbackState, FrameStatusEntry, HsErr, HsInitPayload, HsRespPayload, InputState, Need,
    ReportVideoFields, UpReport, VideoChunk,
};
use std::net::SocketAddr;

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub creds: ClientCreds,
    /// 网关入口地址（从客户端配置读）
    pub entry: SocketAddr,
    /// 客户端名称，写进 TLV 0x07
    pub name: String,
    pub bitrate_cap_kbps: Option<u32>,
    /// 带接管位（用户在 BUSY 确认框里点了「接管」）
    pub takeover: bool,
}

/// 客户端事件（经 sink 回调交给调用方；Video / Audio 借用收包缓冲）
#[derive(Debug)]
pub enum ClientEvent<'a> {
    HandshakeOk(HsRespPayload),
    HandshakeRejected(HsErr),
    /// 密钥已确认（收到第一个认证通过的 DATA）
    Established,
    Video(VideoChunk<'a>, RxMeta),
    Audio(AudioChunk<'a>, RxMeta),
    Feedback(FeedbackState),
    FrameStatus(FrameStatusEntry),
    UpReport(UpReport<'a>),
    Control(ControlMsg),
    Rtt(Micros),
    Closed { code: u16 },
    Reset,
    Dropped(DropReason),
}

#[derive(Debug)]
pub struct ClientConn {
    _priv: (),
}

impl ClientConn {
    pub fn new(cfg: ClientConfig) -> Self {
        todo!()
    }

    /// 开始握手。init 里的 ts 由调用方读墙钟一次（Tai64N::next_after）后传入；之后每次重传都是新尝试，
    /// 时间戳 = ts0 + (now − now0)，按单调时钟推算、保证递增，ClientConn 自己不读墙钟（sans-IO）
    pub fn connect(&mut self, now: Micros, init: HsInitPayload) {
        todo!()
    }

    pub fn handle_datagram(
        &mut self,
        now: Micros,
        from: SocketAddr,
        pkt: &mut [u8],
        sink: &mut dyn FnMut(ClientEvent<'_>),
    ) {
        todo!()
    }

    pub fn handle_timeout(&mut self, now: Micros, sink: &mut dyn FnMut(ClientEvent<'_>)) {
        todo!()
    }

    /// 上行 ≤ 400 B
    pub fn poll_transmit(&mut self, now: Micros, out: &mut [u8; BASE_PAYLOAD]) -> Option<Transmit> {
        todo!()
    }

    pub fn next_timeout(&self) -> Option<Micros> {
        todo!()
    }

    pub fn input(&mut self, now: Micros, s: InputState, discrete: &[(u8, u8)]) {
        todo!()
    }

    pub fn send_control(&mut self, m: &ControlMsg) -> Result<(), CtlError> {
        todo!()
    }

    pub fn send_need(&mut self, n: Need) {
        todo!()
    }

    pub fn set_video_report(&mut self, v: ReportVideoFields) {
        todo!()
    }

    /// 在 0、5、10 ms 各发一份 CLOSE，然后丢弃密钥
    pub fn close(&mut self, code: u16, reason: &str) {
        todo!()
    }

    pub fn stats(&self) -> ConnStats {
        todo!()
    }
}
