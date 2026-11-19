//! CONTROL 可靠层（B 组，附录 A.6）：
//! - 每个流、每个会话 msg_seq 从 0 开始，窗口 32 条，按序交付；
//! - 收端 2 ms 内回 ACK；重传整条消息；RTO = max(25 ms, SRTT + 4 × RTTVAR)，封顶 1 s，以握手 RTT 作种子；
//! - 关键消息在 +5 ms 再发一份；v1 只发单片（下行 ≤ 1100 B，上行 ≤ 240 B）。

use crate::sched::Prio;
use crate::time::Micros;
use crate::wire::{Ack, ControlFrag, ControlFragOwned};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CtlError {
    /// M2 只支持单片，超长返回它
    TooLarge,
    /// 32 条的窗口满了
    WindowFull,
    /// 流号不对
    BadStream,
    /// 会话已关闭或还没确认
    NotReady,
}

#[derive(Debug)]
pub struct StreamTx {
    _priv: (),
}

impl StreamTx {
    /// frag_max：下行 1100，上行 240
    pub fn new(stream: u8, frag_max: usize) -> Self {
        todo!()
    }

    pub fn send(&mut self, body: Vec<u8>, critical: bool) -> Result<(), CtlError> {
        todo!()
    }

    pub fn on_ack(&mut self, a: &Ack, now: Micros) {
        todo!()
    }

    /// 取下一片要发的（新消息、重传或关键消息的第二份）
    pub fn poll(&mut self, now: Micros, rto: Micros) -> Option<(ControlFragOwned, Prio)> {
        todo!()
    }

    pub fn next_deadline(&self) -> Option<Micros> {
        todo!()
    }
}

#[derive(Debug)]
pub struct StreamRx {
    _priv: (),
}

impl StreamRx {
    pub fn new(stream: u8) -> Self {
        todo!()
    }

    pub fn on_frag(&mut self, f: &ControlFrag, now: Micros) {
        todo!()
    }

    /// 按序取出已收齐的消息体
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        todo!()
    }

    /// force：不等 2 ms，立即给出 ACK
    pub fn take_ack(&mut self, now: Micros, force: bool) -> Option<Ack> {
        todo!()
    }
}
