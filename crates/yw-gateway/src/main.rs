//! yw-gateway：家庭网关。
//!
//! 在局域网里长期持有 PS5 的 Remote Play 会话：唤醒、建会话、看门狗、退避重试；
//! 没有客户端时一直发中性输入保持会话。M2a 起经 YWP（yw-serve）把画面转发给远端客户端；
//! keys/ 有问题或没有配对客户端时不起 YWP，行为和 M1 一样。
//!
//! 用法见 `yw-gateway --help`。在 macOS 上要装成 LaunchDaemon 运行（见 deploy/macos/），
//! 用户级 LaunchAgent 会被本地网络隐私拦截，连不上 PS5。

mod config;
mod control;
mod cred;
mod discovery;
mod handler;
mod hub;
mod jlog;
mod pairing;
mod serve_glue;
mod supervisor;

use config::{Cli, GatewayConf, HoldAction};
use std::fs::OpenOptions;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;
use supervisor::{Config, Supervisor};
use yw_chiaki_sys::Codec;

const USAGE: &str = "用法：yw-gateway --host <PS5 地址> [选项]
       yw-gateway keygen | pair …（配对子命令，见 yw-gateway pair --help）
  <目录>/gateway.conf（每行 key=value，# 开头是注释）里写了的项优先于命令行（PS5 换 IP 只改这个文件、重启网关，
  不用改 LaunchDaemon 的 plist）。认识的 key：host、bitrate_kbps、lan_max_kbps、loss_max、hold_minutes（0 = 一直持有）、
  hold_action（none | sleep）、ywp_port（默认 9420）、ywp_bind_if（网卡名，留空不绑）、ywp_takeover（0 | 1，默认 0）
  --cred <文件>        PS5 凭据（默认 ~/.config/yw/ps5.cred）
  --dir <目录>         日志、控制口、锁文件所在目录（默认 ~/yw-gw）
  --res 1080|720|540|360   分辨率（默认 1080）
  --fps 60|30          帧率（默认 60）
  --bitrate <kbps>     申请码率（默认 15000；PS5 走 Wi-Fi 时不要超过 15000）
  --codec h265|h264|hdr    编码（默认 h265）
  --ls-mtu <字节>      覆盖 LaunchSpec 的 mtu（默认 1236：PS5 单元 1184 B，正好放进 YWP 一个符号）；0 表示不覆盖
  --loss-max <0~1>     上报给 PS5 的丢包率上限（默认 0.03）
  --no-wake            PS5 待机时不唤醒
  --stats <秒>         统计间隔（默认 10）
  --hold <分钟>        没有客户端时最多保持会话多久，到期结束会话并暂停（默认 0 = 不限；M1 还没有客户端）
  --hold-sleep         保持期满时顺便让 PS5 休眠
  --quiet-stderr       stderr 只写错误和警告（LaunchDaemon 用：stderr.log 不轮转，完整日志在 gateway.jsonl）
控制口：echo status | nc -U <目录>/control.sock
暂停（pause、sleep、PS5 主动结束串流、登录 PIN 有问题）会写 <目录>/paused，网关重启后仍暂停，resume 解除。";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if matches!(args.first().map(String::as_str), Some("keygen" | "pair")) {
        if args.iter().any(|a| a == "-h" || a == "--help") {
            println!("{}", pairing::USAGE);
            return;
        }
        std::process::exit(pairing::run(&args, &PathBuf::from(&home).join("yw-gw")));
    }
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    let mut cli = Cli::default();
    let mut cred_path = PathBuf::from(&home).join(".config/yw/ps5.cred");
    let mut dir = PathBuf::from(&home).join("yw-gw");
    let mut cfg = Config {
        host: String::new(),
        height: 1080,
        fps: 60,
        bitrate_kbps: 15000,
        codec: Codec::H265,
        ls_mtu: 1236,
        loss_max: 0.03,
        wake: true,
        stats_interval: Duration::from_secs(10),
        base_dir: PathBuf::new(),
        hold: None,
        hold_sleep: false,
    };
    let mut quiet_stderr = false;
    let mut i = 0;
    let need = |i: usize| -> &String {
        args.get(i + 1).unwrap_or_else(|| {
            eprintln!("{} 缺参数\n{USAGE}", args[i]);
            std::process::exit(2)
        })
    };
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--host" => cli.host = Some(need(i).clone()),
            "--cred" => cred_path = PathBuf::from(need(i)),
            "--dir" => dir = PathBuf::from(need(i)),
            "--res" => cfg.height = need(i).parse().unwrap_or(1080),
            "--fps" => cfg.fps = need(i).parse().unwrap_or(60),
            "--bitrate" => cli.bitrate_kbps = need(i).parse().ok(),
            "--codec" => {
                cfg.codec = match need(i).as_str() {
                    "h264" => Codec::H264,
                    "hdr" => Codec::H265Hdr,
                    _ => Codec::H265,
                }
            }
            "--ls-mtu" => cfg.ls_mtu = need(i).parse().unwrap_or(1236),
            "--loss-max" => cli.loss_max = need(i).parse().ok(),
            "--stats" => cfg.stats_interval = Duration::from_secs(need(i).parse().unwrap_or(10)),
            "--hold" => cli.hold_minutes = Some(need(i).parse().unwrap_or(0)),
            "--no-wake" | "--hold-sleep" | "--quiet-stderr" => {
                match a {
                    "--no-wake" => cfg.wake = false,
                    "--hold-sleep" => cli.hold_sleep = true,
                    _ => quiet_stderr = true,
                }
                i += 1;
                continue;
            }
            _ => {
                eprintln!("不认识的参数：{a}\n{USAGE}");
                std::process::exit(2);
            }
        }
        i += 2;
    }
    // gateway.conf 优先：PS5 换 IP 时不必改 /Library/LaunchDaemons 里的 plist（那要 sudo）
    let (conf, sources) = GatewayConf::load(&dir.join("gateway.conf"), &cli);
    let Some(host) = conf.host.clone() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    cfg.host = host;
    cfg.bitrate_kbps = conf.bitrate_kbps;
    cfg.loss_max = conf.loss_max;
    // M2a 仍按 M1：哪里都没写 hold_minutes 时不限时（M2d 起默认 10 分钟）；0 也是不限时
    cfg.hold = conf.hold_minutes.filter(|&m| m > 0).map(|m| Duration::from_secs(m as u64 * 60));
    cfg.hold_sleep = conf.hold_action == HoldAction::Sleep;

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("建不了目录 {}：{e}", dir.display());
        std::process::exit(1);
    }
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    cfg.base_dir = dir.clone();

    // 单实例：同一时刻只能有一个网关持有 PS5 会话
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("gateway.lock"));
    let lock = match lock {
        Ok(f) => f,
        Err(e) => {
            eprintln!("打不开锁文件：{e}");
            std::process::exit(1);
        }
    };
    if lock.try_lock().is_err() {
        eprintln!("已经有一个 yw-gateway 在运行（{}）", dir.join("gateway.lock").display());
        std::process::exit(1);
    }

    let log = Arc::new(jlog::Logger::open(dir.join("gateway.jsonl")));
    log.set_quiet_stderr(quiet_stderr);
    let cred = match cred::load(&cred_path) {
        Ok(c) => c,
        Err(e) => {
            log.error("gateway", &e);
            std::process::exit(1);
        }
    };
    log.info("gateway", &format!("yw-gateway {} 启动，主机「{}」", env!("CARGO_PKG_VERSION"), cred.nickname));
    let host_src = sources.iter().find(|(k, _)| k == "host").map(|(_, s)| s.label()).unwrap_or("?");
    log.info("gateway", &format!("PS5 地址 {}（来自{host_src}）", cfg.host));
    log.info("gateway", &format!("配置：{}", conf.summary(&sources)));
    for w in &conf.warnings {
        log.warn("gateway", w);
    }
    yw_chiaki_sys::install_signal_handlers();

    let (tx, rx) = mpsc::channel();
    // YWP：keys/ 不对或没有配对客户端时不起，PS5 会话照常（serve_glue 里记日志）
    let hub = Arc::new(hub::Hub::new(hub::random_boot_id()));
    let glue = serve_glue::start(&conf, &dir, hub.clone(), log.clone());
    let route = control::YwpRoute { serve: glue.serve.clone(), why_not: glue.why_not.clone() };
    if let Err(e) = control::spawn(dir.join("control.sock"), tx, route, log.clone()) {
        log.error("control", &format!("控制口启动失败：{e}"));
    }
    Supervisor::new(cfg, cred, log, hub, glue).run(rx);
    let _ = std::fs::remove_file(dir.join("control.sock"));
    drop(lock);
}
