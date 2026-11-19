//! 握手载荷的 TLV（附录 A.3）：type u8 | len u16 | value。
//! 0x00 表示填充，遇到就停止解析；不认识的 type 按 len 跳过；同一个 type 出现两次按格式错误处理。
//! 包的整体布局（e、加密的 s、mac1、mac2）见附录 A.1，由 crypto 和 endpoint 组装。

use super::Writer;
use crate::crypto::Tai64N;
use crate::error::WireError;

/// HS_INIT 的加密载荷
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HsInitPayload {
    /// 0x01，必带
    pub ts: Tai64N,
    /// 0x02，必带
    pub ver: u8,
    pub min_ver: u8,
    /// 0x03，必带（位 0 必须为 1）
    pub caps: u32,
    /// 0x04：(host_id, session_epoch)
    pub resume: Option<([u8; 8], u32)>,
    /// 0x05：(fid, cfg_ver)
    pub last_fid: Option<(u16, u8)>,
    /// 0x06：(mode, flags)，flags 位 0 = 接管
    pub mode: Option<(u8, u8)>,
    /// 0x07：≤ 32 B UTF-8
    pub name: Option<String>,
    /// 0x08：kbps，0 表示不限
    pub bitrate_cap_kbps: Option<u32>,
}

impl HsInitPayload {
    /// 写 TLV（不含补齐；补齐和加密由调用方做）
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }

    /// 解 TLV；缺必带项、type 重复、长度不对都返回错误
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        todo!()
    }
}

/// HS_RESP 的 err（TLV 0x10）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum HsErr {
    Ok = 0,
    CidCollision = 1,
    Busy = 2,
    Version = 3,
}

/// HS_RESP 的加密载荷
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HsRespPayload {
    pub ver: u8,
    pub min_ver: u8,
    /// 双方的交集
    pub caps: u32,
    pub err: HsErr,
    /// err = Ok 时必带
    pub reset_token: Option<[u8; 16]>,
    /// TLV 0x12
    pub session: Option<SessionInfo>,
}

impl HsRespPayload {
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }

    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        todo!()
    }
}

/// TLV 0x12（14 B）：ps_state u8 ‖ host_id 8 B ‖ session_epoch u32 ‖ 能否接回 u8
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionInfo {
    /// 同 SESSION_STATUS 的 ps_state（consts::ps_state）
    pub ps_state: u8,
    pub host_id: [u8; 8],
    /// (boot_id << 16) | 本次启动以来的会话序号
    pub epoch: u32,
    pub resumable: bool,
}
