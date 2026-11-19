//! 输入通道（B 组，M2b）。
//!
//! 发送端：离散事件在 0、+1、+4、+8、+16 ms 各发一份；模拟量两次发送间隔 ≥ 4 ms；16 ms 保活；
//! in_seq 每包加 1；保留 8 个事件的历史。
//! 接收端：每个 ev_seq 只应用一次；旧包只用来补事件；有缺口时输出 Gap + State；首包不回放事件；
//! 150 ms 收不到就 Neutralize。

use crate::time::Micros;
use crate::wire::{InputChunk, InputChunkOwned, InputEvent, InputState};

/// 为什么中性化
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NeutralWhy {
    /// 150 ms 没有 INPUT
    Timeout,
    /// 客户端发了 Z（主动归零）
    Zero,
    /// A = 0（手柄断开）
    Disconnected,
    /// PS5 会话换了 epoch
    NewEpoch,
    /// 被另一台客户端接管
    Takeover,
    /// 会话关闭
    Closed,
}

#[derive(Debug)]
pub struct InputSender {
    _priv: (),
}

impl InputSender {
    pub fn new() -> Self {
        todo!()
    }

    /// discrete：本次采样里的离散事件 (code, value)
    pub fn on_change(&mut self, now: Micros, s: InputState, discrete: &[(u8, u8)]) {
        todo!()
    }

    pub fn poll(&mut self, now: Micros) -> Option<InputChunkOwned> {
        todo!()
    }

    pub fn next_deadline(&self) -> Option<Micros> {
        todo!()
    }
}

/// 接收端交给上层（网关的 InputApplier）的动作
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputAction {
    Event(InputEvent),
    State(InputState),
    /// ev_seq 跳号超过历史长度，丢了 missed 个事件
    Gap { missed: u16 },
    Neutralize(NeutralWhy),
}

#[derive(Debug)]
pub struct InputReceiver {
    _priv: (),
}

impl InputReceiver {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_input(&mut self, c: &InputChunk, now: Micros, out: &mut Vec<InputAction>) {
        todo!()
    }

    pub fn on_tick(&mut self, now: Micros, out: &mut Vec<InputAction>) {
        todo!()
    }

    pub fn reset(&mut self) {
        todo!()
    }
}
