//! RS 封装和均分切片（M2a，规格 §5.5.1、§7.1 v0.2 注、§7.5）：
//! k = ⌈L / 1186⌉ ≤ 1024，d = ⌈L / k⌉；符号长度 S = 2 + d，向上取偶数。
//! 编码时每个源符号写成 `[len u16 大端][数据]`，用 0 补齐到 S。用 reed-solomon-simd 3.1.0（S 必须是偶数）。
//!
//! 每帧一个 RS 块：源符号的下标 0..k−1，校验符号 k..k+R−1（库里的 recovery 下标 = sym_idx − k）。
//! 解码要用编码时的 R（recovery_count），R 写在每个校验包里，不能事后改（T19，见本文件的单测）。

use reed_solomon_simd::{ReedSolomonDecoder, ReedSolomonEncoder};
use std::ops::Range;
use ywp::consts::{FEC_KR_MAX, FEC_K_MAX, FEC_R_MAX, MAX_SYMBOL_DATA, VIDEO_SRC_LEN_LEN};

#[derive(Debug)]
#[non_exhaustive]
pub enum FecError {
    /// 空帧
    Empty,
    /// k 会超过 1024
    FrameTooLarge { len: usize },
    /// k、R、S 不合法
    BadParams(&'static str),
    /// 收到的符号不够 k 个
    NotEnough { have: usize, k: usize },
    Rs(reed_solomon_simd::Error),
}

impl std::fmt::Display for FecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FecError::Empty => f.write_str("空帧"),
            FecError::FrameTooLarge { len } => write!(f, "帧太大：{len} B"),
            FecError::BadParams(what) => write!(f, "参数不对：{what}"),
            FecError::NotEnough { have, k } => write!(f, "符号不够：{have}/{k}"),
            FecError::Rs(e) => write!(f, "RS：{e}"),
        }
    }
}

impl std::error::Error for FecError {}

impl From<reed_solomon_simd::Error> for FecError {
    fn from(e: reed_solomon_simd::Error) -> Self {
        FecError::Rs(e)
    }
}

/// 返回 (k, d)：k 个源符号，每个最多 d 字节数据
pub fn split_frame(len: usize) -> Result<(u16, usize), FecError> {
    if len == 0 {
        return Err(FecError::Empty);
    }
    let k = len.div_ceil(MAX_SYMBOL_DATA);
    if k > FEC_K_MAX as usize {
        return Err(FecError::FrameTooLarge { len });
    }
    Ok((k as u16, len.div_ceil(k)))
}

/// 2 + d，向上取偶数
pub fn shard_len(d: usize) -> usize {
    (VIDEO_SRC_LEN_LEN + d + 1) & !1
}

/// 第 i 个源符号在帧里的字节范围（均分，最后一个可以更短）
pub fn source_range(len: usize, d: usize, i: usize) -> Range<usize> {
    let a = (i * d).min(len);
    a..(a + d).min(len)
}

/// 检查 k、R、S：k 1..=1024，R ≤ 1023，k + R ≤ 2048，S 是非零偶数
pub fn check_params(k: usize, r: usize, s: usize) -> Result<(), FecError> {
    if k == 0 || k > FEC_K_MAX as usize {
        return Err(FecError::BadParams("k 不在 1..=1024"));
    }
    if r > FEC_R_MAX as usize || k + r > FEC_KR_MAX as usize {
        return Err(FecError::BadParams("R 越界"));
    }
    if s < VIDEO_SRC_LEN_LEN || !s.is_multiple_of(2) {
        return Err(FecError::BadParams("S 必须是非零偶数"));
    }
    Ok(())
}

/// 把整帧均分成 k 个源符号，写成 `[len][数据][补 0]`，每个 S 字节。out 至少 k × S
pub fn pack_source(frame: &[u8], k: usize, d: usize, s: usize, out: &mut [u8]) -> Result<(), FecError> {
    if s < VIDEO_SRC_LEN_LEN + d || out.len() < k * s || k * d < frame.len() {
        return Err(FecError::BadParams("pack_source 的长度对不上"));
    }
    for i in 0..k {
        let r = source_range(frame.len(), d, i);
        let shard = &mut out[i * s..(i + 1) * s];
        shard[..2].copy_from_slice(&(r.len() as u16).to_be_bytes());
        shard[2..2 + r.len()].copy_from_slice(&frame[r.clone()]);
        shard[2 + r.len()..].fill(0);
    }
    Ok(())
}

/// 源符号的数据部分：按 len 取，len > S − 2 时返回 None
pub fn shard_data(shard: &[u8]) -> Option<&[u8]> {
    let len = u16::from_be_bytes([*shard.first()?, *shard.get(1)?]) as usize;
    shard.get(VIDEO_SRC_LEN_LEN..VIDEO_SRC_LEN_LEN + len)
}

/// 把 k 个 S 字节的源符号拼回整帧（追加到 out）
pub fn unpack_source(shards: &[u8], k: usize, s: usize, out: &mut Vec<u8>) -> Result<(), FecError> {
    if shards.len() < k * s {
        return Err(FecError::BadParams("源符号不够 k × S"));
    }
    for i in 0..k {
        let data = shard_data(&shards[i * s..(i + 1) * s]).ok_or(FecError::BadParams("len > S − 2"))?;
        out.extend_from_slice(data);
    }
    Ok(())
}

/// RS 编码器（复用工作区）
#[derive(Default)]
pub struct RsEnc {
    enc: Option<ReedSolomonEncoder>,
}

impl std::fmt::Debug for RsEnc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RsEnc")
    }
}

impl RsEnc {
    pub fn new() -> Self {
        Self { enc: None }
    }

    /// src：k × S 的源符号（已按 `[len][数据][补 0]` 排好）；out：R × S
    pub fn encode(&mut self, k: usize, r: usize, s: usize, src: &[u8], out: &mut [u8]) -> Result<(), FecError> {
        check_params(k, r, s)?;
        if src.len() != k * s || out.len() != r * s {
            return Err(FecError::BadParams("缓冲长度不是 k × S / R × S"));
        }
        if r == 0 {
            return Ok(());
        }
        let enc = match &mut self.enc {
            Some(e) => {
                e.reset(k, r, s)?;
                e
            }
            None => self.enc.insert(ReedSolomonEncoder::new(k, r, s)?),
        };
        for shard in src.chunks_exact(s) {
            enc.add_original_shard(shard)?;
        }
        let res = enc.encode()?;
        for (dst, rec) in out.chunks_exact_mut(s).zip(res.recovery_iter()) {
            dst.copy_from_slice(rec);
        }
        Ok(())
    }
}

/// RS 解码器（复用工作区）
#[derive(Default)]
pub struct RsDec {
    dec: Option<ReedSolomonDecoder>,
}

impl std::fmt::Debug for RsDec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RsDec")
    }
}

impl RsDec {
    pub fn new() -> Self {
        Self { dec: None }
    }

    /// have：(sym_idx, 符号)，每个符号 S 字节，重复的下标只取第一个；out：k × S，恢复出的全部源符号。
    /// 源符号齐了就直接拷贝，不调 RS
    pub fn recover<'a>(
        &mut self,
        k: usize,
        r: usize,
        s: usize,
        have: impl Iterator<Item = (u16, &'a [u8])>,
        out: &mut [u8],
    ) -> Result<(), FecError> {
        check_params(k, r, s)?;
        if out.len() != k * s {
            return Err(FecError::BadParams("out 不是 k × S"));
        }
        let mut seen = vec![false; k + r];
        let mut n_src = 0usize;
        let mut parity: Vec<(usize, &'a [u8])> = Vec::new();
        for (idx, sym) in have {
            let i = idx as usize;
            if i >= k + r {
                return Err(FecError::BadParams("sym_idx ≥ k + R"));
            }
            if sym.len() != s {
                return Err(FecError::BadParams("符号长度不是 S"));
            }
            if std::mem::replace(&mut seen[i], true) {
                continue;
            }
            if i < k {
                out[i * s..(i + 1) * s].copy_from_slice(sym);
                n_src += 1;
            } else {
                parity.push((i - k, sym));
            }
        }
        if n_src == k {
            return Ok(());
        }
        if n_src + parity.len() < k {
            return Err(FecError::NotEnough { have: n_src + parity.len(), k });
        }
        let dec = match &mut self.dec {
            Some(d) => {
                d.reset(k, r, s)?;
                d
            }
            None => self.dec.insert(ReedSolomonDecoder::new(k, r, s)?),
        };
        for i in (0..k).filter(|&i| seen[i]) {
            dec.add_original_shard(i, &out[i * s..(i + 1) * s])?;
        }
        // 只加够 k 个，多的用不上
        for &(j, sym) in parity.iter().take(k - n_src) {
            dec.add_recovery_shard(j, sym)?;
        }
        let res = dec.decode()?;
        for (i, shard) in res.restored_original_iter() {
            out[i * s..(i + 1) * s].copy_from_slice(shard);
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 测试用的确定性伪随机数（xorshift64*）
    pub(crate) struct Rng(pub u64);

    impl Rng {
        pub(crate) fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        pub(crate) fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        pub(crate) fn bytes(&mut self, n: usize) -> Vec<u8> {
            (0..n).map(|_| self.next() as u8).collect()
        }
    }

    /// 编一帧：返回 (k, S, 全部 k + R 个符号)
    fn encode_frame(frame: &[u8], r: usize) -> (usize, usize, Vec<Vec<u8>>) {
        let (k, d) = split_frame(frame.len()).unwrap();
        let k = k as usize;
        let s = shard_len(d);
        let mut buf = vec![0u8; (k + r) * s];
        pack_source(frame, k, d, s, &mut buf[..k * s]).unwrap();
        let (src, par) = buf.split_at_mut(k * s);
        RsEnc::new().encode(k, r, s, src, par).unwrap();
        (k, s, buf.chunks_exact(s).map(|c| c.to_vec()).collect())
    }

    /// 去掉 erased 里的符号后恢复，拼回整帧
    fn recover_frame(dec: &mut RsDec, k: usize, r: usize, s: usize, syms: &[Vec<u8>], erased: &[usize]) -> Result<Vec<u8>, FecError> {
        let have = syms.iter().enumerate().filter(|(i, _)| !erased.contains(i)).map(|(i, v)| (i as u16, v.as_slice()));
        let mut out = vec![0u8; k * s];
        dec.recover(k, r, s, have, &mut out)?;
        let mut frame = Vec::new();
        unpack_source(&out, k, s, &mut frame)?;
        Ok(frame)
    }

    #[test]
    fn split_and_shard() {
        assert!(matches!(split_frame(0), Err(FecError::Empty)));
        assert_eq!(split_frame(1).unwrap(), (1, 1));
        assert_eq!(split_frame(1186).unwrap(), (1, 1186));
        assert_eq!(split_frame(1187).unwrap(), (2, 594));
        assert_eq!(split_frame(1024 * 1186).unwrap(), (1024, 1186));
        assert!(matches!(split_frame(1024 * 1186 + 1), Err(FecError::FrameTooLarge { .. })));
        assert_eq!(shard_len(1), 4);
        assert_eq!(shard_len(594), 596);
        assert_eq!(shard_len(1186), 1188);
    }

    /// T19：S 必须是偶数（库拒绝奇数，封装层先拦住）
    #[test]
    fn t19_shard_len_must_be_even() {
        assert!(matches!(
            ReedSolomonEncoder::new(2, 1, 5),
            Err(reed_solomon_simd::Error::InvalidShardSize { shard_bytes: 5 })
        ));
        assert!(ReedSolomonEncoder::new(2, 1, 6).is_ok());
        let src = vec![0u8; 2 * 5];
        let mut out = vec![0u8; 5];
        assert!(matches!(RsEnc::new().encode(2, 1, 5, &src, &mut out), Err(FecError::BadParams(_))));
        // shard_len 一律给偶数
        for d in 1..=MAX_SYMBOL_DATA {
            assert_eq!(shard_len(d) % 2, 0);
            assert!(shard_len(d) >= 2 + d);
        }
    }

    /// T19：同样的源数据，R=3 和 R=5 的前 3 个校验是否相同。
    ///
    /// 实测（reed-solomon-simd 3.1.0）：**一般不同**，例如 k = 10、20、64 时 R=3 和 R=5 的校验完全不一样，
    /// 拿 R=5 的校验按 R=3 解码恢复不出原帧。只有库内部走同一条路径时才碰巧相同：k ≤ R（例如 k = 3），
    /// 或两个 R 落在同一个 2 的幂档里（例如 52 和 54、103 和 107）。所以 R 只能在编码时定死、写进每个校验包，
    /// 解码一律用包里的 R（规格 §7.5）
    #[test]
    fn t19_recovery_count_changes_parity() {
        let mut rng = Rng(0x5EED_0019);
        let mut dec = RsDec::new();
        // (k, R_a, R_b, 前 R_a 个校验是否相同)
        let cases = [
            (10usize, 3usize, 5usize, false),
            (20, 3, 5, false),
            (64, 3, 5, false),
            (10, 2, 4, false),
            (3, 3, 5, true),
            (64, 52, 54, true),
        ];
        for (k, ra, rb, same) in cases {
            let d = 32;
            let len = (k - 1) * d + 1 + rng.below(d);
            let frame = rng.bytes(len);
            let (_, s, a) = encode_frame_with_d(&frame, k, d, ra);
            let (_, _, b) = encode_frame_with_d(&frame, k, d, rb);
            assert_eq!(&a[..k], &b[..k], "源符号与 R 无关");
            assert_eq!(a[k..k + ra] == b[k..k + ra], same, "k={k} R={ra}/{rb}");
            // 拿 R_b 的前 R_a 个校验、按 R_a 解码（擦掉前 R_a 个源符号）
            let erased: Vec<usize> = (0..ra.min(k)).collect();
            let mut mixed: Vec<Vec<u8>> = a[..k].to_vec();
            mixed.extend_from_slice(&b[k..k + ra]);
            let cross = recover_frame(&mut dec, k, ra, s, &mixed, &erased).is_ok_and(|f| f == frame);
            assert_eq!(cross, same, "k={k} R={ra}/{rb}");
            // 按编码时的 R 解码总是对的
            assert_eq!(recover_frame(&mut dec, k, rb, s, &b, &erased).unwrap(), frame);
        }
    }

    /// T19：k = 1 能编能解
    #[test]
    fn t19_k1() {
        let mut dec = RsDec::new();
        for len in [1usize, 2, 7, 600, 1186] {
            let frame: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            for r in [1usize, 2, 3] {
                let (k, s, syms) = encode_frame(&frame, r);
                assert_eq!(k, 1);
                for lost in 0..=r {
                    // 源符号丢了，只剩 1 个校验
                    let erased: Vec<usize> = (0..=r).filter(|&i| i != lost).collect();
                    assert_eq!(recover_frame(&mut dec, k, r, s, &syms, &erased).unwrap(), frame);
                }
            }
        }
    }

    /// T19：k ∈ {1, 2, 4, 10, 25, 64, 1024}，随机擦除不超过 R 个都能恢复；超过 R 个报 NotEnough
    #[test]
    fn t19_random_erasures() {
        let mut rng = Rng(0xFEC0_0001);
        let mut dec = RsDec::new();
        for k in [1usize, 2, 4, 10, 25, 64, 1024] {
            let rs: Vec<usize> = [1usize, 2, 3, k.div_ceil(10).max(2), k.min(FEC_R_MAX as usize)].into_iter().collect();
            for r in rs {
                // 小 d 让 k = 1024 也跑得快；每个 k 另跑一次满长度
                for d in [16usize, MAX_SYMBOL_DATA] {
                    if k >= 64 && d == MAX_SYMBOL_DATA && r > 3 {
                        continue;
                    }
                    let len = (k - 1) * d + 1 + rng.below(d);
                    let frame = rng.bytes(len);
                    let (kk, s, syms) = encode_frame_with_d(&frame, k, d, r);
                    assert_eq!(kk, k);
                    for _ in 0..4 {
                        let n = rng.below(r + 1);
                        let erased = pick(&mut rng, k + r, n);
                        assert_eq!(recover_frame(&mut dec, k, r, s, &syms, &erased).unwrap(), frame, "k={k} r={r} 擦除 {n}");
                    }
                    let erased = pick(&mut rng, k + r, r + 1);
                    assert!(matches!(recover_frame(&mut dec, k, r, s, &syms, &erased), Err(FecError::NotEnough { .. })));
                }
            }
        }
    }

    /// 指定 k、d 编码（d 可以小于 1186，用来造大 k 的小帧）
    fn encode_frame_with_d(frame: &[u8], k: usize, d: usize, r: usize) -> (usize, usize, Vec<Vec<u8>>) {
        assert!(frame.len() <= k * d && frame.len() > (k - 1) * d);
        let s = shard_len(d);
        let mut buf = vec![0u8; (k + r) * s];
        pack_source(frame, k, d, s, &mut buf[..k * s]).unwrap();
        let (src, par) = buf.split_at_mut(k * s);
        RsEnc::new().encode(k, r, s, src, par).unwrap();
        (k, s, buf.chunks_exact(s).map(|c| c.to_vec()).collect())
    }

    /// 从 0..n 里不重复地挑 m 个
    fn pick(rng: &mut Rng, n: usize, m: usize) -> Vec<usize> {
        let mut all: Vec<usize> = (0..n).collect();
        for i in 0..m {
            let j = i + rng.below(n - i);
            all.swap(i, j);
        }
        all.truncate(m);
        all
    }

    /// T19：k ≤ 8 时穷举所有不超过 R 个的擦除组合
    #[test]
    fn t19_exhaustive_small_k() {
        let mut rng = Rng(0xFEC0_0008);
        let mut dec = RsDec::new();
        for k in 1..=8usize {
            for r in 1..=4usize {
                let d = 40;
                let len = (k - 1) * d + 1 + rng.below(d);
                let frame = rng.bytes(len);
                let (_, s, syms) = encode_frame_with_d(&frame, k, d, r);
                let n = k + r;
                for mask in 0u32..(1 << n) {
                    let erased: Vec<usize> = (0..n).filter(|i| mask & (1 << i) != 0).collect();
                    let res = recover_frame(&mut dec, k, r, s, &syms, &erased);
                    if erased.len() <= r {
                        assert_eq!(res.unwrap(), frame, "k={k} r={r} 擦除 {erased:?}");
                    } else {
                        assert!(matches!(res, Err(FecError::NotEnough { .. })));
                    }
                }
            }
        }
    }

    #[test]
    fn duplicates_and_bad_inputs() {
        let frame: Vec<u8> = (0..2500u32).map(|i| (i % 251) as u8).collect();
        let (k, s, syms) = encode_frame(&frame, 2);
        assert_eq!(k, 3);
        let mut dec = RsDec::new();
        // 同一个下标给两次只算一次：0、0、3 不够 3 个
        let have = [(0u16, syms[0].as_slice()), (0, syms[0].as_slice()), (3, syms[3].as_slice())];
        let mut out = vec![0u8; k * s];
        assert!(matches!(dec.recover(k, 2, s, have.into_iter(), &mut out), Err(FecError::NotEnough { have: 2, k: 3 })));
        // 下标越界、长度不对
        let have = [(5u16, syms[0].as_slice())];
        assert!(matches!(dec.recover(k, 2, s, have.into_iter(), &mut out), Err(FecError::BadParams(_))));
        let have = [(0u16, &syms[0][..s - 2])];
        assert!(matches!(dec.recover(k, 2, s, have.into_iter(), &mut out), Err(FecError::BadParams(_))));
        // k、R 越界
        assert!(check_params(0, 1, 4).is_err());
        assert!(check_params(1025, 1, 4).is_err());
        assert!(check_params(1024, 1024, 4).is_err());
        assert!(check_params(1024, 1023, 4).is_ok());
        // len 字段比 S − 2 大
        let mut bad = vec![0u8; 6];
        bad[..2].copy_from_slice(&5u16.to_be_bytes());
        assert!(shard_data(&bad).is_none());
        assert!(unpack_source(&bad, 1, 6, &mut Vec::new()).is_err());
    }
}
