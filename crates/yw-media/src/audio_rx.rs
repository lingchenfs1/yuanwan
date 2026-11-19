//! 音频抖动缓冲（M2b，由 SDL 音频回调线程独占）。
//! 按 aseq 存进 64 个槽；目标水位竞技 20 / 均衡 30 / 稳定 50 ms；缺帧或源头丢失做 PLC，
//! 连续 PLC 超过 5 帧渐弱到静音；水位超过目标 + 60 ms 丢帧；PI 控制变速比 ±0.5%。

use bytes::Bytes;
use smallvec::SmallVec;
use ywp::time::Micros;

/// net 线程交给音频回调的一个包
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub aseq: u16,
    pub age_100us: u16,
    pub send_ts: u32,
    pub arrival: Micros,
    /// (帧序号, 数据)，最新的在前；None = len 0，源头丢失
    pub frames: SmallVec<[(u16, Option<Bytes>); 3]>,
}

pub enum Pull<'a> {
    Frame(&'a [u8]),
    /// 做 PLC
    Conceal,
    Silence,
}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct AudioStats {
    pub played: u64,
    pub concealed: u64,
    /// len = 0 的帧（源头丢失）
    pub source_lost: u64,
    pub duplicates: u64,
    pub late: u64,
    pub overfill_drops: u64,
    pub underruns: u64,
}

#[derive(Debug)]
pub struct AudioJitter {
    _priv: (),
}

impl AudioJitter {
    pub fn new(target_us: u32) -> Self {
        todo!()
    }

    /// 去掉冗余重复；len = 0 记作源头丢失
    pub fn on_packet(&mut self, now: Micros, p: &AudioPacket) {
        todo!()
    }

    /// 每次取一个 10 ms 帧
    pub fn pull(&mut self) -> Pull<'_> {
        todo!()
    }

    pub fn level_us(&self) -> i32 {
        todo!()
    }

    pub fn rate_ratio(&mut self) -> f32 {
        todo!()
    }

    pub fn stats(&self) -> AudioStats {
        todo!()
    }
}
