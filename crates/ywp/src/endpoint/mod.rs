//! 两端的端点状态机（B 组）：握手、加密、PN、CONTROL、调度都在这里汇合。
//! 调用方喂数据报和 now，取要发的包（poll_transmit）和下一次超时（next_timeout）。
//!
//! 驱动方式（两端一样，sans-IO，不读墙钟）：
//! - 收到数据报：`handle_datagram(now, from, pkt, sink)`；
//! - 到了 `next_timeout()`：`handle_timeout(now, sink)`；
//! - 上面两件事之后，以及调用方往里塞了东西（控制消息、视频符号……）之后，
//!   反复 `poll_transmit` 直到返回 None，把每个包发给 `Transmit::dst`。
//!
//! 本文件另有两端共用、不依赖线上编码的两块逻辑：
//! - [`HsSchedule`]：客户端握手的发送节奏（规格 §4.8）；
//! - [`AttemptRing`]：同一 CID 最近 5 次握手尝试的状态，5 s 过期（规格 §4.3，两端一致）。

// lib.rs 为第 0 步骨架整体放宽了这几项；本模块已实现完，重新打开
#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod client;
pub mod gateway;
mod session;

pub use client::{ClientConfig, ClientConn, ClientEvent};
pub use gateway::{GatewayConfig, Gateway, GwConn, GwEvent, Liveness, TakeoverPolicy};

use crate::consts::HS_ATTEMPTS_KEPT;
use crate::error::DropCounters;
use crate::time::{Micros, HS_ATTEMPT_TTL, HS_BACKOFF, HS_BACKOFF_CAP, HS_FIRST_DUP};
use std::net::SocketAddr;

/// 握手的一次发送
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HsSend {
    /// 尝试序号（填进 HS_INIT 外层头的 PN）
    pub attempt: u32,
    /// true 表示首包的第二份：和第一份是同一个包（同一 PN、同一时间戳），原样重发
    pub copy: bool,
}

/// 客户端握手的发送节奏（规格 §4.8）：首包发两份，间隔 10 ms；之后按 250 ms、500 ms、1 s、2 s 重传，
/// 封顶 2 s，直到成功或取消。每次重传都是新的尝试（新的临时密钥、新的时间戳，PN = 尝试序号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HsSchedule {
    /// 下一个尝试的序号
    next_attempt: u32,
    /// 下一个新尝试的时刻
    next_at: Micros,
    /// 首包第二份的时刻
    copy_at: Option<Micros>,
}

impl HsSchedule {
    /// 从 now 开始握手：第 0 次尝试立即发
    pub fn start(now: Micros) -> Self {
        Self { next_attempt: 0, next_at: now, copy_at: None }
    }

    /// 到期的发送（一次只给一个；同一时刻都到期时先给首包第二份）
    pub fn poll(&mut self, now: Micros) -> Option<HsSend> {
        if let Some(t) = self.copy_at {
            if now >= t {
                self.copy_at = None;
                return Some(HsSend { attempt: 0, copy: true });
            }
        }
        if now < self.next_at {
            return None;
        }
        let a = self.next_attempt;
        if a == 0 {
            self.copy_at = Some(now + HS_FIRST_DUP);
        }
        let gap = HS_BACKOFF.get(a as usize).copied().unwrap_or(HS_BACKOFF_CAP).min(HS_BACKOFF_CAP);
        self.next_at = now + gap;
        self.next_attempt = a.saturating_add(1);
        Some(HsSend { attempt: a, copy: false })
    }

    pub fn next_deadline(&self) -> Micros {
        self.copy_at.map_or(self.next_at, |c| c.min(self.next_at))
    }

    /// 已经开始的尝试次数（统计「用了几次 HS_INIT」）
    pub fn attempts(&self) -> u32 {
        self.next_attempt
    }
}

/// 同一 CID 最近几次握手尝试的状态（两端一致：最多 5 份，5 s 过期）。
/// 客户端按 HS_RESP 回显的 PN 找对应的那次尝试；网关收到 DATA 时逐份试解。
#[derive(Clone, Debug)]
pub struct AttemptRing<T> {
    items: Vec<(u32, Micros, T)>,
}

impl<T> Default for AttemptRing<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> AttemptRing<T> {
    pub fn new() -> Self {
        Self { items: Vec::with_capacity(HS_ATTEMPTS_KEPT) }
    }

    /// 加一份；超过 5 份时挤掉最旧的。同一序号再加一次时替换旧的
    pub fn push(&mut self, attempt: u32, now: Micros, state: T) {
        self.items.retain(|(a, _, _)| *a != attempt);
        if self.items.len() >= HS_ATTEMPTS_KEPT {
            self.items.remove(0);
        }
        self.items.push((attempt, now, state));
    }

    /// 去掉 5 s 前的
    pub fn expire(&mut self, now: Micros) {
        self.items.retain(|(_, t, _)| now.saturating_sub(*t) < HS_ATTEMPT_TTL);
    }

    pub fn get(&self, attempt: u32, now: Micros) -> Option<&T> {
        self.items
            .iter()
            .find(|(a, t, _)| *a == attempt && now.saturating_sub(*t) < HS_ATTEMPT_TTL)
            .map(|(_, _, s)| s)
    }

    pub fn get_mut(&mut self, attempt: u32, now: Micros) -> Option<&mut T> {
        self.items
            .iter_mut()
            .find(|(a, t, _)| *a == attempt && now.saturating_sub(*t) < HS_ATTEMPT_TTL)
            .map(|(_, _, s)| s)
    }

    /// 取走某次尝试的状态（确认之后其余的可以 clear）
    pub fn take(&mut self, attempt: u32, now: Micros) -> Option<T> {
        let i = self
            .items
            .iter()
            .position(|(a, t, _)| *a == attempt && now.saturating_sub(*t) < HS_ATTEMPT_TTL)?;
        Some(self.items.remove(i).2)
    }

    /// 没过期的，从新到旧（网关逐份试解时用：最新的那份最可能对）
    pub fn iter_live(&self, now: Micros) -> impl Iterator<Item = (u32, &T)> {
        self.items
            .iter()
            .rev()
            .filter(move |(_, t, _)| now.saturating_sub(*t) < HS_ATTEMPT_TTL)
            .map(|(a, _, s)| (*a, s))
    }

    /// 最早的一份什么时候过期
    pub fn next_expiry(&self) -> Option<Micros> {
        self.items.iter().map(|(_, t, _)| t + HS_ATTEMPT_TTL).min()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }
}

/// poll_transmit 的结果：包已写进调用方给的缓冲
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transmit {
    pub dst: SocketAddr,
    pub len: usize,
    /// 这个包装的是哪个视频符号 (fid, sym)，发出后要回报给 VideoTx::on_sent
    pub video: Option<(u16, u16)>,
}

/// 收到的 DATA 包的元数据
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxMeta {
    pub pn: u64,
    pub send_ts: u32,
    /// 收包时刻（net 线程收包后立即打的单调时间戳）
    pub arrival: Micros,
}

/// 连接统计
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ConnStats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    /// 本次连接用了几次 HS_INIT 尝试
    pub hs_attempts: u32,
    pub srtt_us: Micros,
    pub drops: DropCounters,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::{ms, secs};

    #[test]
    fn hs_schedule_dup_then_backoff() {
        let mut s = HsSchedule::start(1_000);
        let mut sends = Vec::new();
        let mut t = 1_000;
        while t <= 1_000 + secs(10) {
            while let Some(x) = s.poll(t) {
                sends.push((t - 1_000, x.attempt, x.copy));
            }
            t = s.next_deadline();
        }
        assert_eq!(
            &sends[..7],
            &[
                (0, 0, false),
                (ms(10), 0, true),
                (ms(250), 1, false),
                (ms(750), 2, false),
                (ms(1_750), 3, false),
                (ms(3_750), 4, false),
                (ms(5_750), 5, false),
            ]
        );
        assert_eq!(sends[7], (ms(7_750), 6, false));
        assert_eq!(s.attempts(), 8);
    }

    #[test]
    fn attempt_ring_keeps_five_for_five_seconds() {
        let mut r: AttemptRing<&str> = AttemptRing::new();
        for a in 0..7u32 {
            r.push(a, ms(100) * a as u64, "s");
        }
        assert_eq!(r.len(), HS_ATTEMPTS_KEPT);
        assert!(r.get(1, ms(700)).is_none());
        assert!(r.get(2, ms(700)).is_some());
        assert_eq!(r.iter_live(ms(700)).map(|(a, _)| a).collect::<Vec<_>>(), vec![6, 5, 4, 3, 2]);
        // 第 2 次尝试在 200 ms 加入，5.2 s 时过期
        assert_eq!(r.next_expiry(), Some(ms(200) + HS_ATTEMPT_TTL));
        assert!(r.get(2, ms(5_199)).is_some());
        assert!(r.get(2, ms(5_200)).is_none());
        r.expire(ms(5_350));
        assert_eq!(r.iter_live(ms(5_350)).map(|(a, _)| a).collect::<Vec<_>>(), vec![6, 5, 4]);
        assert_eq!(r.take(5, ms(5_350)), Some("s"));
        assert_eq!(r.len(), 2);
    }
}
