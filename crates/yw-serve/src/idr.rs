//! IdrGovernor（纯逻辑）：每秒 ≤ 1 次，同时 ≤ 1 个未决；1.2 s 重发；5 s 后 Escalate（M2d 接 L7）。
//! 网关里放在 PsLink 的 idr 锁里，tick 由监督线程每 100 ms 调；fakegw 自己持有一份。

use crate::ingress::WaitWhy;
use ywp::time::Micros;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdrDecision {
    /// 已向 PS5 请求
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

#[derive(Debug)]
pub struct IdrGovernor {
    _priv: (),
}

impl IdrGovernor {
    pub fn new() -> Self {
        todo!()
    }

    pub fn request(&mut self, now: Micros, why: IdrReason) -> IdrDecision {
        todo!()
    }

    /// 由网关监督线程每 100 ms 调；last_key_us：最近一次收到关键帧的时刻
    pub fn tick(&mut self, now: Micros, last_key_us: Micros) -> Option<IdrDecision> {
        todo!()
    }
}
