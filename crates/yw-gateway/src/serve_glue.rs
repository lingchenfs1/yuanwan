//! serve_glue：起 yw-serve（YWP 服务），把 ywp 线程的日志经有界队列交给日志线程，ServeEvent 交给监督线程。
//!
//! 以下情况不起 YWP，只记日志；PS5 会话照常由监督线程管理，行为和 M1 一样：
//! - keys/ 不存在（还没运行 `yw-gateway keygen`）；
//! - keys/ 的权限不对（目录不是 700、文件不是 600；不自动改），或文件格式不对；
//! - 还没有配对的客户端；
//! - socket 绑不上（端口被占、网卡不存在）。

use crate::config::GatewayConf;
use crate::hub::Hub;
use crate::jlog::Logger;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;
use yw_serve::{JsonlSink, PsControl, ServeConfig, ServeEvent, ServeHandle, ServeOptions};
use ywp::endpoint::TakeoverPolicy;

/// ywp 线程 → 日志线程的队列长度；满了丢弃并计数（ywp 线程不能被磁盘拖慢）
const LOG_QUEUE: usize = 1024;

pub struct Glue {
    pub serve: Option<Arc<ServeHandle>>,
    /// 没起 YWP 的原因（控制口 status 和 YWP 类命令的回复里显示）
    pub why_not: String,
    pub events: Receiver<ServeEvent>,
}

enum Item {
    Record(String, String),
    Log(char, String, String),
}

/// JsonlSink 的实现：try_send 进有界队列，日志线程写 Logger
struct QueuedSink {
    tx: SyncSender<Item>,
    dropped: Arc<AtomicU64>,
}

impl QueuedSink {
    fn new(log: Arc<Logger>) -> Self {
        let (tx, rx) = mpsc::sync_channel::<Item>(LOG_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let d = dropped.clone();
        let spawned = std::thread::Builder::new().name("ywp-log".into()).spawn(move || {
            // 所有发送端都丢了（服务循环退出）时结束
            for item in rx.iter() {
                match item {
                    Item::Record(kind, fields) => log.record(&kind, &fields),
                    Item::Log(level, kind, msg) => log.log(level, &kind, &msg),
                }
                let n = d.swap(0, Relaxed);
                if n > 0 {
                    log.warn("ywp", &format!("日志队列满，丢了 {n} 条 YWP 日志"));
                }
            }
        });
        if let Err(e) = spawned {
            // 起不了日志线程：队列很快就满，之后的日志全部丢弃并计数，服务照常
            eprintln!("起不了 YWP 日志线程：{e}");
        }
        Self { tx, dropped }
    }

    fn put(&self, item: Item) {
        if self.tx.try_send(item).is_err() {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
}

impl JsonlSink for QueuedSink {
    fn record(&self, kind: &str, fields: &str) {
        self.put(Item::Record(kind.to_string(), fields.to_string()));
    }
    fn log(&self, level: char, kind: &str, msg: &str) {
        self.put(Item::Log(level, kind.to_string(), msg.to_string()));
    }
}

/// 起 YWP 服务。失败或条件不满足时 serve 为 None，原因写进 why_not 并记日志
pub fn start(conf: &GatewayConf, base_dir: &Path, hub: Arc<Hub>, log: Arc<Logger>) -> Glue {
    let (etx, erx) = mpsc::channel();
    let fail = |why: String, level: char| {
        log.log(level, "ywp", &format!("不启动 YWP：{why}（PS5 会话照常）"));
        Glue { serve: None, why_not: why, events: mpsc::channel().1 }
    };
    let dir = base_dir.join("keys");
    if !dir.exists() {
        return fail(format!("没有 {}（还没运行 yw-gateway keygen）", dir.display()), 'I');
    }
    let keys = match ywp::creds::load_keys(&dir) {
        Ok(k) => k,
        Err(e) => return fail(format!("keys/ 加载失败：{e}"), 'E'),
    };
    if keys.clients.is_empty() {
        return fail("还没有配对的客户端（yw-gateway pair add 之后重启网关）".into(), 'W');
    }
    let paired = keys.clients.len();
    let cfg = ServeConfig {
        port: conf.ywp_port,
        bind_if: conf.ywp_bind_if.clone(),
        takeover: if conf.ywp_takeover { TakeoverPolicy::Auto } else { TakeoverPolicy::RequireFlag },
        log: Arc::new(QueuedSink::new(log.clone())),
    };
    let opts = ServeOptions { keys_dir: Some(dir), ..ServeOptions::default() };
    let ctl: Arc<dyn PsControl> = hub;
    match yw_serve::spawn_with(cfg, opts, keys, ctl, etx) {
        Ok(h) => {
            let bind = if conf.ywp_bind_if.is_empty() { "不绑网卡".to_string() } else { format!("绑 {}", conf.ywp_bind_if) };
            log.info("ywp", &format!("YWP 已启动：UDP {}，{bind}，已配对 {paired} 个客户端", h.local_addr().port()));
            Glue { serve: Some(Arc::new(h)), why_not: String::new(), events: erx }
        }
        Err(e) => fail(format!("UDP {} 绑不上：{e}", conf.ywp_port), 'E'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Cli;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("yw-glue-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            // 只删本测试自己建的临时目录
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn conf() -> GatewayConf {
        // 端口 0：让系统挑，不和本机别的程序冲突
        GatewayConf::merge("ywp_port=0\n", &Cli::default()).0
    }

    fn logger(t: &TmpDir) -> Arc<Logger> {
        Arc::new(Logger::open(t.0.join("gateway.jsonl")))
    }

    #[test]
    fn no_keys_dir_means_no_ywp() {
        let t = TmpDir::new("nokeys");
        let g = start(&conf(), &t.0, Arc::new(Hub::new(1)), logger(&t));
        assert!(g.serve.is_none());
        assert!(g.why_not.contains("keygen"), "{}", g.why_not);
    }

    #[test]
    fn loose_permissions_mean_no_ywp() {
        let t = TmpDir::new("perm");
        let keys = t.0.join("keys");
        ywp::creds::init_keys(&keys).unwrap();
        std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o755)).unwrap();
        let g = start(&conf(), &t.0, Arc::new(Hub::new(1)), logger(&t));
        assert!(g.serve.is_none());
        assert!(g.why_not.contains("755"), "{}", g.why_not);
        let jl = std::fs::read_to_string(t.0.join("gateway.jsonl")).unwrap();
        assert!(jl.contains("不启动 YWP"), "{jl}");
    }

    #[test]
    fn no_paired_clients_means_no_ywp() {
        let t = TmpDir::new("noclients");
        ywp::creds::init_keys(&t.0.join("keys")).unwrap();
        let g = start(&conf(), &t.0, Arc::new(Hub::new(1)), logger(&t));
        assert!(g.serve.is_none());
        assert!(g.why_not.contains("配对"), "{}", g.why_not);
    }

    #[test]
    fn starts_with_paired_client_and_shuts_down() {
        let t = TmpDir::new("ok");
        let keys = t.0.join("keys");
        ywp::creds::init_keys(&keys).unwrap();
        let psk = ywp::creds::add_client(&keys, "c0", &[7; 32]).unwrap();
        let g = start(&conf(), &t.0, Arc::new(Hub::new(1)), logger(&t));
        let sv = g.serve.expect("YWP 启动了");
        assert_ne!(sv.local_addr().port(), 0);
        assert!(!sv.client_present());
        // 等 status 刷一次
        std::thread::sleep(std::time::Duration::from_millis(300));
        let st = sv.status();
        assert!(st.running);
        assert_eq!(st.paired, 1);
        assert!(sv.shutdown(std::time::Duration::from_millis(500)));
        std::thread::sleep(std::time::Duration::from_millis(50));
        let jl = std::fs::read_to_string(t.0.join("gateway.jsonl")).unwrap();
        assert!(jl.contains("YWP 已启动"), "{jl}");
        assert!(!jl.contains(&ywp::creds::to_hex(psk.expose())), "日志里不能有 PSK");
    }
}
