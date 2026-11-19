//! macOS：线程 QoS、防 App Nap 合并定时器、默认路由检查。

use std::ffi::{c_char, c_void, CStr};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

/// 当前线程设成 QOS_CLASS_USER_INTERACTIVE（失败只打一行日志，不影响运行）
pub fn set_thread_qos_interactive() {
    let r = unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0) };
    if r != 0 {
        eprintln!("设置线程 QoS 失败：{r}");
    }
}

// ---- Objective-C 运行时（只用到 NSProcessInfo 的 begin/endActivity，不引入额外依赖）----

type Id = *mut c_void;
type Sel = *mut c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_autoreleasePoolPush() -> *mut c_void;
    fn objc_autoreleasePoolPop(pool: *mut c_void);
}

#[link(name = "Foundation", kind = "framework")]
extern "C" {}

/// NSActivityUserInitiated | NSActivityLatencyCritical
const ACTIVITY_OPTIONS: u64 = 0x00FF_FFFF | 0xFF_0000_0000;

unsafe fn sel(name: &CStr) -> Sel {
    sel_registerName(name.as_ptr())
}

unsafe fn send0(obj: Id, s: Sel) -> Id {
    let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, s)
}

unsafe fn send_ptr(obj: Id, s: Sel, a: *const c_void) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, *const c_void) -> Id =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    f(obj, s, a)
}

/// 持有期间系统不做 App Nap、不合并定时器（drop 时结束）
pub struct ActivityToken {
    token: Id,
}

impl Drop for ActivityToken {
    fn drop(&mut self) {
        if self.token.is_null() {
            return;
        }
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let pi = send0(objc_getClass(c"NSProcessInfo".as_ptr()), sel(c"processInfo"));
            send_ptr(pi, sel(c"endActivity:"), self.token);
            send0(self.token, sel(c"release"));
            objc_autoreleasePoolPop(pool);
        }
    }
}

/// 防 App Nap 合并定时器
pub fn begin_latency_critical_activity() -> ActivityToken {
    unsafe {
        let pool = objc_autoreleasePoolPush();
        let pi = send0(objc_getClass(c"NSProcessInfo".as_ptr()), sel(c"processInfo"));
        let reason = send_ptr(
            objc_getClass(c"NSString".as_ptr()),
            sel(c"stringWithUTF8String:"),
            c"yw-client streaming".as_ptr().cast(),
        );
        let f: unsafe extern "C" fn(Id, Sel, u64, Id) -> Id = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        let token = f(pi, sel(c"beginActivityWithOptions:reason:"), ACTIVITY_OPTIONS, reason);
        // 返回的是 autorelease 对象，要自己 retain，drop 时 release
        if !token.is_null() {
            send0(token, sel(c"retain"));
        }
        objc_autoreleasePoolPop(pool);
        ActivityToken { token }
    }
}

/// 去往 `dst` 的包会从哪块网卡出去（UDP connect 只查路由表，不发包）。只支持 IPv4
pub fn route_iface(dst: IpAddr) -> Option<String> {
    let IpAddr::V4(_) = dst else { return None };
    let s = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))).ok()?;
    s.connect(SocketAddr::new(dst, 9)).ok()?;
    let IpAddr::V4(local) = s.local_addr().ok()?.ip() else { return None };
    iface_of(local)
}

/// 哪块网卡上配了这个 IPv4 地址
fn iface_of(addr: Ipv4Addr) -> Option<String> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut found = None;
    let mut p = list;
    while !p.is_null() {
        let ifa = unsafe { &*p };
        if !ifa.ifa_addr.is_null() && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_INET {
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            if Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)) == addr {
                found = Some(unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned());
                break;
            }
        }
        p = ifa.ifa_next;
    }
    unsafe { libc::freeifaddrs(list) };
    found
}

/// 隧道类网卡（VPN、代理的 TUN）
pub fn is_tunnel_iface(name: &str) -> bool {
    ["utun", "ipsec", "ppp", "tun", "tap"].iter().any(|p| name.starts_with(p))
}

/// 默认路由是不是走隧道网卡（utun*）；是的话提示「请指定 bind_if」，不去改代理
pub fn default_route_is_tunnel() -> bool {
    // 用文档保留地址（RFC 5737）查默认路由，不会真的发包
    route_iface(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))).is_some_and(|n| is_tunnel_iface(&n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qos_and_activity_do_not_crash() {
        set_thread_qos_interactive();
        let t = begin_latency_critical_activity();
        assert!(!t.token.is_null());
        drop(t);
    }

    #[test]
    fn loopback_routes_via_lo0() {
        assert_eq!(route_iface(IpAddr::V4(Ipv4Addr::LOCALHOST)).as_deref(), Some("lo0"));
        // 只检查能跑，结果取决于本机网络
        let _ = default_route_is_tunnel();
        assert!(is_tunnel_iface("utun3"));
        assert!(!is_tunnel_iface("en0"));
    }
}
