//! net 线程（QoS USER_INTERACTIVE；mio::Poll：UDP 绑配置里的网卡 + Waker）。
//!
//! 独占 ywp::ClientConn 和 yw-media 的 Assembler，收包后立即打单调时间戳。每轮事件循环：
//! 1. 收完 socket 里的包，逐个交给 ClientConn；视频块、FRAME_STATUS、VIDEO_CONFIG 当场交给 Assembler，
//!    收完整的帧当场经有界通道（64）交给 decode 线程；
//! 2. 取完其他线程发来的 NetIn（解码报错、要发的控制消息、断开）；
//! 3. 跑计时器（ClientConn、Assembler）；
//! 4. 处理 Assembler 的其余产出：IDR_REQUEST、NEED 交 ClientConn，REPORT 的视频字段用 `set_video_report` 填进去；
//! 5. 反复 `poll_transmit` 发包。WouldBlock 时注册 WRITABLE 等可写；ENOBUFS 按瞬时错误处理：
//!    这一包留着，1 ms 后重试，连续 3 次失败丢掉（macOS 的 UDP 在网卡输出队列满时返回 ENOBUFS，见 send(2)）。
//!
//! 重连：RESET、CLOSE(5)、1 s 没回音、30 s 静默都由 ClientConn 自己立即重新握手（规格 §4.4、§13.4）。
//! 每次新会话都换一个新的 Assembler：对端可能是新起的网关进程，fid 和 cfg_ver 会重新开始。
//! 交给 decode 线程的 fid 按会话加偏移，全程严格递增；decode 线程报回来的 fid 也按偏移还原。
//!
//! 音频、FEEDBACK 是 M2b 的事：这一阶段收到音频只计数，FEEDBACK 原样转给 input 线程。

use crate::clock;
use crate::config::GatewayProfile;
use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token, Waker};
use smallvec::SmallVec;
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use ywp::consts::{caps, close_code, ps5_cmd, BASE_PAYLOAD, PROTO_MIN_VERSION, PROTO_VERSION};
use ywp::creds::ClientCreds;
use ywp::crypto::{StaticKeypair, Tai64N};
use ywp::endpoint::{ClientConfig, ClientConn, ClientEvent, RxMeta};
use ywp::msg::{ControlMsg, Ps5Cmd, SessionStatus};
use ywp::net::{bound_udp, classify_send_err, SendOutcome};
use ywp::time::{ms, Micros, ENOBUFS_MAX_TRIES, ENOBUFS_RETRY, EXIT_CLOSE_WAIT};
use ywp::wire::{FeedbackState, HsErr, HsInitPayload, InputState};
use yw_media::audio_rx::AudioPacket;
use yw_media::playout::Policy;
use yw_media::video_rx::{AsmEvent, AsmStats, Assembler, DecodeItem};

/// DecodeItem 通道的容量（net → decode，有界 64）
pub const DECODE_QUEUE: usize = 64;
/// 音频包通道的容量（net → SDL 音频回调，M2b 起用）
pub const AUDIO_QUEUE: usize = 64;

const SOCK: Token = Token(0);
const WAKE: Token = Token(1);
/// 收包缓冲：比 RECV_MAX（1500）大，超长的包能被看出来并按 TooLong 丢掉，而不是被悄悄截断
const RECV_BUF: usize = 2048;
/// 一轮最多收多少个包，然后先跑计时器和发包（收不完的下一轮接着收，不等事件）
const RECV_BATCH: usize = 256;
/// 统计快照的刷新周期
const STATS_PERIOD: Micros = ms(100);
/// 连上之前排队的控制消息上限
const CTL_QUEUE_MAX: usize = 32;
/// 客户端名称（握手 TLV 0x07）。网关按配对表认人，这里只作提示，不带主机名等环境信息
const CLIENT_NAME: &str = "yw-client";

/// 其他线程 → net 线程
#[derive(Debug)]
pub enum NetIn {
    Input { now: Micros, state: InputState, discrete: SmallVec<[(u8, u8); 8]> },
    Control(ControlMsg),
    /// 解码出错的 fid（DecodeItem::fid，带会话偏移）
    DecoderError(u64),
    Disconnect,
}

/// net 线程 → 主线程（GUI 里经 SDL_PushEvent 叫醒主线程）
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum UiEvent {
    Connecting,
    /// 密钥已确认（第一次或重连后）
    Connected,
    /// BUSY、VERSION：握手停下（BUSY 的确认框在 M2d）
    Rejected(HsErr),
    /// 网关发来 CLOSE。code = 5 时会自动重连，其余停下
    Closed { code: u16 },
    Status(SessionStatus),
    Rtt(Micros),
    /// 1 s 没有任何包（M2d）
    NetworkDown,
    /// 媒体照常到达，但 1 s 没有 UP_REPORT 或 PONG（M2d）
    UplinkDown,
    /// 会话断了（RESET、CLOSE(5)、没回音、静默超时），正在重新握手
    Reconnecting,
    /// net 线程退出；带原因时是出了无法恢复的错误
    Stopped(Option<String>),
}

/// 主线程提供的投递函数（GUI 里调 SDL_PushEvent；无界面模式只进通道）
pub type UiSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

/// net 线程的统计快照（每 100 ms 刷新一次，退出前再刷一次）
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct NetStats {
    /// 开始握手的时刻（clock::now_us）
    pub connect_at: Micros,
    /// 第一次握手成功（收到 HS_RESP）时用了几次 HS_INIT；还没成功时是到现在为止的次数
    pub hs_attempts_first: u32,
    /// 一共用了几次 HS_INIT（含重连）
    pub hs_attempts_total: u32,
    /// 收到过 HS_RESP（区分「握手没回音」和「回了但 DATA 没通」）
    pub hs_resp_seen: bool,
    /// 第一次密钥确认的时刻
    pub first_established: Option<Micros>,
    /// 现在是否已确认
    pub established: bool,
    /// 确认过几次会话（重连一次加 1）
    pub sessions: u32,
    pub resets: u32,
    pub reconnects: u32,
    pub rejected: Option<HsErr>,
    /// 不会自动重连的 CLOSE
    pub closed: Option<u16>,
    pub srtt_us: Micros,
    /// 下行 DATA 包：收到的和推算丢掉的（按媒体包 PN 的空洞推算，M2a 近似）
    pub recv: u64,
    pub lost: u64,
    pub video_configs: u32,
    /// 历次会话的 Assembler 统计之和
    pub asm: AsmStats,
    /// 送进 decode 通道的帧
    pub frames_queued: u64,
    /// decode 通道满了丢掉的帧（之后按解码出错处理，等关键帧）
    pub decode_queue_full: u64,
    pub decoder_errors_reported: u64,
    pub idr_sent: u64,
    pub need_sent: u64,
    pub audio_chunks: u64,
    pub tx_packets: u64,
    pub send_enobufs: u64,
    pub send_dropped: u64,
    pub send_errors: u64,
    pub recv_errors: u64,
    /// ClientConn 丢弃的包（各种 DropReason 之和）
    pub conn_drops: u64,
    /// 最近一次 socket 错误（系统错误文字，不含地址）
    pub last_os_error: Option<String>,
}

pub struct NetHandle {
    pub tx: Sender<NetIn>,
    pub waker: Arc<mio::Waker>,
    pub join: JoinHandle<()>,
    /// 统计快照（主线程随时可读）
    pub stats: Arc<Mutex<NetStats>>,
}

impl NetHandle {
    /// 发一条消息并叫醒 net 线程；net 线程已经退出时返回 false
    pub fn send(&self, m: NetIn) -> bool {
        self.tx.send(m).is_ok() && self.waker.wake().is_ok()
    }

    pub fn stats(&self) -> NetStats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

/// 起 net 线程：建 socket（绑档案里的 bind_if，没写就不绑）、Poll 和 Waker，然后立即开始握手。
/// socket 建不起来（网卡名不对等）在这里就返回错误，不起线程
pub fn spawn_net(
    p: &GatewayProfile,
    keys: StaticKeypair,
    dec_tx: SyncSender<DecodeItem>,
    audio_tx: SyncSender<AudioPacket>,
    fb_tx: Sender<FeedbackState>,
    ui: UiSink,
) -> io::Result<NetHandle> {
    let local: SocketAddr = if p.entry.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }.parse().map_err(io::Error::other)?;
    let std_sock = bound_udp(local, p.bind_if.as_deref().unwrap_or(""))?;
    let mut sock = UdpSocket::from_std(std_sock);
    let poll = Poll::new()?;
    poll.registry().register(&mut sock, SOCK, Interest::READABLE)?;
    let waker = Arc::new(Waker::new(poll.registry(), WAKE)?);
    let (tx, cmd_rx) = mpsc::channel();
    let stats = Arc::new(Mutex::new(NetStats::default()));

    let cfg = ClientConfig {
        creds: ClientCreds { static_kp: keys, gw_pub: p.gw_pub, psk: p.psk.clone() },
        entry: p.entry,
        name: CLIENT_NAME.to_string(),
        bitrate_cap_kbps: p.bitrate_cap_kbps,
        takeover: false,
    };
    let pol = Policy { mode: p.mode, lan: false, j_override_us: p.j_ms.map(|j| j as u32 * 1000) };
    let lp = NetLoop {
        poll,
        sock,
        conn: ClientConn::new(cfg),
        mode_wire: p.mode.to_wire(),
        rx: RxSide::new(pol, dec_tx, audio_tx, fb_tx, ui),
        cmd_rx,
        shared: stats.clone(),
        st: NetStats::default(),
        out: Box::new([0u8; BASE_PAYLOAD]),
        staged: None,
        write_blocked: false,
        retry_at: None,
        next_publish: 0,
        closing_until: None,
        ctl_queue: Vec::new(),
        auto_start: p.auto_start,
        start_sent: false,
        req_id: 0,
        was_established: false,
        terminal: false,
        sess_rx: 0,
        more_to_read: false,
    };
    let join = std::thread::Builder::new().name("net".into()).spawn(move || lp.run())?;
    Ok(NetHandle { tx, waker, join, stats })
}

/// 还没发出去的一个包（留在 out 缓冲里）
#[derive(Clone, Copy, Debug)]
struct Staged {
    len: usize,
    dst: SocketAddr,
    tries: u8,
}

/// 收包回调里要碰的状态（和 ClientConn 分开，回调才能借用）
struct RxSide {
    pol: Policy,
    asm: Assembler,
    /// Assembler 的产出里除 Decode 以外的（IDR_REQUEST、NEED……），回调返回后由事件循环处理
    ev: Vec<AsmEvent>,
    /// 之前各个 Assembler 的统计
    asm_acc: AsmStats,
    /// 当前会话的 fid 偏移
    fid_base: u64,
    /// 已经交出去的最大 fid + 1（带偏移）
    next_fid: u64,
    dec_tx: SyncSender<DecodeItem>,
    dec_closed: bool,
    #[allow(dead_code)] // M2b 起把音频包交给 SDL 音频回调
    audio_tx: SyncSender<AudioPacket>,
    fb_tx: Sender<FeedbackState>,
    ui: UiSink,
    // ---- 回调里置的标志，回调返回后由事件循环处理 ----
    hs_ok: bool,
    established_at: Option<Micros>,
    reset: bool,
    closed: Option<u16>,
    rejected: Option<HsErr>,
    /// 当前会话里见过的最大 PN（推算丢包）
    sess_max_pn: Option<u64>,
    video_configs: u32,
    audio_chunks: u64,
    frames_queued: u64,
    decode_queue_full: u64,
}

impl RxSide {
    fn new(
        pol: Policy,
        dec_tx: SyncSender<DecodeItem>,
        audio_tx: SyncSender<AudioPacket>,
        fb_tx: Sender<FeedbackState>,
        ui: UiSink,
    ) -> Self {
        Self {
            pol,
            asm: Assembler::new(pol),
            ev: Vec::new(),
            asm_acc: AsmStats::default(),
            fid_base: 0,
            next_fid: 0,
            dec_tx,
            dec_closed: false,
            audio_tx,
            fb_tx,
            ui,
            hs_ok: false,
            established_at: None,
            reset: false,
            closed: None,
            rejected: None,
            sess_max_pn: None,
            video_configs: 0,
            audio_chunks: 0,
            frames_queued: 0,
            decode_queue_full: 0,
        }
    }

    /// 新会话：换一个新的 Assembler，fid 偏移接着上一个会话。旧会话没处理完的请求（IDR 等）作废
    fn new_assembler(&mut self, now: Micros) {
        self.forward_decodes(now);
        self.ev.clear();
        add_asm(&mut self.asm_acc, self.asm.stats());
        self.asm = Assembler::new(self.pol);
        self.fid_base = self.next_fid;
    }

    /// 把 ev 里收完整的帧立即交给 decode 线程（不等这一轮收包结束），其余的留在 ev 里
    fn forward_decodes(&mut self, now: Micros) {
        if !self.ev.iter().any(|e| matches!(e, AsmEvent::Decode(_))) {
            return;
        }
        let mut pending = std::mem::take(&mut self.ev);
        let mut keep = Vec::with_capacity(pending.len());
        while !pending.is_empty() {
            let mut more = Vec::new();
            for e in pending.drain(..) {
                match e {
                    AsmEvent::Decode(item) => self.forward(now, item, &mut more),
                    other => keep.push(other),
                }
            }
            pending = more;
        }
        self.ev = keep;
    }

    fn forward(&mut self, now: Micros, mut item: DecodeItem, more: &mut Vec<AsmEvent>) {
        let local = item.fid;
        item.fid += self.fid_base;
        self.next_fid = self.next_fid.max(item.fid + 1);
        match self.dec_tx.try_send(item) {
            Ok(()) => self.frames_queued += 1,
            Err(TrySendError::Full(_)) => {
                // decode 线程卡住了：这一帧丢掉，参考链断了，按解码出错处理（请求 IDR、等关键帧）
                self.decode_queue_full += 1;
                self.asm.on_decoder_error(now, local, more);
            }
            Err(TrySendError::Disconnected(_)) => self.dec_closed = true,
        }
    }

    fn note_pn(&mut self, m: &RxMeta) {
        self.sess_max_pn = Some(self.sess_max_pn.map_or(m.pn, |x| x.max(m.pn)));
    }

    fn on_event(&mut self, now: Micros, e: ClientEvent<'_>) {
        match e {
            ClientEvent::HandshakeOk(_) => {
                self.hs_ok = true;
                self.new_assembler(now);
            }
            ClientEvent::HandshakeRejected(e) => self.rejected = Some(e),
            ClientEvent::Established => self.established_at = Some(now),
            ClientEvent::Video(c, meta) => {
                self.note_pn(&meta);
                self.asm.on_video(now, &meta, &c, &mut self.ev);
            }
            ClientEvent::Audio(_, meta) => {
                self.note_pn(&meta);
                self.audio_chunks += 1;
            }
            ClientEvent::Feedback(f) => {
                let _ = self.fb_tx.send(f);
            }
            ClientEvent::FrameStatus(e) => self.asm.on_frame_status(now, &e, &mut self.ev),
            ClientEvent::Control(m) => self.on_control(now, m),
            ClientEvent::Closed { code } => self.closed = Some(code),
            ClientEvent::Reset => self.reset = true,
            // RTT 在统计快照里（srtt）；UP_REPORT 是 M2c；丢弃原因 ClientConn 自己计数
            ClientEvent::Rtt(_) | ClientEvent::UpReport(_) | ClientEvent::Dropped(_) => {}
        }
        self.forward_decodes(now);
    }

    fn on_control(&mut self, now: Micros, m: ControlMsg) {
        match m {
            ControlMsg::VideoConfig(vc) => {
                self.video_configs += 1;
                // M2a 只有一个档位（PS5 会话开始时定下的分辨率）；有多个时取第一个
                let params = vc.profiles.first().map(|p| p.param_sets.as_slice()).unwrap_or(&[]);
                self.asm.on_config(now, vc.cfg_ver, params, &mut self.ev);
            }
            ControlMsg::SessionStatus(s) => (self.ui)(UiEvent::Status(s)),
            ControlMsg::Error { code, text } => eprintln!("网关报错：code {code}，{text}"),
            _ => {}
        }
    }
}

/// 把一个 Assembler 的统计加进累计值
fn add_asm(acc: &mut AsmStats, s: &AsmStats) {
    acc.frames_ok += s.frames_ok;
    acc.frames_lost += s.frames_lost;
    acc.fec_recovered += s.fec_recovered;
    acc.w_frames += s.w_frames;
    acc.w_errors += s.w_errors;
    acc.decoder_errors += s.decoder_errors;
    acc.cfg_timeouts += s.cfg_timeouts;
    acc.idr_requests += s.idr_requests;
    acc.freezes += s.freezes;
}

struct NetLoop {
    poll: Poll,
    sock: UdpSocket,
    conn: ClientConn,
    mode_wire: u8,
    rx: RxSide,
    cmd_rx: Receiver<NetIn>,
    shared: Arc<Mutex<NetStats>>,
    st: NetStats,
    out: Box<[u8; BASE_PAYLOAD]>,
    staged: Option<Staged>,
    /// 发送遇到 WouldBlock，在等 WRITABLE
    write_blocked: bool,
    /// ENOBUFS 之后的重试时刻
    retry_at: Option<Micros>,
    next_publish: Micros,
    /// 断开时等 CLOSE 发完的截止时刻
    closing_until: Option<Micros>,
    ctl_queue: Vec<ControlMsg>,
    auto_start: bool,
    start_sent: bool,
    req_id: u16,
    was_established: bool,
    /// 被拒或被关了，不会自动重连
    terminal: bool,
    /// 当前会话最近一次看到的已收包数（会话结束后 ClientConn 就不再给出它，所以每次处理前先记下）
    sess_rx: u64,
    /// 上一轮收满了 RECV_BATCH，socket 里可能还有
    more_to_read: bool,
}

impl NetLoop {
    fn init_payload(&self) -> HsInitPayload {
        HsInitPayload {
            ts: Tai64N::now(),
            ver: PROTO_VERSION,
            min_ver: PROTO_MIN_VERSION,
            caps: caps::SPLIT2 | caps::HEVC,
            resume: None,
            last_fid: None,
            mode: Some((self.mode_wire, 0)),
            name: None,
            bitrate_cap_kbps: None,
        }
    }

    fn run(mut self) {
        crate::platform::set_thread_qos_interactive();
        let now = clock::now_us();
        self.st.connect_at = now;
        let init = self.init_payload();
        self.conn.connect(now, init);
        (self.rx.ui)(UiEvent::Connecting);
        let mut events = Events::with_capacity(64);
        let mut buf = vec![0u8; RECV_BUF];
        let mut fatal: Option<String> = None;
        loop {
            let now = clock::now_us();
            let timeout = if self.more_to_read {
                Some(Duration::ZERO)
            } else {
                self.next_wake(now).map(|t| Duration::from_micros(t.saturating_sub(now)))
            };
            if let Err(e) = self.poll.poll(&mut events, timeout) {
                if e.kind() != io::ErrorKind::Interrupted {
                    fatal = Some(format!("等待网络事件失败：{e}"));
                    break;
                }
            }
            let writable = events.iter().any(|ev| ev.token() == SOCK && ev.is_writable());
            if writable && self.write_blocked {
                self.write_blocked = false;
                if let Err(e) = self.poll.registry().reregister(&mut self.sock, SOCK, Interest::READABLE) {
                    fatal = Some(format!("注册 socket 失败：{e}"));
                    break;
                }
            }
            self.recv_batch(&mut buf);
            self.drain_cmds();
            let now = clock::now_us();
            if self.conn.next_timeout().is_some_and(|t| t <= now) {
                self.snap_rx();
                let rx = &mut self.rx;
                self.conn.handle_timeout(now, &mut |e| rx.on_event(now, e));
            }
            if self.rx.asm.next_timer().is_some_and(|t| t <= now) {
                self.rx.asm.on_timer(now, &mut self.rx.ev);
                self.rx.forward_decodes(now);
            }
            self.after_events();
            self.flush_tx(now);
            if now >= self.next_publish {
                self.next_publish = now + STATS_PERIOD;
                self.publish();
            }
            if self.rx.dec_closed && self.closing_until.is_none() {
                // decode 线程没了：没法再出画面，体面地断开
                self.begin_close(now);
            }
            if let Some(until) = self.closing_until {
                if self.conn.next_timeout().is_none() || now >= until {
                    break;
                }
            }
        }
        self.publish();
        (self.rx.ui)(UiEvent::Stopped(fatal));
    }

    /// 下一次要醒来的时刻
    fn next_wake(&self, now: Micros) -> Option<Micros> {
        let mut t: Option<Micros> = None;
        let mut at = |x: Micros| t = Some(t.map_or(x, |y| y.min(x)));
        let blocked = self.write_blocked || self.retry_at.is_some();
        if let Some(c) = self.conn.next_timeout() {
            // 发不出去的时候，「马上要发」的超时不能让循环空转：最早 1 ms 后再看
            at(if blocked { c.max(now + ENOBUFS_RETRY) } else { c });
        }
        if let Some(a) = self.rx.asm.next_timer() {
            at(a);
        }
        if let Some(r) = self.retry_at {
            at(r);
        }
        if let Some(c) = self.closing_until {
            at(c);
        }
        at(self.next_publish);
        t
    }

    /// 记下当前会话的已收包数（在可能结束会话的调用之前）
    fn snap_rx(&mut self) {
        if self.conn.is_established() {
            self.sess_rx = self.conn.stats().rx_packets;
        }
    }

    /// 收完 socket 里的包（一轮最多 RECV_BATCH 个）。每个包收到后立即打时间戳
    fn recv_batch(&mut self, buf: &mut [u8]) {
        self.more_to_read = false;
        for _ in 0..RECV_BATCH {
            match self.sock.recv_from(buf) {
                Ok((n, from)) => {
                    let now = clock::now_us();
                    self.snap_rx();
                    let rx = &mut self.rx;
                    self.conn.handle_datagram(now, from, &mut buf[..n], &mut |e| rx.on_event(now, e));
                    if self.rx.hs_ok {
                        self.on_new_session();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    // UDP 上的 ICMP 类错误（ECONNREFUSED 等）：计数，下一轮接着收
                    self.st.recv_errors += 1;
                    self.st.last_os_error = Some(e.to_string());
                    return;
                }
            }
        }
        self.more_to_read = true;
    }

    /// 收到 HS_RESP、建了新会话：上一个会话的收包数并进累计（Assembler 已在回调里换新）
    fn on_new_session(&mut self) {
        self.rx.hs_ok = false;
        if !self.st.hs_resp_seen {
            self.st.hs_resp_seen = true;
            self.st.hs_attempts_first = self.conn.stats().hs_attempts;
        }
        let expect = self.rx.sess_max_pn.map_or(0, |p| p + 1);
        self.st.recv += self.sess_rx;
        self.st.lost += expect.saturating_sub(self.sess_rx);
        self.sess_rx = 0;
        self.rx.sess_max_pn = None;
    }

    /// 处理其他线程的消息
    fn drain_cmds(&mut self) {
        loop {
            match self.cmd_rx.try_recv() {
                Ok(NetIn::Input { now, state, discrete }) => self.conn.input(now, state, &discrete),
                Ok(NetIn::Control(m)) => self.send_control(m),
                Ok(NetIn::DecoderError(fid)) => {
                    self.st.decoder_errors_reported += 1;
                    // 上一个会话的帧报的错不用管了：新会话已经从关键帧重新开始
                    if fid >= self.rx.fid_base {
                        let now = clock::now_us();
                        self.rx.asm.on_decoder_error(now, fid - self.rx.fid_base, &mut self.rx.ev);
                        self.rx.forward_decodes(now);
                    }
                }
                Ok(NetIn::Disconnect) => self.begin_close(clock::now_us()),
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    // 主线程那边的发送端都没了：当作断开
                    self.begin_close(clock::now_us());
                    return;
                }
            }
        }
    }

    fn begin_close(&mut self, now: Micros) {
        if self.closing_until.is_some() {
            return;
        }
        self.snap_rx();
        self.conn.close(close_code::NORMAL, "");
        self.closing_until = Some(now + EXIT_CLOSE_WAIT);
    }

    fn send_control(&mut self, m: ControlMsg) {
        if self.conn.is_established() {
            if let Err(e) = self.conn.send_control(&m) {
                eprintln!("控制消息没发出去：{e:?}");
            }
        } else if self.ctl_queue.len() < CTL_QUEUE_MAX {
            self.ctl_queue.push(m);
        }
    }

    /// 回调置的标志和 Assembler 的其余产出
    fn after_events(&mut self) {
        if self.rx.hs_ok {
            self.on_new_session();
        }
        if let Some(at) = self.rx.established_at.take() {
            self.st.sessions += 1;
            if self.st.first_established.is_none() {
                self.st.first_established = Some(at);
            }
            (self.rx.ui)(UiEvent::Connected);
            for m in std::mem::take(&mut self.ctl_queue) {
                if let Err(e) = self.conn.send_control(&m) {
                    eprintln!("控制消息没发出去：{e:?}");
                }
            }
            if self.auto_start && !self.start_sent {
                // 只在第一次连上时发：重连后不再替用户唤醒 PS5（只按用户意图唤醒）
                self.start_sent = true;
                let mut c = Ps5Cmd::default();
                self.req_id = self.req_id.wrapping_add(1);
                c.req_id = self.req_id;
                c.cmd = ps5_cmd::START;
                c.flags = ps5_cmd::FLAG_UNPAUSE;
                if let Err(e) = self.conn.send_control(&ControlMsg::Ps5Cmd(c)) {
                    eprintln!("PS5_CMD START 没发出去：{e:?}");
                }
            }
        }
        if std::mem::take(&mut self.rx.reset) {
            self.st.resets += 1;
        }
        if let Some(code) = self.rx.closed.take() {
            if code != close_code::GATEWAY_EXIT {
                self.st.closed = Some(code);
                self.terminal = true;
            }
            (self.rx.ui)(UiEvent::Closed { code });
        }
        if let Some(e) = self.rx.rejected.take() {
            self.st.rejected = Some(e);
            self.terminal = true;
            (self.rx.ui)(UiEvent::Rejected(e));
        }
        let est = self.conn.is_established();
        if self.was_established && !est && !self.terminal && self.closing_until.is_none() {
            self.st.reconnects += 1;
            (self.rx.ui)(UiEvent::Reconnecting);
        }
        self.was_established = est;
        for e in std::mem::take(&mut self.rx.ev) {
            match e {
                AsmEvent::IdrRequest(reason) => {
                    // 还没连上时发不出去：Assembler 等不到关键帧会在 1.2 s 后再请求
                    if self.conn.send_control(&ControlMsg::IdrRequest { reason }).is_ok() {
                        self.st.idr_sent += 1;
                    }
                }
                AsmEvent::Need(n) => {
                    self.conn.send_need(n);
                    self.st.need_sent += 1;
                }
                // Decode 已在回调里交出去了；冻结和统计事件看 AsmStats
                AsmEvent::Decode(_) | AsmEvent::Freeze(_) | AsmEvent::Unfreeze | AsmEvent::Stat(_) => {}
            }
        }
        self.conn.set_video_report(self.rx.asm.report());
    }

    /// 反复 poll_transmit 直到没有要发的
    fn flush_tx(&mut self, now: Micros) {
        if self.write_blocked {
            return;
        }
        if let Some(at) = self.retry_at {
            if now < at {
                return;
            }
            self.retry_at = None;
        }
        loop {
            if self.staged.is_none() {
                match self.conn.poll_transmit(now, &mut self.out) {
                    Some(t) => self.staged = Some(Staged { len: t.len, dst: t.dst, tries: 0 }),
                    None => return,
                }
            }
            let Some(s) = self.staged.as_mut() else { return };
            let e = match self.sock.send_to(&self.out[..s.len], s.dst) {
                Ok(_) => {
                    self.staged = None;
                    self.st.tx_packets += 1;
                    continue;
                }
                Err(e) => e,
            };
            match classify_send_err(&e) {
                SendOutcome::Sent => self.staged = None,
                SendOutcome::WouldBlock => {
                    self.write_blocked = true;
                    let both = Interest::READABLE | Interest::WRITABLE;
                    if let Err(e) = self.poll.registry().reregister(&mut self.sock, SOCK, both) {
                        // 注册不上就退回定时重试
                        self.st.last_os_error = Some(e.to_string());
                        self.write_blocked = false;
                        self.retry_at = Some(now + ENOBUFS_RETRY);
                    }
                    return;
                }
                SendOutcome::NoBufs => {
                    self.st.send_enobufs += 1;
                    s.tries += 1;
                    if s.tries >= ENOBUFS_MAX_TRIES {
                        self.staged = None;
                        self.st.send_dropped += 1;
                        continue;
                    }
                    self.retry_at = Some(now + ENOBUFS_RETRY);
                    return;
                }
                SendOutcome::Fatal(e) => {
                    // 网卡没地址、路由不通等：这一包丢掉，握手和会话自己会重试
                    self.st.send_errors += 1;
                    self.st.last_os_error = Some(e.to_string());
                    self.staged = None;
                }
            }
        }
    }

    /// 刷新统计快照
    fn publish(&mut self) {
        let cs = self.conn.stats();
        self.st.hs_attempts_total = cs.hs_attempts.max(self.st.hs_attempts_total);
        if !self.st.hs_resp_seen {
            self.st.hs_attempts_first = self.st.hs_attempts_total;
        }
        self.st.established = self.conn.is_established();
        if self.st.established {
            self.st.srtt_us = cs.srtt_us;
            self.sess_rx = cs.rx_packets;
        }
        self.st.conn_drops = cs.drops.snapshot().iter().map(|(_, n)| *n).sum();
        self.st.video_configs = self.rx.video_configs;
        self.st.audio_chunks = self.rx.audio_chunks;
        self.st.frames_queued = self.rx.frames_queued;
        self.st.decode_queue_full = self.rx.decode_queue_full;
        let mut asm = self.rx.asm_acc.clone();
        add_asm(&mut asm, self.rx.asm.stats());
        self.st.asm = asm;
        // 当前会话的收包、丢包也算进去（不改累计值）
        let mut snap = self.st.clone();
        let expect = self.rx.sess_max_pn.map_or(0, |p| p + 1);
        snap.recv += self.sess_rx;
        snap.lost += expect.saturating_sub(self.sess_rx);
        if let Ok(mut g) = self.shared.lock() {
            *g = snap;
        }
    }
}

#[cfg(test)]
mod tests;
