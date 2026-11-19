//! Ingress（M2a）：参数集 → VIDEO_CONFIG、fid 分配、WaitKey。
//!
//! - fid 只分配给真正转发的帧，客户端看到的 fid 连续；
//! - 进入 WaitKey：丢 P 帧、经 IdrGovernor 请求 IDR、发 KEY_PENDING，从下一个关键帧恢复；
//! - 有客户端时 300 ms 没有新帧：请求 IDR 并发 KEY_PENDING；
//! - recovered 帧怎么处理由 RecoveredPolicy 定。T31、T41（2026-11-19）的结论：PS5 码流每帧单 slice、首 NAL 是 slice，
//!   chiaki 的参考核对覆盖所有帧，修补帧能正常解码，所以默认 Forward（照常转发并置 W 位）。

use crate::idr::IdrReason;
use ywp::msg::VideoConfig;
use ywp::time::Micros;
use yw_media::video_tx::FrameIn;

/// 为什么进 WaitKey
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitWhy {
    /// MediaMsg 队列溢出（seq 跳号）
    Overflow,
    /// 帧过大（k > 1024）
    TooLarge,
    /// 新客户端在串流中途接入
    NewClient,
    /// 从 Held 恢复
    HeldResume,
    /// 有客户端时 300 ms 没有新帧
    NoFrames,
    /// 只在 T31 定为「recovered 帧不转发」时用
    Recovered,
}

/// chiaki 改写过参考的帧（recovered）怎么处理。默认 Forward（T31、T41 的结论）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveredPolicy {
    #[default]
    /// 照常转发并置 W 位
    Forward,
    /// 不转发，进 WaitKey
    WaitKey,
    /// frames_lost > 0 起到下一个关键帧前都进 WaitKey（管首 NAL 不是 slice、chiaki 没核对的帧）
    LostUntilKey,
}

/// 一帧整帧输入（来自 MediaMsg::Video）
#[derive(Clone, Copy, Debug)]
pub struct VideoIn<'a> {
    /// MediaMsg 的递增序号，跳号说明队列溢出过
    pub seq: u32,
    /// chiaki 交帧回调的时刻：M2a~M2d 的 t0（规格 §9.1），age 从这里算起
    pub rx_us: Micros,
    /// chiaki 报告的、本帧之前丢掉的帧数（LostUntilKey 用）
    pub frames_lost: i32,
    /// chiaki 改写过本帧的参考
    pub recovered: bool,
    pub data: &'a [u8],
}

#[derive(Debug)]
pub enum IngressOut<'a> {
    Frame(FrameIn<'a>),
    Config(VideoConfig),
    /// fid 填「下一个要分配的 fid」
    KeyPending { fid: u16 },
    RequestIdr(IdrReason),
}

#[derive(Debug)]
pub struct Ingress {
    _priv: (),
}

impl Ingress {
    pub fn new(policy: RecoveredPolicy) -> Self {
        todo!()
    }

    /// cfg_ver += 1；fid 继续递增
    pub fn on_epoch(&mut self, epoch: u32) {
        todo!()
    }

    pub fn on_params(&mut self, data: &[u8], out: &mut Vec<IngressOut<'_>>) {
        todo!()
    }

    pub fn on_frame<'a>(&mut self, now: Micros, frame: VideoIn<'a>, out: &mut Vec<IngressOut<'a>>) {
        todo!()
    }

    /// 有没有活跃客户端：「300 ms 无帧」只在有客户端时生效；新客户端接入时由调用方再调 wait_key(NewClient)
    pub fn set_client_present(&mut self, present: bool) {
        todo!()
    }

    /// 丢 P 帧、请求 IDR、发 KEY_PENDING，等关键帧
    pub fn wait_key(&mut self, why: WaitWhy) {
        todo!()
    }

    /// 有客户端时 300 ms 无帧 → IDR + KEY_PENDING
    pub fn on_tick(&mut self, now: Micros, out: &mut Vec<IngressOut<'_>>) {
        todo!()
    }
}
