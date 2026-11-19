//! 客户端本地用的 Annex-B 小工具：按起始码找 NAL、把整段录像切成 AU、取文件开头的参数集。
//!
//! 只给 `--play` 调试入口和解码基准用。网络路径上的 AU 由 Assembler 组好，不经过这里。
//! TODO(E 组)：yw-media 的 `nal` 和 `AnnexBReader` 交付后改用它们，删掉这个文件。

use std::ops::Range;
use yw_media::Codec;

/// 一个 NAL：`start` 是起始码（含前导 0）的位置，`body` 是去掉起始码后的范围
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nal {
    pub start: usize,
    pub body: Range<usize>,
}

/// 按 00 00 01 / 00 00 00 01 切出所有 NAL
pub fn nals(data: &[u8]) -> Vec<Nal> {
    let mut starts = Vec::new(); // (起始码位置, 负载位置)
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let sc = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((sc, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &(sc, body)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(data.len(), |&(next_sc, _)| next_sc);
        if body < end {
            out.push(Nal { start: sc, body: body..end });
        }
    }
    out
}

/// NAL 的类型（HEVC 取 6 位，AVC 取 5 位）
pub fn nal_type(body: &[u8], c: Codec) -> Option<u8> {
    let b0 = *body.first()?;
    Some(match c {
        Codec::Hevc => (b0 >> 1) & 0x3f,
        Codec::Avc => b0 & 0x1f,
    })
}

fn is_vcl(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => t < 32,
        Codec::Avc => (1..=5).contains(&t),
    }
}

/// 出现在一帧开头、能分隔 AU 的非 VCL NAL（参数集、AUD、前置 SEI）
fn is_au_prefix(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => matches!(t, 32..=35 | 39),
        Codec::Avc => matches!(t, 6..=9),
    }
}

/// 参数集：HEVC 的 VPS/SPS/PPS，AVC 的 SPS/PPS
pub fn is_param_set(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => matches!(t, 32..=34),
        Codec::Avc => matches!(t, 7 | 8),
    }
}

/// 是不是一帧的第一个 slice（HEVC 看 first_slice_segment_in_pic_flag，AVC 看 first_mb_in_slice == 0）
fn first_slice(body: &[u8], c: Codec) -> bool {
    match c {
        Codec::Hevc => body.get(2).is_some_and(|b| b & 0x80 != 0),
        Codec::Avc => body.get(1).is_some_and(|b| b & 0x80 != 0),
    }
}

/// 关键帧：以第一个 VCL NAL 为准（HEVC 16~21 是 BLA/IDR/CRA，AVC 5 是 IDR）
pub fn is_keyframe(au: &[u8], c: Codec) -> bool {
    for n in nals(au) {
        let body = &au[n.body];
        let Some(t) = nal_type(body, c) else { continue };
        if is_vcl(t, c) {
            return match c {
                Codec::Hevc => (16..=21).contains(&t),
                Codec::Avc => t == 5,
            };
        }
    }
    false
}

/// 把整段 Annex-B 码流切成 AU（每个范围含起始码，开头的参数集归进第一个 AU）
pub fn split_aus(data: &[u8], c: Codec) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut cur_start: Option<usize> = None;
    let mut cur_has_vcl = false;
    for n in nals(data) {
        let body = &data[n.body.clone()];
        let Some(t) = nal_type(body, c) else { continue };
        let vcl = is_vcl(t, c);
        let begins_new = cur_has_vcl && ((vcl && first_slice(body, c)) || is_au_prefix(t, c));
        if begins_new {
            if let Some(s) = cur_start {
                out.push(s..n.start);
            }
            cur_start = Some(n.start);
            cur_has_vcl = false;
        } else if cur_start.is_none() {
            cur_start = Some(n.start);
        }
        cur_has_vcl |= vcl;
    }
    if let (Some(s), true) = (cur_start, cur_has_vcl) {
        out.push(s..data.len());
    }
    out
}

/// 一段码流开头（第一个 VCL NAL 之前）的参数集，按原样（带起始码）拼起来
pub fn leading_param_sets(data: &[u8], c: Codec) -> Vec<u8> {
    let mut out = Vec::new();
    for n in nals(data) {
        let body = &data[n.body.clone()];
        let Some(t) = nal_type(body, c) else { continue };
        if is_vcl(t, c) {
            break;
        }
        if is_param_set(t, c) {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(body);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // HEVC NAL 头两字节：类型 << 1，第二字节 1（tid = 1）
    fn hevc(t: u8, rest: &[u8]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 1, t << 1, 1];
        v.extend_from_slice(rest);
        v
    }

    #[test]
    fn split_hevc_stream() {
        let mut s = Vec::new();
        s.extend(hevc(32, &[0xaa])); // VPS
        s.extend(hevc(33, &[0xbb])); // SPS
        s.extend(hevc(34, &[0xcc])); // PPS
        s.extend(hevc(20, &[0x80, 1, 2])); // IDR_N_LP，首个 slice
        let f1 = s.len();
        s.extend(hevc(1, &[0x80, 3])); // TRAIL_R
        let f2 = s.len();
        s.extend(hevc(1, &[0x80, 4]));
        s.extend(hevc(1, &[0x00, 5])); // 同一帧的第二个 slice
        let f3 = s.len();
        s.extend(hevc(33, &[0xbb])); // 中途换参数集
        s.extend(hevc(19, &[0x80, 6])); // IDR_W_RADL

        let aus = split_aus(&s, Codec::Hevc);
        assert_eq!(aus, vec![0..f1, f1..f2, f2..f3, f3..s.len()]);
        assert!(is_keyframe(&s[aus[0].clone()], Codec::Hevc));
        assert!(!is_keyframe(&s[aus[1].clone()], Codec::Hevc));
        assert!(is_keyframe(&s[aus[3].clone()], Codec::Hevc));

        let ps = leading_param_sets(&s, Codec::Hevc);
        let mut want = Vec::new();
        want.extend(hevc(32, &[0xaa]));
        want.extend(hevc(33, &[0xbb]));
        want.extend(hevc(34, &[0xcc]));
        assert_eq!(ps, want);
    }

    #[test]
    fn three_byte_start_codes_and_garbage() {
        let s = [0u8, 0, 1, 0x40, 1, 9, 0, 0, 1, 0x28, 1, 0x80, 7];
        let n = nals(&s);
        assert_eq!(n.len(), 2);
        assert_eq!(&s[n[1].body.clone()], &[0x28, 1, 0x80, 7]);
        assert_eq!(split_aus(&s, Codec::Hevc), vec![0..s.len()]);
        // 没有起始码、全零、空输入都不出 AU，也不 panic
        assert!(split_aus(&[1, 2, 3], Codec::Hevc).is_empty());
        assert!(split_aus(&[0; 16], Codec::Hevc).is_empty());
        assert!(split_aus(&[], Codec::Avc).is_empty());
    }

    #[test]
    fn split_avc_stream() {
        let s = [
            0, 0, 0, 1, 0x67, 1, // SPS
            0, 0, 0, 1, 0x68, 2, // PPS
            0, 0, 0, 1, 0x65, 0x88, // IDR
            0, 0, 0, 1, 0x41, 0x9a, // P
        ];
        let aus = split_aus(&s, Codec::Avc);
        assert_eq!(aus, vec![0..18, 18..24]);
        assert!(is_keyframe(&s[0..18], Codec::Avc));
        assert!(!is_keyframe(&s[18..24], Codec::Avc));
    }
}
