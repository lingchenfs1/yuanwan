//! TAI64N（规格 §4.5）：secs = 2⁶² + Unix 秒（u64），后接 nanos（u32），不加闰秒，共 12 B 大端。
//! 网关为每个客户端在内存里记住收到过的最大值，不大于它的 HS_INIT 一律忽略。

use crate::consts::TAI64N_BASE;
use std::time::{SystemTime, UNIX_EPOCH};

const NANOS_PER_SEC: u32 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tai64N {
    pub secs: u64,
    pub nanos: u32,
}

impl Tai64N {
    /// 由 Unix 时间构造（nanos ≥ 10⁹ 的部分进位到秒）
    pub fn from_unix(secs: u64, nanos: u32) -> Self {
        Self { secs: TAI64N_BASE + secs + (nanos / NANOS_PER_SEC) as u64, nanos: nanos % NANOS_PER_SEC }
    }

    /// 读墙钟
    pub fn now() -> Self {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        Self::from_unix(d.as_secs(), d.subsec_nanos())
    }

    /// 取 max(墙钟, last + 1 ns)。读墙钟是唯一的例外（握手时间戳本来就要墙钟）
    pub fn next_after(last: Option<Self>) -> Self {
        let wall = Self::now();
        match last {
            Some(l) => wall.max(l.plus_nanos(1)),
            None => wall,
        }
    }

    /// 往后推 n 纳秒
    pub fn plus_nanos(self, n: u64) -> Self {
        let total = self.nanos as u64 + n;
        Self {
            secs: self.secs.saturating_add(total / NANOS_PER_SEC as u64),
            nanos: (total % NANOS_PER_SEC as u64) as u32,
        }
    }

    /// 往后推 us 微秒。客户端握手重传用：时间戳 = ts0 + (now − now0)，不再读墙钟（sans-IO）
    pub fn plus_micros(self, us: u64) -> Self {
        self.plus_nanos(us.saturating_mul(1_000))
    }

    pub fn to_bytes(&self) -> [u8; 12] {
        let mut b = [0u8; 12];
        b[..8].copy_from_slice(&self.secs.to_be_bytes());
        b[8..].copy_from_slice(&self.nanos.to_be_bytes());
        b
    }

    pub fn from_bytes(b: &[u8; 12]) -> Self {
        let mut s = [0u8; 8];
        s.copy_from_slice(&b[..8]);
        let mut n = [0u8; 4];
        n.copy_from_slice(&b[8..]);
        Self { secs: u64::from_be_bytes(s), nanos: u32::from_be_bytes(n) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_and_carry() {
        let a = Tai64N::from_unix(100, 999_999_999);
        let b = a.plus_nanos(1);
        assert_eq!(b, Tai64N::from_unix(101, 0));
        assert!(b > a);
        assert_eq!(a.plus_micros(2), Tai64N::from_unix(101, 1_999));
        assert_eq!(Tai64N::from_bytes(&a.to_bytes()), a);
        assert_eq!(a.to_bytes()[..8], (TAI64N_BASE + 100).to_be_bytes());
        // 墙钟往回拨也保证递增
        let future = Tai64N::from_unix(u32::MAX as u64 * 4, 0);
        assert_eq!(Tai64N::next_after(Some(future)), future.plus_nanos(1));
        let now = Tai64N::next_after(None);
        assert!(Tai64N::next_after(Some(now)) > now);
    }
}
