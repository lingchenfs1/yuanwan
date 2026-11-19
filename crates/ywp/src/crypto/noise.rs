//! Noise_IKpsk2_25519_ChaChaPoly_BLAKE2s 的发起方和应答方，以及 HS_INIT / HS_RESP 的组包和拆包（规格 §4.3~§4.5）。
//!
//! ```text
//! HS_INIT（L = 1232）：外层头 14 | e 32 | 加密的 s 48 | 加密载荷 1090 + 16 | mac1 16 | mac2 16
//! HS_RESP          ：外层头 14 | e 32 | 加密载荷 n + 16                | mac1 16 | mac2 16
//! ```
//!
//! - 按包长 L 定位：mac1 在 [L−32, L−16)，覆盖 [0, L−32)；mac2 在 [L−16, L)（v1 全 0）；Noise 消息在 [14, L−32)。
//! - prologue = "YWP v0.1" ‖ CID（8 B 大端），永久冻结。
//! - psk 在位置 2，只进第二条消息：网关读完 HS_INIT、按客户端公钥查到 PSK 后再设进去。
//! - 方向密钥：Split() 的第一把是 c2s；iv 由握手哈希经 HKDF 派生（[`super::derive_ivs`]）。
//! - 用 snow 取原始密钥，AEAD 自己做（外层头要作关联数据）。
//!
//! 端点（endpoint）用法：
//! - 客户端：每次尝试 [`InitiatorAttempt::start`]，按 HS_RESP 回显的 PN 找到对应尝试，调 [`InitiatorAttempt::read_resp`]；
//! - 网关：[`accept_hs_init`] 校验 mac1、解出客户端身份和载荷、查允许列表和 TAI64N，
//!   成功后由调用方更新这个客户端的 max_ts，再 [`HsInitAccepted::respond`] 组 HS_RESP、拿到传输密钥。

use super::aead::{DirKey, TransportKeys};
use super::kdf::derive_ivs;
use super::mac::{mac1, mac1_key, verify_mac1};
use super::{wipe, Secret32, StaticKeypair, Tai64N};
use crate::consts::{
    HDR_LEN, HS_E_LEN, HS_INIT_LEN, HS_INIT_MIN, HS_INIT_PLAINTEXT_LEN, HS_MAC_LEN, NOISE_PARAMS, NOISE_PSK_POS,
    PROLOGUE_LEN, PROLOGUE_PREFIX, RECV_MAX, TAG_LEN,
};
use crate::error::{DropReason, WireError};
use crate::wire::{Cid, Class, HsInitPayload, HsRespPayload, OuterHeader, PacketType, Writer};
use snow::params::NoiseParams;
use snow::{Builder, HandshakeState};
use std::fmt;
use std::sync::OnceLock;

/// mac1 + mac2
const MACS_LEN: usize = 2 * HS_MAC_LEN;
/// HS_RESP 最短：外层头 + e + 空载荷的标签 + mac1 + mac2
const HS_RESP_MIN: usize = HDR_LEN + HS_E_LEN + TAG_LEN + MACS_LEN;
/// HS_RESP 载荷明文的上限（v1 实际约 52 B）
const HS_RESP_PLAIN_MAX: usize = 256;

fn params() -> NoiseParams {
    static P: OnceLock<NoiseParams> = OnceLock::new();
    P.get_or_init(|| NOISE_PARAMS.parse().expect("NOISE_PARAMS 是合法的参数串")).clone()
}

/// prologue = "YWP v0.1" ‖ CID（8 B 大端）
pub fn prologue(cid: Cid) -> [u8; PROLOGUE_LEN] {
    let mut p = [0u8; PROLOGUE_LEN];
    p[..PROLOGUE_PREFIX.len()].copy_from_slice(PROLOGUE_PREFIX);
    p[PROLOGUE_PREFIX.len()..].copy_from_slice(&cid.0.to_be_bytes());
    p
}

/// HS_INIT 里发起方的临时公钥 `e`（网关按它缓存 HS_RESP，规格 §4.3）。不做其他校验
pub fn hs_init_ephemeral(pkt: &[u8]) -> Option<[u8; 32]> {
    pkt.get(HDR_LEN..HDR_LEN + HS_E_LEN)?.try_into().ok()
}

/// 外层头解析错误对应的丢弃原因
pub(crate) fn header_reason(e: WireError) -> DropReason {
    match e {
        WireError::Short => DropReason::TooShort,
        WireError::BadVersion => DropReason::BadVersion,
        WireError::ReservedType => DropReason::ReservedType,
        _ => DropReason::BadHeader,
    }
}

/// 握手包的外层头：类型对、K = 0、Class = 0
fn check_hs_header(pkt: &[u8], want: PacketType) -> Result<OuterHeader, DropReason> {
    let h = OuterHeader::decode(pkt).map_err(header_reason)?;
    if h.ptype != want {
        return Err(DropReason::BadHeader);
    }
    if h.key_phase {
        return Err(DropReason::KeyPhase);
    }
    if h.class != Class::Bulk {
        return Err(DropReason::BadClass);
    }
    Ok(h)
}

fn noise_err(_: snow::Error) -> WireError {
    WireError::Malformed("Noise")
}

/// 测试钩子：固定临时私钥（金标向量）、改 prologue（「prologue 不符」的失败用例）、
/// 改 HS_INIT 包长（测网关「L ≥ 1180 都接受」）。正式代码一律用默认值
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct HsTestHooks {
    pub fixed_ephemeral: Option<[u8; 32]>,
    pub prologue: Option<Vec<u8>>,
    /// HS_INIT 的包长 L（默认 1232）
    pub init_len: Option<usize>,
}

impl fmt::Debug for HsTestHooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HsTestHooks(<redacted>)")
    }
}

fn builder<'a>(
    local: &'a StaticKeypair,
    prologue: &'a [u8],
    hooks: &'a HsTestHooks,
) -> Result<Builder<'a>, snow::Error> {
    let mut b = Builder::new(params()).local_private_key(local.secret().expose())?.prologue(prologue)?;
    if let Some(e) = &hooks.fixed_ephemeral {
        b = b.fixed_ephemeral_key_for_testing_only(e);
    }
    Ok(b)
}

/// 从结束的握手里取方向密钥和 iv：(c2s, s2c)
fn split(hs: &mut HandshakeState) -> (DirKey, DirKey) {
    let (mut k1, mut k2) = hs.dangerously_get_raw_split();
    let (iv_c2s, iv_s2c) = derive_ivs(hs.get_handshake_hash());
    let out = (DirKey::new(k1, iv_c2s), DirKey::new(k2, iv_s2c));
    wipe(&mut k1);
    wipe(&mut k2);
    out
}

// ======================================================================
// 客户端（发起方）
// ======================================================================

/// 客户端的一次握手尝试（同一 CID，PN = 尝试序号）。HS_RESP 按回显的 PN 找到对应的尝试
pub struct InitiatorAttempt {
    hs: HandshakeState,
    cid: Cid,
    pn: u32,
    /// HS_RESP 的 mac1 密钥 = BLAKE2s-256("mac1----" ‖ 自己的公钥)
    resp_mac1_key: [u8; 32],
}

impl fmt::Debug for InitiatorAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitiatorAttempt").field("cid", &self.cid).field("pn", &self.pn).finish_non_exhaustive()
    }
}

impl InitiatorAttempt {
    /// 组一个 HS_INIT（v1 固定 1232 B）写进 out，返回这次尝试的状态和包长。
    /// out 不足 1232 B 返回 NoRoom；载荷 TLV 放不进 1090 B 或不合法（name 超长）返回 Malformed
    pub fn start(
        local: &StaticKeypair,
        gw_pub: &[u8; 32],
        psk: &Secret32,
        cid: Cid,
        attempt: u32,
        payload: &HsInitPayload,
        out: &mut [u8],
    ) -> Result<(Self, usize), WireError> {
        Self::start_inner(local, gw_pub, psk, cid, attempt, payload, out, &HsTestHooks::default())
    }

    /// 同 [`Self::start`]，带测试钩子
    #[cfg(any(test, feature = "testkit"))]
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_hooks(
        local: &StaticKeypair,
        gw_pub: &[u8; 32],
        psk: &Secret32,
        cid: Cid,
        attempt: u32,
        payload: &HsInitPayload,
        out: &mut [u8],
        hooks: &HsTestHooks,
    ) -> Result<(Self, usize), WireError> {
        Self::start_inner(local, gw_pub, psk, cid, attempt, payload, out, hooks)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_inner(
        local: &StaticKeypair,
        gw_pub: &[u8; 32],
        psk: &Secret32,
        cid: Cid,
        attempt: u32,
        payload: &HsInitPayload,
        out: &mut [u8],
        hooks: &HsTestHooks,
    ) -> Result<(Self, usize), WireError> {
        let len = hooks.init_len.unwrap_or(HS_INIT_LEN);
        // 包长 L 时载荷明文 = L − (外层头 + e + 加密的 s + 标签 + mac1 + mac2)
        let fixed = HS_INIT_LEN - HS_INIT_PLAINTEXT_LEN;
        if len < fixed || len > RECV_MAX {
            return Err(WireError::Malformed("HS_INIT 包长"));
        }
        if out.len() < len {
            return Err(WireError::NoRoom);
        }
        let out = &mut out[..len];
        let mut plain_buf = [0u8; RECV_MAX];
        let plain = &mut plain_buf[..len - fixed];
        payload.encode(&mut Writer::new(plain)).map_err(|e| match e {
            WireError::NoRoom => WireError::Malformed("HS_INIT 载荷放不下"),
            e => e,
        })?;

        let pro = prologue(cid);
        let pro: &[u8] = hooks.prologue.as_deref().unwrap_or(&pro);
        let mut hs = builder(local, pro, hooks)
            .and_then(|b| b.remote_public_key(gw_pub))
            .and_then(|b| b.psk(NOISE_PSK_POS, psk.expose()))
            .and_then(|b| b.build_initiator())
            .map_err(noise_err)?;

        OuterHeader::new(PacketType::HsInit, cid, 0, attempt).encode(out);
        let mac_at = len - MACS_LEN;
        let n = hs.write_message(plain, &mut out[HDR_LEN..mac_at]).map_err(noise_err)?;
        debug_assert_eq!(HDR_LEN + n, mac_at, "HS_INIT 布局");
        let m = mac1(&mac1_key(gw_pub), &out[..mac_at]);
        out[mac_at..mac_at + HS_MAC_LEN].copy_from_slice(&m);
        out[mac_at + HS_MAC_LEN..].fill(0);

        let me = Self { hs, cid, pn: attempt, resp_mac1_key: mac1_key(&local.public()) };
        Ok((me, len))
    }

    pub fn cid(&self) -> Cid {
        self.cid
    }

    /// 尝试序号（HS_INIT 外层头的 PN；HS_RESP 回显它）
    pub fn pn(&self) -> u32 {
        self.pn
    }

    /// 读 HS_RESP：外层头（类型、CID、回显的 PN）→ mac1（按自己的公钥）→ Noise 第二条消息 → 载荷 TLV。
    /// 成功返回载荷和传输密钥（tx = c2s，rx = s2c）；err ≠ OK 时密钥照样返回，由端点决定不用。
    ///
    /// 外层头、mac1 或 Noise 解密失败时状态不变，可以继续等别的 HS_RESP；
    /// Noise 解开之后才发现 TLV 不对（HsBadTlv）的，这次尝试作废。
    pub fn read_resp(&mut self, pkt: &[u8]) -> Result<(HsRespPayload, TransportKeys), DropReason> {
        if pkt.len() < HS_RESP_MIN {
            return Err(DropReason::TooShort);
        }
        if pkt.len() > RECV_MAX {
            return Err(DropReason::TooLong);
        }
        let h = check_hs_header(pkt, PacketType::HsResp)?;
        if h.cid != self.cid || h.pn_trunc != self.pn {
            return Err(DropReason::HsUnexpected);
        }
        let mac_at = pkt.len() - MACS_LEN;
        if !verify_mac1(&self.resp_mac1_key, &pkt[..mac_at], &pkt[mac_at..mac_at + HS_MAC_LEN]) {
            return Err(DropReason::Mac1Fail);
        }
        if self.hs.is_handshake_finished() {
            return Err(DropReason::HsUnexpected);
        }
        let mut plain = [0u8; RECV_MAX];
        let n = self.hs.read_message(&pkt[HDR_LEN..mac_at], &mut plain).map_err(|_| DropReason::HsNoiseFail)?;
        let resp = HsRespPayload::decode(&plain[..n]).map_err(|_| DropReason::HsBadTlv)?;
        let (c2s, s2c) = split(&mut self.hs);
        Ok((resp, TransportKeys { tx: c2s, rx: s2c }))
    }
}

// ======================================================================
// 网关（应答方）
// ======================================================================

/// 允许列表里查到的客户端：PSK 和网关记住的最大 TAI64N
#[derive(Clone, Debug)]
pub struct HsPeer {
    pub psk: Secret32,
    pub last_ts: Option<Tai64N>,
}

/// 解开并通过校验的 HS_INIT（还没回 HS_RESP）
pub struct HsInitAccepted {
    hs: HandshakeState,
    pub cid: Cid,
    /// 尝试序号，HS_RESP 回显
    pub pn: u32,
    pub path_id: u8,
    pub client_pub: [u8; 32],
    /// 发起方的临时公钥（缓存 HS_RESP 用）
    pub ephemeral: [u8; 32],
    pub payload: HsInitPayload,
}

impl fmt::Debug for HsInitAccepted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HsInitAccepted")
            .field("cid", &self.cid)
            .field("pn", &self.pn)
            .field("path_id", &self.path_id)
            .field("payload", &self.payload)
            .finish_non_exhaustive()
    }
}

/// 网关处理 HS_INIT（规格 §4.4「网关的处理顺序」）：
/// 长度 ≥ 1180 → 外层头 → mac1（按网关公钥）→ Noise 第一条消息（解出客户端公钥和载荷）→
/// 查允许列表（lookup 返回 None 计 UnknownClient）→ 载荷 TLV → TAI64N 必须大于 last_ts。
///
/// 任何一步失败都返回丢弃原因，调用方静默丢弃。这里不改任何状态：
/// 成功之后由调用方把这个客户端的 max_ts 更新成 `payload.ts`（失败时不更新）。
/// `gw_mac1_key` = [`mac1_key`]`(网关公钥)`，调用方算一次存着。
pub fn accept_hs_init(
    local: &StaticKeypair,
    gw_mac1_key: &[u8; 32],
    pkt: &[u8],
    lookup: impl FnOnce(&[u8; 32]) -> Option<HsPeer>,
) -> Result<HsInitAccepted, DropReason> {
    accept_inner(local, gw_mac1_key, pkt, lookup, &HsTestHooks::default())
}

/// 同 [`accept_hs_init`]，带测试钩子（固定网关的临时私钥）
#[cfg(any(test, feature = "testkit"))]
#[doc(hidden)]
pub fn accept_hs_init_with_hooks(
    local: &StaticKeypair,
    gw_mac1_key: &[u8; 32],
    pkt: &[u8],
    lookup: impl FnOnce(&[u8; 32]) -> Option<HsPeer>,
    hooks: &HsTestHooks,
) -> Result<HsInitAccepted, DropReason> {
    accept_inner(local, gw_mac1_key, pkt, lookup, hooks)
}

fn accept_inner(
    local: &StaticKeypair,
    gw_mac1_key: &[u8; 32],
    pkt: &[u8],
    lookup: impl FnOnce(&[u8; 32]) -> Option<HsPeer>,
    hooks: &HsTestHooks,
) -> Result<HsInitAccepted, DropReason> {
    if pkt.len() < HS_INIT_MIN {
        return Err(DropReason::HsTooShort);
    }
    if pkt.len() > RECV_MAX {
        return Err(DropReason::TooLong);
    }
    let h = check_hs_header(pkt, PacketType::HsInit)?;
    let mac_at = pkt.len() - MACS_LEN;
    if !verify_mac1(gw_mac1_key, &pkt[..mac_at], &pkt[mac_at..mac_at + HS_MAC_LEN]) {
        return Err(DropReason::Mac1Fail);
    }

    let pro = prologue(h.cid);
    let pro: &[u8] = hooks.prologue.as_deref().unwrap_or(&pro);
    let mut hs = builder(local, pro, hooks).and_then(|b| b.build_responder()).map_err(|_| DropReason::HsNoiseFail)?;
    let mut plain = [0u8; RECV_MAX];
    let n = hs.read_message(&pkt[HDR_LEN..mac_at], &mut plain).map_err(|_| DropReason::HsNoiseFail)?;
    let client_pub: [u8; 32] =
        hs.get_remote_static().and_then(|s| s.try_into().ok()).ok_or(DropReason::HsNoiseFail)?;

    let peer = lookup(&client_pub).ok_or(DropReason::UnknownClient)?;
    hs.set_psk(NOISE_PSK_POS as usize, peer.psk.expose()).map_err(|_| DropReason::HsNoiseFail)?;
    let payload = HsInitPayload::decode(&plain[..n]).map_err(|_| DropReason::HsBadTlv)?;
    if peer.last_ts.is_some_and(|last| payload.ts <= last) {
        return Err(DropReason::StaleTimestamp);
    }
    let ephemeral = hs_init_ephemeral(pkt).ok_or(DropReason::TooShort)?;
    Ok(HsInitAccepted { hs, cid: h.cid, pn: h.pn_trunc, path_id: h.path_id, client_pub, ephemeral, payload })
}

impl HsInitAccepted {
    /// 组 HS_RESP 写进 out（不补齐），返回包长和传输密钥（tx = s2c，rx = c2s）。
    /// err ≠ OK（BUSY、CID_COLLISION……）也用它回，密钥丢掉即可。
    /// out 放不下返回 NoRoom；载荷不合法（err = OK 却没有 reset_token）返回 Malformed
    pub fn respond(mut self, resp: &HsRespPayload, out: &mut [u8]) -> Result<(usize, TransportKeys), WireError> {
        let mut plain = [0u8; HS_RESP_PLAIN_MAX];
        let mut w = Writer::new(&mut plain);
        resp.encode(&mut w)?;
        let plain_len = w.pos();
        let total = HDR_LEN + HS_E_LEN + plain_len + TAG_LEN + MACS_LEN;
        if out.len() < total {
            return Err(WireError::NoRoom);
        }
        let out = &mut out[..total];
        OuterHeader::new(PacketType::HsResp, self.cid, self.path_id, self.pn).encode(out);
        let mac_at = total - MACS_LEN;
        let n = self.hs.write_message(&plain[..plain_len], &mut out[HDR_LEN..mac_at]).map_err(noise_err)?;
        debug_assert_eq!(HDR_LEN + n, mac_at, "HS_RESP 布局");
        let m = mac1(&mac1_key(&self.client_pub), &out[..mac_at]);
        out[mac_at..mac_at + HS_MAC_LEN].copy_from_slice(&m);
        out[mac_at + HS_MAC_LEN..].fill(0);
        let (c2s, s2c) = split(&mut self.hs);
        Ok((total, TransportKeys { tx: s2c, rx: c2s }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{caps, PROTO_MIN_VERSION, PROTO_VERSION};
    use crate::wire::HsErr;

    fn init_payload(ts: Tai64N) -> HsInitPayload {
        HsInitPayload {
            ts,
            ver: PROTO_VERSION,
            min_ver: PROTO_MIN_VERSION,
            caps: caps::SPLIT2 | caps::HEVC,
            resume: None,
            last_fid: None,
            mode: Some((0, 0)),
            name: Some("测试".into()),
            bitrate_cap_kbps: None,
        }
    }

    #[test]
    fn roundtrip_keys_match() {
        let gw = StaticKeypair::from_secret([1; 32]);
        let cl = StaticKeypair::from_secret([2; 32]);
        let psk = Secret32::new([3; 32]);
        let cid = Cid(42);
        let mut pkt = [0u8; HS_INIT_LEN];
        let (mut att, len) =
            InitiatorAttempt::start(&cl, &gw.public(), &psk, cid, 0, &init_payload(Tai64N::from_unix(1, 0)), &mut pkt)
                .unwrap();
        assert_eq!(len, HS_INIT_LEN);
        let acc = accept_hs_init(&gw, &mac1_key(&gw.public()), &pkt, |p| {
            assert_eq!(p, &cl.public());
            Some(HsPeer { psk: psk.clone(), last_ts: None })
        })
        .unwrap();
        let resp = HsRespPayload {
            ver: 1,
            min_ver: 1,
            caps: caps::SPLIT2,
            err: HsErr::Ok,
            reset_token: Some([9; 16]),
            session: None,
        };
        let mut out = [0u8; 256];
        let (rlen, gk) = acc.respond(&resp, &mut out).unwrap();
        let (got, ck) = att.read_resp(&out[..rlen]).unwrap();
        assert_eq!(got, resp);
        assert_eq!(ck.tx.raw_for_testing(), gk.rx.raw_for_testing());
        assert_eq!(ck.rx.raw_for_testing(), gk.tx.raw_for_testing());
        assert_ne!(ck.tx.raw_for_testing().0, ck.rx.raw_for_testing().0);
        assert_ne!(ck.tx.raw_for_testing().1, ck.rx.raw_for_testing().1);
        // 同一个 HS_RESP 再读一次：尝试已经结束
        assert_eq!(att.read_resp(&out[..rlen]).err(), Some(DropReason::HsUnexpected));
    }
}
