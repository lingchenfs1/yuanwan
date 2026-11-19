//! 冗余表（M2a 固定规则，M2c 自适应）。
//! M2a：P 帧 m = max(1, ⌈0.05k⌉)，k ≤ 4 时 m ≥ 2；关键帧 m = max(2, ⌈0.1k⌉)；R = m。
//! 备用校验（M2c）：P 帧 R = m + 2，关键帧 R = m + 4。

use ywp::consts::{FEC_KR_MAX, FEC_K_MAX, FEC_R_MAX};

/// 呈现模式（规格 §9.2）。默认竞技（J = 0）
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
    kind: TableKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TableKind {
    /// M2a：只看 k 和是否关键帧
    Fixed,
}

impl ParityTable {
    /// M2a 的固定规则
    pub fn fixed() -> Self {
        Self { kind: TableKind::Fixed }
    }

    /// M2c：按 ε × p × k 现算
    pub fn build() -> Self {
        todo!()
    }

    /// 返回 (m：初始发送的校验数, R：编码生成的校验总数)。
    ///
    /// 固定规则（M2a）：
    /// - P 帧 m = max(1, ⌈0.05k⌉)，k ≤ 4 时 m ≥ 2（规格 §7.6 第 5 条）；
    /// - 关键帧 m = max(2, ⌈0.1k⌉)；
    /// - 盲发档（规格 §7.6 第 7 条）m = max(m, 2, ⌈0.2k⌉)；
    /// - R = m（M2a 不留备用校验），并保证 R ≤ 1023、k + R ≤ 2048。
    ///
    /// 模式和丢包率在固定规则里不起作用。
    pub fn plan(&self, k: u16, key: bool, mode: Mode, st: &LossState) -> (u16, u16) {
        let _ = mode;
        let k = k.clamp(1, FEC_K_MAX);
        let m = match self.kind {
            TableKind::Fixed => fixed_m(k, key, st.blind),
        };
        let m = m.min(FEC_R_MAX).min(FEC_KR_MAX - k);
        (m, m)
    }
}

/// ⌈k × num / den⌉
fn ceil_frac(k: u16, num: u32, den: u32) -> u16 {
    (k as u32 * num).div_ceil(den) as u16
}

fn fixed_m(k: u16, key: bool, blind: bool) -> u16 {
    let mut m = if key {
        ceil_frac(k, 1, 10).max(2)
    } else {
        let m = ceil_frac(k, 1, 20).max(1);
        if k <= 4 {
            m.max(2)
        } else {
            m
        }
    };
    if blind {
        m = m.max(2).max(ceil_frac(k, 1, 5));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(k: u16, key: bool) -> (u16, u16) {
        ParityTable::fixed().plan(k, key, Mode::Competitive, &LossState::default())
    }

    #[test]
    fn small_p_frames_get_two() {
        for k in 1..=4 {
            assert_eq!(plan(k, false), (2, 2), "k={k}");
        }
        // k = 5..=20：⌈0.05k⌉ = 1
        for k in 5..=20 {
            assert_eq!(plan(k, false), (1, 1), "k={k}");
        }
        assert_eq!(plan(21, false), (2, 2));
        assert_eq!(plan(40, false), (2, 2));
        assert_eq!(plan(41, false), (3, 3));
        assert_eq!(plan(1024, false), (52, 52));
    }

    #[test]
    fn key_frames() {
        assert_eq!(plan(1, true), (2, 2));
        assert_eq!(plan(20, true), (2, 2));
        assert_eq!(plan(21, true), (3, 3));
        assert_eq!(plan(26, true), (3, 3));
        assert_eq!(plan(1024, true), (103, 103));
    }

    #[test]
    fn every_k_respects_limits() {
        let t = ParityTable::fixed();
        for k in 1..=FEC_K_MAX {
            for key in [false, true] {
                for blind in [false, true] {
                    let st = LossState { blind, ..LossState::default() };
                    let (m, r) = t.plan(k, key, Mode::Balanced, &st);
                    assert_eq!(m, r);
                    assert!((1..=FEC_R_MAX).contains(&m) && k + r <= FEC_KR_MAX);
                    if k <= 4 {
                        assert!(m >= 2, "k={k} key={key}");
                    }
                    if blind {
                        assert!(m >= 2 && m as u32 >= (k as u32).div_ceil(5));
                    }
                }
            }
        }
    }
}
