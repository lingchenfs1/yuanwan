//! 服务循环（ywp 线程）：mio::Poll 等 UDP socket（绑配置里的网卡）和 Waker；
//! 超时取所有状态机 next_timeout 的最小值。每轮：收完 socket → 取完 MediaMsg → 跑计时器 → 组包、加密、send_to。
//!
//! 发送（规格 §6、附录 B「发送遇到 ENOBUFS」）：
//! - WouldBlock：这一包留在原处，注册 WRITABLE，等可写再发；
//! - ENOBUFS（macOS 的 UDP 在网卡输出队列满时返回它，不是 EAGAIN）：按瞬时错误处理，计数，这一包留在原处，
//!   1 ms 后重试，连续 3 次失败就丢掉这一包（视频符号之后走修复）。WRITABLE 等不到这种情况，所以用计时器；
//! - 其他发送错误：丢掉这一包、计数，不中断事件循环。
//!
//! 视频（规格 §7.1 的 v0.2 注、§7.4、§8.5）：
//! - MediaMsg::Video → Ingress（fid 分配、WaitKey）→ VideoTx（切片、RS）→ 端点调度；
//! - epoch 变了：Ingress::on_epoch、set_stream（PsControl::stream_info），再从 PsControl::params 补参数集；
//!   参数集的通知丢了也能补（当前会话还没有 VIDEO_CONFIG 时每帧都去最新值槽取一次）；
//! - 新客户端接入：先发 SESSION_STATUS（知道 PS5 状态时）和当前的 VIDEO_CONFIG，再 set_client_present(true)、
//!   wait_key(NewClient)：请求 IDR、发 KEY_PENDING，从下一个关键帧开始转发；
//! - 视频回调在队列满时丢了关键帧（ServeHandle::note_key_overflow）：WaitKey(Overflow)；
//! - 客户端 IDR_REQUEST：经 PsControl（内部 IdrGovernor 限频）请求 IDR；
//! - NEED：缓存里有就补发（VideoTx::on_need），没有回 FRAME_STATUS(GONE)；带 K 位时请求 IDR 并回 KEY_PENDING；
//!   Ingress 正在等关键帧时也回 KEY_PENDING。
//!
//! M2a 不转发声音和手柄（INPUT 只计数）；PS5_CMD、REBUILD_CONSENT 在 M2d 接。

use crate::clock;
use crate::idr::{IdrDecision, IdrReason};
use crate::ingress::{Ingress, IngressOut, IngressStats, RecoveredPolicy, VideoIn, WaitWhy};
use crate::source::{DetachWhy, MediaMsg, PsControl, ServeEvent};
use mio::{Events, Interest, Poll, Token, Waker};
use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use yw_media::parity::Mode;
use yw_media::video_tx::{FrameIn, NeedReply, TxParams, VideoTx, VideoTxConfig};
use ywp::consts::{close_code, BASE_PAYLOAD};
use ywp::creds::{ClientEntry, ClientId, Keys};
use ywp::endpoint::{Gateway, GatewayConfig, GwEvent, TakeoverPolicy};
use ywp::msg::{ControlMsg, SessionStatus};
use ywp::net::{classify_send_err, SendOutcome};
use ywp::sched::VideoCarrier;
use ywp::time::{ms, Micros, CLOSE_COPIES, ENOBUFS_MAX_TRIES, ENOBUFS_RETRY, EXIT_CLOSE_WAIT};
use ywp::wire::{FrameStatusEntry, FrameStatusKind, Need, Report, ReportVideoFields, SessionInfo};
use ywp::{DropCounters, DropReason};

/// 日志出口。ywp 线程写 JSONL 一律经它；实现方要用有界队列交给日志线程，满了丢弃并计数，不能阻塞
pub trait JsonlSink: Send + Sync {
    /// 结构化记录：fields 是拼好的 `"k":v,...`（同 yw-gateway 的 jlog::Logger::record）
    fn record(&self, kind: &str, fields: &str);
    /// 人读的日志：level 为 'I'、'W'、'E'
    fn log(&self, level: char, kind: &str, msg: &str);
}

pub struct ServeConfig {
    /// gateway.conf 的 ywp_port（默认 9420）；0 表示让系统挑（测试用）
    pub port: u16,
    /// gateway.conf 的 ywp_bind_if；空串表示不绑网卡
    pub bind_if: String,
    pub takeover: TakeoverPolicy,
    pub log: Arc<dyn JsonlSink>,
}

/// spawn 的可选项（`ServeOptions { keys_dir: Some(..), ..Default::default() }`）
#[derive(Clone, Debug, Default)]
pub struct ServeOptions {
    /// keys/ 目录：控制口 `clients-reload` 从这里重读已配对客户端。None 时不支持 reload
    pub keys_dir: Option<PathBuf>,
    /// recovered 帧怎么处理（默认 Forward，T31、T41 的结论）
    pub recovered: RecoveredPolicy,
    /// 绑哪个本地地址。None = 0.0.0.0（配合 bind_if 绑网卡）；测试用 127.0.0.1
    pub bind_ip: Option<IpAddr>,
}

/// MediaMsg 队列的容量（sync_channel）
pub const MEDIA_QUEUE_CAP: usize = 1024;

// ---- 本模块的私有常量（规格里没有具体值的实现细节；需要时交协调人收进 consts.rs / time.rs） ----

/// 每轮最多收这么多包、取这么多条 MediaMsg、发这么多包，剩下的下一轮立即接着做（不让一边饿死另一边）
const RECV_BUDGET: usize = 256;
const MEDIA_BUDGET: usize = 64;
const SEND_BUDGET: usize = 1024;
/// 收包缓冲比 RECV_MAX 大一些：超长的包能被看出来（端点按 TooLong 丢），不会被静默截断
const RECV_BUF: usize = 2048;
/// 有客户端时 Ingress、VideoTx 的计时（300 ms 无帧等）每 10 ms 跑一次；没有客户端时 100 ms
const HOUSEKEEP_ACTIVE: Micros = ms(10);
const HOUSEKEEP_IDLE: Micros = ms(100);
/// 控制口 status 用的摘要多久刷新一次
const STATUS_EVERY: Micros = ms(250);
/// JSONL 统计多久写一次
const STATS_EVERY: Micros = ms(10_000);
/// 同一类警告最快多久写一次（发送错误、帧错误）
const WARN_GAP: Micros = ms(5_000);
/// 截止时间已过、这一轮又什么都没做时，最少等这么久（防止状态机的 next_timeout 不前进时空转）
const SPIN_GUARD: Micros = ms(1);
/// 关闭时最后一份 CLOSE 之后再多等一点，让它真正发出去
const CLOSE_SETTLE: Micros = ms(2);

const SOCK: Token = Token(0);
const WAKER: Token = Token(1);

/// 服务循环和其他线程（chiaki 回调、监督线程、控制口）共享的状态
#[derive(Debug, Default)]
struct ServeShared {
    /// 有已确认的客户端（chiaki 回调据此决定 push 失败时返回 false 还是 true）
    client_present: AtomicBool,
    /// 视频回调在队列满时丢了一个关键帧：服务循环下一帧之前进 WaitKey(Overflow)
    key_overflow: AtomicBool,
    /// push 失败次数（DropReason::MediaQueueFull）
    media_full: AtomicU64,
    status: Mutex<ServeStatus>,
}

pub struct ServeHandle {
    tx: SyncSender<MediaMsg>,
    waker: Arc<mio::Waker>,
    shared: Arc<ServeShared>,
    local: SocketAddr,
    pub join: JoinHandle<()>,
}

impl ServeHandle {
    /// 在 chiaki 线程上调：try_send + wake，队列满了返回 false（视频回调据此返回 false）
    pub fn push(&self, m: MediaMsg) -> bool {
        match self.tx.try_send(m) {
            Ok(()) => {
                let _ = self.waker.wake();
                true
            }
            Err(_) => {
                self.shared.media_full.fetch_add(1, Relaxed);
                false
            }
        }
    }

    /// 现在有没有已确认的客户端（只是提示：读到的值可能已经过时一拍）
    pub fn client_present(&self) -> bool {
        self.shared.client_present.load(Relaxed)
    }

    /// 视频回调 push 失败、而那一帧是关键帧时调：服务循环据此进 WaitKey(Overflow)
    pub fn note_key_overflow(&self) {
        self.shared.key_overflow.store(true, Relaxed);
    }

    /// 实际绑上的地址（端口为 0 时由系统挑）
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// 控制口 status 用的摘要（最多晚 250 ms）
    pub fn status(&self) -> ServeStatus {
        let mut s = self.shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        s.counters.media_full = self.shared.media_full.load(Relaxed);
        s
    }

    /// 给客户端发 3 份 CLOSE(5)（网关进程退出），最多等 200 ms，然后服务循环退出。
    /// 返回服务循环是否按时确认了
    pub fn shutdown(&self, wait: Duration) -> bool {
        let (ack, ack_rx) = mpsc::channel();
        let deadline = Instant::now() + wait;
        let mut msg = MediaMsg::Shutdown { ack };
        loop {
            match self.tx.try_send(msg) {
                Ok(()) => {
                    let _ = self.waker.wake();
                    break;
                }
                Err(TrySendError::Full(m)) if Instant::now() < deadline => {
                    msg = m;
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return false,
            }
        }
        ack_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok()
    }
}

/// 控制口 status 里的 YWP 摘要
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ServeStatus {
    pub running: bool,
    pub port: u16,
    pub bind_if: String,
    /// 已配对的客户端数
    pub paired: usize,
    pub client: Option<ClientStatus>,
    pub counters: ServeCounters,
    pub ingress: IngressStats,
    /// Ingress 在等关键帧的原因
    pub waiting_key: Option<WaitWhy>,
    pub cfg_ver: Option<u8>,
    pub epoch: Option<u32>,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ClientStatus {
    pub name: String,
    pub id: ClientId,
    pub peer: SocketAddr,
    pub connected_ms: u64,
    pub srtt_us: Micros,
    pub hs_attempts: u32,
    pub rx_packets: u64,
    pub tx_packets: u64,
    /// 最近一个 REPORT：视频部分、第一条路径的 recv_cnt、lost_cnt
    pub report: Option<(ReportVideoFields, u32, u32)>,
}

/// 服务循环的计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServeCounters {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    /// 遇到 ENOBUFS 的次数（含重试）
    pub send_enobufs: u64,
    /// 连续 3 次 ENOBUFS 丢掉的包
    pub send_nobufs_dropped: u64,
    pub send_wouldblock: u64,
    /// 其他发送错误（丢包）
    pub send_errors: u64,
    pub recv_errors: u64,
    pub media_msgs: u64,
    /// chiaki 回调 push 失败（从共享计数取）
    pub media_full: u64,
    /// 交给 VideoTx 的帧
    pub frames_sent: u64,
    pub frame_errors: u64,
    pub video_carriers: u64,
    pub key_pending: u64,
    pub gone: u64,
    pub idr_requested: u64,
    pub client_idr: u64,
    pub needs: u64,
    pub inputs: u64,
    pub ctl_send_fail: u64,
    pub attaches: u64,
    pub detaches: u64,
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn opt_json<T>(v: Option<T>, f: impl FnOnce(T) -> String) -> String {
    v.map(f).unwrap_or_else(|| "null".into())
}

impl ServeStatus {
    /// 一行 JSON（不含任何密钥、PSK）
    pub fn to_json(&self) -> String {
        let c = &self.counters;
        let i = &self.ingress;
        let client = opt_json(self.client.as_ref(), |cl| {
            let report = opt_json(cl.report, |(v, recv, lost)| {
                format!(
                    "{{\"recv\":{recv},\"lost\":{lost},\"frames_ok\":{},\"frames_lost\":{},\"fec_recovered\":{},\"oldest_needed_fid\":{}}}",
                    v.frames_ok, v.frames_lost, v.fec_recovered, v.oldest_needed_fid
                )
            });
            format!(
                "{{\"name\":{},\"id\":\"{}\",\"peer\":\"{}\",\"connected_s\":{:.1},\"srtt_ms\":{:.1},\"hs_attempts\":{},\"rx_packets\":{},\"tx_packets\":{},\"report\":{report}}}",
                json_str(&cl.name),
                cl.id,
                cl.peer,
                cl.connected_ms as f64 / 1000.0,
                cl.srtt_us as f64 / 1000.0,
                cl.hs_attempts,
                cl.rx_packets,
                cl.tx_packets
            )
        });
        format!(
            "{{\"running\":{},\"port\":{},\"bind_if\":{},\"paired\":{},\"client\":{client},\"epoch\":{},\"cfg_ver\":{},\"waiting_key\":{},\
             \"frames_in\":{},\"forwarded\":{},\"key_forwarded\":{},\"w_forwarded\":{},\"seq_gaps\":{},\"seq_missing\":{},\
             \"dropped_wait_key\":{},\"rx_packets\":{},\"tx_packets\":{},\"tx_bytes\":{},\"send_enobufs\":{},\"send_nobufs_dropped\":{},\
             \"send_wouldblock\":{},\"send_errors\":{},\"media_full\":{},\"frame_errors\":{},\"key_pending\":{},\"idr_requested\":{},\
             \"client_idr\":{},\"attaches\":{},\"detaches\":{}}}",
            self.running,
            self.port,
            json_str(&self.bind_if),
            self.paired,
            opt_json(self.epoch, |e| format!("\"{e:08x}\"")),
            opt_json(self.cfg_ver, |v| v.to_string()),
            opt_json(self.waiting_key, |w| format!("\"{w:?}\"")),
            i.frames_in,
            i.forwarded,
            i.key_forwarded,
            i.w_forwarded,
            i.seq_gaps,
            i.seq_missing,
            i.dropped_wait_key,
            c.rx_packets,
            c.tx_packets,
            c.tx_bytes,
            c.send_enobufs,
            c.send_nobufs_dropped,
            c.send_wouldblock,
            c.send_errors,
            c.media_full,
            c.frame_errors,
            c.key_pending,
            c.idr_requested,
            c.client_idr,
            c.attaches,
            c.detaches,
        )
    }
}

// ======================================================================
// socket 抽象：正式实现是 mio 的 UdpSocket，单测用假的（注入 ENOBUFS、WouldBlock）
// ======================================================================

pub(crate) trait DgramSock {
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)>;
    fn send_to(&self, buf: &[u8], dst: SocketAddr) -> io::Result<usize>;
}

impl DgramSock for mio::net::UdpSocket {
    fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        mio::net::UdpSocket::recv_from(self, buf)
    }
    fn send_to(&self, buf: &[u8], dst: SocketAddr) -> io::Result<usize> {
        mio::net::UdpSocket::send_to(self, buf, dst)
    }
}

/// 发不出去、留在原处的一包（已经加密，PN 已占用，重试时原样再发）
struct PendingTx {
    buf: Box<[u8; BASE_PAYLOAD]>,
    len: usize,
    dst: SocketAddr,
    video: Option<(u16, u16)>,
    /// 已经遇到几次 ENOBUFS
    tries: u8,
    retry_at: Micros,
}

/// 端点事件的拥有版（REPORT 借用收包缓冲，在回调里当场处理，不进这里）
enum Ev {
    Connected { client: ClientId, cid: u64, init_name: Option<String> },
    Replaced,
    Input,
    Lost(Vec<(u16, u16)>),
    Need(Need),
    Control(ControlMsg),
    Closed(u16),
    PathChanged(SocketAddr),
}

struct Attached {
    cid: u64,
    client: ClientId,
    name: String,
    since: Micros,
}

/// 关闭中：3 份 CLOSE(5) 发完（或等满 200 ms）就退出
struct Closing {
    ack: Option<Sender<()>>,
    since: Micros,
}

pub(crate) struct Server<S: DgramSock> {
    sock: S,
    gw: Gateway,
    vtx: VideoTx,
    ing: Ingress,
    ctl: Arc<dyn PsControl>,
    events: Sender<ServeEvent>,
    log: Arc<dyn JsonlSink>,
    rx: Receiver<MediaMsg>,
    shared: Arc<ServeShared>,
    keys_dir: Option<PathBuf>,
    /// 已配对客户端的名字（按指纹）
    names: Vec<(ClientId, String)>,
    port: u16,
    bind_if: String,
    epoch: Option<u32>,
    ps_info: Option<SessionInfo>,
    attached: Option<Attached>,
    tx_params: TxParams,
    pending: Option<PendingTx>,
    /// pending 是 WouldBlock 留下的：等 WRITABLE
    blocked: bool,
    c: ServeCounters,
    send_drops: DropCounters,
    last_report: Option<(ReportVideoFields, u32, u32)>,
    next_house: Micros,
    next_status: Micros,
    next_stats: Micros,
    last_warn: HashMap<&'static str, Micros>,
    closing: Option<Closing>,
    exit: bool,
    /// 上一轮有没有实际干活（收、取、发）
    worked: bool,
    out: Box<[u8; BASE_PAYLOAD]>,
    rbuf: Box<[u8; RECV_BUF]>,
    evs: Vec<Ev>,
    carriers: Vec<VideoCarrier>,
}

fn names_of(clients: &[ClientEntry]) -> Vec<(ClientId, String)> {
    clients.iter().map(|c| (ClientId::of(&c.public), c.name.clone())).collect()
}

/// 端点回调：REPORT 当场交给 VideoTx，其余转成拥有版留到回调之后处理
fn collect(
    now: Micros,
    e: GwEvent<'_>,
    vtx: &mut VideoTx,
    evs: &mut Vec<Ev>,
    rep: &mut Option<(ReportVideoFields, u32, u32)>,
) {
    match e {
        GwEvent::Connected { cid, client, init } => evs.push(Ev::Connected { client, cid: cid.0, init_name: init.name }),
        GwEvent::Replaced { .. } => evs.push(Ev::Replaced),
        GwEvent::Input(..) => evs.push(Ev::Input),
        GwEvent::Lost(l) => evs.push(Ev::Lost(l.to_vec())),
        GwEvent::Need(n) => evs.push(Ev::Need(n)),
        GwEvent::Report(r) => {
            on_report(now, &r, vtx, rep);
        }
        GwEvent::Control(m) => evs.push(Ev::Control(m)),
        GwEvent::Closed(code) => evs.push(Ev::Closed(code)),
        GwEvent::PathChanged(a) => evs.push(Ev::PathChanged(a)),
        // M2d；丢弃原因端点自己计数
        GwEvent::Liveness(_) | GwEvent::Dropped(_) => {}
    }
}

fn on_report(now: Micros, r: &Report<'_>, vtx: &mut VideoTx, rep: &mut Option<(ReportVideoFields, u32, u32)>) {
    vtx.on_report(now, r);
    let (recv, lost) = r.paths().next().map_or((0, 0), |p| (p.recv_cnt, p.lost_cnt));
    *rep = Some((r.video, recv, lost));
}

impl<S: DgramSock> Server<S> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        sock: S,
        cfg: &ServeConfig,
        opts: &ServeOptions,
        keys: Keys,
        ctl: Arc<dyn PsControl>,
        events: Sender<ServeEvent>,
        rx: Receiver<MediaMsg>,
        shared: Arc<ServeShared>,
        now: Micros,
    ) -> Self {
        let names = names_of(&keys.clients);
        let mut srv = Self {
            sock,
            gw: Gateway::new(GatewayConfig { keys, takeover: cfg.takeover }),
            vtx: VideoTx::new(VideoTxConfig::default()),
            ing: Ingress::new(opts.recovered),
            ctl,
            events,
            log: cfg.log.clone(),
            rx,
            shared,
            keys_dir: opts.keys_dir.clone(),
            names,
            port: cfg.port,
            bind_if: cfg.bind_if.clone(),
            epoch: None,
            ps_info: None,
            attached: None,
            tx_params: TxParams::default(),
            pending: None,
            blocked: false,
            c: ServeCounters::default(),
            send_drops: DropCounters::new(),
            last_report: None,
            next_house: now,
            next_status: now,
            next_stats: now + STATS_EVERY,
            last_warn: HashMap::new(),
            closing: None,
            exit: false,
            worked: false,
            out: Box::new([0u8; BASE_PAYLOAD]),
            rbuf: Box::new([0u8; RECV_BUF]),
            evs: Vec::new(),
            carriers: Vec::new(),
        };
        srv.publish_status(now);
        srv
    }

    /// 一轮：收完 socket → 取完 MediaMsg → 跑计时器 → 组包发送。返回 true 表示预算用完、还有活，下一轮立即接着做。
    /// writable：上一次 poll 报告了 socket 可写
    pub(crate) fn turn(&mut self, now: Micros, writable: bool) -> bool {
        let (rx_more, rx_n) = self.recv_all(now);
        let (media_more, media_n) = self.drain_media(now);
        self.timers(now);
        let (tx_more, tx_n) = self.flush(now, writable);
        self.check_closing(now);
        self.worked = rx_n + media_n + tx_n > 0;
        rx_more || media_more || tx_more
    }

    /// 下一次要醒的时刻
    pub(crate) fn next_deadline(&self, now: Micros) -> Micros {
        let mut t = self.next_house.min(self.next_status).min(self.next_stats);
        if let Some(g) = self.gw.next_timeout() {
            // 有一包挂着时发送被堵住：端点「随时可发」的截止时间不算，等重试或 WRITABLE
            let g = match &self.pending {
                Some(p) if !self.blocked => g.max(p.retry_at),
                Some(_) => g.max(now + SPIN_GUARD),
                None => g,
            };
            t = t.min(g);
        }
        if let Some(p) = &self.pending {
            if !self.blocked {
                t = t.min(p.retry_at);
            }
        }
        if let Some(c) = &self.closing {
            t = t.min(c.since + CLOSE_COPIES[CLOSE_COPIES.len() - 1] + CLOSE_SETTLE).min(c.since + EXIT_CLOSE_WAIT);
        }
        if t <= now && !self.worked {
            t = now + SPIN_GUARD;
        }
        t
    }

    pub(crate) fn wants_writable(&self) -> bool {
        self.blocked
    }

    // ------------------------------------------------------------------
    // 收包
    // ------------------------------------------------------------------

    fn recv_all(&mut self, now: Micros) -> (bool, usize) {
        for n in 0..RECV_BUDGET {
            let (len, from) = match self.sock.recv_from(&mut self.rbuf[..]) {
                Ok(x) => x,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return (false, n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // UDP 上的 ECONNREFUSED 之类：记一下，这一轮不再收（下一轮照常）
                    self.c.recv_errors += 1;
                    self.warn("recv", &format!("收包出错：{e}"), now);
                    return (false, n);
                }
            };
            self.c.rx_packets += 1;
            self.c.rx_bytes += len as u64;
            if self.closing.is_some() {
                // 正在退出：不再接新握手（收到 CLOSE(5) 的客户端会马上重连），收到的包只读掉
                continue;
            }
            let len = len.min(RECV_BUF);
            {
                let (gw, vtx, evs, rep) = (&mut self.gw, &mut self.vtx, &mut self.evs, &mut self.last_report);
                gw.handle_datagram(now, from, &mut self.rbuf[..len], &mut |e| collect(now, e, vtx, evs, rep));
            }
            self.process_events(now, false);
        }
        (true, RECV_BUDGET)
    }

    // ------------------------------------------------------------------
    // 端点事件
    // ------------------------------------------------------------------

    fn process_events(&mut self, now: Micros, from_timeout: bool) {
        // 拿出来处理（处理过程中不会再调端点的 handle_*，不会有新事件进来），处理完把缓冲放回去复用
        let mut evs = std::mem::take(&mut self.evs);
        for e in evs.drain(..) {
            match e {
                Ev::Connected { client, cid, init_name } => self.attach(now, client, cid, init_name),
                Ev::Replaced => {}
                Ev::Input => self.c.inputs += 1,
                Ev::Lost(l) => {
                    self.vtx.on_lost(now, &l, &mut self.carriers);
                    self.queue_carriers();
                }
                Ev::Need(n) => self.on_need(now, n),
                Ev::Control(m) => self.on_control(now, m),
                Ev::Closed(code) => {
                    let why = if from_timeout { DetachWhy::Timeout } else { DetachWhy::Closed(code) };
                    self.detach(now, why);
                }
                Ev::PathChanged(a) => self.log.log('I', "ywp", &format!("客户端地址变了：{a}")),
            }
        }
        if self.evs.is_empty() {
            self.evs = evs;
        }
        // 端点自己删掉的会话（kick、配对被删……）在这里补上
        self.sync_presence(now, DetachWhy::Kicked);
    }

    fn name_of(&self, id: &ClientId) -> Option<String> {
        self.names.iter().find(|(i, _)| i == id).map(|(_, n)| n.clone())
    }

    fn attach(&mut self, now: Micros, client: ClientId, cid: u64, init_name: Option<String>) {
        if self.attached.is_some() {
            self.detach(now, DetachWhy::Replaced);
        }
        let name = self.name_of(&client).or(init_name).unwrap_or_else(|| "?".into());
        let (peer, hs) = self.gw.conn().map(|c| (c.peer().to_string(), c.stats().hs_attempts)).unwrap_or_default();
        self.log.log('I', "ywp", &format!("客户端「{name}」（{client}）接入：地址 {peer}，握手尝试 {hs} 次"));
        self.log.record(
            "ywp_attach",
            &format!("\"client\":\"{client}\",\"name\":{},\"peer\":\"{peer}\",\"hs_attempts\":{hs}", json_str(&name)),
        );
        // 1. PS5 状态
        if let Some(info) = self.ps_info {
            self.send_ctl(ControlMsg::SessionStatus(session_status(&info)));
        }
        // 2. 当前的 VIDEO_CONFIG（没有就等参数集到了再发）
        if let Some(cfg) = self.ing.config().cloned() {
            self.send_ctl(ControlMsg::VideoConfig(cfg));
        }
        // 3. 从下一个关键帧开始转发：请求 IDR、发 KEY_PENDING
        self.vtx.clear();
        self.ing.set_client_present(true);
        self.shared.client_present.store(true, Relaxed);
        self.ing.wait_key(WaitWhy::NewClient);
        let mut outs = Vec::new();
        self.ing.on_tick(now, &mut outs);
        self.apply_outs(now, outs);
        self.next_house = now + HOUSEKEEP_ACTIVE;
        self.c.attaches += 1;
        self.attached = Some(Attached { cid, client, name, since: now });
        let _ = self.events.send(ServeEvent::ClientAttached { client });
        self.publish_status(now);
    }

    fn detach(&mut self, now: Micros, why: DetachWhy) {
        let Some(a) = self.attached.take() else { return };
        self.ing.set_client_present(false);
        self.shared.client_present.store(false, Relaxed);
        self.vtx.clear();
        self.last_report = None;
        self.c.detaches += 1;
        let secs = now.saturating_sub(a.since) as f64 / 1e6;
        self.log.log('I', "ywp", &format!("客户端「{}」（{}）离开：{why:?}，连了 {secs:.1} s", a.name, a.client));
        self.log.record("ywp_detach", &format!("\"client\":\"{}\",\"why\":\"{why:?}\",\"secs\":{secs:.1}", a.client));
        let _ = self.events.send(ServeEvent::ClientDetached { client: a.client, why });
        self.publish_status(now);
    }

    /// 端点里的会话和这里记的不一致时（被删、被踢）补一次 detach
    fn sync_presence(&mut self, now: Micros, why: DetachWhy) {
        let Some(a) = &self.attached else { return };
        let cur = self.gw.conn().map(|c| c.cid().0);
        if cur != Some(a.cid) {
            self.detach(now, why);
        }
    }

    fn on_need(&mut self, now: Micros, n: Need) {
        self.c.needs += 1;
        let reply = self.vtx.on_need(now, &n, &mut self.carriers);
        self.queue_carriers();
        if reply == NeedReply::Gone {
            self.c.gone += 1;
            self.frame_status(n.fid, FrameStatusKind::Gone);
        }
        if n.key {
            self.request_idr(now, IdrReason::NeedK);
        }
        if n.key || reply == NeedReply::KeyPending || self.ing.waiting().is_some() {
            let fid = self.ing.next_fid();
            self.frame_status(fid, FrameStatusKind::KeyPending);
        }
    }

    fn on_control(&mut self, now: Micros, m: ControlMsg) {
        match m {
            ControlMsg::IdrRequest { reason } => {
                self.c.client_idr += 1;
                let d = self.request_idr(now, IdrReason::ClientRequest(reason));
                self.log.record("ywp_client_idr", &format!("\"reason\":{reason},\"decision\":\"{d:?}\""));
            }
            ControlMsg::ModeSet { mode, j_ms } => {
                if let Some(m) = Mode::from_wire(mode) {
                    self.tx_params.mode = m;
                }
                self.tx_params.j_us = j_ms as u32 * 1000;
                self.vtx.set_params(self.tx_params);
            }
            ControlMsg::Error { code, text } => {
                let t: String = text.chars().take(128).collect();
                self.log.log('W', "ywp", &format!("客户端报错 {code}：{t}"));
            }
            // PS5_CMD、REBUILD_CONSENT 在 M2d 接（转给监督线程）
            other => self.log.record("ywp_ctl_ignored", &format!("\"msg\":{}", json_str(&format!("{other:?}")))),
        }
    }

    fn request_idr(&mut self, now: Micros, why: IdrReason) -> IdrDecision {
        let d = self.ctl.request_idr(now, why);
        if d == IdrDecision::Requested {
            self.c.idr_requested += 1;
        }
        d
    }

    fn frame_status(&mut self, fid: u16, kind: FrameStatusKind) {
        if kind == FrameStatusKind::KeyPending {
            self.c.key_pending += 1;
        }
        if let Some(c) = self.gw.conn() {
            c.frame_status(FrameStatusEntry { fid, kind, arg: 0 });
        }
    }

    fn send_ctl(&mut self, m: ControlMsg) {
        let Some(c) = self.gw.conn() else { return };
        if let Err(e) = c.send_control(&m) {
            self.c.ctl_send_fail += 1;
            let now = clock::now_us();
            self.warn("ctl", &format!("CONTROL 发不出去（{e:?}）"), now);
        }
    }

    fn queue_carriers(&mut self) {
        if self.carriers.is_empty() {
            return;
        }
        match self.gw.conn() {
            Some(c) if c.media_allowed() => {
                self.c.video_carriers += self.carriers.len() as u64;
                for v in self.carriers.drain(..) {
                    c.queue_video(v);
                }
            }
            _ => self.carriers.clear(),
        }
    }

    // ------------------------------------------------------------------
    // MediaMsg
    // ------------------------------------------------------------------

    fn drain_media(&mut self, now: Micros) -> (bool, usize) {
        for n in 0..MEDIA_BUDGET {
            match self.rx.try_recv() {
                Ok(m) => {
                    self.c.media_msgs += 1;
                    self.on_media(now, m);
                }
                Err(TryRecvError::Empty) => return (false, n),
                Err(TryRecvError::Disconnected) => {
                    // ServeHandle 已经丢了：照常关闭（发 CLOSE(5)）
                    if self.closing.is_none() {
                        self.begin_close(now, None);
                    }
                    return (false, n);
                }
            }
        }
        (true, MEDIA_BUDGET)
    }

    fn on_media(&mut self, now: Micros, m: MediaMsg) {
        match m {
            MediaMsg::Video { seq, epoch, data, frames_lost, recovered, rx_us } => {
                self.ensure_epoch(now, epoch);
                if self.shared.key_overflow.swap(false, Relaxed) {
                    self.ing.wait_key(WaitWhy::Overflow);
                }
                if self.ing.config().is_none() {
                    // 参数集的通知丢了也能补
                    self.refill_params(now);
                }
                let mut outs = Vec::new();
                self.ing.on_frame(now, VideoIn { seq, rx_us, frames_lost, recovered, data: &data }, &mut outs);
                self.apply_outs(now, outs);
            }
            MediaMsg::ParamsChanged { epoch } => {
                self.ensure_epoch(now, epoch);
                self.refill_params(now);
            }
            // 声音、反馈在 M2b
            MediaMsg::AudioHeaderChanged { .. } | MediaMsg::Audio { .. } | MediaMsg::Feedback { .. } => {}
            MediaMsg::PsChanged(info) => {
                self.ps_info = Some(info);
                self.gw.set_session_info(info);
                self.send_ctl(ControlMsg::SessionStatus(session_status(&info)));
            }
            // M2d
            MediaMsg::Rebuilding { .. } => {}
            MediaMsg::SendControl(m) => self.send_ctl(m),
            MediaMsg::Control((cmd, reply, deadline)) => {
                if Instant::now() <= deadline {
                    let r = self.command(now, &cmd);
                    let _ = reply.send(r);
                }
            }
            MediaMsg::Shutdown { ack } => self.begin_close(now, Some(ack)),
        }
    }

    /// 每帧先看 epoch 变没变：变了就 on_epoch、set_stream，再从最新值槽补参数集
    fn ensure_epoch(&mut self, now: Micros, epoch: u32) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        self.ing.on_epoch(epoch);
        if let Some(si) = self.ctl.stream_info(epoch) {
            self.ing.set_stream(si);
        }
        self.refill_params(now);
    }

    fn refill_params(&mut self, now: Micros) {
        let Some(e) = self.epoch else { return };
        let Some(p) = self.ctl.params(e) else { return };
        let mut outs = Vec::new();
        self.ing.on_params(&p, &mut outs);
        self.apply_outs(now, outs);
    }

    fn apply_outs(&mut self, now: Micros, outs: Vec<IngressOut<'_>>) {
        for o in outs {
            match o {
                IngressOut::Frame(f) => self.send_frame(now, &f),
                IngressOut::Config(cfg) => self.send_ctl(ControlMsg::VideoConfig(cfg)),
                IngressOut::KeyPending { fid } => self.frame_status(fid, FrameStatusKind::KeyPending),
                IngressOut::RequestIdr(why) => {
                    self.request_idr(now, why);
                }
            }
        }
    }

    fn send_frame(&mut self, now: Micros, f: &FrameIn<'_>) {
        let Some(conn) = self.gw.conn() else { return };
        if !conn.media_allowed() {
            return;
        }
        self.carriers.clear();
        match self.vtx.send_frame(now, f, &mut self.carriers) {
            Ok(()) => {
                self.c.frames_sent += 1;
                self.queue_carriers();
            }
            Err(e) => {
                self.carriers.clear();
                self.c.frame_errors += 1;
                self.warn("frame", &format!("fid {} 发不出去：{e}", f.fid), now);
            }
        }
    }

    // ------------------------------------------------------------------
    // 计时器
    // ------------------------------------------------------------------

    fn timers(&mut self, now: Micros) {
        if self.gw.next_timeout().is_some_and(|t| t <= now) {
            {
                let (gw, vtx, evs, rep) = (&mut self.gw, &mut self.vtx, &mut self.evs, &mut self.last_report);
                gw.handle_timeout(now, &mut |e| collect(now, e, vtx, evs, rep));
            }
            self.process_events(now, true);
        }
        if now >= self.next_house {
            let mut outs = Vec::new();
            self.ing.on_tick(now, &mut outs);
            self.apply_outs(now, outs);
            self.vtx.on_timer(now, &mut self.carriers);
            self.queue_carriers();
            if let Some(c) = self.gw.conn() {
                let p50 = c.rtt().p50().min(u32::MAX as Micros) as u32;
                if p50 != self.tx_params.rtt_p50_us {
                    self.tx_params.rtt_p50_us = p50;
                    self.vtx.set_params(self.tx_params);
                }
            }
            let gap = if self.attached.is_some() { HOUSEKEEP_ACTIVE } else { HOUSEKEEP_IDLE };
            self.next_house = now + gap;
        }
        if now >= self.next_status {
            self.publish_status(now);
            self.next_status = now + STATUS_EVERY;
        }
        if now >= self.next_stats {
            self.log_stats();
            self.next_stats = now + STATS_EVERY;
        }
    }

    // ------------------------------------------------------------------
    // 发送
    // ------------------------------------------------------------------

    /// 返回 (预算用完还有活, 发出的包数)
    fn flush(&mut self, now: Micros, writable: bool) -> (bool, usize) {
        let mut sent = 0;
        if let Some(mut p) = self.pending.take() {
            let due = if self.blocked { writable } else { now >= p.retry_at };
            if !due {
                self.pending = Some(p);
                return (false, 0);
            }
            match send_raw(&self.sock, &mut self.c, &p.buf[..p.len], p.dst) {
                SendResult::Sent => {
                    self.blocked = false;
                    self.on_sent(now, p.video);
                    sent += 1;
                }
                SendResult::WouldBlock => {
                    self.blocked = true;
                    self.pending = Some(p);
                    return (false, 0);
                }
                SendResult::NoBufs => {
                    self.blocked = false;
                    p.tries += 1;
                    if p.tries >= ENOBUFS_MAX_TRIES {
                        self.drop_nobufs(now);
                    } else {
                        p.retry_at = now + ENOBUFS_RETRY;
                        self.pending = Some(p);
                    }
                    return (false, 0);
                }
                SendResult::Fatal(e) => {
                    self.blocked = false;
                    self.warn("send", &format!("发送失败，丢掉这一包：{e}"), now);
                }
            }
        }
        for _ in 0..SEND_BUDGET {
            let Some(t) = self.gw.poll_transmit(now, &self.vtx, &mut self.out) else { return (false, sent) };
            let len = t.len.min(BASE_PAYLOAD);
            match send_raw(&self.sock, &mut self.c, &self.out[..len], t.dst) {
                SendResult::Sent => {
                    self.on_sent(now, t.video);
                    sent += 1;
                }
                SendResult::Fatal(e) => self.warn("send", &format!("发送失败，丢掉这一包：{e}"), now),
                r @ (SendResult::WouldBlock | SendResult::NoBufs) => {
                    let blocked = matches!(r, SendResult::WouldBlock);
                    let mut buf = Box::new([0u8; BASE_PAYLOAD]);
                    buf[..len].copy_from_slice(&self.out[..len]);
                    self.pending = Some(PendingTx {
                        buf,
                        len,
                        dst: t.dst,
                        video: t.video,
                        tries: if blocked { 0 } else { 1 },
                        retry_at: now + ENOBUFS_RETRY,
                    });
                    self.blocked = blocked;
                    return (false, sent);
                }
            }
        }
        (true, sent)
    }

    fn on_sent(&mut self, now: Micros, video: Option<(u16, u16)>) {
        if let Some((fid, sym)) = video {
            self.vtx.on_sent(now, fid, sym);
        }
    }

    fn drop_nobufs(&mut self, now: Micros) {
        self.c.send_nobufs_dropped += 1;
        self.send_drops.bump(DropReason::SendNoBufs);
        self.warn("nobufs", "网卡输出队列满（ENOBUFS），连续 3 次，丢掉一包", now);
    }

    // ------------------------------------------------------------------
    // 关闭
    // ------------------------------------------------------------------

    fn begin_close(&mut self, now: Micros, ack: Option<Sender<()>>) {
        if let Some(c) = &mut self.closing {
            if c.ack.is_none() {
                c.ack = ack;
            }
            return;
        }
        if let Some(a) = &self.attached {
            let id = a.client;
            self.gw.kick(&id, close_code::GATEWAY_EXIT);
            self.detach(now, DetachWhy::GatewayExit);
        }
        self.log.log('I', "ywp", "YWP 服务退出（给客户端发 CLOSE(5)）");
        self.closing = Some(Closing { ack, since: now });
    }

    fn check_closing(&mut self, now: Micros) {
        let Some(c) = &self.closing else { return };
        let copies_done = now >= c.since + CLOSE_COPIES[CLOSE_COPIES.len() - 1] + CLOSE_SETTLE && self.pending.is_none();
        if copies_done || now >= c.since + EXIT_CLOSE_WAIT {
            if let Some(ack) = self.closing.take().and_then(|c| c.ack) {
                let _ = ack.send(());
            }
            self.exit = true;
            self.publish_status(now);
        }
    }

    // ------------------------------------------------------------------
    // 控制口命令（clients、kick、clients-reload）
    // ------------------------------------------------------------------

    fn command(&mut self, now: Micros, line: &str) -> String {
        let mut it = line.split_whitespace();
        match (it.next().unwrap_or(""), it.next()) {
            ("clients", None) => self.clients_json(now),
            ("kick", Some(who)) => {
                let Some((id, name)) = self.find_client(who) else { return format!("没有「{who}」这个已配对客户端") };
                if self.attached.as_ref().map(|a| a.client) != Some(id) {
                    return format!("「{name}」现在没连着");
                }
                self.gw.kick(&id, close_code::GATEWAY_ENDED);
                self.sync_presence(now, DetachWhy::Kicked);
                format!("ok：已断开「{name}」（它会重新握手；不想让它再连请 pair remove）")
            }
            ("clients-reload", None) => self.reload_clients(now),
            _ => "用法：clients | kick <名字或指纹> | clients-reload".into(),
        }
    }

    fn find_client(&self, who: &str) -> Option<(ClientId, String)> {
        self.names.iter().find(|(id, n)| n == who || id.to_string() == who).cloned()
    }

    fn reload_clients(&mut self, now: Micros) -> String {
        let Some(dir) = self.keys_dir.clone() else { return "这个服务没有配 keys/ 目录，不能重读".into() };
        match ywp::creds::load_clients(&dir) {
            Ok(list) => {
                let n = list.len();
                self.names = names_of(&list);
                self.gw.reload_clients(list);
                self.sync_presence(now, DetachWhy::Unpaired);
                self.publish_status(now);
                self.log.log('I', "ywp", &format!("重读已配对客户端：{n} 个"));
                format!("ok：已配对 {n} 个客户端")
            }
            Err(e) => format!("失败：{e}"),
        }
    }

    fn clients_json(&mut self, now: Micros) -> String {
        let cur = self.attached.as_ref().map(|a| a.client);
        let list: Vec<String> = self
            .names
            .iter()
            .map(|(id, n)| format!("{{\"name\":{},\"id\":\"{id}\",\"connected\":{}}}", json_str(n), cur == Some(*id)))
            .collect();
        let current = opt_json(self.client_status(now), |c| {
            format!(
                "{{\"name\":{},\"id\":\"{}\",\"peer\":\"{}\",\"connected_s\":{:.1},\"srtt_ms\":{:.1},\"hs_attempts\":{}}}",
                json_str(&c.name),
                c.id,
                c.peer,
                c.connected_ms as f64 / 1000.0,
                c.srtt_us as f64 / 1000.0,
                c.hs_attempts
            )
        });
        format!("{{\"paired\":[{}],\"current\":{current}}}", list.join(","))
    }

    // ------------------------------------------------------------------
    // 状态和统计
    // ------------------------------------------------------------------

    fn client_status(&mut self, now: Micros) -> Option<ClientStatus> {
        let a = self.attached.as_ref()?;
        let (name, id, since) = (a.name.clone(), a.client, a.since);
        let report = self.last_report;
        let c = self.gw.conn()?;
        let st = c.stats();
        Some(ClientStatus {
            name,
            id,
            peer: c.peer(),
            connected_ms: now.saturating_sub(since) / 1000,
            srtt_us: c.rtt().srtt(),
            hs_attempts: st.hs_attempts,
            rx_packets: st.rx_packets,
            tx_packets: st.tx_packets,
            report,
        })
    }

    fn publish_status(&mut self, now: Micros) {
        let s = ServeStatus {
            running: !self.exit,
            port: self.port,
            bind_if: self.bind_if.clone(),
            paired: self.names.len(),
            client: self.client_status(now),
            counters: self.c,
            ingress: *self.ing.stats(),
            waiting_key: self.ing.waiting(),
            cfg_ver: self.ing.config().map(|c| c.cfg_ver),
            epoch: self.epoch,
        };
        *self.shared.status.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    fn log_stats(&mut self) {
        let c = self.c;
        let i = *self.ing.stats();
        let drops = self.gw.drops().snapshot();
        let mut conn_drops = Vec::new();
        if let Some(conn) = self.gw.conn() {
            conn_drops = conn.stats().drops.snapshot();
        }
        let fmt = |v: &[(&'static str, u64)]| v.iter().map(|(k, n)| format!("\"{k}\":{n}")).collect::<Vec<_>>().join(",");
        self.log.record(
            "ywp_stats",
            &format!(
                "\"client\":{},\"rx_packets\":{},\"tx_packets\":{},\"tx_bytes\":{},\"send_enobufs\":{},\"send_nobufs_dropped\":{},\
                 \"send_errors\":{},\"media_full\":{},\"frames_in\":{},\"forwarded\":{},\"key_forwarded\":{},\"w_forwarded\":{},\
                 \"seq_missing\":{},\"dropped_wait_key\":{},\"key_pending\":{},\"idr_requested\":{},\"client_idr\":{},\
                 \"needs\":{},\"inputs\":{},\"gw_drops\":{{{}}},\"conn_drops\":{{{}}}",
                self.attached.is_some(),
                c.rx_packets,
                c.tx_packets,
                c.tx_bytes,
                c.send_enobufs,
                c.send_nobufs_dropped,
                c.send_errors,
                self.shared.media_full.load(Relaxed),
                i.frames_in,
                i.forwarded,
                i.key_forwarded,
                i.w_forwarded,
                i.seq_missing,
                i.dropped_wait_key,
                c.key_pending,
                c.idr_requested,
                c.client_idr,
                c.needs,
                c.inputs,
                fmt(&drops),
                fmt(&conn_drops),
            ),
        );
    }

    /// 同一类警告 5 s 内只写一次
    fn warn(&mut self, kind: &'static str, msg: &str, now: Micros) {
        let last = self.last_warn.entry(kind).or_insert(0);
        if *last != 0 && now < *last + WARN_GAP {
            return;
        }
        *last = now.max(1);
        self.log.log('W', "ywp", msg);
    }
}

enum SendResult {
    Sent,
    WouldBlock,
    NoBufs,
    /// 其他错误：丢掉这一包（计数已加）
    Fatal(io::Error),
}

/// 发一包并分类结果，顺手计数（成功、WouldBlock、ENOBUFS、其他错误）
fn send_raw<S: DgramSock>(sock: &S, c: &mut ServeCounters, pkt: &[u8], dst: SocketAddr) -> SendResult {
    match sock.send_to(pkt, dst) {
        Ok(_) => {
            c.tx_packets += 1;
            c.tx_bytes += pkt.len() as u64;
            SendResult::Sent
        }
        Err(e) => match classify_send_err(&e) {
            SendOutcome::WouldBlock => {
                c.send_wouldblock += 1;
                SendResult::WouldBlock
            }
            SendOutcome::NoBufs => {
                c.send_enobufs += 1;
                SendResult::NoBufs
            }
            SendOutcome::Sent => SendResult::Sent,
            SendOutcome::Fatal(e) => {
                c.send_errors += 1;
                SendResult::Fatal(e)
            }
        },
    }
}

fn session_status(info: &SessionInfo) -> SessionStatus {
    SessionStatus { ps_state: info.ps_state, host_id: Some(info.host_id), epoch: Some(info.epoch), ..SessionStatus::default() }
}

// ======================================================================
// 启动和线程
// ======================================================================

pub fn spawn(
    cfg: ServeConfig,
    keys: Keys,
    ctl: Arc<dyn PsControl>,
    events: Sender<ServeEvent>,
) -> io::Result<ServeHandle> {
    spawn_with(cfg, ServeOptions::default(), keys, ctl, events)
}

/// 同 spawn，多一组可选项（keys/ 目录、RecoveredPolicy、本地地址）
pub fn spawn_with(
    cfg: ServeConfig,
    opts: ServeOptions,
    keys: Keys,
    ctl: Arc<dyn PsControl>,
    events: Sender<ServeEvent>,
) -> io::Result<ServeHandle> {
    let ip = opts.bind_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let std_sock = ywp::net::bound_udp(SocketAddr::new(ip, cfg.port), &cfg.bind_if)?;
    let local = std_sock.local_addr()?;
    let mut sock = mio::net::UdpSocket::from_std(std_sock);
    let poll = Poll::new()?;
    poll.registry().register(&mut sock, SOCK, Interest::READABLE)?;
    let waker = Arc::new(Waker::new(poll.registry(), WAKER)?);
    let (tx, rx) = mpsc::sync_channel(MEDIA_QUEUE_CAP);
    let shared = Arc::new(ServeShared::default());
    let cfg = ServeConfig { port: local.port(), ..cfg };
    let srv = Server::new(sock, &cfg, &opts, keys, ctl, events, rx, shared.clone(), clock::now_us());
    let join = std::thread::Builder::new().name("ywp".into()).spawn(move || run_loop(srv, poll))?;
    Ok(ServeHandle { tx, waker, shared, local, join })
}

fn run_loop(mut srv: Server<mio::net::UdpSocket>, mut poll: Poll) {
    let mut events = Events::with_capacity(64);
    let mut writable = false;
    let mut want_w = false;
    srv.log.log('I', "ywp", &format!("YWP 服务开始：UDP {}，已配对 {} 个客户端", srv.port, srv.names.len()));
    loop {
        let now = clock::now_us();
        let more = srv.turn(now, writable);
        if srv.exit {
            break;
        }
        let w = srv.wants_writable();
        if w != want_w {
            let interest = if w { Interest::READABLE | Interest::WRITABLE } else { Interest::READABLE };
            if let Err(e) = poll.registry().reregister(&mut srv.sock, SOCK, interest) {
                srv.warn("poll", &format!("改 socket 关注的事件失败：{e}"), now);
            } else {
                want_w = w;
            }
        }
        let timeout = if more {
            Duration::ZERO
        } else {
            let now = clock::now_us();
            Duration::from_micros(srv.next_deadline(now).saturating_sub(now))
        };
        writable = false;
        match poll.poll(&mut events, Some(timeout)) {
            Ok(()) => writable = events.iter().any(|e| e.token() == SOCK && e.is_writable()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                srv.warn("poll", &format!("poll 出错：{e}"), now);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    srv.log.log('I', "ywp", "YWP 服务已退出");
}

#[cfg(test)]
mod tests;
