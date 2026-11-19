//! 判丢和 NACK（B 组，M2c）：客户端按 PN 缺口判丢（宽限 g = 2 ms），
//! 判丢后 1 ms 内发 NACK，≥ 2 ms 后再发一次。

use crate::time::Micros;
use crate::wire::NackOwned;

/// 一段连续的 PN
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PnRange {
    pub start: u64,
    pub count: u8,
}

/// 累计计数（填进 REPORT 的每路径块）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LossCounters {
    pub largest_pn: u64,
    pub recv_cnt: u32,
    pub lost_cnt: u32,
    pub lost_short: u16,
    pub burst_max: u8,
}

#[derive(Debug)]
pub struct RxLoss {
    _priv: (),
}

impl RxLoss {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_packet(&mut self, pn: u64, now: Micros) {
        todo!()
    }

    /// 过了宽限的缺口追加进 out
    pub fn poll(&mut self, now: Micros, grace: Micros, out: &mut Vec<PnRange>) {
        todo!()
    }

    pub fn next_deadline(&self) -> Option<Micros> {
        todo!()
    }

    pub fn counters(&self) -> LossCounters {
        todo!()
    }
}

#[derive(Debug)]
pub struct NackQueue {
    _priv: (),
}

impl NackQueue {
    pub fn new() -> Self {
        todo!()
    }

    pub fn push(&mut self, r: &[PnRange], now: Micros) {
        todo!()
    }

    /// room：包里还剩多少字节
    pub fn take(&mut self, now: Micros, room: usize) -> Option<NackOwned> {
        todo!()
    }

    /// 等不到能搭车的输入包时，单独发一个
    pub fn must_send_alone(&self, now: Micros) -> bool {
        todo!()
    }
}
