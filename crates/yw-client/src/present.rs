//! 呈现：SDL Metal 零拷贝（CVPixelBuffer → 纹理，色彩空间 BT709_LIMITED），NV12 上传兜底。
//! M2a 到达即呈现；M2c 起 Vsync 模式选帧用 yw-media 的 VsyncScheduler，冻结统计用 FreezeMeter。
//!
//! 只能在主线程上用（SDL 的要求）。色彩空间一律设成 `SDL_COLORSPACE_BT709_LIMITED`：
//! SDL 对 NV12 默认按 BT.601 处理，不设就会偏色。

use crate::decode::{AvFrameRef, DecodedFrame};
use ffmpeg_sys_next as ff;
use sdl3::sys::error::SDL_GetError;
use sdl3::sys::init::{SDL_Init, SDL_QuitSubSystem, SDL_INIT_VIDEO};
use sdl3::sys::pixels::{
    SDL_Colorspace, SDL_PixelFormat, SDL_COLORSPACE_BT709_LIMITED, SDL_PIXELFORMAT_IYUV, SDL_PIXELFORMAT_NV12,
    SDL_PIXELFORMAT_P010,
};
use sdl3::sys::properties::{SDL_CreateProperties, SDL_DestroyProperties, SDL_SetNumberProperty, SDL_SetPointerProperty};
use sdl3::sys::rect::SDL_FRect;
use sdl3::sys::render::*;
use sdl3::sys::video::{
    SDL_CreateWindow, SDL_DestroyWindow, SDL_Window, SDL_WINDOW_HIGH_PIXEL_DENSITY, SDL_WINDOW_METAL,
    SDL_WINDOW_RESIZABLE,
};
use std::ffi::{c_int, c_void, CStr, CString};
use std::ptr::{self, NonNull};
use ywp::time::Micros;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentMode {
    /// 到达即呈现（竞技）
    Immediate,
    Vsync,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PresentStat {
    pub fid: u64,
    pub presented_us: Micros,
    /// 上次呈现以来，还没呈现就被更新的帧顶掉的帧数
    pub skipped: u32,
}

pub trait Presenter {
    fn push(&mut self, f: DecodedFrame);
    fn render(&mut self, now: Micros, vsync_at: Option<Micros>) -> Option<PresentStat>;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetPixelFormatType(pb: *mut c_void) -> u32;
}

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// CVPixelBuffer 的像素格式 → SDL 格式（只认 4:2:0 双平面）
pub fn sdl_format_for_cv(cv: u32) -> Option<SDL_PixelFormat> {
    match cv {
        x if x == fourcc(b"420v") || x == fourcc(b"420f") => Some(SDL_PIXELFORMAT_NV12),
        x if x == fourcc(b"x420") || x == fourcc(b"xf20") => Some(SDL_PIXELFORMAT_P010),
        _ => None,
    }
}

/// 按比例缩放、居中（黑边）
pub fn letterbox(out_w: f32, out_h: f32, w: f32, h: f32) -> (f32, f32, f32, f32) {
    if w <= 0.0 || h <= 0.0 || out_w <= 0.0 || out_h <= 0.0 {
        return (0.0, 0.0, out_w.max(0.0), out_h.max(0.0));
    }
    let s = (out_w / w).min(out_h / h);
    let (dw, dh) = (w * s, h * s);
    ((out_w - dw) / 2.0, (out_h - dh) / 2.0, dw, dh)
}

fn sdl_err() -> String {
    unsafe { CStr::from_ptr(SDL_GetError()) }.to_string_lossy().into_owned()
}

/// 视频窗口：SDL 窗口 + Metal 渲染器（不等 vsync）。只在主线程上建和用
pub struct VideoWindow {
    window: *mut SDL_Window,
    renderer: *mut SDL_Renderer,
}

impl VideoWindow {
    pub fn open(title: &str, w: u32, h: u32) -> Result<Self, String> {
        let title = CString::new(title).map_err(|_| "窗口标题里有 NUL".to_string())?;
        unsafe {
            if !SDL_Init(SDL_INIT_VIDEO) {
                return Err(format!("SDL 视频初始化失败：{}", sdl_err()));
            }
            let flags = SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY | SDL_WINDOW_METAL;
            let window = SDL_CreateWindow(title.as_ptr(), w as c_int, h as c_int, flags);
            if window.is_null() {
                let e = sdl_err();
                SDL_QuitSubSystem(SDL_INIT_VIDEO);
                return Err(format!("开窗口失败：{e}"));
            }
            let renderer = SDL_CreateRenderer(window, c"metal".as_ptr());
            if renderer.is_null() {
                let e = sdl_err();
                SDL_DestroyWindow(window);
                SDL_QuitSubSystem(SDL_INIT_VIDEO);
                return Err(format!("建 Metal 渲染器失败：{e}"));
            }
            // 到达即呈现：不等 vsync
            SDL_SetRenderVSync(renderer, 0);
            Ok(Self { window, renderer })
        }
    }

    pub fn renderer_name(&self) -> String {
        let p = unsafe { SDL_GetRendererName(self.renderer) };
        if p.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }

    fn output_size(&self) -> (f32, f32) {
        let (mut w, mut h) = (0, 0);
        unsafe { SDL_GetCurrentRenderOutputSize(self.renderer, &mut w, &mut h) };
        (w as f32, h as f32)
    }
}

impl Drop for VideoWindow {
    fn drop(&mut self) {
        unsafe {
            SDL_DestroyRenderer(self.renderer);
            SDL_DestroyWindow(self.window);
            SDL_QuitSubSystem(SDL_INIT_VIDEO);
        }
    }
}

/// 正在显示的一帧：纹理和 AVFrame 一起保留，到下一帧呈现之后再释放
struct Shown {
    tex: *mut SDL_Texture,
    /// 纹理是不是这一帧独有（零拷贝时是；上传时复用同一张流式纹理）
    owns_tex: bool,
    size: (f32, f32),
    _frame: DecodedFrame,
}

impl Drop for Shown {
    fn drop(&mut self) {
        if self.owns_tex && !self.tex.is_null() {
            unsafe { SDL_DestroyTexture(self.tex) };
        }
    }
}

/// 上传用的流式纹理（格式、尺寸变了就重建）
struct UploadTex {
    tex: *mut SDL_Texture,
    fmt: SDL_PixelFormat,
    w: u32,
    h: u32,
}

impl Drop for UploadTex {
    fn drop(&mut self) {
        unsafe { SDL_DestroyTexture(self.tex) };
    }
}

/// 呈现计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PresenterStats {
    pub presented: u64,
    pub zero_copy: u64,
    pub uploaded: u64,
    /// 还没呈现就被顶掉的帧（累计）
    pub skipped: u64,
    /// 建纹理或上传失败的帧
    pub tex_errors: u64,
}

/// 两种呈现器共用的部分
struct Canvas {
    win: VideoWindow,
    zero_copy: bool,
    colorspace: SDL_Colorspace,
    pending: Option<DecodedFrame>,
    skipped_since: u32,
    shown: Option<Shown>,
    upload: Option<UploadTex>,
    overlay: Vec<String>,
    redraw: bool,
    warned_color: bool,
    stats: PresenterStats,
}

unsafe fn create_texture(
    r: *mut SDL_Renderer,
    fmt: SDL_PixelFormat,
    access: SDL_TextureAccess,
    w: u32,
    h: u32,
    cs: SDL_Colorspace,
    pixbuf: Option<NonNull<c_void>>,
) -> Result<*mut SDL_Texture, String> {
    let props = SDL_CreateProperties();
    if props == 0 {
        return Err(format!("SDL_CreateProperties 失败：{}", sdl_err()));
    }
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, cs.0 as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, fmt.0 as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, access.0 as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, w as i64);
    SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, h as i64);
    if let Some(pb) = pixbuf {
        SDL_SetPointerProperty(props, SDL_PROP_TEXTURE_CREATE_METAL_PIXELBUFFER_POINTER, pb.as_ptr());
    }
    let tex = SDL_CreateTextureWithProperties(r, props);
    SDL_DestroyProperties(props);
    if tex.is_null() {
        return Err(format!("建纹理失败：{}", sdl_err()));
    }
    Ok(tex)
}

impl Canvas {
    fn new(win: VideoWindow, zero_copy: bool) -> Self {
        Self {
            win,
            zero_copy,
            colorspace: SDL_COLORSPACE_BT709_LIMITED,
            pending: None,
            skipped_since: 0,
            shown: None,
            upload: None,
            overlay: Vec::new(),
            redraw: false,
            warned_color: false,
            stats: PresenterStats::default(),
        }
    }

    fn push(&mut self, f: DecodedFrame) {
        // 到达即呈现：信箱里只留最新一帧，旧的记跳显
        if self.pending.replace(f).is_some() {
            self.skipped_since += 1;
            self.stats.skipped += 1;
        }
    }

    fn check_color(&mut self, f: &DecodedFrame) {
        // 固定按 BT709_LIMITED 呈现；码流标的不一样时只提示一次
        let c = f.color;
        let bt709 = ff::AVColorSpace::AVCOL_SPC_BT709 as i32;
        let unspecified = ff::AVColorSpace::AVCOL_SPC_UNSPECIFIED as i32;
        if !self.warned_color && (c.full_range || (c.matrix != bt709 && c.matrix != unspecified)) {
            self.warned_color = true;
            eprintln!("提示：码流标的色彩是 matrix={} full_range={}，呈现仍按 BT.709 limited", c.matrix, c.full_range);
        }
    }

    /// 零拷贝：直接用 CVPixelBuffer 建纹理
    fn zero_copy_tex(&mut self, f: &DecodedFrame) -> Result<*mut SDL_Texture, String> {
        let pb = f.cv_pixel_buffer().ok_or("不是 VideoToolbox 帧")?;
        let cv = unsafe { CVPixelBufferGetPixelFormatType(pb.as_ptr()) };
        let fmt = sdl_format_for_cv(cv).ok_or_else(|| format!("不支持的 CVPixelBuffer 格式 {cv:#010x}"))?;
        unsafe {
            create_texture(
                self.win.renderer,
                fmt,
                SDL_TEXTUREACCESS_STATIC,
                f.width(),
                f.height(),
                self.colorspace,
                Some(pb),
            )
        }
    }

    /// 上传：硬件帧先拷回内存（NV12），软解帧（YUV420P / NV12）直接上传
    fn upload_tex(&mut self, f: &DecodedFrame) -> Result<*mut SDL_Texture, String> {
        let transferred;
        let src: *const ff::AVFrame = if f.is_videotoolbox() {
            let sw = NonNull::new(unsafe { ff::av_frame_alloc() }).ok_or("av_frame_alloc 失败")?;
            let sw = AvFrameRef::from_raw(sw);
            let r = unsafe { ff::av_hwframe_transfer_data(sw.as_ptr(), f.av_frame().as_ptr(), 0) };
            if r < 0 {
                return Err(format!("硬件帧拷回失败：{}", crate::decode::av_err_str(r)));
            }
            transferred = sw;
            transferred.as_ptr()
        } else {
            f.av_frame().as_ptr()
        };
        let fr = unsafe { &*src };
        let (w, h) = (fr.width as u32, fr.height as u32);
        let fmt = if fr.format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
            SDL_PIXELFORMAT_NV12
        } else if fr.format == ff::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int {
            SDL_PIXELFORMAT_IYUV
        } else {
            return Err(format!("上传不支持像素格式 {}", fr.format));
        };
        let reuse = self.upload.as_ref().is_some_and(|u| u.fmt == fmt && u.w == w && u.h == h);
        if !reuse {
            // 旧纹理可能还被正在显示的帧引用：先放掉它
            self.shown = None;
            let tex = unsafe {
                create_texture(self.win.renderer, fmt, SDL_TEXTUREACCESS_STREAMING, w, h, self.colorspace, None)?
            };
            self.upload = Some(UploadTex { tex, fmt, w, h });
        }
        let tex = self.upload.as_ref().map(|u| u.tex).unwrap_or(ptr::null_mut());
        let ok = unsafe {
            if fmt == SDL_PIXELFORMAT_NV12 {
                SDL_UpdateNVTexture(tex, ptr::null(), fr.data[0], fr.linesize[0], fr.data[1], fr.linesize[1])
            } else {
                SDL_UpdateYUVTexture(
                    tex,
                    ptr::null(),
                    fr.data[0],
                    fr.linesize[0],
                    fr.data[1],
                    fr.linesize[1],
                    fr.data[2],
                    fr.linesize[2],
                )
            }
        };
        if !ok {
            return Err(format!("上传纹理失败：{}", sdl_err()));
        }
        Ok(tex)
    }

    fn draw(&self) {
        let r = self.win.renderer;
        let (ow, oh) = self.win.output_size();
        unsafe {
            SDL_SetRenderDrawColor(r, 0, 0, 0, 255);
            SDL_RenderClear(r);
            if let Some(s) = &self.shown {
                let (x, y, w, h) = letterbox(ow, oh, s.size.0, s.size.1);
                let dst = SDL_FRect { x, y, w, h };
                SDL_RenderTexture(r, s.tex, ptr::null(), &dst);
            }
            if !self.overlay.is_empty() {
                // 调试字体 8×8 像素；按输出高度放大，1080 p 左右放大 2 倍
                let scale = (oh / 540.0).max(1.0).floor();
                SDL_SetRenderScale(r, scale, scale);
                for (i, line) in self.overlay.iter().enumerate() {
                    let Ok(c) = CString::new(line.as_str()) else { continue };
                    let y = 6.0 + i as f32 * 10.0;
                    SDL_SetRenderDrawColor(r, 0, 0, 0, 255);
                    SDL_RenderDebugText(r, 7.0, y + 1.0, c.as_ptr());
                    SDL_SetRenderDrawColor(r, 255, 255, 255, 255);
                    SDL_RenderDebugText(r, 6.0, y, c.as_ptr());
                }
                SDL_SetRenderScale(r, 1.0, 1.0);
            }
            SDL_RenderPresent(r);
        }
    }

    fn render(&mut self) -> Option<PresentStat> {
        let Some(f) = self.pending.take() else {
            if std::mem::take(&mut self.redraw) {
                self.draw();
            }
            return None;
        };
        self.check_color(&f);
        let made = if self.zero_copy && f.is_videotoolbox() {
            self.zero_copy_tex(&f).map(|t| (t, true))
        } else {
            self.upload_tex(&f).map(|t| (t, false))
        };
        let (tex, owns_tex) = match made {
            Ok(v) => v,
            Err(e) => {
                self.stats.tex_errors += 1;
                if self.stats.tex_errors <= 5 {
                    eprintln!("呈现失败：{e}");
                }
                return None;
            }
        };
        if owns_tex {
            self.stats.zero_copy += 1;
        } else {
            self.stats.uploaded += 1;
        }
        let fid = f.fid;
        let size = (f.width() as f32, f.height() as f32);
        let old = self.shown.replace(Shown { tex, owns_tex, size, _frame: f });
        self.draw();
        // 上一帧的纹理和 AVFrame 在新帧呈现之后才释放
        drop(old);
        self.stats.presented += 1;
        let skipped = std::mem::take(&mut self.skipped_since);
        self.redraw = false;
        Some(PresentStat { fid, presented_us: crate::clock::now_us(), skipped })
    }
}

/// CVPixelBuffer 零拷贝；AVFrame 和纹理保留到下一帧呈现之后再释放。
/// 不是 VideoToolbox 帧（软解回退）时自动改走上传
pub struct SdlMetalPresenter {
    c: Canvas,
}

impl SdlMetalPresenter {
    pub fn new(win: VideoWindow) -> Self {
        Self { c: Canvas::new(win, true) }
    }
}

/// NV12 上传，各平台通用的兜底
pub struct SdlUploadPresenter {
    c: Canvas,
}

impl SdlUploadPresenter {
    pub fn new(win: VideoWindow) -> Self {
        Self { c: Canvas::new(win, false) }
    }
}

/// 两种呈现器都有的附加操作
pub trait PresenterExt {
    /// 浮层文字（ASCII），下次呈现时画上
    fn set_overlay(&mut self, lines: Vec<String>);
    /// 窗口尺寸变了、被遮挡后重新露出：下一次 render 即使没有新帧也重画
    fn request_redraw(&mut self);
    fn stats(&self) -> PresenterStats;
    fn renderer_name(&self) -> String;
}

macro_rules! impl_presenter {
    ($t:ty) => {
        impl Presenter for $t {
            fn push(&mut self, f: DecodedFrame) {
                self.c.push(f)
            }

            /// M2a 只有到达即呈现：`vsync_at` 不用
            fn render(&mut self, _now: Micros, _vsync_at: Option<Micros>) -> Option<PresentStat> {
                self.c.render()
            }
        }

        impl PresenterExt for $t {
            fn set_overlay(&mut self, lines: Vec<String>) {
                self.c.overlay = lines;
            }

            fn request_redraw(&mut self) {
                self.c.redraw = true;
            }

            fn stats(&self) -> PresenterStats {
                self.c.stats
            }

            fn renderer_name(&self) -> String {
                self.c.win.renderer_name()
            }
        }
    };
}

impl_presenter!(SdlMetalPresenter);
impl_presenter!(SdlUploadPresenter);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cv_formats() {
        assert_eq!(sdl_format_for_cv(fourcc(b"420v")), Some(SDL_PIXELFORMAT_NV12));
        assert_eq!(sdl_format_for_cv(fourcc(b"420f")), Some(SDL_PIXELFORMAT_NV12));
        assert_eq!(sdl_format_for_cv(fourcc(b"x420")), Some(SDL_PIXELFORMAT_P010));
        assert_eq!(sdl_format_for_cv(fourcc(b"BGRA")), None);
        assert_eq!(fourcc(b"420v"), 0x3432_3076);
    }

    #[test]
    fn letterbox_keeps_aspect() {
        assert_eq!(letterbox(1920.0, 1080.0, 1920.0, 1080.0), (0.0, 0.0, 1920.0, 1080.0));
        // 4:3 窗口里放 16:9：上下黑边
        let (x, y, w, h) = letterbox(1600.0, 1200.0, 1920.0, 1080.0);
        assert_eq!((x, w), (0.0, 1600.0));
        assert!((h - 900.0).abs() < 0.01 && (y - 150.0).abs() < 0.01);
        // 竖长窗口：左右黑边
        let (x, _, w, h) = letterbox(1000.0, 400.0, 1920.0, 1080.0);
        assert!((h - 400.0).abs() < 0.01 && (w - 711.11).abs() < 0.1 && x > 0.0);
        assert_eq!(letterbox(100.0, 100.0, 0.0, 0.0), (0.0, 0.0, 100.0, 100.0));
    }
}
