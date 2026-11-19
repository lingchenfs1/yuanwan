//! 录像：`.ywf`（M2e，逐帧带时间元数据）和按 AU 切分 M1 录的 `.h265`（M2a，fakegw 和 FakePs5 共用）。
//!
//! TODO(C 组)：`.ywf` 的字节格式 M2e 定；时间字段要注明来源（交帧时刻还是 0013 的收包时刻），在 flags 里标出。

use crate::nal::Codec;
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

pub struct YwfWriter {
    w: Box<dyn Write + Send>,
}

impl YwfWriter {
    pub fn new(w: impl Write + Send + 'static) -> io::Result<Self> {
        todo!()
    }

    pub fn write_frame(&mut self, f: &YwfFrame) -> io::Result<()> {
        todo!()
    }
}

pub struct YwfReader {
    r: Box<dyn Read + Send>,
}

impl YwfReader {
    pub fn new(r: impl Read + Send + 'static) -> io::Result<Self> {
        todo!()
    }

    pub fn next_frame(&mut self) -> io::Result<Option<YwfFrame>> {
        todo!()
    }
}

/// 按 AU 切分 M1 `record` 录下的 .h265（带参数集头）
pub struct AnnexBReader<R: Read> {
    r: R,
    codec: Codec,
}

impl<R: Read> AnnexBReader<R> {
    pub fn new(r: R, c: Codec) -> Self {
        todo!()
    }

    /// 文件开头的参数集
    pub fn param_sets(&self) -> &[u8] {
        todo!()
    }

    pub fn next_au(&mut self) -> io::Result<Option<Vec<u8>>> {
        todo!()
    }

    pub fn seek_next_key(&mut self) -> io::Result<()> {
        todo!()
    }
}
