//! 帧源：按种子生成的合成帧，或 `.ywf` 录像回放。

use yw_media::ywf::YwfReader;

/// 合成帧的参数（C 组定）
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct GenCfg {
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// 关键帧间隔（帧数），0 表示只有第一帧
    pub gop: u32,
}

pub struct FrameGen {
    _priv: (),
}

impl FrameGen {
    pub fn synthetic(seed: u64, cfg: GenCfg) -> Self {
        todo!()
    }

    pub fn from_ywf(r: YwfReader) -> Self {
        todo!()
    }
}
