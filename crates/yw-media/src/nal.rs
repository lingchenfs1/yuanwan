//! Annex-B NAL 工具（M2a，从 yw-gateway 的 handler.rs 挪过来，支持 HEVC 和 AVC）。

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Avc,
    Hevc,
}

/// 按起始码切出每个 NAL（不含起始码）
pub fn nal_units(au: &[u8]) -> impl Iterator<Item = &[u8]> {
    NalUnits { rest: au }
}

struct NalUnits<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

/// 以第一个 VCL NAL 为准
pub fn is_keyframe(au: &[u8], c: Codec) -> bool {
    todo!()
}

/// 只含参数集（VPS / SPS / PPS），没有 slice
pub fn is_param_sets_only(au: &[u8], c: Codec) -> bool {
    todo!()
}
