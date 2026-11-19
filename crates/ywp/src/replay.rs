//! 重放窗口（A 组）：每条路径、每个方向 4096 位滑动窗口。
//! 先 check，AEAD 校验通过之后才 commit；窗口外的包和已见过的 PN 一律丢弃。

#[derive(Clone, Debug)]
pub struct ReplayWindow {
    _priv: (),
}

impl ReplayWindow {
    pub fn new() -> Self {
        todo!()
    }

    /// pn 是否可以接受（没见过、且不在窗口左边之外）
    pub fn check(&self, pn: u64) -> bool {
        todo!()
    }

    /// AEAD 通过之后才调用
    pub fn commit(&mut self, pn: u64) {
        todo!()
    }
}
