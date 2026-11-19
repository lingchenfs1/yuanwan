//! 网关侧的 YWP 服务，不含 chiaki（D 组）。yw-gateway 和 yw-sim 的 fakegw 跑的是同一份服务代码。
//!
//! 视频来源和 PS5 控制都经 trait 接入：[`source::MediaMsg`]（chiaki 回调 → 服务循环）、
//! [`source::PsControl`]（服务循环 → PS5 会话）。yw-gateway 提供 chiaki 实现，fakegw 提供读录像文件的实现。

pub mod clock;
pub mod idr;
pub mod ingress;
pub mod source;

// 还是第 0 步骨架的模块（函数体是 todo!()，参数和私有字段暂时用不上），实现时去掉这里的 allow：
// server 在 M2a 第二轮（等 endpoint 和 VideoTx），input、feedback 在 M2b
#[allow(unused_variables, dead_code, clippy::new_without_default)]
pub mod feedback;
#[allow(unused_variables, dead_code, clippy::new_without_default)]
pub mod input;
#[allow(unused_variables, dead_code)]
pub mod server;

pub use idr::{IdrDecision, IdrGovernor, IdrReason};
pub use ingress::{Ingress, IngressOut, RecoveredPolicy, StreamInfo, VideoIn, WaitWhy};
pub use server::{spawn, JsonlSink, ServeConfig, ServeHandle};
pub use source::{AudioHeader, CtlRequest, FbEvent, MediaMsg, PsControl, ServeEvent};
