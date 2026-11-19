//! 服务循环的测试：
//! - 假 socket + 真的客户端端点（ywp::endpoint::ClientConn），虚拟时钟逐毫秒推进：
//!   接入顺序（SESSION_STATUS、VIDEO_CONFIG、KEY_PENDING）、从关键帧开始转发、ENOBUFS 重试和丢包、
//!   WouldBlock、其他发送错误、IDR_REQUEST、NEED、epoch 变化、关键帧溢出、关闭、控制口命令；
//! - 真线程 + 回环 UDP：spawn_with 起服务，客户端经 127.0.0.1 连上、收到画面、收到 CLOSE(5)。

use super::*;
use crate::ingress::StreamInfo;
use crate::source::AudioHeader;
use std::cell::RefCell;
use std::collections::VecDeque;
use ywp::consts::{caps, ps_state, PROTO_MIN_VERSION, PROTO_VERSION};
use ywp::creds::{ClientCreds, ClientEntry};
use ywp::crypto::{Secret32, StaticKeypair, Tai64N};
use ywp::endpoint::{ClientConfig, ClientConn, ClientEvent};
use ywp::wire::HsInitPayload;

const EPOCH: u32 = 0xABCD_0001;
const HOST_ID: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];

fn gw_addr() -> SocketAddr {
    "192.0.2.1:9420".parse().unwrap()
}

fn cl_addr() -> SocketAddr {
    "192.0.2.10:50000".parse().unwrap()
}

fn gw_kp() -> StaticKeypair {
    StaticKeypair::from_secret([0x11; 32])
}

fn cl_kp() -> StaticKeypair {
    StaticKeypair::from_secret([0x21; 32])
}

fn psk() -> Secret32 {
    Secret32::new([0x41; 32])
}

fn keys() -> Keys {
    Keys {
        static_kp: gw_kp(),
        reset_key: Secret32::new([0x77; 32]),
        clients: vec![ClientEntry { name: "c0".into(), public: cl_kp().public(), psk: psk() }],
    }
}

fn client(entry: SocketAddr) -> ClientConn {
    ClientConn::new(ClientConfig {
        creds: ClientCreds { static_kp: cl_kp(), gw_pub: gw_kp().public(), psk: psk() },
        entry,
        name: "测试客户端".into(),
        bitrate_cap_kbps: None,
        takeover: false,
    })
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

// HEVC NAL 头第一个字节 = type << 1
const VPS: u8 = 32 << 1;
const SPS: u8 = 33 << 1;
const PPS: u8 = 34 << 1;
const IDR: u8 = 19 << 1;
const TRAIL: u8 = 1 << 1;

fn au(nals: &[u8], body: usize) -> Vec<u8> {
    let mut v = Vec::new();
    for &h in nals {
        v.extend_from_slice(&[0, 0, 0, 1, h, 0x01]);
        v.extend(std::iter::repeat_n(0xAB, body));
    }
    v
}

fn params(tag: u8) -> Arc<[u8]> {
    let mut p = au(&[VPS, SPS, PPS], 8);
    p.push(tag);
    p.into()
}

/// 关键帧 3 个源符号（k = 3，m = 2）；P 帧 1 个（k = 1，m = 2）
fn key_au() -> Vec<u8> {
    au(&[IDR], 3000)
}

fn p_au() -> Vec<u8> {
    au(&[TRAIL], 500)
}

// ---------------------------------------------------------------------- 假件

#[derive(Default)]
struct FakeSock {
    inbox: RefCell<VecDeque<(Vec<u8>, SocketAddr)>>,
    sent: RefCell<Vec<(Vec<u8>, SocketAddr)>>,
    /// 接下来几次 send 返回的错误码（空：成功）
    fail: RefCell<VecDeque<i32>>,
    /// 每次 send 的 (字节, 成功与否)
    attempts: RefCell<Vec<(Vec<u8>, bool)>>,
}

impl DgramSock for FakeSock {
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let Some((p, from)) = self.inbox.borrow_mut().pop_front() else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        let n = p.len().min(buf.len());
        buf[..n].copy_from_slice(&p[..n]);
        Ok((n, from))
    }
    fn send_to(&self, buf: &[u8], dst: SocketAddr) -> io::Result<usize> {
        if let Some(errno) = self.fail.borrow_mut().pop_front() {
            self.attempts.borrow_mut().push((buf.to_vec(), false));
            return Err(io::Error::from_raw_os_error(errno));
        }
        self.attempts.borrow_mut().push((buf.to_vec(), true));
        self.sent.borrow_mut().push((buf.to_vec(), dst));
        Ok(buf.len())
    }
}

#[derive(Default)]
struct FakeCtl {
    idr: Mutex<Vec<IdrReason>>,
    params: Mutex<HashMap<u32, Arc<[u8]>>>,
}

impl FakeCtl {
    fn idr(&self) -> Vec<IdrReason> {
        self.idr.lock().unwrap().clone()
    }
}

impl PsControl for FakeCtl {
    fn request_idr(&self, _now: Micros, why: IdrReason) -> IdrDecision {
        self.idr.lock().unwrap().push(why);
        IdrDecision::Requested
    }
    fn set_controller_full(&self, _c: &ywp::wire::InputState) {}
    fn set_controller_idle(&self) {}
    fn params(&self, epoch: u32) -> Option<Arc<[u8]>> {
        self.params.lock().unwrap().get(&epoch).cloned()
    }
    fn audio_header(&self, _epoch: u32) -> Option<AudioHeader> {
        None
    }
    fn stream_info(&self, _epoch: u32) -> Option<StreamInfo> {
        Some(StreamInfo { codec: ywp::consts::video_codec::HEVC, fps: 60, width: 1920, height: 1080 })
    }
}

/// 日志收集（顺便检查不出现 PSK、私钥的十六进制）
#[derive(Default)]
struct Sink(Mutex<Vec<String>>);

impl JsonlSink for Sink {
    fn record(&self, kind: &str, fields: &str) {
        self.0.lock().unwrap().push(format!("R {kind} {fields}"));
    }
    fn log(&self, level: char, kind: &str, msg: &str) {
        self.0.lock().unwrap().push(format!("{level} {kind} {msg}"));
    }
}

impl Sink {
    fn assert_no_secrets(&self) {
        let secrets = [ywp::creds::to_hex(psk().expose()), ywp::creds::to_hex(&[0x11; 32]), ywp::creds::to_hex(&[0x21; 32])];
        for line in self.0.lock().unwrap().iter() {
            for s in &secrets {
                assert!(!line.contains(s.as_str()), "日志里出现了密钥：{line}");
            }
        }
    }
}

/// 客户端事件的拥有版
#[derive(Clone, Debug, PartialEq)]
enum CEv {
    Established,
    Video { fid: u16, sym: u16, key: bool, k: u16, cfg: u8 },
    FrameStatus(FrameStatusEntry),
    Control(ControlMsg),
    Closed(u16),
    Other,
}

fn own(e: ClientEvent<'_>) -> CEv {
    match e {
        ClientEvent::Established => CEv::Established,
        ClientEvent::Video(v, _) => CEv::Video { fid: v.hdr.fid, sym: v.hdr.sym_idx, key: v.hdr.key, k: v.hdr.k, cfg: v.hdr.cfg },
        ClientEvent::FrameStatus(f) => CEv::FrameStatus(f),
        ClientEvent::Control(m) => CEv::Control(m),
        ClientEvent::Closed { code } => CEv::Closed(code),
        _ => CEv::Other,
    }
}

struct H {
    srv: Server<FakeSock>,
    tx: SyncSender<MediaMsg>,
    ev: Receiver<ServeEvent>,
    ctl: Arc<FakeCtl>,
    sink: Arc<Sink>,
    cl: ClientConn,
    cev: Vec<CEv>,
    now: Micros,
    seq: u32,
}

impl H {
    fn new() -> Self {
        let sink = Arc::new(Sink::default());
        let ctl = Arc::new(FakeCtl::default());
        let cfg = ServeConfig { port: 9420, bind_if: "en0".into(), takeover: TakeoverPolicy::RequireFlag, log: sink.clone() };
        let (tx, rx) = mpsc::sync_channel(MEDIA_QUEUE_CAP);
        let (etx, erx) = mpsc::channel();
        let now = ms(1_000);
        let srv = Server::new(
            FakeSock::default(),
            &cfg,
            &ServeOptions::default(),
            keys(),
            ctl.clone(),
            etx,
            rx,
            Arc::new(ServeShared::default()),
            now,
        );
        Self { srv, tx, ev: erx, ctl, sink, cl: client(gw_addr()), cev: Vec::new(), now, seq: 0 }
    }

    /// 网关 → 客户端：把假 socket 发出的包交给客户端
    fn deliver(&mut self) {
        let sent: Vec<_> = self.srv.sock.sent.borrow_mut().drain(..).collect();
        for (mut p, dst) in sent {
            assert_eq!(dst, cl_addr());
            let ev = &mut self.cev;
            self.cl.handle_datagram(self.now, gw_addr(), &mut p, &mut |e| ev.push(own(e)));
        }
    }

    /// 一毫秒：客户端发包 → 网关一轮 → 网关的包交给客户端 → 客户端计时器
    fn step(&mut self) {
        let mut buf = [0u8; BASE_PAYLOAD];
        while let Some(t) = self.cl.poll_transmit(self.now, &mut buf) {
            self.srv.sock.inbox.borrow_mut().push_back((buf[..t.len].to_vec(), cl_addr()));
        }
        while self.srv.turn(self.now, false) {}
        self.deliver();
        if self.cl.next_timeout().is_some_and(|t| t <= self.now) {
            let ev = &mut self.cev;
            self.cl.handle_timeout(self.now, &mut |e| ev.push(own(e)));
        }
    }

    fn run_ms(&mut self, n: u64) {
        for _ in 0..n {
            self.step();
            self.now += ms(1);
        }
    }

    fn push(&mut self, m: MediaMsg) {
        self.tx.try_send(m).unwrap();
    }

    fn video(&mut self, epoch: u32, data: Vec<u8>) {
        self.seq += 1;
        let seq = self.seq;
        self.push(MediaMsg::Video { seq, epoch, data, frames_lost: 0, recovered: false, rx_us: self.now });
    }

    fn take(&mut self) -> Vec<CEv> {
        std::mem::take(&mut self.cev)
    }

    fn server_events(&self) -> Vec<String> {
        self.ev.try_iter().map(|e| format!("{e:?}")).collect()
    }

    /// 参数集到位、PS5 在串流、客户端接入并收到第一个关键帧（fid 0）
    fn streaming() -> Self {
        let mut h = Self::new();
        h.ctl.params.lock().unwrap().insert(EPOCH, params(0));
        h.push(MediaMsg::ParamsChanged { epoch: EPOCH });
        h.push(MediaMsg::PsChanged(SessionInfo { ps_state: ps_state::STREAMING, host_id: HOST_ID, epoch: EPOCH, resumable: false }));
        h.cl.connect(h.now, init_payload());
        h.run_ms(50);
        assert!(h.take().contains(&CEv::Established));
        h.video(EPOCH, key_au());
        h.run_ms(5);
        let v = videos(&h.take());
        assert_eq!(v.len(), 5, "关键帧 3 源 + 2 校验");
        h
    }
}

fn videos(ev: &[CEv]) -> Vec<(u16, u16, bool)> {
    ev.iter()
        .filter_map(|e| match e {
            CEv::Video { fid, sym, key, .. } => Some((*fid, *sym, *key)),
            _ => None,
        })
        .collect()
}

fn key_pending(ev: &[CEv]) -> Vec<u16> {
    let mut v: Vec<u16> = ev
        .iter()
        .filter_map(|e| match e {
            CEv::FrameStatus(f) if f.kind == FrameStatusKind::KeyPending => Some(f.fid),
            _ => None,
        })
        .collect();
    v.dedup();
    v
}

// ---------------------------------------------------------------------- 测试

#[test]
fn attach_sends_status_config_key_pending_then_forwards_from_keyframe() {
    let mut h = H::new();
    h.ctl.params.lock().unwrap().insert(EPOCH, params(0));
    h.push(MediaMsg::ParamsChanged { epoch: EPOCH });
    h.push(MediaMsg::PsChanged(SessionInfo { ps_state: ps_state::STREAMING, host_id: HOST_ID, epoch: EPOCH, resumable: false }));
    // 没有客户端：帧不转发、不占 fid
    h.video(EPOCH, key_au());
    h.run_ms(2);
    assert_eq!(h.srv.ing.stats().dropped_no_client, 1);
    assert!(h.ctl.idr().is_empty());

    h.cl.connect(h.now, init_payload());
    h.run_ms(50);
    let ev = h.take();
    assert!(ev.contains(&CEv::Established));
    let status = ev.iter().find_map(|e| match e {
        CEv::Control(ControlMsg::SessionStatus(s)) => Some(s.clone()),
        _ => None,
    });
    let status = status.expect("接入时发 SESSION_STATUS");
    assert_eq!((status.ps_state, status.host_id, status.epoch), (ps_state::STREAMING, Some(HOST_ID), Some(EPOCH)));
    let cfg = ev.iter().find_map(|e| match e {
        CEv::Control(ControlMsg::VideoConfig(c)) => Some(c.clone()),
        _ => None,
    });
    let cfg = cfg.expect("接入时发 VIDEO_CONFIG");
    assert_eq!(cfg.cfg_ver, 1);
    assert_eq!(cfg.fps, 60);
    assert_eq!(cfg.profiles[0].param_sets, params(0).to_vec());
    assert_eq!(key_pending(&ev), vec![0], "KEY_PENDING 的 fid 是下一个要分配的 fid");
    assert_eq!(h.ctl.idr(), vec![IdrReason::WaitKey(WaitWhy::NewClient)]);
    assert!(h.server_events().iter().any(|e| e.contains("ClientAttached")));
    assert!(h.srv.shared.client_present.load(Relaxed));

    // 等关键帧期间 P 帧丢掉；关键帧从 fid 0 开始
    h.video(EPOCH, p_au());
    h.run_ms(3);
    assert!(videos(&h.take()).is_empty());
    h.video(EPOCH, key_au());
    h.run_ms(3);
    let ev = h.take();
    let v = videos(&ev);
    assert_eq!(v.len(), 5);
    assert!(v.iter().all(|&(fid, _, key)| fid == 0 && key));
    assert!(ev.iter().any(|e| matches!(e, CEv::Video { k: 3, cfg: 1, .. })));
    h.video(EPOCH, p_au());
    h.run_ms(3);
    let v = videos(&h.take());
    assert_eq!(v.iter().map(|x| x.0).collect::<Vec<_>>(), vec![1, 1, 1]);
    assert_eq!(h.srv.ing.stats().forwarded, 2);

    // status 摘要
    h.run_ms(300);
    let st = h.srv.shared.status.lock().unwrap().clone();
    let cl = st.client.as_ref().expect("有客户端");
    assert_eq!(cl.name, "c0");
    assert_eq!(cl.peer, cl_addr());
    assert_eq!(st.ingress.forwarded, 2);
    let j = st.to_json();
    assert!(j.contains("\"name\":\"c0\"") && j.contains("\"forwarded\":2"), "{j}");
    h.sink.assert_no_secrets();
}

#[test]
fn enobufs_retries_after_1ms_and_drops_after_three() {
    let mut h = H::streaming();
    let c0 = h.srv.c;
    // 两次 ENOBUFS 后成功：同一包原样重发
    h.srv.sock.fail.borrow_mut().extend([libc_enobufs(), libc_enobufs()]);
    h.video(EPOCH, p_au());
    let n0 = h.srv.sock.attempts.borrow().len();
    h.srv.turn(h.now, false);
    assert_eq!(h.srv.sock.attempts.borrow().len(), n0 + 1);
    assert!(h.srv.pending.is_some());
    // 1 ms 之内不重试
    h.srv.turn(h.now + 500, false);
    assert_eq!(h.srv.sock.attempts.borrow().len(), n0 + 1);
    assert!(h.srv.next_deadline(h.now + 500) <= h.now + ENOBUFS_RETRY);
    h.srv.turn(h.now + ENOBUFS_RETRY, false);
    assert_eq!(h.srv.sock.attempts.borrow().len(), n0 + 2);
    h.srv.turn(h.now + 2 * ENOBUFS_RETRY, false);
    {
        let a = h.srv.sock.attempts.borrow();
        assert!(a[n0 + 2].1, "第三次成功");
        assert_eq!(a[n0].0, a[n0 + 2].0, "重试发的是同一包");
        assert_eq!(a[n0 + 1].0, a[n0 + 2].0);
    }
    assert_eq!(h.srv.c.send_enobufs - c0.send_enobufs, 2);
    assert_eq!(h.srv.c.send_nobufs_dropped, 0);
    h.now += ms(3);
    h.run_ms(3);
    assert_eq!(videos(&h.take()).len(), 3, "这一帧 3 个包都到了");

    // 连续 3 次 ENOBUFS：丢掉这一包，后面的照常发，循环不退出
    h.srv.sock.fail.borrow_mut().extend([libc_enobufs(), libc_enobufs(), libc_enobufs()]);
    h.video(EPOCH, p_au());
    for i in 0..3 {
        h.srv.turn(h.now + i * ENOBUFS_RETRY, false);
    }
    assert_eq!(h.srv.c.send_nobufs_dropped, 1);
    assert_eq!(h.srv.send_drops.get(DropReason::SendNoBufs), 1);
    assert_eq!(h.srv.c.send_enobufs - c0.send_enobufs, 5);
    assert!(h.srv.pending.is_none());
    h.now += ms(3);
    h.run_ms(5);
    let v = videos(&h.take());
    assert_eq!(v.len(), 2, "丢了 1 个符号，其余 2 个照常到（固定冗余能补回）：{v:?}");
    assert!(!h.srv.exit);
    // 之后照常
    h.video(EPOCH, p_au());
    h.run_ms(3);
    assert_eq!(videos(&h.take()).len(), 3);
}

fn libc_enobufs() -> i32 {
    // macOS、Linux 上的 ENOBUFS（55 / 105）；从 io::Error 反查，免得 yw-serve 依赖 libc
    #[cfg(target_vendor = "apple")]
    {
        55
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        105
    }
}

fn libc_eagain() -> i32 {
    #[cfg(target_vendor = "apple")]
    {
        35
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        11
    }
}

fn libc_ehostunreach() -> i32 {
    #[cfg(target_vendor = "apple")]
    {
        65
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        113
    }
}

#[test]
fn errno_constants_match_classification() {
    assert!(matches!(classify_send_err(&io::Error::from_raw_os_error(libc_enobufs())), SendOutcome::NoBufs));
    assert!(matches!(classify_send_err(&io::Error::from_raw_os_error(libc_eagain())), SendOutcome::WouldBlock));
    assert!(matches!(classify_send_err(&io::Error::from_raw_os_error(libc_ehostunreach())), SendOutcome::Fatal(_)));
}

#[test]
fn wouldblock_waits_for_writable() {
    let mut h = H::streaming();
    h.srv.sock.fail.borrow_mut().push_back(libc_eagain());
    h.video(EPOCH, p_au());
    let n0 = h.srv.sock.attempts.borrow().len();
    h.srv.turn(h.now, false);
    assert!(h.srv.wants_writable());
    // 没有可写事件：时间到了也不重试
    h.srv.turn(h.now + ms(5), false);
    assert_eq!(h.srv.sock.attempts.borrow().len(), n0 + 1);
    assert!(h.srv.next_deadline(h.now + ms(5)) > h.now + ms(5), "不空转");
    h.srv.turn(h.now + ms(5), true);
    assert!(!h.srv.wants_writable());
    assert!(h.srv.sock.attempts.borrow().len() >= n0 + 2);
    assert_eq!(h.srv.c.send_wouldblock, 1);
    h.now += ms(6);
    h.run_ms(3);
    assert_eq!(videos(&h.take()).len(), 3);
}

#[test]
fn other_send_errors_drop_the_packet_and_continue() {
    let mut h = H::streaming();
    h.srv.sock.fail.borrow_mut().push_back(libc_ehostunreach());
    h.video(EPOCH, p_au());
    h.run_ms(3);
    assert_eq!(h.srv.c.send_errors, 1);
    assert_eq!(videos(&h.take()).len(), 2);
    assert!(h.sink.0.lock().unwrap().iter().any(|l| l.contains("发送失败")));
    h.video(EPOCH, p_au());
    h.run_ms(3);
    assert_eq!(videos(&h.take()).len(), 3);
}

#[test]
fn client_idr_request_goes_through_ps_control() {
    let mut h = H::streaming();
    h.cl.send_control(&ControlMsg::IdrRequest { reason: ywp::consts::idr_reason::DECODER_ERROR }).unwrap();
    h.run_ms(20);
    assert!(h.ctl.idr().contains(&IdrReason::ClientRequest(ywp::consts::idr_reason::DECODER_ERROR)));
    assert_eq!(h.srv.c.client_idr, 1);
}

#[test]
fn need_for_missing_frame_replies_gone_and_key_pending() {
    let mut h = H::streaming();
    h.cl.send_need(Need { fid: 999, need: 0xFF, key: true, tries: 0 });
    h.run_ms(30);
    let ev = h.take();
    assert!(ev.iter().any(|e| matches!(e, CEv::FrameStatus(f) if f.kind == FrameStatusKind::Gone && f.fid == 999)));
    assert_eq!(key_pending(&ev), vec![1]);
    assert!(h.ctl.idr().contains(&IdrReason::NeedK));
    // 缓存里有的帧：补发全部源符号
    h.cl.send_need(Need { fid: 0, need: 0xFF, key: false, tries: 0 });
    h.run_ms(30);
    let v = videos(&h.take());
    assert_eq!(v.iter().filter(|x| x.0 == 0).count(), 3, "{v:?}");
}

#[test]
fn epoch_change_bumps_cfg_ver_and_resends_config() {
    let mut h = H::streaming();
    let e2 = EPOCH + 1;
    h.ctl.params.lock().unwrap().insert(e2, params(9));
    h.video(e2, key_au());
    h.run_ms(20);
    let ev = h.take();
    let cfg = ev.iter().find_map(|e| match e {
        CEv::Control(ControlMsg::VideoConfig(c)) => Some(c.clone()),
        _ => None,
    });
    let cfg = cfg.expect("新会话的 VIDEO_CONFIG");
    assert_eq!(cfg.cfg_ver, 2);
    assert_eq!(cfg.profiles[0].param_sets, params(9).to_vec());
    // fid 跨会话继续递增，cfg 是新版本的低 2 位
    assert!(ev.iter().any(|e| matches!(e, CEv::Video { fid: 1, cfg: 2, key: true, .. })));
}

#[test]
fn params_notification_lost_is_refilled_from_slot() {
    let mut h = H::new();
    // 只放进最新值槽，不发 ParamsChanged
    h.ctl.params.lock().unwrap().insert(EPOCH, params(3));
    h.cl.connect(h.now, init_payload());
    h.run_ms(50);
    h.take();
    h.video(EPOCH, key_au());
    h.run_ms(20);
    let ev = h.take();
    assert!(ev.iter().any(|e| matches!(e, CEv::Control(ControlMsg::VideoConfig(c)) if c.cfg_ver == 1)));
    assert_eq!(videos(&ev).len(), 5);
}

#[test]
fn key_overflow_flag_enters_wait_key() {
    let mut h = H::streaming();
    h.srv.shared.key_overflow.store(true, Relaxed);
    h.video(EPOCH, p_au());
    h.run_ms(20);
    let ev = h.take();
    assert!(videos(&ev).is_empty(), "P 帧不转发");
    assert_eq!(key_pending(&ev), vec![1]);
    assert!(h.ctl.idr().contains(&IdrReason::WaitKey(WaitWhy::Overflow)));
    assert_eq!(h.srv.ing.waiting(), Some(WaitWhy::Overflow));
    h.video(EPOCH, key_au());
    h.run_ms(5);
    assert_eq!(videos(&h.take()).len(), 5);
    assert_eq!(h.srv.ing.waiting(), None);
}

#[test]
fn no_frames_for_300ms_requests_idr() {
    let mut h = H::streaming();
    h.run_ms(350);
    let ev = h.take();
    assert_eq!(key_pending(&ev), vec![1]);
    assert!(h.ctl.idr().contains(&IdrReason::NoFrames));
}

#[test]
fn shutdown_sends_close_5_and_acks() {
    let mut h = H::streaming();
    let (ack, ack_rx) = mpsc::channel();
    h.push(MediaMsg::Shutdown { ack });
    for _ in 0..40 {
        if h.srv.exit {
            break;
        }
        h.step();
        h.now += ms(1);
    }
    assert!(h.srv.exit, "关闭不超过 40 ms");
    assert!(ack_rx.try_recv().is_ok());
    assert!(h.take().contains(&CEv::Closed(close_code::GATEWAY_EXIT)));
    assert!(h.server_events().iter().any(|e| e.contains("GatewayExit")));
    assert!(!h.srv.shared.client_present.load(Relaxed));
    assert!(!h.srv.shared.status.lock().unwrap().running);
}

#[test]
fn control_commands_clients_kick_reload() {
    let mut h = H::streaming();
    let ask = |h: &mut H, cmd: &str| -> String {
        let (rtx, rrx) = mpsc::channel();
        h.push(MediaMsg::Control((cmd.to_string(), rtx, Instant::now() + Duration::from_secs(5))));
        h.run_ms(2);
        rrx.try_recv().expect("有回复")
    };
    let r = ask(&mut h, "clients");
    assert!(r.contains("\"name\":\"c0\"") && r.contains("\"connected\":true") && r.contains("\"current\":{"), "{r}");
    assert!(ask(&mut h, "kick nobody").contains("没有"));
    assert!(ask(&mut h, "clients-reload").contains("没有配 keys/"));
    let r = ask(&mut h, "kick c0");
    assert!(r.starts_with("ok"), "{r}");
    h.run_ms(20);
    assert!(h.take().contains(&CEv::Closed(close_code::GATEWAY_ENDED)));
    assert!(h.server_events().iter().any(|e| e.contains("Kicked")));
    assert!(ask(&mut h, "clients").contains("\"current\":null"));
    // 过了截止时间的命令不执行、不回复
    let (rtx, rrx) = mpsc::channel();
    h.push(MediaMsg::Control(("clients".into(), rtx, Instant::now() - Duration::from_millis(1))));
    h.run_ms(2);
    assert!(rrx.try_recv().is_err());
}

#[test]
fn client_close_detaches() {
    let mut h = H::streaming();
    h.cl.close(close_code::NORMAL, "bye");
    h.run_ms(20);
    assert!(h.srv.attached.is_none());
    assert!(h.server_events().iter().any(|e| e.contains("Closed(0)")));
    // 没有客户端了：帧不转发
    let before = h.srv.ing.stats().forwarded;
    h.video(EPOCH, key_au());
    h.run_ms(3);
    assert_eq!(h.srv.ing.stats().forwarded, before);
}

// ---------------------------------------------------------------------- 真线程 + 回环 UDP

#[test]
fn spawned_server_over_loopback() {
    let sink = Arc::new(Sink::default());
    let ctl = Arc::new(FakeCtl::default());
    ctl.params.lock().unwrap().insert(EPOCH, params(0));
    let (etx, erx) = mpsc::channel();
    let cfg = ServeConfig { port: 0, bind_if: String::new(), takeover: TakeoverPolicy::RequireFlag, log: sink.clone() };
    let opts = ServeOptions { bind_ip: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), ..ServeOptions::default() };
    let h = spawn_with(cfg, opts, keys(), ctl.clone(), etx).expect("起服务");
    let gw = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), h.local_addr().port());
    assert_ne!(gw.port(), 0);

    let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_nonblocking(true).unwrap();
    let mut cl = client(gw);
    let mut ev = Vec::new();
    cl.connect(clock::now_us(), init_payload());
    let t0 = Instant::now();
    let mut pushed = false;
    let mut seq = 0u32;
    let mut buf = [0u8; BASE_PAYLOAD];
    let mut rbuf = [0u8; 2048];
    while t0.elapsed() < Duration::from_secs(5) {
        let now = clock::now_us();
        while let Some(t) = cl.poll_transmit(now, &mut buf) {
            sock.send_to(&buf[..t.len], t.dst).unwrap();
        }
        while let Ok((n, from)) = sock.recv_from(&mut rbuf) {
            cl.handle_datagram(clock::now_us(), from, &mut rbuf[..n], &mut |e| ev.push(own(e)));
        }
        if cl.next_timeout().is_some_and(|t| t <= clock::now_us()) {
            cl.handle_timeout(clock::now_us(), &mut |e| ev.push(own(e)));
        }
        let established = ev.contains(&CEv::Established);
        if established && !pushed {
            pushed = true;
            assert!(h.client_present() || {
                std::thread::sleep(Duration::from_millis(20));
                h.client_present()
            });
        }
        if pushed && videos(&ev).len() < 5 && seq < 50 {
            seq += 1;
            assert!(h.push(MediaMsg::Video { seq, epoch: EPOCH, data: key_au(), frames_lost: 0, recovered: false, rx_us: now }));
            std::thread::sleep(Duration::from_millis(5));
        }
        if videos(&ev).len() >= 5 {
            break;
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    assert!(ev.contains(&CEv::Established), "{ev:?}");
    assert!(ev.iter().any(|e| matches!(e, CEv::Control(ControlMsg::VideoConfig(_)))), "{ev:?}");
    assert!(videos(&ev).len() >= 5, "收到画面：{ev:?}");
    assert!(h.status().client.is_some());
    assert!(erx.try_iter().any(|e| matches!(e, ServeEvent::ClientAttached { .. })));

    assert!(h.shutdown(Duration::from_millis(500)), "服务循环确认退出");
    let t1 = Instant::now();
    while t1.elapsed() < Duration::from_millis(500) && !ev.contains(&CEv::Closed(close_code::GATEWAY_EXIT)) {
        while let Ok((n, from)) = sock.recv_from(&mut rbuf) {
            cl.handle_datagram(clock::now_us(), from, &mut rbuf[..n], &mut |e| ev.push(own(e)));
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(ev.contains(&CEv::Closed(close_code::GATEWAY_EXIT)));
    assert!(!h.status().running);
    sink.assert_no_secrets();
}
