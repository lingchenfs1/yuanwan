//! 大端字节游标：编码用 [`Writer`]，解码用 [`Reader`]。越界一律返回错误，不 panic。

use crate::error::WireError;

/// 往一段缓冲里顺序写大端整数和字节串
#[derive(Debug)]
pub struct Writer<'b> {
    buf: &'b mut [u8],
    pos: usize,
}

impl<'b> Writer<'b> {
    pub fn new(buf: &'b mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// 已写的字节数
    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn written(&self) -> &[u8] {
        &self.buf[..self.pos]
    }

    fn take(&mut self, n: usize) -> Result<&mut [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::NoRoom);
        }
        let s = &mut self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn put_u8(&mut self, v: u8) -> Result<(), WireError> {
        self.take(1)?[0] = v;
        Ok(())
    }

    pub fn put_u16(&mut self, v: u16) -> Result<(), WireError> {
        self.take(2)?.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }

    pub fn put_u32(&mut self, v: u32) -> Result<(), WireError> {
        self.take(4)?.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }

    pub fn put_u64(&mut self, v: u64) -> Result<(), WireError> {
        self.take(8)?.copy_from_slice(&v.to_be_bytes());
        Ok(())
    }

    pub fn put_slice(&mut self, s: &[u8]) -> Result<(), WireError> {
        self.take(s.len())?.copy_from_slice(s);
        Ok(())
    }

    pub fn put_zeros(&mut self, n: usize) -> Result<(), WireError> {
        self.take(n)?.fill(0);
        Ok(())
    }
}

/// 从一段字节里顺序读大端整数和字节串
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// 读 n 个字节，借用原缓冲
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], WireError> {
        if self.remaining() < n {
            return Err(WireError::Truncated);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    /// 剩下的全部字节
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }

    pub fn skip(&mut self, n: usize) -> Result<(), WireError> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.bytes(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, WireError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32, WireError> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, WireError> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], WireError> {
        let b = self.bytes(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(b);
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_bounds() {
        let mut buf = [0u8; 15];
        let mut w = Writer::new(&mut buf);
        w.put_u8(1).unwrap();
        w.put_u16(0x0203).unwrap();
        w.put_u32(0x0405_0607).unwrap();
        w.put_u64(0x0809_0A0B_0C0D_0E0F).unwrap();
        assert_eq!(w.put_u8(0), Err(WireError::NoRoom));
        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x0203);
        assert_eq!(r.u32().unwrap(), 0x0405_0607);
        assert_eq!(r.u64().unwrap(), 0x0809_0A0B_0C0D_0E0F);
        assert_eq!(r.u8(), Err(WireError::Truncated));
    }
}
