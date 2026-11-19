//! CONTROL 消息（B 组，规格 §5.5.5）：消息体 = msg_type u8 | TLV*，TLV 格式和握手相同（规格 §4.4）；
//! 0xFF 是扩展前缀，后面跟 u16 类型号。各 TLV 编号见 consts::msg_tlv。
//!
//! M2a 用到的负载（SessionStatus、VideoConfig）和 AudioConfig 字段已定；
//! M2d 才用的（Ps5Cmd、CmdResult、RateState、Rebuild*）标了 `#[non_exhaustive]`，只由 B 组加字段。
//!
//! 编解码规则（TLV 部分和规格 §4.4、§5.5.5 一致，其余是本实现的约定，交付时回写规格）：
//! - type 0x00 表示填充，遇到就停止解析；不认识的 type 按 len 跳过；
//!   同一个 type 出现两次按格式错误处理，VIDEO_CONFIG 的 TLV 4（档位）可重复；
//! - 定长字段的 len 比定义短算格式错误；比定义长只读定义的部分（给以后加字段留余地，同规格 §5.3 对块的规则）；
//! - 必带项：结构体里不是 Option 的字段都必带，缺了算格式错误。例外（缺了取 0 或空）：
//!   ERROR 的文本、MODE_SET 的 J、PS5_CMD 的 flags、RATE_STATE 的 flags、REBUILD_NOTICE / PROMPT 的预计时长；
//! - 文本按 UTF-8 解码，非法字节替换成 U+FFFD，不报错；SESSION_STATUS 的说明编码时按字符边界截到 128 B；
//! - 布尔值编码成 0 / 1，解码时非 0 即真；
//! - 编码时 Option 为 None 的字段不写；
//! - 不认识的 msg_type（含 0xFF 扩展）解成 [`ControlMsg::Unknown`]，不报错。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::consts::msg_tlv::{
    audio_config as ac, cmd_result as cr, error as er, idr_request as ir, mode_set as ms, ps5_cmd as pc,
    rate_state as rs, rebuild as rb, session_status as ss, video_config as vc,
};
use crate::consts::{msg_ty, stream, MSG_EXT_PREFIX, TLV_HDR_LEN};
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
    /// TLV 3，u8（规格 §5.5.5）
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
    /// 编码成消息体（msg_type | TLV*）。长度由调用方检查（v1 单片：下行 ≤ 1100 B，上行 ≤ 240 B）
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Enc::new(self.msg_type());
        match self {
            ControlMsg::SessionStatus(m) => {
                e.u8(ss::PS_STATE, m.ps_state);
                if let Some(v) = &m.host_id {
                    e.bytes(ss::HOST_ID, v);
                }
                e.opt_u32(ss::EPOCH, m.epoch);
                if let Some(t) = &m.text {
                    e.bytes(ss::TEXT, truncate_utf8(t, ss::TEXT_MAX));
                }
                e.opt_u8(ss::PAUSE_KIND, m.pause_kind);
                e.opt_u32(ss::STATE_MS, m.state_ms);
                e.opt_u8(ss::CAN_START, m.can_start.map(u8::from));
                e.opt_u32(ss::B_START_KBPS, m.b_start_kbps);
            }
            ControlMsg::VideoConfig(m) => {
                e.u8(vc::CFG_VER, m.cfg_ver);
                e.u8(vc::CODEC, m.codec);
                e.u8(vc::FPS, m.fps);
                for p in &m.profiles {
                    let mut v = Vec::with_capacity(5 + p.param_sets.len());
                    v.push(p.idx);
                    v.extend_from_slice(&p.width.to_be_bytes());
                    v.extend_from_slice(&p.height.to_be_bytes());
                    v.extend_from_slice(&p.param_sets);
                    e.bytes(vc::PROFILE, &v);
                }
            }
            ControlMsg::AudioConfig(m) => {
                e.u8(ac::CHANNELS, m.channels);
                e.u32(ac::RATE, m.rate);
                e.u16(ac::FRAME_SAMPLES, m.frame_samples);
                e.u8(ac::CODEC, m.codec);
            }
            ControlMsg::IdrRequest { reason } => e.u8(ir::REASON, *reason),
            ControlMsg::ModeSet { mode, j_ms } => {
                e.u8(ms::MODE, *mode);
                e.u8(ms::J_MS, *j_ms);
            }
            ControlMsg::Ps5Cmd(m) => {
                e.u16(pc::REQ_ID, m.req_id);
                e.u8(pc::CMD, m.cmd);
                e.u8(pc::FLAGS, m.flags);
                if let Some(v) = &m.host_id {
                    e.bytes(pc::HOST_ID, v);
                }
            }
            ControlMsg::CmdResult(m) => {
                e.u16(cr::REQ_ID, m.req_id);
                e.u8(cr::CODE, m.code);
                if let Some(t) = &m.text {
                    e.bytes(cr::TEXT, t.as_bytes());
                }
            }
            ControlMsg::RateState(m) => {
                e.u32(rs::T_CUR_KBPS, m.t_cur_kbps);
                e.u16(rs::R_LAN, m.r_lan_permille);
                e.u16(rs::R_REP, m.r_rep_permille);
                e.u8(rs::RC_STATE, m.rc_state);
                e.u32(rs::B_START_KBPS, m.b_start_kbps);
                e.u8(rs::FLAGS, m.flags);
            }
            ControlMsg::RebuildNotice(m) => {
                e.u8(rb::REASON, m.reason);
                e.u32(rb::ETA_MS, m.eta_ms);
            }
            ControlMsg::RebuildPrompt(m) => {
                e.u8(rb::REASON, m.reason);
                e.u32(rb::ETA_MS, m.eta_ms);
                e.u16(rb::PROMPT_ID, m.prompt_id);
            }
            ControlMsg::RebuildConsent(m) => {
                e.u16(rb::PROMPT_ID, m.prompt_id);
                e.u8(rb::ANSWER, u8::from(m.yes));
            }
            ControlMsg::Error { code, text } => {
                e.u16(er::CODE, *code);
                if !text.is_empty() {
                    e.bytes(er::TEXT, text.as_bytes());
                }
            }
            ControlMsg::Unknown { ty } => {
                if *ty == MSG_EXT_PREFIX {
                    // 扩展前缀后面必须跟 u16 类型号
                    e.raw(&[0, 0]);
                }
            }
        }
        e.finish()
    }

    /// 从消息体解码。不认识的 msg_type 返回 Unknown（不报错）
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let (&ty, body) = b.split_first().ok_or(WireError::Short)?;
        let known = matches!(
            ty,
            msg_ty::SESSION_STATUS
                | msg_ty::VIDEO_CONFIG
                | msg_ty::AUDIO_CONFIG
                | msg_ty::IDR_REQUEST
                | msg_ty::MODE_SET
                | msg_ty::PS5_CMD
                | msg_ty::CMD_RESULT
                | msg_ty::RATE_STATE
                | msg_ty::REBUILD_NOTICE
                | msg_ty::REBUILD_PROMPT
                | msg_ty::REBUILD_CONSENT
                | msg_ty::ERROR
        );
        if !known {
            if ty == MSG_EXT_PREFIX && body.len() < 2 {
                return Err(WireError::Truncated);
            }
            return Ok(ControlMsg::Unknown { ty });
        }
        let repeatable: &[u8] = if ty == msg_ty::VIDEO_CONFIG { &[vc::PROFILE] } else { &[] };
        let f = Fields::parse(body, repeatable)?;
        let m = match ty {
            msg_ty::SESSION_STATUS => ControlMsg::SessionStatus(SessionStatus {
                ps_state: f.req_u8(ss::PS_STATE)?,
                host_id: f.arr8(ss::HOST_ID)?,
                epoch: f.u32(ss::EPOCH)?,
                text: f.text(ss::TEXT),
                pause_kind: f.u8(ss::PAUSE_KIND)?,
                state_ms: f.u32(ss::STATE_MS)?,
                can_start: f.u8(ss::CAN_START)?.map(|v| v != 0),
                b_start_kbps: f.u32(ss::B_START_KBPS)?,
            }),
            msg_ty::VIDEO_CONFIG => {
                let mut profiles = Vec::new();
                for v in f.all(vc::PROFILE) {
                    if v.len() < 5 {
                        return Err(WireError::Malformed("VIDEO_CONFIG 档位太短"));
                    }
                    profiles.push(VideoProfile {
                        idx: v[0],
                        width: u16::from_be_bytes([v[1], v[2]]),
                        height: u16::from_be_bytes([v[3], v[4]]),
                        param_sets: v[5..].to_vec(),
                    });
                }
                ControlMsg::VideoConfig(VideoConfig {
                    cfg_ver: f.req_u8(vc::CFG_VER)?,
                    codec: f.req_u8(vc::CODEC)?,
                    fps: f.req_u8(vc::FPS)?,
                    profiles,
                })
            }
            msg_ty::AUDIO_CONFIG => ControlMsg::AudioConfig(AudioConfig {
                channels: f.req_u8(ac::CHANNELS)?,
                rate: f.req_u32(ac::RATE)?,
                frame_samples: f.req_u16(ac::FRAME_SAMPLES)?,
                codec: f.req_u8(ac::CODEC)?,
            }),
            msg_ty::IDR_REQUEST => ControlMsg::IdrRequest { reason: f.req_u8(ir::REASON)? },
            msg_ty::MODE_SET => {
                ControlMsg::ModeSet { mode: f.req_u8(ms::MODE)?, j_ms: f.u8(ms::J_MS)?.unwrap_or(0) }
            }
            msg_ty::PS5_CMD => ControlMsg::Ps5Cmd(Ps5Cmd {
                req_id: f.req_u16(pc::REQ_ID)?,
                cmd: f.req_u8(pc::CMD)?,
                flags: f.u8(pc::FLAGS)?.unwrap_or(0),
                host_id: f.arr8(pc::HOST_ID)?,
            }),
            msg_ty::CMD_RESULT => ControlMsg::CmdResult(CmdResult {
                req_id: f.req_u16(cr::REQ_ID)?,
                code: f.req_u8(cr::CODE)?,
                text: f.text(cr::TEXT),
            }),
            msg_ty::RATE_STATE => ControlMsg::RateState(RateState {
                t_cur_kbps: f.req_u32(rs::T_CUR_KBPS)?,
                r_lan_permille: f.req_u16(rs::R_LAN)?,
                r_rep_permille: f.req_u16(rs::R_REP)?,
                rc_state: f.req_u8(rs::RC_STATE)?,
                b_start_kbps: f.req_u32(rs::B_START_KBPS)?,
                flags: f.u8(rs::FLAGS)?.unwrap_or(0),
            }),
            msg_ty::REBUILD_NOTICE => ControlMsg::RebuildNotice(RebuildNotice {
                reason: f.req_u8(rb::REASON)?,
                eta_ms: f.u32(rb::ETA_MS)?.unwrap_or(0),
            }),
            msg_ty::REBUILD_PROMPT => ControlMsg::RebuildPrompt(RebuildPrompt {
                reason: f.req_u8(rb::REASON)?,
                eta_ms: f.u32(rb::ETA_MS)?.unwrap_or(0),
                prompt_id: f.req_u16(rb::PROMPT_ID)?,
            }),
            msg_ty::REBUILD_CONSENT => ControlMsg::RebuildConsent(RebuildConsent {
                prompt_id: f.req_u16(rb::PROMPT_ID)?,
                yes: f.req_u8(rb::ANSWER)? != 0,
            }),
            msg_ty::ERROR => {
                ControlMsg::Error { code: f.req_u16(er::CODE)?, text: f.text(er::TEXT).unwrap_or_default() }
            }
            _ => ControlMsg::Unknown { ty },
        };
        Ok(m)
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

    /// 走哪个流（规格 v0.2 §5.5.5：MODE_SET、RATE_STATE 走流 1）
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

/// 按字符边界截到 max 字节
fn truncate_utf8(s: &str, max: usize) -> &[u8] {
    if s.len() <= max {
        return s.as_bytes();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s.as_bytes()[..end]
}

/// 消息体编码器：msg_type 之后逐个写 TLV
struct Enc(Vec<u8>);

impl Enc {
    fn new(ty: u8) -> Self {
        Self(vec![ty])
    }

    fn raw(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }

    /// TLV 的 len 是 u16；超长的值（实际不会出现：单片上限 1100 B）截到 65535 B
    fn bytes(&mut self, ty: u8, v: &[u8]) {
        let v = &v[..v.len().min(u16::MAX as usize)];
        self.0.push(ty);
        self.0.extend_from_slice(&(v.len() as u16).to_be_bytes());
        self.0.extend_from_slice(v);
    }

    fn u8(&mut self, ty: u8, v: u8) {
        self.bytes(ty, &[v]);
    }

    fn u16(&mut self, ty: u8, v: u16) {
        self.bytes(ty, &v.to_be_bytes());
    }

    fn u32(&mut self, ty: u8, v: u32) {
        self.bytes(ty, &v.to_be_bytes());
    }

    fn opt_u8(&mut self, ty: u8, v: Option<u8>) {
        if let Some(v) = v {
            self.u8(ty, v);
        }
    }

    fn opt_u32(&mut self, ty: u8, v: Option<u32>) {
        if let Some(v) = v {
            self.u32(ty, v);
        }
    }

    fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// 解析好的 TLV 列表（按出现顺序）
struct Fields<'a> {
    items: Vec<(u8, &'a [u8])>,
}

impl<'a> Fields<'a> {
    /// repeatable：允许重复出现的 type
    fn parse(mut b: &'a [u8], repeatable: &[u8]) -> Result<Self, WireError> {
        let mut seen = [false; 256];
        let mut items = Vec::new();
        while let Some(&ty) = b.first() {
            if ty == 0x00 {
                // 填充：停止解析
                break;
            }
            if b.len() < TLV_HDR_LEN {
                return Err(WireError::Truncated);
            }
            let len = u16::from_be_bytes([b[1], b[2]]) as usize;
            let end = TLV_HDR_LEN + len;
            if b.len() < end {
                return Err(WireError::Truncated);
            }
            if seen[ty as usize] && !repeatable.contains(&ty) {
                return Err(WireError::Malformed("TLV 重复"));
            }
            seen[ty as usize] = true;
            items.push((ty, &b[TLV_HDR_LEN..end]));
            b = &b[end..];
        }
        Ok(Self { items })
    }

    fn get(&self, ty: u8) -> Option<&'a [u8]> {
        self.items.iter().find(|(t, _)| *t == ty).map(|(_, v)| *v)
    }

    fn all(&self, ty: u8) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.items.iter().filter(move |(t, _)| *t == ty).map(|(_, v)| *v)
    }

    /// 定长字段：短了算格式错误，长了只读前 N 字节
    fn fixed<const N: usize>(&self, ty: u8) -> Result<Option<[u8; N]>, WireError> {
        match self.get(ty) {
            None => Ok(None),
            Some(v) if v.len() < N => Err(WireError::Malformed("TLV 太短")),
            Some(v) => {
                let mut a = [0u8; N];
                a.copy_from_slice(&v[..N]);
                Ok(Some(a))
            }
        }
    }

    fn u8(&self, ty: u8) -> Result<Option<u8>, WireError> {
        Ok(self.fixed::<1>(ty)?.map(|a| a[0]))
    }

    fn u16(&self, ty: u8) -> Result<Option<u16>, WireError> {
        Ok(self.fixed::<2>(ty)?.map(u16::from_be_bytes))
    }

    fn u32(&self, ty: u8) -> Result<Option<u32>, WireError> {
        Ok(self.fixed::<4>(ty)?.map(u32::from_be_bytes))
    }

    fn arr8(&self, ty: u8) -> Result<Option<[u8; 8]>, WireError> {
        self.fixed::<8>(ty)
    }

    fn req_u8(&self, ty: u8) -> Result<u8, WireError> {
        self.u8(ty)?.ok_or(WireError::Malformed("缺必带的 TLV"))
    }

    fn req_u16(&self, ty: u8) -> Result<u16, WireError> {
        self.u16(ty)?.ok_or(WireError::Malformed("缺必带的 TLV"))
    }

    fn req_u32(&self, ty: u8) -> Result<u32, WireError> {
        self.u32(ty)?.ok_or(WireError::Malformed("缺必带的 TLV"))
    }

    fn text(&self, ty: u8) -> Option<String> {
        self.get(ty).map(|v| String::from_utf8_lossy(v).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{ps_state, video_codec};

    fn roundtrip(m: ControlMsg) {
        let b = m.encode();
        assert_eq!(b[0], m.msg_type());
        assert_eq!(ControlMsg::decode(&b).unwrap(), m, "{b:02x?}");
    }

    #[test]
    fn all_messages_roundtrip() {
        roundtrip(ControlMsg::SessionStatus(SessionStatus { ps_state: ps_state::STREAMING, ..Default::default() }));
        roundtrip(ControlMsg::SessionStatus(SessionStatus {
            ps_state: ps_state::PAUSED,
            host_id: Some([1, 2, 3, 4, 5, 6, 7, 8]),
            epoch: Some(0xDEAD_0001),
            text: Some("暂停中".into()),
            pause_kind: Some(1),
            state_ms: Some(12_345),
            can_start: Some(true),
            b_start_kbps: Some(15_000),
        }));
        roundtrip(ControlMsg::VideoConfig(VideoConfig {
            cfg_ver: 3,
            codec: video_codec::HEVC,
            fps: 60,
            profiles: vec![
                VideoProfile { idx: 0, width: 1920, height: 1080, param_sets: vec![0, 0, 0, 1, 0x40, 1, 0x0C] },
                VideoProfile { idx: 1, width: 1280, height: 720, param_sets: vec![] },
            ],
        }));
        roundtrip(ControlMsg::AudioConfig(AudioConfig { channels: 2, rate: 48_000, frame_samples: 480, codec: 1 }));
        roundtrip(ControlMsg::IdrRequest { reason: 2 });
        roundtrip(ControlMsg::ModeSet { mode: 1, j_ms: 17 });
        let mut cmd = Ps5Cmd { req_id: 0xBEEF, cmd: 1, flags: 1, host_id: None };
        roundtrip(ControlMsg::Ps5Cmd(cmd));
        cmd.host_id = Some([9; 8]);
        roundtrip(ControlMsg::Ps5Cmd(cmd));
        roundtrip(ControlMsg::CmdResult(CmdResult { req_id: 7, code: 4, text: Some("busy".into()) }));
        roundtrip(ControlMsg::CmdResult(CmdResult { req_id: 7, code: 0, text: None }));
        roundtrip(ControlMsg::RateState(RateState {
            t_cur_kbps: 15_000,
            r_lan_permille: 36,
            r_rep_permille: 30,
            rc_state: 2,
            b_start_kbps: 20_000,
            flags: 1,
        }));
        roundtrip(ControlMsg::RebuildNotice(RebuildNotice { reason: 1, eta_ms: 5_000 }));
        roundtrip(ControlMsg::RebuildPrompt(RebuildPrompt { reason: 2, eta_ms: 8_000, prompt_id: 3 }));
        roundtrip(ControlMsg::RebuildConsent(RebuildConsent { prompt_id: 3, yes: true }));
        roundtrip(ControlMsg::Error { code: 0x0102, text: "坏了".into() });
        roundtrip(ControlMsg::Error { code: 1, text: String::new() });
    }

    #[test]
    fn exact_bytes() {
        assert_eq!(ControlMsg::IdrRequest { reason: 5 }.encode(), vec![0x10, 1, 0, 1, 5]);
        let m = ControlMsg::SessionStatus(SessionStatus { ps_state: 4, epoch: Some(0x0102_0304), ..Default::default() });
        assert_eq!(m.encode(), vec![0x01, 1, 0, 1, 4, 3, 0, 4, 1, 2, 3, 4]);
        let m = ControlMsg::ModeSet { mode: 0, j_ms: 0 };
        assert_eq!(m.encode(), vec![0x11, 1, 0, 1, 0, 2, 0, 1, 0]);
        let m = ControlMsg::VideoConfig(VideoConfig {
            cfg_ver: 1,
            codec: 2,
            fps: 60,
            profiles: vec![VideoProfile { idx: 0, width: 0x0780, height: 0x0438, param_sets: vec![0xAB] }],
        });
        assert_eq!(
            m.encode(),
            vec![0x02, 1, 0, 1, 1, 2, 0, 1, 2, 3, 0, 1, 60, 4, 0, 6, 0, 0x07, 0x80, 0x04, 0x38, 0xAB]
        );
    }

    #[test]
    fn stream_and_critical_follow_spec_table() {
        assert_eq!(ControlMsg::SessionStatus(Default::default()).stream(), stream::SESSION);
        assert!(ControlMsg::SessionStatus(Default::default()).critical());
        assert_eq!(ControlMsg::VideoConfig(Default::default()).stream(), stream::CONFIG);
        assert!(ControlMsg::VideoConfig(Default::default()).critical());
        assert_eq!(ControlMsg::AudioConfig(Default::default()).stream(), stream::CONFIG);
        assert!(!ControlMsg::AudioConfig(Default::default()).critical());
        assert_eq!(ControlMsg::ModeSet { mode: 0, j_ms: 0 }.stream(), stream::CONFIG);
        assert!(!ControlMsg::ModeSet { mode: 0, j_ms: 0 }.critical());
        assert_eq!(ControlMsg::RateState(Default::default()).stream(), stream::CONFIG);
        assert!(ControlMsg::IdrRequest { reason: 0 }.critical());
        assert_eq!(ControlMsg::Error { code: 0, text: String::new() }.stream(), stream::SESSION);
        assert!(!ControlMsg::Error { code: 0, text: String::new() }.critical());
    }

    #[test]
    fn tlv_rules() {
        // 不认识的 TLV 跳过；填充 0x00 之后的内容不再解析
        let b = [0x10, 0x09, 0, 2, 0xAA, 0xBB, 1, 0, 1, 3, 0x00, 0xFF, 0xFF];
        assert_eq!(ControlMsg::decode(&b).unwrap(), ControlMsg::IdrRequest { reason: 3 });
        // 重复的 TLV 算格式错误
        let b = [0x10, 1, 0, 1, 3, 1, 0, 1, 4];
        assert!(matches!(ControlMsg::decode(&b), Err(WireError::Malformed(_))));
        // 长了只读定义的部分
        let b = [0x10, 1, 0, 2, 7, 0xEE];
        assert_eq!(ControlMsg::decode(&b).unwrap(), ControlMsg::IdrRequest { reason: 7 });
        // 短了算格式错误（CMD_RESULT 的 req_id 是 u16）
        let b = [0x14, 1, 0, 1, 7, 2, 0, 1, 0];
        assert!(matches!(ControlMsg::decode(&b), Err(WireError::Malformed(_))));
        // 越界
        assert_eq!(ControlMsg::decode(&[0x10, 1, 0, 5, 7]), Err(WireError::Truncated));
        assert_eq!(ControlMsg::decode(&[0x10, 1, 0]), Err(WireError::Truncated));
        // 缺必带项
        assert!(matches!(ControlMsg::decode(&[0x01]), Err(WireError::Malformed(_))));
        assert!(matches!(ControlMsg::decode(&[0x02, 1, 0, 1, 1]), Err(WireError::Malformed(_))));
        // 档位太短
        let b = [0x02, 1, 0, 1, 1, 2, 0, 1, 2, 3, 0, 1, 60, 4, 0, 4, 0, 0, 0, 0];
        assert!(matches!(ControlMsg::decode(&b), Err(WireError::Malformed(_))));
        // 空消息体
        assert_eq!(ControlMsg::decode(&[]), Err(WireError::Short));
        // 可选项缺了取默认值
        assert_eq!(
            ControlMsg::decode(&[0x7F, 1, 0, 2, 0, 9]).unwrap(),
            ControlMsg::Error { code: 9, text: String::new() }
        );
        assert_eq!(ControlMsg::decode(&[0x11, 1, 0, 1, 2]).unwrap(), ControlMsg::ModeSet { mode: 2, j_ms: 0 });
    }

    #[test]
    fn unknown_and_extended_types() {
        assert_eq!(ControlMsg::decode(&[0x30, 9, 0, 0]).unwrap(), ControlMsg::Unknown { ty: 0x30 });
        assert_eq!(ControlMsg::decode(&[0x12]).unwrap(), ControlMsg::Unknown { ty: 0x12 });
        assert_eq!(ControlMsg::decode(&[0xFF, 0x01, 0x00, 1, 0, 0]).unwrap(), ControlMsg::Unknown { ty: 0xFF });
        assert_eq!(ControlMsg::decode(&[0xFF, 0x01]), Err(WireError::Truncated));
        assert_eq!(ControlMsg::Unknown { ty: 0xFF }.encode(), vec![0xFF, 0, 0]);
        assert_eq!(ControlMsg::Unknown { ty: 0x30 }.encode(), vec![0x30]);
    }

    #[test]
    fn text_is_truncated_and_lossy() {
        let long = "长".repeat(100); // 300 B
        let m = ControlMsg::SessionStatus(SessionStatus { text: Some(long), ..Default::default() });
        let ControlMsg::SessionStatus(back) = ControlMsg::decode(&m.encode()).unwrap() else { panic!("类型不对") };
        let t = back.text.unwrap();
        assert_eq!(t.len(), 126);
        assert!(t.chars().all(|c| c == '长'));
        // 非法 UTF-8 不报错
        let b = [0x7F, 1, 0, 2, 0, 1, 2, 0, 2, 0xFF, b'a'];
        let ControlMsg::Error { text, .. } = ControlMsg::decode(&b).unwrap() else { panic!("类型不对") };
        assert_eq!(text, "\u{FFFD}a");
    }

    #[test]
    fn decode_never_panics_on_garbage() {
        let mut rng = crate::testkit::Rng::new(1);
        const TYPES: [u8; 13] = [0x01, 0x02, 0x03, 0x10, 0x11, 0x13, 0x14, 0x20, 0x21, 0x22, 0x23, 0x7F, 0xFF];
        for _ in 0..20_000 {
            let n = rng.below(40) as usize;
            let mut b: Vec<u8> = (0..n).map(|_| rng.next_u64() as u8).collect();
            if let Some(first) = b.first_mut() {
                // 多数落在已知的 msg_type 上
                *first = TYPES[rng.below(TYPES.len() as u64) as usize];
            }
            let _ = ControlMsg::decode(&b);
        }
    }
}
