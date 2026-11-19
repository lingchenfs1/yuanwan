//! 呈现时间量（规格 §9）：时钟映射、帧展开估计、三种模式的 J / S_b / R_wait；
//! VsyncScheduler（选帧，M2c）和 FreezeMeter（W3C 冻结统计，M2a 起），客户端和仿真器共用；
//! AvSyncMeter（音画差，M2b 只测）。

use crate::parity::Mode;
use ywp::time::Micros;

/// 本地时钟 ↔ 网关 SendTS 的映射（取最小单向时延作基准）
#[derive(Clone, Debug)]
pub struct ClockMap {
    _priv: (),
}

impl ClockMap {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_packet(&mut self, now: Micros, send_ts: u32) {
        todo!()
    }

    /// 本地时刻 − 网关时刻（微秒）
    pub fn delta(&self) -> i64 {
        todo!()
    }
}

/// 帧展开估计：L 包的 age
#[derive(Clone, Debug)]
pub struct SpreadEst {
    _priv: (),
}

impl SpreadEst {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_last_age(&mut self, now: Micros, age_us: u32) {
        todo!()
    }

    pub fn s50(&self) -> u32 {
        todo!()
    }

    pub fn s90(&self) -> u32 {
        todo!()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub mode: Mode,
    /// RTT < 2 ms 才算局域网
    pub lan: bool,
    /// 客户端配置的 j_ms 覆盖
    pub j_override_us: Option<u32>,
}

impl Policy {
    pub fn j_us(&self) -> u32 {
        todo!()
    }

    pub fn sb_us(&self, e: &SpreadEst) -> u32 {
        todo!()
    }

    /// 150 / 200 / 250 ms（竞技 / 均衡 / 稳定）
    pub fn r_wait_us(&self) -> u32 {
        todo!()
    }
}

/// 不碰 I/O 的选帧器：yw-client 的 SdlMetalPresenter 和 yw-sim 共用（M2c）
#[derive(Debug)]
pub struct VsyncScheduler {
    _priv: (),
}

impl VsyncScheduler {
    pub fn new() -> Self {
        todo!()
    }

    pub fn push(&mut self, fid: u64, p_us: Micros, decoded_us: Micros) {
        todo!()
    }

    /// 已解码且 P(f) ≤ vsync_at 的最新一帧，其余记跳显
    pub fn pick(&mut self, vsync_at: Micros) -> Option<u64> {
        todo!()
    }

    pub fn skipped(&self) -> u64 {
        todo!()
    }
}

/// W3C 冻结统计：呈现间隔 ≥ max(3 × 平均帧间隔, 平均帧间隔 + 150 ms) 记一次（M2a 起）
#[derive(Debug)]
pub struct FreezeMeter {
    _priv: (),
}

impl FreezeMeter {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_present(&mut self, at: Micros) {
        todo!()
    }

    /// (开始, 结束)
    pub fn freezes(&self) -> &[(Micros, Micros)] {
        todo!()
    }

    pub fn total_us(&self) -> u64 {
        todo!()
    }
}

/// 每秒一次的音画差样本
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AvSyncSample {
    pub at: Micros,
    pub audio_ms: f32,
    pub video_ms: f32,
    pub diff_ms: f32,
}

/// 音画差（M2b 只测）：两个量都是「本地时刻 − 网关时刻」，相减时时钟偏差抵消
#[derive(Debug)]
pub struct AvSyncMeter {
    _priv: (),
}

impl AvSyncMeter {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_audio_played(&mut self, at: Micros, send_ts: u32, age_us: u32) {
        todo!()
    }

    pub fn on_video_presented(&mut self, at: Micros, send_ts: u32, age_us: u32) {
        todo!()
    }

    pub fn per_second(&mut self, now: Micros) -> Option<AvSyncSample> {
        todo!()
    }
}
