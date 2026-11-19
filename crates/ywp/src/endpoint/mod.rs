//! 两端的端点状态机（B 组）：握手、加密、PN、CONTROL、调度都在这里汇合。
//! 调用方喂数据报和 now，取要发的包（poll_transmit）和下一次超时（next_timeout）。

pub mod client;
pub mod gateway;

pub use client::{ClientConfig, ClientConn, ClientEvent};
pub use gateway::{GatewayConfig, Gateway, GwConn, GwEvent, Liveness, TakeoverPolicy};

use crate::error::DropCounters;
use crate::time::Micros;
use std::net::SocketAddr;

/// poll_transmit 的结果：包已写进调用方给的缓冲
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transmit {
    pub dst: SocketAddr,
    pub len: usize,
    /// 这个包装的是哪个视频符号 (fid, sym)，发出后要回报给 VideoTx::on_sent
    pub video: Option<(u16, u16)>,
}

/// 收到的 DATA 包的元数据
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxMeta {
    pub pn: u64,
    pub send_ts: u32,
    /// 收包时刻（net 线程收包后立即打的单调时间戳）
    pub arrival: Micros,
}

/// 连接统计
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ConnStats {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    /// 本次连接用了几次 HS_INIT 尝试
    pub hs_attempts: u32,
    pub srtt_us: Micros,
    pub drops: DropCounters,
}
