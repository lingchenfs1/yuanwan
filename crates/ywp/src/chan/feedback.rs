//! 反馈通道（B 组，M2b）：变化时在 0、+8、+24 ms 发，之后每 250 ms 保活一次；
//! 客户端只应用 fb_seq 更大的那一份；会话结束时清零。

use crate::time::Micros;
use crate::wire::FeedbackState;

#[derive(Debug)]
pub struct FeedbackTx {
    _priv: (),
}

impl FeedbackTx {
    pub fn new() -> Self {
        todo!()
    }

    pub fn update(&mut self, s: FeedbackState, now: Micros) {
        todo!()
    }

    pub fn poll(&mut self, now: Micros) -> Option<FeedbackState> {
        todo!()
    }
}

#[derive(Debug)]
pub struct FeedbackRx {
    _priv: (),
}

impl FeedbackRx {
    pub fn new() -> Self {
        todo!()
    }

    /// fb_seq 更大才接受
    pub fn accept(&mut self, s: &FeedbackState) -> bool {
        todo!()
    }

    pub fn reset(&mut self) {
        todo!()
    }
}
