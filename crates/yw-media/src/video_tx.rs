//! 网关侧视频发送（M2a 基础版，M2c 补修复）：切片、RS 编码、发送缓存、修复记账。

use crate::fec::FecError;
use crate::parity::Mode;
use ywp::sched::{SymbolSource, VideoCarrier};
use ywp::time::Micros;
use ywp::wire::{Need, Report};

/// Ingress 交给 VideoTx 的一帧
#[derive(Clone, Copy, Debug)]
pub struct FrameIn<'a> {
    pub fid: u16,
    pub key: bool,
    /// VIDEO_CONFIG 版本号的低 2 位
    pub cfg: u8,
    pub profile: u8,
    /// chiaki 改写过参考（写进 meta 的 W 位）
    pub recovered: bool,
    /// 本帧 t0（M2a~M2d 是 chiaki 交帧回调的时刻，M2e 起是首 unit 的 socket 收包时刻）
    pub t0: Micros,
    pub au: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedReply {
    Repairing,
    /// 发送缓存里已经没有这一帧（回 FRAME_STATUS GONE）
    Gone,
    /// 网关已在等关键帧（回 KEY_PENDING）
    KeyPending,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct VideoTxConfig {
    /// 发送缓存上限（默认 1 s / 8 MB）
    pub cache_age: Micros,
    pub cache_bytes: usize,
}

impl Default for VideoTxConfig {
    fn default() -> Self {
        Self { cache_age: ywp::time::SEND_CACHE_AGE, cache_bytes: ywp::time::SEND_CACHE_BYTES }
    }
}

#[cfg(test)]
mod config_tests {
    #[test]
    fn default_cache_is_not_zero() {
        let c = super::VideoTxConfig::default();
        assert_eq!(c.cache_age, ywp::time::SEND_CACHE_AGE);
        assert_eq!(c.cache_bytes, ywp::time::SEND_CACHE_BYTES);
    }
}

/// 模式、J、S_b90、RTT（归 C 组，字段可能还要加）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxParams {
    pub mode: Mode,
    pub j_us: u32,
    pub sb90_us: u32,
    pub rtt_p50_us: u32,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum TxError {
    /// k 超过 1024，网关不转发、进 WaitKey
    FrameTooLarge { len: usize },
    Fec(FecError),
}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct VideoTxStats {
    pub frames: u64,
    pub key_frames: u64,
    pub src_symbols: u64,
    pub parity_symbols: u64,
    pub repair_symbols: u64,
    pub bytes: u64,
}

#[derive(Debug)]
pub struct VideoTx {
    _priv: (),
}

impl VideoTx {
    pub fn new(cfg: VideoTxConfig) -> Self {
        todo!()
    }

    /// 切片、RS，源符号加初始校验交给调度
    pub fn send_frame(&mut self, now: Micros, f: &FrameIn, out: &mut Vec<VideoCarrier>) -> Result<(), TxError> {
        todo!()
    }

    pub fn on_sent(&mut self, now: Micros, fid: u16, sym: u16) {
        todo!()
    }

    pub fn on_lost(&mut self, now: Micros, lost: &[(u16, u16)], out: &mut Vec<VideoCarrier>) {
        todo!()
    }

    pub fn on_need(&mut self, now: Micros, n: &Need, out: &mut Vec<VideoCarrier>) -> NeedReply {
        todo!()
    }

    /// 和 GwEvent::Report 同一个类型（ywp::wire::Report）
    pub fn on_report(&mut self, now: Micros, r: &Report<'_>) {
        todo!()
    }

    pub fn set_params(&mut self, p: TxParams) {
        todo!()
    }

    pub fn set_blind(&mut self, blind: bool) {
        todo!()
    }

    pub fn on_timer(&mut self, now: Micros, out: &mut Vec<VideoCarrier>) {
        todo!()
    }

    pub fn next_timer(&self) -> Option<Micros> {
        todo!()
    }

    /// 停发媒体时清空
    pub fn clear(&mut self) {
        todo!()
    }

    pub fn stats(&self) -> &VideoTxStats {
        todo!()
    }
}

/// 从每帧那块连续的 (k + R) × S 缓冲里取
impl SymbolSource for VideoTx {
    fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]> {
        todo!()
    }
}
