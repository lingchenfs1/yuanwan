//! 音频输出（M2b）：SDL get-callback 拉模型；SDL_AUDIO_DEVICE_SAMPLE_FRAMES=240；
//! 回调线程独占 AudioJitter，先 try_recv 收完新到的包，再取一帧 → Opus 解码或 PLC；
//! 用 SDL_SetAudioStreamFrequencyRatio 调速。

use std::sync::mpsc::Receiver;
use ywp::msg::AudioConfig;
use yw_media::audio_rx::AudioPacket;

pub struct AudioOut {
    _priv: (),
}

impl AudioOut {
    pub fn open(rx: Receiver<AudioPacket>, cfg: &AudioConfig, target_us: u32) -> Result<Self, String> {
        todo!()
    }
}
