//! 单播查询 PS5 状态（UDP 9302 SRCH）。广播在跨网段时收不到，所以只用单播。

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostState {
    /// 开机，可以建会话（HTTP 200）
    Ready,
    /// 待机（HTTP 620），可以唤醒
    Standby,
    /// 没有回应（关机、掉线，或网络不通）
    NoReply,
}

impl HostState {
    pub fn name(self) -> &'static str {
        match self {
            HostState::Ready => "开机",
            HostState::Standby => "待机",
            HostState::NoReply => "无回应",
        }
    }
}

const SRCH: &[u8] = b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n";

fn bind() -> std::io::Result<UdpSocket> {
    // chiaki 先试 9303~9319（pyremoteplay 注释说 PS5 需要），都被占了再用随机端口
    for port in 9303..=9319 {
        if let Ok(s) = UdpSocket::bind(("0.0.0.0", port)) {
            return Ok(s);
        }
    }
    UdpSocket::bind(("0.0.0.0", 0))
}

pub fn query(host: &str, timeout: Duration) -> HostState {
    let Ok(addr) = (host, 9302u16).to_socket_addrs().map(|mut a| a.find(SocketAddr::is_ipv4)) else {
        return HostState::NoReply;
    };
    let Some(addr) = addr else { return HostState::NoReply };
    let Ok(sock) = bind() else { return HostState::NoReply };
    let _ = sock.set_read_timeout(Some(timeout));
    if sock.send_to(SRCH, addr).is_err() {
        return HostState::NoReply;
    }
    let mut buf = [0u8; 2048];
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if from.ip() == addr.ip() => {
                let text = String::from_utf8_lossy(&buf[..n]);
                let first = text.lines().next().unwrap_or("");
                if first.contains(" 200") {
                    return HostState::Ready;
                }
                if first.contains(" 620") {
                    return HostState::Standby;
                }
                return HostState::NoReply;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    HostState::NoReply
}
