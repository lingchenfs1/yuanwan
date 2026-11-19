//! 会话回调：统计、参数集缓存、截屏和录像，chiaki 日志的过滤，以及把视频交给 YWP 服务循环。
//! 回调都在 chiaki 的线程上执行，所以只做计数、拷贝和少量文件写入：
//! - 视频帧拷贝一份经 ServeHandle::push（try_send + wake）交给 ywp 线程，带递增的 seq；队列满了不阻塞；
//! - 参数集、音频头写进 Hub 按 epoch 的最新值槽（只拿那一把短锁），再发一条通知；
//! - 关键帧到达时写 Hub.last_key_us（原子量，不拿 PsLink 的锁）；
//! - 有客户端时 push 失败返回 false（chiaki 不把这一帧记作参考，下一帧开始时向 PS5 报 CORRUPTFRAME）；
//!   丢的是关键帧还要置标志，让 Ingress 进 WaitKey(Overflow)。没有客户端时总是返回 true，行为和 M1 一样。

use crate::hub::Hub;
use crate::jlog::Logger;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use yw_chiaki_sys::{log_level, Event, Handler, QuitInfo};
use yw_media::nal::{self, Codec};
use yw_serve::{AudioHeader, MediaMsg, ServeHandle};

/// 进程内的单调时钟微秒数。和 yw-serve 的服务循环、IdrGovernor 用同一个时钟（yw_serve::clock）
pub use yw_serve::clock::now_us;

/// 写盘线程的缓冲上限（帧）。15 Mbps 下约 2 s、几 MB；满了说明磁盘跟不上，就中断录制。
const REC_QUEUE_FRAMES: usize = 120;

/// 截屏或录像：从下一个关键帧开始写（带参数集），写够帧数或时长后把 .tmp 改名成正式文件。
/// 收包线程（on_video）只把帧拷进有界通道；建文件、写入、改名都在单独的写盘线程里做。
/// 录制没写完就被丢弃（会话结束、被新命令替换、写盘跟不上）时，写盘线程删掉 .tmp 并记日志。
pub struct Recorder {
    tx: SyncSender<Vec<u8>>,
    finished: Arc<AtomicBool>,
    started: bool,
    frames_left: Option<u32>,
    duration_us: Option<u64>,
    until_us: Option<u64>,
}

enum Feed {
    More,
    Done,
    Abort(String),
}

impl Recorder {
    pub fn frames(path: PathBuf, n: u32, log: Arc<Logger>) -> Result<Recorder, String> {
        Self::spawn(path, Some(n), None, log)
    }

    pub fn seconds(path: PathBuf, secs: f64, log: Arc<Logger>) -> Result<Recorder, String> {
        Self::spawn(path, None, Some((secs * 1e6) as u64), log)
    }

    fn spawn(path: PathBuf, frames_left: Option<u32>, duration_us: Option<u64>, log: Arc<Logger>) -> Result<Recorder, String> {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(REC_QUEUE_FRAMES);
        let finished = Arc::new(AtomicBool::new(false));
        let fin = finished.clone();
        std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || write_recording(rx, path, fin, log))
            .map_err(|e| format!("建不了写盘线程：{e}"))?;
        Ok(Recorder { tx, finished, started: false, frames_left, duration_us, until_us: None })
    }

    fn feed(&mut self, data: &[u8], key: bool, header: &Mutex<Vec<u8>>) -> Feed {
        if !self.started {
            if !key {
                return Feed::More;
            }
            let h = header.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if !h.is_empty() && self.tx.try_send(h).is_err() {
                return Feed::Abort("写盘线程已退出".into());
            }
            self.started = true;
            if let Some(d) = self.duration_us {
                self.until_us = Some(now_us() + d);
            }
        }
        match self.tx.try_send(data.to_vec()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Feed::Abort(format!("写盘跟不上（缓冲 {REC_QUEUE_FRAMES} 帧已满）")),
            Err(TrySendError::Disconnected(_)) => return Feed::Abort("写盘线程已退出".into()),
        }
        let done = match (self.frames_left.as_mut(), self.until_us) {
            (Some(n), _) => {
                *n = n.saturating_sub(1);
                *n == 0
            }
            (None, Some(u)) => now_us() >= u,
            _ => false,
        };
        if done {
            self.finished.store(true, Relaxed);
            Feed::Done
        } else {
            Feed::More
        }
    }
}

/// 写盘线程：收到第一块数据才建 .tmp；通道关闭时，写完了就改名，没写完就删掉 .tmp。
fn write_recording(rx: Receiver<Vec<u8>>, path: PathBuf, finished: Arc<AtomicBool>, log: Arc<Logger>) {
    let tmp = path.with_extension("tmp");
    let mut file: Option<File> = None;
    for buf in rx.iter() {
        if file.is_none() {
            match File::create(&tmp) {
                Ok(f) => file = Some(f),
                Err(e) => {
                    log.warn("record", &format!("写不了 {}：{e}", tmp.display()));
                    return;
                }
            }
        }
        if let Some(f) = file.as_mut() {
            if let Err(e) = f.write_all(&buf) {
                drop(file.take());
                let _ = std::fs::remove_file(&tmp);
                log.warn("record", &format!("写入 {} 失败，已删除：{e}", tmp.display()));
                return;
            }
        }
    }
    // 发送端已丢弃：正常写完，或被中断
    let Some(f) = file else {
        if !finished.load(Relaxed) {
            log.info("record", &format!("录制 {} 在收到关键帧之前被取消", path.display()));
        }
        return;
    };
    drop(f);
    if finished.load(Relaxed) {
        match std::fs::rename(&tmp, &path) {
            Ok(()) => log.info("record", &format!("已写入 {}", path.display())),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                log.warn("record", &format!("改名成 {} 失败，已删除临时文件：{e}", path.display()));
            }
        }
    } else {
        let _ = std::fs::remove_file(&tmp);
        log.warn("record", &format!("录制 {} 被中断，已删除临时文件", path.display()));
    }
}

/// 一次 PS5 会话的共享状态。
pub struct Shared {
    pub hevc: bool,
    /// 凭据里有 console_pin（已在建会话前预设给 chiaki）
    pub has_pin: bool,
    pub connected: AtomicBool,
    pub quit: Mutex<Option<QuitInfo>>,
    /// PS5 要登录 PIN 但没有、或 PIN 不对：会话线程会一直等 PIN，监督线程看到后要结束会话
    pub pin_failed: Mutex<Option<String>>,
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub keyframes: AtomicU64,
    pub frames_lost: AtomicI64,
    pub frames_recovered: AtomicU64,
    pub fec_failures: AtomicU64,
    pub audio_frames: AtomicU64,
    pub haptic_frames: AtomicU64,
    pub last_frame_us: AtomicU64,
    pub last_keyframe_us: AtomicU64,
    /// PS5 在 CONNECTIONQUALITY 里给的目标码率（bit/s），从 chiaki 的日志里解析
    pub target_bitrate: AtomicU64,
    /// 下面这些 chiaki 日志太多，只计数不写日志
    pub noise_fec_attempts: AtomicU64,
    pub noise_missing_frames: AtomicU64,
    pub noise_takion_dup: AtomicU64,
    pub fast_resends: AtomicU64,
    pub header: Mutex<Vec<u8>>,
    pub recorder: Mutex<Option<Recorder>>,
    pub log: Arc<Logger>,
    /// 本会话的 epoch（监督线程 install 之后、start 之前写）
    pub epoch: AtomicU32,
    /// MediaMsg::Video 的递增序号（每个视频回调加 1，push 失败也加，服务循环据此看出跳号）
    pub video_seq: AtomicU32,
    /// 有客户端时 push 失败的次数（回调返回了 false）
    pub media_full: AtomicU64,
}

impl Shared {
    pub fn new(hevc: bool, has_pin: bool, log: Arc<Logger>) -> Shared {
        Shared {
            hevc,
            has_pin,
            connected: AtomicBool::new(false),
            quit: Mutex::new(None),
            pin_failed: Mutex::new(None),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            keyframes: AtomicU64::new(0),
            frames_lost: AtomicI64::new(0),
            frames_recovered: AtomicU64::new(0),
            fec_failures: AtomicU64::new(0),
            audio_frames: AtomicU64::new(0),
            haptic_frames: AtomicU64::new(0),
            last_frame_us: AtomicU64::new(0),
            last_keyframe_us: AtomicU64::new(0),
            target_bitrate: AtomicU64::new(0),
            noise_fec_attempts: AtomicU64::new(0),
            noise_missing_frames: AtomicU64::new(0),
            noise_takion_dup: AtomicU64::new(0),
            fast_resends: AtomicU64::new(0),
            header: Mutex::new(Vec::new()),
            recorder: Mutex::new(None),
            log,
            epoch: AtomicU32::new(0),
            video_seq: AtomicU32::new(0),
            media_full: AtomicU64::new(0),
        }
    }

    pub fn take_quit(&self) -> Option<QuitInfo> {
        self.quit.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn take_pin_failed(&self) -> Option<String> {
        self.pin_failed.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

pub struct GwHandler {
    pub s: Arc<Shared>,
    pub hub: Arc<Hub>,
    /// None：YWP 没启动（keys/ 有问题或没配对客户端），行为和 M1 一样
    pub serve: Option<Arc<ServeHandle>>,
}

impl GwHandler {
    fn codec(&self) -> Codec {
        if self.s.hevc {
            Codec::Hevc
        } else {
            Codec::Avc
        }
    }
}

impl Handler for GwHandler {
    fn on_log(&self, level: u32, msg: &str) {
        let s = &self.s;
        if msg.contains("connection quality: target_bitrate=") {
            if let Some(rest) = msg.split("target_bitrate=").nth(1) {
                let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = num.parse::<u64>() {
                    s.target_bitrate.store(v, Relaxed);
                }
            }
            return;
        }
        if level & (log_level::VERBOSE | log_level::DEBUG) != 0 {
            return;
        }
        // 弱网下每秒几十条的日志：只计数
        if msg.starts_with("Frame Processor received") || msg.starts_with("FEC successful") {
            s.noise_fec_attempts.fetch_add(1, Relaxed);
            return;
        }
        if msg.contains("missing or corrupt frame")
            || msg.contains("reporting corrupt frame")
            || msg.starts_with("Missing unit")
            || msg.starts_with("Missing reference frame")
            || msg.starts_with("Failed to complete frame")
            || msg.starts_with("Video receiver could not flush")
            || msg.starts_with("FEC failed")
        {
            s.noise_missing_frames.fetch_add(1, Relaxed);
            return;
        }
        if msg.starts_with("Takion dropping data") {
            s.noise_takion_dup.fetch_add(1, Relaxed);
            return;
        }
        if msg.contains("fast re-sending") {
            s.fast_resends.fetch_add(1, Relaxed);
            return;
        }
        // ctrl 未知消息的 hexdump（多行，形如「     0 00 00 00 08 …」）不写
        let is_hexdump_line = msg.starts_with("offset 0")
            || (msg.starts_with("    ") && msg.trim_start().split(' ').next().is_some_and(|t| t.chars().all(|c| c.is_ascii_hexdigit())) && msg.len() > 20);
        if is_hexdump_line {
            return;
        }
        let lvl = if level & log_level::ERROR != 0 {
            'E'
        } else if level & log_level::WARNING != 0 {
            'W'
        } else {
            'I'
        };
        s.log.log(lvl, "chiaki", msg);
    }

    fn on_event(&self, ev: Event) {
        let s = &self.s;
        match ev {
            Event::Connected => {
                s.connected.store(true, Relaxed);
                s.log.info("session", "已连上 PS5");
            }
            Event::Quit(q) => {
                s.log.info("session", &format!("会话结束：{}{}", q.reason, q.text.as_deref().map(|t| format!("（{t}）")).unwrap_or_default()));
                *s.quit.lock().unwrap_or_else(|e| e.into_inner()) = Some(q);
            }
            Event::LoginPin { incorrect } => {
                // 只记标志：这个回调在 chiaki 持锁时调用，不能在这里调任何会话接口
                let problem = if incorrect {
                    Some("PS5 说登录 PIN 不对")
                } else if !s.has_pin {
                    Some("PS5 要求登录 PIN，但凭据里没有 console_pin")
                } else {
                    None
                };
                match problem {
                    Some(m) => *s.pin_failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(m.to_string()),
                    None => s.log.info("session", "PS5 要求登录 PIN，已用凭据里的 PIN"),
                }
            }
            Event::FecFailure => {
                s.fec_failures.fetch_add(1, Relaxed);
            }
            _ => {}
        }
    }

    /// 返回 false 表示没收下（只在有客户端、而 MediaMsg 队列满时）
    fn on_video(&self, data: &[u8], frames_lost: i32, recovered: bool) -> bool {
        let s = &self.s;
        let codec = self.codec();
        let epoch = s.epoch.load(Relaxed);
        if nal::is_param_sets_only(data, codec) {
            *s.header.lock().unwrap_or_else(|e| e.into_inner()) = data.to_vec();
            self.hub.set_params(epoch, data);
            if let Some(sv) = &self.serve {
                // 只是通知：丢了也不要紧，服务循环会从最新值槽补取
                sv.push(MediaMsg::ParamsChanged { epoch });
            }
            return true;
        }
        let key = nal::is_keyframe(data, codec);
        let t = now_us();
        s.frames.fetch_add(1, Relaxed);
        s.bytes.fetch_add(data.len() as u64, Relaxed);
        s.last_frame_us.store(t, Relaxed);
        if key {
            s.keyframes.fetch_add(1, Relaxed);
            s.last_keyframe_us.store(t, Relaxed);
            self.hub.note_keyframe(t);
        }
        if frames_lost > 0 {
            s.frames_lost.fetch_add(frames_lost as i64, Relaxed);
        }
        if recovered {
            s.frames_recovered.fetch_add(1, Relaxed);
        }
        let mut rec = s.recorder.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = rec.as_mut() {
            match r.feed(data, key, &s.header) {
                Feed::More => {}
                Feed::Done => *rec = None, // 丢掉发送端，写盘线程收尾改名
                Feed::Abort(why) => {
                    s.log.warn("record", &format!("录制中断：{why}"));
                    *rec = None;
                }
            }
        }
        drop(rec);
        let Some(sv) = &self.serve else { return true };
        let seq = s.video_seq.fetch_add(1, Relaxed).wrapping_add(1);
        let msg = MediaMsg::Video { seq, epoch, data: data.to_vec(), frames_lost, recovered, rx_us: t };
        if sv.push(msg) || !sv.client_present() {
            // 没有客户端时队列满了也照常返回 true：不影响 PS5 会话（M1 行为）
            return true;
        }
        s.media_full.fetch_add(1, Relaxed);
        if key {
            sv.note_key_overflow();
        }
        false
    }

    fn on_audio(&self, _data: &[u8]) {
        self.s.audio_frames.fetch_add(1, Relaxed);
    }

    fn on_haptics(&self, _data: &[u8]) {
        self.s.haptic_frames.fetch_add(1, Relaxed);
    }

    fn on_audio_header(&self, channels: u32, bits: u32, rate: u32, frame_size: u32) {
        self.s.log.info("audio", &format!("声道 {channels}，位深 {bits}，采样率 {rate}，帧长 {frame_size}"));
        let epoch = self.s.epoch.load(Relaxed);
        self.hub.set_audio_header(epoch, AudioHeader { channels, bits, rate, frame_size });
        if let Some(sv) = &self.serve {
            sv.push(MediaMsg::AudioHeaderChanged { epoch });
        }
    }
}
