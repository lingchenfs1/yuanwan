//! fakegw（F 组）：不链接 chiaki 的假网关。读 M1 `record`、m0-probe `--dump` 录下的 .h265（开头带参数集），
//! 按帧率切成 AU，经 yw-serve 的真服务代码（握手、加密、Ingress、VideoTx）发给客户端，用来不碰 PS5 测客户端。
//!
//! - 视频来源：`yw_media::ywf::AnnexBReader` 按 AU 切分，每帧一条 `MediaMsg::Video`（seq 递增、rx_us 取送出时刻）；
//!   文件开头的参数集放进最新值槽，经 `MediaMsg::ParamsChanged` 通知，Ingress 据此生成 VIDEO_CONFIG（规格 §7.8）；
//! - `PsControl`：request_idr 经自持的 IdrGovernor 限频（规格 §12）；真正发出时，回放线程在下一帧之前
//!   `seek_next_key`，从下一个关键帧开始送。带 `--loop` 时读到文件尾就从头再找（M1 的录像往往只在开头有一个关键帧）；
//! - IdrGovernor 的 tick 由回放线程每 100 ms 调一次（网关里是监督线程），last_key_us 是最近一次送出关键帧的时刻。
//!   所有时刻都取 `yw_serve::clock::now_us()`，和服务循环同一个时钟；
//! - M2a 不转发声音和手柄：INPUT 只由服务循环计数，set_controller_* 什么都不做。
//!
//! 另有两个子命令，不用编 yw-gateway（它要链接 chiaki）也能在本机建 keys/、做配对。
//! keys/ 的格式和配对串都走 `ywp::creds`，和网关的 `yw-gateway keygen`、`pair add` 一样；
//! 配对串含 PSK，不进日志、不往终端输出。

use std::collections::hash_map::RandomState;
use std::fmt::Write as _;
use std::fs::File;
use std::hash::BuildHasher;
use std::io::{self, IsTerminal, Write};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use yw_media::nal::{self, Codec};
use yw_media::ywf::AnnexBReader;
use yw_serve::clock::now_us;
use yw_serve::{
    AudioHeader, IdrDecision, IdrGovernor, IdrReason, JsonlSink, MediaMsg, PsControl, ServeConfig, ServeEvent, ServeHandle,
    ServeOptions, StreamInfo,
};
use ywp::consts::{ps_state, video_codec};
use ywp::creds::ClientId;
use ywp::endpoint::TakeoverPolicy;
use ywp::time::{ms, Micros};
use ywp::wire::{InputState, SessionInfo};

const USAGE: &str = "\
用法：
  fakegw --keys <目录> --listen <地址:端口> --file <录像.h265> [--loop] [选项]
      回放录像给客户端（不碰 PS5）。stdout 先输出一行 {\"event\":\"listening\",…}，退出时再输出一行 summary；日志走 stderr
      --keys：keys/ 目录（目录 700、文件 600；fakegw keygen、pair add 生成）
      --listen：监听地址，例如 127.0.0.1:9420（本机回环测试时客户端档案不要设 bind_if）
      --file：M1 record 或 m0-probe --dump 录下的 Annex-B 录像，开头必须是参数集
      --loop：放完从头再放；不加时放完就退出
    选项：
      --fps <帧率>：默认 60
      --codec hevc|avc：默认 hevc
      --size <宽>x<高>：写进 VIDEO_CONFIG，默认 0x0（未知，客户端以参数集为准）
      --bind-if <网卡>：socket 绑哪块网卡（例如 en0），默认不绑
      --duration <秒>：跑这么久后给客户端发 CLOSE(5) 并退出，默认一直跑
      --takeover：别的客户端身份连进来时直接接管（默认回 BUSY，同网关的 ywp_takeover=0）
      --stop-file <文件>：这个文件出现时给客户端发 CLOSE(5)、输出 summary 后退出（脚本用它代替发信号）
  fakegw keygen --keys <目录>
      建 keys/（700）、网关静态密钥和 reset_key（600）；已有密钥时拒绝覆盖
  fakegw pair add <名字> <客户端公钥 64 位十六进制> --keys <目录> [--entry <地址:端口>] (--out <文件> | --stdout)
      配对一个客户端（yw-client pubkey 打印的公钥）。配对串含 PSK：--out 写成 600 权限的新文件；
      --stdout 只在 stdout 不是终端时可用，例如 … --stdout | yw-client pair import --name local
      fakegw 启动时读一次 keys/，配对要在启动之前做
";

// ---- 本程序的私有常量（规格里没有具体值的实现细节） ----

/// IdrGovernor::tick 的节拍（同网关监督线程的 100 ms 循环）
const IDR_TICK: Micros = ms(100);
/// 本程序自己的统计多久写一次（同服务循环的 ywp_stats）
const STATS_EVERY: Micros = ms(10_000);
/// stderr 日志队列长度：满了丢弃并计数（服务循环不能被 stderr 拖慢）
const LOG_QUEUE: usize = 1024;
/// 退出时最多等服务循环发完 3 份 CLOSE(5)
const SHUTDOWN_WAIT: Duration = Duration::from_millis(500);
/// 落后超过这么多帧（例如跳关键帧时读了很久）就不追了，从现在重新计时，免得一下子灌一串帧
const MAX_LAG_FRAMES: u64 = 2;
/// 配对串的入口提示最长多少字节（同配对串格式）
const ENTRY_MAX: usize = 255;

// ======================================================================
// 命令行
// ======================================================================

#[derive(Debug)]
struct RunArgs {
    keys: PathBuf,
    listen: SocketAddr,
    file: PathBuf,
    looping: bool,
    fps: u32,
    codec: Codec,
    size: (u16, u16),
    bind_if: String,
    duration_s: Option<u64>,
    takeover: bool,
    stop_file: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
enum PairOut {
    File(PathBuf),
    Stdout,
}

#[derive(Debug)]
enum Cmd {
    Help,
    Run(RunArgs),
    Keygen { keys: PathBuf },
    PairAdd { keys: PathBuf, name: String, public: [u8; 32], entry: Option<String>, out: PairOut },
}

fn parse(args: &[String]) -> Result<Cmd, String> {
    match args.first().map(String::as_str) {
        Some("keygen") => parse_keygen(&args[1..]),
        Some("pair") if args.get(1).map(String::as_str) == Some("add") => parse_pair_add(&args[2..]),
        Some("pair") => Err("pair 只有 add 一个子命令".into()),
        _ => parse_run(args),
    }
}

fn parse_size(v: &str) -> Option<(u16, u16)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

fn parse_run(args: &[String]) -> Result<Cmd, String> {
    let (mut keys, mut listen, mut file) = (None, None, None);
    let mut looping = false;
    let mut fps = 60u32;
    let mut codec = Codec::Hevc;
    let mut size = (0u16, 0u16);
    let mut bind_if = String::new();
    let mut duration_s = None;
    let mut takeover = false;
    let mut stop_file = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} 缺少值"));
        match a.as_str() {
            "-h" | "--help" => return Ok(Cmd::Help),
            "--keys" => keys = Some(PathBuf::from(val("--keys")?)),
            "--listen" => {
                let v = val("--listen")?;
                listen = Some(
                    v.parse::<SocketAddr>().map_err(|_| format!("--listen 要「地址:端口」，例如 127.0.0.1:9420：{v}"))?,
                );
            }
            "--file" => file = Some(PathBuf::from(val("--file")?)),
            "--loop" => looping = true,
            "--fps" => {
                let v = val("--fps")?;
                fps = v.parse().ok().filter(|f| (1..=240).contains(f)).ok_or(format!("--fps 要 1~240 的整数：{v}"))?;
            }
            "--codec" => {
                codec = match val("--codec")?.as_str() {
                    "hevc" | "h265" => Codec::Hevc,
                    "avc" | "h264" => Codec::Avc,
                    other => return Err(format!("--codec 只认 hevc、avc：{other}")),
                }
            }
            "--size" => {
                let v = val("--size")?;
                size = parse_size(&v).ok_or(format!("--size 要「宽x高」，例如 1920x1080：{v}"))?;
            }
            "--bind-if" => bind_if = val("--bind-if")?,
            "--duration" => {
                let v = val("--duration")?;
                duration_s = Some(v.parse::<u64>().ok().filter(|d| *d > 0).ok_or(format!("--duration 要正整数秒：{v}"))?);
            }
            "--takeover" => takeover = true,
            "--stop-file" => stop_file = Some(PathBuf::from(val("--stop-file")?)),
            other => return Err(format!("不认识的参数：{other}")),
        }
    }
    Ok(Cmd::Run(RunArgs {
        keys: keys.ok_or("缺少 --keys")?,
        listen: listen.ok_or("缺少 --listen")?,
        file: file.ok_or("缺少 --file")?,
        looping,
        fps,
        codec,
        size,
        bind_if,
        duration_s,
        takeover,
        stop_file,
    }))
}

fn parse_keygen(args: &[String]) -> Result<Cmd, String> {
    match args {
        [k, dir] if k == "--keys" => Ok(Cmd::Keygen { keys: PathBuf::from(dir) }),
        _ => Err("keygen 的参数是 --keys <目录>".into()),
    }
}

fn valid_entry(e: &str) -> bool {
    !e.is_empty() && e.len() <= ENTRY_MAX && !e.chars().any(char::is_whitespace) && e.contains(':')
}

fn parse_pair_add(args: &[String]) -> Result<Cmd, String> {
    let mut pos = Vec::new();
    let (mut keys, mut entry, mut out) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} 缺少值"));
        match a.as_str() {
            "--keys" => keys = Some(PathBuf::from(val("--keys")?)),
            "--entry" => {
                let v = val("--entry")?;
                if !valid_entry(&v) {
                    return Err("--entry 要「地址:端口」，不含空白，最长 255 字节".into());
                }
                entry = Some(v);
            }
            "--out" | "--stdout" if out.is_some() => return Err("--out 和 --stdout 只能给一个".into()),
            "--out" => out = Some(PairOut::File(PathBuf::from(val("--out")?))),
            "--stdout" => out = Some(PairOut::Stdout),
            s if s.starts_with("--") => return Err(format!("pair add 不认识的参数：{s}")),
            s => pos.push(s.to_string()),
        }
    }
    let [name, key] = pos.as_slice() else { return Err("pair add 要两个参数：<名字> <客户端公钥>".into()) };
    let public = ywp::creds::parse_hex32(key).ok_or("客户端公钥要 64 位十六进制（yw-client pubkey 的输出）")?;
    Ok(Cmd::PairAdd {
        keys: keys.ok_or("缺少 --keys")?,
        name: name.clone(),
        public,
        entry,
        out: out.ok_or("配对串含 PSK，要指定 --out <文件> 或 --stdout（接管道）")?,
    })
}

// ======================================================================
// keygen、pair add
// ======================================================================

fn cmd_keygen(keys: &Path) -> Result<(), String> {
    let public = ywp::creds::init_keys(keys).map_err(|e| format!("keygen 失败：{e}"))?;
    let gid = ywp::crypto::gateway_id(&public);
    eprintln!("已在 {} 生成网关密钥（gateway_id {}）", keys.display(), ywp::creds::to_hex(&gid));
    Ok(())
}

fn write_pairing(out: &PairOut, s: &str) -> io::Result<()> {
    match out {
        PairOut::File(p) => {
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
            f.write_all(s.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()
        }
        PairOut::Stdout => {
            let mut o = io::stdout().lock();
            o.write_all(s.as_bytes())?;
            o.write_all(b"\n")?;
            o.flush()
        }
    }
}

fn cmd_pair_add(keys: &Path, name: &str, public: &[u8; 32], entry: Option<&str>, out: &PairOut) -> Result<(), String> {
    match out {
        PairOut::Stdout if io::stdout().is_terminal() => {
            return Err("配对串含 PSK，不往终端输出：用 --out <文件>，或者接管道（… --stdout | yw-client pair import）".into())
        }
        PairOut::File(p) if p.exists() => return Err(format!("{} 已存在，不覆盖", p.display())),
        _ => {}
    }
    let gw = ywp::creds::load_keys(keys).map_err(|e| format!("读 keys/ 失败：{e}"))?;
    let psk = ywp::creds::add_client(keys, name, public).map_err(|e| format!("配对失败：{e}"))?;
    let s = ywp::creds::encode_pairing(&gw.static_kp.public(), &psk, entry);
    let r = write_pairing(out, &s);
    // 配对串用完就清零
    let mut b = s.into_bytes();
    b.iter_mut().for_each(|x| *x = 0);
    std::hint::black_box(&b);
    if let Err(e) = r {
        let _ = ywp::creds::remove_client(keys, name);
        return Err(format!("配对串写不出去，已撤销这次配对：{e}"));
    }
    let to = match out {
        PairOut::File(p) => format!("写到 {}（600）", p.display()),
        PairOut::Stdout => "经 stdout 交给管道".to_string(),
    };
    eprintln!("已配对「{name}」（{}），配对串{to}", ClientId::of(public));
    Ok(())
}

// ======================================================================
// 日志：有界队列交给日志线程写 stderr
// ======================================================================

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// JsonlSink 的实现：结构化记录写成一行 JSON，人读的日志写成「[I] 类别：内容」，都进 stderr
struct StderrSink {
    tx: SyncSender<String>,
    dropped: Arc<AtomicU64>,
}

impl StderrSink {
    fn start() -> io::Result<(Arc<Self>, JoinHandle<()>)> {
        let (tx, rx) = mpsc::sync_channel::<String>(LOG_QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let d = dropped.clone();
        let join = std::thread::Builder::new().name("fakegw-log".into()).spawn(move || {
            // 所有发送端都丢了（服务循环和主线程都结束）时退出
            for line in rx.iter() {
                let mut e = io::stderr().lock();
                let _ = writeln!(e, "{line}");
                let n = d.swap(0, Relaxed);
                if n > 0 {
                    let _ = writeln!(e, "[W] fakegw：日志队列满，丢了 {n} 条");
                }
            }
        })?;
        Ok((Arc::new(Self { tx, dropped }), join))
    }

    fn put(&self, line: String) {
        if self.tx.try_send(line).is_err() {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
}

impl JsonlSink for StderrSink {
    fn record(&self, kind: &str, fields: &str) {
        let sep = if fields.is_empty() { "" } else { "," };
        self.put(format!("{{\"t_ms\":{:.1},\"kind\":{}{sep}{fields}}}", now_us() as f64 / 1000.0, json_str(kind)));
    }
    fn log(&self, level: char, kind: &str, msg: &str) {
        self.put(format!("[{level}] {kind}：{msg}"));
    }
}

// ======================================================================
// PsControl：假的 PS5 会话
// ======================================================================

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 回放线程和服务循环共享的「PS5 会话」
struct FakePs {
    epoch: u32,
    stream: StreamInfo,
    /// 参数集最新值槽（只有一个 epoch）
    params: Mutex<Option<Arc<[u8]>>>,
    gov: Mutex<IdrGovernor>,
    /// 有一次真正发出的 IDR 请求还没兑现：回放线程在下一帧之前跳到下一个关键帧
    want_key: AtomicBool,
    /// 最近一次送出关键帧的时刻（0 表示还没有）
    last_key_us: AtomicU64,
    /// 真正「发给 PS5」的 IDR 次数（含 1.2 s 重发）
    idr_sent: AtomicU64,
    escalated: AtomicU64,
}

impl FakePs {
    fn new(epoch: u32, stream: StreamInfo) -> Self {
        Self {
            epoch,
            stream,
            params: Mutex::new(None),
            gov: Mutex::new(IdrGovernor::new()),
            want_key: AtomicBool::new(false),
            last_key_us: AtomicU64::new(0),
            idr_sent: AtomicU64::new(0),
            escalated: AtomicU64::new(0),
        }
    }

    fn on_decision(&self, d: IdrDecision) {
        match d {
            IdrDecision::Requested => {
                self.idr_sent.fetch_add(1, Relaxed);
                self.want_key.store(true, Relaxed);
            }
            IdrDecision::Escalate => {
                self.escalated.fetch_add(1, Relaxed);
            }
            IdrDecision::Coalesced | IdrDecision::Deferred => {}
        }
    }

    /// 回放线程每 100 ms 调：1.2 s 重发、限频推迟的补发、5 s Escalate
    fn tick(&self, now: Micros) -> Option<IdrDecision> {
        let d = lock(&self.gov).tick(now, self.last_key_us.load(Relaxed));
        if let Some(d) = d {
            self.on_decision(d);
        }
        d
    }

    /// 换参数集；返回 true 表示变了
    fn set_params(&self, p: &[u8]) -> bool {
        let mut slot = lock(&self.params);
        if slot.as_deref() == Some(p) {
            return false;
        }
        *slot = Some(Arc::from(p));
        true
    }
}

impl PsControl for FakePs {
    fn request_idr(&self, now: Micros, why: IdrReason) -> IdrDecision {
        let d = lock(&self.gov).request(now, why);
        self.on_decision(d);
        d
    }
    // M2a 不转发手柄
    fn set_controller_full(&self, _c: &InputState) {}
    fn set_controller_idle(&self) {}
    fn params(&self, epoch: u32) -> Option<Arc<[u8]>> {
        if epoch == self.epoch {
            lock(&self.params).clone()
        } else {
            None
        }
    }
    fn audio_header(&self, _epoch: u32) -> Option<AudioHeader> {
        None
    }
    fn stream_info(&self, epoch: u32) -> Option<StreamInfo> {
        (epoch == self.epoch).then_some(self.stream)
    }
}

// ======================================================================
// 回放
// ======================================================================

enum Unit {
    /// 一帧
    Au(Vec<u8>),
    /// 文件中途出现的参数集
    Params(Vec<u8>),
    Eof,
}

#[derive(Debug, Default)]
struct PlayStats {
    frames: u64,
    keyframes: u64,
    push_full: u64,
    seeks: u64,
    seek_us_max: Micros,
    loops: u64,
    lag_resets: u64,
    params_changes: u64,
    attaches: u64,
    detaches: u64,
}

struct Player {
    path: PathBuf,
    codec: Codec,
    reader: AnnexBReader<File>,
    /// 这一次跳关键帧已经在当前 reader 上做过（送出关键帧或重开文件时清掉）
    seek_done: bool,
    /// 要关键帧时连着读到文件尾的次数：整个文件都找不到关键帧就报错，不空转
    eof_while_wanting: u32,
}

impl Player {
    fn open(path: &Path, codec: Codec) -> io::Result<Self> {
        let reader = AnnexBReader::new(File::open(path)?, codec);
        Ok(Self { path: path.to_path_buf(), codec, reader, seek_done: false, eof_while_wanting: 0 })
    }

    fn reopen(&mut self) -> io::Result<()> {
        self.reader = AnnexBReader::new(File::open(&self.path)?, self.codec);
        self.seek_done = false;
        Ok(())
    }

    fn params(&self) -> &[u8] {
        self.reader.param_sets()
    }

    /// 下一个单元。want_key 时先跳到下一个关键帧（每个未兑现的请求在同一个 reader 上只跳一次）
    fn next(&mut self, want_key: bool, st: &mut PlayStats) -> io::Result<Unit> {
        if want_key && !self.seek_done {
            let t = now_us();
            self.reader.seek_next_key()?;
            self.seek_done = true;
            st.seeks += 1;
            st.seek_us_max = st.seek_us_max.max(now_us() - t);
        }
        Ok(match self.reader.next_au()? {
            Some(u) if nal::is_param_sets_only(&u, self.codec) => Unit::Params(u),
            Some(u) => Unit::Au(u),
            None => Unit::Eof,
        })
    }
}

/// 网关每次启动时的 16 位随机数（session_epoch 的高 16 位，规格 §4.5），低 16 位是会话序号 1
fn boot_epoch() -> u32 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let boot = (RandomState::new().hash_one(t) & 0xFFFF) as u32;
    (boot << 16) | 1
}

struct Ctx<'a> {
    a: &'a RunArgs,
    ps: Arc<FakePs>,
    serve: ServeHandle,
    events: Receiver<ServeEvent>,
    log: Arc<StderrSink>,
    st: PlayStats,
    /// ParamsChanged 没推进队列：下一帧之前再推
    params_dirty: bool,
}

impl Ctx<'_> {
    fn publish_params(&mut self, p: &[u8]) {
        if p.is_empty() {
            return;
        }
        if self.ps.set_params(p) {
            self.st.params_changes += 1;
            self.params_dirty = true;
        }
    }

    fn flush_params(&mut self) {
        if self.params_dirty && self.serve.push(MediaMsg::ParamsChanged { epoch: self.ps.epoch }) {
            self.params_dirty = false;
        }
    }

    fn drain_events(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(ServeEvent::ClientAttached { .. }) => self.st.attaches += 1,
                Ok(ServeEvent::ClientDetached { .. }) => self.st.detaches += 1,
                // PS5_CMD 等在 M2d 才有；回复通道直接丢掉，服务循环那边按没有回复处理
                Ok(_) => {}
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => return,
            }
        }
    }

    fn stats_json(&self) -> String {
        let s = &self.st;
        let p = &self.ps;
        format!(
            "\"frames\":{},\"keyframes\":{},\"push_full\":{},\"idr_sent\":{},\"idr_escalated\":{},\"seeks\":{},\
             \"seek_ms_max\":{:.1},\"loops\":{},\"lag_resets\":{},\"params_changes\":{},\"attaches\":{},\"detaches\":{}",
            s.frames,
            s.keyframes,
            s.push_full,
            p.idr_sent.load(Relaxed),
            p.escalated.load(Relaxed),
            s.seeks,
            s.seek_us_max as f64 / 1000.0,
            s.loops,
            s.lag_resets,
            s.params_changes,
            s.attaches,
            s.detaches
        )
    }

    /// 按帧率回放，直到 --duration 到点、放完（不带 --loop）或读文件出错
    fn play(&mut self, mut player: Player) -> Result<(), String> {
        let fps = self.a.fps as u64;
        let period = 1_000_000 / fps;
        let deadline = self.a.duration_s.map(|s| now_us() + s * 1_000_000);
        let (mut base, mut n) = (now_us(), 0u64);
        let mut seq = 0u32;
        let mut next_tick = now_us() + IDR_TICK;
        let mut next_stats = now_us() + STATS_EVERY;
        loop {
            let now = now_us();
            if deadline.is_some_and(|d| now >= d) {
                return Ok(());
            }
            if now >= next_tick {
                if self.a.stop_file.as_deref().is_some_and(Path::exists) {
                    self.log.log('I', "fakegw", "看到停止文件，退出");
                    return Ok(());
                }
                if self.ps.tick(now) == Some(IdrDecision::Escalate) {
                    self.log.log('W', "fakegw", "IDR 请求 5 s 还没等到关键帧（录像里关键帧太少？）");
                }
                next_tick = now + IDR_TICK;
            }
            if now >= next_stats {
                self.log.record("fakegw_stats", &self.stats_json());
                next_stats = now + STATS_EVERY;
            }
            self.drain_events();
            self.flush_params();
            let want = self.ps.want_key.load(Relaxed);
            let unit = player.next(want, &mut self.st).map_err(|e| format!("读录像出错：{e}"))?;
            let au = match unit {
                Unit::Params(p) => {
                    self.publish_params(&p);
                    continue;
                }
                Unit::Eof if !self.a.looping => {
                    self.log.log('I', "fakegw", "录像放完了（没加 --loop）");
                    return Ok(());
                }
                Unit::Eof => {
                    if want {
                        player.eof_while_wanting += 1;
                        if player.eof_while_wanting > 1 {
                            return Err("整个录像里找不到关键帧，没法响应 IDR 请求".into());
                        }
                    }
                    player.reopen().map_err(|e| format!("重开录像失败：{e}"))?;
                    self.st.loops += 1;
                    let p = player.params().to_vec();
                    self.publish_params(&p);
                    continue;
                }
                Unit::Au(au) => au,
            };
            // 按帧率排时刻；落后太多（跳关键帧读了很久）就从现在重新计时
            let due = base + n * period;
            let now = now_us();
            if due > now {
                std::thread::sleep(Duration::from_micros(due - now));
            } else if now - due > MAX_LAG_FRAMES * period {
                base = now;
                n = 0;
                self.st.lag_resets += 1;
            }
            let key = nal::is_keyframe(&au, player.codec);
            let rx_us = now_us();
            if key {
                // 先清标志再记时刻：之后才到的请求会再跳一次，不会被这一帧「顶掉」
                self.ps.want_key.store(false, Relaxed);
                self.ps.last_key_us.store(rx_us, Relaxed);
                player.seek_done = false;
                player.eof_while_wanting = 0;
                self.st.keyframes += 1;
            }
            let msg = MediaMsg::Video { seq, epoch: self.ps.epoch, data: au, frames_lost: 0, recovered: false, rx_us };
            if !self.serve.push(msg) {
                self.st.push_full += 1;
                if key {
                    self.serve.note_key_overflow();
                }
            }
            seq = seq.wrapping_add(1);
            n += 1;
            self.st.frames += 1;
        }
    }
}

fn cmd_run(a: &RunArgs) -> Result<(), String> {
    let player = Player::open(&a.file, a.codec).map_err(|e| format!("打不开录像 {}：{e}", a.file.display()))?;
    if player.params().is_empty() {
        return Err(format!("{} 开头没有参数集（要 M1 record 或 m0-probe --dump 录的、带参数集头的 Annex-B）", a.file.display()));
    }
    let keys = ywp::creds::load_keys(&a.keys).map_err(|e| format!("读 keys/ 失败：{e}"))?;
    let paired = keys.clients.len();
    let (log, log_join) = StderrSink::start().map_err(|e| format!("起不了日志线程：{e}"))?;
    if paired == 0 {
        log.log('W', "fakegw", "keys/ 里还没有配对的客户端（fakegw pair add 之后重启 fakegw）");
    }
    let codec = if a.codec == Codec::Avc { video_codec::AVC } else { video_codec::HEVC };
    let stream = StreamInfo { codec, fps: a.fps.min(255) as u8, width: a.size.0, height: a.size.1 };
    let epoch = boot_epoch();
    let ps = Arc::new(FakePs::new(epoch, stream));
    let (etx, erx) = mpsc::channel();
    let cfg = ServeConfig {
        port: a.listen.port(),
        bind_if: a.bind_if.clone(),
        takeover: if a.takeover { TakeoverPolicy::Auto } else { TakeoverPolicy::RequireFlag },
        log: log.clone(),
    };
    let opts = ServeOptions { keys_dir: Some(a.keys.clone()), bind_ip: Some(a.listen.ip()), ..ServeOptions::default() };
    let ctl: Arc<dyn PsControl> = ps.clone();
    let serve = yw_serve::spawn_with(cfg, opts, keys, ctl, etx).map_err(|e| format!("{} 绑不上：{e}", a.listen))?;
    let local = serve.local_addr();
    let file_name = a.file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    log.log(
        'I',
        "fakegw",
        &format!(
            "已启动：UDP {local}{}，录像 {file_name}（{} fps{}），epoch {epoch:08x}，已配对 {paired} 个客户端",
            if a.bind_if.is_empty() { String::new() } else { format!("，绑 {}", a.bind_if) },
            a.fps,
            if a.looping { "，循环" } else { "" }
        ),
    );
    {
        let mut o = io::stdout().lock();
        let _ = writeln!(o, "{{\"event\":\"listening\",\"addr\":\"{local}\",\"paired\":{paired},\"epoch\":\"{epoch:08x}\"}}");
        let _ = o.flush();
    }

    let mut ctx = Ctx { a, ps, serve, events: erx, log, st: PlayStats::default(), params_dirty: false };
    let p = player.params().to_vec();
    ctx.publish_params(&p);
    ctx.flush_params();
    let host_id = ywp::crypto::host_id("fakegw");
    if !ctx.serve.push(MediaMsg::PsChanged(SessionInfo { ps_state: ps_state::STREAMING, host_id, epoch, resumable: false })) {
        ctx.log.log('W', "fakegw", "会话状态没推进队列（SESSION_STATUS 会缺一次）");
    }
    let result = ctx.play(player);
    if let Err(e) = &result {
        ctx.log.log('E', "fakegw", e);
    }

    // 退出：给客户端发 CLOSE(5)，等服务循环结束，再输出汇总
    if !ctx.serve.shutdown(SHUTDOWN_WAIT) {
        ctx.log.log('W', "fakegw", "服务循环没有按时确认退出");
    }
    ctx.drain_events();
    let status = ctx.serve.status().to_json();
    let summary = format!("{{\"event\":\"summary\",\"ok\":{},{},\"serve\":{status}}}", result.is_ok(), ctx.stats_json());
    let Ctx { serve, log, .. } = ctx;
    let _ = serve.join.join();
    drop(log);
    let _ = log_join.join();
    let mut o = io::stdout().lock();
    let _ = writeln!(o, "{summary}");
    let _ = o.flush();
    result
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let r = match &cmd {
        Cmd::Help => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Cmd::Run(a) => cmd_run(a),
        Cmd::Keygen { keys } => cmd_keygen(keys),
        Cmd::PairAdd { keys, name, public, entry, out } => cmd_pair_add(keys, name, public, entry.as_deref(), out),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cmd, String> {
        let v: Vec<String> = s.split_whitespace().map(String::from).collect();
        parse(&v)
    }

    const PUB: &str = "0101010101010101010101010101010101010101010101010101010101010101";

    #[test]
    fn parse_run_args() {
        match p("--keys k --listen 127.0.0.1:9420 --file a.h265 --loop --fps 30 --size 1920x1080 --duration 5") {
            Ok(Cmd::Run(a)) => {
                assert_eq!(a.listen, "127.0.0.1:9420".parse().unwrap());
                assert!(a.looping);
                assert_eq!(a.fps, 30);
                assert_eq!(a.size, (1920, 1080));
                assert_eq!(a.duration_s, Some(5));
                assert!(a.bind_if.is_empty());
                assert_eq!(a.codec, Codec::Hevc);
                assert!(!a.takeover);
                assert!(a.stop_file.is_none());
            }
            other => panic!("{other:?}"),
        }
        match p("--keys k --listen 127.0.0.1:1 --file a --stop-file /s --takeover --bind-if en0 --codec avc") {
            Ok(Cmd::Run(a)) => {
                assert_eq!(a.stop_file.as_deref(), Some(Path::new("/s")));
                assert!(a.takeover);
                assert_eq!(a.bind_if, "en0");
                assert_eq!(a.codec, Codec::Avc);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(p("--help"), Ok(Cmd::Help)));
        assert!(p("--keys k --file a.h265").is_err(), "缺 --listen");
        assert!(p("--keys k --listen 9420 --file a").is_err(), "--listen 要地址:端口");
        assert!(p("--keys k --listen 127.0.0.1:1 --file a --fps 0").is_err());
        assert!(p("--keys k --listen 127.0.0.1:1 --file a --size 1920").is_err());
        assert!(p("--keys k --listen 127.0.0.1:1 --file a --codec vp9").is_err());
        assert!(p("--keys k --listen 127.0.0.1:1 --file a --duration 0").is_err());
        assert!(p("--keys k --listen 127.0.0.1:1 --file a --bogus").is_err());
    }

    #[test]
    fn parse_subcommands() {
        assert!(matches!(p("keygen --keys /k"), Ok(Cmd::Keygen { .. })));
        assert!(p("keygen").is_err());
        match p(&format!("pair add c0 {PUB} --keys /k --entry 127.0.0.1:9420 --stdout")) {
            Ok(Cmd::PairAdd { name, public, entry, out, .. }) => {
                assert_eq!(name, "c0");
                assert_eq!(public, [1u8; 32]);
                assert_eq!(entry.as_deref(), Some("127.0.0.1:9420"));
                assert_eq!(out, PairOut::Stdout);
            }
            other => panic!("{other:?}"),
        }
        assert!(p(&format!("pair add c0 {PUB} --keys /k")).is_err(), "必须指定配对串去哪");
        assert!(p(&format!("pair add c0 {PUB} --keys /k --out /x --stdout")).is_err());
        assert!(p("pair add c0 1234 --keys /k --stdout").is_err(), "公钥要 64 位十六进制");
        assert!(p(&format!("pair add c0 {PUB} --keys /k --entry nope --stdout")).is_err());
        assert!(p("pair list").is_err());
    }

    /// 合成一段 HEVC：参数集（VPS、SPS、PPS）+ 一个 IDR + 两个 P 帧（只看 NAL 头，内容随便填）
    fn synth_hevc(path: &Path) {
        let nal = |t: u8, first_slice: bool| {
            let mut v = vec![0, 0, 0, 1, t << 1, 1];
            if t < 32 {
                v.push(if first_slice { 0x80 } else { 0x00 });
            }
            v.extend_from_slice(&[0x11, 0x22, 0x33]);
            v
        };
        let mut f = Vec::new();
        for t in [32u8, 33, 34] {
            f.extend(nal(t, false));
        }
        f.extend(nal(19, true));
        f.extend(nal(1, true));
        f.extend(nal(1, true));
        std::fs::write(path, f).unwrap();
    }

    #[test]
    fn player_seeks_to_next_key_and_wraps_on_loop() {
        let dir = std::env::temp_dir().join(format!("fakegw-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.h265");
        synth_hevc(&path);
        let mut pl = Player::open(&path, Codec::Hevc).unwrap();
        assert!(!pl.params().is_empty());
        let mut st = PlayStats::default();
        let is_key = |u: &Unit| matches!(u, Unit::Au(a) if nal::is_keyframe(a, Codec::Hevc));
        // 依次是关键帧、P、P、文件尾
        assert!(is_key(&pl.next(false, &mut st).unwrap()));
        assert!(matches!(pl.next(false, &mut st).unwrap(), Unit::Au(_)));
        // 要关键帧：后面没有了，读到文件尾
        assert!(matches!(pl.next(true, &mut st).unwrap(), Unit::Eof));
        assert_eq!(st.seeks, 1);
        // 同一个 reader 上不重复跳；重开后再跳，第一个就是关键帧
        pl.reopen().unwrap();
        assert!(is_key(&pl.next(true, &mut st).unwrap()));
        assert_eq!(st.seeks, 2);
        // 只清掉本测试自己写的临时文件
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn idr_goes_through_governor() {
        let ps = FakePs::new(0x1234_0001, StreamInfo::default());
        let t0 = 10_000_000;
        assert_eq!(ps.request_idr(t0, IdrReason::NoFrames), IdrDecision::Requested);
        assert!(ps.want_key.load(Relaxed));
        // 未决期间合并
        assert_eq!(ps.request_idr(t0 + 1000, IdrReason::NoFrames), IdrDecision::Coalesced);
        assert_eq!(ps.idr_sent.load(Relaxed), 1);
        // 送出关键帧后 tick 兑现；1 s 内再来的请求推迟，由 tick 补发
        ps.want_key.store(false, Relaxed);
        ps.last_key_us.store(t0 + 20_000, Relaxed);
        assert_eq!(ps.tick(t0 + 100_000), None);
        assert_eq!(ps.request_idr(t0 + 200_000, IdrReason::NoFrames), IdrDecision::Deferred);
        assert!(!ps.want_key.load(Relaxed));
        assert_eq!(ps.tick(t0 + 1_000_000), Some(IdrDecision::Requested));
        assert!(ps.want_key.load(Relaxed));
        assert_eq!(ps.idr_sent.load(Relaxed), 2);
        // 参数集最新值槽只认自己的 epoch
        assert!(ps.set_params(&[0, 0, 0, 1, 0x40, 1]));
        assert!(!ps.set_params(&[0, 0, 0, 1, 0x40, 1]));
        assert!(ps.params(0x1234_0001).is_some());
        assert!(ps.params(0x1234_0002).is_none());
    }
}
