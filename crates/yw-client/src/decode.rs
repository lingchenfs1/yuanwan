//! 解码（只在 ffmpeg-sys-next 上包一层薄封装，不用 ffmpeg-next）。
//! FFmpeg + VideoToolbox，LOW_DELAY、thread_count = 1，同步 send/receive。

use ffmpeg_sys_next as ff;
use std::ptr::NonNull;
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

pub struct DecodedFrame {
    pub fid: u64,
    pub p_us: Micros,
    pub decoded_us: Micros,
    pub color: ColorInfo,
    frame: AvFrameRef,
}

pub trait VideoDecoder: Send {
    fn configure(&mut self, codec: Codec, params: &[u8]) -> Result<(), DecErr>;
    fn decode(&mut self, au: &[u8]) -> Result<Option<DecodedFrame>, DecErr>;
    fn flush(&mut self);
}

pub struct FfmpegDecoder {
    _priv: (),
}

impl FfmpegDecoder {
    /// LOW_DELAY、thread_count = 1
    pub fn new(hw: HwKind) -> Result<Self, DecErr> {
        todo!()
    }
}

impl VideoDecoder for FfmpegDecoder {
    fn configure(&mut self, codec: Codec, params: &[u8]) -> Result<(), DecErr> {
        todo!()
    }

    fn decode(&mut self, au: &[u8]) -> Result<Option<DecodedFrame>, DecErr> {
        todo!()
    }

    fn flush(&mut self) {
        todo!()
    }
}
