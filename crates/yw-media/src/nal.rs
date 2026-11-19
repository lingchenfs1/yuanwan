//! Annex-B NAL 工具（M2a，从 yw-gateway 的 handler.rs 挪过来，支持 HEVC 和 AVC）。
//!
//! - NAL 以起始码 `00 00 01` 或 `00 00 00 01` 分隔；切出的 NAL 不含起始码，末尾的 0x00
//!   （trailing_zero_8bits，也就是下一个 4 字节起始码的头一个 0）去掉。NAL 本身的最后一个字节
//!   不会是 0x00（rbsp_trailing_bits 以 1 结尾，cabac_zero_word 经防竞争处理后是 `00 00 03`）。
//! - 关键帧以第一个 VCL NAL 为准（规格 §7.1）：H.264 是 5（IDR）；HEVC 是 16~21（IRAP）。

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Avc,
    Hevc,
}

/// 写 AU 时用的 4 字节起始码（和 PS5 会话库交出来的格式一致）
pub const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// 按起始码切出每个 NAL（不含起始码）。第一个起始码之前的字节忽略；空 NAL 跳过
pub fn nal_units(au: &[u8]) -> impl Iterator<Item = &[u8]> {
    NalUnits { rest: find_start(au).map_or(&[][..], |(_, after)| &au[after..]) }
}

/// 找下一个 `00 00 01`：返回 (起始码前面那段的长度, 起始码之后的偏移)
pub(crate) fn find_start(b: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 2 < b.len() {
        // 跳读：b[i + 2] 不是 0 也不是 1 时，起始码不可能从 i、i+1、i+2 开始
        match b[i + 2] {
            0 => i += 1,
            1 if b[i] == 0 && b[i + 1] == 0 => return Some((i, i + 3)),
            1 => i += 3,
            _ => i += 3,
        }
    }
    None
}

/// 去掉末尾的 0x00
pub(crate) fn trim_zeros(mut b: &[u8]) -> &[u8] {
    while let [head @ .., 0] = b {
        b = head;
    }
    b
}

struct NalUnits<'a> {
    /// 当前 NAL 的开头（已跳过起始码）
    rest: &'a [u8],
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        while !self.rest.is_empty() {
            let (nal, rest) = match find_start(self.rest) {
                Some((len, after)) => (&self.rest[..len], &self.rest[after..]),
                None => (self.rest, &[][..]),
            };
            self.rest = rest;
            let nal = trim_zeros(nal);
            if !nal.is_empty() {
                return Some(nal);
            }
        }
        None
    }
}

/// NAL 类型（HEVC 取首字节的 6 位，AVC 取低 5 位）。空 NAL 返回 None
pub fn nal_type(nal: &[u8], c: Codec) -> Option<u8> {
    let b = *nal.first()?;
    Some(match c {
        Codec::Hevc => (b >> 1) & 0x3f,
        Codec::Avc => b & 0x1f,
    })
}

/// 是不是 VCL（slice）：HEVC 0~31，AVC 1~5
pub fn is_vcl(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => t <= 31,
        Codec::Avc => (1..=5).contains(&t),
    }
}

/// 是不是关键帧的 slice：HEVC 16~21（IRAP），AVC 5（IDR）
pub fn is_key_vcl(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => (16..=21).contains(&t),
        Codec::Avc => t == 5,
    }
}

/// 是不是参数集：HEVC 的 VPS / SPS / PPS（32~34），AVC 的 SPS / PPS（7、8）
pub fn is_param_set(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => (32..=34).contains(&t),
        Codec::Avc => t == 7 || t == 8,
    }
}

/// VCL NAL 是不是一帧的第一个 slice：HEVC 看 first_slice_segment_in_pic_flag（NAL 头之后的第一位），
/// AVC 看 first_mb_in_slice == 0（ue(v) 的 0 编码成单个 1 位）。不是 VCL 或太短时返回 false
pub fn is_first_slice(nal: &[u8], c: Codec) -> bool {
    let Some(t) = nal_type(nal, c) else { return false };
    if !is_vcl(t, c) {
        return false;
    }
    let hdr = match c {
        Codec::Hevc => 2,
        Codec::Avc => 1,
    };
    nal.get(hdr).is_some_and(|b| b & 0x80 != 0)
}

/// 这个 NAL 是否开始一个新的 AU（前面已经有 VCL 时才有意义）：
/// HEVC 是 VPS/SPS/PPS、AUD、前缀 SEI、41~44、48~55，或 first_slice_segment_in_pic_flag = 1 的 slice；
/// AVC 是 SEI、SPS、PPS、AUD、14~18，或 first_mb_in_slice = 0 的 slice
pub fn starts_new_au(nal: &[u8], c: Codec) -> bool {
    let Some(t) = nal_type(nal, c) else { return false };
    let non_vcl = match c {
        Codec::Hevc => matches!(t, 32..=35 | 39 | 41..=44 | 48..=55),
        Codec::Avc => matches!(t, 6..=9 | 14..=18),
    };
    non_vcl || is_first_slice(nal, c)
}

/// 以第一个 VCL NAL 为准
pub fn is_keyframe(au: &[u8], c: Codec) -> bool {
    nal_units(au).filter_map(|n| nal_type(n, c)).find(|&t| is_vcl(t, c)).is_some_and(|t| is_key_vcl(t, c))
}

/// 只含参数集（VPS / SPS / PPS），没有 slice
pub fn is_param_sets_only(au: &[u8], c: Codec) -> bool {
    let mut params = false;
    for t in nal_units(au).filter_map(|n| nal_type(n, c)) {
        if is_vcl(t, c) {
            return false;
        }
        if is_param_set(t, c) {
            params = true;
        }
    }
    params
}

/// AU 的第一个 NAL 是不是参数集（已经自带参数集的关键帧不用再补）
pub fn starts_with_param_set(au: &[u8], c: Codec) -> bool {
    nal_units(au).next().and_then(|n| nal_type(n, c)).is_some_and(|t| is_param_set(t, c))
}

/// 按参数集的 NAL 类型猜编码格式：HEVC 的 VPS/SPS/PPS 和 AVC 的 SPS/PPS 首字节互不重叠。认不出返回 None
pub fn guess_codec(params: &[u8]) -> Option<Codec> {
    let first = nal_units(params).next()?;
    if nal_type(first, Codec::Hevc).is_some_and(|t| is_param_set(t, Codec::Hevc)) {
        Some(Codec::Hevc)
    } else if nal_type(first, Codec::Avc).is_some_and(|t| is_param_set(t, Codec::Avc)) {
        Some(Codec::Avc)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 拼一个 AU：每个 NAL 前面放 4 字节起始码
    fn au(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&START_CODE);
            v.extend_from_slice(n);
        }
        v
    }

    // HEVC NAL 头：type << 1，第二字节 tid + 1 = 1
    const VPS: &[u8] = &[0x40, 0x01, 0x0c, 0x01];
    const SPS: &[u8] = &[0x42, 0x01, 0x01, 0x22];
    const PPS: &[u8] = &[0x44, 0x01, 0xc1, 0x72];
    const IDR_N_LP: &[u8] = &[0x28, 0x01, 0xaf, 0x1d, 0x80];
    const TRAIL_R: &[u8] = &[0x02, 0x01, 0xd0, 0x12, 0x34];
    const SEI: &[u8] = &[0x4e, 0x01, 0x05, 0x10];

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        // 3 字节起始码、4 字节起始码混用，开头有垃圾字节，NAL 里有 00 00 03
        let mut b = vec![0xAA, 0xBB];
        b.extend_from_slice(&[0, 0, 1, 0x40, 0x01, 0x00, 0x00, 0x03, 0x01]);
        b.extend_from_slice(&[0, 0, 0, 1, 0x42, 0x01, 0x07]);
        b.extend_from_slice(&[0, 0, 1, 0x44, 0x01]);
        let v: Vec<&[u8]> = nal_units(&b).collect();
        assert_eq!(v, vec![&[0x40, 0x01, 0x00, 0x00, 0x03, 0x01][..], &[0x42, 0x01, 0x07][..], &[0x44, 0x01][..]]);
    }

    #[test]
    fn trailing_zeros_and_empty_nals() {
        let b = [0, 0, 0, 1, 0x02, 0x01, 0x05, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0x02, 0x01, 0, 0];
        let v: Vec<&[u8]> = nal_units(&b).collect();
        assert_eq!(v, vec![&[0x02, 0x01, 0x05][..], &[0x02, 0x01][..]]);
        assert_eq!(nal_units(&[]).count(), 0);
        assert_eq!(nal_units(&[1, 2, 3, 4]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 1]).count(), 0);
    }

    #[test]
    fn hevc_keyframe_by_first_vcl() {
        assert!(is_keyframe(&au(&[IDR_N_LP]), Codec::Hevc));
        assert!(is_keyframe(&au(&[VPS, SPS, PPS, SEI, IDR_N_LP]), Codec::Hevc));
        assert!(!is_keyframe(&au(&[TRAIL_R]), Codec::Hevc));
        // 以第一个 VCL 为准：P slice 在前，后面的 IRAP 不算
        assert!(!is_keyframe(&au(&[TRAIL_R, IDR_N_LP]), Codec::Hevc));
        assert!(!is_keyframe(&au(&[VPS, SPS, PPS]), Codec::Hevc));
        // CRA（21）算，类型 22（保留的 IRAP）不算
        assert!(is_keyframe(&au(&[&[21 << 1, 0x01, 0x80]]), Codec::Hevc));
        assert!(!is_keyframe(&au(&[&[22 << 1, 0x01, 0x80]]), Codec::Hevc));
    }

    #[test]
    fn hevc_param_sets_only() {
        assert!(is_param_sets_only(&au(&[VPS, SPS, PPS]), Codec::Hevc));
        assert!(is_param_sets_only(&au(&[SEI, PPS]), Codec::Hevc));
        assert!(!is_param_sets_only(&au(&[VPS, SPS, PPS, IDR_N_LP]), Codec::Hevc));
        assert!(!is_param_sets_only(&au(&[SEI]), Codec::Hevc));
        assert!(!is_param_sets_only(&[], Codec::Hevc));
        assert!(starts_with_param_set(&au(&[VPS, IDR_N_LP]), Codec::Hevc));
        assert!(!starts_with_param_set(&au(&[IDR_N_LP]), Codec::Hevc));
    }

    #[test]
    fn avc_rules() {
        let sps: &[u8] = &[0x67, 0x64, 0x00, 0x28];
        let pps: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
        let idr: &[u8] = &[0x65, 0x88, 0x84];
        let p: &[u8] = &[0x41, 0x9a, 0x02];
        let sei: &[u8] = &[0x06, 0x05, 0x10];
        assert!(is_keyframe(&au(&[sps, pps, idr]), Codec::Avc));
        assert!(is_keyframe(&au(&[sei, idr]), Codec::Avc));
        assert!(!is_keyframe(&au(&[p]), Codec::Avc));
        assert!(!is_keyframe(&au(&[p, idr]), Codec::Avc));
        assert!(is_param_sets_only(&au(&[sps, pps]), Codec::Avc));
        assert!(!is_param_sets_only(&au(&[sps, pps, idr]), Codec::Avc));
        assert!(is_first_slice(idr, Codec::Avc));
        // first_mb_in_slice ≠ 0
        assert!(!is_first_slice(&[0x41, 0x40], Codec::Avc));
        assert_eq!(guess_codec(&au(&[sps, pps])), Some(Codec::Avc));
    }

    #[test]
    fn first_slice_flag_and_codec_guess() {
        assert!(is_first_slice(IDR_N_LP, Codec::Hevc));
        assert!(!is_first_slice(&[0x02, 0x01, 0x40], Codec::Hevc));
        assert!(!is_first_slice(VPS, Codec::Hevc));
        assert!(!is_first_slice(&[0x02, 0x01], Codec::Hevc));
        assert_eq!(guess_codec(&au(&[VPS, SPS, PPS])), Some(Codec::Hevc));
        assert_eq!(guess_codec(&au(&[TRAIL_R])), None);
        assert_eq!(guess_codec(&[]), None);
    }
}
