//! fakegw（F 组）：不链接 chiaki，用 yw-serve 的真服务代码（含加密、Ingress）回放录像。
//! 内部就是 yw_serve::spawn，加一个读文件的 MediaMsg 来源和一个 PsControl（request_idr → 跳到下一个关键帧）。
//! 第 0 步骨架：只解析参数、打印用法。

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
用法：
  fakegw --bind-if <网卡> --port <端口> --keys <目录> --stream <录像.h265|.ywf> [--loop]
      --bind-if：绑哪块网卡（例如 en0；空串表示不绑）
      --keys：keys/ 目录（目录 700、文件 600）
      --loop：放完从头再放
";

// 第 0 步骨架：字段还没用上，F 组实现时去掉 allow
#[allow(dead_code)]
#[derive(Debug)]
struct Args {
    bind_if: String,
    port: u16,
    keys: PathBuf,
    stream: PathBuf,
    looping: bool,
}

fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let (mut bind_if, mut port, mut keys, mut stream, mut looping) = (None, None, None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} 缺少值"));
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--bind-if" => bind_if = Some(val("--bind-if")?),
            "--port" => {
                let v = val("--port")?;
                port = Some(v.parse::<u16>().map_err(|_| format!("--port 不是端口号：{v}"))?);
            }
            "--keys" => keys = Some(PathBuf::from(val("--keys")?)),
            "--stream" => stream = Some(PathBuf::from(val("--stream")?)),
            "--loop" => looping = true,
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    Ok(Some(Args {
        bind_if: bind_if.ok_or("缺少 --bind-if")?,
        port: port.ok_or("缺少 --port")?,
        keys: keys.ok_or("缺少 --keys")?,
        stream: stream.ok_or("缺少 --stream")?,
        looping,
    }))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match parse(&args) {
        Ok(None) => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Some(a)) => {
            eprintln!("第 0 步骨架，还没实现：{a:?}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("{e}");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
