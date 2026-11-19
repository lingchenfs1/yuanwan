//! 错误类型和丢弃原因（归协调人）。
//!
//! 网络输入一律不 panic：解析返回 [`WireError`]，被丢弃的包、块、帧按 [`DropReason`] 计数，
//! 由上层写进 JSONL。要加原因找协调人，名字（JSONL 里的键）一经发布不改。

use std::fmt;

/// 字节层面的解析和编码错误
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    /// 比最小长度还短
    Short,
    /// 外层头 Ver ≠ 01
    BadVersion,
    /// 保留的包类型或块类型
    ReservedType,
    /// 字段声明的长度超出了剩余字节
    Truncated,
    /// 字段取值不合法
    Malformed(&'static str),
    /// 编码时输出缓冲不够
    NoRoom,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Short => f.write_str("包太短"),
            WireError::BadVersion => f.write_str("版本不符"),
            WireError::ReservedType => f.write_str("保留类型"),
            WireError::Truncated => f.write_str("长度越界"),
            WireError::Malformed(what) => write!(f, "格式错误：{what}"),
            WireError::NoRoom => f.write_str("输出缓冲不够"),
        }
    }
}

impl std::error::Error for WireError {}

/// 定义 DropReason：变体、JSONL 里的名字、全集
macro_rules! drop_reasons {
    ($( $(#[$m:meta])* $v:ident => $name:literal, )+) => {
        /// 丢弃原因。每种原因有自己的计数器（[`DropCounters`]）
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum DropReason {
            $( $(#[$m])* $v, )+
        }

        impl DropReason {
            /// 全部原因，按声明顺序
            pub const ALL: &'static [DropReason] = &[ $( DropReason::$v, )+ ];
            pub const COUNT: usize = Self::ALL.len();

            /// JSONL 里用的名字（snake_case）
            pub const fn name(self) -> &'static str {
                match self {
                    $( DropReason::$v => $name, )+
                }
            }
        }
    };
}

drop_reasons! {
    // ---- 包长、外层头（A 组） ----
    /// 比 RECV_MIN 短，或比该包类型的最小长度短
    TooShort => "too_short",
    /// 比 RECV_MAX 长
    TooLong => "too_long",
    /// 外层头解析失败
    BadHeader => "bad_header",
    /// Ver ≠ 01
    BadVersion => "bad_version",
    /// 外层 Type = 7（保留）
    ReservedType => "reserved_type",
    /// v1 不支持的包类型或功能（HS_COOKIE、HOP、RELAY_INIT……），计数丢弃
    Unsupported => "unsupported",
    /// K = 1（v1 不做密钥更新，B 组判）
    KeyPhase => "key_phase",
    /// 非 DATA 包的 Class 不是 0
    BadClass => "bad_class",

    // ---- 加密与重放（A 组） ----
    /// AEAD 校验失败
    AeadFail => "aead_fail",
    /// 重放窗口里已见过的 PN
    Replay => "replay",
    /// 落在重放窗口左边之外
    TooOld => "too_old",

    // ---- 块解析（A 组） ----
    /// 0x01~0x0F 保留块，整包丢弃，前面的块也不生效
    ReservedChunk => "reserved_chunk",
    /// 第一遍校验失败（长度越界、到包尾的块后面还有块……），整包丢弃
    MalformedChunk => "malformed_chunk",
    /// 已知块的 len 比定义短，跳过这一块（长了只读定义的部分）
    ShortChunk => "short_chunk",
    /// 类型 ≥ 0x20 的未知块，按 len 跳过（不丢包，只计数）
    UnknownChunk => "unknown_chunk",
    /// 不该在这个方向出现的块（比如客户端收到 INPUT）
    WrongDirection => "wrong_direction",

    // ---- 握手（A 组） ----
    /// HS_INIT 短于 1180 B
    HsTooShort => "hs_too_short",
    /// mac1 不对，静默丢弃
    Mac1Fail => "mac1_fail",
    /// Noise 解不开：网关公钥错、PSK 错、prologue 不符……
    HsNoiseFail => "hs_noise_fail",
    /// 静态公钥不在已配对列表（不更新 max_ts）
    UnknownClient => "unknown_client",
    /// TAI64N 没有大于这个客户端上一次的值
    StaleTimestamp => "stale_timestamp",
    /// 握手 TLV 格式错、type 重复、缺必带项
    HsBadTlv => "hs_bad_tlv",
    /// 收到不该收的握手包：没在等 HS_RESP，或 PN 对不上任何一次尝试
    HsUnexpected => "hs_unexpected",
    /// 握手限频
    HsRateLimited => "hs_rate_limited",

    // ---- RESET（A 组编解码，B 组处理） ----
    /// 长度不是 38 B
    ResetBadLen => "reset_bad_len",
    /// CID 不是自己当前的 CID
    ResetBadCid => "reset_bad_cid",
    /// reset_token 不一致（常量时间比较）
    ResetBadToken => "reset_bad_token",
    /// 未知 CID 的 DATA 短于 64 B，不回 RESET
    ResetTriggerShort => "reset_trigger_short",
    /// RESET 全局限频，没有回
    ResetRateLimited => "reset_rate_limited",

    // ---- 会话（B 组） ----
    /// DATA 包的 CID 没有对应的会话
    UnknownCid => "unknown_cid",
    /// 源地址不是当前对端地址
    NotPeer => "not_peer",
    /// path_id 不是当前路径（v1 单路径）
    UnknownPath => "unknown_path",
    /// 密钥还没确认时收到不该收的内容
    NotConfirmed => "not_confirmed",
    /// 会话已关闭（CLOSE 之后、密钥已丢弃）
    SessionClosed => "session_closed",

    // ---- CONTROL（B 组） ----
    /// stream 不是 0、1、2
    CtlBadStream => "ctl_bad_stream",
    /// v1 只支持单片
    CtlMultiFrag => "ctl_multi_frag",
    /// msg_seq 超出 32 条的窗口
    CtlOutOfWindow => "ctl_out_of_window",
    /// 已交付过的消息
    CtlDuplicate => "ctl_duplicate",
    /// 消息体解不开（TLV 错）
    CtlBadMsg => "ctl_bad_msg",
    /// 不认识的 msg_type（ControlMsg::Unknown）
    CtlUnknownMsg => "ctl_unknown_msg",
    /// 单片超过上限（下行 1100 B、上行 240 B）
    CtlTooLarge => "ctl_too_large",

    // ---- 输入、反馈、音频（B 组） ----
    /// n_ev > 8
    InputTooManyEvents => "input_too_many_events",
    /// 保留的 EVENT code，网关忽略并计数
    InputReservedCode => "input_reserved_code",
    /// fb_seq 不比已应用的新
    FeedbackStale => "feedback_stale",
    /// AUDIO 的 cnt 不在 1~4
    AudioBadCount => "audio_bad_count",
    /// 已收过的音频帧（冗余副本去重）
    AudioDuplicate => "audio_duplicate",
    /// 晚于播放位置才到的音频帧
    AudioLate => "audio_late",
    /// 抖动缓冲水位过高丢的帧
    AudioOverfill => "audio_overfill",

    // ---- NACK / NEED / FRAME_STATUS（B、C 组） ----
    /// NACK 段数不在 1~16，或 count = 0
    NackBadSegments => "nack_bad_segments",
    /// NACK 指向发送记录里已经没有的 PN
    NackUnknownPn => "nack_unknown_pn",
    /// NEED 指向发送缓存里没有的帧
    NeedUnknownFid => "need_unknown_fid",
    /// 不认识的 FRAME_STATUS status
    FrameStatusBadKind => "frame_status_bad_kind",

    // ---- 视频块与 FEC（C 组） ----
    /// k、R 越界（k 1..=1024，R ≤ 1023，k + R ≤ 2048），或 sym_idx ≥ k + R
    VideoBadFec => "video_bad_fec",
    /// P 位和 sym_idx ≥ k 不一致，丢这个块
    VideoParityMismatch => "video_parity_mismatch",
    /// 校验符号的 S 是奇数
    VideoOddShard => "video_odd_shard",
    /// 同一帧校验包的 S 不同（以第一个校验包为准）
    VideoShardMismatch => "video_shard_mismatch",
    /// 源包的 12 + len 没有正好到包尾，或 len > S − 2
    VideoBadLen => "video_bad_len",
    /// 同一帧前后 k 不一致
    VideoKMismatch => "video_k_mismatch",
    /// 帧已交付或已放弃
    VideoStaleFid => "video_stale_fid",
    /// 重复的符号
    VideoDuplicate => "video_duplicate",
    /// 同时在组的帧太多
    VideoTooManyFrames => "video_too_many_frames",
    /// RS 恢复失败
    FecDecodeFail => "fec_decode_fail",

    // ---- 本端发送侧（D 组的服务循环、客户端 net 线程） ----
    /// ENOBUFS 连续 3 次，丢掉这一包
    SendNoBufs => "send_nobufs",
    /// 发送调度队列满
    SendQueueFull => "send_queue_full",
    /// MediaMsg 队列满（chiaki 回调里 try_send 失败）
    MediaQueueFull => "media_queue_full",
    /// 帧太大（k 超过 1024），网关不转发、进 WaitKey
    FrameTooLarge => "frame_too_large",
}

/// 按原因计数
#[derive(Clone, Debug)]
pub struct DropCounters {
    counts: [u64; DropReason::COUNT],
}

impl Default for DropCounters {
    fn default() -> Self {
        Self { counts: [0; DropReason::COUNT] }
    }
}

impl DropCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bump(&mut self, r: DropReason) {
        let c = &mut self.counts[r as usize];
        *c = c.saturating_add(1);
    }

    pub fn get(&self, r: DropReason) -> u64 {
        self.counts[r as usize]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().fold(0u64, |a, &b| a.saturating_add(b))
    }

    /// 只列出非零的原因（写 JSONL 用），按声明顺序
    pub fn snapshot(&self) -> Vec<(&'static str, u64)> {
        DropReason::ALL
            .iter()
            .filter(|r| self.counts[**r as usize] != 0)
            .map(|r| (r.name(), self.counts[*r as usize]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_unique_and_counters_work() {
        let mut names: Vec<_> = DropReason::ALL.iter().map(|r| r.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), DropReason::COUNT);
        for (i, r) in DropReason::ALL.iter().enumerate() {
            assert_eq!(*r as usize, i);
        }

        let mut c = DropCounters::new();
        c.bump(DropReason::AeadFail);
        c.bump(DropReason::AeadFail);
        c.bump(DropReason::Replay);
        assert_eq!(c.get(DropReason::AeadFail), 2);
        assert_eq!(c.total(), 3);
        assert_eq!(c.snapshot(), vec![("aead_fail", 2), ("replay", 1)]);
    }
}
