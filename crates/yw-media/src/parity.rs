//! 冗余表（M2a 固定规则，M2c 自适应）。
//! M2a：P 帧 m = max(1, ⌈0.05k⌉)，k ≤ 4 时 m ≥ 2；关键帧 m = max(2, ⌈0.1k⌉)；R = m。
//! 备用校验（M2c）：P 帧 R = m + 2，关键帧 R = m + 4。

/// 呈现模式。默认竞技（J = 0，计划 §0.5 第 6 项）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    #[default]
    Competitive,
    Balanced,
    Stable,
}

impl Mode {
    /// 线上的取值（ywp::consts::mode）
    pub fn to_wire(self) -> u8 {
        match self {
            Mode::Competitive => ywp::consts::mode::COMPETITIVE,
            Mode::Balanced => ywp::consts::mode::BALANCED,
            Mode::Stable => ywp::consts::mode::STABLE,
        }
    }

    pub fn from_wire(v: u8) -> Option<Self> {
        match v {
            ywp::consts::mode::COMPETITIVE => Some(Mode::Competitive),
            ywp::consts::mode::BALANCED => Some(Mode::Balanced),
            ywp::consts::mode::STABLE => Some(Mode::Stable),
            _ => None,
        }
    }
}

/// 下行丢包状态（M2c 只用 p_s；p_l 和 b 的修正在 M3）
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LossState {
    pub p_s: f32,
    pub p_l: f32,
    pub b: f32,
    /// 盲发档（上行静默 ≥ 500 ms）
    pub blind: bool,
}

#[derive(Clone, Debug)]
pub struct ParityTable {
    _priv: (),
}

impl ParityTable {
    /// M2a 的固定规则
    pub fn fixed() -> Self {
        todo!()
    }

    /// M2c：按 ε × p × k 现算
    pub fn build() -> Self {
        todo!()
    }

    /// 返回 (m：初始发送的校验数, R：编码生成的校验总数)
    pub fn plan(&self, k: u16, key: bool, mode: Mode, st: &LossState) -> (u16, u16) {
        todo!()
    }
}
