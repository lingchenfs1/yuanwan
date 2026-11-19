//! PN 还原（A 组，规格 §4.5）：RFC 9000 附录 A.3 的算法改成 32 位截断。
//! 以该路径收到过的最大 PN + 1 为期望值，在 ±2³¹ 范围内取低 32 位与线上值相等、且离期望值最近的 64 位数。

#![warn(unused_variables, dead_code)]

const WIN: u64 = 1 << 32;
const HWIN: u64 = WIN / 2;
const MASK: u64 = WIN - 1;
/// RFC 9000 的 PN 上限是 2⁶²，这里沿用
const PN_LIMIT: u64 = 1 << 62;

/// largest 为 None 时（还没收到过包）按期望值 0 算
pub fn decode_pn(largest: Option<u64>, trunc: u32) -> u64 {
    let expected = largest.map_or(0, |l| l.saturating_add(1));
    let candidate = (expected & !MASK) | trunc as u64;
    if expected >= HWIN && candidate <= expected - HWIN && candidate < PN_LIMIT - WIN {
        candidate + WIN
    } else if candidate > expected.saturating_add(HWIN) && candidate >= WIN {
        candidate - WIN
    } else {
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9000_style() {
        assert_eq!(decode_pn(None, 0), 0);
        assert_eq!(decode_pn(None, 5), 5);
        assert_eq!(decode_pn(Some(0xFFFF_FFFE), 0xFFFF_FFFF), 0xFFFF_FFFF);
        // 回绕
        assert_eq!(decode_pn(Some(0xFFFF_FFFF), 0), 0x1_0000_0000);
        assert_eq!(decode_pn(Some(0x1_0000_0002), 0xFFFF_FFF0), 0xFFFF_FFF0);
        // 乱序到达的旧包
        assert_eq!(decode_pn(Some(1000), 900), 900);
        assert_eq!(decode_pn(Some(0x1_0000_0010), 0x0000_0005), 0x1_0000_0005);
    }
}
