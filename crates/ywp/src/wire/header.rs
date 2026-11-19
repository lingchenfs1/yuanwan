//! 外层头（14 B，明文，作为 AEAD 的关联数据）：Flags | CID 8 B | path_id | PN 低 32 位。
//! Flags = Ver(2) | Type(3) | K(1) | Class(2)，见规格 §5.1。

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

/// Class：供中转排优先级、局域网打 DSCP；非 DATA 包填 0
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    Bulk = 0,
    Video,
    Media,
    Input,
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
    /// 写进 out 的前 14 B（调用方保证 out 至少 14 B）
    pub fn encode(&self, out: &mut [u8]) {
        todo!()
    }

    /// 解前 14 B：长度不够返回 Short，Ver ≠ 01 返回 BadVersion，Type = 7 返回 ReservedType
    pub fn decode(b: &[u8]) -> Result<Self, WireError> {
        todo!()
    }

    /// 不做其他校验，只取 CID（网关按 CID 找会话）
    pub fn peek_cid(b: &[u8]) -> Option<Cid> {
        todo!()
    }
}
