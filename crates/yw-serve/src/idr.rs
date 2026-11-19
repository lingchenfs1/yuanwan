//! IdrGovernor（纯逻辑）：每秒 ≤ 1 次，同时 ≤ 1 个未决；1.2 s 重发；5 s 后 Escalate（M2d 接 L7）。
//! 网关里放在 PsLink 的 idr 锁里，tick 由监督线程每 100 ms 调；fakegw 自己持有一份。
//!
//! 规则（规格 §12、§7.4 第 3 条）：
//! - 「未决」：向 PS5 请求过 IDR，之后还没收到关键帧；
//! - 有未决时再来的请求一律合并（Coalesced），由 tick 在 1.2 s 没等到关键帧时重发；
//! - 没有未决、但离上一次真正发出不到 1 s 的请求推迟（Deferred），由 tick 在满 1 s 时发；
//!   推迟期间如果来了关键帧，推迟的请求作废；
//! - 未决从第一次发出算起 5 s 还没等到关键帧，tick 报一次 Escalate（每个未决期只报一次），之后照常重发。
//!
//! 时间一律用 `Micros`，调用方（ywp 线程的 request、监督线程的 tick）必须用同一个时钟（见 [`crate::clock`]）。

use crate::ingress::WaitWhy;
use ywp::time::{Micros, IDR_ESCALATE, IDR_MIN_INTERVAL, IDR_RESEND};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdrDecision {
    /// 已向 PS5 请求（调用方要真正调一次 PS5 会话的 request_idr）
    Requested,
    /// 合并进未决的那一次
    Coalesced,
    /// 限频，稍后由 tick 发
    Deferred,
    /// 5 s 还没等到关键帧
    Escalate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdrReason {
    /// 客户端 IDR_REQUEST，原因见 ywp::consts::idr_reason
    ClientRequest(u8),
    /// NEED 带 K 位
    NeedK,
    WaitKey(WaitWhy),
    NoFrames,
    /// 网关看门狗
    Watchdog,
    DecoderError,
}

/// 一个未决期
#[derive(Clone, Copy, Debug)]
struct Pending {
    /// 第一次发出的时刻：之后到达的关键帧才算数，Escalate 也从这里算
    since: Micros,
    /// 最近一次发出（含重发）的时刻
    last_sent: Micros,
    why: IdrReason,
    escalated: bool,
}

/// 计数（给控制口 status 和 JSONL 用）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct IdrStats {
    /// 真正发给 PS5 的次数（含重发）
    pub sent: u64,
    pub resent: u64,
    pub coalesced: u64,
    pub deferred: u64,
    pub escalated: u64,
    /// 等到了关键帧的未决期数
    pub satisfied: u64,
}

#[derive(Debug)]
pub struct IdrGovernor {
    pending: Option<Pending>,
    /// 推迟中的请求：(第一次被推迟的时刻, 原因)
    deferred: Option<(Micros, IdrReason)>,
    /// 最近一次真正发出的时刻（限频用）
    last_sent: Option<Micros>,
    stats: IdrStats,
}

impl Default for IdrGovernor {
    fn default() -> Self {
        Self::new()
    }
}

impl IdrGovernor {
    pub fn new() -> Self {
        Self { pending: None, deferred: None, last_sent: None, stats: IdrStats::default() }
    }

    pub fn request(&mut self, now: Micros, why: IdrReason) -> IdrDecision {
        if self.pending.is_some() {
            self.stats.coalesced += 1;
            return IdrDecision::Coalesced;
        }
        if self.last_sent.is_some_and(|t| now.saturating_sub(t) < IDR_MIN_INTERVAL) {
            if self.deferred.is_none() {
                self.deferred = Some((now, why));
            }
            self.stats.deferred += 1;
            return IdrDecision::Deferred;
        }
        self.deferred = None;
        self.start_pending(now, why);
        IdrDecision::Requested
    }

    /// 由网关监督线程每 100 ms 调；last_key_us：最近一次收到关键帧的时刻（0 表示还没有）。
    /// 返回 `Some(Requested)` 时调用方要真正向 PS5 请求一次
    pub fn tick(&mut self, now: Micros, last_key_us: Micros) -> Option<IdrDecision> {
        if let Some(p) = self.pending {
            if last_key_us > p.since {
                self.pending = None;
                self.stats.satisfied += 1;
            }
        }
        if let Some((at, _)) = self.deferred {
            if last_key_us > at {
                self.deferred = None;
            }
        }
        if let Some(p) = self.pending.as_mut() {
            if !p.escalated && now.saturating_sub(p.since) >= IDR_ESCALATE {
                p.escalated = true;
                self.stats.escalated += 1;
                return Some(IdrDecision::Escalate);
            }
            if now.saturating_sub(p.last_sent) >= IDR_RESEND {
                p.last_sent = now;
                self.last_sent = Some(now);
                self.stats.sent += 1;
                self.stats.resent += 1;
                return Some(IdrDecision::Requested);
            }
            return None;
        }
        if let Some((_, why)) = self.deferred {
            if self.last_sent.is_none_or(|t| now.saturating_sub(t) >= IDR_MIN_INTERVAL) {
                self.deferred = None;
                self.start_pending(now, why);
                return Some(IdrDecision::Requested);
            }
        }
        None
    }

    /// PS5 会话换了（取走、重建）：未决和推迟的请求都作废，新会话从关键帧开始。限频的时刻保留
    pub fn reset(&mut self) {
        self.pending = None;
        self.deferred = None;
    }

    /// 有没有未决的请求（发出过、还没等到关键帧）
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 当前未决期的原因（排查用）
    pub fn pending_reason(&self) -> Option<IdrReason> {
        self.pending.map(|p| p.why)
    }

    pub fn stats(&self) -> IdrStats {
        self.stats
    }

    fn start_pending(&mut self, now: Micros, why: IdrReason) {
        self.pending = Some(Pending { since: now, last_sent: now, why, escalated: false });
        self.last_sent = Some(now);
        self.stats.sent += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ywp::time::ms;

    const WHY: IdrReason = IdrReason::NoFrames;

    /// 按监督线程的节奏（100 ms 一拍）从 from 跑到 to，收集每拍的结果
    fn run(g: &mut IdrGovernor, from: Micros, to: Micros, key: Micros) -> Vec<(Micros, IdrDecision)> {
        let mut out = Vec::new();
        let mut t = from;
        while t <= to {
            if let Some(d) = g.tick(t, key) {
                out.push((t, d));
            }
            t += ms(100);
        }
        out
    }

    #[test]
    fn first_request_goes_out_then_coalesces() {
        let mut g = IdrGovernor::new();
        assert_eq!(g.request(ms(1_000), WHY), IdrDecision::Requested);
        assert!(g.is_pending());
        assert_eq!(g.request(ms(1_050), IdrReason::DecoderError), IdrDecision::Coalesced);
        assert_eq!(g.request(ms(3_000), WHY), IdrDecision::Coalesced);
        assert_eq!(g.pending_reason(), Some(WHY));
        assert_eq!(g.stats().sent, 1);
        assert_eq!(g.stats().coalesced, 2);
    }

    #[test]
    fn keyframe_satisfies_pending() {
        let mut g = IdrGovernor::new();
        assert_eq!(g.request(ms(1_000), WHY), IdrDecision::Requested);
        // 关键帧比请求早：不算
        assert_eq!(g.tick(ms(1_100), ms(900)), None);
        assert!(g.is_pending());
        // 请求之后来了关键帧
        assert_eq!(g.tick(ms(1_200), ms(1_150)), None);
        assert!(!g.is_pending());
        assert_eq!(g.stats().satisfied, 1);
    }

    #[test]
    fn resend_every_1200ms_and_escalate_once_at_5s() {
        let mut g = IdrGovernor::new();
        let t0 = ms(10_000);
        assert_eq!(g.request(t0, WHY), IdrDecision::Requested);
        let ev = run(&mut g, t0 + ms(100), t0 + ms(12_000), 0);
        let resends: Vec<Micros> = ev.iter().filter(|(_, d)| *d == IdrDecision::Requested).map(|(t, _)| t - t0).collect();
        let esc: Vec<Micros> = ev.iter().filter(|(_, d)| *d == IdrDecision::Escalate).map(|(t, _)| t - t0).collect();
        assert_eq!(esc, vec![ms(5_000)], "只报一次 Escalate，在第 5 s");
        // 1.2 s 一次；5 s 那一拍报了 Escalate，重发顺延到下一拍
        assert_eq!(resends[..4], [ms(1_200), ms(2_400), ms(3_600), ms(4_800)]);
        assert_eq!(resends[4], ms(6_000));
        assert!(resends.windows(2).all(|w| w[1] - w[0] >= IDR_RESEND));
        assert_eq!(g.stats().escalated, 1);
        assert_eq!(g.stats().resent as usize, resends.len());
    }

    #[test]
    fn at_most_one_per_second() {
        let mut g = IdrGovernor::new();
        let t0 = ms(5_000);
        assert_eq!(g.request(t0, WHY), IdrDecision::Requested);
        // 关键帧很快就来了，未决结束
        assert_eq!(g.tick(t0 + ms(100), t0 + ms(80)), None);
        // 不到 1 s 又来请求：推迟
        assert_eq!(g.request(t0 + ms(300), IdrReason::DecoderError), IdrDecision::Deferred);
        assert_eq!(g.request(t0 + ms(400), WHY), IdrDecision::Deferred);
        // 满 1 s 之前 tick 不发
        assert_eq!(g.tick(t0 + ms(900), t0 + ms(80)), None);
        assert_eq!(g.tick(t0 + ms(1_000), t0 + ms(80)), Some(IdrDecision::Requested));
        assert!(g.is_pending());
        assert_eq!(g.pending_reason(), Some(IdrReason::DecoderError), "保留第一次被推迟的原因");
        assert_eq!(g.stats().sent, 2);
        assert_eq!(g.stats().deferred, 2);
    }

    #[test]
    fn keyframe_cancels_deferred() {
        let mut g = IdrGovernor::new();
        let t0 = ms(5_000);
        g.request(t0, WHY);
        g.tick(t0 + ms(100), t0 + ms(50));
        assert_eq!(g.request(t0 + ms(200), WHY), IdrDecision::Deferred);
        // 推迟之后 PS5 自己发了关键帧：推迟的请求作废
        assert_eq!(g.tick(t0 + ms(1_100), t0 + ms(600)), None);
        assert!(!g.is_pending());
        assert_eq!(g.tick(t0 + ms(2_000), t0 + ms(600)), None);
        assert_eq!(g.stats().sent, 1);
    }

    #[test]
    fn after_interval_request_goes_out_directly() {
        let mut g = IdrGovernor::new();
        g.request(ms(1_000), WHY);
        g.tick(ms(1_100), ms(1_050));
        assert_eq!(g.request(ms(2_000), WHY), IdrDecision::Requested);
    }

    #[test]
    fn reset_drops_pending_but_keeps_rate_limit() {
        let mut g = IdrGovernor::new();
        g.request(ms(1_000), WHY);
        g.reset();
        assert!(!g.is_pending());
        assert_eq!(run(&mut g, ms(1_100), ms(8_000), 0), vec![]);
        // 限频的时刻还在：离上一次发出不到 1 s 的请求照样推迟
        let mut g = IdrGovernor::new();
        g.request(ms(1_000), WHY);
        g.reset();
        assert_eq!(g.request(ms(1_500), WHY), IdrDecision::Deferred);
        assert_eq!(g.tick(ms(2_000), 0), Some(IdrDecision::Requested));
    }

    #[test]
    fn no_request_no_output() {
        let mut g = IdrGovernor::new();
        assert_eq!(run(&mut g, 0, ms(10_000), ms(3_000)), vec![]);
        assert_eq!(g.stats(), IdrStats::default());
    }
}
