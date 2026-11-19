//! 发送调度（B 组，规格 §6.2、§5.3）：严格按优先级发，同一级里截止期早的先发。
//!
//! - [`prio_of`] 是规格 §6.2 的优先级表，按方向（下行 G→C、上行 C→G）给出每种块的级别；
//! - [`Scheduler`] 是端点内部的组包调度：小块带 not_before / deadline / repeat，
//!   视频载体按 (prio, order, 入队序号) 排；每次 [`Scheduler::pack`] 给出下一个包里放哪些块。
//!
//! 组包规则（规格 §5.3）：
//! - 一个视频符号独占一个包，VIDEO 块必须是最后一块；它前面可以放装得下的小块；
//! - 整体严格按优先级：最高优先级的东西决定这个包「以谁为主」，剩下的空间按优先级顺序贪心装填，
//!   装不下的跳过、留给下一个包（空包一定装得下队首，所以不会饿死）；
//! - 要求连续几个包各带一次的块（FRAME_STATUS 连续 3 个包），每装一次 repeat 减 1，同一个包里只装一次。
//!
//! 调度器只看长度和优先级，不碰字节编码：调用方把「编码后的块长」（含 3 B 的 type/len）告诉它，
//! 拿到计划后再用 PacketWriter 逐块写。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::consts::{stream, VIDEO_HDR_LEN, VIDEO_SRC_LEN_LEN};
use crate::time::Micros;
use crate::wire::VideoHeader;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

/// 发送优先级，P1 最高
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prio {
    P1 = 1,
    P2,
    P3,
    P4,
    P5,
    P6,
}

/// 一个待发的视频符号。正文在组包时经 SymbolSource 取，不拷贝
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCarrier {
    pub hdr: crate::wire::VideoHeader,
    pub prio: Prio,
    pub order: u64,
}

/// 视频符号的正文来源（网关里是 yw-media 的 VideoTx）
pub trait SymbolSource {
    fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]>;
}

/// 发送方向
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    /// 网关 → 客户端
    Down,
    /// 客户端 → 网关
    Up,
}

/// 要排优先级的东西（规格 §6.2 表里的各行）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkKind {
    PathChallenge,
    PathResponse,
    Ping,
    Pong,
    Ack,
    /// CONTROL 消息：critical 是关键消息；stream 是流号（流 2 = 批量）
    Control { critical: bool, stream: u8 },
    FrameStatus,
    Audio,
    Feedback,
    UpReport,
    /// 视频修复包（响应 NACK / NEED）
    VideoRepair,
    /// 视频新源符号
    VideoSource,
    /// 视频初始校验
    VideoParity,
    Input,
    Nack,
    Need,
    Report,
    /// CLOSE：会话收尾，排最前
    Close,
}

/// 规格 §6.2 的优先级表。返回 None 表示这种块不该在这个方向发
pub fn prio_of(dir: Dir, kind: ChunkKind) -> Option<Prio> {
    use ChunkKind as K;
    use Prio::*;
    let p = match (dir, kind) {
        (_, K::Close) => P1,
        // ---- 网关 → 客户端 ----
        (Dir::Down, K::PathChallenge | K::PathResponse | K::Pong | K::FrameStatus | K::Ack) => P1,
        (Dir::Down, K::Control { critical: true, .. }) => P1,
        (Dir::Down, K::Audio | K::Feedback | K::UpReport) => P2,
        (Dir::Down, K::VideoRepair | K::Ping) => P3,
        (Dir::Down, K::VideoSource) => P4,
        (Dir::Down, K::VideoParity) => P5,
        (Dir::Down, K::Control { critical: false, .. }) => P6,
        // ---- 客户端 → 网关 ----
        (Dir::Up, K::Input) => P1,
        (Dir::Up, K::Nack | K::Need | K::PathChallenge | K::PathResponse | K::Ack) => P2,
        (Dir::Up, K::Control { critical: true, .. }) => P2,
        (Dir::Up, K::Report | K::Ping) => P3,
        (Dir::Up, K::Control { critical: false, stream: s }) if s == stream::BULK => P5,
        (Dir::Up, K::Control { critical: false, .. }) => P4,
        // 规格 §6.2 没列的组合：客户端发 PONG（回网关的 PING）和 ACK 同级
        (Dir::Up, K::Pong) => P2,
        _ => return None,
    };
    Some(p)
}

/// 一个视频块编码后的长度（不含外层头、SendTS 和标签）：
/// 源符号 = 10 + 2 + 数据，校验符号 = 10 + S（规格 §5.5.1）
pub fn video_chunk_len(hdr: &VideoHeader, body_len: usize) -> usize {
    if hdr.parity {
        VIDEO_HDR_LEN + body_len
    } else {
        VIDEO_HDR_LEN + VIDEO_SRC_LEN_LEN + body_len
    }
}

/// 小块入队时的选项
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SmallOpts {
    /// 不早于这个时刻发
    pub not_before: Micros,
    /// 同一级里截止期早的先发；None 排在有截止期的后面
    pub deadline: Option<Micros>,
    /// 要在连续几个包里各带一次（≥ 1）
    pub repeat: u8,
}

impl Default for SmallOpts {
    fn default() -> Self {
        Self { not_before: 0, deadline: None, repeat: 1 }
    }
}

#[derive(Clone, Debug)]
struct SmallEntry<T> {
    prio: Prio,
    deadline: Option<Micros>,
    not_before: Micros,
    repeat: u8,
    seq: u64,
    len: usize,
    item: T,
}

impl<T> SmallEntry<T> {
    /// 排序键：(级别, 截止期（没有的排后）, 入队序号)
    fn key(&self) -> (Prio, Micros, u64) {
        (self.prio, self.deadline.unwrap_or(Micros::MAX), self.seq)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VideoEntry {
    v: VideoCarrier,
    seq: u64,
}

impl Ord for VideoEntry {
    fn cmp(&self, o: &Self) -> Ordering {
        (self.v.prio, self.v.order, self.seq).cmp(&(o.v.prio, o.v.order, o.seq))
    }
}

impl PartialOrd for VideoEntry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// 一个包的装填计划
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packed<T> {
    /// 按优先级排好的小块（照这个顺序写进包里）
    pub small: Vec<T>,
    /// 包尾的视频符号（有的话）
    pub video: Option<VideoCarrier>,
    /// 计划占用的字节数（各块编码后的长度之和）
    pub used: usize,
}

/// 组包调度器。T 是小块（调用方自己的类型，比如 CONTROL 分片、ACK、PONG）
#[derive(Clone, Debug)]
pub struct Scheduler<T> {
    small: Vec<SmallEntry<T>>,
    video: BinaryHeap<Reverse<VideoEntry>>,
    seq: u64,
    /// 正文已不在 SymbolSource 里、被丢掉的视频载体数
    video_gone: u64,
}

impl<T: Clone> Default for Scheduler<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> Scheduler<T> {
    pub fn new() -> Self {
        Self { small: Vec::new(), video: BinaryHeap::new(), seq: 0, video_gone: 0 }
    }

    /// 小块入队。len 是编码后的块长（含 type/len 3 B）
    pub fn push(&mut self, prio: Prio, len: usize, item: T, opts: SmallOpts) {
        self.small.push(SmallEntry {
            prio,
            deadline: opts.deadline,
            not_before: opts.not_before,
            repeat: opts.repeat.max(1),
            seq: self.seq,
            len,
            item,
        });
        self.seq += 1;
    }

    /// 视频载体入队
    pub fn push_video(&mut self, v: VideoCarrier) {
        self.video.push(Reverse(VideoEntry { v, seq: self.seq }));
        self.seq += 1;
    }

    pub fn small_len(&self) -> usize {
        self.small.len()
    }

    pub fn video_len(&self) -> usize {
        self.video.len()
    }

    pub fn is_empty(&self) -> bool {
        self.small.is_empty() && self.video.is_empty()
    }

    /// 正文已经不在、被丢掉的视频载体累计数
    pub fn video_gone(&self) -> u64 {
        self.video_gone
    }

    /// 只保留 f 返回 true 的小块（比如会话关闭时清掉媒体类）
    pub fn retain_small(&mut self, mut f: impl FnMut(&T) -> bool) {
        self.small.retain(|e| f(&e.item));
    }

    /// 清空视频载体（比如进入 Held、换了会话）
    pub fn clear_video(&mut self) {
        self.video.clear();
    }

    /// 现在有没有可以发的小块
    pub fn small_due(&self, now: Micros) -> bool {
        self.small.iter().any(|e| e.not_before <= now)
    }

    /// 下一次要醒来的时刻：小块里最早的 not_before（已经到期的就是它本身，可能早于 now）；
    /// 有视频载体时返回 Some(0)，表示随时可发（是否允许发媒体由端点决定）
    pub fn next_due(&self) -> Option<Micros> {
        let s = self.small.iter().map(|e| e.not_before).min();
        if self.video.is_empty() {
            s
        } else {
            Some(0)
        }
    }

    /// 给出下一个包的装填计划。
    /// - room：这个包里留给块的字节数（整包上限减去外层头、SendTS、标签）；
    /// - video：视频正文来源；传 None 表示这次不发视频（比如密钥还没确认、在 Held）。
    ///
    /// 没有可发的东西时返回 None。
    pub fn pack(&mut self, now: Micros, room: usize, video: Option<&dyn SymbolSource>) -> Option<Packed<T>> {
        // 1. 视频队首（跳过正文已经不在的）
        let mut head_video: Option<(VideoCarrier, usize)> = None;
        if let Some(src) = video {
            while let Some(Reverse(e)) = self.video.peek().copied() {
                match src.symbol(e.v.hdr.fid, e.v.hdr.sym_idx) {
                    Some(body) => {
                        head_video = Some((e.v, video_chunk_len(&e.v.hdr, body.len())));
                        break;
                    }
                    None => {
                        self.video.pop();
                        self.video_gone += 1;
                    }
                }
            }
        }

        // 2. 到期的小块，按 (级别, 截止期, 序号) 排
        let mut due: Vec<usize> = (0..self.small.len()).filter(|&i| self.small[i].not_before <= now).collect();
        due.sort_unstable_by_key(|&i| self.small[i].key());

        if due.is_empty() && head_video.is_none() {
            return None;
        }

        // 3. 这个包以谁为主：视频队首的级别严格高于最高的小块时，先给视频留位置；同级时小块先装
        let top_small = due.first().map(|&i| self.small[i].prio);
        let video_first = match (head_video, top_small) {
            (Some((v, len)), Some(p)) => v.prio < p && len <= room,
            (Some((_, len)), None) => len <= room,
            _ => false,
        };

        let mut used = 0usize;
        let mut take_video = None;
        if video_first {
            let (v, len) = head_video.expect("video_first 意味着有视频");
            used += len;
            take_video = Some(v);
        }

        // 4. 小块按顺序贪心装填
        let mut picked: Vec<usize> = Vec::new();
        for &i in &due {
            let len = self.small[i].len;
            if used + len <= room {
                used += len;
                picked.push(i);
            }
        }

        // 5. 以小块为主的包，剩下的空间还够放视频队首，就顺带放上
        if take_video.is_none() {
            if let Some((v, len)) = head_video {
                if used + len <= room {
                    used += len;
                    take_video = Some(v);
                }
            }
        }

        if picked.is_empty() && take_video.is_none() {
            // 队首比 room 还大：调用方给的 room 不对，或块本身超长。丢掉最前面那个，免得卡死
            if let Some(&i) = due.first() {
                self.small.remove(i);
            } else if head_video.is_some() {
                self.video.pop();
                self.video_gone += 1;
            }
            return None;
        }

        if take_video.is_some() {
            self.video.pop();
        }

        // 6. 取出选中的小块；repeat > 1 的留在队里，计数减 1
        let mut small = Vec::with_capacity(picked.len());
        for &i in &picked {
            small.push(self.small[i].item.clone());
        }
        let mut remove: Vec<usize> = Vec::new();
        for &i in &picked {
            let e = &mut self.small[i];
            if e.repeat > 1 {
                e.repeat -= 1;
            } else {
                remove.push(i);
            }
        }
        remove.sort_unstable();
        for &i in remove.iter().rev() {
            self.small.remove(i);
        }

        Some(Packed { small, video: take_video, used })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Src(HashMap<(u16, u16), Vec<u8>>);
    impl SymbolSource for Src {
        fn symbol(&self, fid: u16, sym: u16) -> Option<&[u8]> {
            self.0.get(&(fid, sym)).map(|v| v.as_slice())
        }
    }

    fn carrier(fid: u16, sym: u16, prio: Prio, order: u64) -> VideoCarrier {
        VideoCarrier { hdr: VideoHeader { fid, sym_idx: sym, k: 4, ..Default::default() }, prio, order }
    }

    #[test]
    fn table_matches_spec() {
        use ChunkKind as K;
        assert_eq!(prio_of(Dir::Down, K::Pong), Some(Prio::P1));
        assert_eq!(prio_of(Dir::Down, K::Ack), Some(Prio::P1));
        assert_eq!(prio_of(Dir::Down, K::Control { critical: true, stream: 0 }), Some(Prio::P1));
        assert_eq!(prio_of(Dir::Down, K::Audio), Some(Prio::P2));
        assert_eq!(prio_of(Dir::Down, K::UpReport), Some(Prio::P2));
        assert_eq!(prio_of(Dir::Down, K::Ping), Some(Prio::P3));
        assert_eq!(prio_of(Dir::Down, K::VideoRepair), Some(Prio::P3));
        assert_eq!(prio_of(Dir::Down, K::VideoSource), Some(Prio::P4));
        assert_eq!(prio_of(Dir::Down, K::VideoParity), Some(Prio::P5));
        assert_eq!(prio_of(Dir::Down, K::Control { critical: false, stream: 1 }), Some(Prio::P6));
        assert_eq!(prio_of(Dir::Down, K::Input), None);
        assert_eq!(prio_of(Dir::Up, K::Input), Some(Prio::P1));
        assert_eq!(prio_of(Dir::Up, K::Nack), Some(Prio::P2));
        assert_eq!(prio_of(Dir::Up, K::Ack), Some(Prio::P2));
        assert_eq!(prio_of(Dir::Up, K::Control { critical: true, stream: 0 }), Some(Prio::P2));
        assert_eq!(prio_of(Dir::Up, K::Report), Some(Prio::P3));
        assert_eq!(prio_of(Dir::Up, K::Ping), Some(Prio::P3));
        assert_eq!(prio_of(Dir::Up, K::Control { critical: false, stream: 1 }), Some(Prio::P4));
        assert_eq!(prio_of(Dir::Up, K::Control { critical: false, stream: 2 }), Some(Prio::P5));
        assert_eq!(prio_of(Dir::Up, K::VideoSource), None);
    }

    #[test]
    fn video_len_matches_budget() {
        // 满源符号：10 + 2 + 1186 = 1198 = 1232 − 14 − 4 − 16
        let src = VideoHeader { k: 1, ..Default::default() };
        assert_eq!(video_chunk_len(&src, crate::consts::MAX_SYMBOL_DATA), 1198);
        let par = VideoHeader { k: 1, sym_idx: 1, parity: true, ..Default::default() };
        assert_eq!(video_chunk_len(&par, crate::consts::MAX_SHARD_LEN), 1198);
    }

    #[test]
    fn strict_priority_then_deadline_then_fifo() {
        let mut s: Scheduler<&str> = Scheduler::new();
        s.push(Prio::P6, 10, "ctl-normal", SmallOpts::default());
        s.push(Prio::P3, 10, "ping", SmallOpts::default());
        s.push(Prio::P1, 10, "ack", SmallOpts::default());
        s.push(Prio::P1, 10, "pong-late-deadline", SmallOpts { deadline: Some(500), ..Default::default() });
        s.push(Prio::P1, 10, "pong-early-deadline", SmallOpts { deadline: Some(100), ..Default::default() });
        s.push(Prio::P2, 10, "audio", SmallOpts::default());
        // 一次只能装 3 个
        let p = s.pack(0, 30, None).unwrap();
        assert_eq!(p.small, vec!["pong-early-deadline", "pong-late-deadline", "ack"]);
        let p = s.pack(0, 30, None).unwrap();
        assert_eq!(p.small, vec!["audio", "ping", "ctl-normal"]);
        assert!(s.pack(0, 30, None).is_none());
    }

    #[test]
    fn not_before_holds_items() {
        let mut s: Scheduler<u8> = Scheduler::new();
        s.push(Prio::P1, 10, 1, SmallOpts { not_before: 5_000, ..Default::default() });
        s.push(Prio::P6, 10, 2, SmallOpts::default());
        assert_eq!(s.next_due(), Some(0));
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec![2]);
        assert_eq!(s.next_due(), Some(5_000));
        assert!(s.pack(4_999, 100, None).is_none());
        assert_eq!(s.pack(5_000, 100, None).unwrap().small, vec![1]);
    }

    #[test]
    fn greedy_fill_skips_what_does_not_fit() {
        let mut s: Scheduler<u8> = Scheduler::new();
        s.push(Prio::P1, 60, 1, SmallOpts::default());
        s.push(Prio::P2, 50, 2, SmallOpts::default());
        s.push(Prio::P3, 30, 3, SmallOpts::default());
        let p = s.pack(0, 100, None).unwrap();
        assert_eq!(p.small, vec![1, 3]);
        assert_eq!(p.used, 90);
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec![2]);
    }

    #[test]
    fn repeat_goes_into_consecutive_packets() {
        let mut s: Scheduler<&str> = Scheduler::new();
        s.push(Prio::P1, 5, "fs", SmallOpts { repeat: 3, ..Default::default() });
        s.push(Prio::P2, 5, "a", SmallOpts::default());
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec!["fs", "a"]);
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec!["fs"]);
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec!["fs"]);
        assert!(s.pack(0, 100, None).is_none());
    }

    #[test]
    fn video_one_per_packet_with_small_in_front() {
        let mut map = HashMap::new();
        map.insert((1, 0), vec![0u8; 1186]);
        map.insert((1, 1), vec![0u8; 100]);
        let src = Src(map);
        let mut s: Scheduler<&str> = Scheduler::new();
        s.push_video(carrier(1, 0, Prio::P4, 0));
        s.push_video(carrier(1, 1, Prio::P4, 1));
        s.push(Prio::P1, 20, "pong", SmallOpts::default());
        s.push(Prio::P6, 20, "ctl", SmallOpts::default());
        let room = crate::consts::BASE_PAYLOAD - crate::consts::DATA_OVERHEAD;
        // P1 的小块先装；满源符号和它一起装不下，所以第一包只有小块（P6 的控制也顺带装上）
        let p = s.pack(0, room, Some(&src)).unwrap();
        assert_eq!(p.small, vec!["pong", "ctl"]);
        assert_eq!(p.video, None);
        // 第二包：满源符号独占
        let p = s.pack(0, room, Some(&src)).unwrap();
        assert!(p.small.is_empty());
        assert_eq!(p.video.unwrap().hdr.sym_idx, 0);
        assert_eq!(p.used, 1198);
        // 第三包：小符号，前面没有小块
        s.push(Prio::P6, 20, "ctl2", SmallOpts::default());
        let p = s.pack(0, room, Some(&src)).unwrap();
        // 视频 P4 比控制 P6 优先，先给视频留位置；剩下的空间够放控制，顺带装上
        assert_eq!(p.video.unwrap().hdr.sym_idx, 1);
        assert_eq!(p.small, vec!["ctl2"]);
        assert!(s.is_empty());
    }

    #[test]
    fn video_order_and_gone_symbols() {
        let mut map = HashMap::new();
        for sym in 0..4 {
            map.insert((7, sym), vec![0u8; 50]);
        }
        let src = Src(map);
        let mut s: Scheduler<()> = Scheduler::new();
        s.push_video(carrier(7, 2, Prio::P5, 0)); // 初始校验
        s.push_video(carrier(7, 1, Prio::P4, 1));
        s.push_video(carrier(7, 0, Prio::P4, 0));
        s.push_video(carrier(9, 0, Prio::P3, 5)); // 修复包，正文已经不在
        s.push_video(carrier(7, 3, Prio::P3, 9)); // 修复包
        let mut got = Vec::new();
        while let Some(p) = s.pack(0, 1000, Some(&src)) {
            got.push(p.video.unwrap().hdr.sym_idx);
        }
        assert_eq!(got, vec![3, 0, 1, 2]);
        assert_eq!(s.video_gone(), 1);
    }

    #[test]
    fn no_video_source_means_no_video() {
        let mut s: Scheduler<u8> = Scheduler::new();
        s.push_video(carrier(1, 0, Prio::P4, 0));
        assert!(s.pack(0, 1000, None).is_none());
        assert_eq!(s.video_len(), 1);
    }

    #[test]
    fn oversized_head_is_dropped_not_stuck() {
        let mut s: Scheduler<u8> = Scheduler::new();
        s.push(Prio::P1, 2000, 1, SmallOpts::default());
        s.push(Prio::P2, 10, 2, SmallOpts::default());
        // 队首超长但后面的小块能装，就先装后面的
        assert_eq!(s.pack(0, 100, None).unwrap().small, vec![2]);
        // 只剩超长的：丢掉，不卡死
        assert!(s.pack(0, 100, None).is_none());
        assert_eq!(s.small_len(), 0);
    }
}
