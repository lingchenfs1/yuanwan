//! 场景回归：两端用真实的 VideoTx / Assembler / SentLog / RxLoss / VsyncScheduler / FreezeMeter，不跑加密。
//! M2c 里由框架在上行静默 ≥ 500 ms 时调 set_blind，M2d 起改由 Liveness 驱动。

use crate::gen::FrameGen;
use crate::link::LinkModel;
use ywp::time::Micros;
use yw_media::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngressMode {
    /// 整帧（M2 的路线 a）
    Whole,
    /// 逐 unit（以后的 b′ / b）
    PerUnit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Scenario {
    pub name: &'static str,
    pub duration: Micros,
    pub down: LinkModel,
    pub up: LinkModel,
    pub mode: Mode,
    pub ingress: IngressMode,
    pub seed: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct SimReport {
    /// (开始, 结束)，W3C 口径
    pub freezes: Vec<(Micros, Micros)>,
    pub l2: u32,
    pub l3: u32,
    pub fec_overhead: f32,
    pub repair_ratio: f32,
    pub audio_plc: u32,
}

pub fn run(s: &Scenario, gen: FrameGen) -> SimReport {
    todo!()
}
