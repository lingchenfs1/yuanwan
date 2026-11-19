//! INPUT 块（0x38，C→G，规格 §5.5.3、附录 A.4）：
//! in_seq u16 | 标志 u8（A、Z、n_ev）| STATE 40 B | n_ev × EVENT 6 B（最新的在前）。
//! 量化公式、按键位表、EVENT code 表见 consts 和附录 A.4，按规格里我们自己写的描述实现。

use super::{EncodeChunk, Writer};
use crate::consts::{chunk_len, chunk_ty};
use crate::error::{DropReason, WireError};

/// 一个触摸点：id = −1 表示没有触摸；x、y 是触摸板原始坐标，各 12 位
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    pub id: i8,
    pub x: u16,
    pub y: u16,
}

impl Touch {
    pub const NONE: Touch = Touch { id: crate::consts::TOUCH_NONE, x: 0, y: 0 };
}

/// STATE（40 B）：完整手柄状态，已量化
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputState {
    /// 位 0~15 见 consts::button，16~31 为 0
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    /// lx、ly、rx、ry
    pub sticks: [i16; 4],
    pub touch: [Touch; 2],
    /// 陀螺仪 x/y/z，±30 rad/s 线性量化
    pub gyro: [u16; 3],
    /// 加速度 x/y/z，±5 g 线性量化
    pub accel: [u16; 3],
    /// 姿态：v1 客户端填 0，网关忽略
    pub orient: u32,
    /// 位 0 M（运动数据有效）、位 1 T（触摸数据有效）
    pub flags: u16,
}

/// EVENT（6 B）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputEvent {
    pub ev_seq: u16,
    pub code: u8,
    pub value: u8,
    /// 事件发生时刻比本包 SendTS 早多少，单位 100 µs，封顶 0xFFFF
    pub dt_100us: u16,
}

/// INPUT 块的只读视图
#[derive(Clone, Copy, Debug)]
pub struct InputChunk<'a> {
    /// 每个输入包加 1，副本也加 1
    pub in_seq: u16,
    /// A：手柄已连接
    pub connected: bool,
    /// Z：主动归零（用户拔掉手柄）
    pub zero: bool,
    pub state: InputState,
    n_ev: u8,
    raw_events: &'a [u8],
}

impl<'a> InputChunk<'a> {
    pub fn n_events(&self) -> usize {
        self.n_ev as usize
    }

    /// 最近 n_ev 个事件，最新的在前
    pub fn events(&self) -> InputEvents<'a> {
        InputEvents { rest: self.raw_events }
    }
}

#[derive(Clone, Debug)]
pub struct InputEvents<'a> {
    rest: &'a [u8],
}

impl Iterator for InputEvents<'_> {
    type Item = InputEvent;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// 发送端组好的 INPUT 块（InputSender::poll 的输出）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputChunkOwned {
    pub in_seq: u16,
    pub connected: bool,
    pub zero: bool,
    pub state: InputState,
    /// events[..n_ev] 有效，最新的在前
    pub n_ev: u8,
    pub events: [InputEvent; crate::consts::INPUT_MAX_EVENTS],
}

impl<'a> InputChunk<'a> {
    /// 解析 INPUT 块的内容（INPUT_FIXED + INPUT_EVENT_LEN × n_ev）
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        todo!()
    }
}

impl EncodeChunk for InputChunkOwned {
    fn ty(&self) -> u8 {
        chunk_ty::INPUT
    }
    fn content_len(&self) -> usize {
        chunk_len::INPUT_FIXED + crate::consts::INPUT_EVENT_LEN * self.n_ev as usize
    }
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        todo!()
    }
}
