//! 测试工具（B 组，feature = "testkit"）：虚拟时钟和内存管道。
//! 仿真器（yw-sim）和 ywp 的集成测试用它把两端在内存里直接接起来。

use crate::time::Micros;

/// 手动推进的时钟
#[derive(Clone, Copy, Debug, Default)]
pub struct VirtualClock {
    now: Micros,
}

impl VirtualClock {
    pub fn new(start: Micros) -> Self {
        Self { now: start }
    }

    pub fn now(&self) -> Micros {
        self.now
    }

    pub fn advance(&mut self, d: Micros) -> Micros {
        self.now += d;
        self.now
    }

    /// 只能往前拨
    pub fn set(&mut self, t: Micros) {
        self.now = self.now.max(t);
    }
}

/// 两个端点在内存里对接，可以加简单的丢包和时延
#[derive(Debug)]
pub struct Pipe {
    _priv: (),
}

impl Pipe {
    /// delay：单向时延；loss：丢包率；seed：丢包用的随机种子
    pub fn new(delay: Micros, loss: f64, seed: u64) -> Self {
        todo!()
    }

    /// 送进一个包，at 是发出时刻
    pub fn send(&mut self, at: Micros, pkt: &[u8]) {
        todo!()
    }

    /// 取出到 now 为止该到达的包
    pub fn recv(&mut self, now: Micros) -> Option<Vec<u8>> {
        todo!()
    }

    pub fn next_arrival(&self) -> Option<Micros> {
        todo!()
    }
}
