//! 端点的集成测试（B 组）：网关和客户端在内存里对接（testkit 的 Pipe），走真的线上编码和加密。
//!
//! 场景：正常握手与收发；握手首包两份都丢；RTT 尖峰 437 ms（握手时、会话中）；网关状态清空 → RESET →
//! 重新握手；BUSY 与接管；乱序、重复、10% 丢包下 CONTROL 和视频载体送达。
//!
//! 运行：cargo test -p ywp --features testkit,net

#![cfg(feature = "testkit")]

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use ywp::consts::{caps, close_code, ps_state, BASE_PAYLOAD, HS_INIT_LEN, PROTO_MIN_VERSION, PROTO_VERSION, UPLINK_MAX};
use ywp::creds::{ClientCreds, ClientEntry, ClientId, Keys};
use ywp::crypto::{Secret32, StaticKeypair, Tai64N};
use ywp::endpoint::{ClientConfig, ClientConn, ClientEvent, Gateway, GatewayConfig, GwEvent, TakeoverPolicy};
use ywp::msg::{AudioConfig, ControlMsg, SessionStatus};
use ywp::sched::{Prio, SymbolSource, VideoCarrier};
use ywp::testkit::{min_opt, LinkCfg, Pipe};
use ywp::time::{ms, secs, Micros};
use ywp::wire::{FrameStatusEntry, FrameStatusKind, HsErr, HsInitPayload, ReportVideoFields, SessionInfo, VideoHeader};
use ywp::DropReason;

fn gw_addr() -> SocketAddr {
    "192.0.2.1:9420".parse().unwrap()
}

fn client_addr(i: usize) -> SocketAddr {
    format!("192.0.2.{}:{}", 10 + i, 50_000 + i).parse().unwrap()
}

fn client_kp(i: usize) -> StaticKeypair {
    StaticKeypair::from_secret([0x20 + i as u8; 32])
}

fn client_psk(i: usize) -> Secret32 {
    Secret32::new([0x40 + i as u8; 32])
}

fn gw_kp() -> StaticKeypair {
    StaticKeypair::from_secret([0x11; 32])
}

/// 网关的 keys/：两个已配对的客户端
fn gw_cfg(policy: TakeoverPolicy) -> GatewayConfig {
    let clients = (0..2)
        .map(|i| ClientEntry { name: format!("c{i}"), public: client_kp(i).public(), psk: client_psk(i) })
        .collect();
    GatewayConfig { keys: Keys { static_kp: gw_kp(), reset_key: Secret32::new([0x77; 32]), clients }, takeover: policy }
}

fn client_cfg(i: usize, takeover: bool) -> ClientConfig {
    ClientConfig {
        creds: ClientCreds { static_kp: client_kp(i), gw_pub: gw_kp().public(), psk: client_psk(i) },
        entry: gw_addr(),
        name: format!("测试客户端{i}"),
        bitrate_cap_kbps: None,
        takeover,
    }
}

fn init_payload() -> HsInitPayload {
    HsInitPayload {
        ts: Tai64N::from_unix(1_700_000_000, 0),
        ver: PROTO_VERSION,
        min_ver: PROTO_MIN_VERSION,
        caps: caps::SPLIT2 | caps::HEVC,
        resume: None,
        last_fid: None,
        mode: Some((0, 0)),
        name: None,
        bitrate_cap_kbps: None,
    }
}

const HOST_ID: [u8; 8] = [9, 8, 7, 6, 5, 4, 3, 2];
const EPOCH: u32 = 0x1234_0001;

/// 客户端事件的拥有版
#[derive(Clone, Debug, PartialEq)]
enum CEv {
    HsOk,
    Rejected(HsErr),
    Established,
    Video { fid: u16, sym: u16, body: Vec<u8> },
    FrameStatus(FrameStatusEntry),
    Control(ControlMsg),
    Rtt(Micros),
    Closed(u16),
    Reset,
    Dropped(DropReason),
}

/// 网关事件的拥有版
#[derive(Clone, Debug, PartialEq)]
enum GEv {
    Connected { cid: u64, client: ClientId, resume: Option<([u8; 8], u32)>, takeover: bool },
    Replaced { old: u64, new: u64 },
    Report { video: ReportVideoFields, recv: u32 },
    Control(ControlMsg),
    Closed(u16),
    PathChanged,
    Dropped(DropReason),
    Other,
}

fn own_c(e: ClientEvent<'_>) -> CEv {
    match e {
        ClientEvent::HandshakeOk(_) => CEv::HsOk,
        ClientEvent::HandshakeRejected(e) => CEv::Rejected(e),
        ClientEvent::Established => CEv::Established,
        ClientEvent::Video(v, _) => CEv::Video { fid: v.hdr.fid, sym: v.hdr.sym_idx, body: v.body.to_vec() },
        ClientEvent::FrameStatus(f) => CEv::FrameStatus(f),
        ClientEvent::Control(m) => CEv::Control(m),
        ClientEvent::Rtt(r) => CEv::Rtt(r),
        ClientEvent::Closed { code } => CEv::Closed(code),
        ClientEvent::Reset => CEv::Reset,
        ClientEvent::Dropped(r) => CEv::Dropped(r),
        other => panic!("M2a 不该出现的客户端事件：{other:?}"),
    }
}

fn own_g(e: GwEvent<'_>) -> GEv {
    match e {
        GwEvent::Connected { cid, client, init } => GEv::Connected {
            cid: cid.0,
            client,
            resume: init.resume,
            takeover: init.mode.is_some_and(|(_, f)| f & 1 != 0),
        },
        GwEvent::Replaced { old, new } => GEv::Replaced { old: old.0, new: new.0 },
        GwEvent::Report(r) => GEv::Report { video: r.video, recv: r.paths().next().map_or(0, |p| p.recv_cnt) },
        GwEvent::Control(m) => GEv::Control(m),
        GwEvent::Closed(c) => GEv::Closed(c),
        GwEvent::PathChanged(_) => GEv::PathChanged,
        GwEvent::Dropped(r) => GEv::Dropped(r),
        _ => GEv::Other,
    }
}

/// 视频符号的正文（测试里一直留着）
#[derive(Default)]
struct Syms(HashMap<(u16, u16), Vec<u8>>);

impl SymbolSource for Syms {
    fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]> {
        self.0.get(&(fid, sym)).map(|v| v.as_slice())
    }
}

fn sym_body(fid: u16, sym: u16, len: usize) -> Vec<u8> {
    (0..len).map(|i| (fid as usize * 31 + sym as usize * 7 + i) as u8).collect()
}

struct Cl {
    conn: ClientConn,
    addr: SocketAddr,
    up: Pipe,
    down: Pipe,
    ev: Vec<(Micros, CEv)>,
    /// 上行 HS_INIT 的个数
    hs_inits: u32,
    up_packets: u64,
    /// 每个上行包的发出时刻
    up_times: Vec<Micros>,
}

struct World {
    now: Micros,
    gw: Gateway,
    gw_ev: Vec<(Micros, GEv)>,
    cls: Vec<Cl>,
    syms: Syms,
}

impl World {
    fn new(policy: TakeoverPolicy, n_clients: usize, link: LinkCfg, seed: u64) -> Self {
        let mut gw = Gateway::new(gw_cfg(policy));
        gw.set_session_info(SessionInfo { ps_state: ps_state::STREAMING, host_id: HOST_ID, epoch: EPOCH, resumable: true });
        let cls = (0..n_clients)
            .map(|i| Cl {
                conn: ClientConn::new(client_cfg(i, false)),
                addr: client_addr(i),
                up: Pipe::with_cfg(link, seed.wrapping_mul(31).wrapping_add(i as u64 * 2 + 1)),
                down: Pipe::with_cfg(link, seed.wrapping_mul(31).wrapping_add(i as u64 * 2 + 2)),
                ev: Vec::new(),
                hs_inits: 0,
                up_packets: 0,
                up_times: Vec::new(),
            })
            .collect();
        Self { now: 0, gw, gw_ev: Vec::new(), cls, syms: Syms::default() }
    }

    fn connect(&mut self, i: usize) {
        let now = self.now;
        self.cls[i].conn.connect(now, init_payload());
    }

    /// 一轮：收包 → 计时器 → 发包
    fn step(&mut self) {
        let now = self.now;
        let mut buf = [0u8; BASE_PAYLOAD];
        for c in self.cls.iter_mut() {
            while let Some(mut p) = c.up.recv(now) {
                let ev = &mut self.gw_ev;
                self.gw.handle_datagram(now, c.addr, &mut p, &mut |e| ev.push((now, own_g(e))));
            }
            while let Some(mut p) = c.down.recv(now) {
                let ev = &mut c.ev;
                c.conn.handle_datagram(now, gw_addr(), &mut p, &mut |e| ev.push((now, own_c(e))));
            }
        }
        if self.gw.next_timeout().is_some_and(|t| t <= now) {
            let ev = &mut self.gw_ev;
            self.gw.handle_timeout(now, &mut |e| ev.push((now, own_g(e))));
        }
        for c in self.cls.iter_mut() {
            if c.conn.next_timeout().is_some_and(|t| t <= now) {
                let ev = &mut c.ev;
                c.conn.handle_timeout(now, &mut |e| ev.push((now, own_c(e))));
            }
        }
        while let Some(t) = self.gw.poll_transmit(now, &self.syms, &mut buf) {
            assert!(t.len <= BASE_PAYLOAD, "下行包超长：{}", t.len);
            if let Some(c) = self.cls.iter_mut().find(|c| c.addr == t.dst) {
                c.down.send_bytes(now, &buf[..t.len]);
            }
        }
        for c in self.cls.iter_mut() {
            while let Some(t) = c.conn.poll_transmit(now, &mut buf) {
                assert_eq!(t.dst, gw_addr());
                if t.len == HS_INIT_LEN && buf[0] == 0x48 {
                    c.hs_inits += 1;
                } else {
                    assert!(t.len <= UPLINK_MAX, "上行包超长：{}", t.len);
                }
                c.up_packets += 1;
                c.up_times.push(now);
                c.up.send_bytes(now, &buf[..t.len]);
            }
        }
    }

    fn next_event(&self) -> Option<Micros> {
        let mut t = self.gw.next_timeout();
        for c in &self.cls {
            t = min_opt(t, c.conn.next_timeout());
            t = min_opt(t, c.up.next_arrival());
            t = min_opt(t, c.down.next_arrival());
        }
        t
    }

    /// 跑到 until（含）
    fn run_until(&mut self, until: Micros) {
        let mut spins = 0u32;
        loop {
            self.step();
            let next = self.next_event().unwrap_or(until).min(until);
            if next <= self.now {
                if self.now >= until {
                    break;
                }
                spins += 1;
                assert!(spins < 1_000, "计时器空转：now = {} µs", self.now);
                continue;
            }
            spins = 0;
            self.now = next;
        }
    }

    fn run_for(&mut self, d: Micros) {
        let t = self.now + d;
        self.run_until(t);
    }

    fn established_at(&self, i: usize) -> Option<Micros> {
        self.cls[i].ev.iter().find(|(_, e)| *e == CEv::Established).map(|(t, _)| *t)
    }

    fn count_c(&self, i: usize, f: impl Fn(&CEv) -> bool) -> usize {
        self.cls[i].ev.iter().filter(|(_, e)| f(e)).count()
    }

    fn count_g(&self, f: impl Fn(&GEv) -> bool) -> usize {
        self.gw_ev.iter().filter(|(_, e)| f(e)).count()
    }

    /// 网关排一帧的 k 个源符号
    fn queue_frame(&mut self, fid: u16, k: u16, len: usize, key: bool) {
        let conn = self.gw.conn().expect("网关应当已有会话");
        for sym in 0..k {
            let body = sym_body(fid, sym, len);
            self.syms.0.insert((fid, sym), body);
            let hdr = VideoHeader {
                key,
                parity: false,
                last_src: sym + 1 == k,
                repair: false,
                fid,
                sym_idx: sym,
                k,
                r: 0,
                age_100us: 0,
                profile: 0,
                n: 1,
                cfg: 1,
                w: false,
            };
            conn.queue_video(VideoCarrier { hdr, prio: Prio::P4, order: ((fid as u64) << 12) | sym as u64 });
        }
    }
}

fn ideal() -> LinkCfg {
    LinkCfg::ideal(ms(10))
}

#[test]
fn normal_handshake_and_traffic() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 1);
    w.connect(0);
    w.run_until(ms(300));
    let est = w.established_at(0).expect("应当建立");
    // 1 个 RTT 的握手 + 第一个 DATA 往返一次：约 40 ms
    assert!(est <= ms(60), "建立用了 {est} µs");
    assert_eq!(w.cls[0].conn.stats().hs_attempts, 1);
    // 首包双发：同一个包发了两份
    assert_eq!(w.cls[0].hs_inits, 2);
    assert_eq!(w.count_c(0, |e| *e == CEv::HsOk), 1);
    assert_eq!(w.count_g(|e| matches!(e, GEv::Connected { .. })), 1);
    assert!(w.cls[0].conn.is_established());

    // 网关：SESSION_STATUS、KEY_PENDING、一帧视频；客户端：IDR_REQUEST、REPORT 的视频字段
    let status = ControlMsg::SessionStatus(SessionStatus {
        ps_state: ps_state::STREAMING,
        host_id: Some(HOST_ID),
        epoch: Some(EPOCH),
        ..Default::default()
    });
    {
        let conn = w.gw.conn().unwrap();
        conn.send_control(&status).unwrap();
        conn.frame_status(FrameStatusEntry { fid: 5, kind: FrameStatusKind::KeyPending, arg: 0 });
    }
    w.queue_frame(5, 4, 1186, true);
    w.cls[0].conn.send_control(&ControlMsg::IdrRequest { reason: 1 }).unwrap();
    let vf = ReportVideoFields { frames_ok: 42, blocked_fid: 0xFFFF, ..Default::default() };
    w.cls[0].conn.set_video_report(vf);
    w.run_for(ms(500));

    assert_eq!(w.count_c(0, |e| *e == CEv::Control(status.clone())), 1, "SESSION_STATUS 只交付一次");
    assert!(w.count_c(0, |e| matches!(e, CEv::FrameStatus(f) if f.fid == 5 && f.kind == FrameStatusKind::KeyPending)) >= 1);
    for sym in 0..4 {
        let body = sym_body(5, sym, 1186);
        assert_eq!(w.count_c(0, |e| *e == CEv::Video { fid: 5, sym, body: body.clone() }), 1, "符号 {sym}");
    }
    assert_eq!(w.count_g(|e| *e == GEv::Control(ControlMsg::IdrRequest { reason: 1 })), 1);
    assert!(w.count_g(|e| matches!(e, GEv::Report { video, recv } if video.frames_ok == 42 && *recv > 0)) >= 1);
    // PING/PONG：RTT 约 20 ms
    let rtts: Vec<Micros> =
        w.cls[0].ev.iter().filter_map(|(_, e)| if let CEv::Rtt(r) = e { Some(*r) } else { None }).collect();
    assert!(rtts.len() >= 3, "RTT 样本 {rtts:?}");
    assert!(rtts.iter().all(|&r| (ms(19)..=ms(22)).contains(&r)), "RTT 样本 {rtts:?}");
    let gw_rtt = w.gw.conn().unwrap().rtt().srtt();
    assert!((ms(15)..=ms(25)).contains(&gw_rtt), "网关 srtt {gw_rtt}");
    // 首包的第二份命中网关的 HS_RESP 缓存，回来的第二个 HS_RESP 被忽略；除此之外没有任何丢弃
    let drops: Vec<&CEv> = w.cls[0].ev.iter().map(|(_, e)| e).filter(|e| matches!(e, CEv::Dropped(_))).collect();
    assert_eq!(drops, vec![&CEv::Dropped(DropReason::HsUnexpected)]);
    assert_eq!(w.gw.drops().total(), 0, "{:?}", w.gw.drops().snapshot());

    // 客户端正常关闭：3 份 CLOSE，网关收到 Closed(0)、会话删除
    let before = w.cls[0].up_packets as usize;
    w.cls[0].conn.close(close_code::NORMAL, "");
    w.run_for(ms(100));
    let t = &w.cls[0].up_times[before..];
    assert_eq!(t.len(), 3, "CLOSE 发 3 份");
    assert_eq!([t[1] - t[0], t[2] - t[0]], [ms(5), ms(10)], "CLOSE 在 0、5、10 ms 各一份");
    assert_eq!(w.count_g(|e| *e == GEv::Closed(close_code::NORMAL)), 1);
    assert!(w.gw.conn().is_none());
    assert!(!w.cls[0].conn.is_established());
    assert!(w.cls[0].conn.next_timeout().is_none(), "关闭之后没有计时器");
}

#[test]
fn first_init_both_copies_lost() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 2);
    w.cls[0].up.drop_next(2);
    w.connect(0);
    w.run_until(ms(600));
    let est = w.established_at(0).expect("第二次尝试应当成功");
    assert!((ms(250)..=ms(320)).contains(&est), "建立于 {est} µs");
    assert_eq!(w.cls[0].conn.stats().hs_attempts, 2);
    assert_eq!(w.cls[0].hs_inits, 3, "首包两份 + 250 ms 的第二次尝试");
    assert_eq!(w.count_g(|e| matches!(e, GEv::Connected { .. })), 1);
}

#[test]
fn handshake_under_437ms_rtt_uses_late_first_resp() {
    let half = 218_500;
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, LinkCfg::ideal(half), 3);
    w.connect(0);
    w.run_until(secs(2));
    let est = w.established_at(0).expect("应当建立");
    // HS_RESP（回第一次尝试）在 437 ms 到，第一个 DATA 再往返一次：约 874 ms
    assert!(est < ms(950), "建立于 {est} µs");
    // 第二次尝试（250 ms）已经发出，它的 HS_RESP 晚到、被忽略；网关从两份候选里试解出第一份
    assert_eq!(w.cls[0].conn.stats().hs_attempts, 2);
    assert_eq!(w.count_c(0, |e| *e == CEv::HsOk), 1);
    assert!(w.count_c(0, |e| *e == CEv::Dropped(DropReason::HsUnexpected)) >= 1);
    assert_eq!(w.count_g(|e| matches!(e, GEv::Connected { .. })), 1);
    assert_eq!(w.count_c(0, |e| *e == CEv::Reset), 0);
    let srtt = w.gw.conn().unwrap().rtt().srtt();
    assert!((ms(420)..=ms(460)).contains(&srtt), "网关 srtt {srtt}");
}

#[test]
fn rtt_spike_437ms_mid_session() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 4);
    w.connect(0);
    w.run_until(ms(500));
    assert!(w.cls[0].conn.is_established());
    let cid = w.cls[0].conn.cid();

    // 尖峰：单向 218.5 ms，持续 3 s；期间两边都发控制消息
    let spike = LinkCfg::ideal(218_500);
    w.cls[0].up.set_cfg(spike);
    w.cls[0].down.set_cfg(spike);
    let mut sent_down = Vec::new();
    let mut sent_up = Vec::new();
    for i in 0..10u8 {
        let m = ControlMsg::SessionStatus(SessionStatus { ps_state: i, ..Default::default() });
        w.gw.conn().unwrap().send_control(&m).unwrap();
        sent_down.push(m);
        let m = ControlMsg::IdrRequest { reason: i };
        w.cls[0].conn.send_control(&m).unwrap();
        sent_up.push(m);
        w.run_for(ms(300));
    }
    w.cls[0].up.set_cfg(ideal());
    w.cls[0].down.set_cfg(ideal());
    w.run_for(secs(3));

    assert_eq!(w.cls[0].conn.cid(), cid, "尖峰期间不应重新握手");
    assert!(w.cls[0].conn.is_established());
    assert_eq!(w.count_c(0, |e| *e == CEv::Reset), 0);
    let got_down: Vec<ControlMsg> =
        w.cls[0].ev.iter().filter_map(|(_, e)| if let CEv::Control(m) = e { Some(m.clone()) } else { None }).collect();
    assert_eq!(got_down, sent_down, "下行控制消息按序、不重不漏");
    let got_up: Vec<ControlMsg> =
        w.gw_ev.iter().filter_map(|(_, e)| if let GEv::Control(m) = e { Some(m.clone()) } else { None }).collect();
    assert_eq!(got_up, sent_up, "上行控制消息按序、不重不漏");
    let rtts: Vec<Micros> =
        w.cls[0].ev.iter().filter_map(|(_, e)| if let CEv::Rtt(r) = e { Some(*r) } else { None }).collect();
    assert!(rtts.iter().any(|&r| r >= ms(430)), "应当量到尖峰：{rtts:?}");
    assert!(*rtts.last().unwrap() <= ms(22), "尖峰过后回落：{rtts:?}");
    let srtt = w.gw.conn().unwrap().rtt().srtt();
    assert!(srtt < ms(60), "网关 srtt 回落到 {srtt}");
}

#[test]
fn gateway_state_lost_reset_and_rehandshake() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 5);
    w.connect(0);
    w.run_until(ms(500));
    assert!(w.cls[0].conn.is_established());
    let old_cid = w.cls[0].conn.cid();

    // 网关进程重启：状态全丢（同一套 keys/）
    let t_clear = w.now;
    w.gw = Gateway::new(gw_cfg(TakeoverPolicy::RequireFlag));
    w.gw_ev.clear();
    w.run_for(secs(2));

    let reset_at = w.cls[0].ev.iter().find(|(_, e)| *e == CEv::Reset).map(|(t, _)| *t).expect("应当收到 RESET");
    // 客户端 50 ms 内会发 REPORT（≥ 64 B），触发 RESET
    assert!(reset_at - t_clear <= ms(100), "RESET 在清空后 {} µs", reset_at - t_clear);
    let est2 = w.cls[0].ev.iter().filter(|(_, e)| *e == CEv::Established).nth(1).map(|(t, _)| *t).expect("应当重新建立");
    assert!(est2 - reset_at < secs(1), "收到 RESET 后 {} µs 才重新建立", est2 - reset_at);
    assert_ne!(w.cls[0].conn.cid(), old_cid, "重新握手换了 CID");
    // 新握手带上 HS_RESP 里拿到的 resume
    let resume = w.gw_ev.iter().find_map(|(_, e)| if let GEv::Connected { resume, .. } = e { Some(*resume) } else { None });
    assert_eq!(resume, Some(Some((HOST_ID, EPOCH))));
    assert!(w.gw.drops().get(DropReason::UnknownCid) >= 1);
}

#[test]
fn reset_with_wrong_token_or_length_is_ignored() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 6);
    w.connect(0);
    w.run_until(ms(300));
    let cid = w.cls[0].conn.cid();
    // 用别的 reset_key 的网关（令牌不对）：RESET 被忽略，客户端不重握
    let mut cfg = gw_cfg(TakeoverPolicy::RequireFlag);
    cfg.keys.reset_key = Secret32::new([0x55; 32]);
    w.gw = Gateway::new(cfg);
    w.run_for(ms(400));
    assert_eq!(w.count_c(0, |e| *e == CEv::Reset), 0);
    assert!(w.count_c(0, |e| *e == CEv::Dropped(DropReason::ResetBadToken)) >= 1);
    assert_eq!(w.cls[0].conn.cid(), cid);
    // 长度不对的包
    let mut junk = vec![0x70u8; 39];
    let now = w.now;
    let mut ev = Vec::new();
    w.cls[0].conn.handle_datagram(now, gw_addr(), &mut junk, &mut |e| ev.push(own_c(e)));
    assert_eq!(ev, vec![CEv::Dropped(DropReason::ResetBadLen)]);
}

#[test]
fn busy_then_takeover() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 2, ideal(), 7);
    w.connect(0);
    w.run_until(ms(300));
    assert!(w.cls[0].conn.is_established());
    let a_cid = w.cls[0].conn.cid().0;

    // 另一台客户端没带接管位：BUSY，A 不受影响
    w.connect(1);
    w.run_for(ms(500));
    assert_eq!(w.count_c(1, |e| *e == CEv::Rejected(HsErr::Busy)), 1);
    assert_eq!(w.count_c(1, |e| *e == CEv::Established), 0);
    assert!(w.cls[0].conn.is_established());
    assert_eq!(w.gw.conn().unwrap().client(), ClientId::of(&client_kp(0).public()));
    assert!(w.cls[1].conn.next_timeout().is_none(), "被拒后停下");

    // 用户确认接管：带接管位重连。旧客户端收到 CLOSE(1)
    w.cls[1].conn.set_takeover(true);
    w.connect(1);
    w.run_for(ms(500));
    assert!(w.cls[1].conn.is_established());
    let b_cid = w.cls[1].conn.cid().0;
    assert_eq!(w.gw.conn().unwrap().client(), ClientId::of(&client_kp(1).public()));
    assert_eq!(w.count_c(0, |e| *e == CEv::Closed(close_code::TAKEN_OVER)), 1, "{:?}", w.cls[0].ev);
    assert!(!w.cls[0].conn.is_established());
    assert_eq!(w.count_g(|e| *e == GEv::Replaced { old: a_cid, new: b_cid }), 1);
    assert_eq!(w.count_g(|e| matches!(e, GEv::Connected { takeover: true, .. })), 1);

    // 旧客户端停下，不自动重连；新会话照常收发
    w.run_for(secs(2));
    assert!(!w.cls[0].conn.is_established());
    assert_eq!(w.count_c(0, |e| *e == CEv::HsOk), 1);
    assert!(w.cls[1].conn.is_established());
}

#[test]
fn auto_takeover_policy() {
    let mut w = World::new(TakeoverPolicy::Auto, 2, ideal(), 8);
    w.connect(0);
    w.run_until(ms(300));
    w.connect(1);
    w.run_for(ms(500));
    assert!(w.cls[1].conn.is_established());
    assert_eq!(w.count_c(0, |e| *e == CEv::Closed(close_code::TAKEN_OVER)), 1);
}

#[test]
fn same_identity_reconnect_replaces_silently() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 9);
    w.connect(0);
    w.run_until(ms(300));
    let old = w.cls[0].conn.cid().0;
    // 同一身份（比如客户端重启）：新会话静默顶掉旧会话，不回 BUSY
    w.cls[0].conn = ClientConn::new(client_cfg(0, false));
    w.connect(0);
    w.run_for(ms(300));
    assert!(w.cls[0].conn.is_established());
    let new = w.cls[0].conn.cid().0;
    assert_eq!(w.count_g(|e| *e == GEv::Replaced { old, new }), 1);
}

/// 乱序、重复、10% 丢包：CONTROL 两个方向都按序送达；视频符号不重复、内容不错、送达约 90%
fn lossy_run(seed: u64) {
    let link = LinkCfg { delay: ms(10), jitter: ms(4), loss: 0.10, dup: 0.05, reorder: 0.10, reorder_extra: ms(8) };
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, link, seed);
    w.connect(0);
    w.run_until(secs(3));
    assert!(w.cls[0].conn.is_established(), "种子 {seed}：3 s 内应当建立");

    let mut sent_down = Vec::new();
    let mut sent_up = Vec::new();
    let mut total_syms = 0usize;
    for i in 0..60u16 {
        if i % 2 == 0 {
            let m = ControlMsg::SessionStatus(SessionStatus {
                ps_state: (i % 10) as u8,
                text: Some(format!("状态 {i}")),
                ..Default::default()
            });
            w.gw.conn().unwrap().send_control(&m).unwrap();
            sent_down.push(m);
            let m = ControlMsg::AudioConfig(AudioConfig { channels: 2, rate: 48_000, frame_samples: i, codec: 1 });
            w.gw.conn().unwrap().send_control(&m).unwrap();
            sent_down.push(m);
            let m = ControlMsg::IdrRequest { reason: i as u8 };
            w.cls[0].conn.send_control(&m).unwrap();
            sent_up.push(m);
            let m = ControlMsg::ModeSet { mode: 1, j_ms: i as u8 };
            w.cls[0].conn.send_control(&m).unwrap();
            sent_up.push(m);
        }
        let k = 1 + (i % 6);
        w.queue_frame(i, k, 600 + (i as usize * 13) % 586, i % 30 == 0);
        total_syms += k as usize;
        w.run_for(ms(16));
    }
    w.run_for(secs(10));
    assert!(w.cls[0].conn.is_established(), "种子 {seed}");

    // CONTROL：按流比较（不同流之间不保证先后）
    let split = |v: &[ControlMsg]| -> (Vec<ControlMsg>, Vec<ControlMsg>) { v.iter().cloned().partition(|m| m.stream() == 0) };
    let got_down: Vec<ControlMsg> =
        w.cls[0].ev.iter().filter_map(|(_, e)| if let CEv::Control(m) = e { Some(m.clone()) } else { None }).collect();
    assert_eq!(split(&got_down), split(&sent_down), "种子 {seed}：下行控制消息");
    let got_up: Vec<ControlMsg> =
        w.gw_ev.iter().filter_map(|(_, e)| if let GEv::Control(m) = e { Some(m.clone()) } else { None }).collect();
    assert_eq!(split(&got_up), split(&sent_up), "种子 {seed}：上行控制消息");

    // 视频：不重复（链路复制的包被重放窗口挡掉）、内容正确、送达率合理
    let mut seen = HashSet::new();
    for (_, e) in &w.cls[0].ev {
        if let CEv::Video { fid, sym, body } = e {
            assert!(seen.insert((*fid, *sym)), "种子 {seed}：符号 ({fid}, {sym}) 交付了两次");
            assert_eq!(Some(body), w.syms.0.get(&(*fid, *sym)), "种子 {seed}：符号内容");
        }
    }
    let ratio = seen.len() as f64 / total_syms as f64;
    assert!((0.80..=0.98).contains(&ratio), "种子 {seed}：视频送达率 {ratio:.3}（{}/{total_syms}）", seen.len());
    assert!(w.cls[0].conn.stats().drops.get(DropReason::Replay) > 0, "种子 {seed}：链路应当造出重复包");
    // 网关收到的 REPORT 计数能反映丢包
    assert!(w.count_g(|e| matches!(e, GEv::Report { .. })) > 50);
}

#[test]
fn lossy_reordered_duplicated_link() {
    for seed in [11, 12, 13, 14, 15] {
        lossy_run(seed);
    }
}

#[test]
fn unknown_client_and_junk_are_silent() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 16);
    // 没配对的客户端（网关不认识它的公钥）：静默丢弃，不回任何包
    let mut cfg = client_cfg(0, false);
    cfg.creds.static_kp = StaticKeypair::from_secret([0x99; 32]);
    w.cls[0].conn = ClientConn::new(cfg);
    w.connect(0);
    w.run_until(secs(1));
    assert_eq!(w.count_c(0, |e| *e == CEv::HsOk), 0);
    assert!(w.gw.drops().get(DropReason::UnknownClient) >= 1);
    assert_eq!(w.cls[0].down.stats().sent, 0, "网关不该回任何包");

    // 未知 CID 的短包（< 64 B）不回 RESET
    let mut junk = [0u8; 40];
    junk[0] = 0x40;
    let now = w.now;
    let mut ev = Vec::new();
    w.gw.handle_datagram(now, client_addr(0), &mut junk, &mut |e| ev.push(own_g(e)));
    assert!(ev.contains(&GEv::Dropped(DropReason::ResetTriggerShort)));
    let mut buf = [0u8; BASE_PAYLOAD];
    assert!(w.gw.poll_transmit(now, &w.syms, &mut buf).is_none());
}

#[test]
fn removed_client_is_dropped_with_close4() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 17);
    w.connect(0);
    w.run_until(ms(300));
    assert!(w.cls[0].conn.is_established());
    // 配对列表里删掉这个客户端：会话立即删除，客户端收到 CLOSE(4)
    w.gw.reload_clients(Vec::new());
    assert!(w.gw.conn().is_none());
    w.run_for(ms(200));
    assert_eq!(w.count_c(0, |e| *e == CEv::Closed(close_code::GATEWAY_ENDED)), 1);
    assert!(!w.cls[0].conn.is_established());
}

#[test]
fn silent_client_removed_after_60s() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 18);
    w.connect(0);
    w.run_until(ms(300));
    // 上行断了（下行还通）：网关 60 s 收不到任何包就删会话，并给客户端发 CLOSE(4)
    let t0 = w.now;
    w.cls[0].up.add_outage(t0, t0 + secs(120));
    w.run_for(secs(65));
    let (t, _) = w.gw_ev.iter().find(|(_, e)| *e == GEv::Closed(close_code::GATEWAY_ENDED)).expect("应当删会话");
    assert!((t0 + secs(59)..=t0 + secs(61)).contains(t), "删会话于 {} µs", t - t0);
    assert!(w.gw.conn().is_none());
    // 下行一直通，客户端没有 30 s 静默；它收到 CLOSE(4) 后停下
    assert_eq!(w.count_c(0, |e| *e == CEv::Closed(close_code::GATEWAY_ENDED)), 1);
}

#[test]
fn debug_output_has_no_secrets() {
    let mut w = World::new(TakeoverPolicy::RequireFlag, 1, ideal(), 19);
    w.connect(0);
    w.run_until(ms(300));
    assert!(w.cls[0].conn.is_established());
    // 私钥、PSK、reset_key 的字节（各自是 32 个相同的字节）不能出现在 Debug 输出里
    let dump = format!("{:?}\n{:?}", w.gw, w.cls[0].conn);
    for (what, b) in [("网关私钥", 0x11u8), ("客户端私钥", 0x20), ("PSK", 0x40), ("reset_key", 0x77)] {
        let pat = vec![b.to_string(); 8].join(", ");
        assert!(!dump.contains(&pat), "Debug 输出里出现了{what}");
    }
    assert!(dump.contains("<redacted>"));
}
