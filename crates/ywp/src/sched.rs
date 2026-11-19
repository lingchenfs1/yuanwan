//! 发送调度（B 组）：严格优先级。
//! Scheduler 是端点内部的：小块带 not_before / deadline / repeat；载体按 (prio, order, 入队序号) 排。

/// 发送优先级，P1 最高
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prio {
    P1 = 1,
    P2,
    P3,
    P4,
    P5,
    P6,
}

/// 一个待发的视频符号。正文在组包时经 SymbolSource 取，不拷贝
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCarrier {
    pub hdr: crate::wire::VideoHeader,
    pub prio: Prio,
    pub order: u64,
}

/// 视频符号的正文来源（网关里是 yw-media 的 VideoTx）
pub trait SymbolSource {
    fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]>;
}
