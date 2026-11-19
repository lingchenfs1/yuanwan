//! 客户端侧组帧（Assembler）。
//!
//! M2a 规则（规格 §7.8、§7.9 的基础版）：
//! - **送解码**：严格按 fid 顺序送；一帧收完整（≥ k 个不同的符号，必要时 RS 恢复）就送。
//! - **判 Lost**：最老的未完成帧 f 不完整，而它后面已经有完整的帧并超过 50 ms；或者 f 自己挂了 1000 ms
//!   （整帧都丢了时从它后面那一帧的第一个符号算起）。判 Lost 后发 IDR_REQUEST，冻结到下一个完整的关键帧；
//!   收到 KEY_PENDING 也冻结（不另发 IDR_REQUEST，网关已经在请求）；收到 GONE、ABORT_LAN 的帧轮到它时直接判 Lost。
//!   等关键帧时 1.2 s 没等到就再发一次 IDR_REQUEST。
//! - **完整的关键帧可以解除阻塞**：f 不完整、后面的 g 是完整的关键帧时，丢掉 f..g−1，从 g 开始解码（规格 §7.9）。
//! - **参数集**：每个关键帧前面补上 VIDEO_CONFIG 里的参数集。cfg_ver 变了之后从下一个关键帧重新开始；
//!   `DecodeItem::cfg` 填完整的 cfg_ver，解码线程看到它变了就 flush 并重开解码器。
//! - **cfg 对不上的帧先压着**：帧的 cfg（VIDEO_CONFIG 版本号的低 2 位）和当前配置对不上时不送解码，
//!   等新的 VIDEO_CONFIG 到了再送，最多压 1000 ms，超时按 Lost 处理（IDR_REQUEST 原因 CFG_TIMEOUT）。
//! - **W 帧**：照常送解码，计数，并单独统计它之后 1 s 内的解码报错。
//! - **解码出错**：发 IDR_REQUEST，冻结到下一个关键帧（规格 §7.9 最后一条）。
//!
//! M2c 换成截止期、NEED 和 R_wait。

use crate::fec::{self, RsDec};
use crate::nal::{self, Codec};
use crate::playout::Policy;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use ywp::consts::{idr_reason, FEC_KR_MAX, FEC_K_MAX, FEC_R_MAX, MAX_SYMBOL_DATA};
use ywp::endpoint::RxMeta;
use ywp::time::{
    Micros, Unwrap16, ASM_CFG_HOLD_MAX, ASM_FRAME_HANG, ASM_LOST_BEHIND_COMPLETE, ASM_W_ERROR_WINDOW, KEYFRAME_WAIT_RESEND,
};
use ywp::wire::{FrameStatusEntry, FrameStatusKind, Need, VideoChunk};
use ywp::{DropCounters, DropReason};

/// REPORT 偏移 2~19 的视频部分。定义在 ywp（ClientConn::set_video_report 要用，ywp 不能依赖本 crate），这里原样转出
pub use ywp::wire::ReportVideoFields;

/// 同时在组的帧数上限（1000 ms 的压帧窗口在 120 fps 下也只有 120 帧）
const MAX_FRAMES: usize = 256;

/// 交给解码线程的一帧
#[derive(Clone, Debug)]
pub struct DecodeItem {
    /// 展开后的 fid
    pub fid: u64,
    /// 关键帧已在前面补上参数集
    pub au: Arc<[u8]>,
    pub key: bool,
    /// 送解码时所用 VIDEO_CONFIG 的完整 cfg_ver（低 2 位就是帧头里的 cfg）。变了就 flush 并重开解码器
    pub cfg: u8,
    /// 呈现时刻 P(f)。M2a 到达即呈现，取送解码的时刻
    pub p_us: Micros,
    /// 收完整的时刻
    pub complete_us: Micros,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FreezeWhy {
    Lost,
    KeyPending,
    CfgTimeout,
    DecoderError,
}

/// 统计事件（客户端浮层和 JSONL 用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatEvent {
    FecRecovered { fid: u64 },
    WFrame { fid: u64 },
    Lost { fid: u64 },
    Late { fid: u64 },
}

#[derive(Clone, Debug)]
pub enum AsmEvent {
    Decode(DecodeItem),
    Need(Need),
    /// reason 见 ywp::consts::idr_reason
    IdrRequest(u8),
    Freeze(FreezeWhy),
    Unfreeze,
    Stat(StatEvent),
}

/// Assembler 的累计统计
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AsmStats {
    /// 送进解码器的帧
    pub frames_ok: u64,
    /// 判 Lost、或等关键帧时被丢掉的帧（含整帧没收到的 fid）
    pub frames_lost: u64,
    /// 靠 RS 校验恢复出来的帧
    pub fec_recovered: u64,
    /// W = 1 的帧
    pub w_frames: u64,
    /// W 帧送出后 1 s 内的解码报错
    pub w_errors: u64,
    pub decoder_errors: u64,
    /// cfg 对不上压满 1000 ms 的次数
    pub cfg_timeouts: u64,
    pub idr_requests: u64,
    pub freezes: u64,
}

/// 正在组的一帧
#[derive(Debug)]
struct RxFrame {
    k: u16,
    /// 第一个校验包里的 R 和 S
    r: u16,
    s: usize,
    key: bool,
    cfg: u8,
    w: bool,
    first_us: Micros,
    /// 源符号的数据（不含 len），按 sym_idx
    src: Vec<Option<Vec<u8>>>,
    n_src: u16,
    parity: Vec<(u16, Vec<u8>)>,
    /// 收完整之后的整帧
    au: Option<Arc<[u8]>>,
    complete_us: Micros,
    fec_used: bool,
    /// RS 恢复失败或数据不一致：只能判 Lost
    failed: bool,
}

impl RxFrame {
    fn complete(&self) -> bool {
        self.au.is_some()
    }

    /// 还缺几个符号（REPORT 的 blocked_need）
    fn missing(&self) -> usize {
        (self.k as usize).saturating_sub(self.n_src as usize + self.parity.len())
    }
}

#[derive(Debug)]
pub struct Assembler {
    pol: Policy,
    codec: Codec,
    fids: Unwrap16,
    frames: BTreeMap<u64, RxFrame>,
    /// 收到 GONE / ABORT_LAN 的 fid
    gone: BTreeSet<u64>,
    /// 小于它的 fid 都已交付或放弃
    floor: u64,
    /// 在等关键帧（开始时、判 Lost 后、KEY_PENDING、解码出错、换配置）
    need_key: bool,
    frozen: bool,
    delivered_any: bool,
    /// 开始等关键帧的时刻，或上一次因为等不到而重发 IDR_REQUEST 的时刻
    key_wait_since: Option<Micros>,
    /// 当前的 VIDEO_CONFIG：(cfg_ver, 参数集)
    cfg: Option<(u8, Arc<[u8]>)>,
    last_key: Option<u64>,
    w_until: Option<Micros>,
    rs: RsDec,
    drops: DropCounters,
    stats: AsmStats,
}

impl Assembler {
    pub fn new(pol: Policy) -> Self {
        Self {
            pol,
            codec: Codec::Hevc,
            fids: Unwrap16::new(),
            frames: BTreeMap::new(),
            gone: BTreeSet::new(),
            floor: 0,
            need_key: true,
            frozen: false,
            delivered_any: false,
            key_wait_since: None,
            cfg: None,
            last_key: None,
            w_until: None,
            rs: RsDec::new(),
            drops: DropCounters::new(),
            stats: AsmStats::default(),
        }
    }

    /// 编码格式（默认 HEVC；on_config 也会按参数集自己判断）。只用来在 K 位没置时按 NAL 再判一次关键帧
    pub fn set_codec(&mut self, c: Codec) {
        self.codec = c;
    }

    pub fn stats(&self) -> &AsmStats {
        &self.stats
    }

    /// 被丢弃的块（VIDEO 头不合法、重复、过期等），按原因计数
    pub fn drops(&self) -> &DropCounters {
        &self.drops
    }

    pub fn on_video(&mut self, now: Micros, meta: &RxMeta, c: &VideoChunk, ev: &mut Vec<AsmEvent>) {
        let _ = meta;
        self.touch(now);
        if let Err(r) = self.add_symbol(now, c) {
            self.drops.bump(r);
        }
        self.step(now, ev);
    }

    pub fn on_frame_status(&mut self, now: Micros, e: &FrameStatusEntry, ev: &mut Vec<AsmEvent>) {
        self.touch(now);
        let fid = self.fids.unwrap(e.fid);
        match e.kind {
            FrameStatusKind::KeyPending => {
                // fid 是网关下一个要分配的 fid：已经交付过不早于它的关键帧，说明这条是迟到的副本
                if !self.last_key.is_some_and(|k| k >= fid) {
                    self.enter_key_wait(now, FreezeWhy::KeyPending, None, ev);
                }
            }
            FrameStatusKind::Gone | FrameStatusKind::AbortLan => {
                if fid >= self.floor {
                    self.gone.insert(fid);
                }
            }
            FrameStatusKind::RefOk | FrameStatusKind::CatchUp => {}
        }
        self.step(now, ev);
    }

    /// 放行 cfg 对上了的压着的帧
    pub fn on_config(&mut self, now: Micros, cfg_ver: u8, params: &[u8], ev: &mut Vec<AsmEvent>) {
        self.touch(now);
        if let Some(c) = nal::guess_codec(params) {
            self.codec = c;
        }
        let changed = self.cfg.as_ref().is_some_and(|(v, _)| *v != cfg_ver);
        self.cfg = Some((cfg_ver, params.into()));
        if changed && !self.need_key {
            // 换了配置，解码器要重开：从下一个关键帧开始。网关重建会话后先发的就是关键帧，不另请求
            self.need_key = true;
            self.key_wait_since = Some(now);
        }
        self.step(now, ev);
    }

    pub fn on_decoder_error(&mut self, now: Micros, fid: u64, ev: &mut Vec<AsmEvent>) {
        self.touch(now);
        self.stats.decoder_errors += 1;
        if self.w_until.is_some_and(|t| now <= t) {
            self.stats.w_errors += 1;
        }
        // 上一个关键帧之前的帧报的错：已经重新开始了，不再理会
        let stale = self.last_key.is_some_and(|k| fid < k);
        if !stale && !self.need_key {
            self.enter_key_wait(now, FreezeWhy::DecoderError, Some(idr_reason::DECODER_ERROR), ev);
        }
        self.step(now, ev);
    }

    pub fn on_timer(&mut self, now: Micros, ev: &mut Vec<AsmEvent>) {
        self.step(now, ev);
    }

    pub fn next_timer(&self) -> Option<Micros> {
        let mut t: Option<Micros> = None;
        let mut at = |v: Micros| t = Some(t.map_or(v, |x| x.min(v)));
        if self.need_key {
            if let Some(w) = self.key_wait_since {
                at(w + KEYFRAME_WAIT_RESEND);
            }
            if let Some((_, f)) = self.key_candidate() {
                if !self.cfg_matches(f) {
                    at(f.complete_us + ASM_CFG_HOLD_MAX);
                }
            }
            // 等关键帧时，挂了 1000 ms 的帧清掉（完整的关键帧不在此列，它们由上面的压帧计时器管）
            if let Some(first) = self.frames.values().filter(|f| Self::prunable(f)).map(|f| f.first_us).min() {
                at(first + ASM_FRAME_HANG);
            }
        } else {
            match self.frames.get(&self.floor) {
                Some(f) if f.complete() => {
                    if !self.cfg_matches(f) {
                        at(f.complete_us + ASM_CFG_HOLD_MAX);
                    }
                }
                _ => {
                    let (hang, behind) = self.block_times();
                    if let Some(h) = hang {
                        at(h + ASM_FRAME_HANG);
                    }
                    if let Some(b) = behind {
                        at(b + ASM_LOST_BEHIND_COMPLETE);
                    }
                }
            }
        }
        t
    }

    pub fn report(&self) -> ReportVideoFields {
        let (blocked_fid, blocked_need) = match self.blocked() {
            Some(n) => (self.floor as u16, n),
            None => (ywp::consts::BLOCKED_FID_NONE, 0),
        };
        ReportVideoFields {
            mode: self.pol.mode.to_wire(),
            sb90_ms: 0,
            j_ms: (self.pol.j_us().saturating_add(500) / 1000).min(255) as u8,
            frames_ok: self.stats.frames_ok as u16,
            frames_late: 0,
            frames_repair: 0,
            frames_lost: self.stats.frames_lost as u16,
            fec_recovered: self.stats.fec_recovered as u16,
            oldest_needed_fid: self.floor as u16,
            blocked_fid,
            blocked_need,
        }
    }

    // ---------------- 内部 ----------------

    /// 第一次有动静时开始计「等关键帧」
    fn touch(&mut self, now: Micros) {
        if self.need_key && self.key_wait_since.is_none() {
            self.key_wait_since = Some(now);
        }
    }

    fn cfg_matches(&self, f: &RxFrame) -> bool {
        self.cfg.as_ref().is_some_and(|(v, _)| v & ywp::consts::META_CFG_MASK == f.cfg)
    }

    /// 等关键帧时可以清掉的帧：不完整的、或者完整但不是关键帧
    fn prunable(f: &RxFrame) -> bool {
        !(f.complete() && f.key)
    }

    /// 等关键帧时的候选：floor 之后第一个完整的关键帧
    fn key_candidate(&self) -> Option<(u64, &RxFrame)> {
        self.frames.range(self.floor..).find(|(_, f)| f.complete() && f.key).map(|(k, f)| (*k, f))
    }

    /// floor 那一帧不完整时：(挂起起点, 后面最早完整的时刻)。整帧没收到时挂起起点取后面最早的第一个符号
    fn block_times(&self) -> (Option<Micros>, Option<Micros>) {
        let me = self.frames.get(&self.floor).map(|f| f.first_us);
        let later = self.frames.range(self.floor + 1..);
        let later_first = later.clone().map(|(_, f)| f.first_us).min();
        let behind = later.filter(|(_, f)| f.complete()).map(|(_, f)| f.complete_us).min();
        (me.or(later_first), behind)
    }

    /// 阻塞时还缺几个符号（0xFF 表示整帧都没收到、不知道 k）
    fn blocked(&self) -> Option<u8> {
        if self.need_key {
            return None;
        }
        let later_complete = self.frames.range(self.floor + 1..).any(|(_, f)| f.complete());
        match self.frames.get(&self.floor) {
            Some(f) if f.complete() => None,
            Some(f) if later_complete => Some(f.missing().clamp(1, 254) as u8),
            None if later_complete => Some(ywp::consts::NEED_UNKNOWN_K),
            _ => None,
        }
    }

    fn add_symbol(&mut self, now: Micros, c: &VideoChunk) -> Result<(), DropReason> {
        let h = &c.hdr;
        if h.k == 0 || h.k > FEC_K_MAX {
            return Err(DropReason::VideoBadFec);
        }
        if h.parity != (h.sym_idx >= h.k) {
            return Err(DropReason::VideoParityMismatch);
        }
        if h.parity {
            if h.r == 0 || h.r > FEC_R_MAX || h.k + h.r > FEC_KR_MAX || h.sym_idx >= h.k + h.r {
                return Err(DropReason::VideoBadFec);
            }
            if c.body.is_empty() || !c.body.len().is_multiple_of(2) {
                return Err(DropReason::VideoOddShard);
            }
        } else if c.body.len() > MAX_SYMBOL_DATA {
            return Err(DropReason::VideoBadLen);
        }
        let fid = self.fids.unwrap(h.fid);
        if fid < self.floor {
            // 源符号齐了、帧已经交付之后才到的校验包很正常，不计数
            return if h.parity { Ok(()) } else { Err(DropReason::VideoStaleFid) };
        }
        if !self.frames.contains_key(&fid) && self.frames.len() >= MAX_FRAMES {
            return Err(DropReason::VideoTooManyFrames);
        }
        let f = self.frames.entry(fid).or_insert_with(|| RxFrame {
            k: h.k,
            r: 0,
            s: 0,
            key: false,
            cfg: h.cfg & ywp::consts::META_CFG_MASK,
            w: false,
            first_us: now,
            src: vec![None; h.k as usize],
            n_src: 0,
            parity: Vec::new(),
            au: None,
            complete_us: 0,
            fec_used: false,
            failed: false,
        });
        if f.complete() || f.failed {
            // 帧已经齐了（多出来的校验包很正常），或者已经坏了
            return Ok(());
        }
        if h.k != f.k {
            return Err(DropReason::VideoKMismatch);
        }
        f.key |= h.key;
        f.w |= h.w;
        if h.parity {
            if f.parity.is_empty() {
                f.r = h.r;
                f.s = c.body.len();
            } else if c.body.len() != f.s {
                return Err(DropReason::VideoShardMismatch);
            } else if h.r != f.r {
                return Err(DropReason::VideoBadFec);
            }
            if f.parity.iter().any(|(i, _)| *i == h.sym_idx) {
                return Err(DropReason::VideoDuplicate);
            }
            f.parity.push((h.sym_idx, c.body.to_vec()));
        } else {
            let slot = &mut f.src[h.sym_idx as usize];
            if slot.is_some() {
                return Err(DropReason::VideoDuplicate);
            }
            *slot = Some(c.body.to_vec());
            f.n_src += 1;
        }
        let res = Self::try_complete(f, &mut self.rs, now, self.codec);
        if res.is_err() {
            f.failed = true;
        }
        res
    }

    /// 源符号齐了直接拼；不齐但够 k 个就 RS 恢复
    fn try_complete(f: &mut RxFrame, rs: &mut RsDec, now: Micros, codec: Codec) -> Result<(), DropReason> {
        let k = f.k as usize;
        let au: Vec<u8> = if f.n_src as usize == k {
            f.src.iter().flatten().flat_map(|v| v.iter().copied()).collect()
        } else if !f.parity.is_empty() && f.n_src as usize + f.parity.len() >= k {
            let s = f.s;
            if f.src.iter().flatten().any(|d| d.len() + 2 > s) {
                return Err(DropReason::VideoBadLen);
            }
            let shards: Vec<(u16, Vec<u8>)> = f
                .src
                .iter()
                .enumerate()
                .filter_map(|(i, d)| d.as_ref().map(|d| (i as u16, d)))
                .map(|(i, d)| {
                    let mut v = vec![0u8; s];
                    v[..2].copy_from_slice(&(d.len() as u16).to_be_bytes());
                    v[2..2 + d.len()].copy_from_slice(d);
                    (i, v)
                })
                .collect();
            let have = shards.iter().map(|(i, v)| (*i, v.as_slice())).chain(f.parity.iter().map(|(i, v)| (*i, v.as_slice())));
            let mut out = vec![0u8; k * s];
            rs.recover(k, f.r as usize, s, have, &mut out).map_err(|_| DropReason::FecDecodeFail)?;
            let mut au = Vec::new();
            fec::unpack_source(&out, k, s, &mut au).map_err(|_| DropReason::FecDecodeFail)?;
            f.fec_used = true;
            au
        } else {
            return Ok(());
        };
        if !f.key {
            f.key = nal::is_keyframe(&au, codec);
        }
        f.au = Some(au.into());
        f.complete_us = now;
        f.src = Vec::new();
        f.parity = Vec::new();
        Ok(())
    }

    /// 把能做的都做完：送解码、判 Lost、压帧超时、重发 IDR_REQUEST、清理
    fn step(&mut self, now: Micros, ev: &mut Vec<AsmEvent>) {
        loop {
            if self.need_key {
                if self.key_wait_since.is_some_and(|t| now >= t + KEYFRAME_WAIT_RESEND) {
                    self.request_idr(now, idr_reason::KEY_WAIT_TIMEOUT, ev);
                }
                if let Some((g, f)) = self.key_candidate() {
                    if self.cfg_matches(f) {
                        self.discard_below(g, false, ev);
                        self.deliver(now, g, ev);
                        continue;
                    }
                    if now >= f.complete_us + ASM_CFG_HOLD_MAX {
                        // 新配置一直没来：丢掉这个关键帧，再请求一次
                        self.stats.cfg_timeouts += 1;
                        ev.push(AsmEvent::Stat(StatEvent::Lost { fid: g }));
                        self.discard_below(g + 1, false, ev);
                        self.request_idr(now, idr_reason::CFG_TIMEOUT, ev);
                        continue;
                    }
                }
                // 等关键帧时，挂了 1000 ms 的帧（不完整的、P 帧）清掉
                let stale: Vec<u64> =
                    self.frames.iter().filter(|(_, f)| now >= f.first_us + ASM_FRAME_HANG && Self::prunable(f)).map(|(k, _)| *k).collect();
                for fid in stale {
                    self.frames.remove(&fid);
                    self.stats.frames_lost += 1;
                }
                return;
            }

            let floor = self.floor;
            let cur = self.frames.get(&floor);
            if let Some(f) = cur.filter(|f| f.complete()) {
                if self.cfg_matches(f) {
                    self.deliver(now, floor, ev);
                    continue;
                }
                if now >= f.complete_us + ASM_CFG_HOLD_MAX {
                    self.stats.cfg_timeouts += 1;
                    self.lose_floor(now, FreezeWhy::CfgTimeout, idr_reason::CFG_TIMEOUT, ev);
                    continue;
                }
                return;
            }
            // floor 不完整（或整帧没收到）：后面有完整的关键帧就从它开始
            let unblock = self.frames.range(floor + 1..).find(|(_, f)| f.complete() && f.key && self.cfg_matches(f)).map(|(k, _)| *k);
            if let Some(g) = unblock {
                self.discard_below(g, true, ev);
                self.deliver(now, g, ev);
                continue;
            }
            let (hang, behind) = self.block_times();
            let lost = self.gone.contains(&floor)
                || hang.is_some_and(|t| now >= t + ASM_FRAME_HANG)
                || behind.is_some_and(|t| now >= t + ASM_LOST_BEHIND_COMPLETE);
            if lost {
                self.lose_floor(now, FreezeWhy::Lost, idr_reason::FRAME_LOST, ev);
                continue;
            }
            return;
        }
    }

    /// floor 那一帧判 Lost，进入等关键帧
    fn lose_floor(&mut self, now: Micros, why: FreezeWhy, reason: u8, ev: &mut Vec<AsmEvent>) {
        let fid = self.floor;
        self.discard_below(fid + 1, true, ev);
        self.enter_key_wait(now, why, Some(reason), ev);
    }

    /// 丢掉 fid < upto 的帧，floor 推到 upto。count 为真时每个 fid 记一次 Lost
    fn discard_below(&mut self, upto: u64, count: bool, ev: &mut Vec<AsmEvent>) {
        if upto <= self.floor {
            return;
        }
        if count {
            for fid in self.floor..upto {
                self.stats.frames_lost += 1;
                ev.push(AsmEvent::Stat(StatEvent::Lost { fid }));
            }
        } else {
            self.stats.frames_lost += self.frames.range(..upto).count() as u64;
        }
        self.frames = self.frames.split_off(&upto);
        self.gone = self.gone.split_off(&upto);
        self.floor = upto;
    }

    /// 进入等关键帧。reason 为 None 时不发 IDR_REQUEST（KEY_PENDING：网关已经在请求）
    fn enter_key_wait(&mut self, now: Micros, why: FreezeWhy, reason: Option<u8>, ev: &mut Vec<AsmEvent>) {
        if !self.need_key {
            self.need_key = true;
            self.key_wait_since = Some(now);
            if let Some(r) = reason {
                self.request_idr(now, r, ev);
            }
        } else if reason.is_none() {
            // KEY_PENDING：从现在起重新计 1.2 s
            self.key_wait_since = Some(now);
        }
        if self.delivered_any && !self.frozen {
            self.frozen = true;
            self.stats.freezes += 1;
            ev.push(AsmEvent::Freeze(why));
        }
    }

    fn request_idr(&mut self, now: Micros, reason: u8, ev: &mut Vec<AsmEvent>) {
        self.key_wait_since = Some(now);
        self.stats.idr_requests += 1;
        ev.push(AsmEvent::IdrRequest(reason));
    }

    /// 送解码。调用前已确认这一帧完整、cfg 对得上
    fn deliver(&mut self, now: Micros, fid: u64, ev: &mut Vec<AsmEvent>) {
        let Some((ver, params)) = self.cfg.as_ref().map(|(v, p)| (*v, p.clone())) else { return };
        let Some(f) = self.frames.remove(&fid) else { return };
        let Some(au) = f.au else { return };
        let au: Arc<[u8]> = if f.key && !params.is_empty() && !nal::starts_with_param_set(&au, self.codec) {
            let mut v = Vec::with_capacity(params.len() + au.len());
            v.extend_from_slice(&params);
            v.extend_from_slice(&au);
            v.into()
        } else {
            au
        };
        if f.key {
            self.last_key = Some(fid);
            self.need_key = false;
            self.key_wait_since = None;
            if self.frozen {
                self.frozen = false;
                ev.push(AsmEvent::Unfreeze);
            }
        }
        self.floor = fid + 1;
        self.gone = self.gone.split_off(&self.floor);
        self.delivered_any = true;
        self.stats.frames_ok += 1;
        if f.fec_used {
            self.stats.fec_recovered += 1;
            ev.push(AsmEvent::Stat(StatEvent::FecRecovered { fid }));
        }
        if f.w {
            self.stats.w_frames += 1;
            self.w_until = Some(now + ASM_W_ERROR_WINDOW);
            ev.push(AsmEvent::Stat(StatEvent::WFrame { fid }));
        }
        ev.push(AsmEvent::Decode(DecodeItem { fid, au, key: f.key, cfg: ver, p_us: now, complete_us: f.complete_us }));
    }
}

#[cfg(test)]
mod tests;
