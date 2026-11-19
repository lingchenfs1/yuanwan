//! 重放窗口（A 组，规格 §4.5）：每条路径、每个方向 4096 位滑动窗口。
//! 先 check，AEAD 校验通过之后才 commit；窗口外的包和已见过的 PN 一律丢弃。
//!
//! 窗口覆盖 (largest − 4096, largest]，用环形位图存：PN 对应位号 PN mod 4096。

#![warn(unused_variables, dead_code, clippy::new_without_default)]

use crate::consts::REPLAY_WINDOW;
use crate::error::DropReason;

const WORDS: usize = (REPLAY_WINDOW / 64) as usize;

#[derive(Clone, Debug)]
pub struct ReplayWindow {
    largest: Option<u64>,
    bits: [u64; WORDS],
}

impl Default for ReplayWindow {
    fn default() -> Self {
        Self { largest: None, bits: [0; WORDS] }
    }
}

impl ReplayWindow {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已提交的最大 PN
    pub fn largest(&self) -> Option<u64> {
        self.largest
    }

    fn slot(pn: u64) -> (usize, u64) {
        let i = (pn % REPLAY_WINDOW) as usize;
        (i / 64, 1u64 << (i % 64))
    }

    fn get(&self, pn: u64) -> bool {
        let (w, m) = Self::slot(pn);
        self.bits[w] & m != 0
    }

    fn set(&mut self, pn: u64, on: bool) {
        let (w, m) = Self::slot(pn);
        if on {
            self.bits[w] |= m;
        } else {
            self.bits[w] &= !m;
        }
    }

    /// pn 是否可以接受（没见过、且不在窗口左边之外）
    pub fn check(&self, pn: u64) -> bool {
        self.check_reason(pn).is_ok()
    }

    /// 同 check，不接受时给出原因：TooOld（窗口左边之外）或 Replay（见过）
    pub fn check_reason(&self, pn: u64) -> Result<(), DropReason> {
        match self.largest {
            None => Ok(()),
            Some(l) if pn > l => Ok(()),
            Some(l) if l - pn >= REPLAY_WINDOW => Err(DropReason::TooOld),
            Some(_) if self.get(pn) => Err(DropReason::Replay),
            Some(_) => Ok(()),
        }
    }

    /// AEAD 通过之后才调用。窗口左边之外的 PN 不记（check 本来就不会放它过来）
    pub fn commit(&mut self, pn: u64) {
        match self.largest {
            None => {
                self.bits = [0; WORDS];
                self.largest = Some(pn);
            }
            Some(l) if pn > l => {
                let d = pn - l;
                if d >= REPLAY_WINDOW {
                    self.bits = [0; WORDS];
                } else {
                    // (l, pn) 这些位原来记的是滑出窗口的旧 PN，清掉
                    for x in l + 1..pn {
                        self.set(x, false);
                    }
                }
                self.largest = Some(pn);
            }
            Some(l) if l - pn >= REPLAY_WINDOW => return,
            Some(_) => {}
        }
        self.set(pn, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges() {
        let mut w = ReplayWindow::new();
        assert!(w.check(0));
        w.commit(0);
        assert_eq!(w.check_reason(0), Err(DropReason::Replay));
        w.commit(5000);
        assert_eq!(w.check_reason(0), Err(DropReason::TooOld));
        assert_eq!(w.check_reason(5000 - 4095), Ok(()));
        assert_eq!(w.check_reason(5000 - 4096), Err(DropReason::TooOld));
        w.commit(5000 - 4095);
        assert_eq!(w.check_reason(5000 - 4095), Err(DropReason::Replay));
        // 向右滑 1 位，最左边那个出窗口
        w.commit(5001);
        assert_eq!(w.check_reason(5000 - 4095), Err(DropReason::TooOld));
        assert!(w.check(4999));
    }
}
