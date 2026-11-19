//! 发送记录（B 组，M2c）：每个 PN 装的是什么，NACK 来了由它查出丢的是哪个 (fid, sym)。

use crate::time::Micros;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentTag {
    None,
    Video { fid: u16, sym: u16 },
    Audio,
}

#[derive(Debug)]
pub struct SentLog {
    _priv: (),
}

impl SentLog {
    pub fn new() -> Self {
        todo!()
    }

    pub fn push(&mut self, pn: u64, t: Micros, len: u16, tag: SentTag) {
        todo!()
    }

    /// 第二次报丢返回 None
    pub fn take_lost(&mut self, pn: u64) -> Option<SentTag> {
        todo!()
    }

    pub fn expire(&mut self, before: Micros) {
        todo!()
    }
}
