//! 会话监督：唤醒 → 建会话 → 串流（看门狗）→ 出错退避重试；PS5 会话由网关长期持有。
//! 计时依据 M0 实测（docs/experiments/M0-report.md）：
//! - 唤醒约 14 s 就绪；新会话 1.3~2.7 s 出首帧；
//! - 正常结束后冷却 3~6 s，kill 后 IN_USE 约 10 s，真正断网约 28 s；
//! - 请求 IDR 约 1 s 才到关键帧。
//!
//! 暂停是持久的（--dir 下的 paused 文件）：pause、sleep、PS5 主动结束串流、登录 PIN 有问题都会进入暂停，
//! 网关重启后仍保持暂停，直到 resume。这样网关不会和用户在 PS5 上的操作（待机、关机）对着干。

use crate::control::Request;
use crate::cred::Cred;
use crate::discovery::{self, HostState};
use crate::handler::{now_us, GwHandler, Recorder, Shared};
use crate::jlog::{escape, Logger};
use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};
use yw_chiaki_sys::{log_level, Codec, Session, SessionParams};

pub struct Config {
    pub host: String,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    pub ls_mtu: u32,
    pub loss_max: f64,
    pub wake: bool,
    pub stats_interval: Duration,
    pub base_dir: PathBuf,
    /// 没有客户端时最多保持会话多久；None 表示不限（M1 还没有客户端，默认不限）
    pub hold: Option<Duration>,
    /// 保持期满时让 PS5 休眠
    pub hold_sleep: bool,
}

/// 唤醒后多久没就绪就算失败
const WAKE_DEADLINE: Duration = Duration::from_secs(60);
/// 唤醒后仍在待机时，隔多久重发唤醒包（唤醒包只是一个 UDP 包，Wi-Fi 下会丢）
const WAKE_RESEND: Duration = Duration::from_secs(10);

enum State {
    /// 查询 PS5 状态
    Probe,
    /// 已发唤醒包，等它就绪
    WaitReady { deadline: Instant, next_query: Instant, last_wake: Instant },
    /// 已发起会话，等连上并出首帧
    Connecting { since: Instant },
    /// 串流中
    Streaming { since: Instant, idr_1s: bool, idr_3s: bool },
    /// 出错后等待重试
    Backoff { until: Instant },
    /// 暂停（不连 PS5、不唤醒），原因在 Supervisor::pause_reason
    Paused,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::Probe => "查询",
            State::WaitReady { .. } => "唤醒中",
            State::Connecting { .. } => "建会话",
            State::Streaming { .. } => "串流中",
            State::Backoff { .. } => "退避",
            State::Paused => "暂停",
        }
    }
}

struct Live {
    session: Session,
    shared: Arc<Shared>,
    started: Instant,
    id: u64,
}

#[derive(Default, Clone, Copy)]
struct Snap {
    session_id: u64,
    frames: u64,
    bytes: u64,
    lost: i64,
    audio: u64,
}

pub struct Supervisor {
    cfg: Config,
    cred: Cred,
    log: Arc<Logger>,
    state: State,
    live: Option<Live>,
    failure_since: Option<Instant>,
    failures: u64,
    sessions_started: u64,
    last_host_state: Option<HostState>,
    last_quit: String,
    pause_reason: String,
    /// 保持期从这里算起（启动或离开暂停时）
    hold_since: Instant,
    exit: bool,
    started_at: Instant,
    last_stats: Instant,
    prev: Snap,
}

impl Supervisor {
    pub fn new(cfg: Config, cred: Cred, log: Arc<Logger>) -> Supervisor {
        let mut sup = Supervisor {
            cfg,
            cred,
            log,
            state: State::Probe,
            live: None,
            failure_since: None,
            failures: 0,
            sessions_started: 0,
            last_host_state: None,
            last_quit: String::new(),
            pause_reason: String::new(),
            hold_since: Instant::now(),
            exit: false,
            started_at: Instant::now(),
            last_stats: Instant::now(),
            prev: Snap::default(),
        };
        if let Ok(why) = std::fs::read_to_string(sup.pause_marker()) {
            sup.state = State::Paused;
            sup.pause_reason = why.trim().to_string();
        }
        sup
    }

    fn pause_marker(&self) -> PathBuf {
        self.cfg.base_dir.join("paused")
    }

    /// 进入暂停并写标记文件（网关重启后仍暂停）。调用前先结束会话。
    fn enter_pause(&mut self, why: &str) {
        self.state = State::Paused;
        self.pause_reason = why.to_string();
        if let Err(e) = std::fs::write(self.pause_marker(), format!("{why}\n")) {
            self.log.warn("gateway", &format!("写不了暂停标记 {}：{e}（网关重启后不会保持暂停）", self.pause_marker().display()));
        }
        self.log.info("gateway", &format!("已暂停：{why}。用 resume 恢复"));
    }

    fn leave_pause(&mut self) {
        match std::fs::remove_file(self.pause_marker()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => self.log.warn("gateway", &format!("删不掉暂停标记 {}：{e}", self.pause_marker().display())),
        }
        self.pause_reason.clear();
        self.failure_since = None;
        self.hold_since = Instant::now();
        self.log.info("gateway", "离开暂停");
    }

    pub fn run(mut self, rx: Receiver<Request>) {
        self.log.info("gateway", &format!("开始管理 PS5 {}", self.cfg.host));
        if matches!(self.state, State::Paused) {
            self.log.info("gateway", &format!("上次退出前处于暂停（{}），继续暂停，用 resume 恢复", self.pause_reason));
        }
        loop {
            if yw_chiaki_sys::shutdown_requested() && !self.exit {
                self.log.info("gateway", "收到退出信号，正常结束会话");
                self.exit = true;
            }
            if self.exit {
                self.stop_session("网关退出");
                break;
            }
            // 先处理会话结束，命令看到的才是最新状态（会话已结束就不会再对它发 IDR 之类）
            self.check_quit();
            while let Ok((cmd, reply, deadline)) = rx.try_recv() {
                if Instant::now() > deadline {
                    self.log.warn("control", &format!("命令「{cmd}」等太久，控制口已回复超时，丢弃不执行"));
                    continue;
                }
                let r = self.command(&cmd);
                let _ = reply.send(r);
            }
            self.step();
            self.maybe_stats();
            std::thread::sleep(Duration::from_millis(100));
        }
        self.log.info("gateway", "已退出");
    }

    // ------------------------------------------------------------ 状态机

    fn step(&mut self) {
        let now = Instant::now();
        let state = std::mem::replace(&mut self.state, State::Probe);
        self.state = match state {
            State::Probe => self.probe(),
            State::WaitReady { deadline, next_query, mut last_wake } => {
                if now < next_query {
                    return self.state = State::WaitReady { deadline, next_query, last_wake };
                }
                let hs = discovery::query(&self.cfg.host, Duration::from_millis(1500));
                if hs == HostState::Ready {
                    self.log.info("wake", "PS5 已就绪");
                    self.start_session()
                } else if now >= deadline {
                    self.fail("唤醒后 60 秒仍未就绪")
                } else {
                    // 仍在待机（不是没回应：PS5 启动中查询会超时）就隔 10 s 重发一次唤醒包
                    if hs == HostState::Standby && last_wake.elapsed() >= WAKE_RESEND {
                        match yw_chiaki_sys::wakeup(&self.cfg.host, &self.cred.regist_key) {
                            Ok(()) => self.log.info("wake", "PS5 仍在待机，重发唤醒包"),
                            Err(e) => self.log.warn("wake", &format!("重发唤醒包失败：{e}")),
                        }
                        last_wake = Instant::now();
                    }
                    State::WaitReady { deadline, next_query: now + Duration::from_secs(1), last_wake }
                }
            }
            State::Connecting { since } => {
                let ready = self
                    .live
                    .as_ref()
                    .map(|l| l.shared.connected.load(Relaxed) && l.shared.frames.load(Relaxed) > 0)
                    .unwrap_or(false);
                if ready {
                    if let Some(l) = &self.live {
                        l.session.set_controller_idle(); // 没有客户端时一直发中性输入，保持会话
                        self.log.info("session", &format!("串流中（发起会话后 {:.2} s 出首帧）", l.started.elapsed().as_secs_f64()));
                    }
                    State::Streaming { since: now, idr_1s: false, idr_3s: false }
                } else if self.live.is_none() {
                    self.fail("会话对象丢失")
                } else if since.elapsed() > Duration::from_secs(25) {
                    self.stop_session("建会话超时");
                    self.fail("25 秒内没有连上或没有画面")
                } else {
                    State::Connecting { since }
                }
            }
            State::Streaming { since, mut idr_1s, mut idr_3s } => {
                let Some(l) = &self.live else {
                    return self.state = self.fail("会话对象丢失");
                };
                let gap_ms = now_us().saturating_sub(l.shared.last_frame_us.load(Relaxed)) / 1000;
                if gap_ms > 8000 {
                    self.log.warn("watchdog", "8 秒没有视频，重建会话（L7）");
                    self.stop_session("视频停流");
                    return self.state = self.fail("视频停流 8 秒");
                }
                if gap_ms > 3000 && !idr_3s {
                    self.log.warn("watchdog", "3 秒没有视频，再次请求 IDR");
                    let _ = l.session.request_idr();
                    idr_3s = true;
                } else if gap_ms > 1000 && !idr_1s {
                    self.log.warn("watchdog", "1 秒没有视频，请求 IDR");
                    let _ = l.session.request_idr();
                    idr_1s = true;
                } else if gap_ms < 500 && (idr_1s || idr_3s) {
                    self.log.info("watchdog", "视频恢复");
                    idr_1s = false;
                    idr_3s = false;
                }
                if since.elapsed() > Duration::from_secs(60) && self.failure_since.is_some() {
                    self.failure_since = None;
                    self.log.info("gateway", "会话已稳定 60 秒，退避计时清零");
                }
                if let Some(hold) = self.cfg.hold {
                    if self.hold_since.elapsed() >= hold {
                        let mins = hold.as_secs() / 60;
                        if self.cfg.hold_sleep {
                            let r = l.session.goto_bed();
                            self.log.info("hold", &format!("保持期 {mins} 分钟已到，让 PS5 休眠：{}", r.err().unwrap_or_else(|| "已发送".into())));
                        }
                        self.stop_session("保持期满");
                        self.enter_pause(&format!("保持期 {mins} 分钟已到"));
                        return;
                    }
                }
                State::Streaming { since, idr_1s, idr_3s }
            }
            State::Backoff { until } => {
                if now >= until {
                    State::Probe
                } else {
                    State::Backoff { until }
                }
            }
            State::Paused => State::Paused,
        };
    }

    fn probe(&mut self) -> State {
        let hs = discovery::query(&self.cfg.host, Duration::from_secs(2));
        if self.last_host_state != Some(hs) {
            self.log.info("probe", &format!("PS5 状态：{}", hs.name()));
            self.last_host_state = Some(hs);
        }
        match hs {
            HostState::Ready => self.start_session(),
            HostState::Standby if self.cfg.wake => match yw_chiaki_sys::wakeup(&self.cfg.host, &self.cred.regist_key) {
                Ok(()) => {
                    self.log.info("wake", "已发送唤醒包");
                    let now = Instant::now();
                    State::WaitReady { deadline: now + WAKE_DEADLINE, next_query: now + Duration::from_secs(1), last_wake: now }
                }
                Err(e) => self.fail(&format!("唤醒包发送失败：{e}")),
            },
            HostState::Standby => State::Backoff { until: Instant::now() + Duration::from_secs(5) },
            HostState::NoReply => self.fail("PS5 没有回应（关机、掉线或网络不通）"),
        }
    }

    fn start_session(&mut self) -> State {
        self.stop_session("开新会话前清理");
        self.sessions_started += 1;
        let hevc = self.cfg.codec != Codec::H264;
        let shared = Arc::new(Shared::new(hevc, self.cred.console_pin > 0, self.log.clone()));
        let params = SessionParams {
            host: self.cfg.host.clone(),
            regist_key: self.cred.regist_key,
            morning: self.cred.rp_key,
            height: self.cfg.height,
            fps: self.cfg.fps,
            bitrate_kbps: self.cfg.bitrate_kbps,
            codec: self.cfg.codec,
            packet_loss_max: self.cfg.loss_max,
            ls_mtu: self.cfg.ls_mtu,
            ls_rtt_ms: 0,
            console_pin: self.cred.console_pin,
            log_mask: log_level::ERROR | log_level::WARNING | log_level::INFO | log_level::VERBOSE,
        };
        let session = match Session::new(&params, Arc::new(GwHandler { s: shared.clone() })) {
            Ok(s) => s,
            Err(e) => return self.fail(&format!("创建会话失败：{e}")),
        };
        if let Err(e) = session.start() {
            drop(session);
            return self.fail(&format!("启动会话失败：{e}"));
        }
        self.log.info(
            "session",
            &format!(
                "发起会话（第 {} 次）：{}p{} {} kbps，LaunchSpec mtu {}，上报丢包上限 {:.1}%",
                self.sessions_started,
                self.cfg.height,
                self.cfg.fps,
                self.cfg.bitrate_kbps,
                self.cfg.ls_mtu,
                self.cfg.loss_max * 100.0
            ),
        );
        self.live = Some(Live { session, shared, started: Instant::now(), id: self.sessions_started });
        State::Connecting { since: Instant::now() }
    }

    /// 正常结束当前会话（不 kill：kill 之后 PS5 会返回约 10 s 的 IN_USE）。
    fn stop_session(&mut self, why: &str) {
        if let Some(mut l) = self.live.take() {
            self.log.info("session", &format!("结束会话：{why}"));
            l.session.stop();
            drop(l); // Drop 里 join 并释放，之后不会再有回调
        }
    }

    /// 会话意外结束（PS5 断开、IN_USE、连不上……），或登录 PIN 有问题
    fn check_quit(&mut self) {
        // 登录 PIN 没有或不对：chiaki 会一直等 PIN，必须由这里结束；不自动重试，免得反复用错的 PIN
        if let Some(msg) = self.live.as_ref().and_then(|l| l.shared.take_pin_failed()) {
            self.log.error("session", &msg);
            self.stop_session("登录 PIN 有问题");
            self.last_quit = msg.clone();
            self.enter_pause(&format!("{msg}；改好凭据里的 console_pin、重启网关后用 resume 重试"));
            return;
        }
        let quit = self.live.as_ref().and_then(|l| l.shared.take_quit());
        if let Some(q) = quit {
            if let Some(l) = self.live.take() {
                drop(l);
            }
            self.last_quit = q.reason.clone();
            if matches!(self.state, State::Paused) {
                return;
            }
            if q.remote_shutdown {
                // PS5 自己结束了串流（待机、关机、系统更新重启、在 PS5 上退出远程游玩）：尊重它，不唤醒也不重连
                self.log.warn("session", &format!("PS5 主动结束串流（{}），网关不自动唤醒或重连", q.reason));
                self.enter_pause("PS5 主动结束了串流（待机、关机或在 PS5 上退出远程游玩）");
                return;
            }
            let why = if q.in_use { format!("PS5 正被占用（IN_USE）：{}", q.reason) } else { format!("会话结束：{}", q.reason) };
            self.state = self.fail(&why);
        }
    }

    /// 记一次失败并计算退避：前 30 s 每 2 s，之后到 5 min 每 5 s，再之后每 30 s。
    fn fail(&mut self, why: &str) -> State {
        self.failures += 1;
        let since = *self.failure_since.get_or_insert_with(Instant::now);
        let wait = match since.elapsed().as_secs() {
            0..=29 => 2,
            30..=299 => 5,
            _ => 30,
        };
        self.log.warn("retry", &format!("{why}；{wait} 秒后重试（连续失败 {:.0} s）", since.elapsed().as_secs_f64()));
        State::Backoff { until: Instant::now() + Duration::from_secs(wait) }
    }

    // ------------------------------------------------------------ 统计

    fn maybe_stats(&mut self) {
        if self.last_stats.elapsed() < self.cfg.stats_interval {
            return;
        }
        let dt = self.last_stats.elapsed().as_secs_f64();
        self.last_stats = Instant::now();
        let Some(l) = &self.live else { return };
        if !matches!(self.state, State::Streaming { .. }) {
            return;
        }
        let s = &l.shared;
        let cur = Snap {
            session_id: l.id,
            frames: s.frames.load(Relaxed),
            bytes: s.bytes.load(Relaxed),
            lost: s.frames_lost.load(Relaxed),
            audio: s.audio_frames.load(Relaxed),
        };
        let prev = if self.prev.session_id == l.id { self.prev } else { Snap { session_id: l.id, ..Snap::default() } };
        let fps = (cur.frames - prev.frames) as f64 / dt;
        let mbps = (cur.bytes - prev.bytes) as f64 * 8.0 / dt / 1e6;
        let lost = cur.lost - prev.lost;
        let target = s.target_bitrate.load(Relaxed) as f64 / 1e6;
        self.log.info(
            "stats",
            &format!(
                "fps {fps:.1}，{mbps:.2} Mbps，PS5 目标 {target:.1} Mbps，本段丢帧 {lost}，累计：关键帧 {} 丢帧 {} FEC 失败 {} 快速重传 {}",
                s.keyframes.load(Relaxed),
                cur.lost,
                s.fec_failures.load(Relaxed),
                s.fast_resends.load(Relaxed)
            ),
        );
        self.log.record(
            "stats",
            &format!(
                "\"session\":{},\"fps\":{fps:.2},\"mbps\":{mbps:.3},\"target_mbps\":{target:.3},\"lost\":{lost},\"audio\":{},\"keyframes\":{},\"lost_total\":{},\"fec_fail\":{},\"fast_resend\":{},\"missing_noise\":{},\"takion_dup\":{}",
                l.id,
                cur.audio - prev.audio,
                s.keyframes.load(Relaxed),
                cur.lost,
                s.fec_failures.load(Relaxed),
                s.fast_resends.load(Relaxed),
                s.noise_missing_frames.load(Relaxed),
                s.noise_takion_dup.load(Relaxed)
            ),
        );
        self.prev = cur;
    }

    // ------------------------------------------------------------ 控制命令

    fn resolve(&self, p: &str) -> PathBuf {
        let p = PathBuf::from(p);
        if p.is_absolute() {
            p
        } else {
            self.cfg.base_dir.join(p)
        }
    }

    fn command(&mut self, line: &str) -> String {
        let mut it = line.split_whitespace();
        let cmd = it.next().unwrap_or("");
        let arg1 = it.next();
        let arg2 = it.next();
        // idr、snap、record 只在串流中执行：建会话期间 takion 还没初始化，会话结束后已销毁
        let streaming = matches!(self.state, State::Streaming { .. });
        let not_streaming = || format!("未在串流（当前：{}）", self.state.name());
        match cmd {
            "status" => self.status_json(),
            "idr" => match &self.live {
                Some(l) if streaming => l.session.request_idr().map(|_| "ok".into()).unwrap_or_else(|e| format!("失败：{e}")),
                _ => not_streaming(),
            },
            "snap" | "record" => {
                let Some(l) = self.live.as_ref().filter(|_| streaming) else { return not_streaming() };
                let Some(path) = arg1 else { return "用法：snap <文件> 或 record <文件> <秒>".into() };
                let path = self.resolve(path);
                let rec = if cmd == "snap" {
                    Recorder::frames(path.clone(), 8, self.log.clone())
                } else {
                    let secs = arg2.and_then(|s| s.parse::<f64>().ok()).unwrap_or(10.0_f64).clamp(1.0, 600.0);
                    Recorder::seconds(path.clone(), secs, self.log.clone())
                };
                let rec = match rec {
                    Ok(r) => r,
                    Err(e) => return format!("失败：{e}"),
                };
                let old = l.shared.recorder.lock().unwrap_or_else(|e| e.into_inner()).replace(rec);
                let _ = l.session.request_idr();
                let note = if old.is_some() { "（已取消上一个录制）" } else { "" };
                drop(old); // 旧录制的写盘线程会删掉它的临时文件
                format!("ok{note}：从下一个关键帧开始写入 {}", path.display())
            }
            "inject" => {
                let Some(l) = &self.live else { return "没有会话".into() };
                let Some(v) = arg1.and_then(|s| s.parse::<f64>().ok()) else { return "用法：inject <0~1，-1 恢复>".into() };
                l.session.set_inject_loss(v);
                self.log.info("control", &format!("注入丢包上报 {v}"));
                "ok".into()
            }
            "sleep" => {
                // 休眠命令要经 ctrl 连接发给 PS5，没有会话就发不出去
                let reply = match &self.live {
                    Some(l) => match l.session.goto_bed() {
                        Ok(()) => "ok：已让 PS5 休眠，网关已暂停（resume 恢复）".to_string(),
                        Err(e) => format!("让 PS5 休眠失败：{e}；网关已暂停（resume 恢复）"),
                    },
                    None => "没有会话，PS5 未休眠；网关已暂停（resume 恢复）".to_string(),
                };
                self.log.info("control", &reply);
                self.stop_session("让 PS5 休眠");
                self.enter_pause("用户让 PS5 休眠");
                reply
            }
            "pause" => {
                self.stop_session("用户暂停");
                self.enter_pause("用户暂停");
                "ok：已暂停（resume 恢复）".into()
            }
            "resume" => {
                if !matches!(self.state, State::Paused) {
                    return format!("当前「{}」，不在暂停，忽略", self.state.name());
                }
                self.leave_pause();
                self.state = State::Probe;
                "ok：已离开暂停".into()
            }
            "restart" => {
                let note = if matches!(self.state, State::Paused) {
                    self.leave_pause();
                    "（已离开暂停）"
                } else {
                    ""
                };
                self.stop_session("用户要求重建");
                self.state = State::Backoff { until: Instant::now() + Duration::from_secs(1) };
                format!("ok{note}：重建会话")
            }
            "quit" => {
                self.exit = true;
                "ok：正在退出".into()
            }
            _ => "不认识的命令。可用：status idr snap record inject sleep pause resume restart quit".into(),
        }
    }

    fn status_json(&self) -> String {
        let (session_s, frames, keyframes, lost, fec_fail, target, last_frame_ms, fast) = match &self.live {
            Some(l) => {
                let s = &l.shared;
                let last = s.last_frame_us.load(Relaxed);
                (
                    l.started.elapsed().as_secs_f64(),
                    s.frames.load(Relaxed),
                    s.keyframes.load(Relaxed),
                    s.frames_lost.load(Relaxed),
                    s.fec_failures.load(Relaxed),
                    s.target_bitrate.load(Relaxed),
                    if last == 0 { -1 } else { (now_us().saturating_sub(last) / 1000) as i64 },
                    s.fast_resends.load(Relaxed),
                )
            }
            None => (0.0, 0, 0, 0, 0, 0, -1, 0),
        };
        format!(
            "{{\"state\":\"{}\",\"pause_reason\":\"{}\",\"host\":\"{}\",\"uptime_s\":{:.0},\"session_s\":{:.0},\"sessions\":{},\"failures\":{},\"last_quit\":\"{}\",\"frames\":{},\"keyframes\":{},\"frames_lost\":{},\"fec_failures\":{},\"fast_resends\":{},\"target_bitrate\":{},\"last_frame_ms_ago\":{}}}",
            self.state.name(),
            escape(&self.pause_reason),
            escape(&self.cfg.host),
            self.started_at.elapsed().as_secs_f64(),
            session_s,
            self.sessions_started,
            self.failures,
            escape(&self.last_quit),
            frames,
            keyframes,
            lost,
            fec_fail,
            fast,
            target,
            last_frame_ms
        )
    }
}
