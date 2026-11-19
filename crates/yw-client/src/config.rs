//! 客户端配置：网关档案、本机密钥、配对串导入。
//! 配置目录在 `~/Library/Application Support/yuanwan/`（不受 iCloud 同步），目录 700、文件一律 600；
//! 权限不对就拒绝读，不自动改（和网关的 keys/ 同一条规则）。
//!
//! 目录结构：
//! - `client.key`：本机静态私钥，一行 `secret=<64 位十六进制>`；
//! - `gateways/<名字>.conf`：一个网关一个档案，`key=value`（`#` 开头是注释），含 PSK。
//!
//! 环境变量 `YW_CONFIG_DIR` 可以换目录（测试脚本、同一台机器上跑多个客户端时用）。

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use ywp::crypto::{Secret32, StaticKeypair};
use yw_media::Mode;

/// 一个网关的连接档案。地址、网卡都从配置文件读，不写死
#[derive(Clone, Debug)]
pub struct GatewayProfile {
    pub name: String,
    pub gw_pub: [u8; 32],
    pub psk: Secret32,
    pub gateway_id: [u8; 8],
    /// 网关入口，例如 192.0.2.10:9420
    pub entry: SocketAddr,
    /// 绑哪块网卡（例如 en0）；None 表示不绑。默认路由走隧道网卡时会提示指定它
    pub bind_if: Option<String>,
    pub bitrate_cap_kbps: Option<u32>,
    pub mode: Mode,
    /// 覆盖 J（ms）
    pub j_ms: Option<u8>,
    /// 连上后自动发 PS5_CMD START
    pub auto_start: bool,
}

const KEY_FILE: &str = "client.key";
const GATEWAYS_DIR: &str = "gateways";
/// 配对串最长读多少（防止误把大文件导进来）
const PAIRING_MAX: u64 = 16 * 1024;

/// `~/Library/Application Support/yuanwan/`（`YW_CONFIG_DIR` 优先）
pub fn config_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("YW_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("yuanwan")
}

fn mode_bits(p: &Path) -> io::Result<u32> {
    Ok(fs::metadata(p)?.permissions().mode() & 0o777)
}

/// 建目录（700）；已经存在但权限不是 700 就报错，不自动改
pub fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("建目录 {} 失败：{e}", dir.display()))?;
    }
    check_private_dir(dir)
}

fn check_private_dir(dir: &Path) -> Result<(), String> {
    let m = mode_bits(dir).map_err(|e| format!("读不到 {}：{e}", dir.display()))?;
    if m != 0o700 {
        return Err(format!("{} 的权限是 {m:o}，要求 700（不会自动改，请自己 chmod 700）", dir.display()));
    }
    Ok(())
}

/// 读 600 的文件；权限不对就拒绝
fn read_private_file(p: &Path) -> Result<String, String> {
    let m = mode_bits(p).map_err(|e| format!("读不到 {}：{e}", p.display()))?;
    if m != 0o600 {
        return Err(format!("{} 的权限是 {m:o}，要求 600（不会自动改，请自己 chmod 600）", p.display()));
    }
    fs::read_to_string(p).map_err(|e| format!("读不到 {}：{e}", p.display()))
}

/// 写 600 的文件：先写同目录的临时文件再改名，写一半不会留下坏文件。
/// `replace = false` 时目标已存在就报错
fn write_private_file(p: &Path, contents: &str, replace: bool) -> Result<(), String> {
    if !replace && p.exists() {
        return Err(format!("{} 已经存在", p.display()));
    }
    let tmp = p.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    f.write_all(contents.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("写 {} 失败：{e}", tmp.display()))?;
    drop(f);
    fs::rename(&tmp, p).map_err(|e| format!("改名到 {} 失败：{e}", p.display()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// 公钥的显示形式（十六进制，给 `yw-gateway pair add --pub` 用；公钥不是秘密）
pub fn pub_hex(p: &[u8; 32]) -> String {
    hex(p)
}

/// 指纹：BLAKE2s-256(公钥) 的前 4 B（= gateway_id 的前 4 B），日志和终端里只显示它
pub fn fingerprint(gateway_id: &[u8; 8]) -> String {
    hex(&gateway_id[..4])
}

fn random32() -> Result<[u8; 32], String> {
    let mut b = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("读系统随机数失败：{e}"))?;
    Ok(b)
}

fn key_path(dir: &Path) -> PathBuf {
    dir.join(KEY_FILE)
}

/// 生成本机静态密钥（私钥写进配置目录，600），返回公钥。已有密钥就拒绝（换密钥会让已配对的网关都认不出来）
pub fn keygen() -> Result<[u8; 32], String> {
    keygen_in(&config_dir())
}

pub fn keygen_in(dir: &Path) -> Result<[u8; 32], String> {
    ensure_private_dir(dir)?;
    let p = key_path(dir);
    if p.exists() {
        return Err(format!(
            "已经有本机密钥 {}。换密钥会让已配对的网关都认不出本机；确实要换，先把它移走。查看公钥用 `yw-client pubkey`",
            p.display()
        ));
    }
    let secret = random32()?;
    // 先算公钥再落盘：失败时不留下文件
    let kp = StaticKeypair::from_secret(secret);
    let text = format!("# yw-client 本机静态私钥，不要外传；权限必须是 600\nsecret={}\n", hex(&secret));
    write_private_file(&p, &text, false)?;
    Ok(kp.public())
}

/// 读本机密钥
pub fn load_keypair() -> Result<StaticKeypair, String> {
    load_keypair_in(&config_dir())
}

pub fn load_keypair_in(dir: &Path) -> Result<StaticKeypair, String> {
    check_private_dir(dir)?;
    let p = key_path(dir);
    if !p.exists() {
        return Err(format!("还没有本机密钥（{}），先运行 `yw-client keygen`", p.display()));
    }
    let text = read_private_file(&p)?;
    let secret = parse_key_file(&text).ok_or_else(|| format!("{} 格式不对", p.display()))?;
    Ok(StaticKeypair::from_secret(secret))
}

fn parse_key_file(text: &str) -> Option<[u8; 32]> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix("secret="))
        .and_then(unhex::<32>)
}

// ---- 网关档案 ----

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Competitive => "competitive",
        Mode::Balanced => "balanced",
        Mode::Stable => "stable",
    }
}

fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "competitive" | "竞技" => Some(Mode::Competitive),
        "balanced" | "均衡" => Some(Mode::Balanced),
        "stable" | "稳定" => Some(Mode::Stable),
        _ => None,
    }
}

/// 档案名：字母、数字、`-`、`_`、`.`，1~32 个字符，不以 `.` 开头（要当文件名用）
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && !n.starts_with('.')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl GatewayProfile {
    /// 档案文件的内容（含 PSK）
    pub fn to_conf(&self) -> String {
        let mut s = String::from("# yw-client 网关档案（含 PSK，不要外传；权限必须是 600）\n");
        s += &format!("name={}\n", self.name);
        s += &format!("gw_pub={}\n", hex(&self.gw_pub));
        s += &format!("psk={}\n", hex(self.psk.expose()));
        s += &format!("gateway_id={}\n", hex(&self.gateway_id));
        s += &format!("entry={}\n", self.entry);
        if let Some(b) = &self.bind_if {
            s += &format!("bind_if={b}\n");
        }
        if let Some(c) = self.bitrate_cap_kbps {
            s += &format!("bitrate_cap_kbps={c}\n");
        }
        s += &format!("mode={}\n", mode_name(self.mode));
        if let Some(j) = self.j_ms {
            s += &format!("j_ms={j}\n");
        }
        s += &format!("auto_start={}\n", self.auto_start as u8);
        s
    }

    /// 解析档案文件。不认识的键报错（手改配置时拼错了能发现）
    pub fn from_conf(text: &str) -> Result<Self, String> {
        let mut name = None;
        let mut gw_pub = None;
        let mut psk = None;
        let mut gateway_id = None;
        let mut entry = None;
        let mut bind_if = None;
        let mut bitrate_cap_kbps = None;
        let mut mode = Mode::default();
        let mut j_ms = None;
        let mut auto_start = false;
        for (no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or_else(|| format!("第 {} 行没有「=」", no + 1))?;
            let (k, v) = (k.trim(), v.trim());
            let bad = |what: &str| format!("第 {} 行 {k} 的值不对：{what}", no + 1);
            match k {
                "name" => name = Some(v.to_string()),
                "gw_pub" => gw_pub = Some(unhex::<32>(v).ok_or_else(|| bad("要 64 位十六进制"))?),
                // 不把值放进报错信息
                "psk" => psk = Some(Secret32::new(unhex::<32>(v).ok_or_else(|| bad("要 64 位十六进制"))?)),
                "gateway_id" => gateway_id = Some(unhex::<8>(v).ok_or_else(|| bad("要 16 位十六进制"))?),
                "entry" => entry = Some(v.parse::<SocketAddr>().map_err(|_| bad("要 地址:端口"))?),
                "bind_if" => bind_if = (!v.is_empty()).then(|| v.to_string()),
                "bitrate_cap_kbps" => bitrate_cap_kbps = Some(v.parse().map_err(|_| bad("要整数"))?),
                "mode" => mode = parse_mode(v).ok_or_else(|| bad("competitive / balanced / stable"))?,
                "j_ms" => j_ms = Some(v.parse().map_err(|_| bad("要 0~255 的整数"))?),
                "auto_start" => {
                    auto_start = match v {
                        "0" | "false" => false,
                        "1" | "true" => true,
                        _ => return Err(bad("要 0 或 1")),
                    }
                }
                _ => return Err(format!("第 {} 行：不认识的键 {k}", no + 1)),
            }
        }
        let name = name.ok_or("缺 name")?;
        if !valid_name(&name) {
            return Err(format!("name 不合法：{name}"));
        }
        let gw_pub = gw_pub.ok_or("缺 gw_pub")?;
        Ok(Self {
            name,
            gw_pub,
            psk: psk.ok_or("缺 psk")?,
            // 没写就按规格现算（BLAKE2s-256(公钥) 的前 8 B）
            gateway_id: match gateway_id {
                Some(g) => g,
                None => ywp::crypto::gateway_id(&gw_pub),
            },
            entry: entry.ok_or("缺 entry")?,
            bind_if,
            bitrate_cap_kbps,
            mode,
            j_ms,
            auto_start,
        })
    }
}

fn profile_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(GATEWAYS_DIR).join(format!("{name}.conf"))
}

pub fn save_profile_in(dir: &Path, p: &GatewayProfile, replace: bool) -> Result<PathBuf, String> {
    if !valid_name(&p.name) {
        return Err(format!("档案名不合法：{}（字母、数字、- _ .，最多 32 个）", p.name));
    }
    ensure_private_dir(dir)?;
    ensure_private_dir(&dir.join(GATEWAYS_DIR))?;
    let path = profile_path(dir, &p.name);
    if !replace && path.exists() {
        return Err(format!("已经有名为「{}」的网关档案；要覆盖加 --replace，或用 --name 换个名字", p.name));
    }
    write_private_file(&path, &p.to_conf(), replace)?;
    Ok(path)
}

pub fn load_profile_in(dir: &Path, name: &str) -> Result<GatewayProfile, String> {
    if !valid_name(name) {
        return Err(format!("档案名不合法：{name}"));
    }
    check_private_dir(dir)?;
    check_private_dir(&dir.join(GATEWAYS_DIR))?;
    let path = profile_path(dir, name);
    if !path.exists() {
        return Err(format!("没有名为「{name}」的网关档案"));
    }
    let p = GatewayProfile::from_conf(&read_private_file(&path)?).map_err(|e| format!("{}：{e}", path.display()))?;
    if p.name != name {
        return Err(format!("{} 里的 name 是「{}」，和文件名不一致", path.display(), p.name));
    }
    Ok(p)
}

/// 所有档案名（按名字排序）
pub fn list_profiles_in(dir: &Path) -> Result<Vec<String>, String> {
    let g = dir.join(GATEWAYS_DIR);
    if !g.exists() {
        return Ok(Vec::new());
    }
    let mut out: Vec<String> = fs::read_dir(&g)
        .map_err(|e| format!("读不到 {}：{e}", g.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".conf")).map(str::to_string))
        .filter(|n| valid_name(n))
        .collect();
    out.sort();
    Ok(out)
}

/// 选档案：给了名字就用它；没给且只有一个档案时用那一个
pub fn select_profile(name: Option<&str>) -> Result<GatewayProfile, String> {
    let dir = config_dir();
    match name {
        Some(n) => load_profile_in(&dir, n),
        None => {
            let all = list_profiles_in(&dir)?;
            match all.as_slice() {
                [one] => load_profile_in(&dir, one),
                [] => Err("还没有网关档案，先用 `yw-client pair import` 导入配对串".into()),
                _ => Err(format!("有多个网关档案（{}），用 --profile <名字> 指定", all.join("、"))),
            }
        }
    }
}

// ---- 导入配对串 ----

/// `pair import` 的选项（命令行给的优先于配对串里的入口提示）
#[derive(Clone, Debug, Default)]
pub struct ImportOpts {
    pub name: Option<String>,
    pub entry: Option<String>,
    pub bind_if: Option<String>,
    pub replace: bool,
}

/// 从 stdin 读配对串（不走命令行参数，免得进 shell 历史）；
/// 不回显配对串，只打印网关名和指纹（解码走 ywp::creds）
pub fn pair_import_stdin() -> Result<GatewayProfile, String> {
    pair_import_with(io::stdin().lock(), &ImportOpts::default())
}

fn resolve_entry(s: &str) -> Result<SocketAddr, String> {
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    // 域名:端口，导入时解析一次（只取 IPv4，网卡绑定只支持 IPv4）
    let addrs = s.to_socket_addrs().map_err(|e| format!("解析入口 {s} 失败：{e}"))?;
    let v: Vec<SocketAddr> = addrs.collect();
    v.iter().find(|a| a.is_ipv4()).or(v.first()).copied().ok_or_else(|| format!("入口 {s} 解析不出地址"))
}

/// 读配对串、写档案，返回导入的档案。配对串和 PSK 不进报错信息
pub fn pair_import_with(r: impl Read, o: &ImportOpts) -> Result<GatewayProfile, String> {
    pair_import_into(&config_dir(), r, o)
}

pub fn pair_import_into(dir: &Path, r: impl Read, o: &ImportOpts) -> Result<GatewayProfile, String> {
    let mut raw = Vec::new();
    r.take(PAIRING_MAX).read_to_end(&mut raw).map_err(|e| format!("读 stdin 失败：{e}"))?;
    let decoded = {
        let text = String::from_utf8_lossy(&raw);
        let line = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'));
        match line {
            None => Err("stdin 里没有配对串".to_string()),
            // KeyError 的 Display 不含配对串内容
            Some(l) => ywp::creds::decode_pairing(l).map_err(|e| e.to_string()),
        }
    };
    // 读进来的原文用完就清零
    raw.iter_mut().for_each(|b| *b = 0);
    let info = decoded?;
    let gateway_id = ywp::crypto::gateway_id(&info.gw_pub);
    let entry_s = o
        .entry
        .clone()
        .or_else(|| info.entry_hint.clone())
        .ok_or("配对串里没有入口地址，请加 --entry <地址:端口>")?;
    let entry = resolve_entry(&entry_s)?;
    let name = o.name.clone().unwrap_or_else(|| format!("gw-{}", fingerprint(&gateway_id)));
    let p = GatewayProfile {
        name,
        gw_pub: info.gw_pub,
        psk: info.psk.clone(),
        gateway_id,
        entry,
        bind_if: o.bind_if.clone(),
        bitrate_cap_kbps: None,
        mode: Mode::default(),
        j_ms: None,
        auto_start: false,
    };
    save_profile_in(dir, &p, o.replace)?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("yw-client-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn sample() -> GatewayProfile {
        GatewayProfile {
            name: "home".into(),
            gw_pub: [7; 32],
            psk: Secret32::new([9; 32]),
            gateway_id: [1, 2, 3, 4, 5, 6, 7, 8],
            entry: "192.0.2.10:9420".parse().unwrap(),
            bind_if: Some("en0".into()),
            bitrate_cap_kbps: Some(30_000),
            mode: Mode::Balanced,
            j_ms: Some(8),
            auto_start: true,
        }
    }

    #[test]
    fn profile_roundtrip() {
        let p = sample();
        let q = GatewayProfile::from_conf(&p.to_conf()).unwrap();
        assert_eq!(q.name, p.name);
        assert_eq!(q.gw_pub, p.gw_pub);
        assert_eq!(q.psk.expose(), p.psk.expose());
        assert_eq!(q.gateway_id, p.gateway_id);
        assert_eq!(q.entry, p.entry);
        assert_eq!(q.bind_if, p.bind_if);
        assert_eq!(q.bitrate_cap_kbps, p.bitrate_cap_kbps);
        assert_eq!(q.mode, p.mode);
        assert_eq!(q.j_ms, p.j_ms);
        assert!(q.auto_start);
        // 可选项省略时取默认
        let mut m = p.clone();
        m.bind_if = None;
        m.bitrate_cap_kbps = None;
        m.j_ms = None;
        m.mode = Mode::Competitive;
        m.auto_start = false;
        let q = GatewayProfile::from_conf(&m.to_conf()).unwrap();
        assert!(q.bind_if.is_none() && q.j_ms.is_none() && q.bitrate_cap_kbps.is_none() && !q.auto_start);
        assert_eq!(q.mode, Mode::Competitive);
    }

    #[test]
    fn profile_errors_do_not_leak_psk() {
        let bad = sample().to_conf().replace("psk=0909", "psk=zz09");
        let e = GatewayProfile::from_conf(&bad).unwrap_err();
        assert!(e.contains("psk") && !e.contains("0909"), "{e}");
        assert!(GatewayProfile::from_conf("name=a\nfoo=1\n").unwrap_err().contains("不认识的键"));
        assert!(GatewayProfile::from_conf("name=../x\n").is_err());
        // Debug 不打 PSK
        let dbg = format!("{:?}", sample());
        assert!(dbg.contains("<redacted>") && !dbg.contains("9, 9, 9"), "{dbg}");
    }

    #[test]
    fn names() {
        assert!(valid_name("home-1.mac_mini"));
        assert!(!valid_name(""));
        assert!(!valid_name(".hidden"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name("中文"));
        assert!(!valid_name(&"x".repeat(33)));
    }

    #[test]
    fn save_load_and_permissions() {
        let d = tmpdir("perm");
        let p = sample();
        let path = save_profile_in(&d, &p, false).unwrap();
        assert_eq!(mode_bits(&d).unwrap(), 0o700);
        assert_eq!(mode_bits(&path).unwrap(), 0o600);
        assert!(save_profile_in(&d, &p, false).unwrap_err().contains("--replace"));
        save_profile_in(&d, &p, true).unwrap();
        assert_eq!(load_profile_in(&d, "home").unwrap().entry, p.entry);
        assert_eq!(list_profiles_in(&d).unwrap(), vec!["home".to_string()]);
        assert!(load_profile_in(&d, "nope").is_err());

        // 文件权限被放宽：拒绝读
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_profile_in(&d, "home").unwrap_err().contains("600"));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        // 目录权限被放宽：拒绝读
        fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load_profile_in(&d, "home").unwrap_err().contains("700"));
        fs::set_permissions(&d, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn key_file_format() {
        let k = format!("# x\nsecret={}\n", hex(&[0xab; 32]));
        assert_eq!(parse_key_file(&k), Some([0xab; 32]));
        assert_eq!(parse_key_file("secret=12\n"), None);
        assert_eq!(unhex::<2>("0aFf"), Some([0x0a, 0xff]));
        assert_eq!(unhex::<2>("0aF"), None);
        assert_eq!(fingerprint(&[0xde, 0xad, 0xbe, 0xef, 1, 2, 3, 4]), "deadbeef");
    }

    #[test]
    fn write_private_is_600_and_refuses_overwrite() {
        let d = tmpdir("write");
        ensure_private_dir(&d).unwrap();
        let p = d.join("f");
        write_private_file(&p, "a", false).unwrap();
        assert_eq!(mode_bits(&p).unwrap(), 0o600);
        assert!(write_private_file(&p, "b", false).is_err());
        write_private_file(&p, "c", true).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "c");
        assert!(!p.with_extension("tmp").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn import_rejects_empty_input() {
        let d = tmpdir("import");
        let e = pair_import_into(&d, &b"\n# only comment\n"[..], &ImportOpts::default()).unwrap_err();
        assert!(e.contains("没有配对串"), "{e}");
        assert!(!d.join(GATEWAYS_DIR).exists());
        let _ = fs::remove_dir_all(&d);
    }

    /// 要 A 组实现 StaticKeypair::from_secret、decode_pairing、gateway_id 之后才能跑
    #[test]
    #[ignore = "等 ywp 的 StaticKeypair::from_secret 实现"]
    fn keygen_writes_600_and_refuses_second() {
        let d = tmpdir("keygen");
        let pubk = keygen_in(&d).unwrap();
        assert_eq!(mode_bits(&key_path(&d)).unwrap(), 0o600);
        assert_eq!(load_keypair_in(&d).unwrap().public(), pubk);
        assert!(keygen_in(&d).unwrap_err().contains("已经有本机密钥"));
        fs::remove_dir_all(&d).unwrap();
    }
}
