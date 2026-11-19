//! 握手载荷的 TLV（规格 §4.4）：type u8 | len u16 | value。
//! 0x00 表示填充，遇到就停止解析；不认识的 type 按 len 跳过；同一个 type 出现两次按格式错误处理。
//! 包的整体布局（e、加密的 s、mac1、mac2）由 crypto::noise 组装。
//!
//! [`TlvIter`]、[`put_tlv`] 也给 CONTROL 消息体用（规格 §5.5.5：TLV 的编码和规则与握手载荷相同）。

use super::{Reader, Writer};
use crate::consts::{hs_tlv, TLV_HDR_LEN};
use crate::crypto::Tai64N;
use crate::error::WireError;

/// 写一个 TLV。value 超过 65535 B 返回 Malformed
pub fn put_tlv(w: &mut Writer<'_>, ty: u8, value: &[u8]) -> Result<(), WireError> {
    let len = u16::try_from(value.len()).map_err(|_| WireError::Malformed("TLV 太长"))?;
    if w.remaining() < TLV_HDR_LEN + value.len() {
        return Err(WireError::NoRoom);
    }
    w.put_u8(ty)?;
    w.put_u16(len)?;
    w.put_slice(value)
}

/// 逐个取 TLV：(type, value)。遇到 type 0x00（填充）或数据结束就停；越界返回 Truncated 后停。
/// 「同一个 type 出现两次」由调用方判断（CONTROL 里有的 TLV 可以重复），可以用 [`SeenSet`]
#[derive(Clone, Debug)]
pub struct TlvIter<'a> {
    r: Reader<'a>,
    done: bool,
}

impl<'a> TlvIter<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { r: Reader::new(b), done: false }
    }
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<(u8, &'a [u8]), WireError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.r.is_empty() {
            return None;
        }
        let ty = self.r.u8().ok()?;
        if ty == hs_tlv::PAD {
            self.done = true;
            return None;
        }
        let item = self.r.u16().and_then(|len| self.r.bytes(len as usize)).map(|v| (ty, v));
        if item.is_err() {
            self.done = true;
        }
        Some(item)
    }
}

/// 记录见过的 TLV type，第二次出现时报错
#[derive(Clone, Copy, Debug, Default)]
pub struct SeenSet([u64; 4]);

impl SeenSet {
    /// 第一次见到返回 Ok；重复返回 Malformed
    pub fn insert(&mut self, ty: u8) -> Result<(), WireError> {
        let (w, b) = (ty as usize / 64, ty as usize % 64);
        if self.0[w] & (1 << b) != 0 {
            return Err(WireError::Malformed("TLV 重复"));
        }
        self.0[w] |= 1 << b;
        Ok(())
    }

    pub fn contains(&self, ty: u8) -> bool {
        self.0[ty as usize / 64] & (1 << (ty as usize % 64)) != 0
    }
}

fn fixed<const N: usize>(v: &[u8], what: &'static str) -> Result<[u8; N], WireError> {
    v.try_into().map_err(|_| WireError::Malformed(what))
}

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
    /// 写 TLV（不含补齐；补齐和加密由调用方做）。name 超过 32 B 返回 Malformed
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        put_tlv(w, hs_tlv::TS, &self.ts.to_bytes())?;
        put_tlv(w, hs_tlv::VER, &[self.ver, self.min_ver])?;
        put_tlv(w, hs_tlv::CAPS, &self.caps.to_be_bytes())?;
        if let Some((host_id, epoch)) = self.resume {
            let mut v = [0u8; hs_tlv::RESUME_LEN];
            v[..8].copy_from_slice(&host_id);
            v[8..].copy_from_slice(&epoch.to_be_bytes());
            put_tlv(w, hs_tlv::RESUME, &v)?;
        }
        if let Some((fid, cfg_ver)) = self.last_fid {
            let f = fid.to_be_bytes();
            put_tlv(w, hs_tlv::LAST_FID, &[f[0], f[1], cfg_ver])?;
        }
        if let Some((mode, flags)) = self.mode {
            put_tlv(w, hs_tlv::MODE, &[mode, flags])?;
        }
        if let Some(name) = &self.name {
            if name.len() > hs_tlv::NAME_MAX {
                return Err(WireError::Malformed("name 超过 32 B"));
            }
            put_tlv(w, hs_tlv::NAME, name.as_bytes())?;
        }
        if let Some(cap) = self.bitrate_cap_kbps {
            put_tlv(w, hs_tlv::BITRATE_CAP, &cap.to_be_bytes())?;
        }
        Ok(())
    }

    /// 解 TLV；缺必带项、type 重复、长度不对都返回错误。只属于 HS_RESP 的 type 按不认识的跳过。
    /// caps 位 0、版本是否兼容由端点判断
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let mut seen = SeenSet::default();
        let mut ts = None;
        let mut ver = None;
        let mut caps = None;
        let mut out_resume = None;
        let mut out_last_fid = None;
        let mut out_mode = None;
        let mut out_name = None;
        let mut out_cap = None;
        for item in TlvIter::new(b) {
            let (ty, v) = item?;
            seen.insert(ty)?;
            match ty {
                hs_tlv::TS => ts = Some(Tai64N::from_bytes(&fixed::<12>(v, "ts 长度")?)),
                hs_tlv::VER => ver = Some(fixed::<2>(v, "ver 长度")?),
                hs_tlv::CAPS => caps = Some(u32::from_be_bytes(fixed::<4>(v, "caps 长度")?)),
                hs_tlv::RESUME => {
                    let a = fixed::<12>(v, "resume 长度")?;
                    let mut host = [0u8; 8];
                    host.copy_from_slice(&a[..8]);
                    out_resume = Some((host, u32::from_be_bytes([a[8], a[9], a[10], a[11]])));
                }
                hs_tlv::LAST_FID => {
                    let a = fixed::<3>(v, "last_fid 长度")?;
                    out_last_fid = Some((u16::from_be_bytes([a[0], a[1]]), a[2]));
                }
                hs_tlv::MODE => {
                    let a = fixed::<2>(v, "mode 长度")?;
                    out_mode = Some((a[0], a[1]));
                }
                hs_tlv::NAME => {
                    if v.len() > hs_tlv::NAME_MAX {
                        return Err(WireError::Malformed("name 超过 32 B"));
                    }
                    let s = std::str::from_utf8(v).map_err(|_| WireError::Malformed("name 不是 UTF-8"))?;
                    out_name = Some(s.to_owned());
                }
                hs_tlv::BITRATE_CAP => out_cap = Some(u32::from_be_bytes(fixed::<4>(v, "bitrate_cap 长度")?)),
                _ => {}
            }
        }
        let ts = ts.ok_or(WireError::Malformed("缺 ts"))?;
        let ver = ver.ok_or(WireError::Malformed("缺 ver"))?;
        let caps = caps.ok_or(WireError::Malformed("缺 caps"))?;
        Ok(Self {
            ts,
            ver: ver[0],
            min_ver: ver[1],
            caps,
            resume: out_resume,
            last_fid: out_last_fid,
            mode: out_mode,
            name: out_name,
            bitrate_cap_kbps: out_cap,
        })
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

impl HsErr {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => HsErr::Ok,
            1 => HsErr::CidCollision,
            2 => HsErr::Busy,
            3 => HsErr::Version,
            _ => return None,
        })
    }
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
    /// err = Ok 却没有 reset_token 返回 Malformed
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        if self.err == HsErr::Ok && self.reset_token.is_none() {
            return Err(WireError::Malformed("err = 0 时必带 reset_token"));
        }
        put_tlv(w, hs_tlv::VER, &[self.ver, self.min_ver])?;
        put_tlv(w, hs_tlv::CAPS, &self.caps.to_be_bytes())?;
        put_tlv(w, hs_tlv::ERR, &[self.err as u8])?;
        if let Some(t) = &self.reset_token {
            put_tlv(w, hs_tlv::RESET_TOKEN, t)?;
        }
        if let Some(s) = &self.session {
            put_tlv(w, hs_tlv::SESSION, &s.to_bytes())?;
        }
        Ok(())
    }

    /// 解 TLV；缺必带项（ver、caps、err，err = 0 时的 reset_token）、type 重复、长度不对、
    /// 不认识的 err 都返回错误
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        let mut seen = SeenSet::default();
        let mut ver = None;
        let mut caps = None;
        let mut err = None;
        let mut token = None;
        let mut session = None;
        for item in TlvIter::new(b) {
            let (ty, v) = item?;
            seen.insert(ty)?;
            match ty {
                hs_tlv::VER => ver = Some(fixed::<2>(v, "ver 长度")?),
                hs_tlv::CAPS => caps = Some(u32::from_be_bytes(fixed::<4>(v, "caps 长度")?)),
                hs_tlv::ERR => {
                    let a = fixed::<1>(v, "err 长度")?;
                    err = Some(HsErr::from_u8(a[0]).ok_or(WireError::Malformed("不认识的 err"))?);
                }
                hs_tlv::RESET_TOKEN => token = Some(fixed::<16>(v, "reset_token 长度")?),
                hs_tlv::SESSION => session = Some(SessionInfo::from_bytes(&fixed::<14>(v, "session 长度")?)),
                _ => {}
            }
        }
        let ver = ver.ok_or(WireError::Malformed("缺 ver"))?;
        let caps = caps.ok_or(WireError::Malformed("缺 caps"))?;
        let err = err.ok_or(WireError::Malformed("缺 err"))?;
        if err == HsErr::Ok && token.is_none() {
            return Err(WireError::Malformed("err = 0 时缺 reset_token"));
        }
        Ok(Self { ver: ver[0], min_ver: ver[1], caps, err, reset_token: token, session })
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

impl SessionInfo {
    pub fn to_bytes(&self) -> [u8; hs_tlv::SESSION_LEN] {
        let mut b = [0u8; hs_tlv::SESSION_LEN];
        b[0] = self.ps_state;
        b[1..9].copy_from_slice(&self.host_id);
        b[9..13].copy_from_slice(&self.epoch.to_be_bytes());
        b[13] = self.resumable as u8;
        b
    }

    /// 「能否接回」非 0 即真
    pub fn from_bytes(b: &[u8; hs_tlv::SESSION_LEN]) -> Self {
        let mut host_id = [0u8; 8];
        host_id.copy_from_slice(&b[1..9]);
        Self { ps_state: b[0], host_id, epoch: u32::from_be_bytes([b[9], b[10], b[11], b[12]]), resumable: b[13] != 0 }
    }
}
