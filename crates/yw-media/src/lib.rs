//! YWP 媒体层，不碰 I/O（sans-IO），归 C 组。
//!
//! - 网关侧：NAL 工具、整帧均分切片 + RS 校验（VideoTx）；
//! - 客户端侧：组帧、恢复、截止期、NEED（Assembler），呈现时间量、选帧（VsyncScheduler）、
//!   W3C 冻结统计（FreezeMeter）、音频抖动缓冲；
//! - 录像：`.ywf` 格式，以及按 AU 切分 `.h265` 录像的读取器。
//!
//! RS 只在这里用；ywp 只认 FEC 字的位布局。

// 第 0 步骨架，各组实现时去掉：函数体还是 todo!()，参数和私有字段暂时用不上；
// 输出参数 `&mut Vec` 还没 push，clippy 会误报 ptr_arg。
#![allow(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod audio_rx;
pub mod fec;
pub mod nal;
pub mod parity;
pub mod playout;
pub mod video_rx;
pub mod video_tx;
pub mod ywf;

pub use nal::Codec;
pub use parity::Mode;
