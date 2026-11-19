//! 链路模型：时延、抖动、丢包（Bernoulli / Gilbert-Elliott / 轨迹 / 按包长）、速率、队列、中断。

use ywp::time::Micros;

#[derive(Clone, Debug, PartialEq)]
pub enum LossModel {
    None,
    Bernoulli(f64),
    /// 好坏两态：p 好→坏、r 坏→好、h 坏态下不丢的概率、k 好态下不丢的概率
    GilbertElliott { p: f64, r: f64, h: f64, k: f64 },
    /// 逐包回放（true = 丢）
    Trace(Vec<bool>),
    /// 丢包率 = base + per_byte × 包长（大包更容易丢的链路）
    BySize { base: f64, per_byte: f64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinkModel {
    pub delay_us: u32,
    pub jitter_us: u32,
    pub loss: LossModel,
    pub rate_bps: Option<u64>,
    pub queue_bytes: usize,
    /// (起点, 时长)
    pub outages: Vec<(Micros, Micros)>,
}
