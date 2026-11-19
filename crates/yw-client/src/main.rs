//! yw-client 命令行（第 0 步骨架：只解析参数、打印用法和库版本，各功能由 E 组实现）。

use std::process::ExitCode;

const USAGE: &str = "\
用法：
  yw-client [--profile <名字>]
      有界面，正常使用
  yw-client --headless --connect-once --duration <秒> --json [--profile <名字>] [--start]
      无界面连一次，结束时向 stdout 输出一行 JSON：
      { ok, hs_attempts, fail_reason, first_frame_ms, freezes, freeze_total_ms, w_frames, l2, l3 }
      --start：连上后发 PS5_CMD START（网关在暂停时也恢复并唤醒 PS5）
  yw-client keygen
      生成本机密钥，打印公钥
  yw-client pair import
      从 stdin 读配对串（不回显，只打印网关名和指纹）
  yw-client --version
      打印 SDL、FFmpeg、Opus 的运行时版本
";

#[allow(dead_code)]
#[derive(Debug, Default)]
struct Headless {
    connect_once: bool,
    duration_s: Option<u32>,
    json: bool,
    start: bool,
}

// 第 0 步骨架：字段还没用上，E 组实现时去掉 allow
#[allow(dead_code)]
#[derive(Debug)]
enum Command {
    Gui { profile: Option<String> },
    Headless { profile: Option<String>, opts: Headless },
    Keygen,
    PairImport,
    Version,
    Help,
}

fn parse(args: &[String]) -> Result<Command, String> {
    match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 1 => return Ok(Command::Keygen),
        Some("pair") if args.len() == 2 && args[1] == "import" => return Ok(Command::PairImport),
        Some("keygen") | Some("pair") => return Err("子命令参数不对".into()),
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
            "--profile" => profile = Some(it.next().ok_or("--profile 缺少名字")?.clone()),
            "--headless" => headless = true,
            "--connect-once" => opts.connect_once = true,
            "--duration" => {
                let v = it.next().ok_or("--duration 缺少秒数")?;
                opts.duration_s = Some(v.parse().map_err(|_| format!("--duration 不是整数：{v}"))?);
            }
            "--json" => opts.json = true,
            "--start" => opts.start = true,
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(Command::Help) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("yw-client {}", env!("CARGO_PKG_VERSION"));
            print_versions();
            ExitCode::SUCCESS
        }
        Ok(cmd) => {
            eprintln!("第 0 步骨架，还没实现：{cmd:?}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("{e}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
