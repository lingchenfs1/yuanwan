//! 网关端点（B 组）。v1 只有一个控制客户端。
//!
//! - 同一 CID、同一客户端公钥算新的尝试，加进候选（最多 5 份，5 s 过期）；公钥不同才回 CID_COLLISION；
//!   收到 DATA 时逐份试解，先解开的那份确认（规格 §4.3）；
//! - `accept_hs_init` 成功后才把这个客户端的 max_ts 更新成本次的 TAI64N（规格 §4.6）；
//! - 同一个 HS_INIT 的副本（首包双发的第二份、多入口）按发起者的 `e` 命中 HS_RESP 缓存（1 s），原样回同一个包；
//! - 接管发生在密钥确认的时刻；`TakeoverPolicy::RequireFlag`（默认）时另一台客户端没带接管位就回 BUSY，
//!   被接管的旧客户端收到 CLOSE(1)；同一客户端身份的新会话静默顶掉旧会话（规格 §3.1）；
//! - 路径的对端地址取第一个认证通过的 DATA 的源地址（规格 §3.3、§4.3）；
//! - 半开会话 3 s，已确认会话 60 s 没有任何包就删除（规格 §13.1、附录 B）；
//! - 握手每个源地址前缀每秒 ≤ 10 个；RESET 只回 ≥ 64 B 的未知 CID 包，同一源地址每 100 ms ≤ 1 个，
//!   全局每秒 ≤ 50 个（规格 §4.4、§4.6）；
//! - 密钥确认之后（并且不在关闭）才发媒体。
//!
//! M2a 的简化：
//! - 对端地址变了（NAT 重绑定）：认证通过、PN 是迄今最大时直接切到新地址，并向新地址发一个 PATH_CHALLENGE；
//!   规格 §3.3 的「验证完成前 3 倍放大限制」留到 M2d；
//! - Blind / Held（规格 §13.2）留到 M2d：M2a 收到 CLOSE 就删会话，60 s 静默就删会话（报 Closed(4)）；
//! - NACK（M2c）、音频和反馈（M2b）的接口先占位：收到 NACK 计 Unsupported，queue_audio / set_feedback 不发。

use super::session::{chunk_bytes, close_text, try_open, Common, OpenedMeta, Sess, Small};
use super::{AttemptRing, ConnStats, RxMeta, Transmit};
use crate::consts::{
    caps, mode_flags, BASE_PAYLOAD, FRAME_STATUS_REPEAT, HS_INIT_MIN, HS_MAC_LEN, PROTO_MIN_VERSION, PROTO_VERSION,
    RECV_MAX, RESET_LEN, RESET_TRIGGER_MIN,
};
use crate::consts::close_code;
use crate::creds::{ClientEntry, ClientId, Keys};
use crate::crypto::{
    accept_hs_init, hs_init_ephemeral, mac1_key, reset_token, verify_mac1, HsPeer, Secret32, StaticKeypair, Tai64N,
    TransportKeys,
};
use crate::error::{DropCounters, DropReason};
use crate::msg::ControlMsg;
use crate::packet::{encode_reset, header_drop_reason, PathRx};
use crate::reliable::CtlError;
use crate::rtt::RttEstimator;
use crate::sched::{Dir, SmallOpts, SymbolSource, VideoCarrier};
use crate::time::{ms, secs, Micros, HALF_OPEN_TIMEOUT, RESET_GLOBAL_PER_SEC, SESSION_IDLE_TIMEOUT};
use crate::wire::{
    Chunk, ChunkIter, Cid, FeedbackState, FrameStatusEntry, HsErr, HsInitPayload, HsRespPayload, InputChunk, Need,
    OuterHeader, PacketType, Report, SessionInfo,
};
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};

// ---- 规格里有、consts/time 里还没有的值（交协调人收进 consts.rs / time.rs） ----

/// 每个源地址前缀每秒最多这么多个握手（规格 §4.6）
const HS_PER_PREFIX_PER_SEC: u32 = 10;
/// 握手限频的前缀：IPv4 取 /24，IPv6 取 /64
const HS_PREFIX_V4_BITS: u32 = 24;
/// 同一个 HS_INIT 的副本按 `e` 命中 HS_RESP 缓存，缓存 1 s（规格 §4.3）
const RESP_CACHE_TTL: Micros = secs(1);
/// HS_RESP 缓存最多这么多份（防内存被刷爆）
const RESP_CACHE_MAX: usize = 64;
/// 同一源地址每 100 ms 最多回一个 RESET（规格 §4.4）
const RESET_PER_SRC_GAP: Micros = ms(100);
/// 同时在等第一个 DATA 的半开会话（按 CID）最多这么多个
const PENDING_MAX: usize = 16;
/// 限频表超过这么多项时清掉过期的
const RATE_TABLE_PRUNE: usize = 1024;
/// 网关支持的能力（v1 不做 HDR）
const GW_CAPS: u32 = caps::SPLIT2 | caps::HEVC;

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    /// 来自 creds::load_keys
    pub keys: Keys,
    pub takeover: TakeoverPolicy,
}

/// 第二台客户端连上时怎么办（gateway.conf 的 ywp_takeover）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TakeoverPolicy {
    /// 默认（ywp_takeover=0）：没带接管位就回 BUSY
    #[default]
    RequireFlag,
    /// ywp_takeover=1
    Auto,
}

/// 上行存活状态（M2d）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// 500 ms 没有任何包：盲发档
    Blind,
    /// 10 s 没有任何包或收到 CLOSE：停发媒体，每 1 s 心跳，最多 60 s
    Held,
    /// 恢复
    Back,
}

#[derive(Debug)]
pub enum GwEvent<'a> {
    /// 密钥确认、会话建立（init 是确认的那次尝试的 HS_INIT 载荷）
    Connected { cid: Cid, client: ClientId, init: HsInitPayload },
    /// 新会话顶掉了旧会话（同一身份静默作废；另一身份接管时旧客户端收到 CLOSE(1)）
    Replaced { old: Cid, new: Cid },
    Input(InputChunk<'a>, RxMeta),
    /// NACK 换算出的丢失符号 (fid, sym)（M2c）
    Lost(&'a [(u16, u16)]),
    Need(Need),
    Report(Report<'a>),
    Control(ControlMsg),
    /// M2d
    Liveness(Liveness),
    /// 会话结束：收到客户端的 CLOSE（带对方的 code），或 60 s 静默被删（Closed(4)）
    Closed(u16),
    PathChanged(SocketAddr),
    Dropped(DropReason),
}

/// 一份握手候选：HS_RESP 已发出，等第一个 DATA
#[derive(Debug)]
struct Cand {
    keys: TransportKeys,
    resp_at: Micros,
    init: HsInitPayload,
    takeover: bool,
}

/// 同一 CID 的半开会话
#[derive(Debug)]
struct Pending {
    client_pub: [u8; 32],
    cands: AttemptRing<Cand>,
    last_resp: Micros,
    /// 这个 CID 收过几个被接受的 HS_INIT
    attempts: u32,
}

#[derive(Debug)]
struct CachedResp {
    e: [u8; 32],
    cid: Cid,
    at: Micros,
    bytes: Vec<u8>,
}

/// 握手限频的源地址前缀
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Prefix {
    V4(u32),
    V6(u64),
}

fn prefix_of(ip: IpAddr) -> Prefix {
    match ip {
        IpAddr::V4(a) => Prefix::V4(u32::from(a) & (u32::MAX << (32 - HS_PREFIX_V4_BITS))),
        IpAddr::V6(a) => Prefix::V6((u128::from(a) >> 64) as u64),
    }
}

fn emit_drop(d: &mut DropCounters, r: DropReason, sink: &mut dyn FnMut(GwEvent<'_>)) {
    d.bump(r);
    sink(GwEvent::Dropped(r));
}

#[derive(Debug)]
pub struct Gateway {
    static_kp: StaticKeypair,
    mac1_key: [u8; 32],
    reset_key: Secret32,
    clients: Vec<ClientEntry>,
    /// 每个客户端收到过的最大 TAI64N（只在 HS_INIT 整个解开之后更新）
    max_ts: HashMap<[u8; 32], Tai64N>,
    takeover: TakeoverPolicy,
    session_info: Option<SessionInfo>,
    pending: HashMap<Cid, Pending>,
    resp_cache: VecDeque<CachedResp>,
    conn: Option<GwConn>,
    /// 被接管（CLOSE(1)）、被踢（kick）、配对被删（reload_clients）的旧会话：只把 3 份 CLOSE 发完；
    /// 60 s 静默的会话直接丢弃，不进这里
    retiring: Vec<GwConn>,
    /// 待发的握手回包和 RESET
    raw: VecDeque<(SocketAddr, Vec<u8>)>,
    hs_rate: HashMap<Prefix, (Micros, u32)>,
    reset_src: HashMap<SocketAddr, Micros>,
    reset_global: (Micros, u32),
    /// 最近一次调用传进来的 now（kick、reload_clients 不带 now）
    last_now: Micros,
    /// 会话之外的丢弃计数（握手、未知 CID……）；会话内的在 GwConn::stats
    drops: DropCounters,
}

impl Gateway {
    pub fn new(cfg: GatewayConfig) -> Self {
        let Keys { static_kp, reset_key, clients } = cfg.keys;
        let mac1_key = mac1_key(&static_kp.public());
        Self {
            static_kp,
            mac1_key,
            reset_key,
            clients,
            max_ts: HashMap::new(),
            takeover: cfg.takeover,
            session_info: None,
            pending: HashMap::new(),
            resp_cache: VecDeque::new(),
            conn: None,
            retiring: Vec::new(),
            raw: VecDeque::new(),
            hs_rate: HashMap::new(),
            reset_src: HashMap::new(),
            reset_global: (0, 0),
            last_now: 0,
            drops: DropCounters::new(),
        }
    }

    /// 被删掉的客户端，会话立即删除（还会给它发 3 份 CLOSE(4)）
    pub fn reload_clients(&mut self, clients: Vec<ClientEntry>) {
        self.clients = clients;
        let known = |p: &[u8; 32]| self.clients.iter().any(|c| c.public == *p);
        self.pending.retain(|_, p| known(&p.client_pub));
        if self.conn.as_ref().is_some_and(|c| !known(&c.client_pub)) {
            let now = self.last_now;
            self.retire_conn(close_code::GATEWAY_ENDED, now);
        }
    }

    /// 填进 HS_RESP 的 TLV 0x12
    pub fn set_session_info(&mut self, s: SessionInfo) {
        self.session_info = Some(s);
    }

    /// 会话之外的丢弃计数（握手、未知 CID、限频……）
    /// 还有旧会话的 CLOSE 没发完（网关退出时据此等 3 份 CLOSE(5) 真正发出去）
    pub fn closing_in_progress(&self) -> bool {
        self.retiring.iter().any(|c| !c.s.close_done())
    }

    pub fn drops(&self) -> &DropCounters {
        &self.drops
    }

    pub fn handle_datagram(&mut self, now: Micros, from: SocketAddr, pkt: &mut [u8], sink: &mut dyn FnMut(GwEvent<'_>)) {
        self.last_now = self.last_now.max(now);
        if pkt.len() > RECV_MAX {
            return emit_drop(&mut self.drops, DropReason::TooLong, sink);
        }
        let hdr = match OuterHeader::decode(pkt) {
            Ok(h) => h,
            Err(e) => return emit_drop(&mut self.drops, header_drop_reason(e), sink),
        };
        match hdr.ptype {
            PacketType::HsInit => self.on_hs_init(now, from, pkt, sink),
            PacketType::Data => self.on_data(now, from, &hdr, pkt, sink),
            PacketType::HsResp | PacketType::Reset => emit_drop(&mut self.drops, DropReason::WrongDirection, sink),
            _ => emit_drop(&mut self.drops, DropReason::Unsupported, sink),
        }
    }

    // ------------------------------------------------------------------
    // 握手
    // ------------------------------------------------------------------

    fn on_hs_init(&mut self, now: Micros, from: SocketAddr, pkt: &[u8], sink: &mut dyn FnMut(GwEvent<'_>)) {
        if pkt.len() < HS_INIT_MIN {
            return emit_drop(&mut self.drops, DropReason::HsTooShort, sink);
        }
        // mac1 先单独验一次（便宜），通不过的连缓存都不查
        let mac_at = pkt.len() - 2 * HS_MAC_LEN;
        if !verify_mac1(&self.mac1_key, &pkt[..mac_at], &pkt[mac_at..mac_at + HS_MAC_LEN]) {
            return emit_drop(&mut self.drops, DropReason::Mac1Fail, sink);
        }
        let cid = OuterHeader::peek_cid(pkt).unwrap_or(Cid(0));
        // 同一个 HS_INIT 的副本：原样回同一个 HS_RESP（规格 §4.3、§4.6）
        self.resp_cache.retain(|c| now < c.at + RESP_CACHE_TTL);
        if let Some(e) = hs_init_ephemeral(pkt) {
            if let Some(c) = self.resp_cache.iter().find(|c| c.e == e && c.cid == cid) {
                self.raw.push_back((from, c.bytes.clone()));
                return;
            }
        }
        if !self.hs_allow(now, from) {
            return emit_drop(&mut self.drops, DropReason::HsRateLimited, sink);
        }
        let clients = &self.clients;
        let max_ts = &self.max_ts;
        let acc = accept_hs_init(&self.static_kp, &self.mac1_key, pkt, |p| {
            clients
                .iter()
                .find(|c| c.public == *p)
                .map(|c| HsPeer { psk: c.psk.clone(), last_ts: max_ts.get(p).copied() })
        });
        let acc = match acc {
            Ok(a) => a,
            Err(r) => return emit_drop(&mut self.drops, r, sink),
        };
        self.max_ts.insert(acc.client_pub, acc.payload.ts);

        let err = self.decide(&acc.cid, &acc.client_pub, &acc.payload);
        let resp = HsRespPayload {
            ver: PROTO_VERSION,
            min_ver: PROTO_MIN_VERSION,
            caps: acc.payload.caps & GW_CAPS,
            err,
            reset_token: (err == HsErr::Ok).then(|| reset_token(self.reset_key.expose(), acc.cid)),
            session: self.session_info,
        };
        let (cid, pn, client_pub, e) = (acc.cid, acc.pn, acc.client_pub, acc.ephemeral);
        let init = acc.payload.clone();
        let mut buf = [0u8; RECV_MAX];
        let (len, keys) = match acc.respond(&resp, &mut buf) {
            Ok(x) => x,
            Err(_) => return emit_drop(&mut self.drops, DropReason::HsBadTlv, sink),
        };
        let bytes = buf[..len].to_vec();
        if self.resp_cache.len() >= RESP_CACHE_MAX {
            self.resp_cache.pop_front();
        }
        self.resp_cache.push_back(CachedResp { e, cid, at: now, bytes: bytes.clone() });
        self.raw.push_back((from, bytes));
        if err != HsErr::Ok {
            return;
        }

        if !self.pending.contains_key(&cid) && self.pending.len() >= PENDING_MAX {
            // 挤掉最久没动静的半开会话
            if let Some(old) = self.pending.iter().min_by_key(|(_, p)| p.last_resp).map(|(c, _)| *c) {
                self.pending.remove(&old);
            }
        }
        let takeover = self.may_take_over(&init);
        let p = self.pending.entry(cid).or_insert_with(|| Pending {
            client_pub,
            cands: AttemptRing::new(),
            last_resp: now,
            attempts: 0,
        });
        p.cands.push(pn, now, Cand { keys, resp_at: now, init, takeover });
        p.last_resp = now;
        p.attempts = p.attempts.saturating_add(1);
    }

    fn may_take_over(&self, init: &HsInitPayload) -> bool {
        self.takeover == TakeoverPolicy::Auto || init.mode.is_some_and(|(_, f)| f & mode_flags::TAKEOVER != 0)
    }

    /// HS_RESP 的 err：版本 → CID 冲突 → BUSY → OK
    fn decide(&self, cid: &Cid, client_pub: &[u8; 32], init: &HsInitPayload) -> HsErr {
        if init.ver < PROTO_MIN_VERSION || init.min_ver > PROTO_VERSION || init.caps & caps::SPLIT2 == 0 {
            return HsErr::Version;
        }
        let other_owner = |c: &GwConn| c.s.cid == *cid && c.client_pub != *client_pub;
        if self.pending.get(cid).is_some_and(|p| p.client_pub != *client_pub)
            || self.conn.as_ref().is_some_and(other_owner)
            || self.retiring.iter().any(other_owner)
        {
            return HsErr::CidCollision;
        }
        if self.conn.as_ref().is_some_and(|c| c.client_pub != *client_pub) && !self.may_take_over(init) {
            return HsErr::Busy;
        }
        HsErr::Ok
    }

    fn hs_allow(&mut self, now: Micros, from: SocketAddr) -> bool {
        if self.hs_rate.len() > RATE_TABLE_PRUNE {
            self.hs_rate.retain(|_, (t, _)| now < *t + secs(1));
        }
        let e = self.hs_rate.entry(prefix_of(from.ip())).or_insert((now, 0));
        if now >= e.0 + secs(1) {
            *e = (now, 0);
        }
        if e.1 >= HS_PER_PREFIX_PER_SEC {
            return false;
        }
        e.1 += 1;
        true
    }

    // ------------------------------------------------------------------
    // DATA
    // ------------------------------------------------------------------

    fn on_data(
        &mut self,
        now: Micros,
        from: SocketAddr,
        hdr: &OuterHeader,
        pkt: &mut [u8],
        sink: &mut dyn FnMut(GwEvent<'_>),
    ) {
        let cid = hdr.cid;
        // 1. 已确认的会话
        if let Some(conn) = self.conn.as_mut().filter(|c| c.s.cid == cid) {
            if hdr.path_id != conn.s.path_id {
                return emit_drop(&mut conn.s.stats.drops, DropReason::UnknownPath, sink);
            }
            match conn.s.open(pkt) {
                Ok(meta) => return self.on_conn_packet(now, from, meta, pkt, sink),
                // 同一 CID 又握了一次手（客户端很少这样做）：下面试候选
                Err(DropReason::AeadFail) if self.pending.contains_key(&cid) => {}
                Err(r) => return emit_drop(&mut conn.s.stats.drops, r, sink),
            }
        }
        // 2. 握手候选：从新到旧逐份原地试解（AEAD 失败时 pkt 不变）
        if let Some(p) = self.pending.get(&cid) {
            if hdr.path_id != 0 {
                return emit_drop(&mut self.drops, DropReason::UnknownPath, sink);
            }
            let mut hit = None;
            let mut fail = DropReason::AeadFail;
            for (a, c) in p.cands.iter_live(now) {
                let mut rx = PathRx::new();
                match try_open(pkt, &c.keys.rx, &mut rx) {
                    Ok(meta) => {
                        hit = Some((a, rx, meta));
                        break;
                    }
                    Err(DropReason::AeadFail) => {}
                    Err(r) => {
                        fail = r;
                        break;
                    }
                }
            }
            return match hit {
                Some((a, rx, meta)) => self.confirm(now, from, cid, a, rx, meta, pkt, sink),
                None => emit_drop(&mut self.drops, fail, sink),
            };
        }
        // 3. 正在发 CLOSE 的旧会话
        if self.retiring.iter().any(|c| c.s.cid == cid) {
            return emit_drop(&mut self.drops, DropReason::SessionClosed, sink);
        }
        // 4. 未知 CID：够长才回 RESET（规格 §4.4）
        emit_drop(&mut self.drops, DropReason::UnknownCid, sink);
        if pkt.len() < RESET_TRIGGER_MIN {
            return emit_drop(&mut self.drops, DropReason::ResetTriggerShort, sink);
        }
        if !self.reset_allow(now, from) {
            return emit_drop(&mut self.drops, DropReason::ResetRateLimited, sink);
        }
        let token = reset_token(self.reset_key.expose(), cid);
        let mut b = vec![0u8; RESET_LEN];
        encode_reset(cid, hdr.path_id, &token, &mut b);
        self.raw.push_back((from, b));
    }

    fn reset_allow(&mut self, now: Micros, from: SocketAddr) -> bool {
        if self.reset_src.get(&from).is_some_and(|t| now < t + RESET_PER_SRC_GAP) {
            return false;
        }
        if now >= self.reset_global.0 + secs(1) {
            self.reset_global = (now, 0);
        }
        if self.reset_global.1 >= RESET_GLOBAL_PER_SEC {
            return false;
        }
        self.reset_global.1 += 1;
        if self.reset_src.len() > RATE_TABLE_PRUNE {
            self.reset_src.retain(|_, t| now < *t + RESET_PER_SRC_GAP);
        }
        self.reset_src.insert(from, now);
        true
    }

    /// 第一个认证通过的 DATA：密钥确认，会话建立；需要时接管旧会话（规格 §3.1、§4.3）
    #[allow(clippy::too_many_arguments)]
    fn confirm(
        &mut self,
        now: Micros,
        from: SocketAddr,
        cid: Cid,
        attempt: u32,
        rx: PathRx,
        meta: OpenedMeta,
        pkt: &mut [u8],
        sink: &mut dyn FnMut(GwEvent<'_>),
    ) {
        let Some(mut p) = self.pending.remove(&cid) else { return };
        let Some(cand) = p.cands.take(attempt, now) else { return };
        let client_pub = p.client_pub;
        let mut replaced = None;
        if let Some(mut old) = self.conn.take() {
            if old.client_pub == client_pub {
                // 同一客户端身份：旧会话静默作废
                replaced = Some(old.s.cid);
            } else if self.takeover == TakeoverPolicy::Auto || cand.takeover {
                replaced = Some(old.s.cid);
                old.s.start_close(close_code::TAKEN_OVER, close_text(close_code::TAKEN_OVER), now);
                self.retiring.push(old);
            } else {
                // 握手之后另一台客户端先确认了：这份候选作废（客户端 1 s 没回音会重握，届时回 BUSY）
                self.conn = Some(old);
                return emit_drop(&mut self.drops, DropReason::NotConfirmed, sink);
            }
        }
        let mut s = Sess::new(cid, Dir::Down, cand.keys, rx, from, now);
        s.rtt.seed(now.saturating_sub(cand.resp_at));
        s.stats.hs_attempts = p.attempts;
        let client = ClientId::of(&client_pub);
        self.conn = Some(GwConn { s, client, client_pub, init: cand.init.clone() });
        if let Some(old) = replaced {
            sink(GwEvent::Replaced { old, new: cid });
        }
        sink(GwEvent::Connected { cid, client, init: cand.init });
        self.on_conn_packet(now, from, meta, pkt, sink);
    }

    /// 已确认会话收到的一个包（已解密）
    fn on_conn_packet(
        &mut self,
        now: Micros,
        from: SocketAddr,
        meta: OpenedMeta,
        pkt: &mut [u8],
        sink: &mut dyn FnMut(GwEvent<'_>),
    ) {
        let Some(conn) = self.conn.as_mut() else { return };
        let s = &mut conn.s;
        if from != s.peer {
            if !meta.new_largest {
                return emit_drop(&mut s.stats.drops, DropReason::NotPeer, sink);
            }
            // NAT 重绑定：认证通过、PN 是新的，切到新地址（M2a 不做放大限制，见模块注释）
            s.peer = from;
            s.queue(Small::PathChallenge(crate::crypto::random_bytes()), now);
            sink(GwEvent::PathChanged(from));
        }
        s.on_rx(now, pkt.len());
        let rx_meta = RxMeta { pn: meta.pn, send_ts: meta.send_ts, arrival: now };
        let mut closed = None;
        for item in ChunkIter::new(chunk_bytes(pkt)) {
            let c = match item {
                Ok(c) => c,
                Err(e) => {
                    emit_drop(&mut s.stats.drops, e.reason, sink);
                    continue;
                }
            };
            match s.common(c, meta.send_ts, now) {
                Common::Done | Common::Rtt(_) => {}
                Common::Close(code) => closed = Some(code),
                Common::Other(Chunk::Input(i)) => sink(GwEvent::Input(i, rx_meta)),
                Common::Other(Chunk::Report(r)) => sink(GwEvent::Report(r)),
                Common::Other(Chunk::Need(n)) => sink(GwEvent::Need(n)),
                // NACK 要 SentLog 换算成 (fid, sym)，M2c 再接
                Common::Other(Chunk::Nack(_)) => emit_drop(&mut s.stats.drops, DropReason::Unsupported, sink),
                Common::Other(_) => emit_drop(&mut s.stats.drops, DropReason::WrongDirection, sink),
            }
            while let Some(m) = s.pop_control() {
                sink(GwEvent::Control(m));
            }
        }
        if let Some(code) = closed {
            // 对端已经丢弃密钥，不必回 CLOSE
            self.conn = None;
            sink(GwEvent::Closed(code));
        }
    }

    // ------------------------------------------------------------------
    // 计时器与发送
    // ------------------------------------------------------------------

    fn retire_conn(&mut self, code: u16, now: Micros) {
        if let Some(mut c) = self.conn.take() {
            c.s.start_close(code, close_text(code), now);
            self.retiring.push(c);
        }
    }

    pub fn handle_timeout(&mut self, now: Micros, sink: &mut dyn FnMut(GwEvent<'_>)) {
        self.last_now = self.last_now.max(now);
        // 握手候选 5 s 过期；最后一次 HS_RESP 之后 3 s 没有 DATA 的半开会话删掉
        self.pending.retain(|_, p| {
            p.cands.expire(now);
            !p.cands.is_empty() && now < p.last_resp + HALF_OPEN_TIMEOUT
        });
        self.resp_cache.retain(|c| now < c.at + RESP_CACHE_TTL);
        // 60 s 没收到客户端的包：直接丢掉会话，不发 CLOSE。可能只是上行断了而下行还通，
        // 发 CLOSE(4) 会让客户端永久停下；不发的话上行恢复后它的包换回 RESET，按 §4.4 带 resume 重握，
        // 上行一直不通时客户端也会在下行停 30 s 后自己重握（规格 §13.2、§13.4）
        if self.conn.as_ref().is_some_and(|c| now >= c.s.last_rx + SESSION_IDLE_TIMEOUT) {
            self.conn = None;
            sink(GwEvent::Closed(close_code::GATEWAY_ENDED));
        }
        self.retiring.retain(|c| !c.s.close_done());
    }

    pub fn poll_transmit(
        &mut self,
        now: Micros,
        video: &dyn SymbolSource,
        out: &mut [u8; BASE_PAYLOAD],
    ) -> Option<Transmit> {
        self.last_now = self.last_now.max(now);
        if let Some((dst, b)) = self.raw.pop_front() {
            let len = b.len().min(out.len());
            out[..len].copy_from_slice(&b[..len]);
            return Some(Transmit { dst, len, video: None });
        }
        for c in self.retiring.iter_mut() {
            if let Some(t) = c.s.build(now, BASE_PAYLOAD, None, &mut out[..]) {
                return Some(t);
            }
        }
        self.retiring.retain(|c| !c.s.close_done());
        let c = self.conn.as_mut()?;
        let media = c.media_allowed();
        c.s.build(now, BASE_PAYLOAD, if media { Some(video) } else { None }, &mut out[..])
    }

    pub fn next_timeout(&self) -> Option<Micros> {
        let mut t: Option<Micros> = None;
        let mut take = |x: Micros| t = Some(t.map_or(x, |y| y.min(x)));
        if !self.raw.is_empty() {
            take(0);
        }
        for p in self.pending.values() {
            take(p.last_resp + HALF_OPEN_TIMEOUT);
            if let Some(x) = p.cands.next_expiry() {
                take(x);
            }
        }
        if let Some(c) = &self.conn {
            take(c.s.last_rx + SESSION_IDLE_TIMEOUT);
            if let Some(x) = c.s.next_deadline() {
                take(x);
            }
        }
        for c in &self.retiring {
            if let Some(x) = c.s.next_deadline() {
                take(x);
            }
        }
        t
    }

    /// v1 只有一个控制客户端
    pub fn conn(&mut self) -> Option<&mut GwConn> {
        self.conn.as_mut()
    }

    /// 踢掉某个客户端：给它发 3 份 CLOSE(code) 后丢弃密钥
    pub fn kick(&mut self, client: &ClientId, code: u16) {
        self.pending.retain(|_, p| ClientId::of(&p.client_pub) != *client);
        if self.conn.as_ref().is_some_and(|c| c.client == *client) {
            let now = self.last_now;
            self.retire_conn(code, now);
        }
    }
}

/// 网关上一个已建立的会话
#[derive(Debug)]
pub struct GwConn {
    s: Sess,
    client: ClientId,
    client_pub: [u8; 32],
    init: HsInitPayload,
}

impl GwConn {
    /// 密钥已确认，并且不在关闭（Held 在 M2d）
    pub fn media_allowed(&self) -> bool {
        !self.s.is_closing()
    }

    pub fn cid(&self) -> Cid {
        self.s.cid
    }

    pub fn client(&self) -> ClientId {
        self.client
    }

    /// 当前对端地址
    pub fn peer(&self) -> SocketAddr {
        self.s.peer
    }

    /// 确认的那次尝试的 HS_INIT 载荷（名字、模式、码率上限、resume……）
    pub fn init(&self) -> &HsInitPayload {
        &self.init
    }

    /// 视频符号入队（正文在组包时经 SymbolSource 取）
    pub fn queue_video(&mut self, v: VideoCarrier) {
        if self.media_allowed() {
            self.s.sched.push_video(v);
        }
    }

    /// M2b：音频通道。M2a 不发声音（计划 §3.1），这里不发
    pub fn queue_audio(&mut self, opus: Option<&[u8]>, arrival: Micros, now: Micros) {
        let _ = (opus, arrival, now);
    }

    /// M2b：反馈通道。M2a 不转发手柄（计划 §3.1），这里不发
    pub fn set_feedback(&mut self, s: FeedbackState, now: Micros) {
        let _ = (s, now);
    }

    /// 连续 3 个包各带一次（立即可发：没有能搭载的包时单独发）
    pub fn frame_status(&mut self, e: FrameStatusEntry) {
        self.s.queue_opts(Small::FrameStatus(e), SmallOpts { not_before: 0, deadline: None, repeat: FRAME_STATUS_REPEAT });
    }

    pub fn send_control(&mut self, m: &ControlMsg) -> Result<(), CtlError> {
        self.s.send_control(m)
    }

    pub fn rtt(&self) -> &RttEstimator {
        &self.s.rtt
    }

    /// 距上次收到任何包多久
    pub fn silence(&self, now: Micros) -> Micros {
        now.saturating_sub(self.s.last_rx)
    }

    pub fn stats(&self) -> ConnStats {
        self.s.snapshot_stats()
    }
}
