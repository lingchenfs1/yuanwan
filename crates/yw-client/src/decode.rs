//! 解码（只在 ffmpeg-sys-next 上包一层薄封装，不用 ffmpeg-next）。
//! FFmpeg + VideoToolbox，LOW_DELAY、thread_count = 1，同步 send/receive：送一帧、当场取出这一帧。
//!
//! - 参数集：`configure` 把 VPS/SPS/PPS（Annex-B）放进 extradata；参数集或编码没变时什么都不做，
//!   变了（会话中途换 SPS/PPS、重建会话）就关掉旧解码器、重开一个。码流里带的参数集 FFmpeg 自己会处理。
//! - 报错：`send_packet`/`receive_frame` 返回错误时，`decode` 返回 [`DecErr::Ffmpeg`]，由调用方上报
//!   （发 IDR_REQUEST）。FFmpeg 日志里的「Could not find ref with POC」在 PS5 段修补过的帧之后是正常现象
//!   （网关照常转发修补帧并置 W 位），只计数、不打印，也不算解码出错，不据此请求 IDR。
//! - VideoToolbox 不支持这路码流时（get_format 里没有 VIDEOTOOLBOX），退回软解并记一次。

use ffmpeg_sys_next as ff;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Once;
use ywp::time::Micros;
use yw_media::Codec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwKind {
    VideoToolbox,
    /// Windows（以后）
    D3d11va,
    Software,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum DecErr {
    /// 找不到解码器或硬件不可用
    Unavailable(String),
    /// FFmpeg 返回的错误码
    Ffmpeg(i32),
    NotConfigured,
}

impl std::fmt::Display for DecErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecErr::Unavailable(s) => write!(f, "解码器不可用：{s}"),
            DecErr::Ffmpeg(e) => write!(f, "FFmpeg 返回 {e}（{}）", av_err_str(*e)),
            DecErr::NotConfigured => f.write_str("解码器还没配置参数集"),
        }
    }
}

impl std::error::Error for DecErr {}

/// FFmpeg 错误码的文字说明
pub fn av_err_str(e: i32) -> String {
    let mut buf = [0 as c_char; 128];
    unsafe {
        ff::av_strerror(e, buf.as_mut_ptr(), buf.len());
        CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
    }
}

/// 色彩信息（FFmpeg 的 AVColor* 原值）。呈现时色彩空间必须设成 BT709_LIMITED
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ColorInfo {
    pub primaries: i32,
    pub transfer: i32,
    pub matrix: i32,
    pub full_range: bool,
}

/// 持有一个 AVFrame（VideoToolbox 时里面是 CVPixelBuffer），drop 时释放
pub struct AvFrameRef(NonNull<ff::AVFrame>);

// AVFrame 只在一个线程上同时使用：decode 线程产出后整体移交给主线程
unsafe impl Send for AvFrameRef {}

impl Drop for AvFrameRef {
    fn drop(&mut self) {
        let mut p = self.0.as_ptr();
        unsafe { ff::av_frame_free(&mut p) };
    }
}

impl AvFrameRef {
    /// 接管一个 av_frame_alloc 出来的帧
    pub(crate) fn from_raw(p: NonNull<ff::AVFrame>) -> Self {
        Self(p)
    }

    pub(crate) fn as_ptr(&self) -> *mut ff::AVFrame {
        self.0.as_ptr()
    }
}

pub struct DecodedFrame {
    pub fid: u64,
    pub p_us: Micros,
    pub decoded_us: Micros,
    pub color: ColorInfo,
    frame: AvFrameRef,
}

impl DecodedFrame {
    pub fn width(&self) -> u32 {
        unsafe { (*self.frame.as_ptr()).width as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { (*self.frame.as_ptr()).height as u32 }
    }

    /// 是不是 VideoToolbox 的硬件帧（data[3] 是 CVPixelBufferRef）
    pub fn is_videotoolbox(&self) -> bool {
        unsafe { (*self.frame.as_ptr()).format == ff::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX as c_int }
    }

    /// VideoToolbox 帧的 CVPixelBufferRef（不转移所有权，随本帧释放）
    pub fn cv_pixel_buffer(&self) -> Option<NonNull<c_void>> {
        if !self.is_videotoolbox() {
            return None;
        }
        NonNull::new(unsafe { (*self.frame.as_ptr()).data[3] } as *mut c_void)
    }

    /// 解码器认为这一帧有损（缺参考、码流错误）
    pub fn is_corrupt(&self) -> bool {
        let f = unsafe { &*self.frame.as_ptr() };
        f.flags & ff::AV_FRAME_FLAG_CORRUPT != 0 || f.decode_error_flags != 0
    }

    pub(crate) fn av_frame(&self) -> &AvFrameRef {
        &self.frame
    }
}

pub trait VideoDecoder: Send {
    fn configure(&mut self, codec: Codec, params: &[u8]) -> Result<(), DecErr>;
    fn decode(&mut self, au: &[u8]) -> Result<Option<DecodedFrame>, DecErr>;
    fn flush(&mut self);
}

// ---- FFmpeg 日志：全局回调，分类计数 ----

static LOG_INIT: Once = Once::new();
static LOG_MISSING_REF: AtomicU64 = AtomicU64::new(0);
static LOG_ERRORS: AtomicU64 = AtomicU64::new(0);
static LOG_WARNINGS: AtomicU64 = AtomicU64::new(0);
/// 打到 stderr 的行数上限（之后只计数），免得出错时刷屏
const LOG_PRINT_LIMIT: u64 = 50;

/// FFmpeg 日志的累计计数（进程内全局）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogStats {
    /// 「Could not find ref with POC」：PS5 段修补帧之后的正常现象，不算出错
    pub missing_ref: u64,
    /// 其余 error 级别以上的日志
    pub errors: u64,
    pub warnings: u64,
}

pub fn log_stats() -> LogStats {
    LogStats {
        missing_ref: LOG_MISSING_REF.load(Ordering::Relaxed),
        errors: LOG_ERRORS.load(Ordering::Relaxed),
        warnings: LOG_WARNINGS.load(Ordering::Relaxed),
    }
}

/// 日志行分类：找不到参考是正常现象
fn is_benign_missing_ref(line: &str) -> bool {
    line.contains("Could not find ref with POC")
}

unsafe extern "C" fn log_cb(avcl: *mut c_void, level: c_int, fmt: *const c_char, vl: ff::va_list) {
    if level > ff::AV_LOG_WARNING {
        return;
    }
    let mut buf = [0 as c_char; 512];
    let mut prefix: c_int = 1;
    ff::av_log_format_line2(avcl, level, fmt, vl, buf.as_mut_ptr(), buf.len() as c_int, &mut prefix);
    let line = CStr::from_ptr(buf.as_ptr()).to_string_lossy();
    let line = line.trim_end();
    if is_benign_missing_ref(line) {
        LOG_MISSING_REF.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let n = if level <= ff::AV_LOG_ERROR {
        LOG_ERRORS.fetch_add(1, Ordering::Relaxed)
    } else {
        LOG_WARNINGS.fetch_add(1, Ordering::Relaxed)
    };
    if n < LOG_PRINT_LIMIT {
        eprintln!("[ffmpeg] {line}");
    }
}

fn install_log_callback() {
    LOG_INIT.call_once(|| unsafe { ff::av_log_set_callback(Some(log_cb)) });
}

// ---- 解码器 ----

/// 解码器自己的计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DecStats {
    /// 送进去的 AU
    pub sent: u64,
    /// 出来的帧
    pub frames: u64,
    /// 返回错误的次数
    pub errors: u64,
    /// （重）开解码器的次数
    pub opens: u32,
    /// 出帧时比刚送入的 AU 晚了几帧，取最大值；0 表示没有多缓帧
    pub max_lag: u64,
    /// 晚出（lag > 0）的帧数
    pub lagged: u64,
    /// 一次 decode 里出了不止一帧、只留最新一帧时丢掉的帧
    pub extra_dropped: u64,
    /// VideoToolbox 不支持这路码流、退回了软解
    pub sw_fallback: bool,
}

pub struct FfmpegDecoder {
    hw: HwKind,
    /// VideoToolbox 设备（软解时为空）
    device: *mut ff::AVBufferRef,
    ctx: *mut ff::AVCodecContext,
    pkt: *mut ff::AVPacket,
    codec: Option<Codec>,
    params: Vec<u8>,
    /// get_format 回调写它（地址要稳定，所以放在堆上）
    fallback: Box<AtomicBool>,
    /// 下一个 AU 的 pts（只用来核对出帧是不是刚送进去的那一帧）
    next_pts: i64,
    stats: DecStats,
}

// 裸指针只在持有者线程上用；整体移交给 decode 线程
unsafe impl Send for FfmpegDecoder {}

unsafe extern "C" fn get_format_vt(ctx: *mut ff::AVCodecContext, fmts: *const ff::AVPixelFormat) -> ff::AVPixelFormat {
    let mut p = fmts;
    while *p != ff::AVPixelFormat::AV_PIX_FMT_NONE {
        if *p == ff::AVPixelFormat::AV_PIX_FMT_VIDEOTOOLBOX {
            return *p;
        }
        p = p.add(1);
    }
    // 这路码流 VideoToolbox 不支持：退回软解（列表里第一个是软件格式）
    let flag = (*ctx).opaque as *const AtomicBool;
    if !flag.is_null() {
        (*flag).store(true, Ordering::Relaxed);
    }
    *fmts
}

impl FfmpegDecoder {
    /// LOW_DELAY、thread_count = 1
    pub fn new(hw: HwKind) -> Result<Self, DecErr> {
        install_log_callback();
        let mut device: *mut ff::AVBufferRef = ptr::null_mut();
        match hw {
            HwKind::VideoToolbox => {
                let r = unsafe {
                    ff::av_hwdevice_ctx_create(
                        &mut device,
                        ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
                        ptr::null(),
                        ptr::null_mut(),
                        0,
                    )
                };
                if r < 0 {
                    return Err(DecErr::Unavailable(format!("建 VideoToolbox 设备失败：{}", av_err_str(r))));
                }
            }
            HwKind::D3d11va => return Err(DecErr::Unavailable("D3D11VA 只在 Windows 上有".into())),
            HwKind::Software => {}
        }
        let pkt = unsafe { ff::av_packet_alloc() };
        if pkt.is_null() {
            unsafe { ff::av_buffer_unref(&mut device) };
            return Err(DecErr::Unavailable("av_packet_alloc 失败".into()));
        }
        Ok(Self {
            hw,
            device,
            ctx: ptr::null_mut(),
            pkt,
            codec: None,
            params: Vec::new(),
            fallback: Box::new(AtomicBool::new(false)),
            next_pts: 0,
            stats: DecStats::default(),
        })
    }

    pub fn hw(&self) -> HwKind {
        self.hw
    }

    pub fn stats(&self) -> DecStats {
        let mut s = self.stats;
        s.sw_fallback = self.fallback.load(Ordering::Relaxed);
        s
    }

    fn close(&mut self) {
        if !self.ctx.is_null() {
            // extradata 是 av_mallocz 分配的，avcodec_free_context 会一起释放
            unsafe { ff::avcodec_free_context(&mut self.ctx) };
            self.ctx = ptr::null_mut();
        }
        self.codec = None;
        self.params.clear();
    }

    fn open(&mut self, codec: Codec, params: &[u8]) -> Result<(), DecErr> {
        let id = match codec {
            Codec::Hevc => ff::AVCodecID::AV_CODEC_ID_HEVC,
            Codec::Avc => ff::AVCodecID::AV_CODEC_ID_H264,
        };
        unsafe {
            let dec = ff::avcodec_find_decoder(id);
            if dec.is_null() {
                return Err(DecErr::Unavailable(format!("FFmpeg 里没有 {codec:?} 解码器")));
            }
            let mut ctx = ff::avcodec_alloc_context3(dec);
            if ctx.is_null() {
                return Err(DecErr::Unavailable("avcodec_alloc_context3 失败".into()));
            }
            (*ctx).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*ctx).thread_count = 1;
            if !params.is_empty() {
                let pad = ff::AV_INPUT_BUFFER_PADDING_SIZE as usize;
                let ed = ff::av_mallocz(params.len() + pad) as *mut u8;
                if ed.is_null() {
                    ff::avcodec_free_context(&mut ctx);
                    return Err(DecErr::Unavailable("av_mallocz 失败".into()));
                }
                ptr::copy_nonoverlapping(params.as_ptr(), ed, params.len());
                (*ctx).extradata = ed;
                (*ctx).extradata_size = params.len() as c_int;
            }
            if !self.device.is_null() {
                (*ctx).hw_device_ctx = ff::av_buffer_ref(self.device);
                (*ctx).opaque = &*self.fallback as *const AtomicBool as *mut c_void;
                (*ctx).get_format = Some(get_format_vt);
            }
            let r = ff::avcodec_open2(ctx, dec, ptr::null_mut());
            if r < 0 {
                ff::avcodec_free_context(&mut ctx);
                return Err(DecErr::Ffmpeg(r));
            }
            self.ctx = ctx;
        }
        self.codec = Some(codec);
        self.params = params.to_vec();
        self.stats.opens += 1;
        Ok(())
    }

    fn wrap(&self, f: NonNull<ff::AVFrame>, now: Micros) -> DecodedFrame {
        let fr = unsafe { f.as_ref() };
        DecodedFrame {
            fid: 0,
            p_us: 0,
            decoded_us: now,
            color: ColorInfo {
                primaries: fr.color_primaries as i32,
                transfer: fr.color_trc as i32,
                matrix: fr.colorspace as i32,
                full_range: fr.color_range == ff::AVColorRange::AVCOL_RANGE_JPEG,
            },
            frame: AvFrameRef(f),
        }
    }
}

impl Drop for FfmpegDecoder {
    fn drop(&mut self) {
        self.close();
        unsafe {
            ff::av_packet_free(&mut self.pkt);
            ff::av_buffer_unref(&mut self.device);
        }
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn configure(&mut self, codec: Codec, params: &[u8]) -> Result<(), DecErr> {
        if !self.ctx.is_null() && self.codec == Some(codec) && self.params == params {
            return Ok(());
        }
        self.close();
        self.open(codec, params)
    }

    fn decode(&mut self, au: &[u8]) -> Result<Option<DecodedFrame>, DecErr> {
        if self.ctx.is_null() {
            return Err(DecErr::NotConfigured);
        }
        if au.is_empty() || au.len() > i32::MAX as usize {
            return Ok(None);
        }
        let eagain = ff::AVERROR(libc::EAGAIN);
        let pts = self.next_pts;
        self.next_pts += 1;
        self.stats.sent += 1;
        unsafe {
            // 不带引用计数的包：send_packet 内部会拷一份（带填充），所以直接指向调用方的缓冲
            (*self.pkt).data = au.as_ptr() as *mut u8;
            (*self.pkt).size = au.len() as c_int;
            (*self.pkt).pts = pts;
            (*self.pkt).dts = pts;
            let r = ff::avcodec_send_packet(self.ctx, self.pkt);
            (*self.pkt).data = ptr::null_mut();
            (*self.pkt).size = 0;
            if r < 0 && r != eagain {
                self.stats.errors += 1;
                return Err(DecErr::Ffmpeg(r));
            }
            let mut out: Option<DecodedFrame> = None;
            loop {
                let f = ff::av_frame_alloc();
                let Some(f) = NonNull::new(f) else {
                    return Err(DecErr::Unavailable("av_frame_alloc 失败".into()));
                };
                let r = ff::avcodec_receive_frame(self.ctx, f.as_ptr());
                if r < 0 {
                    let mut p = f.as_ptr();
                    ff::av_frame_free(&mut p);
                    if r == eagain || r == ff::AVERROR_EOF {
                        break;
                    }
                    self.stats.errors += 1;
                    return Err(DecErr::Ffmpeg(r));
                }
                let fpts = (*f.as_ptr()).best_effort_timestamp;
                let fpts = if fpts == ff::AV_NOPTS_VALUE { (*f.as_ptr()).pts } else { fpts };
                if fpts != ff::AV_NOPTS_VALUE && fpts < pts {
                    let lag = (pts - fpts) as u64;
                    self.stats.lagged += 1;
                    self.stats.max_lag = self.stats.max_lag.max(lag);
                }
                self.stats.frames += 1;
                if out.replace(self.wrap(f, crate::clock::now_us())).is_some() {
                    self.stats.extra_dropped += 1;
                }
            }
            Ok(out)
        }
    }

    fn flush(&mut self) {
        if !self.ctx.is_null() {
            unsafe { ff::avcodec_flush_buffers(self.ctx) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::Samples;
    use crate::play::{load_clip, Clip};

    #[test]
    fn classify_log_lines() {
        assert!(is_benign_missing_ref("[hevc @ 0x1] Could not find ref with POC 186"));
        assert!(!is_benign_missing_ref("[hevc @ 0x1] PPS id out of range: 0"));
    }

    #[test]
    fn not_configured_and_garbage_input() {
        let mut d = FfmpegDecoder::new(HwKind::Software).unwrap();
        assert!(matches!(d.decode(&[0, 0, 1, 0x40]), Err(DecErr::NotConfigured)));
        d.configure(Codec::Hevc, &[]).unwrap();
        // 没有参数集的乱码：返回错误或不出帧都行，不能崩
        let junk: Vec<u8> = (0..4096u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let _ = d.decode(&junk);
        let _ = d.decode(&[0, 0, 0, 1, 0x26, 1, 0xaf, 0x00]);
        assert!(d.decode(&[]).unwrap().is_none());
        d.flush();
    }

    /// 录像路径：环境变量 YW_TEST_H265（Annex-B，带参数集头），没设就跳过。
    /// 用 yw-media 的 AnnexBReader 切 AU，关键帧前面补上参数集（和网络路径一样）
    fn test_clip() -> Option<Clip> {
        let path = std::env::var_os("YW_TEST_H265")?;
        Some(load_clip(std::path::Path::new(&path), Codec::Hevc).unwrap_or_else(|e| panic!("读不到 YW_TEST_H265：{e}")))
    }

    /// 解码基准：逐帧送进 VideoToolbox，量首帧冷启动、送入到出帧的 P50/P95，
    /// 并确认每个 AU 都在同一次 decode 里出帧（没有多缓 1 帧）。
    /// `YW_TEST_H265=<录像> cargo test -p yw-client --release vt_decode_bench -- --nocapture`
    #[test]
    fn vt_decode_bench() {
        let Some(Clip { aus, params }) = test_clip() else {
            eprintln!("没设 YW_TEST_H265，跳过解码基准");
            return;
        };
        assert!(!aus.is_empty() && !params.is_empty(), "录像里找不到 AU 或参数集");
        let max = std::env::var("YW_TEST_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
        let log0 = log_stats();

        let t0 = crate::clock::now_us();
        let mut d = FfmpegDecoder::new(HwKind::VideoToolbox).expect("VideoToolbox 不可用");
        let t_dev = crate::clock::now_us();
        d.configure(Codec::Hevc, &params).unwrap();
        let t_open = crate::clock::now_us();
        let mut lat = Samples::default();
        let mut cold_us = None;
        let mut no_frame = 0u64;
        let mut errors = 0u64;
        let mut corrupt = 0u64;
        let n = aus.len().min(max);
        for au in aus.iter().take(n) {
            let s = crate::clock::now_us();
            match d.decode(au) {
                Ok(Some(f)) => {
                    let e = crate::clock::now_us();
                    if cold_us.is_none() {
                        cold_us = Some(e - t0);
                        assert!(f.is_videotoolbox(), "首帧不是 VideoToolbox 帧");
                        assert_eq!((f.width(), f.height()), (1920, 1080));
                    } else {
                        lat.push(e - s);
                    }
                    corrupt += f.is_corrupt() as u64;
                }
                Ok(None) => no_frame += 1,
                Err(_) => errors += 1,
            }
        }
        let total_ms = (crate::clock::now_us() - t0) as f64 / 1000.0;
        let st = d.stats();
        let log1 = log_stats();
        eprintln!(
            "解码基准：{n} 个 AU，出帧 {}，没出帧 {no_frame}，报错 {errors}，有损帧 {corrupt}，总耗时 {total_ms:.1} ms\n\
             首帧冷启动 {:.1} ms（建设备 {:.1} + 开解码器 {:.1} + 解第一帧 {:.1}）；之后送入到出帧 P50 {:.2} ms、P95 {:.2} ms、P99 {:.2} ms、最大 {:.2} ms\n\
             多缓帧检查：max_lag {}，晚出帧 {}，一次出多帧丢弃 {}；软解回退 {}；\
             FFmpeg 日志：找不到参考 {} 行（正常），其余错误 {} 行",
            st.frames,
            cold_us.unwrap_or(0) as f64 / 1000.0,
            (t_dev - t0) as f64 / 1000.0,
            (t_open - t_dev) as f64 / 1000.0,
            cold_us.map_or(0.0, |c| (t0 + c - t_open) as f64 / 1000.0),
            lat.pct(50.0) as f64 / 1000.0,
            lat.pct(95.0) as f64 / 1000.0,
            lat.pct(99.0) as f64 / 1000.0,
            lat.max() as f64 / 1000.0,
            st.max_lag,
            st.lagged,
            st.extra_dropped,
            st.sw_fallback,
            log1.missing_ref - log0.missing_ref,
            log1.errors - log0.errors,
        );
        assert!(!st.sw_fallback, "退回了软解");
        assert_eq!(st.max_lag, 0, "解码器多缓了帧");
        assert_eq!(st.extra_dropped, 0);
        assert_eq!(no_frame, 0, "有 AU 没当场出帧");
        assert_eq!(errors, 0);
    }

    /// 中途重开：参数集变了要重开，没变不重开；码流里自带参数集时不靠 extradata 也能解
    #[test]
    fn vt_reconfigure_mid_session() {
        let Some(Clip { aus, params }) = test_clip() else {
            eprintln!("没设 YW_TEST_H265，跳过");
            return;
        };
        let mut d = FfmpegDecoder::new(HwKind::VideoToolbox).unwrap();
        d.configure(Codec::Hevc, &params).unwrap();
        for au in aus.iter().take(10) {
            assert!(d.decode(au).unwrap().is_some());
        }
        d.configure(Codec::Hevc, &params).unwrap();
        assert_eq!(d.stats().opens, 1, "参数集没变不该重开");

        // 换一份「参数集」（这里用空的，模拟 cfg_ver 变了）：重开，从关键帧（自带参数集）起照样出帧
        d.configure(Codec::Hevc, &[]).unwrap();
        assert_eq!(d.stats().opens, 2);
        assert!(yw_media::nal::is_keyframe(&aus[0], Codec::Hevc));
        for au in aus.iter().take(5) {
            assert!(d.decode(au).unwrap().is_some());
        }
        // 再换回来，flush 之后从关键帧继续
        d.configure(Codec::Hevc, &params).unwrap();
        d.flush();
        assert_eq!(d.stats().opens, 3);
        assert!(d.decode(&aus[0]).unwrap().is_some());
        assert_eq!(d.stats().max_lag, 0);
    }
}
