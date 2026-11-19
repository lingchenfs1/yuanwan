//! CONTROL 消息（B 组，附录 A.6）：消息体 = msg_type u8 | TLV*，TLV 格式和握手相同；
//! 0xFF 是扩展前缀，后面跟 u16 类型号。各 TLV 编号见 consts::msg_tlv。
//!
//! M2a 用到的负载（SessionStatus、VideoConfig）和 AudioConfig 字段已定；
//! M2d 才用的（Ps5Cmd、CmdResult、RateState、Rebuild*）标了 `#[non_exhaustive]`，只由 B 组加字段。

use crate::consts::{msg_ty, stream};
use crate::error::WireError;

/// SESSION_STATUS（0x01，G→C、流 0、关键）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionStatus {
    /// TLV 1，consts::ps_state
    pub ps_state: u8,
    /// TLV 2
    pub host_id: Option<[u8; 8]>,
    /// TLV 3
    pub epoch: Option<u32>,
    /// TLV 4，≤ 128 B
    pub text: Option<String>,
    /// TLV 5，consts::pause_kind
    pub pause_kind: Option<u8>,
    /// TLV 6，当前状态已持续 ms
    pub state_ms: Option<u32>,
    /// TLV 7
    pub can_start: Option<bool>,
    /// TLV 8
    pub b_start_kbps: Option<u32>,
}

/// VIDEO_CONFIG 的一个档位（TLV 4，可重复）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoProfile {
    pub idx: u8,
    pub width: u16,
    pub height: u16,
    /// 参数集（Annex-B）
    pub param_sets: Vec<u8>,
}

/// VIDEO_CONFIG（0x02，G→C、流 1、关键）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoConfig {
    /// TLV 1；帧的 cfg 是它的低 2 位
    pub cfg_ver: u8,
    /// TLV 2，consts::video_codec
    pub codec: u8,
    /// TLV 3（附录 A.6 没写宽度，先定 1 B）
    pub fps: u8,
    /// TLV 4
    pub profiles: Vec<VideoProfile>,
}

/// AUDIO_CONFIG（0x03，G→C、流 1）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioConfig {
    /// TLV 1
    pub channels: u8,
    /// TLV 2
    pub rate: u32,
    /// TLV 3，每帧采样数
    pub frame_samples: u16,
    /// TLV 4，1 = Opus
    pub codec: u8,
}

/// PS5_CMD 的 cmd
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
#[non_exhaustive]
pub enum Ps5CmdKind {
    /// 含唤醒
    Start = 1,
    Stop = 2,
    Standby = 3,
    Rebuild = 4,
}

impl Ps5CmdKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Start),
            2 => Some(Self::Stop),
            3 => Some(Self::Standby),
            4 => Some(Self::Rebuild),
            _ => None,
        }
    }
}

/// CMD_RESULT 的 code
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum CmdResultCode {
    Ok = 0,
    TooFast = 1,
    NeedCred = 2,
    NoSession = 3,
    Busy = 4,
}

/// PS5_CMD（0x13，C→G、流 0、关键）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Ps5Cmd {
    pub req_id: u16,
    /// 原值（不认识的也保留），用 Ps5CmdKind::from_u8 转
    pub cmd: u8,
    /// 位 0 = 允许解除暂停
    pub flags: u8,
    pub host_id: Option<[u8; 8]>,
}

/// CMD_RESULT（0x14，G→C、流 0、关键，新增）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CmdResult {
    pub req_id: u16,
    pub code: u8,
    pub text: Option<String>,
}

/// RATE_STATE（0x20，G→C、流 1）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RateState {
    pub t_cur_kbps: u32,
    pub r_lan_permille: u16,
    pub r_rep_permille: u16,
    pub rc_state: u8,
    pub b_start_kbps: u32,
    /// 位 0 = 上限待生效
    pub flags: u8,
}

/// REBUILD_NOTICE（0x21，G→C、流 0、关键）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RebuildNotice {
    pub reason: u8,
    pub eta_ms: u32,
}

/// REBUILD_PROMPT（0x22，G→C、流 0、关键）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RebuildPrompt {
    pub reason: u8,
    pub eta_ms: u32,
    pub prompt_id: u16,
}

/// REBUILD_CONSENT（0x23，C→G、流 0、关键）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RebuildConsent {
    pub prompt_id: u16,
    pub yes: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlMsg {
    SessionStatus(SessionStatus),
    VideoConfig(VideoConfig),
    AudioConfig(AudioConfig),
    /// reason 见 consts::idr_reason
    IdrRequest { reason: u8 },
    ModeSet { mode: u8, j_ms: u8 },
    Ps5Cmd(Ps5Cmd),
    CmdResult(CmdResult),
    RateState(RateState),
    RebuildNotice(RebuildNotice),
    RebuildPrompt(RebuildPrompt),
    RebuildConsent(RebuildConsent),
    Error { code: u16, text: String },
    /// 不认识的 msg_type（计 DropReason::CtlUnknownMsg）
    Unknown { ty: u8 },
}

impl ControlMsg {
    pub fn encode(&self) -> Vec<u8> {
        todo!()
    }

    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        todo!()
    }

    pub fn msg_type(&self) -> u8 {
        match self {
            ControlMsg::SessionStatus(_) => msg_ty::SESSION_STATUS,
            ControlMsg::VideoConfig(_) => msg_ty::VIDEO_CONFIG,
            ControlMsg::AudioConfig(_) => msg_ty::AUDIO_CONFIG,
            ControlMsg::IdrRequest { .. } => msg_ty::IDR_REQUEST,
            ControlMsg::ModeSet { .. } => msg_ty::MODE_SET,
            ControlMsg::Ps5Cmd(_) => msg_ty::PS5_CMD,
            ControlMsg::CmdResult(_) => msg_ty::CMD_RESULT,
            ControlMsg::RateState(_) => msg_ty::RATE_STATE,
            ControlMsg::RebuildNotice(_) => msg_ty::REBUILD_NOTICE,
            ControlMsg::RebuildPrompt(_) => msg_ty::REBUILD_PROMPT,
            ControlMsg::RebuildConsent(_) => msg_ty::REBUILD_CONSENT,
            ControlMsg::Error { .. } => msg_ty::ERROR,
            ControlMsg::Unknown { ty } => *ty,
        }
    }

    /// 走哪个流（附录 A.6；MODE_SET、RATE_STATE 按计划 §6.2 第 20 条改到流 1）
    pub fn stream(&self) -> u8 {
        match self {
            ControlMsg::VideoConfig(_)
            | ControlMsg::AudioConfig(_)
            | ControlMsg::ModeSet { .. }
            | ControlMsg::RateState(_) => stream::CONFIG,
            _ => stream::SESSION,
        }
    }

    /// 关键消息在 +5 ms 再发一份
    pub fn critical(&self) -> bool {
        matches!(
            self,
            ControlMsg::SessionStatus(_)
                | ControlMsg::VideoConfig(_)
                | ControlMsg::IdrRequest { .. }
                | ControlMsg::Ps5Cmd(_)
                | ControlMsg::CmdResult(_)
                | ControlMsg::RebuildNotice(_)
                | ControlMsg::RebuildPrompt(_)
                | ControlMsg::RebuildConsent(_)
        )
    }
}
