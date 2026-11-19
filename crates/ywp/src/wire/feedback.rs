//! FEEDBACK 块（0x39，G→C，32 B，规格 §5.5.4、附录 A.5）。
//! 语义是状态同步：新的覆盖旧的，客户端只应用 fb_seq 更大的那一份。

use super::{EncodeChunk, Writer};
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
    /// 左扳机效果：type 1 B + 参数 10 B（原样放 chiaki 事件里的内容）
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
    /// 解析 FEEDBACK 块的内容（32 B）
    pub fn parse(content: &[u8]) -> Result<Self, DropReason> {
        todo!()
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
        todo!()
    }
}
