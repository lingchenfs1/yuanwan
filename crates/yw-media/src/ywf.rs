//! 录像：`.ywf`（M2e，逐帧带时间元数据）和按 AU 切分 M1 录的 `.h265`（M2a，fakegw 和 FakePs5 共用）。
//!
//! TODO(C 组)：`.ywf` 的字节格式 M2e 定；时间字段要注明来源（交帧时刻还是 0013 的收包时刻），在 flags 里标出。

use crate::nal::{self, Codec};
use std::collections::VecDeque;
use std::io::{self, Read, Write};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YwfFrame {
    pub frame_index: u16,
    pub first_age_us: u32,
    pub span_us: u32,
    pub key: bool,
    pub recovered: bool,
    pub data: Vec<u8>,
}

// 骨架（M2e 实现时去掉）
#[allow(dead_code)]
pub struct YwfWriter {
    w: Box<dyn Write + Send>,
}

#[allow(unused_variables)] // 骨架（M2e 实现时去掉）
impl YwfWriter {
    pub fn new(w: impl Write + Send + 'static) -> io::Result<Self> {
        todo!()
    }

    pub fn write_frame(&mut self, f: &YwfFrame) -> io::Result<()> {
        todo!()
    }
}

// 骨架（M2e 实现时去掉）
#[allow(dead_code)]
pub struct YwfReader {
    r: Box<dyn Read + Send>,
}

#[allow(unused_variables)] // 骨架（M2e 实现时去掉）
impl YwfReader {
    pub fn new(r: impl Read + Send + 'static) -> io::Result<Self> {
        todo!()
    }

    pub fn next_frame(&mut self) -> io::Result<Option<YwfFrame>> {
        todo!()
    }
}

/// 每次从文件读这么多
const READ_CHUNK: usize = 64 << 10;

/// 按 AU 切分 M1 `record` 录下的 .h265（带参数集头）。
///
/// - 文件格式：开头是参数集（VPS/SPS/PPS，和 PS5 会话库单独交出的那份一样），之后是一帧接一帧的 Annex-B；
/// - AU 的切分按 H.265 §7.4.2.4.4 / H.264 §7.4.1.2.3：前面已有 slice 时，遇到参数集、AUD、前缀 SEI 等
///   非 VCL NAL，或者一帧的第一个 slice，就开始新的 AU；
/// - 输出的每个 NAL 前面都是 4 字节起始码（和会话库交出的格式一致；文件本身用 4 字节起始码时逐字节相同）；
/// - 开头的参数集在 `new` 里读掉，由 [`param_sets`](Self::param_sets) 给出，不经 `next_au` 返回；
///   文件中途再出现的参数集（例如拼接的录像）单独作为一个只含参数集的「AU」返回，
///   和会话库交付参数集的方式一致，同时更新 `param_sets`。
///
/// `new` 不返回错误：读头时出的 I/O 错误留到第一次 `next_au` 再报。
pub struct AnnexBReader<R: Read> {
    r: R,
    codec: Codec,
    /// 未处理的字节；已找到第一个起始码之后，buf 从一个 NAL 的内容开头算起
    buf: Vec<u8>,
    /// buf 里已经确认没有起始码的前缀长度（下次从这里接着找，留出 2 B 跨块）
    scanned: usize,
    started: bool,
    eof: bool,
    params: Vec<u8>,
    /// 读到但还属于下一个 AU 的 NAL
    peeked: Option<Vec<u8>>,
    /// 已经切好、还没交出去的 AU
    pending: VecDeque<Vec<u8>>,
    err: Option<io::Error>,
}

impl<R: Read> AnnexBReader<R> {
    pub fn new(r: R, c: Codec) -> Self {
        let mut me = Self {
            r,
            codec: c,
            buf: Vec::new(),
            scanned: 0,
            started: false,
            eof: false,
            params: Vec::new(),
            peeked: None,
            pending: VecDeque::new(),
            err: None,
        };
        match me.read_unit() {
            Ok(Some(u)) if nal::is_param_sets_only(&u, c) => {}
            Ok(Some(u)) => me.pending.push_back(u),
            Ok(None) => {}
            Err(e) => me.err = Some(e),
        }
        me
    }

    /// 文件开头的参数集（文件中途换了参数集时是最新的那份）
    pub fn param_sets(&self) -> &[u8] {
        &self.params
    }

    pub fn next_au(&mut self) -> io::Result<Option<Vec<u8>>> {
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        if let Some(u) = self.pending.pop_front() {
            return Ok(Some(u));
        }
        self.read_unit()
    }

    /// 跳到下一个关键帧：之后的 `next_au` 从关键帧开始（途中的参数集照样保留）。读到文件尾就停在文件尾
    pub fn seek_next_key(&mut self) -> io::Result<()> {
        if let Some(e) = self.err.take() {
            return Err(e);
        }
        let mut keep = VecDeque::new();
        loop {
            let u = match self.pending.pop_front() {
                Some(u) => u,
                None => match self.read_unit()? {
                    Some(u) => u,
                    None => break,
                },
            };
            if nal::is_param_sets_only(&u, self.codec) {
                keep.push_back(u);
            } else if nal::is_keyframe(&u, self.codec) {
                keep.push_back(u);
                break;
            }
        }
        keep.append(&mut self.pending);
        self.pending = keep;
        Ok(())
    }

    /// 切出下一个 AU（或一段只含参数集的数据，同时更新 params）
    fn read_unit(&mut self) -> io::Result<Option<Vec<u8>>> {
        let c = self.codec;
        let mut au = Vec::new();
        let mut has_vcl = false;
        let mut only_params = true;
        loop {
            let nal = match self.peeked.take() {
                Some(n) => n,
                None => match self.next_nal()? {
                    Some(n) => n,
                    None => break,
                },
            };
            let t = nal::nal_type(&nal, c).unwrap_or(0);
            let param = nal::is_param_set(t, c);
            let new_au = has_vcl && nal::starts_new_au(&nal, c);
            // 参数集单独成段：段里只有参数集，而下一个 NAL 不是参数集
            let end_params = !au.is_empty() && only_params && !param;
            if new_au || end_params {
                self.peeked = Some(nal);
                break;
            }
            au.extend_from_slice(&nal::START_CODE);
            au.extend_from_slice(&nal);
            has_vcl |= nal::is_vcl(t, c);
            only_params &= param;
        }
        if au.is_empty() {
            return Ok(None);
        }
        if only_params {
            self.params.clone_from(&au);
        }
        Ok(Some(au))
    }

    /// 下一个非空 NAL（不含起始码，末尾的 0 已去掉）
    fn next_nal(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if !self.started {
                if let Some((_, after)) = nal::find_start(&self.buf) {
                    self.buf.drain(..after);
                    self.started = true;
                    self.scanned = 0;
                    continue;
                }
                if self.eof {
                    self.buf.clear();
                    return Ok(None);
                }
                // 留 2 B，起始码可能跨块
                let keep = self.buf.len().min(2);
                self.buf.drain(..self.buf.len() - keep);
                self.fill()?;
                continue;
            }
            let from = self.scanned;
            if let Some((len, after)) = nal::find_start(&self.buf[from..]) {
                let nal = nal::trim_zeros(&self.buf[..from + len]).to_vec();
                self.buf.drain(..from + after);
                self.scanned = 0;
                if nal.is_empty() {
                    continue;
                }
                return Ok(Some(nal));
            }
            if self.eof {
                let nal = nal::trim_zeros(&self.buf).to_vec();
                self.buf.clear();
                self.scanned = 0;
                if nal.is_empty() {
                    return Ok(None);
                }
                return Ok(Some(nal));
            }
            self.scanned = self.buf.len().saturating_sub(2);
            self.fill()?;
        }
    }

    /// 再读一块；读到文件尾时置 eof
    fn fill(&mut self) -> io::Result<()> {
        let old = self.buf.len();
        self.buf.resize(old + READ_CHUNK, 0);
        let n = loop {
            match self.r.read(&mut self.buf[old..]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    self.buf.truncate(old);
                    return Err(e);
                }
            }
        };
        self.buf.truncate(old + n);
        if n == 0 {
            self.eof = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nal::START_CODE;

    /// 每次最多给 n 字节的读取器，用来测跨块的起始码
    struct Trickle<'a> {
        data: &'a [u8],
        n: usize,
        i: usize,
    }

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.i += 1;
            let n = self.data.len().min(buf.len()).min(1 + self.i % self.n);
            buf[..n].copy_from_slice(&self.data[..n]);
            self.data = &self.data[n..];
            Ok(n)
        }
    }

    const VPS: &[u8] = &[0x40, 0x01, 0x0c, 0x01];
    const SPS: &[u8] = &[0x42, 0x01, 0x01, 0x22];
    const PPS: &[u8] = &[0x44, 0x01, 0xc1, 0x72];
    const SEI: &[u8] = &[0x4e, 0x01, 0x05, 0x10];

    fn idr(tag: u8) -> Vec<u8> {
        vec![0x28, 0x01, 0xaf, tag, 0x00, 0x00, 0x03, 0x01, 0x80]
    }

    fn p(tag: u8) -> Vec<u8> {
        vec![0x02, 0x01, 0xd0, tag, 0x34]
    }

    /// 同一帧的第二个 slice（first_slice_segment_in_pic_flag = 0）
    fn p2(tag: u8) -> Vec<u8> {
        vec![0x02, 0x01, 0x50, tag]
    }

    fn annexb(nals: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in nals {
            v.extend_from_slice(&START_CODE);
            v.extend_from_slice(n);
        }
        v
    }

    fn read_all<R: Read>(r: &mut AnnexBReader<R>) -> Vec<Vec<u8>> {
        let mut v = Vec::new();
        while let Some(au) = r.next_au().unwrap() {
            v.push(au);
        }
        v
    }

    #[test]
    fn splits_recording_into_aus() {
        let (i1, p1, p2a, p2b, p3, i2, p4) = (idr(1), p(1), p(2), p2(2), p(3), idr(2), p(4));
        let header = annexb(&[VPS, SPS, PPS]);
        let aus = [
            annexb(&[&i1]),
            annexb(&[&p1]),
            annexb(&[&p2a, &p2b]),
            annexb(&[SEI, &p3]),
            annexb(&[VPS, SPS, PPS]),
            annexb(&[&i2]),
            annexb(&[&p4]),
        ];
        let mut file = header.clone();
        for a in &aus {
            file.extend_from_slice(a);
        }
        for n in [1usize, 2, 3, 7, 64] {
            let mut r = AnnexBReader::new(Trickle { data: &file, n, i: 0 }, Codec::Hevc);
            assert_eq!(r.param_sets(), &header[..]);
            let got = read_all(&mut r);
            assert_eq!(got, aus, "每次最多读 {n} B");
            assert_eq!(r.next_au().unwrap(), None);
        }
        // 3 字节起始码也能切，输出统一成 4 字节
        let three: Vec<usize> = file.windows(4).enumerate().filter(|(_, w)| *w == START_CODE).map(|(i, _)| i).collect();
        let mut f3 = file.clone();
        for i in three.into_iter().rev() {
            f3.remove(i);
        }
        let mut r = AnnexBReader::new(&f3[..], Codec::Hevc);
        assert_eq!(read_all(&mut r), aus);
    }

    #[test]
    fn seek_next_key_keeps_param_sets() {
        let (i1, p1, i2, p4) = (idr(1), p(1), idr(2), p(4));
        let mut file = annexb(&[VPS, SPS, PPS, &i1, &p1, &p1]);
        file.extend_from_slice(&annexb(&[VPS, SPS, PPS, &i2, &p4]));
        let mut r = AnnexBReader::new(&file[..], Codec::Hevc);
        assert_eq!(r.next_au().unwrap().unwrap(), annexb(&[&i1]));
        r.seek_next_key().unwrap();
        assert_eq!(r.next_au().unwrap().unwrap(), annexb(&[VPS, SPS, PPS]));
        assert_eq!(r.next_au().unwrap().unwrap(), annexb(&[&i2]));
        // 后面没有关键帧了：停在文件尾
        r.seek_next_key().unwrap();
        assert_eq!(r.next_au().unwrap(), None);
        // 已经停在关键帧上时 seek 不动
        let mut r = AnnexBReader::new(&file[..], Codec::Hevc);
        r.seek_next_key().unwrap();
        r.seek_next_key().unwrap();
        assert_eq!(r.next_au().unwrap().unwrap(), annexb(&[&i1]));
    }

    #[test]
    fn empty_and_garbage_input() {
        let mut r = AnnexBReader::new(&[][..], Codec::Hevc);
        assert!(r.param_sets().is_empty());
        assert_eq!(r.next_au().unwrap(), None);
        let mut r = AnnexBReader::new(&[1u8, 2, 3, 4, 5][..], Codec::Hevc);
        assert_eq!(r.next_au().unwrap(), None);
        // 没有参数集头，直接是帧
        let file = annexb(&[&p(1), &p(2)]);
        let mut r = AnnexBReader::new(&file[..], Codec::Hevc);
        assert!(r.param_sets().is_empty());
        assert_eq!(read_all(&mut r).len(), 2);
    }

    #[test]
    fn read_error_surfaces_on_first_next_au() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("坏了"))
            }
        }
        let mut r = AnnexBReader::new(Broken, Codec::Hevc);
        assert!(r.next_au().is_err());
    }

    /// 用真实录像做冒烟测试：YW_TEST_H265 指向一段 M1 `record` 录的 .h265，没设就跳过
    #[test]
    fn real_recording_smoke() {
        let Some(path) = std::env::var_os("YW_TEST_H265") else {
            eprintln!("没设 YW_TEST_H265，跳过");
            return;
        };
        let file = std::fs::read(&path).unwrap();
        let mut r = AnnexBReader::new(std::fs::File::open(&path).unwrap(), Codec::Hevc);
        assert!(nal::is_param_sets_only(r.param_sets(), Codec::Hevc));
        let mut joined = r.param_sets().to_vec();
        let (mut n, mut keys, mut max) = (0usize, 0usize, 0usize);
        while let Some(au) = r.next_au().unwrap() {
            if n == 0 {
                assert!(nal::is_keyframe(&au, Codec::Hevc), "录像从关键帧开始");
            }
            // T31：每帧 1 个 slice
            let vcl = nal::nal_units(&au).filter(|x| nal::nal_type(x, Codec::Hevc).is_some_and(|t| nal::is_vcl(t, Codec::Hevc))).count();
            assert_eq!(vcl, 1);
            keys += nal::is_keyframe(&au, Codec::Hevc) as usize;
            max = max.max(au.len());
            n += 1;
            joined.extend_from_slice(&au);
        }
        eprintln!("AU {n} 个，关键帧 {keys} 个，最大 {max} B，参数集 {} B", r.param_sets().len());
        assert!(n > 0);
        // 录像用 4 字节起始码：参数集 + 各 AU 拼回来和原文件逐字节一致
        assert_eq!(joined.len(), file.len());
        assert!(joined == file);
    }
}
