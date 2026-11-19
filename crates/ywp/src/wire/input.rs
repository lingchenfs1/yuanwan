//! INPUT 块（0x38，C→G，规格 §5.5.3）：
//! in_seq u16 | 标志 u8（A、Z、n_ev）| STATE 40 B | n_ev × EVENT 6 B（最新的在前）。
//! 量化公式、按键位表、EVENT code 表见 consts 和规格 §5.5.3；这里只管字节布局。

use super::{EncodeChunk, Reader, Writer};
use crate::consts::{
    chunk_len, chunk_ty, INPUT_EVENT_LEN, INPUT_FLAG_CONNECTED, INPUT_FLAG_ZERO, INPUT_MAX_EVENTS, INPUT_NEV_MASK,
    INPUT_STATE_LEN, TOUCH_COORD_BITS, TOUCH_COORD_MAX,
};
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

    /// 4 B：id i8 | (x << 12 | y) 的 3 B 大端。x、y 超过 12 位的部分被截掉
    fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        let v = ((self.x & TOUCH_COORD_MAX) as u32) << TOUCH_COORD_BITS | (self.y & TOUCH_COORD_MAX) as u32;
        w.put_u8(self.id as u8)?;
        w.put_slice(&v.to_be_bytes()[1..])
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        let id = r.u8()? as i8;
        let b = r.array::<3>()?;
        let v = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        Ok(Self { id, x: (v >> TOUCH_COORD_BITS) as u16 & TOUCH_COORD_MAX, y: v as u16 & TOUCH_COORD_MAX })
    }
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

impl Default for InputState {
    /// 中性状态：没按键、摇杆居中、没有触摸、运动数据无效
    fn default() -> Self {
        Self {
            buttons: 0,
            l2: 0,
            r2: 0,
            sticks: [0; 4],
            touch: [Touch::NONE; 2],
            gyro: [0; 3],
            accel: [0; 3],
            orient: 0,
            flags: 0,
        }
    }
}

impl InputState {
    /// 写 40 B
    pub fn encode(&self, w: &mut Writer<'_>) -> Result<(), WireError> {
        if w.remaining() < INPUT_STATE_LEN {
            return Err(WireError::NoRoom);
        }
        w.put_u32(self.buttons)?;
        w.put_u8(self.l2)?;
        w.put_u8(self.r2)?;
        for s in self.sticks {
            w.put_u16(s as u16)?;
        }
        for t in &self.touch {
            t.encode(w)?;
        }
        for g in self.gyro {
            w.put_u16(g)?;
        }
        for a in self.accel {
            w.put_u16(a)?;
        }
        w.put_u32(self.orient)?;
        w.put_u16(self.flags)
    }

    /// 读 40 B
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, WireError> {
        if r.remaining() < INPUT_STATE_LEN {
            return Err(WireError::Truncated);
        }
        let buttons = r.u32()?;
        let l2 = r.u8()?;
        let r2 = r.u8()?;
        let mut sticks = [0i16; 4];
        for s in sticks.iter_mut() {
            *s = r.u16()? as i16;
        }
        let touch = [Touch::decode(r)?, Touch::decode(r)?];
        let mut gyro = [0u16; 3];
        for g in gyro.iter_mut() {
            *g = r.u16()?;
        }
        let mut accel = [0u16; 3];
        for a in accel.iter_mut() {
            *a = r.u16()?;
        }
        let orient = r.u32()?;
        let flags = r.u16()?;
        Ok(Self { buttons, l2, r2, sticks, touch, gyro, accel, orient, flags })
    }
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
        if self.rest.len() < INPUT_EVENT_LEN {
            self.rest = &[];
            return None;
        }
        let (e, rest) = self.rest.split_at(INPUT_EVENT_LEN);
        self.rest = rest;
        Some(InputEvent {
            ev_seq: u16::from_be_bytes([e[0], e[1]]),
            code: e[2],
            value: e[3],
            dt_100us: u16::from_be_bytes([e[4], e[5]]),
        })
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
    /// 解析 INPUT 块的内容（INPUT_FIXED + INPUT_EVENT_LEN × n_ev）。n_ev > 8 计 InputTooManyEvents
    pub fn parse(content: &'a [u8]) -> Result<Self, DropReason> {
        if content.len() < chunk_len::INPUT_FIXED {
            return Err(DropReason::ShortChunk);
        }
        let mut r = Reader::new(content);
        let in_seq = r.u16().map_err(|_| DropReason::ShortChunk)?;
        let flags = r.u8().map_err(|_| DropReason::ShortChunk)?;
        let n_ev = flags & INPUT_NEV_MASK;
        if n_ev as usize > INPUT_MAX_EVENTS {
            return Err(DropReason::InputTooManyEvents);
        }
        let state = InputState::decode(&mut r).map_err(|_| DropReason::ShortChunk)?;
        let raw_events = r.bytes(INPUT_EVENT_LEN * n_ev as usize).map_err(|_| DropReason::ShortChunk)?;
        Ok(Self {
            in_seq,
            connected: flags & INPUT_FLAG_CONNECTED != 0,
            zero: flags & INPUT_FLAG_ZERO != 0,
            state,
            n_ev,
            raw_events,
        })
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
        if self.n_ev as usize > INPUT_MAX_EVENTS {
            return Err(WireError::Malformed("INPUT 事件超过 8 个"));
        }
        w.put_u16(self.in_seq)?;
        let flags = if self.connected { INPUT_FLAG_CONNECTED } else { 0 }
            | if self.zero { INPUT_FLAG_ZERO } else { 0 }
            | self.n_ev;
        w.put_u8(flags)?;
        self.state.encode(w)?;
        for e in &self.events[..self.n_ev as usize] {
            w.put_u16(e.ev_seq)?;
            w.put_u8(e.code)?;
            w.put_u8(e.value)?;
            w.put_u16(e.dt_100us)?;
        }
        Ok(())
    }
}
