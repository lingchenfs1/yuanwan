//! 线上格式的常量（归协调人）：规格 ywp.md v0.2（§4、§5、附录 A）。
//! 计时器在 [`crate::time`]。要加常量找协调人，不要在各组的文件里另写一份。

// ======== 版本、端口 ========

/// 协议版本（TLV 0x02 的 ver）
pub const PROTO_VERSION: u8 = 1;
/// 最低兼容版本（TLV 0x02 的 min_ver）
pub const PROTO_MIN_VERSION: u8 = 1;
/// 外层头 Flags 的 Ver 字段（高 2 位）= 01
pub const FLAGS_VER: u8 = 0b01;
/// 默认 UDP 端口（gateway.conf 的 ywp_port 可改）
pub const DEFAULT_PORT: u16 = 9420;

// ======== 包长 ========

/// 外层头：Flags 1 + CID 8 + path_id 1 + PN 低 32 位 4
pub const HDR_LEN: usize = 14;
/// DATA 包明文开头的 SendTS
pub const SEND_TS_LEN: usize = 4;
/// ChaCha20-Poly1305 标签
pub const TAG_LEN: usize = 16;
/// 发出的包一律不超过它（IPv6 最小 MTU 1280 − 48）
pub const BASE_PAYLOAD: usize = 1232;
/// 上行（客户端 → 网关）整包上限
pub const UPLINK_MAX: usize = 400;
/// 收包缓冲；大于它的包丢弃
pub const RECV_MAX: usize = 1500;
/// 收端只处理 RECV_MIN..=RECV_MAX 的 UDP 载荷，其余直接丢弃并计数。
/// 34 = 外层头 14 + SendTS 4 + 标签 16，即最短的 DATA 包（RESET 38 B 也在范围内）
pub const RECV_MIN: usize = HDR_LEN + SEND_TS_LEN + TAG_LEN;
/// DATA 包的固定开销：外层头 + SendTS + 标签
pub const DATA_OVERHEAD: usize = HDR_LEN + SEND_TS_LEN + TAG_LEN;

// ======== 外层头 Flags ========

/// Flags 位布局：Ver(7..6) | Type(5..3) | K(2) | Class(1..0)
pub const FLAGS_VER_SHIFT: u8 = 6;
pub const FLAGS_TYPE_SHIFT: u8 = 3;
pub const FLAGS_TYPE_MASK: u8 = 0b111;
pub const FLAGS_K_BIT: u8 = 0b100;
pub const FLAGS_CLASS_MASK: u8 = 0b11;

/// 握手包和 RESET 的 Flags 字节是固定值（规格 v0.2 §4.4）
pub const FLAGS_HS_INIT: u8 = 0x48;
pub const FLAGS_HS_RESP: u8 = 0x50;
pub const FLAGS_RESET: u8 = 0x70;
const _: () = assert!(FLAGS_HS_INIT == (FLAGS_VER << FLAGS_VER_SHIFT) | (ptype::HS_INIT << FLAGS_TYPE_SHIFT));
const _: () = assert!(FLAGS_HS_RESP == (FLAGS_VER << FLAGS_VER_SHIFT) | (ptype::HS_RESP << FLAGS_TYPE_SHIFT));
const _: () = assert!(FLAGS_RESET == (FLAGS_VER << FLAGS_VER_SHIFT) | (ptype::RESET << FLAGS_TYPE_SHIFT));

/// 外层 Type 的取值（wire::PacketType 的判别值）
pub mod ptype {
    pub const DATA: u8 = 0;
    pub const HS_INIT: u8 = 1;
    pub const HS_RESP: u8 = 2;
    /// 后（cookie）
    pub const HS_COOKIE: u8 = 3;
    /// 后（中转逐跳）
    pub const HOP: u8 = 4;
    /// 后（中转）
    pub const RELAY_INIT: u8 = 5;
    pub const RESET: u8 = 6;
    /// 保留，收到计数丢弃
    pub const RESERVED: u8 = 7;
}

/// Class 的取值：供中转排优先级、局域网打 DSCP；非 DATA 包填 0
pub mod class {
    pub const BULK: u8 = 0;
    pub const VIDEO: u8 = 1;
    pub const MEDIA: u8 = 2;
    pub const INPUT: u8 = 3;
}

// ======== 握手包（附录 A.1） ========

/// v1 发出的 HS_INIT 固定长度
pub const HS_INIT_LEN: usize = 1232;
/// 网关只接受不短于它的 HS_INIT
pub const HS_INIT_MIN: usize = 1180;
/// Noise 的临时公钥 `e`
pub const HS_E_LEN: usize = 32;
/// 加密的静态公钥 `s`（32 B + 16 B 标签）
pub const HS_S_ENC_LEN: usize = 48;
/// HS_INIT 加密载荷的起点：14 + 32 + 48
pub const HS_INIT_PAYLOAD_OFF: usize = HDR_LEN + HS_E_LEN + HS_S_ENC_LEN;
/// mac1、mac2 各 16 B，固定在包尾：mac1 在 [L−32, L−16)，mac2 在 [L−16, L)
pub const HS_MAC_LEN: usize = 16;
/// 1232 B 的 HS_INIT 里，加密载荷的明文长度（TLV 后补 0x00）
pub const HS_INIT_PLAINTEXT_LEN: usize = HS_INIT_LEN - HS_INIT_PAYLOAD_OFF - TAG_LEN - 2 * HS_MAC_LEN;
/// mac1 的密钥 = BLAKE2s-256(MAC1_LABEL ‖ 对端静态公钥)
pub const MAC1_LABEL: &[u8; 8] = b"mac1----";
/// mac1 是带密钥的 BLAKE2s，digest_length = 16（按参数块计算，不是把 256 位结果截断）
pub const MAC1_DIGEST_LEN: usize = 16;

/// RESET：外层头 14 + 随机 8 + reset_token 16
pub const RESET_LEN: usize = 38;
pub const RESET_RANDOM_LEN: usize = 8;
pub const RESET_TOKEN_LEN: usize = 16;
/// 只对不短于它的未知 CID DATA 包回 RESET（防反射放大）
pub const RESET_TRIGGER_MIN: usize = 64;

// ======== 加密（附录 A.2） ========

pub const NOISE_PARAMS: &str = "Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s";
/// psk 放在 Noise 消息的位置 2
pub const NOISE_PSK_POS: u8 = 2;
/// prologue = PROLOGUE_PREFIX ‖ CID（8 B 大端）。**永久冻结**，版本号走 TLV 0x02
pub const PROLOGUE_PREFIX: &[u8; 8] = b"YWP v0.1";
pub const PROLOGUE_LEN: usize = PROLOGUE_PREFIX.len() + 8;
/// iv 派生：prk = HKDF-Extract(IV_SALT, 握手哈希 h)，HMAC-BLAKE2s
pub const IV_SALT: &[u8] = b"ywp v0.1 iv";
pub const IV_INFO_C2S: &[u8] = b"c2s";
pub const IV_INFO_S2C: &[u8] = b"s2c";
pub const IV_LEN: usize = 12;
pub const KEY_LEN: usize = 32;
pub const PSK_LEN: usize = 32;
/// 以后的密钥更新：k' = HKDF-Expand(k, KU_INFO, 32)（v1 不做，K 位恒为 0）
pub const KU_INFO: &[u8] = b"ywp ku";
/// TAI64N：secs = 2⁶² + Unix 秒，后接 nanos（u32），共 12 B
pub const TAI64N_BASE: u64 = 1 << 62;
pub const TAI64N_LEN: usize = 12;
/// gateway_id = BLAKE2s-256(网关公钥) 的前 8 B
pub const GATEWAY_ID_LEN: usize = 8;
/// host_id = BLAKE2s(HOST_ID_LABEL ‖ gateway.conf 的 host 字符串) 的前 8 B
pub const HOST_ID_LABEL: &[u8] = b"yw-host";
pub const HOST_ID_LEN: usize = 8;
/// 客户端身份指纹：BLAKE2s(公钥) 的前 4 B（日志里只记它和名字）
pub const CLIENT_ID_LEN: usize = 4;

// ======== PN、重放、握手尝试 ========

/// 每条路径、每个方向的重放窗口位数
pub const REPLAY_WINDOW: u64 = 4096;
/// 单个方向发满这么多包时，客户端带 resume 重新握手（v1 不做密钥更新）
pub const PN_REHANDSHAKE: u64 = 1 << 31;
/// 线上只带 PN 的低 32 位
pub const PN_TRUNC_BITS: u32 = 32;
/// 同一 CID 保留的握手尝试份数（两端一致；过期时间见 time::HS_ATTEMPT_TTL）
pub const HS_ATTEMPTS_KEPT: usize = 5;

// ======== 握手 TLV（附录 A.3）：type u8 | len u16 | value ========

pub const TLV_HDR_LEN: usize = 3;

pub mod hs_tlv {
    /// 填充：遇到就停止解析
    pub const PAD: u8 = 0x00;
    /// INIT，必带，12 B TAI64N
    pub const TS: u8 = 0x01;
    /// 双向，必带，ver u8 ‖ min_ver u8
    pub const VER: u8 = 0x02;
    /// 双向，必带，u32 能力位图；RESP 里取交集
    pub const CAPS: u8 = 0x03;
    /// INIT，可选，host_id 8 B ‖ session_epoch u32
    pub const RESUME: u8 = 0x04;
    /// INIT，可选，fid u16 ‖ cfg_ver u8
    pub const LAST_FID: u8 = 0x05;
    /// INIT，可选，mode u8 ‖ flags u8
    pub const MODE: u8 = 0x06;
    /// INIT，可选，UTF-8 ≤ 32 B
    pub const NAME: u8 = 0x07;
    /// INIT，可选，u32 kbps，0 表示不限
    pub const BITRATE_CAP: u8 = 0x08;
    /// RESP，必带，u8（hs_err）
    pub const ERR: u8 = 0x10;
    /// RESP，err = 0 时必带，16 B
    pub const RESET_TOKEN: u8 = 0x11;
    /// RESP，可选，ps_state u8 ‖ host_id 8 B ‖ session_epoch u32 ‖ 能否接回 u8
    pub const SESSION: u8 = 0x12;
    /// 后（中转）
    pub const RELAY_INFO: u8 = 0x13;

    pub const TS_LEN: usize = 12;
    pub const VER_LEN: usize = 2;
    pub const CAPS_LEN: usize = 4;
    pub const RESUME_LEN: usize = 12;
    pub const LAST_FID_LEN: usize = 3;
    pub const MODE_LEN: usize = 2;
    pub const NAME_MAX: usize = 32;
    pub const BITRATE_CAP_LEN: usize = 4;
    pub const ERR_LEN: usize = 1;
    pub const RESET_TOKEN_LEN: usize = 16;
    pub const SESSION_LEN: usize = 14;
}

/// TLV 0x03 caps 的位
pub mod caps {
    /// 每个 unit 能切 2 份（v1 必须为 1）
    pub const SPLIT2: u32 = 1 << 0;
    pub const HEVC: u32 = 1 << 1;
    pub const HDR: u32 = 1 << 2;
}

/// TLV 0x06 的 flags 位
pub mod mode_flags {
    /// 接管：另一台客户端在线时也要建会话（ywp_takeover=0 时没带它就回 BUSY）
    pub const TAKEOVER: u8 = 1 << 0;
}

/// 模式（TLV 0x06、MODE_SET、REPORT 偏移 2 的高 2 位）
pub mod mode {
    pub const COMPETITIVE: u8 = 0;
    pub const BALANCED: u8 = 1;
    pub const STABLE: u8 = 2;
}

/// TLV 0x10 err 的取值
pub mod hs_err {
    pub const OK: u8 = 0;
    pub const CID_COLLISION: u8 = 1;
    pub const BUSY: u8 = 2;
    pub const VERSION: u8 = 3;
}

// ======== 块（规格 §5.3、§5.4） ========

/// 块类型号。0x00 PADDING 和 0x10~0x1F VIDEO 延伸到包尾、没有长度字段；
/// 0x01~0x0F 保留（遇到整包丢弃）；0x20 起一律 `type u8 | len u16 | 内容`
pub mod chunk_ty {
    pub const PADDING: u8 = 0x00;
    pub const RESERVED_FIRST: u8 = 0x01;
    pub const RESERVED_LAST: u8 = 0x0F;
    /// VIDEO 占 0x10~0x1F，低 4 位是 K/P/L/X 标志
    pub const VIDEO: u8 = 0x10;
    pub const VIDEO_MASK: u8 = 0xF0;
    /// 0x20 起的块有 2 B 长度
    pub const TLV_FIRST: u8 = 0x20;
    pub const PING: u8 = 0x20;
    pub const PONG: u8 = 0x21;
    pub const PATH_CHALLENGE: u8 = 0x22;
    pub const PATH_RESPONSE: u8 = 0x23;
    pub const CLOSE: u8 = 0x24;
    pub const CONTROL: u8 = 0x28;
    pub const ACK: u8 = 0x29;
    pub const AUDIO: u8 = 0x30;
    /// 后（M4）
    pub const HAPTICS: u8 = 0x31;
    /// 后（M4）
    pub const MIC: u8 = 0x32;
    pub const INPUT: u8 = 0x38;
    pub const FEEDBACK: u8 = 0x39;
    pub const REPORT: u8 = 0x40;
    pub const NACK: u8 = 0x41;
    pub const NEED: u8 = 0x42;
    pub const FRAME_STATUS: u8 = 0x43;
    pub const UP_REPORT: u8 = 0x44;
    /// 后
    pub const PMTU_PROBE: u8 = 0x48;
    /// 后
    pub const BW_PROBE: u8 = 0x49;
    /// 后
    pub const PROBE_ACK: u8 = 0x4A;
    /// 后：0x50~0x55 HOP_BIND / HOP_NACK / HOP_PING / HOP_PONG / HOP_STATS / HOP_BIND_ACK，只用在 HOP 包里
    pub const HOP_FIRST: u8 = 0x50;
    pub const HOP_LAST: u8 = 0x55;
}

/// 带长度的块：type 1 + len 2
pub const CHUNK_TLV_HDR: usize = 3;

/// 各块的内容长度（不含 type 和 len）
pub mod chunk_len {
    pub const PING: usize = 2;
    pub const PONG: usize = 8;
    pub const PATH_CHALLENGE: usize = 8;
    pub const PATH_RESPONSE: usize = 8;
    /// CLOSE = code u16 + 原因（UTF-8 ≤ CLOSE_REASON_MAX）
    pub const CLOSE_MIN: usize = 2;
    pub const CLOSE_REASON_MAX: usize = 64;
    /// CONTROL = stream 1 + msg_seq 2 + frag 1 + 数据
    pub const CONTROL_HDR: usize = 4;
    pub const ACK: usize = 7;
    /// AUDIO = aseq 2 + age 2 + cnt 1 + cnt × {len u16, 数据}
    pub const AUDIO_HDR: usize = 5;
    pub const AUDIO_FRAME_HDR: usize = 2;
    /// INPUT = in_seq 2 + 标志 1 + STATE 40 + 6 × n_ev
    pub const INPUT_FIXED: usize = 43;
    pub const FEEDBACK: usize = 32;
    /// REPORT = 20 + 22 × 路径数
    pub const REPORT_FIXED: usize = 20;
    pub const REPORT_PATH: usize = 22;
    /// NACK = path_id 1 + n 1 + 5 × n
    pub const NACK_HDR: usize = 2;
    pub const NACK_SEG: usize = 5;
    pub const NEED: usize = 4;
    /// FRAME_STATUS = n 1 + 4 × n
    pub const FRAME_STATUS_HDR: usize = 1;
    pub const FRAME_STATUS_ENTRY: usize = 4;
    /// UP_REPORT = n 1 + 12 × 路径数
    pub const UP_REPORT_HDR: usize = 1;
    pub const UP_REPORT_PATH: usize = 12;
}

/// REPORT / UP_REPORT 里的路径数上限
pub const MAX_PATHS: usize = 4;
/// NACK 每块 1~16 段，每段 count 1~255
pub const NACK_MAX_SEGS: usize = 16;

// ======== VIDEO（规格 §5.5.1、附录 A.7） ========

/// VIDEO 块头：type/标志 1 + fid 2 + FEC 字 4 + age 2 + meta 1
pub const VIDEO_HDR_LEN: usize = 10;
/// 源符号的 len 字段
pub const VIDEO_SRC_LEN_LEN: usize = 2;
/// 源符号数据的上限：1232 − 14 − 4 − 10 − 2 − 16
pub const MAX_SYMBOL_DATA: usize = BASE_PAYLOAD - DATA_OVERHEAD - VIDEO_HDR_LEN - VIDEO_SRC_LEN_LEN;
/// 符号长度 S 的上限（2 + 1186）
pub const MAX_SHARD_LEN: usize = VIDEO_SRC_LEN_LEN + MAX_SYMBOL_DATA;

/// VIDEO 类型字节的低 4 位
pub mod video_flags {
    /// K：关键帧
    pub const KEY: u8 = 0x08;
    /// P：校验符号
    pub const PARITY: u8 = 0x04;
    /// L：本帧初次发送中的最后一个源符号
    pub const LAST_SRC: u8 = 0x02;
    /// X：修复包（响应 NACK 或 NEED 重发的）
    pub const REPAIR: u8 = 0x01;
}

/// FEC 字：sym_idx(31..21) | k−1(20..10) | R(9..0)
pub const FEC_SYM_SHIFT: u32 = 21;
pub const FEC_K_SHIFT: u32 = 10;
pub const FEC_11_MASK: u32 = 0x7FF;
pub const FEC_R_MASK: u32 = 0x3FF;
/// 每帧源符号数 1..=1024
pub const FEC_K_MAX: u16 = 1024;
/// 每帧校验符号数 ≤ 1023
pub const FEC_R_MAX: u16 = 1023;
/// k + R ≤ 2048
pub const FEC_KR_MAX: u16 = 2048;

/// meta：profile(7..5) | n−1(4..3) | cfg(2..1) | W(0)
pub const META_PROFILE_SHIFT: u8 = 5;
pub const META_PROFILE_MASK: u8 = 0b111;
pub const META_N_SHIFT: u8 = 3;
pub const META_N_MASK: u8 = 0b11;
pub const META_CFG_SHIFT: u8 = 1;
pub const META_CFG_MASK: u8 = 0b11;
pub const META_W_BIT: u8 = 0b1;

// ======== AUDIO（§5.5.2） ========

/// 每块 1~4 帧
pub const AUDIO_MAX_FRAMES: usize = 4;
/// v1 每包固定带最新 2 帧（§6.1 第 12 条）
pub const AUDIO_FRAMES_V1: usize = 2;

// ======== INPUT（§5.5.3、附录 A.4） ========

pub const INPUT_STATE_LEN: usize = 40;
pub const INPUT_EVENT_LEN: usize = 6;
pub const INPUT_MAX_EVENTS: usize = 8;
/// 标志字节：位 7 A = 手柄已连接；位 6 Z = 主动归零；位 3~0 = 事件数
pub const INPUT_FLAG_CONNECTED: u8 = 0x80;
pub const INPUT_FLAG_ZERO: u8 = 0x40;
pub const INPUT_NEV_MASK: u8 = 0x0F;
/// STATE 偏移 38 的 flags：位 0 M（运动数据有效）、位 1 T（触摸数据有效）
pub const INPUT_STATE_FLAG_MOTION: u16 = 1 << 0;
pub const INPUT_STATE_FLAG_TOUCH: u16 = 1 << 1;
/// 触摸 id = −1 表示没有触摸；后 3 B = x << 12 | y，各 12 位原始坐标
pub const TOUCH_NONE: i8 = -1;
pub const TOUCH_COORD_BITS: u32 = 12;
pub const TOUCH_COORD_MAX: u16 = (1 << TOUCH_COORD_BITS) - 1;
/// 陀螺仪（rad/s）先钳到 ±30，u16 = trunc(65535 × (v + 30) / 60)
pub const GYRO_RANGE: f32 = 30.0;
/// 加速度（g）先钳到 ±5，u16 = trunc(65535 × (v + 5) / 10)
pub const ACCEL_RANGE: f32 = 5.0;
/// SDL 给的加速度是 m/s²，先除以它换成 g
pub const STANDARD_GRAVITY: f32 = 9.80665;

/// buttons 的位号（附录 A.4）；16~31 为 0
pub mod button {
    pub const CROSS: u8 = 0;
    pub const CIRCLE: u8 = 1;
    pub const SQUARE: u8 = 2;
    pub const TRIANGLE: u8 = 3;
    pub const DPAD_LEFT: u8 = 4;
    pub const DPAD_RIGHT: u8 = 5;
    pub const DPAD_UP: u8 = 6;
    pub const DPAD_DOWN: u8 = 7;
    pub const L1: u8 = 8;
    pub const R1: u8 = 9;
    pub const L3: u8 = 10;
    pub const R3: u8 = 11;
    pub const OPTIONS: u8 = 12;
    pub const CREATE: u8 = 13;
    pub const TOUCHPAD: u8 = 14;
    pub const PS: u8 = 15;
    pub const COUNT: u8 = 16;
}

/// EVENT 的 code（附录 A.4）。触摸移动和扳机中间值只走 STATE
pub mod ev_code {
    /// 0x00~0x0F：按键，对应 buttons 的位号；value 1 按下 / 0 抬起
    pub const BUTTON_FIRST: u8 = 0x00;
    pub const BUTTON_LAST: u8 = 0x0F;
    /// 0x10~0x1F 保留给以后的按键
    pub const BUTTON_RESERVED_FIRST: u8 = 0x10;
    pub const BUTTON_RESERVED_LAST: u8 = 0x1F;
    /// L2 / R2 离开 0 或回到 0，value = 当时的模拟量
    pub const L2: u8 = 0x20;
    pub const R2: u8 = 0x21;
    /// 触摸槽 0 / 1 开始，value = 触摸 id（0~127，客户端递增分配）
    pub const TOUCH0_START: u8 = 0x28;
    pub const TOUCH1_START: u8 = 0x29;
    /// 触摸槽 0 / 1 结束，value = 触摸 id
    pub const TOUCH0_END: u8 = 0x2A;
    pub const TOUCH1_END: u8 = 0x2B;
}

/// EVENT 的 dt 单位 100 µs，封顶 0xFFFF
pub const INPUT_EVENT_DT_MAX: u16 = 0xFFFF;

// ======== FEEDBACK（§5.5.4、附录 A.5） ========

/// 有效位掩码（偏移 2）；位 6、7 为 0
pub mod fb_valid {
    pub const RUMBLE: u8 = 1 << 0;
    pub const TRIGGER_L: u8 = 1 << 1;
    pub const TRIGGER_R: u8 = 1 << 2;
    pub const LED: u8 = 1 << 3;
    pub const PLAYER: u8 = 1 << 4;
    pub const INTENSITY: u8 = 1 << 5;
}
/// 扳机效果：type u8 + 10 B 参数
pub const TRIGGER_EFFECT_LEN: usize = 11;
/// 偏移 31：高 4 位触觉强度，低 4 位扳机强度
pub const FB_HAPTIC_INTENSITY_SHIFT: u8 = 4;
pub const FB_INTENSITY_MASK: u8 = 0x0F;

// ======== CONTROL / ACK（§5.5.5、附录 A.6） ========

/// 流：0 会话、1 配置、2 批量（三个流都按序交付；v1 不用流 2）
pub mod stream {
    pub const SESSION: u8 = 0;
    pub const CONFIG: u8 = 1;
    pub const BULK: u8 = 2;
    pub const COUNT: u8 = 3;
}
/// 下行每片上限
pub const CTL_FRAG_MAX_DOWN: usize = 1100;
/// 上行每片上限
pub const CTL_FRAG_MAX_UP: usize = 240;
/// 每条消息最多 16 片（v1 只发单片，frag = 0x00）
pub const CTL_MAX_FRAGS: usize = 16;
/// 发送窗口：32 条
pub const CTL_WINDOW: u16 = 32;
/// msg_seq 从 0 开始；ACK 的 cum 初值 0xFFFF（还没收到任何一条）
pub const CTL_SEQ_INIT: u16 = 0;
pub const CTL_CUM_INIT: u16 = 0xFFFF;
/// 消息体 = msg_type u8 | TLV*；0xFF 是扩展前缀，后面跟 u16 类型号
pub const MSG_EXT_PREFIX: u8 = 0xFF;

/// msg_type
pub mod msg_ty {
    /// G→C、流 0、关键
    pub const SESSION_STATUS: u8 = 0x01;
    /// G→C、流 1、关键
    pub const VIDEO_CONFIG: u8 = 0x02;
    /// G→C、流 1
    pub const AUDIO_CONFIG: u8 = 0x03;
    /// C→G、流 0、关键
    pub const IDR_REQUEST: u8 = 0x10;
    /// C→G、流 1、非关键（规格原为流 0、关键，计划 §6.2 第 20 条改）
    pub const MODE_SET: u8 = 0x11;
    /// 后
    pub const PATH_PREF: u8 = 0x12;
    /// C→G、流 0、关键
    pub const PS5_CMD: u8 = 0x13;
    /// G→C、流 0、关键（新增）
    pub const CMD_RESULT: u8 = 0x14;
    /// G→C、流 1（规格原为流 0，计划 §6.2 第 20 条改）
    pub const RATE_STATE: u8 = 0x20;
    /// G→C、流 0、关键
    pub const REBUILD_NOTICE: u8 = 0x21;
    /// G→C、流 0、关键
    pub const REBUILD_PROMPT: u8 = 0x22;
    /// C→G、流 0、关键
    pub const REBUILD_CONSENT: u8 = 0x23;
    /// v1 不发（§6.1 第 10 条）
    pub const STATS_SUMMARY: u8 = 0x30;
    /// 后
    pub const RELAY_INFO: u8 = 0x40;
    /// 双向、流 0
    pub const ERROR: u8 = 0x7F;
}

/// 各消息的 TLV 编号（附录 A.6）
pub mod msg_tlv {
    pub mod session_status {
        pub const PS_STATE: u8 = 1;
        pub const HOST_ID: u8 = 2;
        pub const EPOCH: u8 = 3;
        /// 说明 ≤ 128 B
        pub const TEXT: u8 = 4;
        pub const PAUSE_KIND: u8 = 5;
        /// 当前状态已持续 ms（u32）
        pub const STATE_MS: u8 = 6;
        pub const CAN_START: u8 = 7;
        pub const B_START_KBPS: u8 = 8;
        pub const TEXT_MAX: usize = 128;
    }
    pub mod video_config {
        pub const CFG_VER: u8 = 1;
        pub const CODEC: u8 = 2;
        pub const FPS: u8 = 3;
        /// 档位，可重复：idx u8 ‖ 宽 u16 ‖ 高 u16 ‖ 参数集（Annex-B）
        pub const PROFILE: u8 = 4;
    }
    pub mod audio_config {
        pub const CHANNELS: u8 = 1;
        /// u32
        pub const RATE: u8 = 2;
        /// u16
        pub const FRAME_SAMPLES: u8 = 3;
        /// 1 = Opus
        pub const CODEC: u8 = 4;
    }
    pub mod idr_request {
        pub const REASON: u8 = 1;
    }
    pub mod mode_set {
        pub const MODE: u8 = 1;
        /// J（ms）
        pub const J_MS: u8 = 2;
    }
    pub mod ps5_cmd {
        /// u16
        pub const REQ_ID: u8 = 1;
        pub const CMD: u8 = 2;
        /// 位 0 = 允许解除暂停
        pub const FLAGS: u8 = 3;
        /// 可选
        pub const HOST_ID: u8 = 4;
    }
    pub mod cmd_result {
        pub const REQ_ID: u8 = 1;
        pub const CODE: u8 = 2;
        pub const TEXT: u8 = 3;
    }
    pub mod rate_state {
        pub const T_CUR_KBPS: u8 = 1;
        /// ‰
        pub const R_LAN: u8 = 2;
        /// ‰
        pub const R_REP: u8 = 3;
        pub const RC_STATE: u8 = 4;
        pub const B_START_KBPS: u8 = 5;
        /// 位 0 = 上限待生效
        pub const FLAGS: u8 = 6;
    }
    /// REBUILD_NOTICE / PROMPT / CONSENT 共用
    pub mod rebuild {
        pub const REASON: u8 = 1;
        /// u32
        pub const ETA_MS: u8 = 2;
        /// u16
        pub const PROMPT_ID: u8 = 3;
        pub const ANSWER: u8 = 4;
    }
    pub mod error {
        /// u16
        pub const CODE: u8 = 1;
        pub const TEXT: u8 = 2;
    }
}

/// SESSION_STATUS 的 ps_state
pub mod ps_state {
    pub const IDLE: u8 = 0;
    pub const STANDBY: u8 = 1;
    pub const WAKING: u8 = 2;
    pub const CONNECTING: u8 = 3;
    pub const STREAMING: u8 = 4;
    pub const HOLDING: u8 = 5;
    pub const REBUILDING: u8 = 6;
    pub const BACKOFF: u8 = 7;
    pub const ERROR: u8 = 8;
    pub const PAUSED: u8 = 9;
}

/// SESSION_STATUS 的 pause_kind
pub mod pause_kind {
    pub const NONE: u8 = 0;
    pub const USER: u8 = 1;
    /// PS5 主动结束（待机、关机）
    pub const PS5_ENDED: u8 = 2;
    pub const PIN: u8 = 3;
}

/// VIDEO_CONFIG 的 codec
pub mod video_codec {
    pub const AVC: u8 = 1;
    pub const HEVC: u8 = 2;
    pub const HEVC_HDR: u8 = 3;
}

/// AUDIO_CONFIG 的 codec
pub const AUDIO_CODEC_OPUS: u8 = 1;

/// PS5_CMD 的 cmd
pub mod ps5_cmd {
    /// 含唤醒
    pub const START: u8 = 1;
    pub const STOP: u8 = 2;
    pub const STANDBY: u8 = 3;
    pub const REBUILD: u8 = 4;
    /// 选主机，留给以后
    pub const SELECT_HOST: u8 = 5;
    /// flags 位 0：允许解除暂停
    pub const FLAG_UNPAUSE: u8 = 1 << 0;
}

/// CMD_RESULT 的 code
pub mod cmd_result {
    pub const OK: u8 = 0;
    pub const TOO_FAST: u8 = 1;
    pub const NEED_CRED: u8 = 2;
    pub const NO_SESSION: u8 = 3;
    pub const BUSY: u8 = 4;
}

// ======== CLOSE（§5.5.8、附录 A.7） ========

/// CLOSE 的 code。一律发 3 份（time::CLOSE_COPIES）
pub mod close_code {
    pub const NORMAL: u16 = 0;
    pub const TAKEN_OVER: u16 = 1;
    pub const PROTOCOL_ERROR: u16 = 2;
    pub const VERSION_MISMATCH: u16 = 3;
    pub const GATEWAY_ENDED: u16 = 4;
    /// 网关进程退出（可能马上重启）
    pub const GATEWAY_EXIT: u16 = 5;
}

// ======== NEED / FRAME_STATUS / REPORT（§5.5.6~§5.5.11、附录 A.7） ========

/// NEED 的 need = 0xFF 表示不知道 k
pub const NEED_UNKNOWN_K: u8 = 0xFF;
/// NEED 的 flags：位 7 K = 同时请网关请求关键帧；位 1–0 = 尝试次数
pub const NEED_FLAG_KEY: u8 = 0x80;
pub const NEED_TRIES_MASK: u8 = 0x03;

/// FRAME_STATUS 的 status（v1 只发 GONE 和 KEY_PENDING）
pub mod frame_status {
    pub const ABORT_LAN: u8 = 1;
    pub const REF_OK: u8 = 2;
    pub const GONE: u8 = 3;
    /// fid 填「下一个要分配的 fid」
    pub const KEY_PENDING: u8 = 4;
    pub const CATCHUP: u8 = 5;
}
/// 同一条 FRAME_STATUS 在发往该客户端的连续 3 个包里各带一次
pub const FRAME_STATUS_REPEAT: u8 = 3;

/// REPORT 偏移 2：位 7–6 模式，位 2–0 路径数
pub const REPORT_MODE_SHIFT: u8 = 6;
pub const REPORT_PATHS_MASK: u8 = 0b111;
/// blocked_need == 0 表示没有阻塞，这时 blocked_fid 填 0xFFFF（只作提示）
pub const BLOCKED_FID_NONE: u16 = 0xFFFF;

// ======== IDR_REQUEST 的原因（客户端 → 网关） ========

/// IDR_REQUEST 的原因取值。规格 v0.2 §5.5.5 写明取值表「在实现交付时补写」，这里是暂定版，交付时回写规格；
/// 网关对不认识的原因照常请求 IDR，只在日志里记原值。
pub mod idr_reason {
    pub const DECODER_ERROR: u8 = 1;
    pub const FRAME_LOST: u8 = 2;
    pub const KEY_WAIT_TIMEOUT: u8 = 3;
    pub const USER: u8 = 4;
    pub const CFG_TIMEOUT: u8 = 5;
}
