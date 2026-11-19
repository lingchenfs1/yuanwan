//! AUDIO 块（0x30，规格 §5.5.2）：aseq u16 | age u16 | cnt u8 | cnt × {len u16, Opus 数据}，最新的在前。
//! len = 0 表示源头丢失（chiaki 已判丢的帧）。

use super::{EncodeChunk, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

/// AUDIO 块的只读视图
#[derive(Clone, Copy, Debug)]
pub struct AudioChunk<'a> {
    /// 本块里最新一帧的序号；其余依次是 aseq − 1、aseq − 2……
    pub aseq: u16,
    /// 最新一帧到达网关到发送的时间，单位 100 µs
    pub age_100us: u16,
    /// 帧数 1~4
    pub cnt: u8,
    raw: &'a [u8],
}

impl<'a> AudioChunk<'a> {
    /// (帧序号, Opus 数据)，最新的在前；数据为空表示源头丢失
    pub fn frames(&self) -> AudioFrames<'a> {
        AudioFrames { seq: self.aseq, left: self.cnt, rest: self.raw }
    }
}

#[derive(Clone, Debug)]
pub struct AudioFrames<'a> {
    seq: u16,
    left: u8,
    rest: &'a [u8],
}

impl<'a> Iterator for AudioFrames<'a> {
    type Item = (u16, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// 发送端组好的 AUDIO 块（AudioTx::push 的输出）
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AudioChunkOwned {
    /// 块类型：chunk_ty::AUDIO（默认）或 chunk_ty::HAPTICS
    pub ty: u8,
    pub aseq: u16,
    pub age_100us: u16,
    /// 最新的在前；空 Vec 表示源头丢失（len = 0）
    pub frames: Vec<Vec<u8>>,
}

impl Default for AudioChunkOwned {
    fn default() -> Self {
        Self { ty: chunk_ty::AUDIO, aseq: 0, age_100us: 0, frames: Vec::new() }
    }
}

impl<'a> AudioChunk<'a> {
    /// 解析 AUDIO / HAPTICS 块的内容（≥ AUDIO_HDR，cnt 1~4）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl EncodeChunk for AudioChunkOwned {
    fn ty(&self) -> u8 {
        self.ty
    }
    fn content_len(&self) -> usize {
        chunk_len::AUDIO_HDR + self.frames.iter().map(|f| chunk_len::AUDIO_FRAME_HDR + f.len()).sum::<usize>()
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}
