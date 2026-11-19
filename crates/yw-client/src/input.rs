//! 手柄线程（M2b，1 kHz，USER_INTERACTIVE）：SDL_UpdateGamepads → 读按键、摇杆、扳机、触摸、传感器，
//! 有变化才交给 net 线程；执行震动、灯条、扳机效果。运动传感器的 M 位默认为 0。
//! 映射和量化按规格 §5.5.3、§11（自己写的描述，不移植 chiaki）。

use crate::net::NetIn;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use sdl3::gamepad::Gamepad;
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;
use ywp::time::Micros;
use ywp::wire::{FeedbackState, InputState};

/// 一次采样的原始值（SDL 的单位）。字段由 E 组定
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct GamepadSnapshot {
    pub at: Micros,
    /// SDL_GamepadButton 的位图
    pub buttons: u32,
    /// SDL_GamepadAxis：lx、ly、rx、ry、l2、r2
    pub axes: [i16; 6],
    /// 两个触摸槽：(手指 id, x, y)，x、y 为 0~1
    pub touch: [Option<(u8, f32, f32)>; 2],
    /// rad/s
    pub gyro: Option<[f32; 3]>,
    /// m/s²
    pub accel: Option<[f32; 3]>,
}

/// net_tx 和 waker 从 NetHandle 克隆而来：NetHandle 本身（含 join）留给主线程
pub fn spawn_gamepad(net_tx: Sender<NetIn>, waker: Arc<mio::Waker>, fb_rx: Receiver<FeedbackState>) -> JoinHandle<()> {
    todo!()
}

/// 映射到 YWP 的按键位表并量化（规格 §5.5.3）
pub fn map_sdl(g: &GamepadSnapshot) -> InputState {
    todo!()
}

/// 震动用 SDL_RumbleGamepad（每份 FEEDBACK 重新下发，持续 1000 ms）、灯条、玩家灯、扳机效果原始报文
pub fn apply_feedback(g: &mut Gamepad, s: &FeedbackState) {
    todo!()
}
