//! 测试工具（B 组，feature = "testkit"）：虚拟时钟、确定性随机数和内存管道。
//! 仿真器（yw-sim）和各组的集成测试用它把两端在内存里直接接起来。
//!
//! - [`VirtualClock`]：手动推进的时钟；
//! - [`Rng`]：SplitMix64，同一个种子每次给出同一串数，测试结果可复现；
//! - [`Pipe`]：单向链路模型，可以注入丢包、重复、乱序、固定时延和抖动，另有中断时段、
//!   「接下来丢 N 个」和按内容丢包的过滤器；载荷是泛型，可以直接传结构体，不必先编码成字节；
//! - [`Duplex`]：两条 Pipe 组成的双向链路。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::time::Micros;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt;

/// 手动推进的时钟
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtualClock {
    now: Micros,
}

impl VirtualClock {
    pub fn new(start: Micros) -> Self {
        Self { now: start }
    }

    pub fn now(&self) -> Micros {
        self.now
    }

    pub fn advance(&mut self, d: Micros) -> Micros {
        self.now += d;
        self.now
    }

    /// 只能往前拨
    pub fn set(&mut self, t: Micros) {
        self.now = self.now.max(t);
    }
}

/// 确定性伪随机数（SplitMix64）。不是密码学随机数，只给测试和仿真用
#[derive(Clone, Debug)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// [0, 1) 的均匀分布
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// 以概率 p 返回 true（p ≤ 0 恒假，p ≥ 1 恒真）
    pub fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && (p >= 1.0 || self.f64() < p)
    }

    /// [0, n) 的整数；n = 0 时返回 0
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// [lo, hi] 的整数
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            lo
        } else {
            lo + self.below(hi - lo + 1)
        }
    }
}

/// 单向链路的参数
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkCfg {
    /// 固定单向时延
    pub delay: Micros,
    /// 每个包另加 [0, jitter] 的均匀随机时延（会自然造成乱序）
    pub jitter: Micros,
    /// 丢包率（独立同分布）
    pub loss: f64,
    /// 重复率：命中的包多送一份，那一份另抽一次时延
    pub dup: f64,
    /// 乱序率：命中的包额外推迟 [1, reorder_extra] µs，排到后发的包后面
    pub reorder: f64,
    pub reorder_extra: Micros,
}

impl LinkCfg {
    /// 只有固定时延的理想链路
    pub const fn ideal(delay: Micros) -> Self {
        Self { delay, jitter: 0, loss: 0.0, dup: 0.0, reorder: 0.0, reorder_extra: 0 }
    }
}

impl Default for LinkCfg {
    fn default() -> Self {
        Self::ideal(0)
    }
}

/// 链路计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PipeStats {
    /// send 的次数
    pub sent: u64,
    /// 随机丢弃的
    pub lost: u64,
    /// 中断时段、drop_next、过滤器丢掉的
    pub blocked: u64,
    /// 多送的副本数
    pub duplicated: u64,
    /// 被额外推迟（乱序）的
    pub reordered: u64,
    /// recv 交出去的
    pub delivered: u64,
}

type DropFilter<T> = Box<dyn FnMut(&T, Micros) -> bool + Send>;

/// 单向内存链路。按到达时刻交付，同一时刻按送入顺序
pub struct Pipe<T = Vec<u8>> {
    cfg: LinkCfg,
    rng: Rng,
    /// (到达时刻, 送入序号) → slot 下标
    heap: BinaryHeap<Reverse<(Micros, u64, usize)>>,
    slots: Vec<Option<T>>,
    free: Vec<usize>,
    seq: u64,
    outages: Vec<(Micros, Micros)>,
    drop_next: u32,
    filter: Option<DropFilter<T>>,
    stats: PipeStats,
}

impl<T> fmt::Debug for Pipe<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pipe")
            .field("cfg", &self.cfg)
            .field("in_flight", &self.heap.len())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl<T: Clone> Pipe<T> {
    /// delay：单向时延；loss：丢包率；seed：随机种子
    pub fn new(delay: Micros, loss: f64, seed: u64) -> Self {
        Self::with_cfg(LinkCfg { loss, ..LinkCfg::ideal(delay) }, seed)
    }

    pub fn with_cfg(cfg: LinkCfg, seed: u64) -> Self {
        Self {
            cfg,
            rng: Rng::new(seed),
            heap: BinaryHeap::new(),
            slots: Vec::new(),
            free: Vec::new(),
            seq: 0,
            outages: Vec::new(),
            drop_next: 0,
            filter: None,
            stats: PipeStats::default(),
        }
    }

    pub fn cfg(&self) -> &LinkCfg {
        &self.cfg
    }

    /// 中途改参数（比如注入一段 RTT 尖峰），已在路上的包不受影响
    pub fn set_cfg(&mut self, cfg: LinkCfg) {
        self.cfg = cfg;
    }

    /// [from, to) 时段内送入的包全部丢弃
    pub fn add_outage(&mut self, from: Micros, to: Micros) {
        self.outages.push((from, to));
    }

    /// 接下来送入的 n 个包全部丢弃
    pub fn drop_next(&mut self, n: u32) {
        self.drop_next = n;
    }

    /// 过滤器返回 true 的包丢弃（比如「丢掉所有 HS_INIT」）。None 取消
    pub fn set_filter(&mut self, f: Option<DropFilter<T>>) {
        self.filter = f;
    }

    /// 送进一个包，at 是发出时刻
    pub fn send(&mut self, at: Micros, pkt: T) {
        self.stats.sent += 1;
        if self.drop_next > 0 {
            self.drop_next -= 1;
            self.stats.blocked += 1;
            return;
        }
        if self.outages.iter().any(|&(a, b)| at >= a && at < b) {
            self.stats.blocked += 1;
            return;
        }
        if let Some(f) = self.filter.as_mut() {
            if f(&pkt, at) {
                self.stats.blocked += 1;
                return;
            }
        }
        if self.rng.chance(self.cfg.loss) {
            self.stats.lost += 1;
            return;
        }
        if self.rng.chance(self.cfg.dup) {
            self.stats.duplicated += 1;
            let t = self.arrival(at);
            self.enqueue(t, pkt.clone());
        }
        let t = self.arrival(at);
        self.enqueue(t, pkt);
    }

    fn arrival(&mut self, at: Micros) -> Micros {
        let mut t = at + self.cfg.delay + self.rng.range(0, self.cfg.jitter);
        if self.rng.chance(self.cfg.reorder) {
            self.stats.reordered += 1;
            t += self.rng.range(1, self.cfg.reorder_extra.max(1));
        }
        t
    }

    fn enqueue(&mut self, t: Micros, pkt: T) {
        let idx = match self.free.pop() {
            Some(i) => {
                self.slots[i] = Some(pkt);
                i
            }
            None => {
                self.slots.push(Some(pkt));
                self.slots.len() - 1
            }
        };
        self.heap.push(Reverse((t, self.seq, idx)));
        self.seq += 1;
    }

    /// 取出到 now 为止该到达的包
    pub fn recv(&mut self, now: Micros) -> Option<T> {
        self.recv_timed(now).map(|(_, p)| p)
    }

    /// 同 recv，另给出到达时刻
    pub fn recv_timed(&mut self, now: Micros) -> Option<(Micros, T)> {
        let &Reverse((t, _, idx)) = self.heap.peek()?;
        if t > now {
            return None;
        }
        self.heap.pop();
        let pkt = self.slots[idx].take()?;
        self.free.push(idx);
        self.stats.delivered += 1;
        Some((t, pkt))
    }

    pub fn next_arrival(&self) -> Option<Micros> {
        self.heap.peek().map(|Reverse((t, _, _))| *t)
    }

    /// 还在路上的包数
    pub fn in_flight(&self) -> usize {
        self.heap.len()
    }

    pub fn stats(&self) -> PipeStats {
        self.stats
    }
}

impl Pipe<Vec<u8>> {
    /// 送进一段字节（拷贝一份）
    pub fn send_bytes(&mut self, at: Micros, pkt: &[u8]) {
        self.send(at, pkt.to_vec());
    }
}

/// 双向链路：a_to_b 是 A 发给 B 的方向
#[derive(Debug)]
pub struct Duplex<T = Vec<u8>> {
    pub a_to_b: Pipe<T>,
    pub b_to_a: Pipe<T>,
}

impl<T: Clone> Duplex<T> {
    /// 两个方向用同样的参数，种子分开
    pub fn symmetric(cfg: LinkCfg, seed: u64) -> Self {
        Self { a_to_b: Pipe::with_cfg(cfg, seed), b_to_a: Pipe::with_cfg(cfg, seed ^ 0x5DEE_CE66_D1CE_5EED) }
    }

    pub fn new(a_to_b: LinkCfg, b_to_a: LinkCfg, seed: u64) -> Self {
        Self { a_to_b: Pipe::with_cfg(a_to_b, seed), b_to_a: Pipe::with_cfg(b_to_a, seed ^ 0x5DEE_CE66_D1CE_5EED) }
    }

    /// 两个方向里最早的到达时刻
    pub fn next_arrival(&self) -> Option<Micros> {
        min_opt(self.a_to_b.next_arrival(), self.b_to_a.next_arrival())
    }
}

/// 两个可选时刻取早的那个
pub fn min_opt(a: Option<Micros>, b: Option<Micros>) -> Option<Micros> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_in_range() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
            let x = a.f64();
            b.f64();
            assert!((0.0..1.0).contains(&x));
            let r = a.range(3, 7);
            b.range(3, 7);
            assert!((3..=7).contains(&r));
        }
        assert!(!a.chance(0.0));
        assert!(a.chance(1.0));
    }

    #[test]
    fn ideal_pipe_keeps_order_and_delay() {
        let mut p: Pipe<u32> = Pipe::new(1_000, 0.0, 1);
        for i in 0..10 {
            p.send(i * 10, i as u32);
        }
        assert_eq!(p.next_arrival(), Some(1_000));
        assert_eq!(p.recv(999), None);
        let got: Vec<u32> = std::iter::from_fn(|| p.recv(10_000)).collect();
        assert_eq!(got, (0..10).collect::<Vec<_>>());
        assert_eq!(p.stats().delivered, 10);
    }

    #[test]
    fn loss_dup_reorder_are_injected() {
        let cfg = LinkCfg { delay: 500, jitter: 200, loss: 0.2, dup: 0.1, reorder: 0.1, reorder_extra: 5_000 };
        let mut p: Pipe<u32> = Pipe::with_cfg(cfg, 7);
        for i in 0..10_000u32 {
            p.send(i as Micros * 100, i);
        }
        let mut got = Vec::new();
        while let Some(x) = p.recv(Micros::MAX) {
            got.push(x);
        }
        let s = p.stats();
        assert_eq!(s.sent, 10_000);
        assert!(s.lost > 1_500 && s.lost < 2_500, "{s:?}");
        assert!(s.duplicated > 500, "{s:?}");
        assert!(s.reordered > 500, "{s:?}");
        assert_eq!(got.len() as u64, s.sent - s.lost + s.duplicated);
        assert!(got.windows(2).any(|w| w[1] < w[0]), "应当出现乱序");
    }

    #[test]
    fn outage_drop_next_and_filter() {
        let mut p: Pipe<u32> = Pipe::new(0, 0.0, 1);
        p.add_outage(100, 200);
        p.drop_next(2);
        p.set_filter(Some(Box::new(|x: &u32, _| *x == 7)));
        for i in 0..10u32 {
            p.send(i as Micros * 30, i);
        }
        let got: Vec<u32> = std::iter::from_fn(|| p.recv(Micros::MAX)).collect();
        // 0、1 被 drop_next 丢；120、150、180 时刻（4、5、6）落在中断里；7 被过滤器丢
        assert_eq!(got, vec![2, 3, 8, 9]);
        assert_eq!(p.stats().blocked, 6);
    }
}
