//! RTT 估计（B 组）：PING/PONG 取样，RTT = 收到 PONG 的时刻 − echo_ts − hold。
//! 以握手 RTT 作种子；CONTROL 的 RTO 由它给出（不从 ACK 取样本）。

use crate::time::Micros;
use crate::wire::Pong;

#[derive(Clone, Debug)]
pub struct RttEstimator {
    _priv: (),
}

impl RttEstimator {
    pub fn new() -> Self {
        todo!()
    }

    pub fn seed(&mut self, rtt: Micros) {
        todo!()
    }

    /// 返回这次的样本
    pub fn on_pong(&mut self, now: Micros, p: &Pong) -> Option<Micros> {
        todo!()
    }

    /// max(25 ms, SRTT + 4 × RTTVAR)，封顶 1 s
    pub fn rto(&self) -> Micros {
        todo!()
    }

    pub fn srtt(&self) -> Micros {
        todo!()
    }

    pub fn min(&self) -> Micros {
        todo!()
    }

    pub fn p50(&self) -> Micros {
        todo!()
    }
}
