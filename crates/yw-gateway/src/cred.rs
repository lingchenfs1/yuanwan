//! 读 PS5 凭据文件（格式与 tools/m0-probe 的 .cred 相同：key=value，十六进制）。
//! 凭据内容不打印、不写日志。

use std::path::Path;

pub struct Cred {
    pub regist_key: [u8; 16],
    pub rp_key: [u8; 16],
    pub console_pin: u32,
    pub nickname: String,
}

fn hex16(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

pub fn load(path: &Path) -> Result<Cred, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("读不到凭据 {}：{e}", path.display()))?;
    let mut regist = None;
    let mut rp = None;
    let mut pin = 0;
    let mut nick = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "regist_key" => regist = hex16(v),
            "rp_key" => rp = hex16(v),
            "console_pin" => pin = v.parse().unwrap_or(0),
            "nickname" => nick = v.to_string(),
            _ => {}
        }
    }
    Ok(Cred {
        regist_key: regist.ok_or("凭据里缺 regist_key 或格式不对")?,
        rp_key: rp.ok_or("凭据里缺 rp_key 或格式不对")?,
        console_pin: pin,
        nickname: nick,
    })
}
