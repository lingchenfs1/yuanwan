//! 绑网卡的 UDP socket（A 组，feature = "net"）。
//!
//! - IP_BOUND_IF（macOS 上 socket2 的 bind_device_by_index_v4 就是它），网卡名从配置读；
//! - IP_DONTFRAG = 28（macOS），4 MB 收发缓冲，非阻塞；
//! - 不改系统的代理和网络设置。默认路由走隧道网卡、又没有指定网卡时，只给提示。

use std::io;
use std::net::{SocketAddr, UdpSocket};

/// ifname 为空时不绑网卡（只给 lossy-relay 经 127.0.0.1 的测试用）
pub fn bound_udp(local: SocketAddr, ifname: &str) -> io::Result<UdpSocket> {
    todo!()
}

/// send_to 的结果分类
#[derive(Debug)]
pub enum SendOutcome {
    Sent,
    /// 注册 WRITABLE，等可写再发
    WouldBlock,
    /// ENOBUFS：瞬时错误，计数，1 ms 后重试，连续 3 次失败丢掉这一包
    NoBufs,
    Fatal(io::Error),
}

pub fn classify_send_err(e: &io::Error) -> SendOutcome {
    todo!()
}
