//! 两个接入点：MediaMsg（PS5 会话 → 服务循环）和 PsControl（服务循环 → PS5 会话），
//! 以及服务循环交给网关监督线程的 ServeEvent。

use crate::idr::{IdrDecision, IdrReason};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;
use ywp::creds::ClientId;
use ywp::msg::{CmdResultCode, ControlMsg, Ps5CmdKind};
use ywp::time::Micros;
use ywp::wire::{InputState, SessionInfo};

/// 控制口的一条命令：命令、回复通道、截止时间。
/// 和 yw-gateway 现有的 `control::Request` 是同一个类型，D 组 M2a 时把 control.rs 改成引用这里
pub type CtlRequest = (String, Sender<String>, Instant);

/// 服务循环调 PS5 会话。网关里由 Hub（内含 PsLink）实现；fakegw 另有一份
pub trait PsControl: Send + Sync {
    /// 内部经 IdrGovernor 限频
    fn request_idr(&self, now: Micros, why: IdrReason) -> IdrDecision;
    fn set_controller_full(&self, c: &InputState);
    fn set_controller_idle(&self);
    /// 参数集最新值槽
    fn params(&self, epoch: u32) -> Option<Arc<[u8]>>;
    /// 音频头最新值槽
    fn audio_header(&self, epoch: u32) -> Option<AudioHeader>;
}

/// 音频头（同现有外壳的 on_audio_header）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioHeader {
    pub channels: u32,
    pub bits: u32,
    pub rate: u32,
    pub frame_size: u32,
}

/// chiaki 的反馈事件（外壳 on_feedback 的内容）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FbEvent {
    /// YW_FB_*（外壳的枚举：1 RUMBLE、2 TRIGGER_EFFECTS、3 LED、4 PLAYER_INDEX、
    /// 5 HAPTIC_INTENSITY、6 TRIGGER_INTENSITY、7 MOTION_RESET）
    pub kind: u8,
    pub len: u8,
    /// 扳机效果 22 B、LED 3 B、其余 1 B
    pub data: [u8; 22],
}

/// 重建原因（M2d）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RebuildReason {
    /// 塌陷检测，用户同意
    Collapse,
    /// 客户端发了 PS5_CMD REBUILD
    UserRequest,
    /// 码率上限变了
    BitrateCap,
    /// PS5 会话出错
    SessionError,
}

/// PS5 会话 → 服务循环。chiaki 回调里只拷贝、try_send、wake，绝不阻塞
#[derive(Debug)]
pub enum MediaMsg {
    /// seq 递增，ywp 线程看到跳号就知道队列丢过帧
    Video { seq: u32, epoch: u32, data: Vec<u8>, frames_lost: i32, recovered: bool, rx_us: Micros },
    /// 只是通知，内容从 PsControl::params 取
    ParamsChanged { epoch: u32 },
    AudioHeaderChanged { epoch: u32 },
    /// data 为 None：chiaki 已判丢
    Audio { epoch: u32, data: Option<Vec<u8>>, rx_us: Micros },
    Feedback { epoch: u32, fb: FbEvent },
    PsChanged(SessionInfo),
    Rebuilding { reason: RebuildReason, eta_ms: u32 },
    /// 网关要发给客户端的 RATE_STATE、REBUILD_PROMPT 等
    SendControl(ControlMsg),
    /// 控制口的 YWP 类命令：clients、kick、clients-reload
    Control(CtlRequest),
    Shutdown { ack: Sender<()> },
}

/// 客户端为什么离开
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DetachWhy {
    /// 收到 CLOSE(code)
    Closed(u16),
    /// 60 s 没有任何包
    Timeout,
    /// 被另一台客户端接管
    Replaced,
    /// 控制口 kick
    Kicked,
    /// 配对被删除
    Unpaired,
}

/// 服务循环 → 网关的监督线程（M2d）
#[derive(Debug)]
pub enum ServeEvent {
    ClientAttached { client: ClientId },
    ClientDetached { client: ClientId, why: DetachWhy },
    Ps5Cmd { cmd: Ps5CmdKind, reply: Sender<CmdResultCode> },
    RebuildConsent { prompt_id: u16, yes: bool },
    BitrateCap(Option<u32>),
}
