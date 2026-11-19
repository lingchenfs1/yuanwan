//! AUDIO 块（0x30，规格 §5.5.2）：aseq u16 | age u16 | cnt u8 | cnt × {len u16, Opus 数据}，最新的在前。
//! len = 0 表示源头丢失（PS5 段就丢了的帧）。HAPTICS（0x31）以后用同一种格式。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{chunk_len, chunk_ty, AUDIO_MAX_FRAMES};
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
        if self.left == 0 {
            return None;
        }
        let mut r = Reader::new(self.rest);
        // parse 已保证 cnt 帧都在 raw 里；这里越界只结束迭代
        let Ok(len) = r.u16() else {
            self.left = 0;
            return None;
        };
        let Ok(data) = r.bytes(len as usize) else {
            self.left = 0;
            return None;
        };
        let item = (self.seq, data);
        self.rest = r.rest();
        self.seq = self.seq.wrapping_sub(1);
        self.left -= 1;
        Some(item)
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

impl AudioChunkOwned {
    /// 结构体是 `#[non_exhaustive]`，别的 crate 用它构造
    pub fn new(aseq: u16, age_100us: u16, frames: Vec<Vec<u8>>) -> Self {
        Self { ty: chunk_ty::AUDIO, aseq, age_100us, frames }
    }
}

impl<'a> AudioChunk<'a> {
    /// 解析 AUDIO / HAPTICS 块的内容（≥ AUDIO_HDR，cnt 1~4）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::AUDIO_HDR {
            return Err(DropReason::ShortChunk);
        }
        let mut r = Reader::new(content);
        let aseq = r.u16().map_err(|_| DropReason::ShortChunk)?;
        let age_100us = r.u16().map_err(|_| DropReason::ShortChunk)?;
        let cnt = r.u8().map_err(|_| DropReason::ShortChunk)?;
        if cnt == 0 || cnt as usize > AUDIO_MAX_FRAMES {
            return Err(DropReason::AudioBadCount);
        }
        let start = r.pos();
        for _ in 0..cnt {
            let len = r.u16().map_err(|_| DropReason::ShortChunk)?;
            r.skip(len as usize).map_err(|_| DropReason::ShortChunk)?;
        }
        // 比定义长：多出的字节忽略
        Ok(Self { aseq, age_100us, cnt, raw: &content[start..r.pos()] })
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
        if self.frames.is_empty() || self.frames.len() > AUDIO_MAX_FRAMES {
            return Err(WireError::Malformed("AUDIO 帧数不在 1~4"));
        }
        w.put_u16(self.aseq)?;
        w.put_u16(self.age_100us)?;
        w.put_u8(self.frames.len() as u8)?;
        for f in &self.frames {
            let len = u16::try_from(f.len()).map_err(|_| WireError::Malformed("AUDIO 帧太长"))?;
            w.put_u16(len)?;
            w.put_slice(f)?;
        }
        Ok(())
    }
}
