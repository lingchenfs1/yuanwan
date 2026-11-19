//! RTT 估计（B 组，规格 §5.5.5、§5.5.8）：PING/PONG 取样，RTT = 收到 PONG 的时刻 − echo_ts − hold。
//!
//! - 平滑按 RFC 6298：首个样本 SRTT = R、RTTVAR = R/2；之后 RTTVAR = ¾·RTTVAR + ¼·|SRTT − R|，SRTT = ⅞·SRTT + ⅛·R；
//! - 以握手 RTT 作种子（[`RttEstimator::seed`]）；CONTROL 的 RTO 由它给出，不从 ACK 取样本；
//! - RTO = max(25 ms, SRTT + 4 × RTTVAR)，封顶 1 s；没有任何样本时取 1 s；
//! - min、p50 取最近 [`RTT_WINDOW`] 个样本（PING 每 100 ms 一次，约 6.4 s）；
//! - 只认 ping_id 比上一个已采用的更新的 PONG，重复、乱序晚到的 PONG 不再取样。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::time::{send_ts, send_ts_diff, seq_lt, Micros, CTL_RTO_MAX, CTL_RTO_MIN};
use crate::wire::Pong;

/// min、p50 统计用的样本数
pub const RTT_WINDOW: usize = 64;
/// 比它还大的样本当作坏样本丢掉（SendTS 回绕约 71 分钟，正常 RTT 远小于它）
const RTT_SAMPLE_MAX: Micros = 30_000_000;

#[derive(Clone, Debug)]
pub struct RttEstimator {
    /// 平滑值，单位 µs（None = 还没有任何样本或种子）
    srtt: Option<Micros>,
    rttvar: Micros,
    latest: Micros,
    /// 最近的样本（环形）
    ring: [Micros; RTT_WINDOW],
    ring_len: usize,
    ring_pos: usize,
    last_ping: Option<u16>,
    samples: u64,
}

impl Default for RttEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl RttEstimator {
    pub fn new() -> Self {
        Self {
            srtt: None,
            rttvar: 0,
            latest: 0,
            ring: [0; RTT_WINDOW],
            ring_len: 0,
            ring_pos: 0,
            last_ping: None,
            samples: 0,
        }
    }

    /// 用握手 RTT 作种子：重置平滑值（SRTT = rtt、RTTVAR = rtt/2），并计入 min、p50 的样本
    pub fn seed(&mut self, rtt: Micros) {
        self.srtt = Some(rtt);
        self.rttvar = rtt / 2;
        self.record(rtt);
    }

    /// 收到 PONG：返回这次的样本；坏样本、重复或晚到的 PONG 返回 None
    pub fn on_pong(&mut self, now: Micros, p: &Pong) -> Option<Micros> {
        if let Some(last) = self.last_ping {
            if !seq_lt(last, p.ping_id) {
                return None;
            }
        }
        let raw = send_ts_diff(send_ts(now), p.echo_ts);
        if raw < 0 {
            return None;
        }
        let hold = p.hold_10us as Micros * 10;
        let raw = raw as Micros;
        if raw < hold {
            return None;
        }
        let r = raw - hold;
        if r > RTT_SAMPLE_MAX {
            return None;
        }
        self.last_ping = Some(p.ping_id);
        self.sample(r);
        Some(r)
    }

    /// 直接喂一个样本（测试、仿真用；端点只走 seed 和 on_pong）
    pub fn sample(&mut self, r: Micros) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(s) => {
                let d = s.abs_diff(r);
                self.rttvar = (3 * self.rttvar + d) / 4;
                self.srtt = Some((7 * s + r) / 8);
            }
        }
        self.record(r);
    }

    fn record(&mut self, r: Micros) {
        self.latest = r;
        self.samples += 1;
        self.ring[self.ring_pos] = r;
        self.ring_pos = (self.ring_pos + 1) % RTT_WINDOW;
        self.ring_len = (self.ring_len + 1).min(RTT_WINDOW);
    }

    /// max(25 ms, SRTT + 4 × RTTVAR)，封顶 1 s；还没有样本时 1 s
    pub fn rto(&self) -> Micros {
        match self.srtt {
            None => CTL_RTO_MAX,
            Some(s) => (s + 4 * self.rttvar).clamp(CTL_RTO_MIN, CTL_RTO_MAX),
        }
    }

    /// 平滑 RTT；还没有样本时 0
    pub fn srtt(&self) -> Micros {
        self.srtt.unwrap_or(0)
    }

    pub fn rttvar(&self) -> Micros {
        self.rttvar
    }

    /// 最近 RTT_WINDOW 个样本的最小值；还没有样本时 0
    pub fn min(&self) -> Micros {
        self.ring[..self.ring_len].iter().copied().min().unwrap_or(0)
    }

    /// 最近 RTT_WINDOW 个样本的中位数（偶数个时取下中位数）；还没有样本时 0
    pub fn p50(&self) -> Micros {
        if self.ring_len == 0 {
            return 0;
        }
        let mut v = [0; RTT_WINDOW];
        let v = &mut v[..self.ring_len];
        v.copy_from_slice(&self.ring[..self.ring_len]);
        let mid = (self.ring_len - 1) / 2;
        *v.select_nth_unstable(mid).1
    }

    /// 最近一个样本；还没有样本时 0
    pub fn latest(&self) -> Micros {
        self.latest
    }

    /// 累计样本数（含种子）
    pub fn samples(&self) -> u64 {
        self.samples
    }

    pub fn has_sample(&self) -> bool {
        self.srtt.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::ms;

    fn pong(id: u16, sent_at: Micros, hold_us: Micros) -> Pong {
        Pong { ping_id: id, echo_ts: send_ts(sent_at), hold_10us: (hold_us / 10) as u16 }
    }

    #[test]
    fn unseeded_rto_is_one_second() {
        let r = RttEstimator::new();
        assert_eq!(r.rto(), CTL_RTO_MAX);
        assert_eq!(r.srtt(), 0);
        assert_eq!(r.p50(), 0);
    }

    #[test]
    fn seed_then_rto_floor_and_cap() {
        let mut r = RttEstimator::new();
        r.seed(ms(2));
        // 2 + 4 × 1 = 6 ms < 25 ms
        assert_eq!(r.rto(), CTL_RTO_MIN);
        r.seed(ms(19));
        // 19 + 4 × 9.5 = 57 ms
        assert_eq!(r.rto(), 57_000);
        r.seed(ms(500));
        assert_eq!(r.rto(), CTL_RTO_MAX);
    }

    #[test]
    fn pong_sample_subtracts_hold_and_smooths() {
        let mut r = RttEstimator::new();
        r.seed(ms(20));
        // PING 在 1000 ms 发出，对端压了 0.5 ms，1021 ms 收到 PONG → 样本 20.5 ms
        assert_eq!(r.on_pong(ms(1021), &pong(1, ms(1000), 500)), Some(20_500));
        assert_eq!(r.srtt(), (7 * 20_000 + 20_500) / 8);
        assert_eq!(r.latest(), 20_500);
    }

    #[test]
    fn duplicate_and_stale_pongs_ignored() {
        let mut r = RttEstimator::new();
        assert!(r.on_pong(ms(30), &pong(5, ms(10), 0)).is_some());
        // 同一个 PONG 又来一份（链路重复）
        assert!(r.on_pong(ms(31), &pong(5, ms(10), 0)).is_none());
        // 更早的 PING 晚到
        assert!(r.on_pong(ms(40), &pong(4, ms(0), 0)).is_none());
        // ping_id 回绕后仍然认
        let mut r = RttEstimator::new();
        assert!(r.on_pong(ms(30), &pong(0xFFFF, ms(10), 0)).is_some());
        assert!(r.on_pong(ms(130), &pong(0, ms(110), 0)).is_some());
        assert_eq!(r.samples(), 2);
    }

    #[test]
    fn bogus_samples_rejected() {
        let mut r = RttEstimator::new();
        // echo_ts 在将来
        assert!(r.on_pong(ms(10), &pong(1, ms(20), 0)).is_none());
        // hold 比原始差值还大
        assert!(r.on_pong(ms(20), &pong(2, ms(19), 2_000)).is_none());
        assert!(!r.has_sample());
    }

    #[test]
    fn send_ts_wrap_is_handled() {
        let mut r = RttEstimator::new();
        // 本端时钟跨过 2³² µs
        let sent = (1u64 << 32) - 5_000;
        let now = (1u64 << 32) + 15_000;
        assert_eq!(r.on_pong(now, &pong(1, sent, 0)), Some(20_000));
    }

    #[test]
    fn spike_raises_rto_then_decays() {
        // 19 ms 的链路上来一个 437 ms 的尖峰
        let mut r = RttEstimator::new();
        r.seed(ms(19));
        for _ in 0..50 {
            r.sample(ms(19));
        }
        let steady = r.rto();
        assert!(steady >= CTL_RTO_MIN && steady < ms(30), "{steady}");
        r.sample(ms(437));
        let spiked = r.rto();
        assert!(spiked > ms(250), "{spiked}");
        for _ in 0..100 {
            r.sample(ms(19));
        }
        assert!(r.rto() < ms(30));
        // p50、min 不受单个尖峰影响
        assert_eq!(r.p50(), ms(19));
        assert_eq!(r.min(), ms(19));
    }

    #[test]
    fn window_min_and_median() {
        let mut r = RttEstimator::new();
        for v in [5, 1, 9, 3, 7] {
            r.sample(ms(v));
        }
        assert_eq!(r.min(), ms(1));
        assert_eq!(r.p50(), ms(5));
        // 窗口滚动后，旧的最小值淘汰
        for _ in 0..RTT_WINDOW {
            r.sample(ms(10));
        }
        assert_eq!(r.min(), ms(10));
    }
}
