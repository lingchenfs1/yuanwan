//! YWP 协议核心，不碰 I/O（sans-IO）。
//!
//! - 不开 socket、不起线程、不读单调时钟：调用方传入 `now: Micros` 和数据报，
//!   库返回要发的数据报、事件和下一次超时。
//! - 网络输入一律不 panic：解析都返回 `Result`，丢弃原因计入 [`DropReason`]。
//! - `net` feature 提供绑网卡的 socket，`testkit` feature 提供虚拟时钟和内存管道。
//!
//! 文件归属（M2 计划 §5）：`lib.rs` 的 mod 行和 `consts`、`time`、`error` 归协调人；
//! `wire`、`crypto`、`pn`、`replay`、`packet`、`net`、`creds` 归 A 组；其余归 B 组。

// 第 0 步骨架，各组实现时去掉：函数体还是 todo!()，参数和私有字段暂时用不上；
// 输出参数 `&mut Vec` 还没 push，clippy 会误报 ptr_arg。
#![allow(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

// ---- 协调人 ----
pub mod consts;
pub mod error;
pub mod time;

// ---- A 组：线上格式与加密 ----
pub mod creds;
pub mod crypto;
pub mod packet;
pub mod pn;
pub mod replay;
pub mod wire;
#[cfg(feature = "net")]
pub mod net;

// ---- B 组：传输状态机 ----
pub mod chan;
pub mod endpoint;
pub mod loss;
pub mod msg;
pub mod path;
pub mod reliable;
pub mod rtt;
pub mod sched;
pub mod sentlog;
pub mod uplink;
#[cfg(any(test, feature = "testkit"))]
pub mod testkit;

pub use error::{DropCounters, DropReason, WireError};
pub use time::Micros;

// 第 0 步骨架：依赖一次写全，还没用上的先占位（免得 cargo 报 unused_dependencies），各组用上后删掉
use hkdf as _;
use snow as _;
#[cfg(feature = "net")]
use socket2 as _;
