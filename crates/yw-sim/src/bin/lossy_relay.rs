//! lossy-relay（F 组，M2c）：本机 UDP 损伤中继。
//! 经中继测试时，客户端的 bind_if 必须留空（客户端发 127.0.0.1，绑物理网卡就发不到）。
//! 第 0 步骨架：只解析参数、打印用法。

use std::net::SocketAddr;
use std::process::ExitCode;

const USAGE: &str = "\
用法：
  lossy-relay --listen 127.0.0.1:<端口> --upstream <网关地址:端口> --bind-if <网卡>
              --down <模型> --up <模型> [--outage-up <起点+时长>]
      --upstream：例如 192.0.2.10:9420
      --bind-if：上游那一侧绑哪块网卡（例如 en0）
      <模型>：none | bern:<丢包率> | ge:<p>,<r>,<h>,<k> | size:<base>,<per_byte>，可加 @<时延ms>[~<抖动ms>]
      --outage-up：上行中断，例如 10s+3s（第 10 秒起中断 3 秒）
";

// 第 0 步骨架：字段还没用上，F 组实现时去掉 allow
#[allow(dead_code)]
#[derive(Debug)]
struct Args {
    listen: SocketAddr,
    upstream: SocketAddr,
    bind_if: String,
    down: String,
    up: String,
    outage_up: Option<String>,
}

fn parse(args: &[String]) -> Result<Option<Args>, String> {
    let (mut listen, mut upstream, mut bind_if, mut down, mut up, mut outage_up) = (None, None, None, None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} 缺少值"));
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--listen" => {
                let v = val("--listen")?;
                listen = Some(v.parse::<SocketAddr>().map_err(|_| format!("--listen 不是地址:端口：{v}"))?);
            }
            "--upstream" => {
                let v = val("--upstream")?;
                upstream = Some(v.parse::<SocketAddr>().map_err(|_| format!("--upstream 不是地址:端口：{v}"))?);
            }
            "--bind-if" => bind_if = Some(val("--bind-if")?),
            "--down" => down = Some(val("--down")?),
            "--up" => up = Some(val("--up")?),
            "--outage-up" => outage_up = Some(val("--outage-up")?),
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    Ok(Some(Args {
        listen: listen.ok_or("缺少 --listen")?,
        upstream: upstream.ok_or("缺少 --upstream")?,
        bind_if: bind_if.ok_or("缺少 --bind-if")?,
        down: down.ok_or("缺少 --down")?,
        up: up.ok_or("缺少 --up")?,
        outage_up,
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
