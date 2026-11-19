//! 网关侧视频发送（M2a 基础版，M2c 补修复）：切片、RS 编码、发送缓存、修复记账。
//!
//! M2a 的做法（规格 §7.1 v0.2 注、§7.5、§8.6）：
//! - 整帧均分成 k 个源符号，按固定冗余表生成 R = m 个校验，全部放进这一帧的连续缓冲 (k + R) × S；
//! - `send_frame` 产出 k 个源符号（优先级 4）和 m 个初始校验（优先级 5）的载体，正文在组包时经
//!   [`SymbolSource`] 取，不拷贝；
//! - 发送缓存保留 1 s 或 8 MB，先到为准（按帧入缓存的时刻算），在每次调用时顺手清理；
//! - 修复记账（NACK、截止期、缺口）M2c 再做。M2a 收到 NEED 时只按规格 §8.5「need = 0xFF、没有报丢记录」
//!   的做法补发全部源符号。

use crate::fec::{self, FecError, RsEnc};
use crate::parity::{LossState, Mode, ParityTable};
use std::collections::VecDeque;
use ywp::sched::{Prio, SymbolSource, VideoCarrier};
use ywp::time::{seq_lt, Micros, Unwrap16, REPAIR_IDEMPOTENT_MIN};
use ywp::wire::{Need, Report, VideoHeader};

/// Ingress 交给 VideoTx 的一帧
#[derive(Clone, Copy, Debug)]
pub struct FrameIn<'a> {
    pub fid: u16,
    pub key: bool,
    /// VIDEO_CONFIG 版本号的低 2 位
    pub cfg: u8,
    pub profile: u8,
    /// chiaki 改写过参考（写进 meta 的 W 位）
    pub recovered: bool,
    /// 本帧 t0（M2a~M2d 是 chiaki 交帧回调的时刻，M2e 起是首 unit 的 socket 收包时刻）
    pub t0: Micros,
    pub au: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedReply {
    Repairing,
    /// 发送缓存里已经没有这一帧（回 FRAME_STATUS GONE）
    Gone,
    /// 网关已在等关键帧（回 KEY_PENDING）
    KeyPending,
}

#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct VideoTxConfig {
    /// 发送缓存上限（默认 1 s / 8 MB）
    pub cache_age: Micros,
    pub cache_bytes: usize,
}

impl Default for VideoTxConfig {
    fn default() -> Self {
        Self { cache_age: ywp::time::SEND_CACHE_AGE, cache_bytes: ywp::time::SEND_CACHE_BYTES }
    }
}

#[cfg(test)]
mod config_tests {
    #[test]
    fn default_cache_is_not_zero() {
        let c = super::VideoTxConfig::default();
        assert_eq!(c.cache_age, ywp::time::SEND_CACHE_AGE);
        assert_eq!(c.cache_bytes, ywp::time::SEND_CACHE_BYTES);
    }
}

/// 模式、J、S_b90、RTT（归 C 组，字段可能还要加）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxParams {
    pub mode: Mode,
    pub j_us: u32,
    pub sb90_us: u32,
    pub rtt_p50_us: u32,
}

#[derive(Debug)]
#[non_exhaustive]
pub enum TxError {
    /// k 超过 1024，网关不转发、进 WaitKey
    FrameTooLarge { len: usize },
    Fec(FecError),
    /// fid 没有比缓存里最新的一帧更新（Ingress 的 fid 必须逐帧递增）
    StaleFid { fid: u16 },
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxError::FrameTooLarge { len } => write!(f, "帧太大：{len} B"),
            TxError::Fec(e) => write!(f, "FEC：{e}"),
            TxError::StaleFid { fid } => write!(f, "fid {fid} 没有递增"),
        }
    }
}

impl std::error::Error for TxError {}

#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct VideoTxStats {
    pub frames: u64,
    pub key_frames: u64,
    pub src_symbols: u64,
    pub parity_symbols: u64,
    pub repair_symbols: u64,
    pub bytes: u64,
}

/// 发送缓存里的一帧
#[derive(Debug)]
struct TxFrame {
    /// 展开后的 fid，用来排载体的 order
    ufid: u64,
    t0: Micros,
    /// 进缓存的时刻
    queued: Micros,
    k: u16,
    r: u16,
    s: usize,
    /// (k + R) × S：源符号 `[len][数据][补 0]`，之后是校验
    buf: Vec<u8>,
    /// 各符号共用的头字段（key、fid、k、profile、n、cfg、W）
    base: VideoHeader,
    /// 上一次整帧补发的时刻（幂等窗口）
    last_repair: Option<Micros>,
}

impl TxFrame {
    /// 源符号只给数据部分（不含 len，组包时由 VIDEO 块的编码写 len）；校验符号给 S 字节
    fn symbol(&self, sym: u16) -> Option<&[u8]> {
        let (i, k, r, s) = (sym as usize, self.k as usize, self.r as usize, self.s);
        if i < k {
            fec::shard_data(&self.buf[i * s..(i + 1) * s])
        } else if i < k + r {
            Some(&self.buf[i * s..(i + 1) * s])
        } else {
            None
        }
    }

    fn carrier(&self, now: Micros, sym: u16, last_src: bool, repair: bool, prio: Prio) -> VideoCarrier {
        let parity = sym >= self.k;
        VideoCarrier {
            hdr: VideoHeader {
                parity,
                last_src,
                repair,
                sym_idx: sym,
                r: if parity { self.r } else { 0 },
                age_100us: age_100us(now, self.t0),
                ..self.base
            },
            prio,
            order: order(self.ufid, sym),
        }
    }
}

/// 发送时刻 − t0，单位 100 µs，封顶 0xFFFF
fn age_100us(now: Micros, t0: Micros) -> u16 {
    (now.saturating_sub(t0) / 100).min(u16::MAX as u64) as u16
}

/// 同一优先级里越旧的帧、越小的 sym_idx 越先发：展开后的 fid × 4096 + sym_idx（k + R ≤ 2048）
fn order(ufid: u64, sym: u16) -> u64 {
    (ufid << 12) | sym as u64
}

#[derive(Debug)]
pub struct VideoTx {
    cfg: VideoTxConfig,
    params: TxParams,
    blind: bool,
    table: ParityTable,
    enc: RsEnc,
    frames: VecDeque<TxFrame>,
    /// 缓存里所有帧的 buf 总长
    bytes: usize,
    fids: Unwrap16,
    stats: VideoTxStats,
}

impl VideoTx {
    pub fn new(cfg: VideoTxConfig) -> Self {
        Self {
            cfg,
            params: TxParams::default(),
            blind: false,
            table: ParityTable::fixed(),
            enc: RsEnc::new(),
            frames: VecDeque::new(),
            bytes: 0,
            fids: Unwrap16::new(),
            stats: VideoTxStats::default(),
        }
    }

    /// 切片、RS，源符号加初始校验交给调度
    pub fn send_frame(&mut self, now: Micros, f: &FrameIn, out: &mut Vec<VideoCarrier>) -> Result<(), TxError> {
        self.evict(now);
        let (k, d) = fec::split_frame(f.au.len()).map_err(|e| match e {
            FecError::FrameTooLarge { len } => TxError::FrameTooLarge { len },
            e => TxError::Fec(e),
        })?;
        if let Some(last) = self.frames.back() {
            if !seq_lt(last.base.fid, f.fid) {
                return Err(TxError::StaleFid { fid: f.fid });
            }
        }
        let st = LossState { blind: self.blind, ..LossState::default() };
        let (m, r) = self.table.plan(k, f.key, self.params.mode, &st);
        let s = fec::shard_len(d);
        let (ku, ru) = (k as usize, r as usize);
        let mut buf = vec![0u8; (ku + ru) * s];
        fec::pack_source(f.au, ku, d, s, &mut buf[..ku * s]).map_err(TxError::Fec)?;
        let (src, par) = buf.split_at_mut(ku * s);
        self.enc.encode(ku, ru, s, src, par).map_err(TxError::Fec)?;

        let frame = TxFrame {
            ufid: self.fids.unwrap(f.fid),
            t0: f.t0,
            queued: now,
            k,
            r,
            s,
            buf,
            base: VideoHeader {
                key: f.key,
                fid: f.fid,
                k,
                profile: f.profile,
                n: 1,
                cfg: f.cfg & ywp::consts::META_CFG_MASK,
                w: f.recovered,
                ..VideoHeader::default()
            },
            last_repair: None,
        };
        out.reserve(ku + m as usize);
        for i in 0..k {
            out.push(frame.carrier(now, i, i + 1 == k, false, Prio::P4));
        }
        for j in 0..m {
            out.push(frame.carrier(now, k + j, false, false, Prio::P5));
        }

        self.stats.frames += 1;
        self.stats.key_frames += f.key as u64;
        self.stats.src_symbols += k as u64;
        self.stats.parity_symbols += m as u64;
        self.stats.bytes += f.au.len() as u64;
        self.bytes += frame.buf.len();
        self.frames.push_back(frame);
        self.evict(now);
        Ok(())
    }

    /// M2c 用来记初次发送时刻；M2a 不用
    pub fn on_sent(&mut self, now: Micros, fid: u16, sym: u16) {
        let _ = (now, fid, sym);
    }

    /// M2c：NACK 报丢的 (fid, sym)。M2a 的客户端不发 NACK，这里什么都不做
    pub fn on_lost(&mut self, now: Micros, lost: &[(u16, u16)], out: &mut Vec<VideoCarrier>) {
        let _ = (now, lost, out);
    }

    /// M2a：缓存里没有这一帧回 Gone；有就补发全部源符号（X = 1，优先级 3），
    /// 同一帧在 max(RTT_p50, 10 ms) 之内只补一次。M2c 换成按缺口补
    pub fn on_need(&mut self, now: Micros, n: &Need, out: &mut Vec<VideoCarrier>) -> NeedReply {
        self.evict(now);
        let window = REPAIR_IDEMPOTENT_MIN.max(self.params.rtt_p50_us as Micros);
        let Some(fr) = self.frames.iter_mut().rev().find(|f| f.base.fid == n.fid) else {
            return NeedReply::Gone;
        };
        if fr.last_repair.is_some_and(|t| now < t + window) {
            return NeedReply::Repairing;
        }
        fr.last_repair = Some(now);
        for i in 0..fr.k {
            out.push(fr.carrier(now, i, false, true, Prio::P3));
        }
        self.stats.repair_symbols += fr.k as u64;
        NeedReply::Repairing
    }

    /// 和 GwEvent::Report 同一个类型（ywp::wire::Report）。
    /// 按 oldest_needed_fid 提前清理缓存（规格 §8.6）；它比缓存里最新的帧还新时当作无效，不清
    pub fn on_report(&mut self, now: Micros, r: &Report<'_>) {
        self.prune_needed(now, r.video.oldest_needed_fid);
    }

    fn prune_needed(&mut self, now: Micros, oldest: u16) {
        self.evict(now);
        let Some(newest) = self.frames.back().map(|f| f.base.fid) else { return };
        if seq_lt(newest.wrapping_add(1), oldest) {
            return;
        }
        while self.frames.front().is_some_and(|f| seq_lt(f.base.fid, oldest)) {
            self.pop_front();
        }
    }

    pub fn set_params(&mut self, p: TxParams) {
        self.params = p;
    }

    pub fn set_blind(&mut self, blind: bool) {
        self.blind = blind;
    }

    /// M2a 没有计时器，这里只清理过期的缓存
    pub fn on_timer(&mut self, now: Micros, out: &mut Vec<VideoCarrier>) {
        let _ = out;
        self.evict(now);
    }

    /// M2a 没有计时器（缓存在每次调用时顺手清理）
    pub fn next_timer(&self) -> Option<Micros> {
        None
    }

    /// 停发媒体时清空。fid 的展开基准保留（fid 跨会话继续递增）
    pub fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }

    pub fn stats(&self) -> &VideoTxStats {
        &self.stats
    }

    /// 缓存里的帧数和字节数
    pub fn cached(&self) -> (usize, usize) {
        (self.frames.len(), self.bytes)
    }

    fn pop_front(&mut self) {
        if let Some(f) = self.frames.pop_front() {
            self.bytes -= f.buf.len();
        }
    }

    /// 超过 cache_age 的帧都清掉；超过 cache_bytes 时从最旧的清，最新的一帧留着
    fn evict(&mut self, now: Micros) {
        while let Some(f) = self.frames.front() {
            let expired = now.saturating_sub(f.queued) > self.cfg.cache_age;
            let over = self.bytes > self.cfg.cache_bytes && self.frames.len() > 1;
            if !(expired || over) {
                break;
            }
            self.pop_front();
        }
    }
}

/// 从每帧那块连续的 (k + R) × S 缓冲里取。
/// 源符号（sym_idx < k）返回数据部分，不含 2 B 的 len；校验符号返回 S 字节。
/// 和 `VideoChunk::body` 的含义一致：组包时源符号由 VIDEO 块的编码写 len = body.len()
impl SymbolSource for VideoTx {
    fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]> {
        self.frames.iter().rev().find(|f| f.base.fid == fid)?.symbol(sym)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fec::tests::Rng;
    use crate::fec::RsDec;
    use proptest::prelude::*;
    use ywp::time::{ms, secs};

    fn frame_in(fid: u16, key: bool, au: &[u8]) -> FrameIn<'_> {
        FrameIn { fid, key, cfg: 1, profile: 0, recovered: false, t0: 0, au }
    }

    /// 只用源符号拼回整帧
    fn reassemble(tx: &VideoTx, carriers: &[VideoCarrier]) -> Vec<u8> {
        let mut v = Vec::new();
        for c in carriers.iter().filter(|c| !c.hdr.parity) {
            v.extend_from_slice(tx.symbol(c.hdr.fid, c.hdr.sym_idx).unwrap());
        }
        v
    }

    #[test]
    fn headers_and_priorities() {
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let au: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        let f = FrameIn { fid: 7, key: false, cfg: 6, profile: 0, recovered: true, t0: 1_000, au: &au };
        tx.send_frame(1_000 + 2_345, &f, &mut out).unwrap();
        // k = 3，k ≤ 4 的 P 帧 m = 2
        assert_eq!(out.len(), 5);
        for (i, c) in out.iter().enumerate() {
            let h = c.hdr;
            assert_eq!((h.fid, h.k, h.sym_idx), (7, 3, i as u16));
            assert_eq!(h.parity, i >= 3);
            assert_eq!(h.last_src, i == 2);
            assert!(!h.repair && !h.key && h.w);
            assert_eq!(h.r, if i >= 3 { 2 } else { 0 });
            assert_eq!((h.cfg, h.n, h.profile), (2, 1, 0));
            assert_eq!(h.age_100us, 23);
            assert_eq!(c.prio, if i >= 3 { Prio::P5 } else { Prio::P4 });
        }
        assert!(out.windows(2).all(|w| w[0].order < w[1].order));
        // 源符号均分：1000 B 一个；校验 S = 1002
        assert_eq!(tx.symbol(7, 0).unwrap().len(), 1000);
        assert_eq!(tx.symbol(7, 3).unwrap().len(), 1002);
        assert!(tx.symbol(7, 5).is_none());
        assert!(tx.symbol(8, 0).is_none());
        assert_eq!(reassemble(&tx, &out), au);
        let st = tx.stats();
        assert_eq!((st.frames, st.src_symbols, st.parity_symbols, st.bytes), (1, 3, 2, 3000));
    }

    #[test]
    fn key_frame_redundancy_and_errors() {
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let au = vec![0x5a; 26 * 1186];
        let mut out = Vec::new();
        tx.send_frame(0, &frame_in(1, true, &au), &mut out).unwrap();
        // k = 26：关键帧 m = max(2, ⌈2.6⌉) = 3
        assert_eq!(out.iter().filter(|c| c.hdr.parity).count(), 3);
        assert!(out.iter().all(|c| c.hdr.key));
        // fid 没递增
        assert!(matches!(tx.send_frame(0, &frame_in(1, false, &au), &mut out), Err(TxError::StaleFid { fid: 1 })));
        assert!(matches!(tx.send_frame(0, &frame_in(0, false, &au), &mut out), Err(TxError::StaleFid { .. })));
        // 太大、空帧
        let big = vec![0u8; 1024 * 1186 + 1];
        assert!(matches!(tx.send_frame(0, &frame_in(2, false, &big), &mut out), Err(TxError::FrameTooLarge { .. })));
        assert!(matches!(tx.send_frame(0, &frame_in(2, false, &[]), &mut out), Err(TxError::Fec(FecError::Empty))));
        // fid 跨回绕
        let mut tx = VideoTx::new(VideoTxConfig::default());
        tx.send_frame(0, &frame_in(0xFFFF, false, &au[..10]), &mut out).unwrap();
        tx.send_frame(0, &frame_in(0, false, &au[..10]), &mut out).unwrap();
        assert_eq!(tx.cached().0, 2);
    }

    #[test]
    fn cache_age_and_bytes() {
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let au = vec![1u8; 100_000];
        let mut out = Vec::new();
        for i in 0..10u16 {
            tx.send_frame(ms(100) * i as u64, &frame_in(i, false, &au), &mut out).unwrap();
        }
        assert_eq!(tx.cached().0, 10);
        // fid 0 在 t = 0 进缓存，t = 1 s 时还在，过了 1 s 就清
        let need = Need { fid: 0, need: 1, key: false, tries: 0 };
        out.clear();
        assert_eq!(tx.on_need(secs(1) - ms(20), &need, &mut out), NeedReply::Repairing);
        assert_eq!(out.len(), 85);
        assert!(out.iter().all(|c| c.hdr.repair && !c.hdr.parity && !c.hdr.last_src && c.prio == Prio::P3));
        // 幂等窗口（10 ms）内不再补
        out.clear();
        assert_eq!(tx.on_need(secs(1) - ms(15), &need, &mut out), NeedReply::Repairing);
        assert!(out.is_empty());
        tx.on_timer(secs(1), &mut out);
        assert!(tx.symbol(0, 0).is_some());
        tx.on_timer(secs(1) + 1, &mut out);
        assert!(tx.symbol(0, 0).is_none());
        assert_eq!(tx.on_need(secs(1) + 1, &need, &mut out), NeedReply::Gone);
        assert_eq!(tx.cached().0, 9);

        // 8 MB：每帧约 1.03 MB，最多留 7 帧，最新的一帧总在
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let au = vec![2u8; 1_000_000];
        for i in 0..20u16 {
            tx.send_frame(i as u64, &frame_in(i, false, &au), &mut out).unwrap();
            let (n, bytes) = tx.cached();
            assert!(bytes <= ywp::time::SEND_CACHE_BYTES && n >= 1);
            assert!(tx.symbol(i, 0).is_some());
        }
        assert_eq!(tx.cached().0, 7);
        tx.clear();
        assert_eq!(tx.cached(), (0, 0));
    }

    #[test]
    fn parity_symbols_recover_lost_sources() {
        let mut rng = Rng(0x7E57);
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let mut dec = RsDec::new();
        let mut out = Vec::new();
        for fid in 0..40u16 {
            let len = 1 + rng.below(60_000);
            let au = rng.bytes(len);
            out.clear();
            tx.send_frame(fid as u64, &frame_in(fid, fid % 10 == 0, &au), &mut out).unwrap();
            let (k, r) = (out[0].hdr.k as usize, out.last().unwrap().hdr.r as usize);
            let s = tx.symbol(fid, k as u16).unwrap().len();
            // 丢掉前 r 个源符号，用校验补
            let lost = r.min(k);
            let mut shards: Vec<(u16, Vec<u8>)> = Vec::new();
            for c in &out {
                let i = c.hdr.sym_idx;
                if (i as usize) < lost {
                    continue;
                }
                let body = tx.symbol(fid, i).unwrap();
                let shard = if c.hdr.parity {
                    body.to_vec()
                } else {
                    let mut v = vec![0u8; s];
                    v[..2].copy_from_slice(&(body.len() as u16).to_be_bytes());
                    v[2..2 + body.len()].copy_from_slice(body);
                    v
                };
                shards.push((i, shard));
            }
            let mut rec = vec![0u8; k * s];
            dec.recover(k, r, s, shards.iter().map(|(i, v)| (*i, v.as_slice())), &mut rec).unwrap();
            let mut got = Vec::new();
            fec::unpack_source(&rec, k, s, &mut got).unwrap();
            assert_eq!(got, au, "fid {fid}");
        }
    }

    #[test]
    fn report_prunes_cache() {
        let mut tx = VideoTx::new(VideoTxConfig::default());
        let mut out = Vec::new();
        for i in 100..110u16 {
            tx.send_frame(0, &frame_in(i, false, &[1, 2, 3]), &mut out).unwrap();
        }
        // Report 只能由 ywp 解析出来，这里直接测清理逻辑
        tx.prune_needed(0, 105);
        assert_eq!(tx.cached().0, 5);
        assert!(tx.symbol(104, 0).is_none() && tx.symbol(105, 0).is_some());
        // 比最新帧还新的 oldest_needed_fid 当作无效
        tx.prune_needed(0, 200);
        assert_eq!(tx.cached().0, 5);
        tx.prune_needed(0, 110);
        assert_eq!(tx.cached().0, 0);
        // 还没实现 REPORT 的客户端填 0：fid 在 4 万附近时不能把缓存清空
        let mut tx = VideoTx::new(VideoTxConfig::default());
        for i in 40_000..40_010u16 {
            tx.send_frame(0, &frame_in(i, false, &[1]), &mut out).unwrap();
        }
        tx.prune_needed(0, 0);
        assert_eq!(tx.cached().0, 10);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// 切片：随机长度 1 B~1.2 MB 的帧，切完拼回逐字节一致
        #[test]
        fn slicing_roundtrip(len in 1usize..=1_200_000, seed in any::<u64>()) {
            let mut rng = Rng(seed | 1);
            let au = rng.bytes(len);
            let mut tx = VideoTx::new(VideoTxConfig::default());
            let mut out = Vec::new();
            tx.send_frame(0, &frame_in(9, false, &au), &mut out).unwrap();
            let k = out[0].hdr.k as usize;
            prop_assert_eq!(k, len.div_ceil(ywp::consts::MAX_SYMBOL_DATA));
            let src: Vec<_> = out.iter().filter(|c| !c.hdr.parity).collect();
            prop_assert_eq!(src.len(), k);
            // 均分：各源符号长度相差不超过 d，且都 ≤ 1186
            for c in &src {
                prop_assert!(tx.symbol(9, c.hdr.sym_idx).unwrap().len() <= ywp::consts::MAX_SYMBOL_DATA);
            }
            prop_assert_eq!(reassemble(&tx, &out), au);
        }
    }
}
