//! FeedbackMerge（M2b）：把 chiaki 的 on_feedback 事件合并成 FEEDBACK 状态；PS5 会话结束时清零。

use crate::source::FbEvent;
use ywp::wire::FeedbackState;

#[derive(Debug)]
pub struct FeedbackMerge {
    _priv: (),
}

impl FeedbackMerge {
    pub fn new() -> Self {
        todo!()
    }

    /// 状态有变化时返回新状态（fb_seq 已加 1）
    pub fn on_event(&mut self, ev: &FbEvent) -> Option<FeedbackState> {
        todo!()
    }

    /// 清零，返回清零后的状态（要发给客户端）
    pub fn reset(&mut self) -> FeedbackState {
        todo!()
    }
}
