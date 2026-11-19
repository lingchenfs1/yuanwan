//! 离线仿真（C 组）：链路模型、合成帧源、E8 轨迹回放、场景回归。
//! 两端用真实的 VideoTx / Assembler / SentLog / RxLoss / VsyncScheduler / FreezeMeter，不跑加密。
//! 工具 fakegw、lossy-relay 在 src/bin（F 组）。

// 第 0 步骨架，各组实现时去掉：函数体还是 todo!()，参数和私有字段暂时用不上；
// 输出参数 `&mut Vec` 还没 push，clippy 会误报 ptr_arg。
#![allow(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

pub mod gen;
pub mod link;
pub mod scenario;

pub use gen::{FrameGen, GenCfg};
pub use link::{LinkModel, LossModel};
pub use scenario::{run, IngressMode, Scenario, SimReport};

// 第 0 步骨架：fakegw 用到 yw-serve 之前先占位（免得 cargo 报 unused_dependencies），F 组用上后删掉
use yw_serve as _;
