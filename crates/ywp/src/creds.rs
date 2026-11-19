//! keys/ 目录的文件格式和加载、已配对客户端列表、配对串编解码（A 组）。
//! 网关的 pairing 子命令、fakegw、客户端的 `pair import` 共用这一份。
//!
//! - 目录不是 700、文件不是 600 就拒绝，不自动改权限；
//! - PSK、私钥、reset_key 不进日志、不进 Debug；客户端在日志里只记名字和 [`ClientId`]。
//!
//! TODO(A 组)：keys/ 里的文件名和编码（文件格式在附录 C 里是 `#[non_exhaustive]`）、
//! 以及 keygen、pair add / remove 要用的写入接口，由 A 组定，签名改动找协调人。

use crate::crypto::{Secret32, StaticKeypair};
use std::fmt;
use std::path::{Path, PathBuf};

/// 一个已配对的客户端
#[derive(Clone)]
pub struct ClientEntry {
    pub name: String,
    pub public: [u8; 32],
    pub psk: Secret32,
}

impl fmt::Debug for ClientEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // TODO(A 组)：ClientId::of 实现后改成打印指纹
        f.debug_struct("ClientEntry").field("name", &self.name).field("psk", &self.psk).finish_non_exhaustive()
    }
}

/// 客户端身份指纹：BLAKE2s(公钥) 的前 4 B，日志里显示成 8 位十六进制
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientId(pub [u8; 4]);

impl ClientId {
    pub fn of(public: &[u8; 32]) -> Self {
        todo!()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// 网关的 keys/：静态密钥、reset_key、已配对客户端
#[derive(Clone, Debug)]
pub struct Keys {
    pub static_kp: StaticKeypair,
    pub reset_key: Secret32,
    pub clients: Vec<ClientEntry>,
}

/// 客户端连一个网关要用的凭据
#[derive(Clone, Debug)]
pub struct ClientCreds {
    pub static_kp: StaticKeypair,
    pub gw_pub: [u8; 32],
    pub psk: Secret32,
}

/// 配对串解出来的内容（含 PSK：不回显、不进日志）
#[derive(Clone, Debug)]
pub struct PairingInfo {
    pub gw_pub: [u8; 32],
    pub psk: Secret32,
    /// 入口提示（比如 "192.0.2.10:9420"），可选
    pub entry_hint: Option<String>,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum KeyError {
    Io(std::io::Error),
    /// 目录不是 700 或文件不是 600（不自动改）
    BadPermissions { path: PathBuf, mode: u32 },
    /// 文件内容格式不对
    BadFormat { path: PathBuf, what: &'static str },
    /// 配对串格式不对
    BadPairing(&'static str),
    /// 缺少必需的文件
    Missing(PathBuf),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Io(e) => write!(f, "读写失败：{e}"),
            KeyError::BadPermissions { path, mode } => write!(f, "{} 的权限是 {mode:o}，要求目录 700、文件 600", path.display()),
            KeyError::BadFormat { path, what } => write!(f, "{} 格式不对：{what}", path.display()),
            KeyError::BadPairing(what) => write!(f, "配对串格式不对：{what}"),
            KeyError::Missing(path) => write!(f, "缺少 {}", path.display()),
        }
    }
}

impl std::error::Error for KeyError {}

impl From<std::io::Error> for KeyError {
    fn from(e: std::io::Error) -> Self {
        KeyError::Io(e)
    }
}

/// 加载 keys/：目录不是 700、文件不是 600 就拒绝，不自动改权限
pub fn load_keys(dir: &Path) -> Result<Keys, KeyError> {
    todo!()
}

/// 编码配对串（含 PSK）。调用方负责不让它进终端和日志（计划 §7.10）
pub fn encode_pairing(gw_pub: &[u8; 32], psk: &Secret32, entry_hint: Option<&str>) -> String {
    todo!()
}

pub fn decode_pairing(s: &str) -> Result<PairingInfo, KeyError> {
    todo!()
}
