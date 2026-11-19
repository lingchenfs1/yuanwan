//! net 线程（QoS USER_INTERACTIVE；mio::Poll：UDP 绑配置里的网卡 + Waker）。
//! 独占 ywp::ClientConn 和 Assembler；收包后立即打单调时间戳。
//! 输出：DecodeItem（有界 64）→ decode 线程；音频包 → SDL 音频回调；FeedbackState → input 线程；UiEvent → 主线程。

use crate::config::GatewayProfile;
use smallvec::SmallVec;
use std::io;
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use ywp::crypto::StaticKeypair;
use ywp::msg::{ControlMsg, SessionStatus};
use ywp::time::Micros;
use ywp::wire::{FeedbackState, HsErr, InputState};
use yw_media::audio_rx::AudioPacket;
use yw_media::video_rx::DecodeItem;

/// 其他线程 → net 线程
#[derive(Debug)]
pub enum NetIn {
    Input { now: Micros, state: InputState, discrete: SmallVec<[(u8, u8); 8]> },
    Control(ControlMsg),
    /// 解码出错的 fid
    DecoderError(u64),
    Disconnect,
}

/// net 线程 → 主线程（经 SDL_PushEvent）
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum UiEvent {
    Connecting,
    Connected,
    Rejected(HsErr),
    Closed { code: u16 },
    Status(SessionStatus),
    Rtt(Micros),
    /// 1 s 没有任何包
    NetworkDown,
    /// 媒体照常到达，但 1 s 没有 UP_REPORT 或 PONG
    UplinkDown,
}

/// 主线程提供的投递函数（里面调 SDL_PushEvent）
pub type UiSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

pub struct NetHandle {
    pub tx: Sender<NetIn>,
    pub waker: Arc<mio::Waker>,
    pub join: JoinHandle<()>,
}

pub fn spawn_net(
    p: &GatewayProfile,
    keys: StaticKeypair,
    dec_tx: SyncSender<DecodeItem>,
    audio_tx: SyncSender<AudioPacket>,
    fb_tx: Sender<FeedbackState>,
    ui: UiSink,
) -> io::Result<NetHandle> {
    todo!()
}
