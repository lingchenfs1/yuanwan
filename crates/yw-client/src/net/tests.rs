//! net 线程的本机回环测试：测试里起一个 ywp::endpoint::Gateway 的最小收发环（真 UDP，127.0.0.1），
//! 用 yw-media 的 VideoTx 切片、加校验，客户端经 spawn_net 连上，核对：
//! - 收到 VIDEO_CONFIG 后，关键帧前面补上了参数集，几帧的字节和原帧一致，fid 严格递增；
//! - 网关「重启」（换一个全新的 Gateway，状态清空）后，客户端收到 RESET，1 s 内重新握手，
//!   新会话的 fid 从 0 重新开始，交给 decode 的 fid 仍然递增；
//! - 断开时网关收到 CLOSE(0)。
//!
//! 不解码、不开窗口；DecodeItem 直接在测试里收。
//! 另有一条用本地录像（环境变量 YW_TEST_H265）跑无界面模式的端到端测试：经同一个回环网关，真解码，不呈现。

use super::*;
use std::net::UdpSocket as StdUdp;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Instant;
use ywp::consts::video_codec;
use ywp::creds::{ClientEntry, Keys};
use ywp::crypto::{gateway_id, Secret32};
use ywp::endpoint::{Gateway, GatewayConfig, GwEvent, TakeoverPolicy};
use ywp::msg::{VideoConfig, VideoProfile};
use yw_media::video_tx::{FrameIn, VideoTx, VideoTxConfig};
use yw_media::Mode;

const GW_SECRET: [u8; 32] = [0x11; 32];
const CLIENT_SECRET: [u8; 32] = [0x22; 32];
const PSK: [u8; 32] = [0x33; 32];
const RESET_KEY: [u8; 32] = [0x77; 32];
/// 每个会话发多少帧
const FRAMES_PER_SESSION: u32 = 12;

fn gw_keys() -> Keys {
    Keys {
        static_kp: StaticKeypair::from_secret(GW_SECRET),
        reset_key: Secret32::new(RESET_KEY),
        clients: vec![ClientEntry {
            name: "loop".into(),
            public: StaticKeypair::from_secret(CLIENT_SECRET).public(),
            psk: Secret32::new(PSK),
        }],
    }
}

/// HEVC NAL：两字节头（类型 << 1，tid = 1）+ 正文
fn hevc(t: u8, rest: &[u8]) -> Vec<u8> {
    let mut v = vec![0, 0, 0, 1, t << 1, 1];
    v.extend_from_slice(rest);
    v
}

fn param_sets() -> Vec<u8> {
    let mut v = hevc(32, &[0x0c, 0x01]);
    v.extend(hevc(33, &[0x01, 0x01, 0x60]));
    v.extend(hevc(34, &[0xc1, 0x73]));
    v
}

/// 第 i 帧（会话内）：关键帧 IDR_W_RADL、其余 TRAIL_R；正文全是非零字节，不会冒出起始码。
/// 长度从几百字节到 5 KB 不等，大的要切成好几个符号
fn frame_au(i: u32) -> Vec<u8> {
    let key = i == 0;
    let len = if key { 5_000 } else { 300 + (i as usize * 911) % 3_000 };
    let mut body = vec![0x80]; // first_slice_segment_in_pic_flag = 1
    body.extend((0..len).map(|j| (1 + (i as usize * 7 + j) % 250) as u8));
    hevc(if key { 19 } else { 1 }, &body)
}

/// 网关发什么：参数集（进 VIDEO_CONFIG）和一串 (AU, 是不是关键帧)，每个会话从头发一遍
#[derive(Clone)]
struct Source {
    params: Vec<u8>,
    frames: Arc<Vec<(Vec<u8>, bool)>>,
}

fn synthetic() -> Source {
    Source { params: param_sets(), frames: Arc::new((0..FRAMES_PER_SESSION).map(|i| (frame_au(i), i == 0)).collect()) }
}

enum GwCmd {
    /// 换一个全新的 Gateway（模拟网关进程重启：会话、fid、cfg_ver 都从头来）
    Restart,
    Stop,
}

#[derive(Debug, Default)]
struct GwLog {
    connects: u32,
    closed: Vec<u16>,
    frames: u32,
}

/// 起网关线程：socket 用 ywp 的 bound_udp（不绑网卡，4 MB 缓冲），收包等 1 ms 超时
fn spawn_gw(src: Source) -> (SocketAddr, Sender<GwCmd>, std::thread::JoinHandle<GwLog>) {
    let sock = bound_udp("127.0.0.1:0".parse().unwrap(), "").unwrap();
    sock.set_nonblocking(false).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
    let addr = sock.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    (addr, tx, std::thread::spawn(move || gw_loop(sock, rx, src)))
}

/// 发一个包；回环的输出队列满了（ENOBUFS）就稍等重试
fn send_retry(sock: &StdUdp, b: &[u8], dst: SocketAddr) {
    for _ in 0..50 {
        match sock.send_to(b, dst) {
            Ok(_) => return,
            Err(e) if matches!(classify_send_err(&e), SendOutcome::NoBufs | SendOutcome::WouldBlock) => {
                std::thread::sleep(Duration::from_micros(200))
            }
            Err(_) => return,
        }
    }
}

/// 网关的最小收发环：收包 → 计时器 → 有客户端时发 VIDEO_CONFIG 和 60 fps 的视频 → 发包
fn gw_loop(sock: StdUdp, cmd: Receiver<GwCmd>, src: Source) -> GwLog {
    let t0 = Instant::now();
    let now = || t0.elapsed().as_micros() as Micros;
    let new_gw = || Gateway::new(GatewayConfig { keys: gw_keys(), takeover: TakeoverPolicy::RequireFlag });
    let mut gw = new_gw();
    let mut vtx = VideoTx::new(VideoTxConfig::default());
    let mut log = GwLog::default();
    let mut buf = [0u8; 2048];
    let mut out = [0u8; BASE_PAYLOAD];
    let mut streaming: Option<(u32, Micros)> = None; // (会话内帧号, 下一帧时刻)
    let mut fid: u16 = 0;
    let cfg_ver = 1u8;
    loop {
        match cmd.try_recv() {
            Ok(GwCmd::Restart) => {
                gw = new_gw();
                vtx = VideoTx::new(VideoTxConfig::default());
                streaming = None;
                fid = 0;
            }
            Ok(GwCmd::Stop) | Err(TryRecvError::Disconnected) => break,
            Err(TryRecvError::Empty) => {}
        }
        let mut connected = false;
        let mut on_ev = |e: GwEvent<'_>| match e {
            GwEvent::Connected { .. } => connected = true,
            GwEvent::Closed(c) => log.closed.push(c),
            _ => {}
        };
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            gw.handle_datagram(now(), from, &mut buf[..n], &mut on_ev);
        }
        let t = now();
        if gw.next_timeout().is_some_and(|x| x <= t) {
            gw.handle_timeout(t, &mut on_ev);
        }
        if connected {
            log.connects += 1;
            // 新客户端：先发 VIDEO_CONFIG，再从关键帧开始
            let vc = VideoConfig {
                cfg_ver,
                codec: video_codec::HEVC,
                fps: 60,
                profiles: vec![VideoProfile { idx: 0, width: 1920, height: 1080, param_sets: src.params.clone() }],
            };
            gw.conn().unwrap().send_control(&ControlMsg::VideoConfig(vc)).unwrap();
            streaming = Some((0, t));
        }
        if let Some((i, at)) = streaming {
            if (i as usize) < src.frames.len() && t >= at {
                let (au, key) = &src.frames[i as usize];
                let f = FrameIn { fid, key: *key, cfg: cfg_ver, profile: 0, recovered: false, t0: t, au };
                let mut cars = Vec::new();
                vtx.send_frame(t, &f, &mut cars).unwrap();
                if let Some(c) = gw.conn() {
                    for v in cars {
                        c.queue_video(v);
                    }
                }
                fid = fid.wrapping_add(1);
                log.frames += 1;
                streaming = Some((i + 1, at + 16_667));
            }
        }
        while let Some(tx) = gw.poll_transmit(t, &vtx, &mut out) {
            send_retry(&sock, &out[..tx.len], tx.dst);
        }
    }
    log
}

fn profile(entry: SocketAddr) -> GatewayProfile {
    let gw_pub = StaticKeypair::from_secret(GW_SECRET).public();
    GatewayProfile {
        name: "loop".into(),
        gw_pub,
        psk: Secret32::new(PSK),
        gateway_id: gateway_id(&gw_pub),
        entry,
        bind_if: None,
        bitrate_cap_kbps: None,
        mode: Mode::Competitive,
        j_ms: None,
        auto_start: false,
    }
}

/// 等某个 UiEvent，顺便记下途中见过的
fn wait_ui(rx: &Receiver<UiEvent>, seen: &mut Vec<String>, want: impl Fn(&UiEvent) -> bool, within: Duration) -> bool {
    let end = Instant::now() + within;
    loop {
        let left = end.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) => {
                seen.push(format!("{e:?}"));
                if want(&e) {
                    return true;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
}

/// 收 n 个 DecodeItem
fn take_items(rx: &Receiver<DecodeItem>, n: usize, within: Duration) -> Vec<DecodeItem> {
    let end = Instant::now() + within;
    let mut v = Vec::new();
    while v.len() < n {
        match rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
            Ok(it) => v.push(it),
            Err(_) => break,
        }
    }
    v
}

/// 核对一个会话交出来的帧：关键帧 = 参数集 + 原帧，其余逐字节等于原帧，fid 严格递增
fn check_session(items: &[DecodeItem], after_fid: Option<u64>) {
    let mut last = after_fid;
    for (i, it) in items.iter().enumerate() {
        let mut want = frame_au(i as u32);
        if i == 0 {
            assert!(it.key, "会话的第一帧应该是关键帧");
            let mut k = param_sets();
            k.extend_from_slice(&want);
            want = k;
        } else {
            assert!(!it.key);
        }
        assert_eq!(&it.au[..], &want[..], "第 {i} 帧的字节不对");
        assert_eq!(it.cfg, 1);
        if let Some(l) = last {
            assert!(it.fid > l, "fid 没有递增：{} 之后是 {}", l, it.fid);
        }
        last = Some(it.fid);
    }
}

#[test]
fn loopback_video_config_frames_reset_reconnect() {
    let (entry, cmd_tx, gw) = spawn_gw(synthetic());

    let (dec_tx, dec_rx) = mpsc::sync_channel(DECODE_QUEUE);
    let (audio_tx, _audio_rx) = mpsc::sync_channel(AUDIO_QUEUE);
    let (fb_tx, _fb_rx) = mpsc::channel();
    let (ui_tx, ui_rx) = mpsc::channel();
    let ui: UiSink = Arc::new(move |e| {
        let _ = ui_tx.send(e);
    });
    let net =
        spawn_net(&profile(entry), StaticKeypair::from_secret(CLIENT_SECRET), dec_tx, audio_tx, fb_tx, ui).unwrap();
    let mut seen = Vec::new();

    // ---- 第一个会话：连上，收到 VIDEO_CONFIG 和全部帧 ----
    assert!(wait_ui(&ui_rx, &mut seen, |e| matches!(e, UiEvent::Connected), Duration::from_secs(3)), "{seen:?}");
    let first = take_items(&dec_rx, FRAMES_PER_SESSION as usize, Duration::from_secs(3));
    assert_eq!(first.len(), FRAMES_PER_SESSION as usize, "第一个会话只收到 {} 帧", first.len());
    check_session(&first, None);
    // 统计快照每 100 ms 才刷新一次，可能还没追上：在截止时间内轮询
    let end = Instant::now() + Duration::from_millis(500);
    let st = loop {
        let s = net.stats();
        if (s.established && s.sessions == 1 && s.video_configs >= 1) || Instant::now() >= end {
            break s;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(st.established && st.sessions == 1, "{st:?}");
    assert!(st.hs_attempts_first >= 1 && st.hs_resp_seen);
    assert!(st.video_configs >= 1);

    // ---- 网关重启：RESET → 1 s 内重新握手、出新会话的关键帧 ----
    let t_restart = Instant::now();
    cmd_tx.send(GwCmd::Restart).unwrap();
    assert!(wait_ui(&ui_rx, &mut seen, |e| matches!(e, UiEvent::Connected), Duration::from_secs(3)), "{seen:?}");
    let reconnect = t_restart.elapsed();
    eprintln!("网关重启到重新连上：{reconnect:?}");
    assert!(reconnect < Duration::from_secs(1), "RESET 后重连用了 {reconnect:?}");
    assert!(seen.iter().any(|s| s == "Reconnecting"), "{seen:?}");
    let second = take_items(&dec_rx, FRAMES_PER_SESSION as usize, Duration::from_secs(3));
    assert_eq!(second.len(), FRAMES_PER_SESSION as usize, "第二个会话只收到 {} 帧", second.len());
    check_session(&second, first.last().map(|i| i.fid));

    // ---- 断开：网关收到 CLOSE(0) ----
    assert!(net.send(NetIn::Disconnect));
    net.join.join().unwrap();
    let st = net.stats.lock().unwrap().clone();
    assert_eq!(st.sessions, 2, "{st:?}");
    assert!(st.resets >= 1, "{st:?}");
    assert_eq!(st.reconnects, 1, "{st:?}");
    assert!(st.recv >= 2 * FRAMES_PER_SESSION as u64, "{st:?}");
    assert_eq!(st.lost, 0, "回环上不该丢包：{st:?}");
    assert_eq!(st.asm.frames_ok, 2 * FRAMES_PER_SESSION as u64);
    assert_eq!(st.frames_queued, 2 * FRAMES_PER_SESSION as u64);
    assert!(st.first_established.is_some_and(|t| t >= st.connect_at));
    // 这期间 DecodeItem 通道没有多出别的帧
    assert!(dec_rx.try_recv().is_err());
    std::thread::sleep(Duration::from_millis(50));
    cmd_tx.send(GwCmd::Stop).unwrap();
    let log = gw.join().unwrap();
    assert_eq!(log.connects, 2, "{log:?}");
    assert!(log.closed.contains(&close_code::NORMAL), "网关没收到 CLOSE：{log:?}");
    assert_eq!(log.frames, 2 * FRAMES_PER_SESSION);
}

/// 握手被拒（未配对的客户端身份：网关不回包）和 BUSY 不在这里测；这里测「网卡名不存在」时 spawn_net 直接报错
#[test]
fn bad_bind_if_fails_fast() {
    let mut p = profile("127.0.0.1:9".parse().unwrap());
    p.bind_if = Some("yw-no-such-if0".into());
    let (dec_tx, _r1) = mpsc::sync_channel(1);
    let (audio_tx, _r2) = mpsc::sync_channel(1);
    let (fb_tx, _r3) = mpsc::channel();
    let ui: UiSink = Arc::new(|_| {});
    let r = spawn_net(&p, StaticKeypair::from_secret(CLIENT_SECRET), dec_tx, audio_tx, fb_tx, ui);
    assert!(r.is_err());
}

/// 端到端（无界面模式）：本地录像经回环网关（VideoTx 切片加校验、加密）→ net 线程 → Assembler →
/// VideoToolbox 真解码，不呈现。核对出帧、0 解码报错、0 冻结、首帧时间。
/// `YW_TEST_H265=<录像> cargo test -p yw-client headless_real_clip -- --nocapture`，没设就跳过
#[test]
fn headless_real_clip_over_loopback() {
    use crate::decode::{FfmpegDecoder, HwKind};
    use crate::stream::{run_headless_with, HeadlessOpts};
    use yw_media::nal;
    use yw_media::ywf::AnnexBReader;
    use yw_media::Codec;

    let Some(path) = std::env::var_os("YW_TEST_H265") else {
        eprintln!("没设 YW_TEST_H265，跳过");
        return;
    };
    // 4 s 的帧（60 fps），比测试时长多，整个测试期间画面不断
    let f = std::fs::File::open(&path).unwrap_or_else(|e| panic!("读不到 YW_TEST_H265：{e}"));
    let mut r = AnnexBReader::new(std::io::BufReader::new(f), Codec::Hevc);
    let mut frames = Vec::new();
    while frames.len() < 240 {
        let Some(au) = r.next_au().unwrap() else { break };
        if nal::is_param_sets_only(&au, Codec::Hevc) {
            continue;
        }
        let key = nal::is_keyframe(&au, Codec::Hevc);
        frames.push((au, key));
    }
    assert!(frames.first().is_some_and(|(_, k)| *k), "录像要从关键帧开始");
    let src = Source { params: r.param_sets().to_vec(), frames: Arc::new(frames) };
    let (entry, cmd_tx, gw) = spawn_gw(src);

    let o = HeadlessOpts { duration_s: 3, hw: HwKind::VideoToolbox };
    let dec = FfmpegDecoder::new(HwKind::VideoToolbox).expect("VideoToolbox 不可用");
    let rep = run_headless_with(&profile(entry), StaticKeypair::from_secret(CLIENT_SECRET), &o, Box::new(dec));
    eprintln!("{}", rep.to_json());
    cmd_tx.send(GwCmd::Stop).unwrap();
    let log = gw.join().unwrap();
    assert!(rep.ok, "{rep:?}");
    assert_eq!(rep.hs_attempts, 1);
    assert_eq!(rep.decode_errors, 0, "{rep:?}");
    assert!(rep.frames >= 150, "3 s 只解出 {} 帧", rep.frames);
    assert_eq!((rep.freezes, rep.l3, rep.lost), (0, 0, 0), "{rep:?}");
    assert!(rep.first_frame_ms.is_some_and(|t| t < 500.0), "{rep:?}");
    assert!(log.closed.contains(&close_code::NORMAL), "{log:?}");
}
