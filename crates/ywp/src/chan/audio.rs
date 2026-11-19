//! 音频发送（B 组，M2b）：每包带最新 2 帧；opus 为 None（chiaki 已判丢）时发 len = 0。

use crate::time::Micros;
use crate::wire::AudioChunkOwned;

#[derive(Debug)]
pub struct AudioTx {
    _priv: (),
}

impl AudioTx {
    pub fn new() -> Self {
        todo!()
    }

    /// arrival：这一帧到达网关的时刻（算 age 用）
    pub fn push(&mut self, opus: Option<&[u8]>, arrival: Micros, now: Micros) -> AudioChunkOwned {
        todo!()
    }

    /// v1 固定 2 帧（M2e 再调）
    pub fn set_redundancy(&mut self, n: u8) {
        todo!()
    }
}
