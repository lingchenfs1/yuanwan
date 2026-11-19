//! InputApplier（M2b）：内部用 ywp::chan::InputReceiver，自己加推迟抬起、过期不回放、上行时钟映射。
//!
//! - 旧包（in_seq 不比最新的新）只用来补 ev_seq 更大的事件，不用它的 STATE；
//! - 每个事件单独调用一次 apply，这样 chiaki 会为每次变化排一个 HISTORY 包；
//! - 同一个键的抬起离按下不到 min(原始间隔, 17 ms) 时，推迟到那个时刻再应用；
//! - 事件发生在 150 ms 之前的（换算到网关时钟），只并入状态，不单独调用；
//! - 模拟量只取最新包的值。

use ywp::chan::NeutralWhy;
use ywp::endpoint::RxMeta;
use ywp::time::Micros;
use ywp::wire::{InputChunk, InputState};

/// 输入的落点。正式实现走 PsControl::set_controller_full / set_controller_idle
pub trait PadSink {
    fn apply(&mut self, s: &InputState);
    fn neutral(&mut self);
}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct InputStats {
    pub chunks: u64,
    pub events_applied: u64,
    pub events_stale: u64,
    pub gaps: u64,
    pub neutralized: u64,
    pub deferred_releases: u64,
}

#[derive(Debug)]
pub struct InputApplier {
    _priv: (),
}

impl InputApplier {
    pub fn new() -> Self {
        todo!()
    }

    pub fn on_input(&mut self, now: Micros, meta: &RxMeta, c: &InputChunk, sink: &mut dyn PadSink) {
        todo!()
    }

    pub fn on_tick(&mut self, now: Micros, sink: &mut dyn PadSink) {
        todo!()
    }

    pub fn on_new_epoch(&mut self) {
        todo!()
    }

    pub fn neutralize(&mut self, why: NeutralWhy, sink: &mut dyn PadSink) {
        todo!()
    }

    pub fn stats(&self) -> InputStats {
        todo!()
    }
}
