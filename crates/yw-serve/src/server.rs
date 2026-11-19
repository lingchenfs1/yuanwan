//! 服务循环（ywp 线程）：mio::Poll 等 UDP socket（绑配置里的网卡）和 Waker；
//! 超时取所有状态机 next_timeout 的最小值。每轮：收完 socket → 取完 MediaMsg → 跑计时器 → 组包、加密、send_to。
//! WouldBlock 注册 WRITABLE；ENOBUFS 计数、1 ms 后重试、连续 3 次失败丢包，不中断事件循环。

use crate::source::{MediaMsg, PsControl, ServeEvent};
use std::io;
use std::sync::mpsc::{Sender, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;
use ywp::creds::Keys;
use ywp::endpoint::TakeoverPolicy;

/// 日志出口。ywp 线程写 JSONL 一律经它；实现方要用有界队列交给日志线程，满了丢弃并计数，不能阻塞
pub trait JsonlSink: Send + Sync {
    /// 结构化记录：fields 是拼好的 `"k":v,...`（同 yw-gateway 的 jlog::Logger::record）
    fn record(&self, kind: &str, fields: &str);
    /// 人读的日志：level 为 'I'、'W'、'E'
    fn log(&self, level: char, kind: &str, msg: &str);
}

pub struct ServeConfig {
    /// gateway.conf 的 ywp_port（默认 9420）
    pub port: u16,
    /// gateway.conf 的 ywp_bind_if；空串表示不绑网卡
    pub bind_if: String,
    pub takeover: TakeoverPolicy,
    pub log: Arc<dyn JsonlSink>,
}

/// MediaMsg 队列的容量（sync_channel）
pub const MEDIA_QUEUE_CAP: usize = 1024;

pub fn spawn(
    cfg: ServeConfig,
    keys: Keys,
    ctl: Arc<dyn PsControl>,
    events: Sender<ServeEvent>,
) -> io::Result<ServeHandle> {
    todo!()
}

pub struct ServeHandle {
    tx: SyncSender<MediaMsg>,
    waker: Arc<mio::Waker>,
    pub join: JoinHandle<()>,
}

impl ServeHandle {
    /// 在 chiaki 线程上调：try_send + wake，队列满了返回 false（视频回调据此返回 false）
    pub fn push(&self, m: MediaMsg) -> bool {
        match self.tx.try_send(m) {
            Ok(()) => {
                let _ = self.waker.wake();
                true
            }
            Err(_) => false,
        }
    }
}
