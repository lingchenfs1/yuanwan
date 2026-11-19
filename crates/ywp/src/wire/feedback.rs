//! FEEDBACK 块（0x39，G→C，32 B，规格 §5.5.4）。
//! 语义是状态同步：新的覆盖旧的，客户端只应用 fb_seq 更大的那一份。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedbackState {
    /// 任何字段变化就加 1
    pub fb_seq: u16,
    /// 有效位掩码，见 consts::fb_valid
    pub valid: u8,
    /// 震动 L、R
    pub rumble: [u8; 2],
    /// 左扳机效果：type 1 B + 参数 10 B（原样转发 PS5 下发的内容）
    pub trig_l: [u8; 11],
    pub trig_r: [u8; 11],
    /// 灯条 RGB
    pub rgb: [u8; 3],
    /// 玩家指示灯
    pub player: u8,
    /// 高 4 位触觉强度，低 4 位扳机强度
    pub intensity: u8,
}

impl FeedbackState {
    /// 解析 FEEDBACK 块的内容（32 B；更长只读前 32 B）
    pub fn parse(content: &[u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::FEEDBACK {
            return Err(DropReason::ShortChunk);
        }
        let mut r = Reader::new(content);
        let e = |_| DropReason::ShortChunk;
        Ok(Self {
            fb_seq: r.u16().map_err(e)?,
            valid: r.u8().map_err(e)?,
            rumble: r.array().map_err(e)?,
            trig_l: r.array().map_err(e)?,
            trig_r: r.array().map_err(e)?,
            rgb: r.array().map_err(e)?,
            player: r.u8().map_err(e)?,
            intensity: r.u8().map_err(e)?,
        })
    }
}

impl EncodeChunk for FeedbackState {
    fn ty(&self) -> u8 {
        chunk_ty::FEEDBACK
    }
    fn content_len(&self) -> usize {
        chunk_len::FEEDBACK
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        w.put_u16(self.fb_seq)?;
        w.put_u8(self.valid)?;
        w.put_slice(&self.rumble)?;
        w.put_slice(&self.trig_l)?;
        w.put_slice(&self.trig_r)?;
        w.put_slice(&self.rgb)?;
        w.put_u8(self.player)?;
        w.put_u8(self.intensity)
    }
}
