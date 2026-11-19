//! 连网关的整条链路：net 线程 → decode 线程 →（有界面时）主线程呈现。
//!
//! - **正常模式**（有界面）：SDL Metal 窗口，到达即呈现，统计浮层；冻结按呈现时刻统计（FreezeMeter）。
//!   窗口里按 Esc 或 Q、或关窗口结束，结束时向网关发 CLOSE。
//! - **无界面模式**（`--headless --connect-once --duration <秒> --json`）：连一次，真解码、不呈现，
//!   冻结按出帧时刻统计；到时间或遇到不会自动恢复的情况（被拒、被关）就结束，向 stdout 输出一行 JSON。
//!
//! decode 线程：按 DecodeItem 的顺序同步解码。关键帧前面带着参数集（Assembler 补的），
//! 编码格式和参数集都从关键帧里取：参数集变了才重开解码器；cfg_ver 变了先 flush。
//! 解码报错经 NetIn::DecoderError 报给 net 线程（由 Assembler 请求 IDR、冻结到下一个关键帧）。

use crate::clock;
use crate::config::GatewayProfile;
use crate::decode::{log_stats, DecodedFrame, FfmpegDecoder, HwKind, VideoDecoder};
use crate::net::{spawn_net, NetHandle, NetIn, NetStats, UiEvent, UiSink, AUDIO_QUEUE, DECODE_QUEUE};
use crate::overlay::{FpsCounter, OverlayStats, Samples};
use crate::present::{Presenter, PresenterExt, SdlMetalPresenter, VideoWindow};
use sdl3::sys::events::*;
use sdl3::sys::keycode::{SDLK_ESCAPE, SDLK_Q};
use std::fmt::Write as _;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use ywp::consts::close_code;
use ywp::crypto::StaticKeypair;
use ywp::time::Micros;
use ywp::wire::HsErr;
use yw_media::nal::{self, Codec};
use yw_media::playout::FreezeMeter;
use yw_media::video_rx::DecodeItem;

/// Presenter 信箱容量（最多 3 帧）
const MAILBOX: usize = 3;
/// 解码耗时样本最多留多少个（60 fps 下约 4.8 h），超过后不再记
const LAT_SAMPLES_MAX: usize = 1 << 20;

// ======================== decode 线程 ========================

/// decode 线程的统计
#[derive(Debug, Default)]
pub struct DecodeReport {
    /// 收到的 DecodeItem
    pub items: u64,
    pub frames: u64,
    pub no_frame: u64,
    pub errors: u64,
    /// 还没配置解码器（没见到带参数集的关键帧）就到了的帧
    pub skipped: u64,
    pub first_frame_at: Option<Micros>,
    /// 送入到出帧
    pub lat: Samples,
    /// 无界面模式按出帧时刻统计冻结
    pub freeze: Option<FreezeMeter>,
    pub mailbox_full: u64,
    pub first_err: Option<String>,
}

/// 关键帧开头的参数集（带 4 字节起始码拼起来）和编码格式；开头不是参数集时返回 None
pub fn leading_params(au: &[u8]) -> Option<(Codec, Vec<u8>)> {
    let c = nal::guess_codec(au)?;
    let mut out = Vec::new();
    for n in nal::nal_units(au) {
        let Some(t) = nal::nal_type(n, c) else { continue };
        if nal::is_vcl(t, c) {
            break;
        }
        if nal::is_param_set(t, c) {
            out.extend_from_slice(&nal::START_CODE);
            out.extend_from_slice(n);
        }
    }
    Some((c, out))
}

/// 叫醒主线程的 SDL 自定义事件类型（0 表示没开窗口）
type WakeType = u32;

fn wake_main(t: WakeType) {
    if t == 0 {
        return;
    }
    let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
    ev.r#type = t;
    unsafe { SDL_PushEvent(&mut ev) };
}

/// 出帧交给谁
enum FrameOut {
    /// 有界面：信箱 + 叫醒主线程
    Present(SyncSender<DecodedFrame>, WakeType),
    /// 无界面：只统计，帧当场释放
    Discard,
}

fn decode_loop(
    rx: Receiver<DecodeItem>,
    net_tx: Sender<NetIn>,
    waker: Arc<mio::Waker>,
    mut dec: Box<dyn VideoDecoder>,
    out: FrameOut,
) -> DecodeReport {
    crate::platform::set_thread_qos_interactive();
    let mut r = DecodeReport::default();
    if matches!(out, FrameOut::Discard) {
        r.freeze = Some(FreezeMeter::new());
    }
    let mut cur_cfg: Option<u8> = None;
    let mut configured = false;
    let report_err = |fid: u64| {
        let _ = net_tx.send(NetIn::DecoderError(fid));
        let _ = waker.wake();
    };
    while let Ok(item) = rx.recv() {
        r.items += 1;
        if item.key {
            if let Some((codec, params)) = leading_params(&item.au) {
                if cur_cfg.is_some_and(|c| c != item.cfg) {
                    dec.flush();
                }
                // 参数集没变时 configure 什么都不做
                if let Err(e) = dec.configure(codec, &params) {
                    r.errors += 1;
                    r.first_err.get_or_insert_with(|| format!("配置解码器失败：{e}"));
                    configured = false;
                    report_err(item.fid);
                    continue;
                }
                configured = true;
            }
            cur_cfg = Some(item.cfg);
        }
        if !configured {
            r.skipped += 1;
            continue;
        }
        let t_in = clock::now_us();
        match dec.decode(&item.au) {
            Ok(Some(mut f)) => {
                let t_out = clock::now_us();
                if r.lat.len() < LAT_SAMPLES_MAX {
                    r.lat.push(t_out.saturating_sub(t_in));
                }
                r.frames += 1;
                r.first_frame_at.get_or_insert(t_out);
                f.fid = item.fid;
                f.p_us = item.p_us;
                match &out {
                    FrameOut::Present(tx, wake) => match tx.try_send(f) {
                        Ok(()) => wake_main(*wake),
                        Err(TrySendError::Full(_)) => r.mailbox_full += 1,
                        Err(TrySendError::Disconnected(_)) => break,
                    },
                    FrameOut::Discard => {
                        if let Some(m) = r.freeze.as_mut() {
                            m.on_present(t_out);
                        }
                    }
                }
            }
            Ok(None) => r.no_frame += 1,
            Err(e) => {
                r.errors += 1;
                r.first_err.get_or_insert_with(|| format!("fid {}：{e}", item.fid));
                report_err(item.fid);
            }
        }
    }
    r
}

fn spawn_decode(
    rx: Receiver<DecodeItem>,
    net: &NetHandle,
    dec: Box<dyn VideoDecoder>,
    out: FrameOut,
) -> Result<JoinHandle<DecodeReport>, String> {
    let (tx, waker) = (net.tx.clone(), net.waker.clone());
    std::thread::Builder::new()
        .name("decode".into())
        .spawn(move || decode_loop(rx, tx, waker, dec, out))
        .map_err(|e| format!("起 decode 线程失败：{e}"))
}

/// 起 net 线程和它要的通道；返回 net 句柄和 DecodeItem 的接收端
fn start_net(p: &GatewayProfile, kp: StaticKeypair, ui: UiSink) -> Result<(NetHandle, Receiver<DecodeItem>), String> {
    let (dec_tx, dec_rx) = mpsc::sync_channel(DECODE_QUEUE);
    // 音频（M2b）和 FEEDBACK（M2b）这一阶段没有消费者：接收端直接丢掉，net 线程发不出去就算了
    let (audio_tx, _) = mpsc::sync_channel(AUDIO_QUEUE);
    let (fb_tx, _) = mpsc::channel();
    let net = spawn_net(p, kp, dec_tx, audio_tx, fb_tx, ui).map_err(|e| format!("socket：{e}"))?;
    Ok((net, dec_rx))
}

/// UiEvent 的中文日志行（stderr）
fn log_event(e: &UiEvent) {
    match e {
        UiEvent::Connecting => eprintln!("正在握手……"),
        UiEvent::Connected => eprintln!("已连上网关（密钥已确认）"),
        UiEvent::Rejected(HsErr::Busy) => eprintln!("网关回 BUSY：另一台客户端正在用（接管的确认框在 M2d）"),
        UiEvent::Rejected(e) => eprintln!("网关拒绝握手：{e:?}"),
        UiEvent::Closed { code } if *code == close_code::GATEWAY_EXIT => eprintln!("网关进程退出，按握手节奏重连"),
        UiEvent::Closed { code } => eprintln!("网关关闭了会话：code {code}"),
        UiEvent::Status(s) => eprintln!("网关状态：ps_state {}", s.ps_state),
        UiEvent::Reconnecting => eprintln!("会话断了，正在重新握手"),
        UiEvent::Stopped(Some(r)) => eprintln!("网络线程退出：{r}"),
        _ => {}
    }
}

/// 不会自动恢复的情况（无界面模式据此提前结束）
fn terminal_reason(e: &UiEvent) -> Option<String> {
    match e {
        UiEvent::Rejected(HsErr::Busy) => Some("rejected:busy".into()),
        UiEvent::Rejected(HsErr::Version) => Some("rejected:version".into()),
        UiEvent::Rejected(e) => Some(format!("rejected:{e:?}")),
        UiEvent::Closed { code } if *code != close_code::GATEWAY_EXIT => Some(format!("closed:{code}")),
        UiEvent::Stopped(r) => Some(r.clone().unwrap_or_else(|| "net_stopped".into())),
        _ => None,
    }
}

/// 断开、等 net 线程发完 CLOSE 退出，再等 decode 线程收尾（net 线程退出时 DecodeItem 通道随之关闭）
fn shutdown(net: NetHandle, decode: JoinHandle<DecodeReport>) -> (NetStats, DecodeReport) {
    net.send(NetIn::Disconnect);
    let stats = net.stats.clone();
    let _ = net.join.join();
    drop(net.tx);
    let dr = decode.join().unwrap_or_default();
    let st = stats.lock().map(|s| s.clone()).unwrap_or_default();
    (st, dr)
}

fn ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

// ======================== 无界面模式 ========================

#[derive(Clone, Debug)]
pub struct HeadlessOpts {
    pub duration_s: u32,
    pub hw: HwKind,
}

/// 无界面模式的结果（`to_json` 输出一行）
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeadlessReport {
    /// 密钥确认成功，并且没有遇到被拒、被关这类不会自动恢复的情况
    pub ok: bool,
    /// 第一次握手成功用了几次 HS_INIT（没成功时是一共发了几次）
    pub hs_attempts: u32,
    /// no_hs_resp（握手没有回音）、no_data（回了 HS_RESP 但 DATA 没通）、rejected:busy、closed:<code>、socket：…、decoder：…
    pub fail_reason: Option<String>,
    /// 开始握手 → 第一帧解出来
    pub first_frame_ms: Option<f64>,
    /// 开始握手 → 密钥确认
    pub connect_ms: Option<f64>,
    pub freezes: u64,
    pub freeze_total_ms: f64,
    pub w_frames: u64,
    /// 进入 L2 的帧（M2c 才有 L2 修复，M2a 恒为 0）
    pub l2: u64,
    /// 进入 L3（彻底丢失）的帧
    pub l3: u64,
    /// 解出来的帧
    pub frames: u64,
    pub decode_p50_ms: f64,
    pub decode_p95_ms: f64,
    pub decode_errors: u64,
    /// 下行 DATA 包：收到、推算丢掉
    pub recv: u64,
    pub lost: u64,
    pub reconnects: u32,
}

/// JSON 字符串转义
fn json_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn json_num(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.2}")
    } else {
        "null".into()
    }
}

impl HeadlessReport {
    fn failed(reason: String) -> Self {
        Self { fail_reason: Some(reason), ..Default::default() }
    }

    pub fn to_json(&self) -> String {
        let mut s = String::with_capacity(320);
        let opt = |v: Option<f64>| v.map_or_else(|| "null".to_string(), json_num);
        let _ = write!(s, "{{\"ok\":{},\"hs_attempts\":{},\"fail_reason\":", self.ok, self.hs_attempts);
        match &self.fail_reason {
            Some(r) => json_str(&mut s, r),
            None => s.push_str("null"),
        }
        let _ = write!(
            s,
            ",\"first_frame_ms\":{},\"connect_ms\":{},\"freezes\":{},\"freeze_total_ms\":{},\"w_frames\":{},\"l2\":{},\"l3\":{},\
             \"frames\":{},\"decode_p50_ms\":{},\"decode_p95_ms\":{},\"decode_errors\":{},\"recv\":{},\"lost\":{},\"reconnects\":{}}}",
            opt(self.first_frame_ms),
            opt(self.connect_ms),
            self.freezes,
            json_num(self.freeze_total_ms),
            self.w_frames,
            self.l2,
            self.l3,
            self.frames,
            json_num(self.decode_p50_ms),
            json_num(self.decode_p95_ms),
            self.decode_errors,
            self.recv,
            self.lost,
            self.reconnects,
        );
        s
    }

    /// 由 net、decode 两边的统计拼出结果
    fn from_stats(st: &NetStats, dr: &mut DecodeReport, fail: Option<String>) -> Self {
        let established = st.first_established.is_some();
        let fail = fail.or_else(|| {
            if established {
                None
            } else if st.hs_resp_seen {
                Some("no_data".into())
            } else if let Some(e) = &st.last_os_error {
                // 一直发不出去（路由不通、网卡没地址等）：带上系统错误，便于区分「丢包」和「本机发不出」
                Some(format!("no_hs_resp（发送出错 {} 次，最近一次：{e}）", st.send_errors))
            } else {
                Some("no_hs_resp".into())
            }
        });
        let since = |t: Option<Micros>| t.map(|t| ms(t.saturating_sub(st.connect_at)));
        let (freezes, freeze_total) = dr.freeze.as_ref().map_or((0, 0), |m| (m.freezes().len() as u64, m.total_us()));
        Self {
            ok: fail.is_none(),
            hs_attempts: if st.hs_resp_seen { st.hs_attempts_first } else { st.hs_attempts_total },
            fail_reason: fail,
            first_frame_ms: since(dr.first_frame_at),
            connect_ms: since(st.first_established),
            freezes,
            freeze_total_ms: ms(freeze_total),
            w_frames: st.asm.w_frames,
            l2: 0,
            l3: st.asm.frames_lost,
            frames: dr.frames,
            decode_p50_ms: ms(dr.lat.pct(50.0)),
            decode_p95_ms: ms(dr.lat.pct(95.0)),
            decode_errors: dr.errors,
            recv: st.recv,
            lost: st.lost,
            reconnects: st.reconnects,
        }
    }
}

/// 无界面连一次：用 FFmpeg 解码（VideoToolbox，不支持时内部退回软解）
pub fn run_headless(p: &GatewayProfile, kp: StaticKeypair, o: &HeadlessOpts) -> HeadlessReport {
    match FfmpegDecoder::new(o.hw) {
        Ok(d) => run_headless_with(p, kp, o, Box::new(d)),
        Err(e) => HeadlessReport::failed(format!("decoder：{e}")),
    }
}

/// 同上，解码器由调用方给（测试用）
pub fn run_headless_with(
    p: &GatewayProfile,
    kp: StaticKeypair,
    o: &HeadlessOpts,
    dec: Box<dyn VideoDecoder>,
) -> HeadlessReport {
    clock::init();
    let _activity = crate::platform::begin_latency_critical_activity();
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    let ui: UiSink = Arc::new(move |e| {
        let _ = ui_tx.send(e);
    });
    let (net, dec_rx) = match start_net(p, kp, ui) {
        Ok(x) => x,
        Err(e) => return HeadlessReport::failed(e),
    };
    let decode = match spawn_decode(dec_rx, &net, dec, FrameOut::Discard) {
        Ok(h) => h,
        Err(e) => {
            let _ = shutdown_net_only(net);
            return HeadlessReport::failed(e);
        }
    };
    let deadline = clock::now_us() + o.duration_s as u64 * 1_000_000;
    let mut fail = None;
    loop {
        let now = clock::now_us();
        if now >= deadline {
            break;
        }
        match ui_rx.recv_timeout(std::time::Duration::from_micros(deadline - now)) {
            Ok(ev) => {
                log_event(&ev);
                if let Some(r) = terminal_reason(&ev) {
                    fail = Some(r);
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let (st, mut dr) = shutdown(net, decode);
    if let Some(e) = &dr.first_err {
        eprintln!("解码报错 {} 次，第一次：{e}", dr.errors);
    }
    HeadlessReport::from_stats(&st, &mut dr, fail)
}

fn shutdown_net_only(net: NetHandle) -> NetStats {
    net.send(NetIn::Disconnect);
    let _ = net.join.join();
    net.stats.lock().map(|s| s.clone()).unwrap_or_default()
}

// ======================== 正常模式（有界面） ========================

/// 有界面模式结束时的摘要（打到 stderr）
#[derive(Debug, Default)]
pub struct GuiSummary {
    pub presented: u64,
    pub freezes: usize,
    pub freeze_total_ms: f64,
    pub net: NetStats,
    pub decode_errors: u64,
}

impl std::fmt::Display for GuiSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = &self.net;
        write!(
            f,
            "呈现 {} 帧；冻结 {} 次，共 {:.0} ms（W3C 口径）；W 帧 {}，L3 {} 帧；解码报错 {}；\
             下行收到 {} 包、推算丢 {} 包；握手共 {} 次，重连 {} 次，RESET {} 次",
            self.presented,
            self.freezes,
            self.freeze_total_ms,
            n.asm.w_frames,
            n.asm.frames_lost,
            self.decode_errors,
            n.recv,
            n.lost,
            n.hs_attempts_total,
            n.reconnects,
            n.resets,
        )
    }
}

/// 浮层上的状态行（ASCII）
fn status_line(e: &UiEvent) -> Option<String> {
    Some(match e {
        UiEvent::Connecting => "connecting...".into(),
        UiEvent::Connected => "connected".into(),
        UiEvent::Rejected(HsErr::Busy) => "BUSY: another client is using the gateway".into(),
        UiEvent::Rejected(e) => format!("rejected: {e:?}"),
        UiEvent::Closed { code } => format!("closed by gateway (code {code})"),
        UiEvent::Reconnecting => "reconnecting...".into(),
        UiEvent::Stopped(_) => "network stopped".into(),
        _ => return None,
    })
}

/// 有界面：开窗口、连网关、到达即呈现。窗口关掉（或 Esc、Q）才返回
pub fn run_gui(p: &GatewayProfile, kp: StaticKeypair, hw: HwKind) -> Result<GuiSummary, String> {
    clock::init();
    let _activity = crate::platform::begin_latency_critical_activity();
    let dec = FfmpegDecoder::new(hw).map_err(|e| format!("解码器：{e}"))?;
    let win = VideoWindow::open(&format!("yw-client - {}", p.name), 1280, 720)?;
    let mut presenter = SdlMetalPresenter::new(win);
    let wake = unsafe { SDL_RegisterEvents(1) };
    if wake == 0 {
        return Err("SDL_RegisterEvents 失败".into());
    }
    let (ui_tx, ui_rx) = mpsc::channel::<UiEvent>();
    let ui: UiSink = Arc::new(move |e| {
        let _ = ui_tx.send(e);
        wake_main(wake);
    });
    let (net, dec_rx) = start_net(p, kp, ui)?;
    let (mb_tx, mb_rx) = mpsc::sync_channel::<DecodedFrame>(MAILBOX);
    let decode = spawn_decode(dec_rx, &net, Box::new(dec), FrameOut::Present(mb_tx, wake))?;

    let log0 = log_stats();
    let mut freeze = FreezeMeter::new();
    let mut fps = FpsCounter::default();
    let mut ov = OverlayStats::default();
    let mut dec_win = Samples::default();
    let mut status = "connecting...".to_string();
    let mut presented = 0u64;
    let mut next_overlay = 0;
    let mut done = false;
    while !done {
        let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
        if unsafe { SDL_WaitEventTimeout(&mut ev, 50) } {
            let t = unsafe { ev.r#type };
            if t == SDL_EVENT_QUIT.0 || t == SDL_EVENT_WINDOW_CLOSE_REQUESTED.0 {
                done = true;
            } else if t == SDL_EVENT_KEY_DOWN.0 {
                let k = unsafe { ev.key.key };
                if k == SDLK_ESCAPE || k == SDLK_Q {
                    done = true;
                }
            } else if t == SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED.0 || t == SDL_EVENT_WINDOW_EXPOSED.0 {
                presenter.request_redraw();
            }
        }
        while let Ok(e) = ui_rx.try_recv() {
            log_event(&e);
            if let Some(s) = status_line(&e) {
                status = s;
                next_overlay = 0;
            }
        }
        loop {
            match mb_rx.try_recv() {
                Ok(f) => {
                    dec_win.push(f.decoded_us.saturating_sub(f.p_us));
                    ov.size = (f.width(), f.height());
                    presenter.push(f);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        let now = clock::now_us();
        if let Some(st) = presenter.render(now, None) {
            presented += 1;
            freeze.on_present(st.presented_us);
            fps.tick(st.presented_us);
        }
        if now >= next_overlay {
            // 浮层每秒更新一次；没有画面时也重画，让状态行看得见
            next_overlay = now + 1_000_000;
            let n = net.stats();
            let ls = log_stats();
            ov.fps = fps.fps();
            ov.frames = presented;
            ov.rtt_ms = n.established.then_some(n.srtt_us as f32 / 1000.0);
            let total = n.recv + n.lost;
            ov.loss_pct = (total > 0).then(|| n.lost as f32 * 100.0 / total as f32);
            ov.freezes = freeze.freezes().len() as u32;
            ov.w_frames = n.asm.w_frames as u32;
            ov.dec_p95_ms = dec_win.pct(95.0) as f32 / 1000.0;
            dec_win.clear();
            ov.missing_ref = ls.missing_ref - log0.missing_ref;
            ov.dec_errors = ls.errors - log0.errors;
            let mut lines = ov.lines();
            lines.push(status.clone());
            presenter.set_overlay(lines);
            presenter.request_redraw();
        }
    }
    let (st, dr) = shutdown(net, decode);
    drop(presenter);
    Ok(GuiSummary {
        presented,
        freezes: freeze.freezes().len(),
        freeze_total_ms: ms(freeze.total_us()),
        net: st,
        decode_errors: dr.errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::DecErr;

    fn hevc(t: u8, rest: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, t << 1, 1];
        v.extend_from_slice(rest);
        v
    }

    #[test]
    fn leading_params_from_keyframe() {
        let mut ps = hevc(32, &[0xaa]);
        ps.extend(hevc(33, &[0xbb]));
        ps.extend(hevc(34, &[0xcc]));
        let mut au = ps.clone();
        au.extend(hevc(19, &[0x80, 1, 2]));
        assert_eq!(leading_params(&au), Some((Codec::Hevc, ps)));
        // 开头不是参数集：None
        assert_eq!(leading_params(&hevc(19, &[0x80, 1])), None);
        // AVC
        let avc = [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 0x88];
        let (c, p) = leading_params(&avc).unwrap();
        assert_eq!(c, Codec::Avc);
        assert_eq!(p, avc[..12].to_vec());
    }

    #[test]
    fn json_line_shape() {
        let r = HeadlessReport {
            ok: false,
            hs_attempts: 3,
            fail_reason: Some("closed:\"1\"\n".into()),
            first_frame_ms: Some(1234.5),
            ..Default::default()
        };
        let j = r.to_json();
        assert!(!j.contains('\n'), "{j}");
        assert!(j.starts_with("{\"ok\":false,\"hs_attempts\":3,\"fail_reason\":\"closed:\\\"1\\\"\\n\""), "{j}");
        for k in [
            "first_frame_ms\":1234.50",
            "connect_ms\":null",
            "\"freezes\":0",
            "freeze_total_ms\":0.00",
            "\"w_frames\":0",
            "\"l2\":0",
            "\"l3\":0",
            "\"frames\":0",
            "decode_p50_ms",
            "decode_p95_ms",
            "\"recv\":0",
            "\"lost\":0",
        ] {
            assert!(j.contains(k), "缺 {k}：{j}");
        }
        assert!(j.ends_with('}'));
    }

    /// 不解码的假解码器（无界面模式的流程测试用）
    struct NullDecoder;

    impl VideoDecoder for NullDecoder {
        fn configure(&mut self, _c: Codec, _p: &[u8]) -> Result<(), DecErr> {
            Ok(())
        }
        fn decode(&mut self, _au: &[u8]) -> Result<Option<DecodedFrame>, DecErr> {
            Ok(None)
        }
        fn flush(&mut self) {}
    }

    /// 网关不在：握手按 0、10 ms（第二份）、250 ms、750 ms 发，1 s 后以 no_hs_resp 结束，不开窗口
    #[test]
    fn headless_no_gateway() {
        // 占一个端口但从不回包（不碰真网关）
        let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let entry = silent.local_addr().unwrap();
        let gw = StaticKeypair::from_secret([0x51; 32]);
        let p = GatewayProfile {
            name: "silent".into(),
            gw_pub: gw.public(),
            psk: ywp::crypto::Secret32::new([0x52; 32]),
            gateway_id: ywp::crypto::gateway_id(&gw.public()),
            entry,
            bind_if: None,
            bitrate_cap_kbps: None,
            mode: yw_media::Mode::Competitive,
            j_ms: None,
            auto_start: false,
        };
        let o = HeadlessOpts { duration_s: 1, hw: HwKind::Software };
        let t0 = clock::now_us();
        let r = run_headless_with(&p, StaticKeypair::from_secret([0x53; 32]), &o, Box::new(NullDecoder));
        let el = clock::now_us() - t0;
        assert!(!r.ok);
        assert_eq!(r.fail_reason.as_deref(), Some("no_hs_resp"), "{r:?}");
        assert!((3..=4).contains(&r.hs_attempts), "{r:?}");
        assert!(r.first_frame_ms.is_none() && r.connect_ms.is_none());
        assert!(el < 1_500_000, "该在 1 s 左右结束：{el} us");
        // 网关那边确实收到了 HS_INIT（1232 B）
        silent.set_nonblocking(true).unwrap();
        let mut buf = [0u8; 2048];
        let mut inits = 0;
        while let Ok((n, _)) = silent.recv_from(&mut buf) {
            assert_eq!(n, ywp::consts::HS_INIT_LEN);
            inits += 1;
        }
        assert!(inits >= 4, "收到 {inits} 个 HS_INIT");
        let j = r.to_json();
        assert!(j.contains("\"fail_reason\":\"no_hs_resp\""), "{j}");
    }
}
