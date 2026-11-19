//! 桌面客户端（E 组）。协议和媒体逻辑都在 ywp、yw-media 里，这里只放桌面这边的胶水代码。
//!
//! 线程：net 线程（ClientConn + Assembler）、input 线程（1 kHz 采手柄）、
//! decode 线程（FFmpeg + VideoToolbox）、主线程（SDL 事件泵、渲染）、SDL 音频回调线程（AudioJitter）、日志线程。

pub mod clock;
pub mod config;
pub mod decode;
pub mod net;
pub mod overlay;
pub mod platform;
pub mod play;
pub mod present;
pub mod stream;

// 以下模块还是第 0 步骨架（函数体 todo!()，参数暂时用不上），实现时去掉 allow：input、audio_out 在 M2b。
#[allow(unused_variables, dead_code)]
pub mod audio_out;
#[allow(unused_variables, dead_code)]
pub mod input;
