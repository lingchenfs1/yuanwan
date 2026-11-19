//! keys/ 目录的文件格式和加载、已配对客户端列表、配对串编解码（A 组）。
//! 网关的 pairing 子命令、fakegw、客户端的 `pair import` 共用这一份。
//!
//! - 目录、文件的组和其他用户不能有任何权限（目录 700、文件 600，或更严），否则拒绝，不自动改权限；
//! - PSK、私钥、reset_key 不进日志、不进 Debug；客户端在日志里只记名字和 [`ClientId`]。
//!
//! keys/ 的布局（文件格式还可能加内容，见各常量）：
//!
//! ```text
//! keys/            700
//!   gateway.key    600  网关 X25519 静态私钥：64 位十六进制一行，# 开头的行是注释
//!   reset.key      600  gw_reset_key（算 RESET 令牌）：同上
//!   clients        600  已配对客户端，一行一个：<名字> <公钥 64 位十六进制> <PSK 64 位十六进制>
//! ```
//!
//! 名字 1~32 B、不含空白；名字和公钥都不能重复。clients 不存在等于没有客户端。
//!
//! 配对串（网关 `pair add` 输出、客户端 `pair import` 从 stdin 读）：
//! `ywp1-` + base64url（不补 =）编码的 `网关公钥 32 B ‖ PSK 32 B ‖ 入口提示（UTF-8，0~255 B）‖ 校验 4 B`，
//! 校验 = BLAKE2s-256("yw-pair" ‖ 前面全部字节) 的前 4 B，用来发现复制粘贴出错。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::consts::CLIENT_ID_LEN;
use crate::crypto::mac::blake2s256;
use crate::crypto::{wipe, Secret32, StaticKeypair};
use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 网关静态私钥的文件名
pub const GATEWAY_KEY_FILE: &str = "gateway.key";
/// gw_reset_key 的文件名
pub const RESET_KEY_FILE: &str = "reset.key";
/// 已配对客户端列表的文件名
pub const CLIENTS_FILE: &str = "clients";
/// 客户端名字的长度上限（和 HS_INIT 的 TLV 0x07 一致）
pub const CLIENT_NAME_MAX: usize = crate::consts::hs_tlv::NAME_MAX;
/// 配对串的前缀（带格式版本）
pub const PAIRING_PREFIX: &str = "ywp1-";
const PAIRING_CHECK_LABEL: &[u8] = b"yw-pair";
const PAIRING_CHECK_LEN: usize = 4;
const PAIRING_HINT_MAX: usize = 255;

/// 抹掉读进来或拼出来的明文（PSK、私钥的十六进制）
fn wipe_string(s: &mut str) {
    // SAFETY: 只把字节改成 0（ASCII NUL），改完仍是合法 UTF-8
    wipe(unsafe { s.as_bytes_mut() });
}

/// 一个已配对的客户端
#[derive(Clone)]
pub struct ClientEntry {
    pub name: String,
    pub public: [u8; 32],
    pub psk: Secret32,
}

impl fmt::Debug for ClientEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientEntry")
            .field("name", &self.name)
            .field("id", &ClientId::of(&self.public).to_string())
            .field("psk", &self.psk)
            .finish()
    }
}

/// 客户端身份指纹：BLAKE2s(公钥) 的前 4 B，日志里显示成 8 位十六进制
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientId(pub [u8; 4]);

impl ClientId {
    pub fn of(public: &[u8; 32]) -> Self {
        let h = blake2s256(&[public]);
        let mut id = [0u8; CLIENT_ID_LEN];
        id.copy_from_slice(&h[..CLIENT_ID_LEN]);
        Self(id)
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
    /// 名字或公钥和已配对的客户端重复、名字不合法
    Conflict(&'static str),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Io(e) => write!(f, "读写失败：{e}"),
            KeyError::BadPermissions { path, mode } => {
                write!(f, "{} 的权限是 {mode:o}，要求目录 700、文件 600", path.display())
            }
            KeyError::BadFormat { path, what } => write!(f, "{} 格式不对：{what}", path.display()),
            KeyError::BadPairing(what) => write!(f, "配对串格式不对：{what}"),
            KeyError::Missing(path) => write!(f, "缺少 {}", path.display()),
            KeyError::Conflict(what) => write!(f, "{what}"),
        }
    }
}

impl std::error::Error for KeyError {}

impl From<std::io::Error> for KeyError {
    fn from(e: std::io::Error) -> Self {
        KeyError::Io(e)
    }
}

// ======================================================================
// 十六进制、权限、单个密钥文件
// ======================================================================

/// 小写十六进制
pub fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// 64 位十六进制（大小写都行）→ 32 B
pub fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.as_bytes();
    if s.len() != 64 {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = nib(s[2 * i])? << 4 | nib(s[2 * i + 1])?;
    }
    Some(out)
}

fn mode_of(path: &Path) -> Result<u32, KeyError> {
    match fs::metadata(path) {
        Ok(m) => Ok(m.permissions().mode() & 0o7777),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(KeyError::Missing(path.to_path_buf())),
        Err(e) => Err(KeyError::Io(e)),
    }
}

/// 目录：组和其他用户不能有任何权限（700 或更严）
pub fn check_dir_perms(dir: &Path) -> Result<(), KeyError> {
    let mode = mode_of(dir)?;
    if !dir.is_dir() || mode & 0o077 != 0 {
        return Err(KeyError::BadPermissions { path: dir.to_path_buf(), mode });
    }
    Ok(())
}

/// 文件：组和其他用户不能有任何权限（600 或更严）
pub fn check_file_perms(path: &Path) -> Result<(), KeyError> {
    let mode = mode_of(path)?;
    if !path.is_file() || mode & 0o077 != 0 {
        return Err(KeyError::BadPermissions { path: path.to_path_buf(), mode });
    }
    Ok(())
}

/// 读一个密钥文件：先查 600，再取第一个非注释行的 64 位十六进制
pub fn read_key_file(path: &Path) -> Result<Secret32, KeyError> {
    check_file_perms(path)?;
    let mut text = fs::read_to_string(path)?;
    let parsed = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .and_then(parse_hex32)
        .map(Secret32::new);
    wipe_string(&mut text);
    parsed.ok_or(KeyError::BadFormat { path: path.to_path_buf(), what: "要求 64 位十六进制" })
}

/// 新建一个 600 的文件写入内容；已存在返回 Io(AlreadyExists)
fn write_new_600(path: &Path, contents: &[u8]) -> Result<(), KeyError> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
    f.write_all(contents)?;
    f.sync_all()?;
    Ok(())
}

/// 写一个新的密钥文件（600，不覆盖已有的）。comment 是第一行的注释，不要放秘密
pub fn write_key_file(path: &Path, key: &Secret32, comment: &str) -> Result<(), KeyError> {
    let mut text = format!("# {comment}\n{}\n", to_hex(key.expose()));
    let r = write_new_600(path, text.as_bytes());
    wipe_string(&mut text);
    r
}

// ======================================================================
// 网关的 keys/
// ======================================================================

/// keygen：建 keys/（不存在时建成 700；已存在必须是 700）、gateway.key、reset.key（600）。
/// gateway.key 已存在时返回 Io(AlreadyExists)，不覆盖。返回网关公钥
pub fn init_keys(dir: &Path) -> Result<[u8; 32], KeyError> {
    if !dir.exists() {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    check_dir_perms(dir)?;
    let kp = StaticKeypair::generate();
    write_key_file(&dir.join(GATEWAY_KEY_FILE), kp.secret(), "yw 网关静态私钥（X25519），不要外传")?;
    let reset_path = dir.join(RESET_KEY_FILE);
    if !reset_path.exists() {
        write_key_file(&reset_path, &Secret32::random(), "yw 网关 reset_key（算 RESET 令牌），不要外传")?;
    }
    Ok(kp.public())
}

/// 加载 keys/：目录不是 700、文件不是 600 就拒绝，不自动改权限
pub fn load_keys(dir: &Path) -> Result<Keys, KeyError> {
    check_dir_perms(dir)?;
    let sk = read_key_file(&dir.join(GATEWAY_KEY_FILE))?;
    let reset_key = read_key_file(&dir.join(RESET_KEY_FILE))?;
    let static_kp = StaticKeypair::from_secret(*sk.expose());
    let clients = load_clients(dir)?;
    Ok(Keys { static_kp, reset_key, clients })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= CLIENT_NAME_MAX && !name.chars().any(char::is_whitespace)
}

/// 读已配对客户端列表（clients 文件不存在返回空列表）。目录、文件权限照样检查
pub fn load_clients(dir: &Path) -> Result<Vec<ClientEntry>, KeyError> {
    check_dir_perms(dir)?;
    let path = dir.join(CLIENTS_FILE);
    if !path.exists() {
        return Ok(Vec::new());
    }
    check_file_perms(&path)?;
    let mut text = fs::read_to_string(&path)?;
    let r = parse_clients(&text, &path);
    wipe_string(&mut text);
    r
}

fn parse_clients(text: &str, path: &Path) -> Result<Vec<ClientEntry>, KeyError> {
    let bad = |what| KeyError::BadFormat { path: path.to_path_buf(), what };
    let mut out: Vec<ClientEntry> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(name), Some(public), Some(psk), None) = (it.next(), it.next(), it.next(), it.next()) else {
            return Err(bad("每行要求「名字 公钥 PSK」三项"));
        };
        if !valid_name(name) {
            return Err(bad("名字要求 1~32 B、不含空白"));
        }
        let public = parse_hex32(public).ok_or_else(|| bad("公钥要求 64 位十六进制"))?;
        let psk = parse_hex32(psk).map(Secret32::new).ok_or_else(|| bad("PSK 要求 64 位十六进制"))?;
        if out.iter().any(|c| c.name == name || c.public == public) {
            return Err(bad("名字或公钥重复"));
        }
        out.push(ClientEntry { name: name.to_owned(), public, psk });
    }
    Ok(out)
}

/// 整个替换 clients 文件（先写临时文件 600，再改名，中途失败不留半个文件）
pub fn save_clients(dir: &Path, clients: &[ClientEntry]) -> Result<(), KeyError> {
    check_dir_perms(dir)?;
    let mut text = String::from("# 已配对客户端：名字 公钥 PSK（PSK 不要外传）\n");
    for c in clients {
        if !valid_name(&c.name) {
            return Err(KeyError::Conflict("名字要求 1~32 B、不含空白"));
        }
        text.push_str(&format!("{} {} {}\n", c.name, to_hex(&c.public), to_hex(c.psk.expose())));
    }
    let tmp = dir.join(format!("{CLIENTS_FILE}.tmp"));
    let _ = fs::remove_file(&tmp);
    let r = write_new_600(&tmp, text.as_bytes())
        .and_then(|_| fs::rename(&tmp, dir.join(CLIENTS_FILE)).map_err(Into::into));
    wipe_string(&mut text);
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

/// pair add：新配对一个客户端，生成 PSK 并写进 clients，返回 PSK（调用方拿它和网关公钥编配对串）
pub fn add_client(dir: &Path, name: &str, public: &[u8; 32]) -> Result<Secret32, KeyError> {
    if !valid_name(name) {
        return Err(KeyError::Conflict("名字要求 1~32 B、不含空白"));
    }
    let mut clients = load_clients(dir)?;
    if clients.iter().any(|c| c.name == name) {
        return Err(KeyError::Conflict("这个名字已经配对过"));
    }
    if clients.iter().any(|c| &c.public == public) {
        return Err(KeyError::Conflict("这个公钥已经配对过"));
    }
    let psk = Secret32::random();
    clients.push(ClientEntry { name: name.to_owned(), public: *public, psk: psk.clone() });
    save_clients(dir, &clients)?;
    Ok(psk)
}

/// pair remove：按名字或指纹（8 位十六进制）删除，返回删掉的那个的名字；没找到返回 None
pub fn remove_client(dir: &Path, name_or_id: &str) -> Result<Option<String>, KeyError> {
    let mut clients = load_clients(dir)?;
    let Some(i) = clients.iter().position(|c| c.name == name_or_id || ClientId::of(&c.public).to_string() == name_or_id)
    else {
        return Ok(None);
    };
    let removed = clients.remove(i);
    save_clients(dir, &clients)?;
    Ok(Some(removed.name))
}

// ======================================================================
// 配对串
// ======================================================================

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn b64_encode(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            s.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
        }
    }
    s
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| B64.iter().position(|&b| b == c).map(|v| v as u32);
    let s = s.as_bytes();
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3 + 2);
    for c in s.chunks(4) {
        let mut n = 0u32;
        for (i, &ch) in c.iter().enumerate() {
            n |= val(ch)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        let keep = c.len() - 1;
        // 末组多出来的位必须是 0（只认规范写法）
        if bytes[1 + keep..].iter().any(|&b| b != 0) {
            return None;
        }
        out.extend_from_slice(&bytes[1..1 + keep]);
    }
    Some(out)
}

/// 编码配对串（含 PSK）。调用方负责不让它进终端和日志（写 600 文件或经管道交给 `pair import`）。
/// 入口提示超过 255 B 时截到字符边界
pub fn encode_pairing(gw_pub: &[u8; 32], psk: &Secret32, entry_hint: Option<&str>) -> String {
    let hint = entry_hint.unwrap_or("");
    let mut end = hint.len().min(PAIRING_HINT_MAX);
    while !hint.is_char_boundary(end) {
        end -= 1;
    }
    let mut blob = Vec::with_capacity(64 + end + PAIRING_CHECK_LEN);
    blob.extend_from_slice(gw_pub);
    blob.extend_from_slice(psk.expose());
    blob.extend_from_slice(&hint.as_bytes()[..end]);
    let check = blake2s256(&[PAIRING_CHECK_LABEL, &blob]);
    blob.extend_from_slice(&check[..PAIRING_CHECK_LEN]);
    let s = format!("{PAIRING_PREFIX}{}", b64_encode(&blob));
    wipe(&mut blob);
    s
}

/// 解配对串（前后的空白忽略）。错误信息里不带配对串本身
pub fn decode_pairing(s: &str) -> Result<PairingInfo, KeyError> {
    let s = s.trim();
    let body = s.strip_prefix(PAIRING_PREFIX).ok_or(KeyError::BadPairing("开头不是 ywp1-"))?;
    let mut blob = b64_decode(body).ok_or(KeyError::BadPairing("不是合法的 base64url"))?;
    let r = (|| {
        if blob.len() < 64 + PAIRING_CHECK_LEN || blob.len() > 64 + PAIRING_HINT_MAX + PAIRING_CHECK_LEN {
            return Err(KeyError::BadPairing("长度不对"));
        }
        let (data, check) = blob.split_at(blob.len() - PAIRING_CHECK_LEN);
        let want = blake2s256(&[PAIRING_CHECK_LABEL, data]);
        if check != &want[..PAIRING_CHECK_LEN] {
            return Err(KeyError::BadPairing("校验不符，可能复制时漏了字符"));
        }
        let mut gw_pub = [0u8; 32];
        gw_pub.copy_from_slice(&data[..32]);
        let mut psk = [0u8; 32];
        psk.copy_from_slice(&data[32..64]);
        let psk_s = Secret32::new(psk);
        wipe(&mut psk);
        let hint = std::str::from_utf8(&data[64..]).map_err(|_| KeyError::BadPairing("入口提示不是 UTF-8"))?;
        Ok(PairingInfo { gw_pub, psk: psk_s, entry_hint: (!hint.is_empty()).then(|| hint.to_owned()) })
    })();
    wipe(&mut blob);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_roundtrip() {
        for n in 0..70 {
            let data: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            let s = b64_encode(&data);
            assert!(!s.contains('='));
            assert_eq!(b64_decode(&s).unwrap(), data, "长度 {n}");
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_encode(b"fo"), "Zm8");
        assert!(b64_decode("Zm9").is_none(), "末组多出的位不是 0");
        assert!(b64_decode("Z").is_none());
        assert!(b64_decode("Zm+v").is_none(), "不认标准 base64 的 +");
    }

    #[test]
    fn hex() {
        let b = [0xABu8; 32];
        assert_eq!(parse_hex32(&to_hex(&b)), Some(b));
        assert_eq!(parse_hex32(&to_hex(&b).to_uppercase()), Some(b));
        assert_eq!(parse_hex32("00"), None);
        assert_eq!(parse_hex32(&"g".repeat(64)), None);
    }
}
