//! 外层头（14 B，明文，作为 AEAD 的关联数据）：Flags | CID 8 B | path_id | PN 低 32 位。
//! Flags = Ver(2) | Type(3) | K(1) | Class(2)，见规格 §5.1。

use crate::consts::{
    FLAGS_CLASS_MASK, FLAGS_K_BIT, FLAGS_TYPE_MASK, FLAGS_TYPE_SHIFT, FLAGS_VER, FLAGS_VER_SHIFT, HDR_LEN,
};
use crate::error::WireError;

/// 外层 Type
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketType {
    Data = 0,
    HsInit,
    HsResp,
    /// 后（cookie）
    HsCookie,
    /// 后（中转逐跳）
    Hop,
    /// 后（中转）
    RelayInit,
    Reset,
}

impl PacketType {
    /// 外层 Type 的 3 位取值；7 是保留值，返回 None
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => PacketType::Data,
            1 => PacketType::HsInit,
            2 => PacketType::HsResp,
            3 => PacketType::HsCookie,
            4 => PacketType::Hop,
            5 => PacketType::RelayInit,
            6 => PacketType::Reset,
            _ => return None,
        })
    }
}

/// Class：供中转排优先级、局域网打 DSCP；非 DATA 包填 0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    Bulk = 0,
    Video,
    Media,
    Input,
}

impl Class {
    /// 取低 2 位
    pub fn from_bits(v: u8) -> Self {
        match v & FLAGS_CLASS_MASK {
            0 => Class::Bulk,
            1 => Class::Video,
            2 => Class::Media,
            _ => Class::Input,
        }
    }
}

/// 连接 ID（客户端每次连接新选，握手的多次尝试共用）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cid(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OuterHeader {
    pub ptype: PacketType,
    /// 密钥相位，只对 DATA 有意义；v1 恒为 0
    pub key_phase: bool,
    pub class: Class,
    pub cid: Cid,
    pub path_id: u8,
    /// PN 的低 32 位（HS_INIT 填尝试序号，HS_RESP 回显 HS_INIT 的 PN，RESET 随机）
    pub pn_trunc: u32,
}

impl OuterHeader {
    /// 握手包和 RESET 用：K = 0、Class = 0
    pub fn new(ptype: PacketType, cid: Cid, path_id: u8, pn_trunc: u32) -> Self {
        Self { ptype, key_phase: false, class: Class::Bulk, cid, path_id, pn_trunc }
    }

    /// Flags 字节：Ver(7..6) | Type(5..3) | K(2) | Class(1..0)
    pub fn flags(&self) -> u8 {
        (FLAGS_VER << FLAGS_VER_SHIFT)
            | ((self.ptype as u8 & FLAGS_TYPE_MASK) << FLAGS_TYPE_SHIFT)
            | if self.key_phase { FLAGS_K_BIT } else { 0 }
            | (self.class as u8 & FLAGS_CLASS_MASK)
    }

    /// 写进 out 的前 14 B（调用方保证 out 至少 14 B，不够会 panic：这是编程错误，不是网络输入）
    pub fn encode(&self, out: &mut [u8]) {
        let out = &mut out[..HDR_LEN];
        out[0] = self.flags();
        out[1..9].copy_from_slice(&self.cid.0.to_be_bytes());
        out[9] = self.path_id;
        out[10..14].copy_from_slice(&self.pn_trunc.to_be_bytes());
    }

    /// 解前 14 B：长度不够返回 Short，Ver ≠ 01 返回 BadVersion，Type = 7 返回 ReservedType
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        if b.len() < HDR_LEN {
            return Err(WireError::Short);
        }
        let flags = b[0];
        if flags >> FLAGS_VER_SHIFT != FLAGS_VER {
            return Err(WireError::BadVersion);
        }
        let ptype =
            PacketType::from_u8((flags >> FLAGS_TYPE_SHIFT) & FLAGS_TYPE_MASK).ok_or(WireError::ReservedType)?;
        let mut cid = [0u8; 8];
        cid.copy_from_slice(&b[1..9]);
        Ok(Self {
            ptype,
            key_phase: flags & FLAGS_K_BIT != 0,
            class: Class::from_bits(flags),
            cid: Cid(u64::from_be_bytes(cid)),
            path_id: b[9],
            pn_trunc: u32::from_be_bytes([b[10], b[11], b[12], b[13]]),
        })
    }

    /// 不做其他校验，只取 CID（网关按 CID 找会话）。不足 14 B 返回 None
    pub fn peek_cid(b: &[u8]) -> Option<Cid> {
        if b.len() < HDR_LEN {
            return None;
        }
        let mut cid = [0u8; 8];
        cid.copy_from_slice(&b[1..9]);
        Some(Cid(u64::from_be_bytes(cid)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::{FLAGS_HS_INIT, FLAGS_HS_RESP, FLAGS_RESET};

    #[test]
    fn fixed_flags() {
        let cid = Cid(0x0102_0304_0506_0708);
        assert_eq!(OuterHeader::new(PacketType::HsInit, cid, 0, 0).flags(), FLAGS_HS_INIT);
        assert_eq!(OuterHeader::new(PacketType::HsResp, cid, 0, 0).flags(), FLAGS_HS_RESP);
        assert_eq!(OuterHeader::new(PacketType::Reset, cid, 0, 0).flags(), FLAGS_RESET);
        let data = OuterHeader { ptype: PacketType::Data, key_phase: true, class: Class::Input, cid, path_id: 3, pn_trunc: 9 };
        assert_eq!(data.flags(), 0x47);
        let mut b = [0u8; HDR_LEN];
        data.encode(&mut b);
        assert_eq!(b, [0x47, 1, 2, 3, 4, 5, 6, 7, 8, 3, 0, 0, 0, 9]);
        assert_eq!(OuterHeader::decode(&b).unwrap(), data);
        assert_eq!(OuterHeader::peek_cid(&b), Some(cid));
    }
}
