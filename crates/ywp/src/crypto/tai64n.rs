//! TAI64N（附录 A.2）：secs = 2⁶² + Unix 秒（u64），后接 nanos（u32），不加闰秒，共 12 B 大端。
//! 网关为每个客户端在内存里记住收到过的最大值，不大于它的 HS_INIT 一律忽略。

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tai64N {
    pub secs: u64,
    pub nanos: u32,
}

impl Tai64N {
    /// 取 max(墙钟, last + 1 ns)。读墙钟是唯一的例外（握手时间戳本来就要墙钟）
    pub fn next_after(last: Option<Self>) -> Self {
        todo!()
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
