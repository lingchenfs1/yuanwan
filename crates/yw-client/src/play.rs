//! 调试入口 `yw-client --play <录像.h265>`：读本地录像（Annex-B，带参数集头），按固定帧率送解码，
//! 在窗口里到达即呈现。不连网关，用来单独验证解码和呈现；跑完打印帧数和耗时统计。
//!
//! 线程和正式客户端一样：decode 线程（USER_INTERACTIVE）解码，经信箱（最多 3 帧）交给主线程，
//! 并用 SDL 自定义事件叫醒主线程；主线程收到就呈现。

use crate::clock;
use crate::decode::{log_stats, DecStats, DecodedFrame, FfmpegDecoder, HwKind, VideoDecoder};
use crate::overlay::{FpsCounter, OverlayStats, Samples};
use crate::present::{Presenter, PresenterExt, PresenterStats, SdlMetalPresenter, SdlUploadPresenter, VideoWindow};
use sdl3::sys::events::*;
use sdl3::sys::keycode::{SDLK_ESCAPE, SDLK_Q};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, TryRecvError, TrySendError};
use std::sync::Arc;
use ywp::time::Micros;
use yw_media::nal;
use yw_media::playout::FreezeMeter;
use yw_media::ywf::AnnexBReader;
use yw_media::Codec;

/// 信箱容量：Presenter 信箱最多 3 帧
const MAILBOX: usize = 3;

#[derive(Clone, Debug)]
pub struct PlayOpts {
    pub path: PathBuf,
    pub fps: f64,
    /// 最多送多少帧（None = 整段）
    pub max_frames: Option<usize>,
    /// 整段放几遍
    pub loops: u32,
    /// false：只解码、不开窗口
    pub window: bool,
    /// 用上传呈现器（和零拷贝对比用）
    pub upload: bool,
    pub hw: HwKind,
}

impl Default for PlayOpts {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            fps: 60.0,
            max_frames: None,
            loops: 1,
            window: true,
            upload: false,
            hw: HwKind::VideoToolbox,
        }
    }
}

/// decode 线程的统计
#[derive(Debug, Default)]
struct DecodeSide {
    sent: u64,
    frames: u64,
    no_frame: u64,
    errors: u64,
    corrupt: u64,
    cold_us: Option<Micros>,
    lat: Samples,
    /// 送帧时刻比预定时刻晚了超过 2 ms 的次数
    behind: u64,
    mailbox_full: u64,
    stats: DecStats,
    first_err: Option<String>,
    size: (u32, u32),
}

#[derive(Debug, Default)]
pub struct PlayReport {
    pub aus: usize,
    pub params_len: usize,
    pub elapsed_us: Micros,
    pub renderer: String,
    dec: DecodeSide,
    pub presented: u64,
    pub present: PresenterStats,
    /// 解码出帧 → 呈现完成
    present_lat: Samples,
    /// 相邻两次呈现的间隔
    present_gap: Samples,
    /// W3C 口径（yw-media 的 FreezeMeter，按呈现时刻）
    pub freezes: u32,
    pub freeze_total_ms: f64,
    pub missing_ref_logs: u64,
    pub error_logs: u64,
    pub stopped_by_user: bool,
}

impl PlayReport {
    pub fn decode_errors(&self) -> u64 {
        self.dec.errors
    }

    pub fn frames_decoded(&self) -> u64 {
        self.dec.frames
    }
}

impl fmt::Display for PlayReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |us: u64| us as f64 / 1000.0;
        let d = &self.dec;
        let mut lat = d.lat.clone();
        writeln!(
            f,
            "录像：{} 个 AU，参数集 {} B，{}×{}；共送 {} 帧，用时 {:.2} s（{:.1} 帧/秒）{}",
            self.aus,
            self.params_len,
            d.size.0,
            d.size.1,
            d.sent,
            self.elapsed_us as f64 / 1e6,
            if self.elapsed_us > 0 { d.sent as f64 * 1e6 / self.elapsed_us as f64 } else { 0.0 },
            if self.stopped_by_user { "，中途被关闭" } else { "" },
        )?;
        writeln!(
            f,
            "解码：出帧 {}，没出帧 {}，报错 {}，有损帧 {}；首帧冷启动 {:.1} ms；送入到出帧 P50 {:.2} ms、P95 {:.2} ms、P99 {:.2} ms、最大 {:.2} ms",
            d.frames,
            d.no_frame,
            d.errors,
            d.corrupt,
            ms(d.cold_us.unwrap_or(0)),
            ms(lat.pct(50.0)),
            ms(lat.pct(95.0)),
            ms(lat.pct(99.0)),
            ms(lat.max()),
        )?;
        writeln!(
            f,
            "多缓帧检查：max_lag {}，晚出帧 {}，一次出多帧丢弃 {}，软解回退 {}，重开解码器 {} 次；送帧比预定时刻晚 > 2 ms 的 {} 次",
            d.stats.max_lag, d.stats.lagged, d.stats.extra_dropped, d.stats.sw_fallback, d.stats.opens, d.behind,
        )?;
        writeln!(f, "FFmpeg 日志：找不到参考 {} 行（正常现象），其余错误 {} 行", self.missing_ref_logs, self.error_logs)?;
        if let Some(e) = &d.first_err {
            writeln!(f, "第一次解码报错：{e}")?;
        }
        if self.renderer.is_empty() {
            write!(f, "呈现：没开窗口（--no-window）")?;
        } else {
            let mut pl = self.present_lat.clone();
            let mut pg = self.present_gap.clone();
            write!(
                f,
                "呈现（{}）：{} 帧（零拷贝 {}，上传 {}），被新帧顶掉 {}，信箱满丢弃 {}，建纹理失败 {}；\
                 出帧到呈现完成 P50 {:.2} ms、P95 {:.2} ms；呈现间隔 P50 {:.2} ms、P95 {:.2} ms、最大 {:.2} ms；冻结 {} 次、共 {:.0} ms（W3C 口径）",
                self.renderer,
                self.presented,
                self.present.zero_copy,
                self.present.uploaded,
                self.present.skipped,
                d.mailbox_full,
                self.present.tex_errors,
                ms(pl.pct(50.0)),
                ms(pl.pct(95.0)),
                ms(pg.pct(50.0)),
                ms(pg.pct(95.0)),
                ms(pg.max()),
                self.freezes,
                self.freeze_total_ms,
            )?;
        }
        Ok(())
    }
}

/// 读进来的录像：每个 AU 带 4 字节起始码；关键帧前面补上参数集（和 Assembler 交给解码线程的一样）
#[derive(Debug, Default)]
pub struct Clip {
    pub aus: Vec<Vec<u8>>,
    /// 文件开头的参数集
    pub params: Vec<u8>,
}

/// 用 yw-media 的 AnnexBReader 把录像（Annex-B，带参数集头）切成 AU。
/// 中途单独出现的参数集并进后面那一帧；关键帧前面没有参数集时补上最新的一份
pub fn load_clip(path: &Path, c: Codec) -> Result<Clip, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("读不到 {}：{e}", path.display()))?;
    let mut r = AnnexBReader::new(std::io::BufReader::new(f), c);
    let mut clip = Clip::default();
    let mut prefix: Vec<u8> = Vec::new();
    while let Some(au) = r.next_au().map_err(|e| format!("读 {} 出错：{e}", path.display()))? {
        if nal::is_param_sets_only(&au, c) {
            prefix = au;
            continue;
        }
        let key = nal::is_keyframe(&au, c);
        if prefix.is_empty() && key && !nal::starts_with_param_set(&au, c) {
            prefix = r.param_sets().to_vec();
        }
        if prefix.is_empty() {
            clip.aus.push(au);
        } else {
            let mut v = std::mem::take(&mut prefix);
            v.extend_from_slice(&au);
            clip.aus.push(v);
        }
    }
    clip.params = r.param_sets().to_vec();
    Ok(clip)
}

/// 叫醒主线程的 SDL 自定义事件（0 表示没开窗口）
type WakeType = u32;

fn wake_main(t: WakeType) {
    if t == 0 {
        return;
    }
    let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
    ev.r#type = t;
    unsafe { SDL_PushEvent(&mut ev) };
}

fn decode_thread(
    aus: Arc<Vec<Vec<u8>>>,
    params: Vec<u8>,
    o: PlayOpts,
    tx: Option<mpsc::SyncSender<DecodedFrame>>,
    wake: WakeType,
    stop: Arc<AtomicBool>,
) -> Result<DecodeSide, String> {
    crate::platform::set_thread_qos_interactive();
    let mut s = DecodeSide::default();
    let t0 = clock::now_us();
    let mut dec = FfmpegDecoder::new(o.hw).map_err(|e| e.to_string())?;
    dec.configure(Codec::Hevc, &params).map_err(|e| e.to_string())?;
    let frame_us = (1e6 / o.fps.max(1.0)) as u64;
    let total = aus.len() * o.loops.max(1) as usize;
    let n = o.max_frames.map_or(total, |m| m.min(total));
    let start = clock::now_us();
    for i in 0..n {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let due = start + i as u64 * frame_us;
        let now = clock::now_us();
        if now > due + 2_000 {
            s.behind += 1;
        } else {
            clock::sleep_until(due);
        }
        let au = &aus[i % aus.len()];
        let sent_at = clock::now_us();
        s.sent += 1;
        match dec.decode(au) {
            Ok(Some(mut f)) => {
                let done = clock::now_us();
                if s.cold_us.is_none() {
                    s.cold_us = Some(done - t0);
                    s.size = (f.width(), f.height());
                } else {
                    s.lat.push(done - sent_at);
                }
                s.frames += 1;
                s.corrupt += f.is_corrupt() as u64;
                f.fid = i as u64;
                f.p_us = due;
                if let Some(tx) = &tx {
                    match tx.try_send(f) {
                        Ok(()) => wake_main(wake),
                        Err(TrySendError::Full(_)) => s.mailbox_full += 1,
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
            }
            Ok(None) => s.no_frame += 1,
            Err(e) => {
                s.errors += 1;
                if s.first_err.is_none() {
                    s.first_err = Some(format!("第 {i} 帧：{e}"));
                }
            }
        }
    }
    s.stats = dec.stats();
    Ok(s)
}

/// 主线程跑：放完（或窗口被关掉）才返回
pub fn run(o: &PlayOpts) -> Result<PlayReport, String> {
    clock::init();
    let Clip { aus, params } = load_clip(&o.path, Codec::Hevc)?;
    if aus.is_empty() {
        return Err("录像里切不出 AU（要 Annex-B 的 HEVC）".into());
    }
    if params.is_empty() {
        return Err("录像开头没有参数集（VPS/SPS/PPS）".into());
    }
    let mut rep = PlayReport { aus: aus.len(), params_len: params.len(), ..Default::default() };
    let log0 = log_stats();
    let aus = Arc::new(aus);
    let stop = Arc::new(AtomicBool::new(false));
    let _activity = crate::platform::begin_latency_critical_activity();
    let t0 = clock::now_us();

    if !o.window {
        let h = {
            let (aus, o, stop) = (aus.clone(), o.clone(), stop.clone());
            std::thread::Builder::new()
                .name("decode".into())
                .spawn(move || decode_thread(aus, params, o, None, 0, stop))
                .map_err(|e| e.to_string())?
        };
        rep.dec = h.join().map_err(|_| "decode 线程 panic".to_string())??;
        rep.elapsed_us = clock::now_us() - t0;
        finish_logs(&mut rep, log0);
        return Ok(rep);
    }

    let win = VideoWindow::open("yw-client --play", 1280, 720)?;
    let mut presenter: Box<dyn PresentAll> =
        if o.upload { Box::new(SdlUploadPresenter::new(win)) } else { Box::new(SdlMetalPresenter::new(win)) };
    rep.renderer = presenter.renderer_name();
    let wake = unsafe { SDL_RegisterEvents(1) };
    if wake == 0 {
        return Err("SDL_RegisterEvents 失败".into());
    }
    let (tx, rx) = mpsc::sync_channel::<DecodedFrame>(MAILBOX);
    let h = {
        let (aus, o, stop) = (aus.clone(), o.clone(), stop.clone());
        std::thread::Builder::new()
            .name("decode".into())
            .spawn(move || decode_thread(aus, params, o, Some(tx), wake, stop))
            .map_err(|e| e.to_string())?
    };

    let mut fps = FpsCounter::default();
    let mut last_present: Option<Micros> = None;
    let mut freeze = FreezeMeter::new();
    let mut last_pushed: Option<(u64, Micros)> = None; // (fid, decoded_us)
    let mut ov = OverlayStats::default();
    // 这一秒里每帧的「预定送入时刻 → 出帧」，给浮层算解码 P95
    let mut dec_win = Samples::default();
    let mut done = false;
    while !done {
        let mut ev: SDL_Event = unsafe { std::mem::zeroed() };
        if unsafe { SDL_WaitEventTimeout(&mut ev, 50) } {
            let t = unsafe { ev.r#type };
            if t == SDL_EVENT_QUIT.0 || t == SDL_EVENT_WINDOW_CLOSE_REQUESTED.0 {
                rep.stopped_by_user = true;
                stop.store(true, Ordering::Relaxed);
            } else if t == SDL_EVENT_KEY_DOWN.0 {
                let k = unsafe { ev.key.key };
                if k == SDLK_ESCAPE || k == SDLK_Q {
                    rep.stopped_by_user = true;
                    stop.store(true, Ordering::Relaxed);
                }
            } else if t == SDL_EVENT_WINDOW_PIXEL_SIZE_CHANGED.0 || t == SDL_EVENT_WINDOW_EXPOSED.0 {
                presenter.request_redraw();
            }
        }
        let mut pushed = false;
        loop {
            match rx.try_recv() {
                Ok(f) => {
                    last_pushed = Some((f.fid, f.decoded_us));
                    dec_win.push(f.decoded_us.saturating_sub(f.p_us));
                    ov.size = (f.width(), f.height());
                    presenter.push(f);
                    pushed = true;
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
            rep.presented += 1;
            if let Some((fid, dec_us)) = last_pushed {
                if fid == st.fid {
                    rep.present_lat.push(st.presented_us.saturating_sub(dec_us));
                }
            }
            if let Some(lp) = last_present {
                rep.present_gap.push(st.presented_us - lp);
            }
            freeze.on_present(st.presented_us);
            last_present = Some(st.presented_us);
            if fps.tick(st.presented_us) {
                let ls = log_stats();
                ov.fps = fps.fps();
                ov.frames = rep.presented;
                ov.freezes = freeze.freezes().len() as u32;
                ov.dec_p95_ms = dec_win.pct(95.0) as f32 / 1000.0;
                dec_win.clear();
                ov.missing_ref = ls.missing_ref - log0.missing_ref;
                ov.dec_errors = ls.errors - log0.errors;
                presenter.set_overlay(ov.lines());
            }
        } else if !pushed && done {
            break;
        }
    }
    rep.dec = h.join().map_err(|_| "decode 线程 panic".to_string())??;
    rep.elapsed_us = clock::now_us() - t0;
    rep.present = presenter.stats();
    rep.freezes = freeze.freezes().len() as u32;
    rep.freeze_total_ms = freeze.total_us() as f64 / 1000.0;
    finish_logs(&mut rep, log0);
    drop(presenter);
    Ok(rep)
}

fn finish_logs(rep: &mut PlayReport, log0: crate::decode::LogStats) {
    let ls = log_stats();
    rep.missing_ref_logs = ls.missing_ref - log0.missing_ref;
    rep.error_logs = ls.errors - log0.errors;
}

/// Presenter 加附加操作，装进同一个 Box 用
trait PresentAll: Presenter + PresenterExt {}
impl<T: Presenter + PresenterExt> PresentAll for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_clip_prefixes_params_on_keyframes() {
        fn hevc(t: u8, rest: &[u8]) -> Vec<u8> {
            let mut v = vec![0, 0, 0, 1, t << 1, 1];
            v.extend_from_slice(rest);
            v
        }
        let mut ps = hevc(32, &[0xaa]);
        ps.extend(hevc(33, &[0xbb]));
        ps.extend(hevc(34, &[0xcc]));
        let k0 = hevc(19, &[0x80, 1, 2]);
        let p1 = hevc(1, &[0x80, 3]);
        let sps2 = hevc(33, &[0xdd]);
        let k2 = hevc(20, &[0x80, 4]);
        let mut file = ps.clone();
        for x in [&k0, &p1, &sps2, &k2] {
            file.extend_from_slice(x);
        }
        let path = std::env::temp_dir().join(format!("yw-client-clip-{}.h265", std::process::id()));
        std::fs::write(&path, &file).unwrap();
        let clip = load_clip(&path, Codec::Hevc);
        let _ = std::fs::remove_file(&path);
        let clip = clip.unwrap();
        assert_eq!(clip.aus.len(), 3);
        // 开头的参数集补到第一个关键帧前面
        let mut want0 = ps.clone();
        want0.extend_from_slice(&k0);
        assert_eq!(clip.aus[0], want0);
        assert_eq!(clip.aus[1], p1);
        // 中途换的参数集并进后面那一帧
        let mut want2 = sps2.clone();
        want2.extend_from_slice(&k2);
        assert_eq!(clip.aus[2], want2);
        assert!(nal::is_keyframe(&clip.aus[2], Codec::Hevc));
        assert!(load_clip(Path::new("/nonexistent/yw.h265"), Codec::Hevc).is_err());
    }

    /// 不开窗口，只解码：整条 --play 路径（读文件、切 AU、decode 线程、统计）能跑完
    #[test]
    fn play_headless_runs() {
        let Some(path) = std::env::var_os("YW_TEST_H265") else {
            eprintln!("没设 YW_TEST_H265，跳过");
            return;
        };
        let o = PlayOpts { path: path.into(), fps: 600.0, max_frames: Some(300), window: false, ..Default::default() };
        let rep = run(&o).unwrap();
        eprintln!("{rep}");
        assert_eq!(rep.frames_decoded(), 300);
        assert_eq!(rep.decode_errors(), 0);
    }
}
