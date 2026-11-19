//! 时间量和计时器（归协调人）：`Micros`、SendTS、16/32 位序号比较和展开，
//! 以及 M2 计划附录 B 的全部计时器（另收了正文里几处固定的时间常数）。
//!
//! 时间一律用 `Micros`：进程内单调时钟的微秒数，由调用方传入，库里不读时钟。

/// 进程内单调时钟，单位微秒
pub type Micros = u64;

/// 毫秒 → Micros
pub const fn ms(v: u64) -> Micros {
    v * 1_000
}

/// 秒 → Micros
pub const fn secs(v: u64) -> Micros {
    v * 1_000_000
}

/// SendTS：发送方单调时钟微秒数的低 32 位（约 71 分钟回绕，按有符号差值比较）
pub fn send_ts(now: Micros) -> u32 {
    now as u32
}

/// 两个 SendTS 的有符号差值 a − b（微秒）
pub fn send_ts_diff(a: u32, b: u32) -> i32 {
    a.wrapping_sub(b) as i32
}

/// 16 位序号比较：a 是否在 b 之前（差值按 i16 解释）
pub fn seq_lt(a: u16, b: u16) -> bool {
    (b.wrapping_sub(a) as i16) > 0
}

/// 把 16 位回绕的序号展开成单调的 u64。
/// 以迄今最大的展开值为基准，取离它最近（±2¹⁵）的那个数；
/// 比第一个值还早、展开后会小于 0 的，钳到 0。
#[derive(Clone, Copy, Debug, Default)]
pub struct Unwrap16 {
    max: Option<u64>,
}

impl Unwrap16 {
    pub const fn new() -> Self {
        Self { max: None }
    }

    pub fn unwrap(&mut self, v: u16) -> u64 {
        let out = match self.max {
            None => v as u64,
            Some(max) => {
                let d = v.wrapping_sub(max as u16) as i16 as i64;
                (max as i64 + d).max(0) as u64
            }
        };
        if self.max.is_none_or(|m| out > m) {
            self.max = Some(out);
        }
        out
    }
}

/// 同 [`Unwrap16`]，32 位（±2³¹）
#[derive(Clone, Copy, Debug, Default)]
pub struct Unwrap32 {
    max: Option<u64>,
}

impl Unwrap32 {
    pub const fn new() -> Self {
        Self { max: None }
    }

    pub fn unwrap(&mut self, v: u32) -> u64 {
        let out = match self.max {
            None => v as u64,
            Some(max) => {
                let d = v.wrapping_sub(max as u32) as i32 as i64;
                (max as i64 + d).max(0) as u64
            }
        };
        if self.max.is_none_or(|m| out > m) {
            self.max = Some(out);
        }
        out
    }
}

// =====================================================================
// 两端共用
// =====================================================================

/// 握手尝试的状态保留多久（两端一致；份数见 consts::HS_ATTEMPTS_KEPT）
pub const HS_ATTEMPT_TTL: Micros = secs(5);
/// CLOSE 一律发 3 份：0、5、10 ms（两端、所有 code 都一样），然后丢弃密钥
pub const CLOSE_COPIES: [Micros; 3] = [0, ms(5), ms(10)];
/// 会话超过这么久，客户端带 resume 重新握手（v1 不做密钥更新，规格 §4.7）
pub const SESSION_MAX_AGE: Micros = secs(24 * 3600);
/// RTT 低于它才算局域网（规格 §9.2）
pub const LAN_RTT_MAX: Micros = ms(2);

// ---- CONTROL 可靠层（附录 A.6） ----

/// RTO = max(25 ms, SRTT + 4 × RTTVAR)，封顶 1 s；以握手 RTT 作种子
pub const CTL_RTO_MIN: Micros = ms(25);
pub const CTL_RTO_MAX: Micros = secs(1);
/// 收端 2 ms 内回 ACK
pub const CTL_ACK_DELAY: Micros = ms(2);
/// 关键消息在 +5 ms 再发一份
pub const CTL_CRITICAL_DUP: Micros = ms(5);

// =====================================================================
// 网关（附录 B）
// =====================================================================

/// 半开会话：HS_RESP 之后等第一个 DATA
pub const HALF_OPEN_TIMEOUT: Micros = secs(3);
/// 已确认会话没有任何包就删除
pub const SESSION_IDLE_TIMEOUT: Micros = secs(60);
/// RESET 全局限频：每秒最多这么多个
pub const RESET_GLOBAL_PER_SEC: u32 = 50;
/// 中性化：这么久没有 INPUT
pub const INPUT_NEUTRAL_TIMEOUT: Micros = ms(150);
/// 推迟抬起：min(原始间隔, 17 ms)
pub const RELEASE_DEFER_MAX: Micros = ms(17);
/// 事件发生在这么久之前的（换算到网关时钟），只并入状态，不单独回放
pub const INPUT_EVENT_STALE: Micros = ms(150);
/// FEEDBACK：变化时在这几个时刻各发一次，之后每 250 ms 保活
pub const FEEDBACK_BURST: [Micros; 3] = [0, ms(8), ms(24)];
pub const FEEDBACK_KEEPALIVE: Micros = ms(250);
/// UP_REPORT 周期
pub const UP_REPORT_INTERVAL: Micros = ms(100);
/// 修复幂等窗口：max(RTT_p50, 10 ms)
pub const REPAIR_IDEMPOTENT_MIN: Micros = ms(10);
/// 发送缓存：1 s 或 8 MB，先到为准
pub const SEND_CACHE_AGE: Micros = secs(1);
pub const SEND_CACHE_BYTES: usize = 8 << 20;
/// 修复预算：10 个 10 ms 的桶；修复 ≤ max(媒体 / 4, 32 KiB)
pub const REPAIR_BUCKETS: usize = 10;
pub const REPAIR_BUCKET_LEN: Micros = ms(10);
pub const REPAIR_MEDIA_DIV: usize = 4;
pub const REPAIR_MIN_BYTES: usize = 32 << 10;
/// IDR：每秒 ≤ 1 次，同时 ≤ 1 个未决；1.2 s 重发；5 s 后 Escalate
pub const IDR_MIN_INTERVAL: Micros = secs(1);
pub const IDR_RESEND: Micros = ms(1_200);
pub const IDR_ESCALATE: Micros = secs(5);
/// 监督线程一拍（IdrGovernor::tick 在这里调）
pub const SUPERVISOR_TICK: Micros = ms(100);
/// Ingress：有客户端时这么久没有新帧，请求 IDR 并发 KEY_PENDING
pub const INGRESS_NO_FRAME: Micros = ms(300);
/// 盲发档：这么久没有任何包
pub const BLIND_AFTER: Micros = ms(500);
/// 进入 Held：这么久没有任何包（或收到 CLOSE）
pub const HELD_AFTER: Micros = secs(10);
/// Held 期间的心跳：每 1 s，最多 60 s
pub const HELD_HEARTBEAT: Micros = secs(1);
pub const HELD_HEARTBEAT_MAX: Micros = secs(60);
/// 保持期默认值（gateway.conf 的 hold_minutes；0 表示一直持有）
pub const HOLD_DEFAULT_MINUTES: u32 = 10;
/// 退出时发完 CLOSE(5) 最多等这么久再结束 PS5 会话
pub const EXIT_CLOSE_WAIT: Micros = ms(200);
/// 发送遇到 ENOBUFS：1 ms 后重试，连续 3 次失败丢掉这一包
pub const ENOBUFS_RETRY: Micros = ms(1);
pub const ENOBUFS_MAX_TRIES: u8 = 3;
/// 重建：先等 3 s；之后每 2 s；30 s 后每 5 s；5 min 后每 30 s 并报错（不放弃）
pub const REBUILD_FIRST_WAIT: Micros = secs(3);
/// (从首次失败起算的时长, 之后的重试间隔)，按顺序取第一个时长还没到的档
pub const REBUILD_BACKOFF: [(Micros, Micros); 3] = [(secs(30), secs(2)), (secs(300), secs(5)), (Micros::MAX, secs(30))];
/// 5 min 后开始报错
pub const REBUILD_ERROR_AFTER: Micros = secs(300);
/// PS5_CMD 限频：同一种命令每 2 s 最多 1 次
pub const PS5_CMD_MIN_INTERVAL: Micros = secs(2);
/// 塌陷提示：持续 30 s 才提示；被拒后 10 min 内不再提示
pub const COLLAPSE_PROMPT_SUSTAIN: Micros = secs(30);
pub const COLLAPSE_PROMPT_SNOOZE: Micros = secs(600);
/// RATE_STATE：每 1 s，T_cur 变化超过 5%（50 ‰）时立即发
pub const RATE_STATE_INTERVAL: Micros = secs(1);
pub const RATE_STATE_CHANGE_PERMILLE: u32 = 50;
/// FRAME_STATUS 要在连续 3 个包里各带一次；5 ms 内没有包就单独发
pub const FRAME_STATUS_ALONE_AFTER: Micros = ms(5);

// =====================================================================
// 客户端（附录 B）
// =====================================================================

/// 握手：首包双发，间隔 10 ms；之后 250、500、1000、2000 ms，封顶 2 s
pub const HS_FIRST_DUP: Micros = ms(10);
pub const HS_BACKOFF: [Micros; 4] = [ms(250), ms(500), secs(1), secs(2)];
pub const HS_BACKOFF_CAP: Micros = secs(2);
/// 发出第一个 DATA 后这么久没有回音，重新握手
pub const FIRST_DATA_NO_REPLY: Micros = secs(1);
/// 这么久没有任何包就放弃旧密钥、重新握手（收到有效 RESET 立即重握）
pub const KEY_ABANDON_SILENCE: Micros = secs(30);
/// PING 周期
pub const PING_INTERVAL: Micros = ms(100);
/// 等关键帧：这么久没到就再发 IDR_REQUEST
pub const KEYFRAME_WAIT_RESEND: Micros = ms(1_200);
/// 阻塞转 L3（或收到 GONE）
pub const BLOCKED_TO_L3: Micros = ms(1_000);
/// 输入保活
pub const INPUT_KEEPALIVE: Micros = ms(16);
/// 离散事件的 5 份副本：0、+1、+4、+8、+16 ms
pub const INPUT_COPIES: [Micros; 5] = [0, ms(1), ms(4), ms(8), ms(16)];
/// 模拟量两次发送的最小间隔
pub const INPUT_ANALOG_MIN_GAP: Micros = ms(4);
/// REPORT：每 20 ms，等 ≤ 4 ms 搭车
pub const REPORT_INTERVAL: Micros = ms(20);
pub const REPORT_PIGGYBACK_WAIT: Micros = ms(4);
/// 这么久收不到 FEEDBACK 就停震动
pub const RUMBLE_SAFETY_STOP: Micros = secs(1);
/// 每次下发震动的持续时间（每收到一份 FEEDBACK 重新下发，断线后自然停）
pub const RUMBLE_DURATION: Micros = secs(1);
/// 判丢宽限 g
pub const LOSS_GRACE: Micros = ms(2);
/// NACK：判丢后 1 ms 内发出，≥ 2 ms 后再发一次
pub const NACK_FIRST_WITHIN: Micros = ms(1);
pub const NACK_REPEAT_AFTER: Micros = ms(2);
/// R_wait：竞技 / 均衡 / 稳定
pub const R_WAIT: [Micros; 3] = [ms(150), ms(200), ms(250)];
/// NEED 重发：max(RTT_p50, 10 ms)
pub const NEED_RESEND_MIN: Micros = ms(10);
/// 提示「网络中断」：这么久没有任何包
pub const NET_DOWN_HINT: Micros = secs(1);
/// 提示「上行中断」：媒体照常到达，但这么久没有 UP_REPORT 或 PONG
pub const UPLINK_DOWN_HINT: Micros = secs(1);

// ---- Assembler（M2a 规则，计划 §3.1） ----

/// 某帧不完整、而它后面已经有完整的帧，并且超过这么久，判 Lost
pub const ASM_LOST_BEHIND_COMPLETE: Micros = ms(50);
/// 某帧自己挂了这么久，判 Lost
pub const ASM_FRAME_HANG: Micros = ms(1_000);
/// cfg 对不上的帧最多压这么久，超时按 Lost 处理
pub const ASM_CFG_HOLD_MAX: Micros = ms(1_000);
/// W 帧之后这么久内的解码报错单独统计
pub const ASM_W_ERROR_WINDOW: Micros = secs(1);

// ---- 冻结统计（W3C 口径） ----

/// 呈现间隔 ≥ max(3 × 平均帧间隔, 平均帧间隔 + 150 ms) 记一次冻结
pub const FREEZE_MULT: u64 = 3;
pub const FREEZE_EXTRA: Micros = ms(150);

// ---- 音频（计划 §3.2） ----

/// 每个 Opus 帧 10 ms
pub const AUDIO_FRAME: Micros = ms(10);
/// 抖动缓冲目标水位：竞技 / 均衡 / 稳定
pub const AUDIO_TARGET: [Micros; 3] = [ms(20), ms(30), ms(50)];
/// 水位超过目标这么多时丢帧，回到目标
pub const AUDIO_OVERFILL: Micros = ms(60);
/// 连续 PLC 超过这么多帧就渐弱到静音
pub const AUDIO_PLC_FADE_FRAMES: u32 = 5;
/// 变速比范围 ±0.5%
pub const AUDIO_RATE_MAX_DEV: f32 = 0.005;
/// 抖动缓冲的槽数（按 aseq 存）
pub const AUDIO_SLOTS: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_compare_wraps() {
        assert!(seq_lt(1, 2));
        assert!(!seq_lt(2, 1));
        assert!(seq_lt(0xFFFF, 0));
        assert!(!seq_lt(5, 5));
    }

    #[test]
    fn unwrap16_crosses_wrap() {
        let mut u = Unwrap16::new();
        assert_eq!(u.unwrap(0xFFFE), 0xFFFE);
        assert_eq!(u.unwrap(0xFFFF), 0xFFFF);
        assert_eq!(u.unwrap(0), 0x1_0000);
        // 乱序晚到的旧值仍然展开到回绕之前
        assert_eq!(u.unwrap(0xFFFD), 0xFFFD);
        assert_eq!(u.unwrap(3), 0x1_0003);
    }

    #[test]
    fn unwrap_clamps_before_first() {
        let mut u = Unwrap16::new();
        assert_eq!(u.unwrap(2), 2);
        assert_eq!(u.unwrap(0xFFF0), 0);
        let mut v = Unwrap32::new();
        assert_eq!(v.unwrap(u32::MAX), u32::MAX as u64);
        assert_eq!(v.unwrap(1), u32::MAX as u64 + 2);
    }

    #[test]
    fn send_ts_signed_diff() {
        assert_eq!(send_ts(0x1_0000_0005), 5);
        assert_eq!(send_ts_diff(5, u32::MAX), 6);
        assert_eq!(send_ts_diff(u32::MAX, 5), -6);
    }
}
