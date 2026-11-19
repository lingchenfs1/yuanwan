//! PN 还原（A 组）：RFC 9000 附录 A.3 的算法改成 32 位截断。
//! 以该路径收到过的最大 PN + 1 为期望值，在 ±2³¹ 范围内取低 32 位与线上值相等、且离期望值最近的 64 位数。

/// largest 为 None 时（还没收到过包）按期望值 0 算
pub fn decode_pn(largest: Option<u64>, trunc: u32) -> u64 {
    todo!()
}
