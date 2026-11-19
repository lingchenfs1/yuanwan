//! macOS：线程 QoS、防 App Nap 合并定时器、默认路由检查。

/// 当前线程设成 QOS_CLASS_USER_INTERACTIVE
pub fn set_thread_qos_interactive() {
    todo!()
}

/// 持有期间系统不做 App Nap、不合并定时器（drop 时结束）
pub struct ActivityToken {
    _priv: (),
}

/// 防 App Nap 合并定时器
pub fn begin_latency_critical_activity() -> ActivityToken {
    todo!()
}

/// 默认路由是不是走隧道网卡（utun*）；是的话提示「请指定 bind_if」，不去改代理
pub fn default_route_is_tunnel() -> bool {
    todo!()
}
