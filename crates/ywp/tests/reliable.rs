//! CONTROL 可靠层的集成测试（B 组）：两端各一个 ControlPlane + RttEstimator + Scheduler，
//! 经 testkit 的内存链路对接，链路上按种子随机注入丢包、重复、乱序和时延。
//!
//! 线上编码由 A 组实现，这里绕开它：链路上传的是块的结构体（Item），一个包 = 一组块，
//! 包长按各块编码后的长度估算（content_len + 3 B 的 type/len），交给调度器按优先级组包。
//!
//! 运行：cargo test -p ywp --features testkit

#![cfg(feature = "testkit")]

use ywp::consts::{stream, BASE_PAYLOAD, CHUNK_TLV_HDR, DATA_OVERHEAD, UPLINK_MAX};
use ywp::msg::{AudioConfig, ControlMsg, SessionStatus, VideoConfig, VideoProfile};
use ywp::reliable::{ControlPlane, StreamRx, StreamTx};
use ywp::rtt::RttEstimator;
use ywp::sched::{prio_of, ChunkKind, Dir, Scheduler, SmallOpts};
use ywp::testkit::{min_opt, LinkCfg, Pipe, Rng};
use ywp::time::{ms, secs, send_ts, Micros, CTL_RTO_MAX};
use ywp::wire::{Ack, ControlFrag, ControlFragOwned, EncodeChunk, Pong};

/// 链路上的块（不编码）
#[derive(Clone, Debug)]
enum Item {
    Frag(ControlFragOwned),
    Ack(Ack),
    /// PING 和它所在包的 SendTS
    Ping { id: u16, ts: u32 },
    Pong(Pong),
}

impl Item {
    fn len(&self) -> usize {
        CHUNK_TLV_HDR
            + match self {
                Item::Frag(f) => f.content_len(),
                Item::Ack(a) => a.content_len(),
                Item::Ping { .. } => ywp::consts::chunk_len::PING,
                Item::Pong(p) => p.content_len(),
            }
    }
}

type Pkt = Vec<Item>;

fn view(f: &ControlFragOwned) -> ControlFrag<'_> {
    ControlFrag { stream: f.stream, msg_seq: f.msg_seq, frag: f.frag, data: &f.data }
}

/// 一端：可靠层、RTT、调度器
struct Side {
    dir: Dir,
    cp: ControlPlane,
    rtt: RttEstimator,
    sched: Scheduler<Item>,
    got: Vec<ControlMsg>,
    next_ping: Micros,
    ping_id: u16,
    bad: u64,
    packets: u64,
}

impl Side {
    fn new(dir: Dir, seed_rtt: Micros) -> Self {
        let mut rtt = RttEstimator::new();
        rtt.seed(seed_rtt);
        Self {
            dir,
            cp: ControlPlane::new(dir),
            rtt,
            sched: Scheduler::new(),
            got: Vec::new(),
            next_ping: 0,
            ping_id: 0,
            bad: 0,
            packets: 0,
        }
    }

    fn room(&self) -> usize {
        match self.dir {
            Dir::Down => BASE_PAYLOAD - DATA_OVERHEAD,
            Dir::Up => UPLINK_MAX - DATA_OVERHEAD,
        }
    }

    fn push(&mut self, kind: ChunkKind, item: Item) {
        let p = prio_of(self.dir, kind).expect("方向对");
        let len = item.len();
        self.sched.push(p, len, item, SmallOpts::default());
    }

    fn on_packet(&mut self, now: Micros, pkt: Pkt) {
        for it in pkt {
            match it {
                Item::Frag(f) => {
                    // 重复、超窗口的会返回 Err，属于正常现象（关键消息双发、重传）
                    let _ = self.cp.on_frag(&view(&f), now);
                }
                Item::Ack(a) => self.cp.on_ack(&a, now).unwrap(),
                Item::Ping { id, ts } => {
                    self.push(ChunkKind::Pong, Item::Pong(Pong { ping_id: id, echo_ts: ts, hold_10us: 0 }))
                }
                Item::Pong(p) => {
                    self.rtt.on_pong(now, &p);
                }
            }
        }
        while let Some(m) = self.cp.pop() {
            match m {
                Ok(m) => self.got.push(m),
                Err(_) => self.bad += 1,
            }
        }
    }

    /// 本端这一刻要发的包
    fn transmit(&mut self, now: Micros, out: &mut Pipe<Pkt>) {
        if now >= self.next_ping {
            self.ping_id = self.ping_id.wrapping_add(1);
            self.push(ChunkKind::Ping, Item::Ping { id: self.ping_id, ts: send_ts(now) });
            self.next_ping = now + ms(100);
        }
        let rto = self.rtt.rto();
        while let Some((f, p)) = self.cp.poll(now, rto) {
            let it = Item::Frag(f);
            let len = it.len();
            self.sched.push(p, len, it, SmallOpts::default());
        }
        // 有包要发时 ACK 搭车（不等 2 ms），否则到期再单独发
        let force = self.sched.small_due(now);
        while let Some(a) = self.cp.take_ack(now, force) {
            self.push(ChunkKind::Ack, Item::Ack(a));
        }
        let room = self.room();
        while let Some(p) = self.sched.pack(now, room, None) {
            assert!(p.used <= room);
            out.send(now, p.small);
            self.packets += 1;
        }
    }

    fn next_event(&self) -> Option<Micros> {
        let t = min_opt(self.cp.next_deadline(), self.sched.next_due());
        min_opt(t, Some(self.next_ping))
    }
}

/// 网关（Down）和客户端（Up）经两条单向链路对接
struct World {
    t: Micros,
    gw: Side,
    cl: Side,
    down: Pipe<Pkt>,
    up: Pipe<Pkt>,
}

impl World {
    fn new(down: LinkCfg, up: LinkCfg, seed: u64) -> Self {
        let rtt0 = down.delay + up.delay;
        Self {
            t: 0,
            gw: Side::new(Dir::Down, rtt0),
            cl: Side::new(Dir::Up, rtt0),
            down: Pipe::with_cfg(down, seed),
            up: Pipe::with_cfg(up, seed.wrapping_mul(31).wrapping_add(7)),
        }
    }

    /// 跑一拍：先收，再发
    fn step(&mut self) {
        let t = self.t;
        while let Some(p) = self.down.recv(t) {
            self.cl.on_packet(t, p);
        }
        while let Some(p) = self.up.recv(t) {
            self.gw.on_packet(t, p);
        }
        self.gw.transmit(t, &mut self.down);
        self.cl.transmit(t, &mut self.up);
    }

    /// 推进到 until：每次跳到下一个事件（到达、计时器），但最多跳 1 ms
    fn run_until(&mut self, until: Micros) {
        while self.t < until {
            self.step();
            let next = [self.down.next_arrival(), self.up.next_arrival(), self.gw.next_event(), self.cl.next_event()]
                .into_iter()
                .fold(None, min_opt)
                .unwrap_or(until);
            self.t = next.clamp(self.t + 1, (self.t + ms(1)).min(until.max(self.t + 1)));
        }
    }

    fn idle(&self) -> bool {
        self.gw.cp.is_idle() && self.cl.cp.is_idle()
    }
}

fn lossy(delay: Micros, seed_mix: f64) -> LinkCfg {
    LinkCfg {
        delay,
        jitter: ms(4),
        loss: 0.10 + seed_mix * 0.10,
        dup: 0.05,
        reorder: 0.10,
        reorder_extra: ms(30),
    }
}

/// 随机挑一条网关 → 客户端的消息（覆盖流 0、流 1，关键和普通）
fn gw_msg(rng: &mut Rng, i: u32) -> ControlMsg {
    match rng.below(4) {
        0 => ControlMsg::SessionStatus(SessionStatus {
            ps_state: (i % 10) as u8,
            state_ms: Some(i),
            text: Some(format!("第 {i} 条")),
            ..Default::default()
        }),
        1 => ControlMsg::VideoConfig(VideoConfig {
            cfg_ver: i as u8,
            codec: 2,
            fps: 60,
            profiles: vec![VideoProfile { idx: 0, width: 1920, height: 1080, param_sets: vec![i as u8; 90] }],
        }),
        2 => ControlMsg::AudioConfig(AudioConfig { channels: 2, rate: 48_000, frame_samples: 480, codec: 1 }),
        _ => ControlMsg::Error { code: i as u16, text: String::new() },
    }
}

fn cl_msg(rng: &mut Rng, i: u32) -> ControlMsg {
    match rng.below(3) {
        0 => ControlMsg::IdrRequest { reason: (i % 5) as u8 },
        1 => ControlMsg::ModeSet { mode: (i % 3) as u8, j_ms: i as u8 },
        _ => ControlMsg::Error { code: i as u16, text: "x".into() },
    }
}

fn by_stream(v: &[ControlMsg], s: u8) -> Vec<ControlMsg> {
    v.iter().filter(|m| m.stream() == s).cloned().collect()
}

/// 种子随机的丢包、重复、乱序下，两个方向的 CONTROL 最终都按序、不重不漏地交付
#[test]
fn control_delivers_in_order_under_loss_dup_reorder() {
    for seed in 1..=12u64 {
        let mix = (seed % 3) as f64 / 2.0; // 丢包率 10%、15%、20%
        let mut w = World::new(lossy(ms(10), mix), lossy(ms(9), mix), seed);
        let mut rng = Rng::new(seed ^ 0xABCD);
        let mut sent_gw = Vec::new();
        let mut sent_cl = Vec::new();
        // 10 s 里随机时刻发 200 + 60 条
        let mut plan: Vec<(Micros, bool)> = (0..200).map(|_| (rng.below(secs(10)), true)).collect();
        plan.extend((0..60).map(|_| (rng.below(secs(10)), false)));
        plan.sort_unstable();
        for (i, (at, from_gw)) in plan.into_iter().enumerate() {
            w.run_until(at);
            if from_gw {
                let m = gw_msg(&mut rng, i as u32);
                w.gw.cp.send(&m).unwrap();
                sent_gw.push(m);
            } else {
                let m = cl_msg(&mut rng, i as u32);
                w.cl.cp.send(&m).unwrap();
                sent_cl.push(m);
            }
        }
        // 发完之后最多再跑 30 s，期间必须全部确认
        let deadline = w.t + secs(30);
        while !w.idle() && w.t < deadline {
            let t = w.t + ms(50);
            w.run_until(t);
        }
        // 再多跑一会儿，让最后的 ACK、重复包都落地
        let t = w.t + secs(2);
        w.run_until(t);
        assert!(w.idle(), "种子 {seed}：30 s 内没有全部确认");
        for s in [stream::SESSION, stream::CONFIG] {
            assert_eq!(by_stream(&w.cl.got, s), by_stream(&sent_gw, s), "种子 {seed}：下行流 {s} 不一致");
            assert_eq!(by_stream(&w.gw.got, s), by_stream(&sent_cl, s), "种子 {seed}：上行流 {s} 不一致");
        }
        assert_eq!(w.cl.got.len(), sent_gw.len());
        assert_eq!(w.gw.got.len(), sent_cl.len());
        assert_eq!(w.cl.bad + w.gw.bad, 0);
        // 链路确实注入了损伤
        let s = w.down.stats();
        assert!(s.lost > 0 && s.duplicated > 0 && s.reordered > 0, "种子 {seed}：{s:?}");
        // 重传确实发生了
        let rt: u64 = (0..3).map(|s| w.gw.cp.tx(s).unwrap().stats().retransmits).sum();
        assert!(rt > 0, "种子 {seed}：没有重传");
    }
}

/// msg_seq 跨 0xFFFF → 0 回绕：在丢包、乱序的链路上从 0xFF00 起发 1000 条
#[test]
fn msg_seq_wraps_on_lossy_link() {
    for seed in [3u64, 17, 99] {
        let start = 0xFF00;
        let mut tx = StreamTx::new(stream::SESSION, 1100).with_start_seq(start);
        let mut rx = StreamRx::new(stream::SESSION).with_start_seq(start);
        let cfg = LinkCfg { delay: ms(8), jitter: ms(3), loss: 0.15, dup: 0.05, reorder: 0.1, reorder_extra: ms(20) };
        let mut down: Pipe<ControlFragOwned> = Pipe::with_cfg(cfg, seed);
        let mut up: Pipe<Ack> = Pipe::with_cfg(cfg, seed + 1);
        let n = 1000u32;
        let mut next = 0u32;
        let mut got = Vec::new();
        let mut t: Micros = 0;
        while next < n || !tx.is_idle() {
            assert!(t < secs(600), "种子 {seed}：没有收敛");
            while next < n && tx.backlog() < 100 {
                tx.send(next.to_be_bytes().to_vec(), next.is_multiple_of(7)).unwrap();
                next += 1;
            }
            while let Some(f) = down.recv(t) {
                let _ = rx.on_frag(&ControlFrag { stream: f.stream, msg_seq: f.msg_seq, frag: 0, data: &f.data }, t);
            }
            while let Some(b) = rx.pop() {
                got.push(u32::from_be_bytes(b.try_into().unwrap()));
            }
            if let Some(a) = rx.take_ack(t, false) {
                up.send(t, a);
            }
            while let Some(a) = up.recv(t) {
                tx.on_ack(&a, t);
            }
            while let Some((f, _)) = tx.poll(t, ms(25)) {
                down.send(t, f);
            }
            t += 500;
        }
        assert_eq!(got, (0..n).collect::<Vec<_>>(), "种子 {seed}");
        assert_eq!(rx.current_ack().cum, start.wrapping_add(n as u16 - 1));
    }
}

/// 不借助 with_start_seq，自然发满 70000 条，msg_seq 回绕一圈多
#[test]
fn msg_seq_wraps_naturally() {
    let mut tx = StreamTx::new(stream::CONFIG, 240);
    let mut rx = StreamRx::new(stream::CONFIG);
    const N: u32 = 70_000;
    let mut t = 0;
    let mut got = 0u32;
    let mut next = 0u32;
    let mut n = 0u64;
    while next < N || !tx.is_idle() {
        assert!(t < secs(3_600), "没有收敛");
        // 积压不超过 200 条
        while next < N && tx.backlog() < 200 {
            tx.send(next.to_be_bytes().to_vec(), false).unwrap();
            next += 1;
        }
        // 每发 5 片丢 1 片，靠 RTO 补
        while let Some((f, _)) = tx.poll(t, ms(25)) {
            if !n.is_multiple_of(5) {
                let _ = rx.on_frag(&view(&f), t);
            }
            n += 1;
        }
        while let Some(b) = rx.pop() {
            assert_eq!(u32::from_be_bytes(b.try_into().unwrap()), got);
            got += 1;
        }
        if let Some(a) = rx.take_ack(t, true) {
            tx.on_ack(&a, t);
        }
        t += ms(1);
    }
    assert_eq!(got, N);
    assert_eq!(rx.current_ack().cum, (70_000u32 - 1) as u16);
}

/// 关键消息在 +5 ms 再发一份；普通消息只发一份；收端把第二份当重复丢掉
#[test]
fn critical_messages_sent_twice() {
    let mut w = World::new(LinkCfg::ideal(ms(10)), LinkCfg::ideal(ms(10)), 1);
    for i in 0..20u32 {
        w.gw.cp.send(&ControlMsg::SessionStatus(SessionStatus { ps_state: 4, state_ms: Some(i), ..Default::default() }))
            .unwrap();
        w.gw.cp.send(&ControlMsg::AudioConfig(AudioConfig { channels: 2, rate: 48_000, frame_samples: 480, codec: 1 }))
            .unwrap();
        let t = w.t + ms(40);
        w.run_until(t);
    }
    let t = w.t + secs(1);
    w.run_until(t);
    assert!(w.idle());
    let s0 = w.gw.cp.tx(stream::SESSION).unwrap().stats();
    let s1 = w.gw.cp.tx(stream::CONFIG).unwrap().stats();
    assert_eq!((s0.sent, s0.dups, s0.retransmits), (20, 20, 0));
    assert_eq!((s1.sent, s1.dups, s1.retransmits), (20, 0, 0));
    assert_eq!(w.cl.cp.rx(stream::SESSION).unwrap().stats().duplicates, 20);
    assert_eq!(w.cl.cp.rx(stream::CONFIG).unwrap().stats().duplicates, 0);
    assert_eq!(w.cl.got.len(), 40);
}

/// 下行中断 3 s：重传间隔按 RTO 翻倍、封顶 1 s；恢复后 1 s 内送达
#[test]
fn rto_backoff_through_outage() {
    let mut w = World::new(LinkCfg::ideal(ms(10)), LinkCfg::ideal(ms(10)), 5);
    w.run_until(secs(2)); // 先让 RTT 稳定
    let rto0 = w.gw.rtt.rto();
    assert_eq!(rto0, ms(25), "20 ms 的稳定链路，RTO 落在下限");
    w.down.add_outage(secs(2), secs(5));
    w.gw.cp.send(&ControlMsg::Error { code: 1, text: String::new() }).unwrap();
    // 记录这条消息每次发出的时刻
    let mut sends = Vec::new();
    let mut last = 0;
    while w.cl.got.is_empty() {
        assert!(w.t < secs(10));
        w.run_until(w.t + 500);
        let st = w.gw.cp.tx(stream::SESSION).unwrap().stats();
        let n = st.sent + st.retransmits;
        if n != last {
            sends.push(w.t);
            last = n;
        }
    }
    let gaps: Vec<Micros> = sends.windows(2).map(|p| p[1] - p[0]).collect();
    // 每次间隔不比上次短，最后封顶 1 s（允许 1 ms 的步进误差）
    for g in gaps.windows(2) {
        assert!(g[1] + ms(1) >= g[0], "{gaps:?}");
    }
    assert!(gaps.iter().all(|&g| g <= CTL_RTO_MAX + ms(1)), "{gaps:?}");
    assert!(gaps.iter().any(|&g| g + ms(1) >= CTL_RTO_MAX), "{gaps:?}");
    assert!(gaps[0] <= ms(26), "{gaps:?}");
    // 中断在 5 s 结束，最晚下一次重传（≤ 1 s 后）就到
    assert!(w.t < secs(5) + CTL_RTO_MAX + ms(20), "到达 {}", w.t);
}

/// PING/PONG 的 RTT 估计跟得上真实 RTT，尖峰（437 ms）过后回落；尖峰期间 CONTROL 照样送达
#[test]
fn rtt_estimate_follows_link_and_spike() {
    let base = LinkCfg { delay: ms(9), jitter: ms(1), ..LinkCfg::default() };
    let mut w = World::new(base, LinkCfg { delay: ms(10), ..base }, 9);
    w.run_until(secs(3));
    let srtt = w.gw.rtt.srtt();
    assert!(srtt >= ms(19) && srtt <= ms(22), "srtt = {srtt}");
    assert!(w.cl.rtt.p50() >= ms(19) && w.cl.rtt.p50() <= ms(22));
    // 下行单程时延跳到 427 ms（RTT ≈ 437 ms），持续 2 s
    w.down.set_cfg(LinkCfg { delay: ms(427), ..base });
    w.gw.cp.send(&ControlMsg::SessionStatus(SessionStatus { ps_state: 6, ..Default::default() })).unwrap();
    w.run_until(secs(5));
    assert!(w.gw.rtt.latest() >= ms(430), "latest = {}", w.gw.rtt.latest());
    assert!(w.gw.rtt.rto() >= ms(430), "rto = {}", w.gw.rtt.rto());
    assert_eq!(w.cl.got.len(), 1, "尖峰期间关键消息也要送达");
    w.down.set_cfg(base);
    w.run_until(secs(12));
    let srtt = w.gw.rtt.srtt();
    assert!(srtt <= ms(25), "尖峰过后 srtt = {srtt}");
    assert!(w.gw.rtt.min() >= ms(19));
    assert!(w.idle());
}
