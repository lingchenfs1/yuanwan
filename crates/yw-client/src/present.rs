//! 呈现：SDL Metal 零拷贝（CVPixelBuffer → 纹理，色彩空间 BT709_LIMITED），NV12 上传兜底。
//! M2a 到达即呈现；M2c 起 Vsync 模式选帧用 yw-media 的 VsyncScheduler，冻结统计用 FreezeMeter。

use crate::decode::DecodedFrame;
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
    pub skipped: u32,
}

pub trait Presenter {
    fn push(&mut self, f: DecodedFrame);
    fn render(&mut self, now: Micros, vsync_at: Option<Micros>) -> Option<PresentStat>;
}

/// CVPixelBuffer 零拷贝；AVFrame 和纹理保留到下一帧呈现之后再释放
pub struct SdlMetalPresenter {
    _priv: (),
}

impl Presenter for SdlMetalPresenter {
    fn push(&mut self, f: DecodedFrame) {
        todo!()
    }

    fn render(&mut self, now: Micros, vsync_at: Option<Micros>) -> Option<PresentStat> {
        todo!()
    }
}

/// NV12 上传，各平台通用的兜底
pub struct SdlUploadPresenter {
    _priv: (),
}

impl Presenter for SdlUploadPresenter {
    fn push(&mut self, f: DecodedFrame) {
        todo!()
    }

    fn render(&mut self, now: Micros, vsync_at: Option<Micros>) -> Option<PresentStat> {
        todo!()
    }
}
