//! Assembler 的单测和 proptest。发端一律用真实的 VideoTx 切片、编码。

use super::*;
use crate::fec::tests::Rng;
use crate::nal::START_CODE;
use crate::video_tx::{FrameIn, VideoTx, VideoTxConfig};
use proptest::prelude::*;
use ywp::sched::{SymbolSource, VideoCarrier};
use ywp::time::{ms, secs};

const META: RxMeta = RxMeta { pn: 0, send_ts: 0, arrival: 0 };

/// 每个 cfg_ver 一份可区分的「参数集」（HEVC VPS 头 + 版本号）
fn params(ver: u8) -> Vec<u8> {
    let mut v = START_CODE.to_vec();
    v.extend_from_slice(&[0x40, 0x01, 0x0c, ver]);
    v
}

/// 造一帧：起始码 + HEVC NAL 头（IDR_N_LP 或 TRAIL_R）+ 随机数据
fn make_au(rng: &mut Rng, key: bool, len: usize) -> Vec<u8> {
    let mut v = START_CODE.to_vec();
    v.extend_from_slice(if key { &[0x28, 0x01] } else { &[0x02, 0x01] });
    let n = len.saturating_sub(v.len()).max(1);
    v.extend(rng.bytes(n).into_iter().map(|b| b | 0x10));
    v
}

/// 发端：一帧变成若干 (VideoHeader, body)
struct Sender {
    tx: VideoTx,
    fid: u16,
}

impl Sender {
    fn new() -> Self {
        Self { tx: VideoTx::new(VideoTxConfig::default()), fid: 0 }
    }

    fn frame(&mut self, now: Micros, au: &[u8], key: bool, cfg: u8, w: bool) -> Vec<(ywp::wire::VideoHeader, Vec<u8>)> {
        let mut out: Vec<VideoCarrier> = Vec::new();
        let f = FrameIn { fid: self.fid, key, cfg, profile: 0, recovered: w, t0: now, au };
        self.tx.send_frame(now, &f, &mut out).unwrap();
        self.fid = self.fid.wrapping_add(1);
        out.iter().map(|c| (c.hdr, self.tx.symbol(c.hdr.fid, c.hdr.sym_idx).unwrap().to_vec())).collect()
    }
}

fn feed(a: &mut Assembler, now: Micros, syms: &[(ywp::wire::VideoHeader, Vec<u8>)], ev: &mut Vec<AsmEvent>) {
    for (h, b) in syms {
        a.on_video(now, &META, &VideoChunk { hdr: *h, body: b }, ev);
    }
}

fn decoded(ev: &[AsmEvent]) -> Vec<&DecodeItem> {
    ev.iter()
        .filter_map(|e| match e {
            AsmEvent::Decode(d) => Some(d),
            _ => None,
        })
        .collect()
}

fn idrs(ev: &[AsmEvent]) -> Vec<u8> {
    ev.iter()
        .filter_map(|e| match e {
            AsmEvent::IdrRequest(r) => Some(*r),
            _ => None,
        })
        .collect()
}

fn with_params(ver: u8, au: &[u8]) -> Vec<u8> {
    let mut v = params(ver);
    v.extend_from_slice(au);
    v
}

fn asm() -> Assembler {
    Assembler::new(Policy::default())
}

#[test]
fn delivers_in_order_with_param_sets_before_keys() {
    let mut rng = Rng(1);
    let mut a = asm();
    let mut s = Sender::new();
    let mut ev = Vec::new();
    a.on_config(0, 1, &params(1), &mut ev);
    let aus: Vec<Vec<u8>> = (0..5).map(|i| make_au(&mut rng, i == 0, 3000 + i * 500)).collect();
    for (i, au) in aus.iter().enumerate() {
        let syms = s.frame(i as u64 * 16_667, au, i == 0, 1, false);
        feed(&mut a, i as u64 * 16_667 + 100, &syms, &mut ev);
    }
    let d = decoded(&ev);
    assert_eq!(d.len(), 5);
    assert_eq!(&*d[0].au, &with_params(1, &aus[0])[..]);
    assert!(d[0].key && !d[1].key);
    for i in 1..5 {
        assert_eq!(&*d[i].au, &aus[i][..]);
        assert_eq!(d[i].fid, i as u64);
        assert_eq!(d[i].cfg, 1);
    }
    assert!(idrs(&ev).is_empty());
    assert_eq!(a.stats().frames_ok, 5);
    let r = a.report();
    assert_eq!((r.frames_ok, r.oldest_needed_fid, r.blocked_fid, r.blocked_need), (5, 5, 0xFFFF, 0));
    assert_eq!(a.next_timer(), None);
}

#[test]
fn waits_for_key_frame_at_start_and_resends_idr() {
    let mut rng = Rng(2);
    let mut a = asm();
    let mut s = Sender::new();
    let mut ev = Vec::new();
    a.on_config(0, 1, &params(1), &mut ev);
    // 中途接入：先来的是 P 帧
    for i in 0..3 {
        let au = make_au(&mut rng, false, 2000);
        feed(&mut a, i * 16_667, &s.frame(i * 16_667, &au, false, 1, false), &mut ev);
    }
    assert!(decoded(&ev).is_empty());
    assert_eq!(a.next_timer(), Some(ms(1_000)));
    // 1 s 后 P 帧清掉；1.2 s 还没关键帧就再请求
    a.on_timer(ms(1_200), &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::KEY_WAIT_TIMEOUT]);
    assert_eq!(a.next_timer(), Some(ms(2_400)));
    let key = make_au(&mut rng, true, 30_000);
    feed(&mut a, ms(1_300), &s.frame(ms(1_300), &key, true, 1, false), &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].fid, 3);
    // 开始时没有画面，不算冻结
    assert!(!ev.iter().any(|e| matches!(e, AsmEvent::Freeze(_) | AsmEvent::Unfreeze)));
}

/// 送一个关键帧让 Assembler 进入正常状态
fn started(rng: &mut Rng) -> (Assembler, Sender, Vec<AsmEvent>) {
    let mut a = asm();
    let mut s = Sender::new();
    let mut ev = Vec::new();
    a.on_config(0, 1, &params(1), &mut ev);
    let key = make_au(rng, true, 5000);
    feed(&mut a, 0, &s.frame(0, &key, true, 1, false), &mut ev);
    assert_eq!(decoded(&ev).len(), 1);
    ev.clear();
    (a, s, ev)
}

#[test]
fn lost_when_later_frame_complete_for_50ms() {
    let mut rng = Rng(3);
    let (mut a, mut s, mut ev) = started(&mut rng);
    // fid 1 的源符号全丢（只剩 1 个校验，不够）
    let au1 = make_au(&mut rng, false, 5000);
    let syms = s.frame(ms(16), &au1, false, 1, false);
    let k = syms[0].0.k as usize;
    feed(&mut a, ms(17), &syms[k..k + 1], &mut ev);
    let au2 = make_au(&mut rng, false, 3000);
    feed(&mut a, ms(33), &s.frame(ms(33), &au2, false, 1, false), &mut ev);
    assert!(decoded(&ev).is_empty());
    let r = a.report();
    assert_eq!((r.blocked_fid, r.blocked_need), (1, (k - 1) as u8));
    assert_eq!(a.next_timer(), Some(ms(33) + ASM_LOST_BEHIND_COMPLETE));
    a.on_timer(ms(83) - 1, &mut ev);
    assert!(idrs(&ev).is_empty());
    a.on_timer(ms(83), &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::FRAME_LOST]);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Freeze(FreezeWhy::Lost))));
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Stat(StatEvent::Lost { fid: 1 }))));
    // fid 2 是 P 帧，不能送；等到关键帧
    assert!(decoded(&ev).is_empty());
    let key = make_au(&mut rng, true, 9000);
    feed(&mut a, ms(500), &s.frame(ms(500), &key, true, 1, false), &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].fid, 3);
    assert!(matches!(ev.last(), Some(AsmEvent::Decode(_))));
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Unfreeze)));
    assert_eq!(a.stats().freezes, 1);
}

#[test]
fn lost_when_hanging_1000ms_and_whole_frame_gap() {
    let mut rng = Rng(4);
    let (mut a, mut s, mut ev) = started(&mut rng);
    // fid 1 整帧没收到；fid 2 也不完整：从 fid 2 的第一个符号算 1000 ms
    let _ = s.frame(ms(16), &make_au(&mut rng, false, 5000), false, 1, false);
    let syms = s.frame(ms(33), &make_au(&mut rng, false, 5000), false, 1, false);
    feed(&mut a, ms(34), &syms[..1], &mut ev);
    assert_eq!(a.report().blocked_need, 0, "后面没有完整的帧，不算阻塞");
    assert_eq!(a.next_timer(), Some(ms(34) + ASM_FRAME_HANG));
    a.on_timer(ms(1_034), &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::FRAME_LOST]);
    // fid 1 判 Lost 之后 fid 2 还挂着，等关键帧时 1000 ms 一到就清掉
    assert_eq!(a.stats().frames_lost, 2);
}

#[test]
fn complete_key_frame_unblocks() {
    let mut rng = Rng(5);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let _ = s.frame(ms(16), &make_au(&mut rng, false, 5000), false, 1, false);
    let key = make_au(&mut rng, true, 20_000);
    feed(&mut a, ms(40), &s.frame(ms(33), &key, true, 1, false), &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].fid, 2);
    assert!(idrs(&ev).is_empty());
    assert_eq!(a.stats().frames_lost, 1);
}

#[test]
fn fec_recovers_missing_sources() {
    let mut rng = Rng(6);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let au = make_au(&mut rng, false, 4000);
    let syms = s.frame(ms(16), &au, false, 1, false);
    // k = 4，m = 2：丢 2 个源符号
    assert_eq!(syms.len(), 6);
    feed(&mut a, ms(17), &[syms[0].clone(), syms[3].clone(), syms[4].clone(), syms[5].clone()], &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!(&*d[0].au, &au[..]);
    assert_eq!(a.stats().fec_recovered, 1);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Stat(StatEvent::FecRecovered { fid: 1 }))));
    // 多出来的校验和重复包
    ev.clear();
    feed(&mut a, ms(18), &syms[1..2], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoStaleFid), 1);
}

#[test]
fn key_pending_freezes_and_stale_copy_is_ignored() {
    let mut rng = Rng(7);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let kp = FrameStatusEntry { fid: 1, kind: FrameStatusKind::KeyPending, arg: 0 };
    a.on_frame_status(ms(10), &kp, &mut ev);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Freeze(FreezeWhy::KeyPending))));
    assert!(idrs(&ev).is_empty(), "网关已经在请求，客户端不另发");
    // 重复的副本不再冻结一次
    a.on_frame_status(ms(11), &kp, &mut ev);
    assert_eq!(a.stats().freezes, 1);
    let key = make_au(&mut rng, true, 9000);
    feed(&mut a, ms(300), &s.frame(ms(300), &key, true, 1, false), &mut ev);
    assert_eq!(decoded(&ev).len(), 1);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Unfreeze)));
    // 关键帧之后才到的旧副本：忽略
    ev.clear();
    a.on_frame_status(ms(301), &kp, &mut ev);
    assert!(ev.is_empty());
    let p = make_au(&mut rng, false, 900);
    feed(&mut a, ms(316), &s.frame(ms(316), &p, false, 1, false), &mut ev);
    assert_eq!(decoded(&ev).len(), 1);
}

#[test]
fn gone_frame_is_lost_immediately() {
    let mut rng = Rng(8);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let syms = s.frame(ms(16), &make_au(&mut rng, false, 5000), false, 1, false);
    feed(&mut a, ms(17), &syms[..1], &mut ev);
    a.on_frame_status(ms(20), &FrameStatusEntry { fid: 1, kind: FrameStatusKind::Gone, arg: 0 }, &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::FRAME_LOST]);
}

#[test]
fn cfg_mismatch_is_held_until_new_config() {
    let mut rng = Rng(9);
    let (mut a, mut s, mut ev) = started(&mut rng);
    // PS5 会话重建：新关键帧（cfg 2）比新 VIDEO_CONFIG 先到
    let key = make_au(&mut rng, true, 20_000);
    feed(&mut a, ms(100), &s.frame(ms(100), &key, true, 2, false), &mut ev);
    assert!(decoded(&ev).is_empty());
    assert_eq!(a.next_timer(), Some(ms(100) + ASM_CFG_HOLD_MAX));
    a.on_config(ms(130), 2, &params(2), &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].cfg, 2);
    assert_eq!(&*d[0].au, &with_params(2, &key)[..]);
    assert!(idrs(&ev).is_empty());
}

#[test]
fn cfg_change_restarts_from_next_key() {
    let mut rng = Rng(10);
    let (mut a, mut s, mut ev) = started(&mut rng);
    a.on_config(ms(10), 2, &params(2), &mut ev);
    // 配置换了，旧配置的 P 帧压着、超时；新配置的关键帧到了才送
    let p = make_au(&mut rng, false, 3000);
    feed(&mut a, ms(16), &s.frame(ms(16), &p, false, 1, false), &mut ev);
    assert!(decoded(&ev).is_empty());
    let key = make_au(&mut rng, true, 20_000);
    feed(&mut a, ms(40), &s.frame(ms(40), &key, true, 2, false), &mut ev);
    let d = decoded(&ev);
    assert_eq!(d.len(), 1);
    assert_eq!((d[0].fid, d[0].cfg), (2, 2));
    assert!(idrs(&ev).is_empty());
}

#[test]
fn cfg_hold_times_out_after_1000ms() {
    let mut rng = Rng(11);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let p = make_au(&mut rng, false, 3000);
    feed(&mut a, ms(16), &s.frame(ms(16), &p, false, 3, false), &mut ev);
    a.on_timer(ms(1_015), &mut ev);
    assert!(idrs(&ev).is_empty());
    a.on_timer(ms(1_016), &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::CFG_TIMEOUT]);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Freeze(FreezeWhy::CfgTimeout))));
    assert_eq!(a.stats().cfg_timeouts, 1);
    // 等关键帧时，关键帧的 cfg 也对不上：再压 1000 ms，超时再请求
    ev.clear();
    let key = make_au(&mut rng, true, 9000);
    feed(&mut a, ms(1_100), &s.frame(ms(1_100), &key, true, 3, false), &mut ev);
    a.on_timer(ms(2_100), &mut ev);
    assert_eq!(idrs(&ev), vec![idr_reason::CFG_TIMEOUT]);
    assert_eq!(a.stats().cfg_timeouts, 2);
    assert!(decoded(&ev).is_empty());
}

#[test]
fn w_frames_and_decoder_errors() {
    let mut rng = Rng(12);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let p = make_au(&mut rng, false, 3000);
    feed(&mut a, ms(16), &s.frame(ms(16), &p, false, 1, true), &mut ev);
    assert_eq!(decoded(&ev).len(), 1, "W 帧照常送解码");
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Stat(StatEvent::WFrame { fid: 1 }))));
    assert_eq!(a.stats().w_frames, 1);
    // W 帧之后 1 s 内的解码报错单独记
    a.on_decoder_error(ms(500), 1, &mut ev);
    assert_eq!(a.stats().w_errors, 1);
    assert_eq!(idrs(&ev), vec![idr_reason::DECODER_ERROR]);
    assert!(ev.iter().any(|e| matches!(e, AsmEvent::Freeze(FreezeWhy::DecoderError))));
    // 已在等关键帧：再报错不重复请求
    a.on_decoder_error(ms(600), 1, &mut ev);
    assert_eq!(idrs(&ev).len(), 1);
    let key = make_au(&mut rng, true, 9000);
    feed(&mut a, ms(700), &s.frame(ms(700), &key, true, 1, false), &mut ev);
    // 关键帧之前的帧报的错：不理会；W 窗口外的错不计入 w_errors
    ev.clear();
    a.on_decoder_error(ms(1_600), 1, &mut ev);
    assert!(ev.is_empty());
    assert_eq!(a.stats().decoder_errors, 3);
    assert_eq!(a.stats().w_errors, 2);
}

#[test]
fn rejects_bad_headers() {
    let mut rng = Rng(13);
    let (mut a, mut s, mut ev) = started(&mut rng);
    let syms = s.frame(ms(16), &make_au(&mut rng, false, 4000), false, 1, false);
    let mut bad = syms[0].clone();
    bad.0.parity = true;
    feed(&mut a, ms(17), &[bad], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoParityMismatch), 1);
    let mut odd = syms[4].clone();
    odd.1.pop();
    feed(&mut a, ms(17), &[odd], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoOddShard), 1);
    let mut kk = syms[1].clone();
    feed(&mut a, ms(17), &syms[..1], &mut ev);
    kk.0.k = 5;
    feed(&mut a, ms(17), &[kk], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoKMismatch), 1);
    feed(&mut a, ms(17), &syms[..1], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoDuplicate), 1);
    let mut short = syms[5].clone();
    short.1.truncate(short.1.len() - 2);
    feed(&mut a, ms(17), &syms[4..5], &mut ev);
    feed(&mut a, ms(17), &[short], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoShardMismatch), 1);
    let mut zero = syms[0].clone();
    zero.0.k = 0;
    feed(&mut a, ms(17), &[zero], &mut ev);
    assert_eq!(a.drops().get(DropReason::VideoBadFec), 1);
    // 其余符号到齐，照样能组出来
    feed(&mut a, ms(18), &syms[1..4], &mut ev);
    assert_eq!(decoded(&ev).len(), 1);
}

// ---------------- proptest：仿真一条有损的链路 ----------------

#[derive(Clone, Debug)]
enum Ev {
    Sym(ywp::wire::VideoHeader, Vec<u8>),
    Config(u8),
}

#[derive(Clone, Copy, Debug)]
struct Net {
    seed: u64,
    frames: usize,
    loss_pct: u32,
    dup_pct: u32,
    jitter_ms: u64,
    /// 第一份 VIDEO_CONFIG 晚到多久
    cfg0_delay_ms: u64,
    /// 在第几帧换配置（关键帧），新 VIDEO_CONFIG 相对这一帧的发送时刻晚到多久（可以为负）
    switch: Option<(usize, i64)>,
}

fn arb_net() -> impl Strategy<Value = Net> {
    (any::<u64>(), 20usize..150, 0u32..25, 0u32..10, 0u64..40, 0u64..1_500, proptest::option::of((5usize..100, -40i64..1_500))).prop_map(
        |(seed, frames, loss_pct, dup_pct, jitter_ms, cfg0_delay_ms, switch)| Net {
            seed,
            frames,
            loss_pct,
            dup_pct,
            jitter_ms,
            cfg0_delay_ms,
            switch,
        },
    )
}

struct Sim {
    /// 原帧：(cfg_ver, key, au, k)
    sent: Vec<(u8, bool, Vec<u8>, usize)>,
    /// 按到达时刻排好的事件
    events: Vec<(Micros, Ev)>,
}

fn build(n: &Net) -> Sim {
    let mut rng = Rng(n.seed | 1);
    let mut s = Sender::new();
    let mut sent = Vec::new();
    let mut events = Vec::new();
    let mut ver = 1u8;
    events.push((ms(n.cfg0_delay_ms), Ev::Config(1)));
    for i in 0..n.frames {
        let t = ms(5) + i as u64 * 16_667;
        if let Some((at, delay)) = n.switch {
            if i == at {
                ver = 2;
                events.push(((t as i64 + delay * 1_000).max(0) as Micros, Ev::Config(2)));
            }
        }
        let key = i == 0 || i % 40 == 0 || n.switch.is_some_and(|(at, _)| at == i);
        let len = if key { 15_000 + rng.below(15_000) } else { 200 + rng.below(12_000) };
        let au = make_au(&mut rng, key, len);
        let syms = s.frame(t, &au, key, ver, rng.below(20) == 0);
        sent.push((ver, key, au, syms[0].0.k as usize));
        for (j, (h, b)) in syms.into_iter().enumerate() {
            if (rng.below(100) as u32) < n.loss_pct {
                continue;
            }
            let copies = if (rng.below(100) as u32) < n.dup_pct { 2 } else { 1 };
            for _ in 0..copies {
                let at = t + j as u64 * 50 + ms(10) + rng.below(n.jitter_ms as usize * 1000 + 1) as u64;
                events.push((at, Ev::Sym(h, b.clone())));
            }
        }
    }
    events.sort_by_key(|(t, _)| *t);
    Sim { sent, events }
}

/// 跑完仿真，逐个检查送解码的帧；返回送出去的帧数
fn run_and_check(n: &Net) -> Result<usize, TestCaseError> {
    let sim = build(n);
    let mut a = asm();
    let mut ev = Vec::new();
    let mut cur_ver: Option<u8> = None;
    let mut last_fid: Option<u64> = None;
    let mut got = 0usize;
    // 每个 fid 实际送到 Assembler 的不同符号
    let mut seen: BTreeMap<u64, BTreeSet<u16>> = BTreeMap::new();
    let end = sim.events.last().map_or(0, |(t, _)| *t) + secs(3);
    let mut i = 0;
    let mut guard = 0u32;
    loop {
        let next_ev = sim.events.get(i).map(|(t, _)| *t);
        let next_tm = a.next_timer();
        let now = match (next_ev, next_tm) {
            (Some(e), Some(t)) if t < e => t,
            (Some(e), _) => e,
            (None, Some(t)) if t <= end => t,
            _ => break,
        };
        if Some(now) == next_tm && next_ev != Some(now) {
            a.on_timer(now, &mut ev);
            guard += 1;
            prop_assert!(guard < 100_000, "计时器空转");
        } else {
            match &sim.events[i].1 {
                Ev::Sym(h, b) => {
                    seen.entry(h.fid as u64).or_default().insert(h.sym_idx);
                    a.on_video(now, &META, &VideoChunk { hdr: *h, body: b }, &mut ev);
                }
                Ev::Config(v) => {
                    cur_ver = Some(*v);
                    a.on_config(now, *v, &params(*v), &mut ev);
                }
            }
            i += 1;
        }
        // 到期的事都已处理：下一个计时器一定晚于现在
        prop_assert!(a.next_timer().is_none_or(|t| t > now), "计时器没有前进");
        for e in ev.drain(..) {
            let AsmEvent::Decode(d) = e else { continue };
            // fid 严格递增
            prop_assert!(last_fid.is_none_or(|l| d.fid > l), "fid {} 不递增", d.fid);
            last_fid = Some(d.fid);
            let (ver, key, au, k) = &sim.sent[d.fid as usize];
            // cfg 对上了才送：用的是送出时刻的当前配置，而且和帧头的 cfg 一致
            prop_assert_eq!(Some(d.cfg), cur_ver);
            prop_assert_eq!(d.cfg, *ver);
            // 不完整的帧从不送出
            prop_assert!(seen.get(&d.fid).map_or(0, |s| s.len()) >= *k);
            // 字节和原帧一致（关键帧前面补了参数集）
            let want = if *key { with_params(*ver, au) } else { au.clone() };
            prop_assert!(*d.au == want[..], "fid {} 字节不一致", d.fid);
            prop_assert_eq!(d.key, *key);
            // 收完整之后压着不超过 1000 ms
            prop_assert!(d.p_us - d.complete_us <= ASM_CFG_HOLD_MAX, "fid {} 压了 {} us", d.fid, d.p_us - d.complete_us);
            got += 1;
        }
    }
    Ok(got)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, ..ProptestConfig::default() })]

    #[test]
    fn assembler_invariants(n in arb_net()) {
        run_and_check(&n)?;
    }
}

/// 没有损伤时每一帧都送出。中途换配置：新 VIDEO_CONFIG 在新关键帧发出 45 ms 后才到（关键帧先压着），
/// 旧配置的帧那时都已交付（换配置时还没交付的旧帧会被丢掉，从新关键帧重新开始）
#[test]
fn clean_link_delivers_everything() {
    for seed in 1..6 {
        let n = Net { seed, frames: 120, loss_pct: 0, dup_pct: 0, jitter_ms: 0, cfg0_delay_ms: 0, switch: Some((60, 45)) };
        assert_eq!(run_and_check(&n).unwrap(), 120);
        // 有乱序、重复，但不丢包
        let n = Net { jitter_ms: 30, dup_pct: 5, ..n };
        assert_eq!(run_and_check(&n).unwrap(), 120);
    }
}
