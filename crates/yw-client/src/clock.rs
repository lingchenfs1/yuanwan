//! 进程内单调时钟：所有线程共用同一个起点，返回 [`Micros`]（规格里的「本地单调时钟」）。

use std::sync::OnceLock;
use std::time::Instant;
use ywp::time::Micros;

static EPOCH: OnceLock<Instant> = OnceLock::new();

/// 程序启动时调一次，把起点定下来（不调也行，第一次取时间时自动定）
pub fn init() {
    EPOCH.get_or_init(Instant::now);
}

/// 距起点的微秒数
pub fn now_us() -> Micros {
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as Micros
}

/// 睡到某个时刻（粗睡到差 1 ms，再短暂自旋，误差一般在 0.1 ms 以内）
pub fn sleep_until(at: Micros) {
    loop {
        let now = now_us();
        if now >= at {
            return;
        }
        let left = at - now;
        if left > 1_500 {
            std::thread::sleep(std::time::Duration::from_micros(left - 1_000));
        } else {
            std::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_and_sleep() {
        let a = now_us();
        sleep_until(a + 3_000);
        let b = now_us();
        assert!(b >= a + 3_000);
        assert!(b < a + 50_000, "睡过头太多：{} us", b - a);
    }
}
