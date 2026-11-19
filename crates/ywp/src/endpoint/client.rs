//! 客户端端点（B 组）。
//!
//! 握手（规格 §4.3、§4.8）：
//! - 首包双发（间隔 10 ms，同一个包），之后 250 ms、500 ms、1 s、2 s 重传，封顶 2 s，直到成功或取消；
//! - 同一次连接的多次尝试共用 CID，PN = 尝试序号；每次都是新的临时密钥和新的时间戳：
//!   ts = ts0 + (now − now0)，ts0 由调用方读一次墙钟传进 connect，这里不读墙钟；
//! - 保留最近 5 次尝试的状态，5 s 过期；HS_RESP 按回显的 PN 找到对应尝试，最先到的那份胜出，其余忽略；
//! - CID_COLLISION 换一个 CID 立即重来；BUSY、VERSION 报 HandshakeRejected 后停下。
//!
//! 会话（规格 §4.3、§13.1、附录 B）：
//! - 握手完成后立即发第一个 DATA：PING + PATH_CHALLENGE + REPORT；
//! - 收到第一个认证通过的 DATA 才算密钥确认（Established）；发出第一个 DATA 后 1 s 没有回音就重新握手；
//! - 30 s 没有任何包放弃旧密钥、重新握手；收到有效 RESET（38 B、当前 CID、令牌对）立即带 resume 重新握手；
//! - 收到 CLOSE(5)（网关进程退出）按握手节奏重连；其余 CLOSE 报 Closed 后停下；
//! - PING 每 100 ms；REPORT 每 50 ms（M2a；M2b 起改成 20 ms 并搭输入包，见 time::REPORT_INTERVAL）；
//! - 只接收入口地址发来的包。

use super::session::{chunk_bytes, close_text, Common, Sess, Small};
use super::{AttemptRing, ConnStats, HsSchedule, RxMeta, Transmit};
use crate::consts::{
    close_code, hs_tlv, mode, mode_flags, BASE_PAYLOAD, HS_INIT_LEN, PN_REHANDSHAKE, RECV_MAX, UPLINK_MAX,
};
use crate::creds::ClientCreds;
use crate::crypto::{InitiatorAttempt, Tai64N};
use crate::error::{DropCounters, DropReason};
use crate::msg::ControlMsg;
use crate::packet::{check_reset, header_drop_reason, PathRx};
use crate::reliable::CtlError;
use crate::sched::Dir;
use crate::time::{ms, Micros, FIRST_DATA_NO_REPLY, KEY_ABANDON_SILENCE, SESSION_MAX_AGE};
use crate::wire::{
    AudioChunk, Chunk, ChunkIter, Cid, FeedbackState, FrameStatusEntry, HsErr, HsInitPayload, HsRespPayload,
    InputState, Need, OuterHeader, PacketType, PathReport, ReportOwned, ReportVideoFields, UpReport, VideoChunk,
};
use std::collections::VecDeque;
use std::net::SocketAddr;

/// M2a 的 REPORT 周期（M2b 改成 time::REPORT_INTERVAL = 20 ms 并搭输入包）
const REPORT_PERIOD_M2A: Micros = ms(50);

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub creds: ClientCreds,
    /// 网关入口地址（从客户端配置读）
    pub entry: SocketAddr,
    /// 客户端名称，写进 TLV 0x07（超过 32 B 按字符边界截断）
    pub name: String,
    pub bitrate_cap_kbps: Option<u32>,
    /// 带接管位（用户在 BUSY 确认框里点了「接管」）
    pub takeover: bool,
}

/// 客户端事件（经 sink 回调交给调用方；Video / Audio 借用收包缓冲）
#[derive(Debug)]
pub enum ClientEvent<'a> {
    HandshakeOk(HsRespPayload),
    /// BUSY、VERSION：握手停下，等调用方决定（比如问用户要不要接管）
    HandshakeRejected(HsErr),
    /// 密钥已确认（收到第一个认证通过的 DATA）
    Established,
    Video(VideoChunk<'a>, RxMeta),
    Audio(AudioChunk<'a>, RxMeta),
    Feedback(FeedbackState),
    FrameStatus(FrameStatusEntry),
    UpReport(UpReport<'a>),
    Control(ControlMsg),
    Rtt(Micros),
    /// 网关发来 CLOSE。code = 5 时会自动按握手节奏重连，其余停下
    Closed { code: u16 },
    /// 收到有效 RESET，已立即重新握手
    Reset,
    Dropped(DropReason),
}

/// 一次握手尝试
#[derive(Debug)]
struct Attempt {
    st: InitiatorAttempt,
    /// 第一份发出的时刻（握手 RTT 的起点）
    sent_at: Micros,
    /// 包本身（首包的第二份原样重发）
    pkt: Vec<u8>,
}

#[derive(Debug)]
enum Phase {
    /// 没有连接（还没 connect，或被拒、被关、调用方 close 了）
    Idle,
    /// 发 HS_INIT、等 HS_RESP
    Handshaking { sched: HsSchedule },
    /// 有了传输密钥
    Session { sess: Box<Sess>, confirmed: bool, first_data_at: Option<Micros>, reset_token: [u8; 16] },
}

#[derive(Debug)]
pub struct ClientConn {
    cfg: ClientConfig,
    phase: Phase,
    cid: Cid,
    attempts: AttemptRing<Attempt>,
    /// connect 时的载荷模板
    init: Option<HsInitPayload>,
    ts0: Tai64N,
    now0: Micros,
    last_ts: Option<Tai64N>,
    /// 要接回的 PS5 会话（HS_RESP 的 0x12 或 SESSION_STATUS 给的 host_id、epoch）
    resume: Option<([u8; 8], u32)>,
    video_report: ReportVideoFields,
    rep_seq: u16,
    next_report: Micros,
    /// 待发的 HS_INIT（握手阶段的包不走会话调度）
    raw: VecDeque<Vec<u8>>,
    /// 调用方 close 之后还在发 CLOSE 的旧会话
    closing: Option<Box<Sess>>,
    hs_attempts: u32,
    /// 没有会话时的丢弃计数；建会话时整个挪进会话（stats().drops 一处看全），会话结束再挪回来
    drops: DropCounters,
    /// 最近一次调用传进来的 now（close、send_need 不带 now）
    last_now: Micros,
}

impl ClientConn {
    pub fn new(cfg: ClientConfig) -> Self {
        Self {
            cfg,
            phase: Phase::Idle,
            cid: Cid(0),
            attempts: AttemptRing::new(),
            init: None,
            ts0: Tai64N::from_unix(0, 0),
            now0: 0,
            last_ts: None,
            resume: None,
            video_report: ReportVideoFields::default(),
            rep_seq: 0,
            next_report: 0,
            raw: VecDeque::new(),
            closing: None,
            hs_attempts: 0,
            drops: DropCounters::new(),
            last_now: 0,
        }
    }

    /// 开始握手。init 里的 ts 由调用方读墙钟一次（Tai64N::next_after）后传入；之后每次重传都是新尝试，
    /// 时间戳 = ts0 + (now − now0)，按单调时钟推算、保证递增，ClientConn 自己不读墙钟（sans-IO）
    pub fn connect(&mut self, now: Micros, init: HsInitPayload) {
        self.last_now = self.last_now.max(now);
        let ts0 = match self.last_ts {
            Some(l) if init.ts <= l => l.plus_nanos(1),
            _ => init.ts,
        };
        self.ts0 = ts0;
        self.now0 = now;
        if init.resume.is_some() {
            self.resume = init.resume;
        }
        self.init = Some(init);
        self.hs_attempts = 0;
        self.start_handshake(now);
    }

    /// BUSY 确认框里用户选了接管：之后的握手带接管位
    pub fn set_takeover(&mut self, on: bool) {
        self.cfg.takeover = on;
    }

    /// 已建立（密钥已确认）
    pub fn is_established(&self) -> bool {
        matches!(self.phase, Phase::Session { confirmed: true, .. })
    }

    pub fn cid(&self) -> Cid {
        self.cid
    }

    /// 新 CID、新的一轮握手（不清 hs_attempts：一次 connect 里一共用了几次 HS_INIT）
    fn start_handshake(&mut self, now: Micros) {
        if self.init.is_none() {
            self.phase = Phase::Idle;
            return;
        }
        self.cid = Cid(u64::from_be_bytes(crate::crypto::random_bytes()));
        self.attempts.clear();
        self.raw.clear();
        self.end_session();
        self.phase = Phase::Handshaking { sched: HsSchedule::start(now) };
    }

    /// 丢掉当前会话（密钥），丢弃计数挪回来
    fn end_session(&mut self) -> Option<Box<Sess>> {
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Session { mut sess, .. } => {
                self.drops = std::mem::take(sess.drops());
                Some(sess)
            }
            other => {
                self.phase = other;
                None
            }
        }
    }

    /// 这次尝试的载荷：模板 + 时间戳 + 名字、码率上限、接管位、resume
    fn payload(&mut self, now: Micros) -> Option<HsInitPayload> {
        let mut p = self.init.clone()?;
        let mut ts = self.ts0.plus_micros(now.saturating_sub(self.now0));
        if let Some(l) = self.last_ts {
            if ts <= l {
                ts = l.plus_nanos(1);
            }
        }
        self.last_ts = Some(ts);
        p.ts = ts;
        if p.name.is_none() && !self.cfg.name.is_empty() {
            p.name = Some(truncate_utf8(&self.cfg.name, hs_tlv::NAME_MAX).to_owned());
        }
        if p.bitrate_cap_kbps.is_none() {
            p.bitrate_cap_kbps = self.cfg.bitrate_cap_kbps;
        }
        if self.cfg.takeover {
            let (m, f) = p.mode.unwrap_or((mode::COMPETITIVE, 0));
            p.mode = Some((m, f | mode_flags::TAKEOVER));
        }
        if p.resume.is_none() {
            p.resume = self.resume;
        }
        Some(p)
    }

    /// 握手节奏到期的发送
    fn drive_handshake(&mut self, now: Micros) {
        loop {
            let Phase::Handshaking { sched } = &mut self.phase else { return };
            let Some(send) = sched.poll(now) else { return };
            if send.copy {
                if let Some(a) = self.attempts.get(send.attempt, now) {
                    self.raw.push_back(a.pkt.clone());
                }
                continue;
            }
            let Some(payload) = self.payload(now) else { return };
            let c = &self.cfg.creds;
            let mut buf = [0u8; HS_INIT_LEN];
            match InitiatorAttempt::start(&c.static_kp, &c.gw_pub, &c.psk, self.cid, send.attempt, &payload, &mut buf) {
                Ok((st, len)) => {
                    let pkt = buf[..len].to_vec();
                    self.raw.push_back(pkt.clone());
                    self.attempts.push(send.attempt, now, Attempt { st, sent_at: now, pkt });
                    self.hs_attempts = self.hs_attempts.saturating_add(1);
                }
                // 载荷放不下或不合法（name 已截断，正常不会发生）：计数，等下一次
                Err(_) => self.drops.bump(DropReason::HsBadTlv),
            }
        }
    }

    pub fn handle_datagram(
        &mut self,
        now: Micros,
        from: SocketAddr,
        pkt: &mut [u8],
        sink: &mut dyn FnMut(ClientEvent<'_>),
    ) {
        self.last_now = self.last_now.max(now);
        if pkt.len() > RECV_MAX {
            return self.drop(DropReason::TooLong, sink);
        }
        if from != self.cfg.entry {
            return self.drop(DropReason::NotPeer, sink);
        }
        let hdr = match OuterHeader::decode(pkt) {
            Ok(h) => h,
            Err(e) => return self.drop(header_drop_reason(e), sink),
        };
        match hdr.ptype {
            PacketType::HsResp => self.on_hs_resp(now, &hdr, pkt, sink),
            PacketType::Data => self.on_data(now, &hdr, pkt, sink),
            PacketType::Reset => self.on_reset(now, pkt, sink),
            PacketType::HsInit => self.drop(DropReason::WrongDirection, sink),
            _ => self.drop(DropReason::Unsupported, sink),
        }
    }

    fn drop(&mut self, r: DropReason, sink: &mut dyn FnMut(ClientEvent<'_>)) {
        match &mut self.phase {
            Phase::Session { sess, .. } => sess.drops().bump(r),
            _ => self.drops.bump(r),
        }
        sink(ClientEvent::Dropped(r));
    }

    fn on_hs_resp(&mut self, now: Micros, hdr: &OuterHeader, pkt: &[u8], sink: &mut dyn FnMut(ClientEvent<'_>)) {
        if !matches!(self.phase, Phase::Handshaking { .. }) || hdr.cid != self.cid {
            return self.drop(DropReason::HsUnexpected, sink);
        }
        let Some(a) = self.attempts.get_mut(hdr.pn_trunc, now) else {
            return self.drop(DropReason::HsUnexpected, sink);
        };
        let (resp, keys) = match a.st.read_resp(pkt) {
            Ok(x) => x,
            Err(r) => {
                if r == DropReason::HsBadTlv {
                    // Noise 解开了但载荷坏了：这次尝试作废，别的尝试照常等
                    let _ = self.attempts.take(hdr.pn_trunc, now);
                }
                return self.drop(r, sink);
            }
        };
        let sent_at = a.sent_at;
        self.attempts.clear();
        match resp.err {
            HsErr::Ok => {}
            HsErr::CidCollision => {
                // 换一个 CID 立即重来
                self.start_handshake(now);
                return;
            }
            e => {
                self.phase = Phase::Idle;
                sink(ClientEvent::HandshakeRejected(e));
                return;
            }
        }
        let Some(token) = resp.reset_token else {
            self.drops.bump(DropReason::HsBadTlv);
            return;
        };
        if let Some(si) = resp.session {
            self.resume = Some((si.host_id, si.epoch));
        }
        let mut sess = Sess::new(self.cid, Dir::Up, keys, PathRx::new(), self.cfg.entry, now);
        sess.rtt.seed(now.saturating_sub(sent_at));
        sess.stats.hs_attempts = self.hs_attempts;
        *sess.drops() = std::mem::take(&mut self.drops);
        // 第一个 DATA：PING + PATH_CHALLENGE + REPORT（规格 §4.3）
        sess.ping_at(now);
        sess.queue(Small::PathChallenge(crate::crypto::random_bytes()), now);
        self.next_report = now;
        self.phase = Phase::Session { sess: Box::new(sess), confirmed: false, first_data_at: None, reset_token: token };
        sink(ClientEvent::HandshakeOk(resp));
    }

    fn on_reset(&mut self, now: Micros, pkt: &[u8], sink: &mut dyn FnMut(ClientEvent<'_>)) {
        let Phase::Session { reset_token, .. } = &self.phase else {
            return self.drop(DropReason::ResetBadCid, sink);
        };
        if let Err(r) = check_reset(pkt, self.cid, reset_token) {
            return self.drop(r, sink);
        }
        // 网关丢了这个会话（比如进程重启）：立即带 resume 重新握手（规格 §4.4、§13.4）
        self.start_handshake(now);
        sink(ClientEvent::Reset);
    }

    fn on_data(&mut self, now: Micros, hdr: &OuterHeader, pkt: &mut [u8], sink: &mut dyn FnMut(ClientEvent<'_>)) {
        let Phase::Session { sess, confirmed, .. } = &mut self.phase else {
            return self.drop(DropReason::NotConfirmed, sink);
        };
        if hdr.cid != sess.cid {
            sess.drops().bump(DropReason::UnknownCid);
            return sink(ClientEvent::Dropped(DropReason::UnknownCid));
        }
        if hdr.path_id != sess.path_id {
            sess.drops().bump(DropReason::UnknownPath);
            return sink(ClientEvent::Dropped(DropReason::UnknownPath));
        }
        let meta = match sess.open(pkt) {
            Ok(m) => m,
            Err(r) => {
                sess.drops().bump(r);
                return sink(ClientEvent::Dropped(r));
            }
        };
        sess.on_rx(now, pkt.len());
        if !*confirmed {
            *confirmed = true;
            sink(ClientEvent::Established);
        }
        let rx_meta = RxMeta { pn: meta.pn, send_ts: meta.send_ts, arrival: now };
        let mut closed = None;
        for item in ChunkIter::new(chunk_bytes(pkt)) {
            let c = match item {
                Ok(c) => c,
                Err(e) => {
                    sess.drops().bump(e.reason);
                    sink(ClientEvent::Dropped(e.reason));
                    continue;
                }
            };
            match sess.common(c, meta.send_ts, now) {
                Common::Done => {}
                Common::Rtt(r) => sink(ClientEvent::Rtt(r)),
                Common::Close(code) => closed = Some(code),
                Common::Other(Chunk::Video(v)) => sink(ClientEvent::Video(v, rx_meta)),
                Common::Other(Chunk::Audio(a)) => sink(ClientEvent::Audio(a, rx_meta)),
                Common::Other(Chunk::Feedback(f)) => sink(ClientEvent::Feedback(f)),
                Common::Other(Chunk::FrameStatus(l)) => {
                    for _ in 0..l.bad_kinds() {
                        sess.drops().bump(DropReason::FrameStatusBadKind);
                    }
                    for e in l.iter() {
                        sink(ClientEvent::FrameStatus(e));
                    }
                }
                Common::Other(Chunk::UpReport(u)) => sink(ClientEvent::UpReport(u)),
                Common::Other(_) => {
                    sess.drops().bump(DropReason::WrongDirection);
                    sink(ClientEvent::Dropped(DropReason::WrongDirection));
                }
            }
            while let Some(m) = sess.pop_control() {
                if let ControlMsg::SessionStatus(st) = &m {
                    if let (Some(h), Some(e)) = (st.host_id, st.epoch) {
                        self.resume = Some((h, e));
                    }
                }
                sink(ClientEvent::Control(m));
            }
        }
        if let Some(code) = closed {
            sink(ClientEvent::Closed { code });
            if code == close_code::GATEWAY_EXIT {
                // 网关进程退出、可能马上重启：按握手节奏重连（规格 §5.5.8）
                self.start_handshake(now);
            } else {
                self.end_session();
            }
        }
    }

    pub fn handle_timeout(&mut self, now: Micros, sink: &mut dyn FnMut(ClientEvent<'_>)) {
        self.last_now = self.last_now.max(now);
        self.attempts.expire(now);
        if self.closing.as_ref().is_some_and(|s| s.close_done()) {
            self.closing = None;
        }
        let Phase::Session { sess, confirmed, first_data_at, .. } = &mut self.phase else { return };
        let _ = sink;
        // 发出第一个 DATA 后 1 s 没有回音；30 s 没有任何包；PN 用到 2³¹ 或会话超过 24 h：重新握手
        let no_reply = !*confirmed && first_data_at.is_some_and(|t| now >= t + FIRST_DATA_NO_REPLY);
        let silent = *confirmed && now >= sess.last_rx + KEY_ABANDON_SILENCE;
        let worn = sess.tx_pn() >= PN_REHANDSHAKE || now >= sess.created + SESSION_MAX_AGE;
        if no_reply || silent || worn {
            self.start_handshake(now);
        }
    }

    /// 上行 ≤ 400 B（HS_INIT 1232 B 除外）
    pub fn poll_transmit(&mut self, now: Micros, out: &mut [u8; BASE_PAYLOAD]) -> Option<Transmit> {
        self.last_now = self.last_now.max(now);
        if let Some(s) = self.closing.as_mut() {
            if let Some(t) = s.build(now, UPLINK_MAX, None, &mut out[..]) {
                return Some(t);
            }
            if s.close_done() {
                self.closing = None;
            }
        }
        self.drive_handshake(now);
        if let Some(p) = self.raw.pop_front() {
            let len = p.len().min(out.len());
            out[..len].copy_from_slice(&p[..len]);
            return Some(Transmit { dst: self.cfg.entry, len, video: None });
        }
        let Phase::Session { sess, first_data_at, .. } = &mut self.phase else { return None };
        if now >= self.next_report {
            self.next_report = now + REPORT_PERIOD_M2A;
            let (largest, recv, lost) = sess.loss_counts();
            let rep = ReportOwned {
                rep_seq: self.rep_seq,
                video: self.video_report,
                paths: vec![PathReport {
                    path_id: sess.path_id,
                    largest_pn: largest,
                    recv_cnt: recv,
                    lost_cnt: lost,
                    ..PathReport::default()
                }],
            };
            self.rep_seq = self.rep_seq.wrapping_add(1);
            sess.queue(Small::Report(rep), now);
        }
        let t = sess.build(now, UPLINK_MAX, None, &mut out[..])?;
        if first_data_at.is_none() {
            *first_data_at = Some(now);
        }
        Some(t)
    }

    pub fn next_timeout(&self) -> Option<Micros> {
        let mut t: Option<Micros> = None;
        let mut take = |x: Micros| t = Some(t.map_or(x, |y| y.min(x)));
        if !self.raw.is_empty() {
            take(0);
        }
        if let Some(s) = &self.closing {
            if let Some(x) = s.next_deadline() {
                take(x);
            }
        }
        if let Some(x) = self.attempts.next_expiry() {
            take(x);
        }
        match &self.phase {
            Phase::Idle => {}
            Phase::Handshaking { sched } => take(sched.next_deadline()),
            Phase::Session { sess, confirmed, first_data_at, .. } => {
                if let Some(x) = sess.next_deadline() {
                    take(x);
                }
                take(self.next_report);
                match (confirmed, first_data_at) {
                    (false, Some(f)) => take(f + FIRST_DATA_NO_REPLY),
                    (true, _) => take(sess.last_rx + KEY_ABANDON_SILENCE),
                    _ => {}
                }
                take(sess.created + SESSION_MAX_AGE);
            }
        }
        t
    }

    /// M2b：输入通道（InputSender）。M2a 不转发手柄（计划 §3.1），这里不发
    pub fn input(&mut self, now: Micros, s: InputState, discrete: &[(u8, u8)]) {
        let _ = (now, s, discrete);
    }

    pub fn send_control(&mut self, m: &ControlMsg) -> Result<(), CtlError> {
        match &mut self.phase {
            Phase::Session { sess, .. } => sess.send_control(m),
            _ => Err(CtlError::NotReady),
        }
    }

    /// NEED（M2c 的 L2 迟到修复用；现在就能发）
    pub fn send_need(&mut self, n: Need) {
        let now = self.last_now;
        if let Phase::Session { sess, .. } = &mut self.phase {
            sess.queue(Small::Need(n), now);
        }
    }

    /// REPORT 的视频字段（Assembler::report 产出）
    pub fn set_video_report(&mut self, v: ReportVideoFields) {
        self.video_report = v;
    }

    /// 在 0、5、10 ms 各发一份 CLOSE，然后丢弃密钥。握手中途调用时直接停下
    pub fn close(&mut self, code: u16, reason: &str) {
        self.attempts.clear();
        self.raw.clear();
        self.init = None;
        let now = self.last_now;
        let sess = self.end_session();
        self.phase = Phase::Idle;
        if let Some(mut sess) = sess {
            let reason = if reason.is_empty() { close_text(code) } else { reason };
            sess.start_close(code, reason, now);
            self.closing = Some(sess);
        }
    }

    pub fn stats(&self) -> ConnStats {
        let mut s = match &self.phase {
            Phase::Session { sess, .. } => sess.snapshot_stats(),
            _ => ConnStats { drops: self.drops.clone(), ..ConnStats::default() },
        };
        s.hs_attempts = self.hs_attempts;
        s
    }
}

/// 按字符边界截到 max 字节
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
