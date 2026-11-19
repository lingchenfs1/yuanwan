//! 绑网卡的 UDP socket（A 组，feature = "net"）。
//!
//! - IP_BOUND_IF（macOS 上 socket2 的 bind_device_by_index_v4 / _v6 就是它），网卡名从配置读；
//! - 不分片（macOS：IP_DONTFRAG = 28、IPV6_DONTFRAG = 62；Linux：IP_MTU_DISCOVER = IP_PMTUDISC_DO），
//!   4 MB 收发缓冲（系统不给就逐级减半，最少 256 KB），非阻塞；
//! - 不改系统的代理和网络设置。默认路由走隧道网卡、又没有指定网卡时，只给提示（调用方做）。

#![warn(unused_variables, dead_code)]

use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{SocketAddr, UdpSocket};

/// 收发缓冲的目标大小
const SOCK_BUF: usize = 4 << 20;
const SOCK_BUF_MIN: usize = 256 << 10;

#[cfg(target_vendor = "apple")]
const IPV6_DONTFRAG: libc::c_int = 62;

/// 网卡名 → 索引（if_nametoindex）。不存在返回 NotFound
pub fn if_index(ifname: &str) -> io::Result<std::num::NonZeroU32> {
    let c = std::ffi::CString::new(ifname).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "网卡名含 NUL"))?;
    // SAFETY: c 是以 NUL 结尾的有效 C 字符串
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    std::num::NonZeroU32::new(idx)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("找不到网卡 {ifname}")))
}

fn set_int_opt(sock: &Socket, level: libc::c_int, name: libc::c_int, v: libc::c_int) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: fd 有效；optval 指向一个 c_int，长度一致
    let r = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            level,
            name,
            (&v as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// 不分片：DF 置位，超过路径 MTU 的包直接报错（EMSGSIZE），不在 IP 层切
fn set_dont_frag(sock: &Socket, v6: bool) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    {
        if v6 {
            set_int_opt(sock, libc::IPPROTO_IPV6, IPV6_DONTFRAG, 1)
        } else {
            set_int_opt(sock, libc::IPPROTO_IP, libc::IP_DONTFRAG, 1)
        }
    }
    #[cfg(target_os = "linux")]
    {
        if v6 {
            set_int_opt(sock, libc::IPPROTO_IPV6, libc::IPV6_MTU_DISCOVER, libc::IPV6_PMTUDISC_DO)
        } else {
            set_int_opt(sock, libc::IPPROTO_IP, libc::IP_MTU_DISCOVER, libc::IP_PMTUDISC_DO)
        }
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        let _ = (sock, v6);
        Ok(())
    }
}

fn set_buffers(sock: &Socket) {
    let mut n = SOCK_BUF;
    while n >= SOCK_BUF_MIN && sock.set_recv_buffer_size(n).is_err() {
        n /= 2;
    }
    let mut n = SOCK_BUF;
    while n >= SOCK_BUF_MIN && sock.set_send_buffer_size(n).is_err() {
        n /= 2;
    }
}

/// ifname 为空时不绑网卡（只给经 127.0.0.1 的损伤中继测试用）
pub fn bound_udp(local: SocketAddr, ifname: &str) -> io::Result<UdpSocket> {
    let sock = Socket::new(Domain::for_address(local), Type::DGRAM, Some(Protocol::UDP))?;
    let v6 = local.is_ipv6();
    if !ifname.is_empty() {
        let idx = if_index(ifname)?;
        bind_if(&sock, v6, idx)?;
    }
    set_dont_frag(&sock, v6)?;
    set_buffers(&sock);
    sock.set_nonblocking(true)?;
    sock.bind(&local.into())?;
    Ok(sock.into())
}

#[cfg(any(target_vendor = "apple", target_os = "linux", target_os = "android"))]
fn bind_if(sock: &Socket, v6: bool, idx: std::num::NonZeroU32) -> io::Result<()> {
    if v6 {
        sock.bind_device_by_index_v6(Some(idx))
    } else {
        sock.bind_device_by_index_v4(Some(idx))
    }
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn bind_if(_sock: &Socket, _v6: bool, _idx: std::num::NonZeroU32) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "这个平台不支持按网卡绑定"))
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

/// 发送出错时分类（成功时调用方直接用 Sent）
pub fn classify_send_err(e: &io::Error) -> SendOutcome {
    if e.kind() == io::ErrorKind::WouldBlock {
        return SendOutcome::WouldBlock;
    }
    match e.raw_os_error() {
        Some(libc::ENOBUFS) => SendOutcome::NoBufs,
        Some(code) => SendOutcome::Fatal(io::Error::from_raw_os_error(code)),
        None => SendOutcome::Fatal(io::Error::new(e.kind(), e.to_string())),
    }
}

/// 发一个包并分类结果
pub fn send_classified(sock: &UdpSocket, pkt: &[u8], dst: SocketAddr) -> SendOutcome {
    match sock.send_to(pkt, dst) {
        Ok(_) => SendOutcome::Sent,
        Err(e) => classify_send_err(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        assert!(matches!(classify_send_err(&io::Error::from_raw_os_error(libc::ENOBUFS)), SendOutcome::NoBufs));
        assert!(matches!(classify_send_err(&io::Error::from(io::ErrorKind::WouldBlock)), SendOutcome::WouldBlock));
        assert!(matches!(
            classify_send_err(&io::Error::from_raw_os_error(libc::EAGAIN)),
            SendOutcome::WouldBlock
        ));
        assert!(matches!(classify_send_err(&io::Error::from_raw_os_error(libc::EHOSTUNREACH)), SendOutcome::Fatal(_)));
    }

    #[test]
    fn loopback_unbound() {
        let a = bound_udp("127.0.0.1:0".parse().unwrap(), "").unwrap();
        let b = bound_udp("127.0.0.1:0".parse().unwrap(), "").unwrap();
        let dst = b.local_addr().unwrap();
        assert!(matches!(send_classified(&a, b"ping", dst), SendOutcome::Sent));
        let mut buf = [0u8; 16];
        let mut got = None;
        for _ in 0..200 {
            match b.recv_from(&mut buf) {
                Ok((n, _)) => {
                    got = Some(n);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(got, Some(4));
        assert_eq!(&buf[..4], b"ping");
    }

    #[test]
    fn unknown_interface() {
        let e = bound_udp("127.0.0.1:0".parse().unwrap(), "no-such-if9").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(target_vendor = "apple")]
    #[test]
    fn bind_loopback_interface() {
        // lo0 在所有 macOS 上都有：绑上之后经 127.0.0.1 照样能收发
        let a = bound_udp("127.0.0.1:0".parse().unwrap(), "lo0").unwrap();
        let b = bound_udp("127.0.0.1:0".parse().unwrap(), "lo0").unwrap();
        assert!(matches!(send_classified(&a, b"x", b.local_addr().unwrap()), SendOutcome::Sent));
    }
}
