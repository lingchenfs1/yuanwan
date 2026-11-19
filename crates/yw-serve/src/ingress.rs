//! Ingress（M2a）：参数集 → VIDEO_CONFIG、fid 分配、WaitKey（整帧入口，规格 §7.1 的 v0.2 注）。
//!
//! - fid 只分配给真正转发的帧，客户端看到的 fid 连续（规格 §5.5.1）；
//! - 进入 WaitKey：丢 P 帧、经 IdrGovernor 请求 IDR、发 KEY_PENDING，从下一个关键帧恢复；
//! - 有客户端时 300 ms 没有新帧：请求 IDR 并发 KEY_PENDING（规格 §7.4 第 3 条）；
//! - recovered 帧怎么处理由 RecoveredPolicy 定。T31、T41（2026-11-19）的结论：PS5 码流每帧单 slice、首 NAL 是 slice，
//!   PS5 会话库的参考核对覆盖所有帧，修补帧能正常解码，所以默认 Forward（照常转发并置 W 位）。
//!
//! 队列溢出（MediaMsg 的 seq 跳号）：视频回调这时已经向 PS5 会话库返回了 false，它不会把那一帧记作参考，
//! 引用它的 P 帧由它自己改参考（recovered）或丢弃。所以 Forward 档下跳号只计数；
//! 其余两档不信任会话库的改写，跳号也进 WaitKey(Overflow)。
//!
//! 没有客户端时所有帧都不转发（不占 fid）；新客户端接入时由调用方先 `set_client_present(true)`，再 `wait_key(NewClient)`。
//! 本帧所属会话的 VIDEO_CONFIG 还没有时照常转发：客户端会把 cfg 对不上的帧压着，等新配置（规格 §7.8）。
//! 调用方要保证配置最终送到（通知丢了就从最新值槽取，见 PsControl::params）。

use crate::idr::IdrReason;
use ywp::consts::{video_codec, FEC_K_MAX, MAX_SYMBOL_DATA};
use ywp::msg::{VideoConfig, VideoProfile};
use ywp::time::{Micros, INGRESS_NO_FRAME};
use yw_media::video_tx::FrameIn;

/// 为什么进 WaitKey
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitWhy {
    /// MediaMsg 队列溢出（seq 跳号）
    Overflow,
    /// 帧过大（k > 1024）
    TooLarge,
    /// 新客户端在串流中途接入
    NewClient,
    /// 从 Held 恢复
    HeldResume,
    /// 有客户端时 300 ms 没有新帧
    NoFrames,
    /// recovered 帧不转发（RecoveredPolicy::WaitKey），或 frames_lost > 0（RecoveredPolicy::LostUntilKey）
    Recovered,
}

/// chiaki 改写过参考的帧（recovered）怎么处理。默认 Forward（T31、T41 的结论）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveredPolicy {
    #[default]
    /// 照常转发并置 W 位
    Forward,
    /// 不转发，进 WaitKey
    WaitKey,
    /// frames_lost > 0 起到下一个关键帧前都进 WaitKey（管首 NAL 不是 slice、chiaki 没核对的帧）
    LostUntilKey,
}

/// 一帧整帧输入（来自 MediaMsg::Video）
#[derive(Clone, Copy, Debug)]
pub struct VideoIn<'a> {
    /// MediaMsg 的递增序号，跳号说明队列溢出过
    pub seq: u32,
    /// chiaki 交帧回调的时刻：M2a~M2d 的 t0（规格 §9.1），age 从这里算起
    pub rx_us: Micros,
    /// chiaki 报告的、本帧之前丢掉的帧数（LostUntilKey 用）
    pub frames_lost: i32,
    /// chiaki 改写过本帧的参考
    pub recovered: bool,
    pub data: &'a [u8],
}

/// 这一路视频的编码、帧率和分辨率（写进 VIDEO_CONFIG）。网关从建会话的参数得到；
/// 没设时 codec 从参数集的 NAL 类型判断（分不出 HDR），fps、宽、高填 0 表示未知（客户端以参数集为准）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamInfo {
    /// ywp::consts::video_codec
    pub codec: u8,
    pub fps: u8,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug)]
pub enum IngressOut<'a> {
    Frame(FrameIn<'a>),
    Config(VideoConfig),
    /// fid 填「下一个要分配的 fid」
    KeyPending { fid: u16 },
    RequestIdr(IdrReason),
}

/// 计数（给控制口和 JSONL 用）
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct IngressStats {
    pub frames_in: u64,
    pub forwarded: u64,
    pub key_forwarded: u64,
    /// 转发时置了 W 位的帧
    pub w_forwarded: u64,
    /// 本帧所属会话的 VIDEO_CONFIG 还没有就转发了（客户端会压着等配置）
    pub forwarded_before_config: u64,
    /// seq 跳号的次数和跳掉的帧数
    pub seq_gaps: u64,
    pub seq_missing: u64,
    pub dropped_no_client: u64,
    /// WaitKey 期间丢掉的 P 帧
    pub dropped_wait_key: u64,
    pub dropped_too_large: u64,
    pub dropped_empty: u64,
    /// 按 RecoveredPolicy 不转发的帧（不含 WaitKey 期间本来就要丢的）
    pub dropped_policy: u64,
    pub wait_entries: u64,
    /// 300 ms 无帧触发的次数
    pub no_frame_triggers: u64,
    pub configs: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Open,
    Waiting(WaitWhy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Avc,
    Hevc,
}

#[derive(Debug)]
pub struct Ingress {
    policy: RecoveredPolicy,
    stream: Option<StreamInfo>,
    /// 从参数集判断出的编码（没设 StreamInfo 时用）
    detected: Option<Codec>,
    epoch: Option<u32>,
    cfg_ver: u8,
    config: Option<VideoConfig>,
    next_fid: u16,
    last_seq: Option<u32>,
    gate: Gate,
    client_present: bool,
    /// 客户端接入后第一次 on_tick 的时刻：「300 ms 无帧」最早从这里算
    client_baseline: Option<Micros>,
    last_frame_us: Option<Micros>,
    pending_idr: Option<IdrReason>,
    pending_key: bool,
    stats: IngressStats,
}

impl Ingress {
    pub fn new(policy: RecoveredPolicy) -> Self {
        Self {
            policy,
            stream: None,
            detected: None,
            epoch: None,
            cfg_ver: 0,
            config: None,
            next_fid: 0,
            last_seq: None,
            gate: Gate::Open,
            client_present: false,
            client_baseline: None,
            last_frame_us: None,
            pending_idr: None,
            pending_key: false,
            stats: IngressStats::default(),
        }
    }

    /// 设置编码、帧率、分辨率。在这一路会话的 on_params 之前调，从下一次生成 VIDEO_CONFIG 起生效
    pub fn set_stream(&mut self, info: StreamInfo) {
        self.stream = Some(info);
    }

    /// cfg_ver += 1；fid 继续递增。同一个 epoch 重复调用不起作用
    pub fn on_epoch(&mut self, epoch: u32) {
        if self.epoch == Some(epoch) {
            return;
        }
        self.epoch = Some(epoch);
        self.cfg_ver = self.cfg_ver.wrapping_add(1);
        self.config = None;
        // 新会话的 seq 从哪里接着都行，不按跳号算
        self.last_seq = None;
    }

    /// 参数集（Annex-B，只含 VPS/SPS/PPS）。和当前配置相同就不再发；同一会话里变了，cfg_ver 再加 1
    pub fn on_params(&mut self, data: &[u8], out: &mut Vec<IngressOut<'_>>) {
        self.flush(out);
        if data.is_empty() {
            return;
        }
        if let Some(c) = &self.config {
            if c.profiles.first().is_some_and(|p| p.param_sets == data) {
                return;
            }
            self.cfg_ver = self.cfg_ver.wrapping_add(1);
        }
        if let Some(c) = detect_codec(data) {
            self.detected = Some(c);
        }
        let (codec, fps, width, height) = match self.stream {
            Some(s) => (s.codec, s.fps, s.width, s.height),
            None => (if self.codec() == Codec::Avc { video_codec::AVC } else { video_codec::HEVC }, 0, 0, 0),
        };
        let cfg = VideoConfig {
            cfg_ver: self.cfg_ver,
            codec,
            fps,
            profiles: vec![VideoProfile { idx: 0, width, height, param_sets: data.to_vec() }],
        };
        self.config = Some(cfg.clone());
        self.stats.configs += 1;
        out.push(IngressOut::Config(cfg));
    }

    pub fn on_frame<'a>(&mut self, now: Micros, frame: VideoIn<'a>, out: &mut Vec<IngressOut<'a>>) {
        self.flush(out);
        self.stats.frames_in += 1;
        if let Some(prev) = self.last_seq {
            let gap = frame.seq.wrapping_sub(prev).wrapping_sub(1);
            // 往回跳（不该发生）不算跳号
            if gap != 0 && gap < 0x8000_0000 {
                self.stats.seq_gaps += 1;
                self.stats.seq_missing += gap as u64;
                if self.policy != RecoveredPolicy::Forward {
                    self.enter_wait(WaitWhy::Overflow, false);
                }
            }
        }
        self.last_seq = Some(frame.seq);
        self.last_frame_us = Some(now);

        let data = frame.data;
        if data.is_empty() {
            self.stats.dropped_empty += 1;
            self.flush(out);
            return;
        }
        let codec = self.codec();
        if is_param_sets_only(data, codec) {
            // 网关的回调会把参数集写进最新值槽，不走这里；录像回放之类的来源可能直接混在帧里
            self.on_params(data, out);
            return;
        }
        let key = is_keyframe(data, codec);
        if data.len().div_ceil(MAX_SYMBOL_DATA) > FEC_K_MAX as usize {
            // 会话库以为这一帧已经交付，后面的帧可能引用它
            self.stats.dropped_too_large += 1;
            self.enter_wait(WaitWhy::TooLarge, false);
            self.flush(out);
            return;
        }
        if key {
            self.gate = Gate::Open;
        } else {
            let lost = frame.frames_lost > 0 || frame.recovered;
            let reject = match self.policy {
                RecoveredPolicy::WaitKey => frame.recovered,
                RecoveredPolicy::LostUntilKey => lost,
                _ => false,
            };
            if reject {
                if self.gate == Gate::Open {
                    self.stats.dropped_policy += 1;
                }
                self.enter_wait(WaitWhy::Recovered, false);
            }
        }
        if !self.client_present {
            self.stats.dropped_no_client += 1;
            self.flush(out);
            return;
        }
        if let Gate::Waiting(_) = self.gate {
            self.stats.dropped_wait_key += 1;
            self.flush(out);
            return;
        }
        let fid = self.next_fid;
        self.next_fid = fid.wrapping_add(1);
        self.stats.forwarded += 1;
        if key {
            self.stats.key_forwarded += 1;
        }
        if frame.recovered {
            self.stats.w_forwarded += 1;
        }
        if self.config.is_none() {
            self.stats.forwarded_before_config += 1;
        }
        out.push(IngressOut::Frame(FrameIn {
            fid,
            key,
            cfg: self.cfg_ver & 0x03,
            profile: 0,
            recovered: frame.recovered,
            t0: frame.rx_us,
            au: data,
        }));
    }

    /// 有没有活跃客户端：「300 ms 无帧」只在有客户端时生效；新客户端接入时由调用方再调 wait_key(NewClient)
    pub fn set_client_present(&mut self, present: bool) {
        if present && !self.client_present {
            self.client_baseline = None;
        }
        if !present {
            // 没人收了，未发出的 KEY_PENDING、IDR 请求也不用发了
            self.pending_idr = None;
            self.pending_key = false;
        }
        self.client_present = present;
    }

    /// 丢 P 帧、请求 IDR、发 KEY_PENDING，等关键帧。没有客户端时只记状态。
    /// 请求和 KEY_PENDING 在下一次 on_frame / on_tick / on_params 时输出
    pub fn wait_key(&mut self, why: WaitWhy) {
        self.enter_wait(why, true);
    }

    /// 有客户端时 300 ms 无帧 → IDR + KEY_PENDING（已经在等关键帧时不再触发，重发交给 IdrGovernor）
    pub fn on_tick(&mut self, now: Micros, out: &mut Vec<IngressOut<'_>>) {
        self.flush(out);
        if !self.client_present {
            return;
        }
        let baseline = *self.client_baseline.get_or_insert(now);
        if matches!(self.gate, Gate::Waiting(_)) {
            return;
        }
        let last = self.last_frame_us.unwrap_or(0).max(baseline);
        if now.saturating_sub(last) >= INGRESS_NO_FRAME {
            self.stats.no_frame_triggers += 1;
            self.stats.wait_entries += 1;
            self.gate = Gate::Waiting(WaitWhy::NoFrames);
            self.pending_idr = Some(IdrReason::NoFrames);
            self.pending_key = true;
            self.flush(out);
        }
    }

    /// 当前的 VIDEO_CONFIG（新客户端接入时先发它）
    pub fn config(&self) -> Option<&VideoConfig> {
        self.config.as_ref()
    }

    pub fn epoch(&self) -> Option<u32> {
        self.epoch
    }

    /// 下一个要分配的 fid
    pub fn next_fid(&self) -> u16 {
        self.next_fid
    }

    /// 在等关键帧时返回原因
    pub fn waiting(&self) -> Option<WaitWhy> {
        match self.gate {
            Gate::Open => None,
            Gate::Waiting(w) => Some(w),
        }
    }

    pub fn stats(&self) -> &IngressStats {
        &self.stats
    }

    fn codec(&self) -> Codec {
        match self.stream.map(|s| s.codec) {
            Some(video_codec::AVC) => Codec::Avc,
            Some(_) => Codec::Hevc,
            // PS5 默认 HEVC
            None => self.detected.unwrap_or(Codec::Hevc),
        }
    }

    /// force：外部调用（新客户端等）即使已经在等关键帧也要再发 KEY_PENDING 和请求
    fn enter_wait(&mut self, why: WaitWhy, force: bool) {
        let was_waiting = matches!(self.gate, Gate::Waiting(_));
        self.gate = Gate::Waiting(why);
        if was_waiting && !force {
            return;
        }
        self.stats.wait_entries += 1;
        if self.client_present {
            self.pending_idr = Some(IdrReason::WaitKey(why));
            self.pending_key = true;
        }
    }

    fn flush(&mut self, out: &mut Vec<IngressOut<'_>>) {
        if let Some(r) = self.pending_idr.take() {
            out.push(IngressOut::RequestIdr(r));
        }
        if std::mem::take(&mut self.pending_key) {
            out.push(IngressOut::KeyPending { fid: self.next_fid });
        }
    }
}

// ---- NAL 判断 ----
// TODO(C 组 yw_media::nal 落地后)：改用 yw_media::nal::{is_keyframe, is_param_sets_only}，删掉这一段

/// 遍历 Annex-B 里每个 NAL 头的第一个字节（起始码 00 00 01 之后；4 字节起始码同样能匹配）
fn nal_headers(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                let b = data[i + 3];
                i += 3;
                return Some(b);
            }
            i += 1;
        }
        None
    })
}

fn nal_type(b: u8, c: Codec) -> u8 {
    match c {
        Codec::Hevc => (b >> 1) & 0x3f,
        Codec::Avc => b & 0x1f,
    }
}

fn is_vcl(t: u8, c: Codec) -> bool {
    match c {
        Codec::Hevc => t <= 31,
        Codec::Avc => (1..=5).contains(&t),
    }
}

/// 以第一个 VCL NAL 为准：HEVC 是 16~21（IRAP），H.264 是 5（规格 §7.1）
fn is_keyframe(data: &[u8], c: Codec) -> bool {
    match nal_headers(data).map(|b| nal_type(b, c)).find(|&t| is_vcl(t, c)) {
        Some(t) => match c {
            Codec::Hevc => (16..=21).contains(&t),
            Codec::Avc => t == 5,
        },
        None => false,
    }
}

/// 只含参数集（HEVC 的 VPS/SPS/PPS，H.264 的 SPS/PPS），没有 slice
fn is_param_sets_only(data: &[u8], c: Codec) -> bool {
    let mut params = false;
    for t in nal_headers(data).map(|b| nal_type(b, c)) {
        if is_vcl(t, c) {
            return false;
        }
        if matches!((c, t), (Codec::Hevc, 32..=34) | (Codec::Avc, 7 | 8)) {
            params = true;
        }
    }
    params
}

/// 从参数集的第一个 NAL 判断编码
fn detect_codec(data: &[u8]) -> Option<Codec> {
    let b = nal_headers(data).next()?;
    if (32..=34).contains(&nal_type(b, Codec::Hevc)) {
        Some(Codec::Hevc)
    } else if matches!(nal_type(b, Codec::Avc), 7 | 8) {
        Some(Codec::Avc)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ywp::time::ms;

    // HEVC NAL 头第一个字节 = type << 1
    const VPS: u8 = 32 << 1;
    const SPS: u8 = 33 << 1;
    const PPS: u8 = 34 << 1;
    const IDR: u8 = 19 << 1;
    const TRAIL: u8 = 1 << 1;
    const SEI: u8 = 39 << 1;

    fn au(nals: &[u8], body: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for &h in nals {
            v.extend_from_slice(&[0, 0, 0, 1, h, 0x01]);
            v.extend(std::iter::repeat_n(0xAB, body));
        }
        v
    }

    fn params() -> Vec<u8> {
        au(&[VPS, SPS, PPS], 8)
    }

    struct Src {
        seq: u32,
        key: Vec<u8>,
        p: Vec<u8>,
    }

    impl Src {
        fn new() -> Self {
            Self { seq: 0, key: au(&[IDR], 3000), p: au(&[TRAIL], 500) }
        }
        fn next(&mut self, key: bool) -> VideoIn<'_> {
            self.seq += 1;
            VideoIn { seq: self.seq, rx_us: 0, frames_lost: 0, recovered: false, data: if key { &self.key } else { &self.p } }
        }
    }

    /// 送一帧，返回输出的简写
    fn feed(ing: &mut Ingress, now: Micros, f: VideoIn<'_>) -> Vec<String> {
        let mut out = Vec::new();
        ing.on_frame(now, f, &mut out);
        out.iter().map(describe).collect()
    }

    fn describe(o: &IngressOut<'_>) -> String {
        match o {
            IngressOut::Frame(f) => format!("F{}{}{}", f.fid, if f.key { "K" } else { "" }, if f.recovered { "W" } else { "" }),
            IngressOut::Config(c) => format!("C{}", c.cfg_ver),
            IngressOut::KeyPending { fid } => format!("KP{fid}"),
            IngressOut::RequestIdr(r) => format!("IDR:{r:?}"),
        }
    }

    fn streaming() -> (Ingress, Src) {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        ing.on_epoch(1);
        let mut out = Vec::new();
        ing.on_params(&params(), &mut out);
        ing.set_client_present(true);
        (ing, Src::new())
    }

    #[test]
    fn nal_helpers() {
        assert!(is_keyframe(&au(&[IDR], 10), Codec::Hevc));
        assert!(!is_keyframe(&au(&[TRAIL], 10), Codec::Hevc));
        // 以第一个 VCL NAL 为准，前面的 SEI、参数集不算
        assert!(is_keyframe(&au(&[SEI, VPS, SPS, PPS, IDR], 4), Codec::Hevc));
        assert!(!is_keyframe(&au(&[SEI, TRAIL, IDR], 4), Codec::Hevc));
        assert!(is_param_sets_only(&params(), Codec::Hevc));
        assert!(!is_param_sets_only(&au(&[VPS, SPS, PPS, IDR], 4), Codec::Hevc));
        assert!(!is_param_sets_only(&au(&[SEI], 4), Codec::Hevc));
        // H.264：SPS 0x67、PPS 0x68、IDR 0x65、P 0x41
        assert!(is_keyframe(&au(&[0x65], 4), Codec::Avc));
        assert!(!is_keyframe(&au(&[0x41], 4), Codec::Avc));
        assert!(is_param_sets_only(&au(&[0x67, 0x68], 4), Codec::Avc));
        assert_eq!(detect_codec(&params()), Some(Codec::Hevc));
        assert_eq!(detect_codec(&au(&[0x67, 0x68], 4)), Some(Codec::Avc));
        assert_eq!(detect_codec(&[1, 2, 3]), None);
    }

    #[test]
    fn fid_only_for_forwarded_frames_and_contiguous() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        let mut src = Src::new();
        // 没有客户端：不转发、不占 fid
        assert!(feed(&mut ing, 0, src.next(true)).is_empty());
        assert!(feed(&mut ing, 0, src.next(false)).is_empty());
        assert_eq!(ing.next_fid(), 0);
        assert_eq!(ing.stats().dropped_no_client, 2);
        ing.set_client_present(true);
        ing.wait_key(WaitWhy::NewClient);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["IDR:WaitKey(NewClient)", "KP0"]);
        assert!(feed(&mut ing, 0, src.next(false)).is_empty());
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["F1"]);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["F2"]);
        assert_eq!(ing.stats().dropped_wait_key, 2);
        assert_eq!(ing.stats().forwarded, 3);
    }

    #[test]
    fn fid_wraps_and_survives_epoch() {
        let (mut ing, mut src) = streaming();
        ing.next_fid = 0xFFFF;
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F65535K"]);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["F0"]);
        // 重建会话：fid 继续递增，不归零
        ing.on_epoch(2);
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F1K"]);
    }

    #[test]
    fn config_versions() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        ing.set_stream(StreamInfo { codec: video_codec::HEVC, fps: 60, width: 1920, height: 1080 });
        let mut out = Vec::new();
        ing.on_epoch(7);
        ing.on_params(&params(), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["C1"]);
        let IngressOut::Config(c) = &out[0] else { unreachable!() };
        assert_eq!((c.codec, c.fps, c.profiles[0].width, c.profiles[0].height), (video_codec::HEVC, 60, 1920, 1080));
        assert_eq!(c.profiles[0].param_sets, params());
        assert_eq!(ing.config(), Some(c));
        // 同一个参数集再来（通知重复、从最新值槽补取）：不重发
        out.clear();
        ing.on_params(&params(), &mut out);
        assert!(out.is_empty());
        // 同一个 epoch 重复 on_epoch：不变
        ing.on_epoch(7);
        assert_eq!(ing.config().map(|c| c.cfg_ver), Some(1));
        // 同一会话里参数集变了
        ing.on_params(&au(&[VPS, SPS, PPS], 9), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["C2"]);
        // 重建会话：参数集即使和上一个会话一样也要重发，cfg_ver 加 1
        out.clear();
        ing.on_epoch(8);
        assert!(ing.config().is_none());
        ing.on_params(&au(&[VPS, SPS, PPS], 9), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["C3"]);
        // 帧的 cfg 是 cfg_ver 的低 2 位
        ing.set_client_present(true);
        let mut src = Src::new();
        let mut out = Vec::new();
        ing.on_frame(0, src.next(true), &mut out);
        let IngressOut::Frame(f) = &out[0] else { panic!("{out:?}") };
        assert_eq!(f.cfg, 3);
        ing.on_epoch(9);
        let mut out = Vec::new();
        ing.on_frame(0, src.next(true), &mut out);
        let IngressOut::Frame(f) = &out[0] else { panic!("{out:?}") };
        assert_eq!(f.cfg, 0, "cfg_ver 4 的低 2 位");
        assert_eq!(ing.stats().forwarded_before_config, 1, "新会话的配置还没到也照常转发");
    }

    #[test]
    fn codec_detected_from_params_without_stream_info() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        let mut out = Vec::new();
        ing.on_params(&au(&[0x67, 0x68], 4), &mut out);
        let IngressOut::Config(c) = &out[0] else { unreachable!() };
        assert_eq!((c.codec, c.fps, c.profiles[0].width), (video_codec::AVC, 0, 0));
        ing.set_client_present(true);
        let key = au(&[0x65], 100);
        let mut out = Vec::new();
        ing.on_frame(0, VideoIn { seq: 1, rx_us: 5, frames_lost: 0, recovered: false, data: &key }, &mut out);
        let IngressOut::Frame(f) = &out[0] else { panic!("{out:?}") };
        assert!(f.key);
        assert_eq!(f.t0, 5);
    }

    #[test]
    fn param_sets_inside_frame_stream_become_config() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        ing.set_client_present(true);
        let p = params();
        let out = feed(&mut ing, 0, VideoIn { seq: 1, rx_us: 0, frames_lost: 0, recovered: false, data: &p });
        assert_eq!(out, ["C0"]);
        assert_eq!(ing.next_fid(), 0);
    }

    #[test]
    fn recovered_forward_sets_w() {
        let (mut ing, mut src) = streaming();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        let mut f = src.next(false);
        f.recovered = true;
        f.frames_lost = 1;
        assert_eq!(feed(&mut ing, 0, f), ["F1W"]);
        assert_eq!(ing.stats().w_forwarded, 1);
        assert_eq!(ing.waiting(), None);
    }

    #[test]
    fn recovered_policy_wait_key() {
        let mut ing = Ingress::new(RecoveredPolicy::WaitKey);
        ing.set_client_present(true);
        let mut src = Src::new();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        // frames_lost 但没改写：WaitKey 档照常转发
        let mut f = src.next(false);
        f.frames_lost = 2;
        assert_eq!(feed(&mut ing, 0, f), ["F1"]);
        let mut f = src.next(false);
        f.recovered = true;
        assert_eq!(feed(&mut ing, 0, f), ["IDR:WaitKey(Recovered)", "KP2"]);
        assert_eq!(ing.waiting(), Some(WaitWhy::Recovered));
        assert!(feed(&mut ing, 0, src.next(false)).is_empty());
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F2K"]);
        assert_eq!(ing.stats().dropped_policy, 1);
    }

    #[test]
    fn lost_until_key() {
        let mut ing = Ingress::new(RecoveredPolicy::LostUntilKey);
        ing.set_client_present(true);
        let mut src = Src::new();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        let mut f = src.next(false);
        f.frames_lost = 1;
        assert_eq!(feed(&mut ing, 0, f), ["IDR:WaitKey(Recovered)", "KP1"]);
        // 之后的 P 帧不管有没有丢都不转发，也不重复请求
        assert!(feed(&mut ing, 0, src.next(false)).is_empty());
        let mut f = src.next(false);
        f.frames_lost = 3;
        assert!(feed(&mut ing, 0, f).is_empty());
        // 带 frames_lost 的关键帧照常转发，解除等待
        let mut f = src.next(true);
        f.frames_lost = 1;
        assert_eq!(feed(&mut ing, 0, f), ["F1K"]);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["F2"]);
        assert_eq!(ing.stats().wait_entries, 1);
    }

    #[test]
    fn seq_gap_forward_only_counts() {
        let (mut ing, mut src) = streaming();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        src.seq += 3; // 队列里丢了 3 帧
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["F1"]);
        assert_eq!((ing.stats().seq_gaps, ing.stats().seq_missing), (1, 3));
        // 新会话的 seq 不接着算
        ing.on_epoch(2);
        src.seq = 100;
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F2K"]);
        assert_eq!(ing.stats().seq_gaps, 1);
    }

    #[test]
    fn seq_gap_enters_wait_key_when_not_trusting() {
        let mut ing = Ingress::new(RecoveredPolicy::WaitKey);
        ing.set_client_present(true);
        let mut src = Src::new();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        src.seq += 1;
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["IDR:WaitKey(Overflow)", "KP1"]);
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F1K"]);
    }

    #[test]
    fn too_large_frame_dropped_and_waits() {
        let (mut ing, mut src) = streaming();
        assert_eq!(feed(&mut ing, 0, src.next(true)), ["F0K"]);
        let big = au(&[TRAIL], 1024 * MAX_SYMBOL_DATA);
        assert!(big.len() > 1024 * MAX_SYMBOL_DATA);
        let out = feed(&mut ing, 0, VideoIn { seq: src.seq + 1, rx_us: 0, frames_lost: 0, recovered: false, data: &big });
        src.seq += 1;
        assert_eq!(out, ["IDR:WaitKey(TooLarge)", "KP1"]);
        assert_eq!(ing.stats().dropped_too_large, 1);
        // 正好 1024 个符号的帧可以发
        let edge = vec![0u8; 1024 * MAX_SYMBOL_DATA];
        let mut edge_key = au(&[IDR], 0);
        edge_key.resize(edge.len(), 0xAB);
        assert_eq!(feed(&mut ing, 0, VideoIn { seq: src.seq + 1, rx_us: 0, frames_lost: 0, recovered: false, data: &edge_key }), ["F1K"]);
    }

    #[test]
    fn empty_frame_dropped_silently() {
        let (mut ing, _) = streaming();
        assert!(feed(&mut ing, 0, VideoIn { seq: 1, rx_us: 0, frames_lost: 0, recovered: false, data: &[] }).is_empty());
        assert_eq!(ing.stats().dropped_empty, 1);
        assert_eq!(ing.waiting(), None);
    }

    #[test]
    fn wait_key_without_client_emits_nothing() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        let mut src = Src::new();
        ing.wait_key(WaitWhy::HeldResume);
        assert!(feed(&mut ing, 0, src.next(false)).is_empty());
        let mut out = Vec::new();
        ing.on_tick(ms(10_000), &mut out);
        assert!(out.is_empty());
        // 客户端接入前的 KEY_PENDING 不会留到接入后
        ing.set_client_present(true);
        ing.set_client_present(false);
        ing.set_client_present(true);
        ing.on_tick(ms(10_000), &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn new_client_while_waiting_gets_its_own_key_pending() {
        let (mut ing, mut src) = streaming();
        ing.wait_key(WaitWhy::HeldResume);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["IDR:WaitKey(HeldResume)", "KP0"]);
        ing.wait_key(WaitWhy::NewClient);
        assert_eq!(feed(&mut ing, 0, src.next(false)), ["IDR:WaitKey(NewClient)", "KP0"]);
        assert_eq!(ing.waiting(), Some(WaitWhy::NewClient));
    }

    #[test]
    fn no_frames_for_300ms_requests_idr_once() {
        let (mut ing, mut src) = streaming();
        let mut out = Vec::new();
        // 接入后第一次 tick 定下基准
        ing.on_tick(ms(1_000), &mut out);
        assert!(out.is_empty());
        assert_eq!(feed(&mut ing, ms(1_010), src.next(true)), ["F0K"]);
        ing.on_tick(ms(1_309), &mut out);
        assert!(out.is_empty());
        ing.on_tick(ms(1_310), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["IDR:NoFrames", "KP1"]);
        assert_eq!(ing.waiting(), Some(WaitWhy::NoFrames));
        // 已经在等：不再触发
        out.clear();
        ing.on_tick(ms(3_000), &mut out);
        assert!(out.is_empty());
        // P 帧丢掉，关键帧恢复
        assert!(feed(&mut ing, ms(3_100), src.next(false)).is_empty());
        assert_eq!(feed(&mut ing, ms(3_200), src.next(true)), ["F1K"]);
        ing.on_tick(ms(3_400), &mut out);
        assert!(out.is_empty());
        ing.on_tick(ms(3_500), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["IDR:NoFrames", "KP2"]);
        assert_eq!(ing.stats().no_frame_triggers, 2);
    }

    #[test]
    fn no_frames_baseline_is_attach_time() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        let mut src = Src::new();
        // 很久以前有过帧，然后 PS5 停了；客户端后来才接入
        feed(&mut ing, ms(100), src.next(true));
        ing.set_client_present(true);
        let mut out = Vec::new();
        ing.on_tick(ms(50_000), &mut out);
        assert!(out.is_empty(), "接入后第一拍不立即触发");
        ing.on_tick(ms(50_299), &mut out);
        assert!(out.is_empty());
        ing.on_tick(ms(50_300), &mut out);
        assert_eq!(out.iter().map(describe).collect::<Vec<_>>(), ["IDR:NoFrames", "KP0"]);
    }

    #[test]
    fn no_frames_without_client_never_triggers() {
        let mut ing = Ingress::new(RecoveredPolicy::default());
        let mut out = Vec::new();
        for t in 0..100 {
            ing.on_tick(ms(t * 100), &mut out);
        }
        assert!(out.is_empty());
    }
}
