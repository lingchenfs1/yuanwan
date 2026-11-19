//! 配对子命令（不进守护流程）：`keygen`、`pair add / list / remove`。
//!
//! keys/ 的文件格式、加载和配对串编解码都在 ywp::creds（网关、fakegw、客户端共用）；这里只做
//! 命令行、参数校验、重名检查，以及配对串的输出：
//! - 配对串含 PSK，不进日志，也不显示在终端；
//! - 默认写成 600 权限的文件（`--out` 指定路径，默认 `<目录>/pair-<名字>.txt`，已存在就拒绝）；
//! - 只有显式加 `--stdout`、并且 stdout 不是终端时才写 stdout（给 `… | yw-client pair import` 这样的管道用）。
//!   经 ssh BatchMode 执行时远端 stdout 本来就是管道，所以光看「是不是终端」挡不住泄漏，必须显式要求。

use std::fs::OpenOptions;
use std::io::{IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 配对串写到哪里
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PairOut {
    /// 600 权限的文件（默认）；已存在就拒绝
    File(PathBuf),
    /// 必须显式 `--stdout`，并且 stdout 不是终端
    Stdout,
}

/// 一个已配对的客户端：(名字, 公钥, 指纹)
pub(crate) type ClientRow = (String, [u8; 32], [u8; 4]);

/// keys/ 的读写。正式实现走 ywp::creds；单测用内存版
pub(crate) trait KeyStore {
    /// keys/ 里已经有网关静态密钥
    fn has_keys(&self, dir: &Path) -> Result<bool, String>;
    /// 生成网关静态密钥和 reset_key；rotate_reset 时只换 reset_key（已有的静态密钥和配对不动）
    fn generate(&self, dir: &Path, rotate_reset: bool) -> Result<(), String>;
    /// 新增一个客户端（PSK 由实现生成），返回配对串
    fn add_client(&self, dir: &Path, name: &str, client_pub: &[u8; 32]) -> Result<String, String>;
    fn list_clients(&self, dir: &Path) -> Result<Vec<ClientRow>, String>;
    /// 返回是否删掉了
    fn remove_client(&self, dir: &Path, name: &str) -> Result<bool, String>;
}

/// 正式实现：keys/ 的格式和写入接口由 ywp::creds 定。
/// TODO(A 组 creds 的写入接口落地、yw-gateway 加上 ywp 依赖后)：改成调用 ywp::creds
struct CredsStore;

const CREDS_NOT_READY: &str = "keys/ 的读写接口（ywp::creds）还没落地，配对子命令暂时不能用";

impl KeyStore for CredsStore {
    fn has_keys(&self, _dir: &Path) -> Result<bool, String> {
        Err(CREDS_NOT_READY.into())
    }
    fn generate(&self, _dir: &Path, _rotate_reset: bool) -> Result<(), String> {
        Err(CREDS_NOT_READY.into())
    }
    fn add_client(&self, _dir: &Path, _name: &str, _client_pub: &[u8; 32]) -> Result<String, String> {
        Err(CREDS_NOT_READY.into())
    }
    fn list_clients(&self, _dir: &Path) -> Result<Vec<ClientRow>, String> {
        Err(CREDS_NOT_READY.into())
    }
    fn remove_client(&self, _dir: &Path, _name: &str) -> Result<bool, String> {
        Err(CREDS_NOT_READY.into())
    }
}

pub fn keygen(dir: &Path, rotate_reset: bool) -> Result<(), String> {
    keygen_with(&CredsStore, dir, rotate_reset)
}

/// 配对串含 PSK，不进日志，也不显示在终端（见模块说明）
pub fn pair_add(dir: &Path, name: &str, client_pub: &[u8; 32], out: PairOut) -> Result<(), String> {
    pair_add_with(&CredsStore, dir, name, client_pub, &out, std::io::stdout().is_terminal())
}

pub fn pair_list(dir: &Path) -> Result<Vec<(String, [u8; 4])>, String> {
    Ok(CredsStore.list_clients(dir)?.into_iter().map(|(n, _, fp)| (n, fp)).collect())
}

pub fn pair_remove(dir: &Path, name: &str) -> Result<(), String> {
    pair_remove_with(&CredsStore, dir, name)
}

pub(crate) fn keygen_with(store: &dyn KeyStore, dir: &Path, rotate_reset: bool) -> Result<(), String> {
    ensure_dir(dir)?;
    let has = store.has_keys(dir)?;
    if has && !rotate_reset {
        return Err(format!(
            "{} 里已经有网关密钥。换静态密钥会让所有已配对的客户端失效，真要换请先把这个目录移走；只换 reset_key 用 --rotate-reset",
            dir.display()
        ));
    }
    if !has && rotate_reset {
        return Err(format!("{} 里还没有网关密钥，先不带 --rotate-reset 运行 keygen", dir.display()));
    }
    store.generate(dir, rotate_reset)
}

pub(crate) fn pair_add_with(
    store: &dyn KeyStore,
    dir: &Path,
    name: &str,
    client_pub: &[u8; 32],
    out: &PairOut,
    stdout_is_tty: bool,
) -> Result<(), String> {
    check_name(name)?;
    if client_pub.iter().all(|&b| b == 0) {
        return Err("客户端公钥不能全是 0".into());
    }
    // 先检查输出位置，免得加完客户端才发现写不出去
    match out {
        PairOut::Stdout if stdout_is_tty => {
            return Err("配对串含 PSK，不往终端输出：用 --out <文件>，或者接管道（… --stdout | yw-client pair import）".into())
        }
        PairOut::File(p) if p.exists() => return Err(format!("{} 已存在，不覆盖", p.display())),
        _ => {}
    }
    let existing = store.list_clients(dir)?;
    if existing.iter().any(|(n, _, _)| n == name) {
        return Err(format!("已经有叫「{name}」的客户端，先 pair remove 再加"));
    }
    if let Some((n, _, _)) = existing.iter().find(|(_, p, _)| p == client_pub) {
        return Err(format!("这个公钥已经配对过（名字「{n}」）"));
    }
    let s = store.add_client(dir, name, client_pub)?;
    if let Err(e) = write_pairing(out, &s) {
        // 配对串没交出去，这条配对没人能用：撤掉
        let _ = store.remove_client(dir, name);
        return Err(format!("{e}（已撤销这次配对）"));
    }
    Ok(())
}

pub(crate) fn pair_remove_with(store: &dyn KeyStore, dir: &Path, name: &str) -> Result<(), String> {
    if store.remove_client(dir, name)? {
        Ok(())
    } else {
        Err(format!("没有叫「{name}」的客户端"))
    }
}

/// keys/ 目录：不存在就建成 700；已存在但权限不是 700 就拒绝（不自动改，和 ywp::creds 的加载规则一致）
fn ensure_dir(dir: &Path) -> Result<(), String> {
    match std::fs::metadata(dir) {
        Ok(m) => {
            if !m.is_dir() {
                return Err(format!("{} 不是目录", dir.display()));
            }
            let mode = m.permissions().mode() & 0o777;
            if mode != 0o700 {
                return Err(format!("{} 的权限是 {mode:o}，要求 700（不自动改）", dir.display()));
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|e| format!("建不了 {}：{e}", dir.display()))?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("设不了 {} 的权限：{e}", dir.display()))
        }
        Err(e) => Err(format!("读不了 {}：{e}", dir.display())),
    }
}

/// 名字：1~32 字节 UTF-8（握手 TLV 0x07 的上限），不含空白、控制字符和「/」
fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("名字要 1~32 字节".into());
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/') {
        return Err("名字里不能有空白、控制字符或「/」".into());
    }
    Ok(())
}

fn write_pairing(out: &PairOut, s: &str) -> Result<(), String> {
    match out {
        PairOut::File(p) => {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(p)
                .map_err(|e| format!("建不了 {}：{e}", p.display()))?;
            f.write_all(s.as_bytes()).and_then(|_| f.write_all(b"\n")).and_then(|_| f.sync_all()).map_err(|e| {
                let _ = std::fs::remove_file(p);
                format!("写 {} 失败：{e}", p.display())
            })
        }
        PairOut::Stdout => {
            let mut o = std::io::stdout().lock();
            writeln!(o, "{s}").and_then(|_| o.flush()).map_err(|e| format!("写 stdout 失败：{e}"))
        }
    }
}

/// 64 位十六进制 → 32 字节公钥
/// TODO(和 A、E 组对齐)：客户端 keygen 打印公钥的格式定下来后，这里同步（现在只认十六进制）
fn parse_pub_hex(s: &str) -> Result<[u8; 32], String> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return Err("客户端公钥要 64 位十六进制".into());
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| "客户端公钥要 64 位十六进制".to_string())?;
    }
    Ok(out)
}

fn hex4(fp: &[u8; 4]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

pub const USAGE: &str = "配对子命令（keys/ 在 <目录>/keys，<目录> 默认 ~/yw-gw）：
  yw-gateway keygen [--dir <目录>] [--rotate-reset]
      生成网关静态密钥和 reset_key；已有密钥时拒绝（换静态密钥会让所有配对失效）。--rotate-reset 只换 reset_key
  yw-gateway pair add <名字> <客户端公钥（64 位十六进制）> [--out <文件> | --stdout] [--dir <目录>]
      配对串含 PSK：默认写成 600 权限的 <目录>/pair-<名字>.txt；--stdout 只在 stdout 不是终端时可用（接管道）
  yw-gateway pair list [--dir <目录>]
  yw-gateway pair remove <名字> [--dir <目录>]";

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    Keygen { rotate_reset: bool },
    Add { name: String, client_pub: [u8; 32], out: PairOut },
    List,
    Remove { name: String },
}

/// 解析子命令。返回 (基准目录, 命令)；默认目录由调用方给
fn parse(args: &[String], default_dir: &Path) -> Result<(PathBuf, Cmd), String> {
    let mut dir = default_dir.to_path_buf();
    let mut out: Option<PairOut> = None;
    let mut rotate_reset = false;
    let mut pos: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let val = || args.get(i + 1).ok_or_else(|| format!("{a} 缺参数"));
        match a {
            "--dir" => {
                dir = PathBuf::from(val()?);
                i += 1;
            }
            "--out" => {
                if out.is_some() {
                    return Err("--out 和 --stdout 只能给一个".into());
                }
                out = Some(PairOut::File(PathBuf::from(val()?)));
                i += 1;
            }
            "--stdout" => {
                if out.is_some() {
                    return Err("--out 和 --stdout 只能给一个".into());
                }
                out = Some(PairOut::Stdout);
            }
            "--rotate-reset" => rotate_reset = true,
            _ if a.starts_with("--") => return Err(format!("不认识的参数：{a}")),
            _ => pos.push(a),
        }
        i += 1;
    }
    let only = |want: &str, ok: bool| if ok { Ok(()) } else { Err(format!("{want} 不接受这个参数")) };
    if !matches!(pos.as_slice(), ["keygen"] | ["pair", "add", ..]) && (out.is_some() || rotate_reset) {
        return Err("这个子命令不接受 --out、--stdout、--rotate-reset".into());
    }
    let cmd = match pos.as_slice() {
        ["keygen"] => {
            only("keygen 的 --out/--stdout", out.is_none())?;
            Cmd::Keygen { rotate_reset }
        }
        ["pair", "add", name, key] => {
            only("pair add 的 --rotate-reset", !rotate_reset)?;
            check_name(name)?;
            let out = out.unwrap_or_else(|| PairOut::File(dir.join(format!("pair-{name}.txt"))));
            Cmd::Add { name: name.to_string(), client_pub: parse_pub_hex(key)?, out }
        }
        ["pair", "list"] => Cmd::List,
        ["pair", "remove", name] => Cmd::Remove { name: name.to_string() },
        _ => return Err("参数不对".into()),
    };
    Ok((dir, cmd))
}

/// 子命令入口（main 在第一个参数是 keygen 或 pair 时调用），返回进程退出码
pub fn run(args: &[String], default_dir: &Path) -> i32 {
    let (base, cmd) = match parse(args, default_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}\n{USAGE}");
            return 2;
        }
    };
    let keys = base.join("keys");
    let r = match cmd {
        Cmd::Keygen { rotate_reset } => keygen(&keys, rotate_reset).map(|_| {
            if rotate_reset {
                "已换 reset_key（之前发给客户端的 RESET 令牌全部失效）".to_string()
            } else {
                format!("已生成网关密钥：{}", keys.display())
            }
        }),
        Cmd::Add { name, client_pub, out } => {
            let shown = match &out {
                PairOut::File(p) => format!("配对串已写入 {}（600 权限）。导入客户端后请把这个文件移到废纸篓", p.display()),
                PairOut::Stdout => String::new(),
            };
            pair_add(&keys, &name, &client_pub, out).map(|_| {
                if shown.is_empty() {
                    eprintln!("已配对「{name}」");
                }
                shown
            })
        }
        Cmd::List => pair_list(&keys).map(|v| {
            if v.is_empty() {
                "还没有配对的客户端".to_string()
            } else {
                v.iter().map(|(n, fp)| format!("{n}\t{}", hex4(fp))).collect::<Vec<_>>().join("\n")
            }
        }),
        Cmd::Remove { name } => pair_remove(&keys, &name).map(|_| format!("已删除「{name}」（它下次握手会被拒）")),
    };
    match r {
        Ok(msg) => {
            if !msg.is_empty() {
                // 提示一律走 stderr：stdout 可能接着 `yw-client pair import`
                eprintln!("{msg}");
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct MemStore {
        keys: RefCell<bool>,
        resets: RefCell<u32>,
        clients: RefCell<Vec<(String, [u8; 32])>>,
    }

    impl KeyStore for MemStore {
        fn has_keys(&self, _dir: &Path) -> Result<bool, String> {
            Ok(*self.keys.borrow())
        }
        fn generate(&self, _dir: &Path, rotate_reset: bool) -> Result<(), String> {
            *self.keys.borrow_mut() = true;
            if rotate_reset {
                *self.resets.borrow_mut() += 1;
            }
            Ok(())
        }
        fn add_client(&self, _dir: &Path, name: &str, client_pub: &[u8; 32]) -> Result<String, String> {
            self.clients.borrow_mut().push((name.to_string(), *client_pub));
            Ok(format!("PAIRING-FOR-{name}"))
        }
        fn list_clients(&self, _dir: &Path) -> Result<Vec<ClientRow>, String> {
            Ok(self.clients.borrow().iter().map(|(n, p)| (n.clone(), *p, [p[0], p[1], p[2], p[3]])).collect())
        }
        fn remove_client(&self, _dir: &Path, name: &str) -> Result<bool, String> {
            let mut c = self.clients.borrow_mut();
            let n = c.len();
            c.retain(|(x, _)| x != name);
            Ok(c.len() != n)
        }
    }

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("yw-pairing-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            // 只删本测试自己建的临时目录
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    const PUB: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

    #[test]
    fn parse_commands() {
        let d = Path::new("/base");
        assert_eq!(parse(&args("keygen"), d).unwrap(), (d.to_path_buf(), Cmd::Keygen { rotate_reset: false }));
        assert_eq!(parse(&args("keygen --rotate-reset --dir /x"), d).unwrap(), (PathBuf::from("/x"), Cmd::Keygen { rotate_reset: true }));
        let (_, c) = parse(&args(&format!("pair add 客厅 {PUB}")), d).unwrap();
        let Cmd::Add { name, client_pub, out } = c else { panic!() };
        assert_eq!(name, "客厅");
        assert_eq!(client_pub[0], 1);
        assert_eq!(client_pub[31], 0x20);
        assert_eq!(out, PairOut::File(PathBuf::from("/base/pair-客厅.txt")));
        let (_, c) = parse(&args(&format!("pair add a {PUB} --stdout")), d).unwrap();
        assert!(matches!(c, Cmd::Add { out: PairOut::Stdout, .. }));
        let (_, c) = parse(&args(&format!("pair add a {PUB} --out /t/p.txt")), d).unwrap();
        assert!(matches!(c, Cmd::Add { out: PairOut::File(p), .. } if p == Path::new("/t/p.txt")));
        assert_eq!(parse(&args("pair list"), d).unwrap().1, Cmd::List);
        assert_eq!(parse(&args("pair remove a"), d).unwrap().1, Cmd::Remove { name: "a".into() });
        // 错误
        assert!(parse(&args(&format!("pair add a {PUB} --out /x --stdout")), d).is_err());
        assert!(parse(&args("pair add a 0102"), d).is_err());
        assert!(parse(&args(&format!("pair add a {}", "zz".repeat(32))), d).is_err());
        assert!(parse(&args("pair list --stdout"), d).is_err());
        assert!(parse(&args("keygen --stdout"), d).is_err());
        assert!(parse(&args("pair"), d).is_err());
        assert!(parse(&args("keygen --dir"), d).is_err());
        assert!(parse(&args("keygen --force"), d).is_err());
        assert!(parse(&args(&format!("pair add {} {PUB}", "长".repeat(11))), d).is_err(), "33 字节");
    }

    #[test]
    fn keygen_refuses_to_replace_static_key() {
        let t = TmpDir::new("keygen");
        let keys = t.0.join("keys");
        let s = MemStore::default();
        assert!(keygen_with(&s, &keys, true).is_err(), "没有密钥时不能只换 reset_key");
        keygen_with(&s, &keys, false).unwrap();
        assert_eq!(std::fs::metadata(&keys).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(keygen_with(&s, &keys, false).is_err());
        keygen_with(&s, &keys, true).unwrap();
        assert_eq!(*s.resets.borrow(), 1);
    }

    #[test]
    fn keygen_rejects_loose_dir() {
        let t = TmpDir::new("loose");
        std::fs::set_permissions(&t.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let e = keygen_with(&MemStore::default(), &t.0, false).unwrap_err();
        assert!(e.contains("755"), "{e}");
    }

    #[test]
    fn pair_add_writes_600_file_and_rejects_duplicates() {
        let t = TmpDir::new("add");
        let s = MemStore::default();
        let pk = parse_pub_hex(PUB).unwrap();
        let f = t.0.join("pair-a.txt");
        pair_add_with(&s, &t.0, "a", &pk, &PairOut::File(f.clone()), true).unwrap();
        let m = std::fs::metadata(&f).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "PAIRING-FOR-a\n");
        // 重名、重复公钥、目标文件已存在
        let f2 = t.0.join("pair-b.txt");
        assert!(pair_add_with(&s, &t.0, "a", &[9; 32], &PairOut::File(f2.clone()), true).unwrap_err().contains("已经有"));
        assert!(pair_add_with(&s, &t.0, "b", &pk, &PairOut::File(f2.clone()), true).unwrap_err().contains("配对过"));
        assert!(pair_add_with(&s, &t.0, "b", &[9; 32], &PairOut::File(f.clone()), true).unwrap_err().contains("已存在"));
        assert!(!f2.exists());
        assert!(pair_add_with(&s, &t.0, "b", &[0; 32], &PairOut::File(f2.clone()), true).is_err());
        assert_eq!(s.clients.borrow().len(), 1);
    }

    #[test]
    fn pair_add_stdout_refuses_terminal() {
        let s = MemStore::default();
        let e = pair_add_with(&s, Path::new("/nonexistent"), "a", &[7; 32], &PairOut::Stdout, true).unwrap_err();
        assert!(e.contains("终端"));
        assert!(s.clients.borrow().is_empty(), "拒绝时不加客户端");
    }

    #[test]
    fn pair_add_rolls_back_when_output_fails() {
        let t = TmpDir::new("rollback");
        let s = MemStore::default();
        // 目录不存在，文件建不了
        let f = t.0.join("no-such-dir").join("p.txt");
        let e = pair_add_with(&s, &t.0, "a", &[7; 32], &PairOut::File(f), true).unwrap_err();
        assert!(e.contains("撤销"), "{e}");
        assert!(s.clients.borrow().is_empty());
    }

    #[test]
    fn remove_and_list() {
        let t = TmpDir::new("remove");
        let s = MemStore::default();
        pair_add_with(&s, &t.0, "a", &[7; 32], &PairOut::File(t.0.join("1")), true).unwrap();
        pair_add_with(&s, &t.0, "b", &[8; 32], &PairOut::File(t.0.join("2")), true).unwrap();
        assert!(pair_remove_with(&s, &t.0, "c").is_err());
        pair_remove_with(&s, &t.0, "a").unwrap();
        let l = s.list_clients(&t.0).unwrap();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].0, "b");
        assert_eq!(hex4(&l[0].2), "08080808");
    }

    #[test]
    fn real_store_not_ready_yet() {
        // creds 写入接口落地前，子命令明确报错而不是 panic
        assert!(pair_list(Path::new("/nonexistent")).unwrap_err().contains("ywp::creds"));
    }
}
