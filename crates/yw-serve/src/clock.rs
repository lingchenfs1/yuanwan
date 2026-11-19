//! 进程内单调时钟（`Micros`）。
//!
//! ywp 和 yw-media 是 sans-IO，不读时钟；网关里所有要互相比较的时刻（chiaki 回调里的 rx_us、
//! Hub.last_key_us、ywp 线程的 now、监督线程调 IdrGovernor::tick 的 now）都必须取自这一个时钟，
//! 否则 IdrGovernor 判断「请求之后有没有来关键帧」会出错。

use std::sync::OnceLock;
use std::time::Instant;
use ywp::time::Micros;

static T0: OnceLock<Instant> = OnceLock::new();

/// 进程里第一次调用以来的微秒数（单调，不回退）
pub fn now_us() -> Micros {
    T0.get_or_init(Instant::now).elapsed().as_micros() as Micros
}

#[cfg(test)]
mod tests {
    #[test]
    fn monotonic() {
        let a = super::now_us();
        let b = super::now_us();
        assert!(b >= a);
    }
}
