//! 桌面客户端（E 组）。协议和媒体逻辑都在 ywp、yw-media 里，这里只放桌面这边的胶水代码。
//!
//! 线程（计划 §0.3）：net 线程（ClientConn + Assembler）、input 线程（1 kHz 采手柄）、
//! decode 线程（FFmpeg + VideoToolbox）、主线程（SDL 事件泵、渲染）、SDL 音频回调线程（AudioJitter）、日志线程。

// 第 0 步骨架，各组实现时去掉：函数体还是 todo!()，参数和私有字段暂时用不上；
// 输出参数 `&mut Vec` 还没 push，clippy 会误报 ptr_arg。
#![allow(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod audio_out;
pub mod config;
pub mod decode;
pub mod input;
pub mod net;
pub mod overlay;
pub mod platform;
pub mod present;
