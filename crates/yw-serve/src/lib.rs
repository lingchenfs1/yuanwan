//! 网关侧的 YWP 服务，不含 chiaki（D 组）。yw-gateway 和 yw-sim 的 fakegw 跑的是同一份服务代码。
//!
//! 视频来源和 PS5 控制都经 trait 接入：[`source::MediaMsg`]（chiaki 回调 → 服务循环）、
//! [`source::PsControl`]（服务循环 → PS5 会话）。yw-gateway 提供 chiaki 实现，fakegw 提供读录像文件的实现。

// 第 0 步骨架，各组实现时去掉：函数体还是 todo!()，参数和私有字段暂时用不上；
// 输出参数 `&mut Vec` 还没 push，clippy 会误报 ptr_arg。
#![allow(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod feedback;
pub mod idr;
pub mod ingress;
pub mod input;
pub mod server;
pub mod source;

pub use server::{spawn, JsonlSink, ServeConfig, ServeHandle};
pub use source::{AudioHeader, CtlRequest, FbEvent, MediaMsg, PsControl, ServeEvent};
