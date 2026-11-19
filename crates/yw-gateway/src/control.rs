//! 本地控制口：Unix socket，一行命令、一行回复。
//! 例：`echo status | nc -U ~/yw-gw/control.sock`
//!
//! 命令：status | idr | snap <文件> | record <文件> <秒> | inject <丢包率> | sleep | pause | resume | restart | quit

use crate::jlog::Logger;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 命令、回复通道、截止时间（过了截止时间控制口已经回复超时，监督线程不再执行）
pub type Request = (String, Sender<String>, Instant);

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

pub fn spawn(path: PathBuf, tx: Sender<Request>, log: Arc<Logger>) -> std::io::Result<()> {
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
            let reply = if tx.send((cmd, rtx, Instant::now() + REPLY_TIMEOUT)).is_err() {
                "网关正在退出".to_string()
            } else {
                rrx.recv_timeout(REPLY_TIMEOUT).unwrap_or_else(|_| "超时：网关没有回复（命令不会再执行）".to_string())
            };
            let _ = writeln!(stream, "{reply}");
        }
    })?;
    Ok(())
}
