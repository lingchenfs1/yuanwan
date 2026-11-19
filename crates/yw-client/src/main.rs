//! yw-client 命令行。

use std::path::PathBuf;
use std::process::ExitCode;
use yw_client::config::{self, ImportOpts};
use yw_client::decode::HwKind;
use yw_client::play::{self, PlayOpts};
use yw_client::stream::{self, HeadlessOpts};

const USAGE: &str = "\
用法：
  yw-client [--profile <名字>]
      有界面，正常使用
  yw-client --headless --connect-once --duration <秒> --json [--profile <名字>] [--start]
      无界面连一次（真解码、不呈现），到时间或被拒、被关时结束，向 stdout 输出一行 JSON：
      { ok, hs_attempts, fail_reason, first_frame_ms, connect_ms, freezes, freeze_total_ms, w_frames, l2, l3,
        frames, decode_p50_ms, decode_p95_ms, decode_errors, recv, lost, reconnects }
      ok 为 false 时退出码 1。日志走 stderr
      --start：连上后发 PS5_CMD START（网关在暂停时也恢复并唤醒 PS5）
  yw-client --play <录像.h265> [--fps <帧率>] [--frames <帧数>] [--loop <遍数>] [--no-window] [--upload] [--sw]
      调试用：不连网关，读本地录像（Annex-B HEVC，带参数集头），按帧率（默认 60）送解码、到达即呈现，
      放完打印帧数和耗时统计。--no-window 只解码；--upload 用上传呈现（和零拷贝对比）；--sw 用软解。
      窗口里按 Esc 或 Q 提前结束
  yw-client keygen
      生成本机密钥，打印公钥
  yw-client pubkey
      打印本机公钥
  yw-client pair import [--name <名字>] [--entry <地址:端口>] [--bind-if <网卡>] [--replace]
      从 stdin 读配对串（不回显，只打印网关名和指纹）。--entry 覆盖配对串里的入口
  yw-client pair list
      列出已导入的网关档案
  yw-client --version
      打印 SDL、FFmpeg、Opus 的运行时版本
配置目录：~/Library/Application Support/yuanwan/（环境变量 YW_CONFIG_DIR 可换）
";

#[derive(Debug, Default)]
struct Headless {
    connect_once: bool,
    duration_s: Option<u32>,
    json: bool,
    start: bool,
}

#[derive(Debug)]
enum Command {
    Gui { profile: Option<String> },
    Headless {
        profile: Option<String>,
        opts: Headless,
    },
    Play(PlayOpts),
    Keygen,
    Pubkey,
    PairImport(ImportOpts),
    PairList,
    Version,
    Help,
}

fn next_val<'a>(it: &mut impl Iterator<Item = &'a String>, flag: &str, what: &str) -> Result<String, String> {
    it.next().cloned().ok_or_else(|| format!("{flag} 缺少{what}"))
}

fn parse_pair_import(args: &[String]) -> Result<Command, String> {
    let mut o = ImportOpts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => {
                let n = next_val(&mut it, "--name", "名字")?;
                if !config::valid_name(&n) {
                    return Err(format!("名字不合法：{n}（字母、数字、- _ .，最多 32 个）"));
                }
                o.name = Some(n);
            }
            "--entry" => o.entry = Some(next_val(&mut it, "--entry", "地址:端口")?),
            "--bind-if" => o.bind_if = Some(next_val(&mut it, "--bind-if", "网卡名")?),
            "--replace" => o.replace = true,
            other => return Err(format!("pair import 不认识的参数：{other}（配对串从 stdin 读，不要放在命令行上）")),
        }
    }
    Ok(Command::PairImport(o))
}

fn parse_play(path: String, rest: &mut std::slice::Iter<'_, String>) -> Result<Command, String> {
    let mut o = PlayOpts { path: PathBuf::from(path), ..Default::default() };
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--fps" => {
                let v = next_val(rest, "--fps", "帧率")?;
                o.fps = v.parse().ok().filter(|f: &f64| *f >= 1.0 && *f <= 1000.0).ok_or(format!("--fps 不对：{v}"))?;
            }
            "--frames" => {
                let v = next_val(rest, "--frames", "帧数")?;
                o.max_frames = Some(v.parse().map_err(|_| format!("--frames 不是整数：{v}"))?);
            }
            "--loop" => {
                let v = next_val(rest, "--loop", "遍数")?;
                o.loops = v.parse().ok().filter(|n| *n >= 1).ok_or(format!("--loop 不对：{v}"))?;
            }
            "--no-window" => o.window = false,
            "--upload" => o.upload = true,
            "--sw" => o.hw = HwKind::Software,
            other => return Err(format!("--play 不认识的参数：{other}")),
        }
    }
    Ok(Command::Play(o))
}

fn parse(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 1 => return Ok(Command::Keygen),
        Some("pubkey") if args.len() == 1 => return Ok(Command::Pubkey),
        Some("pair") if args.get(1).map(String::as_str) == Some("import") => return parse_pair_import(&args[2..]),
        Some("pair") if args.len() == 2 && args[1] == "list" => return Ok(Command::PairList),
        Some("keygen") | Some("pubkey") | Some("pair") => return Err("子命令参数不对".into()),
        Some("--play") => {
            let mut it = args[1..].iter();
            let path = it.next().ok_or("--play 缺少录像文件")?.clone();
            return parse_play(path, &mut it);
        }
        _ => {}
    }
    let mut profile = None;
    let mut headless = false;
    let mut opts = Headless::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--version" => return Ok(Command::Version),
            "--profile" => profile = Some(next_val(&mut it, "--profile", "名字")?),
            "--headless" => headless = true,
            "--connect-once" => opts.connect_once = true,
            "--duration" => {
                let v = next_val(&mut it, "--duration", "秒数")?;
                opts.duration_s = Some(v.parse().map_err(|_| format!("--duration 不是整数：{v}"))?);
            }
            "--json" => opts.json = true,
            "--start" => opts.start = true,
            "--play" => return Err("--play 要放在最前面：yw-client --play <录像.h265> …".into()),
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    if headless {
        if !opts.connect_once || opts.duration_s.is_none() || !opts.json {
            return Err("--headless 目前只支持 --connect-once --duration <秒> --json".into());
        }
        Ok(Command::Headless { profile, opts })
    } else if opts.connect_once || opts.duration_s.is_some() || opts.json || opts.start {
        Err("--connect-once、--duration、--json、--start 只能和 --headless 一起用".into())
    } else {
        Ok(Command::Gui { profile })
    }
}

fn print_versions() {
    let v = sdl3::version::version();
    println!("SDL {}.{}.{}", v.major, v.minor, v.patch);
    let av = unsafe { ffmpeg_sys_next::avcodec_version() };
    println!("libavcodec {}.{}.{}", av >> 16, (av >> 8) & 0xff, av & 0xff);
    println!("{}", opus::version());
}

/// 连网关之前的检查：档案能读、本机密钥在；默认路由走隧道又没指定网卡时提示
fn preflight(profile: Option<&str>) -> Result<(config::GatewayProfile, ywp::crypto::StaticKeypair), String> {
    let p = config::select_profile(profile)?;
    let kp = config::load_keypair()?;
    if p.bind_if.is_none() {
        if let Some(i) = yw_client::platform::route_iface(p.entry.ip()) {
            if yw_client::platform::is_tunnel_iface(&i) {
                eprintln!(
                    "提示：去网关的包会走隧道网卡 {i}（代理或 VPN）。请在档案里加 bind_if=<物理网卡>，例如 bind_if=en0；本程序不会去改代理设置"
                );
            }
        }
    }
    Ok((p, kp))
}

fn run(cmd: Command) -> Result<ExitCode, String> {
    match cmd {
        Command::Help => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Version => {
            println!("yw-client {}", env!("CARGO_PKG_VERSION"));
            print_versions();
            Ok(ExitCode::SUCCESS)
        }
        Command::Keygen => {
            let p = config::keygen()?;
            println!("已生成本机密钥。公钥（配对时交给网关）：{}", config::pub_hex(&p));
            Ok(ExitCode::SUCCESS)
        }
        Command::Pubkey => {
            let kp = config::load_keypair()?;
            println!("{}", config::pub_hex(&kp.public()));
            Ok(ExitCode::SUCCESS)
        }
        Command::PairImport(o) => {
            let p = config::pair_import_with(std::io::stdin().lock(), &o)?;
            // 只打印名字、指纹、入口，不回显配对串和 PSK
            println!("已导入网关「{}」，指纹 {}，入口 {}", p.name, config::fingerprint(&p.gateway_id), p.entry);
            Ok(ExitCode::SUCCESS)
        }
        Command::PairList => {
            let dir = config::config_dir();
            let names = config::list_profiles_in(&dir)?;
            if names.is_empty() {
                println!("还没有网关档案");
            }
            for n in names {
                match config::load_profile_in(&dir, &n) {
                    Ok(p) => println!(
                        "{}\t指纹 {}\t入口 {}\t网卡 {}",
                        p.name,
                        config::fingerprint(&p.gateway_id),
                        p.entry,
                        p.bind_if.as_deref().unwrap_or("（不绑）")
                    ),
                    Err(e) => println!("{n}\t读不了：{e}"),
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Play(o) => {
            let rep = play::run(&o)?;
            println!("{rep}");
            Ok(if rep.decode_errors() == 0 { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
        Command::Gui { profile } => {
            let (p, kp) = preflight(profile.as_deref())?;
            eprintln!("连网关「{}」：入口 {}，网卡 {}", p.name, p.entry, p.bind_if.as_deref().unwrap_or("（不绑）"));
            let sum = stream::run_gui(&p, kp, HwKind::VideoToolbox)?;
            eprintln!("{sum}");
            Ok(ExitCode::SUCCESS)
        }
        Command::Headless { profile, opts } => {
            // 档案、密钥有问题也输出一行 JSON，脚本好统一处理
            let rep = match preflight(profile.as_deref()) {
                Ok((mut p, kp)) => {
                    if opts.start {
                        p.auto_start = true;
                    }
                    let o = HeadlessOpts { duration_s: opts.duration_s.unwrap_or(0), hw: HwKind::VideoToolbox };
                    stream::run_headless(&p, kp, &o)
                }
                Err(e) => {
                    eprintln!("{e}");
                    stream::HeadlessReport { fail_reason: Some("config".into()), ..Default::default() }
                }
            };
            println!("{}", rep.to_json());
            Ok(if rep.ok { ExitCode::SUCCESS } else { ExitCode::from(1) })
        }
    }
}

fn main() -> ExitCode {
    yw_client::clock::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args).and_then(run) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            if e.contains("参数") || e.contains("缺少") || e.contains("只能") || e.contains("只支持") {
                eprint!("{USAGE}");
            }
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Command, String> {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&v)
    }

    #[test]
    fn parse_commands() {
        assert!(matches!(p(""), Ok(Command::Gui { profile: None })));
        assert!(matches!(p("--profile home"), Ok(Command::Gui { profile: Some(_) })));
        assert!(matches!(p("--headless --connect-once --duration 10 --json --start"), Ok(Command::Headless { .. })));
        assert!(p("--headless --duration 10").is_err());
        assert!(p("--json").is_err());
        assert!(matches!(p("keygen"), Ok(Command::Keygen)));
        assert!(matches!(p("pubkey"), Ok(Command::Pubkey)));
        assert!(p("keygen x").is_err());
        assert!(matches!(p("pair list"), Ok(Command::PairList)));
        match p("pair import --name home --entry 192.0.2.10:9420 --bind-if en0 --replace") {
            Ok(Command::PairImport(o)) => {
                assert_eq!(o.name.as_deref(), Some("home"));
                assert_eq!(o.bind_if.as_deref(), Some("en0"));
                assert!(o.replace);
            }
            other => panic!("{other:?}"),
        }
        assert!(p("pair import YWP1-xxxx").is_err(), "配对串不能放在命令行上");
        assert!(p("pair import --name ../x").is_err());
        match p("--play a.h265 --fps 30 --frames 100 --loop 2 --no-window --upload --sw") {
            Ok(Command::Play(o)) => {
                assert_eq!(o.fps, 30.0);
                assert_eq!(o.max_frames, Some(100));
                assert_eq!(o.loops, 2);
                assert!(!o.window && o.upload);
                assert_eq!(o.hw, HwKind::Software);
            }
            other => panic!("{other:?}"),
        }
        assert!(p("--play").is_err());
        assert!(p("--play a.h265 --fps 0").is_err());
        assert!(p("--profile x --play a.h265").is_err());
        assert!(matches!(p("--version"), Ok(Command::Version)));
    }
}
