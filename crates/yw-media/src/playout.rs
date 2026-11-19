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

// 骨架（M2c 实现时去掉）
#[allow(unused_variables, clippy::new_without_default)]
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

// 骨架（M2c 实现时去掉）
#[allow(unused_variables, clippy::new_without_default)]
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

/// 均衡模式非局域网的 J：1 帧（规格 §9.2）
const J_ONE_FRAME_US: u32 = 16_667;
/// 稳定模式的 J：2 帧
const J_TWO_FRAMES_US: u32 = 33_333;

impl Policy {
    /// 播放余量 J（规格 §9.2）：竞技 0；均衡局域网 0、其他 16.7 ms；稳定 33.3 ms；客户端配置的 j_ms 优先
    pub fn j_us(&self) -> u32 {
        if let Some(j) = self.j_override_us {
            return j;
        }
        match self.mode {
            Mode::Competitive => 0,
            Mode::Balanced if self.lan => 0,
            Mode::Balanced => J_ONE_FRAME_US,
            Mode::Stable => J_TWO_FRAMES_US,
        }
    }

    #[allow(unused_variables)] // 骨架（M2c 实现时去掉）
    pub fn sb_us(&self, e: &SpreadEst) -> u32 {
        todo!()
    }

    /// 150 / 200 / 250 ms（竞技 / 均衡 / 稳定）
    pub fn r_wait_us(&self) -> u32 {
        let i = match self.mode {
            Mode::Competitive => 0,
            Mode::Balanced => 1,
            Mode::Stable => 2,
        };
        ywp::time::R_WAIT[i] as u32
    }
}

/// 不碰 I/O 的选帧器：yw-client 的 SdlMetalPresenter 和 yw-sim 共用（M2c）
#[derive(Debug)]
pub struct VsyncScheduler {
    _priv: (),
}

// 骨架（M2c 实现时去掉）
#[allow(unused_variables, clippy::new_without_default)]
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

/// 平均帧间隔取最近这么多个间隔（和 libwebrtc 统计 freezeCount 的做法一致）
const FREEZE_AVG_WINDOW: usize = 30;

/// W3C 冻结统计：呈现间隔 ≥ max(3 × 平均帧间隔, 平均帧间隔 + 150 ms) 记一次（M2a 起）。
///
/// 口径同 W3C webrtc-stats 的 freezeCount / totalFreezesDuration：平均帧间隔取此前最近 30 个呈现间隔
/// （含冻结的那些）的均值，判完再把本次间隔加进窗口；第一个间隔之前没有均值，不判。
/// 冻结记成 (上一帧呈现时刻, 本帧呈现时刻)，时长就是这个间隔。
#[derive(Debug, Default)]
pub struct FreezeMeter {
    last: Option<Micros>,
    window: std::collections::VecDeque<Micros>,
    sum: Micros,
    freezes: Vec<(Micros, Micros)>,
    total: u64,
}

impl FreezeMeter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_present(&mut self, at: Micros) {
        let Some(last) = self.last.replace(at) else { return };
        let gap = at.saturating_sub(last);
        if !self.window.is_empty() {
            let avg = self.sum / self.window.len() as Micros;
            let limit = (ywp::time::FREEZE_MULT * avg).max(avg + ywp::time::FREEZE_EXTRA);
            if gap >= limit {
                self.freezes.push((last, at));
                self.total += gap;
            }
        }
        self.window.push_back(gap);
        self.sum += gap;
        if self.window.len() > FREEZE_AVG_WINDOW {
            self.sum -= self.window.pop_front().unwrap_or(0);
        }
    }

    /// (开始, 结束)
    pub fn freezes(&self) -> &[(Micros, Micros)] {
        &self.freezes
    }

    pub fn total_us(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ywp::time::ms;

    /// 60 fps 的呈现时刻（微秒取整）
    fn at60(i: u64) -> Micros {
        i * 1_000_000 / 60
    }

    #[test]
    fn steady_60fps_has_no_freeze() {
        let mut m = FreezeMeter::new();
        for i in 0..600 {
            m.on_present(at60(i));
        }
        assert!(m.freezes().is_empty());
        assert_eq!(m.total_us(), 0);
    }

    #[test]
    fn threshold_is_avg_plus_150ms_at_60fps() {
        // 平均 16.67 ms：门限 = max(50 ms, 166.67 ms) = 166.67 ms
        let mut m = FreezeMeter::new();
        for i in 0..60 {
            m.on_present(at60(i));
        }
        let t = at60(59);
        // 间隔 160 ms：不算冻结
        m.on_present(t + ms(160));
        assert!(m.freezes().is_empty());
        // 再稳定一阵子，让均值回到 1 帧附近
        let base = t + ms(160);
        for i in 1..=60 {
            m.on_present(base + at60(i));
        }
        let t2 = base + at60(60);
        // 间隔 200 ms：冻结一次
        m.on_present(t2 + ms(200));
        assert_eq!(m.freezes(), &[(t2, t2 + ms(200))]);
        assert_eq!(m.total_us(), ms(200));
    }

    #[test]
    fn threshold_is_three_times_avg_at_low_fps() {
        // 10 fps：平均 100 ms，门限 = max(300 ms, 250 ms) = 300 ms
        let mut m = FreezeMeter::new();
        for i in 0..40 {
            m.on_present(i * ms(100));
        }
        let t = 39 * ms(100);
        m.on_present(t + ms(290));
        assert!(m.freezes().is_empty());
        let t = t + ms(290);
        for i in 1..=40 {
            m.on_present(t + i * ms(100));
        }
        let t = t + 40 * ms(100);
        m.on_present(t + ms(310));
        assert_eq!(m.freezes().len(), 1);
        assert_eq!(m.total_us(), ms(310));
    }

    #[test]
    fn first_gap_never_counts_and_repeated_freezes_add_up() {
        let mut m = FreezeMeter::new();
        m.on_present(0);
        m.on_present(secs_(5));
        assert!(m.freezes().is_empty());
        // 之后 60 fps，中间卡 3 次各 500 ms
        let mut t = secs_(5);
        for round in 0..3 {
            for _ in 0..60 {
                t += 16_667;
                m.on_present(t);
            }
            t += ms(500);
            m.on_present(t);
            assert_eq!(m.freezes().len(), round + 1);
        }
        assert_eq!(m.total_us(), 3 * ms(500));
    }

    fn secs_(v: u64) -> Micros {
        ywp::time::secs(v)
    }

    #[test]
    fn policy_j_and_r_wait() {
        let p = |mode, lan, j| Policy { mode, lan, j_override_us: j };
        assert_eq!(p(Mode::Competitive, false, None).j_us(), 0);
        assert_eq!(p(Mode::Balanced, true, None).j_us(), 0);
        assert_eq!(p(Mode::Balanced, false, None).j_us(), 16_667);
        assert_eq!(p(Mode::Stable, true, None).j_us(), 33_333);
        assert_eq!(p(Mode::Stable, false, Some(50_000)).j_us(), 50_000);
        assert_eq!(p(Mode::Competitive, false, None).r_wait_us(), 150_000);
        assert_eq!(p(Mode::Balanced, false, None).r_wait_us(), 200_000);
        assert_eq!(p(Mode::Stable, false, None).r_wait_us(), 250_000);
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

// 骨架（M2b 实现时去掉）
#[allow(unused_variables, clippy::new_without_default)]
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
