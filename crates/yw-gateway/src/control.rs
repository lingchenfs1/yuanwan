//! 本地控制口：Unix socket，一行命令、一行回复。
//! 例：`echo status | nc -U ~/yw-gw/control.sock`
//!
//! 会话类命令（交监督线程）：status | idr | snap <文件> | record <文件> <秒> | inject <丢包率> | sleep | pause | resume | restart | quit
//! YWP 类命令（交 ywp 线程，经 MediaMsg::Control）：clients | kick <名字或指纹> | clients-reload

use crate::jlog::Logger;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use yw_serve::{MediaMsg, ServeHandle};

/// 命令、回复通道、截止时间（过了截止时间控制口已经回复超时，监督线程不再执行）。就是 yw_serve::CtlRequest
pub type Request = yw_serve::CtlRequest;

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// 交给 ywp 线程的命令（第一个词）
const YWP_COMMANDS: &[&str] = &["clients", "kick", "clients-reload"];

/// YWP 类命令往哪里送
#[derive(Clone)]
pub struct YwpRoute {
    pub serve: Option<Arc<ServeHandle>>,
    /// 没启动 YWP 的原因
    pub why_not: String,
}

fn is_ywp_command(cmd: &str) -> bool {
    cmd.split_whitespace().next().is_some_and(|w| YWP_COMMANDS.contains(&w))
}

pub fn spawn(path: PathBuf, tx: Sender<Request>, ywp: YwpRoute, log: Arc<Logger>) -> std::io::Result<()> {
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    log.info("control", &format!("控制口：{}", path.display()));
    std::thread::Builder::new().name("control".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).is_err() {
                continue;
            }
            let cmd = line.trim().to_string();
            if cmd.is_empty() {
                continue;
            }
            let (rtx, rrx) = mpsc::channel();
            let req = (cmd, rtx, Instant::now() + REPLY_TIMEOUT);
            let sent = if is_ywp_command(&req.0) {
                match &ywp.serve {
                    Some(sv) => {
                        if sv.push(MediaMsg::Control(req)) {
                            Ok(())
                        } else {
                            Err("YWP 服务忙（队列满），稍后再试".to_string())
                        }
                    }
                    None => Err(format!("YWP 没有启动：{}", ywp.why_not)),
                }
            } else {
                tx.send(req).map_err(|_| "网关正在退出".to_string())
            };
            let reply = match sent {
                Ok(()) => rrx.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| "超时：网关没有回复（命令不会再执行）".to_string()),
                Err(e) => e,
            };
            let _ = writeln!(stream, "{reply}");
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn ywp_commands_routed_by_first_word() {
        assert!(super::is_ywp_command("clients"));
        assert!(super::is_ywp_command("kick c0"));
        assert!(super::is_ywp_command("  clients-reload "));
        assert!(!super::is_ywp_command("status"));
        assert!(!super::is_ywp_command("clientsx"));
        assert!(!super::is_ywp_command(""));
    }
}
