//! 两端共用的会话核心（B 组，端点内部）：密钥确认之后的一条 YWP 会话。
//!
//! - 发送：PN 从 0 起每包加 1；小块（ACK、CONTROL、PING/PONG、PATH_*、FRAME_STATUS、REPORT、NEED、CLOSE）
//!   和视频符号经 [`Scheduler`] 按规格 §6.2 的优先级组包，再用 [`PacketWriter`] 加密（规格 §5.2、§5.3）；
//! - CONTROL 走 [`ControlPlane`]，RTO 取自 [`RttEstimator`]（以握手 RTT 作种子，规格 §5.5.5）；
//! - PING 每 100 ms 一次，PONG 的 hold 按收到 PING 到写进包的时间算（规格 §5.5.8、§13.1）；
//! - CLOSE 一律 3 份：0、5、10 ms，各用新的 PN，发完丢弃密钥（规格 §5.5.8）。
//!
//! 收包的解密和块解析由两端各自做（网关要逐份候选试解），这里只提供共用的块处理。

use super::{ConnStats, Transmit};
use crate::consts::{
    chunk_len, close_code, CHUNK_TLV_HDR, DATA_OVERHEAD, HDR_LEN, PN_TRUNC_BITS, SEND_TS_LEN, TAG_LEN,
};
use crate::crypto::{DirKey, TransportKeys};
use crate::error::{DropCounters, DropReason};
use crate::msg::ControlMsg;
use crate::packet::{open_data, PacketWriter, PathRx};
use crate::reliable::ControlPlane;
use crate::rtt::RttEstimator;
use crate::sched::{prio_of, ChunkKind, Dir, Prio, Scheduler, SmallOpts, SymbolSource};
use crate::time::{send_ts, Micros, CLOSE_COPIES, PING_INTERVAL};
use crate::wire::{
    Ack, Chunk, Cid, Class, CloseOwned, ControlFragOwned, EncodeChunk, FrameStatusEntry, FrameStatusOwned, Need,
    OuterHeader, PacketType, PathChallenge, PathResponse, Ping, Pong, ReportOwned,
};
use std::net::SocketAddr;

/// 块明文的起点：外层头 + SendTS
pub(crate) const CHUNKS_AT: usize = HDR_LEN + SEND_TS_LEN;

/// 解密成功的 DATA 包的元数据（不借用包缓冲；块明文在 pkt[CHUNKS_AT..len − 16]）
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenedMeta {
    pub pn: u64,
    pub send_ts: u32,
    pub new_largest: bool,
}

/// open_data 的包装：成功时只交出元数据，免得借用 pkt（网关逐份试解时要在循环里反复借）
pub(crate) fn try_open(pkt: &mut [u8], key: &DirKey, rx: &mut PathRx) -> Result<OpenedMeta, DropReason> {
    let o = open_data(pkt, key, rx)?;
    Ok(OpenedMeta { pn: o.pn, send_ts: o.send_ts, new_largest: o.new_largest })
}

/// 解密之后块明文所在的切片
pub(crate) fn chunk_bytes(pkt: &[u8]) -> &[u8] {
    &pkt[CHUNKS_AT..pkt.len() - TAG_LEN]
}

/// 调度器里的小块（编码前的形态）
#[derive(Clone, Debug)]
pub(crate) enum Small {
    Ack(Ack),
    Ctl(ControlFragOwned),
    Ping(u16),
    /// hold 在写进包时按 now − rx_at 算
    Pong { ping_id: u16, echo_ts: u32, rx_at: Micros },
    PathChallenge([u8; 8]),
    PathResponse([u8; 8]),
    FrameStatus(FrameStatusEntry),
    Report(ReportOwned),
    Need(Need),
    Close(CloseOwned),
}

impl Small {
    /// 编码后的块长（含 3 B 的 type/len）
    fn len(&self) -> usize {
        CHUNK_TLV_HDR
            + match self {
                Small::Ack(a) => a.content_len(),
                Small::Ctl(f) => f.content_len(),
                Small::Ping(_) => chunk_len::PING,
                Small::Pong { .. } => chunk_len::PONG,
                Small::PathChallenge(_) => chunk_len::PATH_CHALLENGE,
                Small::PathResponse(_) => chunk_len::PATH_RESPONSE,
                Small::FrameStatus(_) => chunk_len::FRAME_STATUS_HDR + chunk_len::FRAME_STATUS_ENTRY,
                Small::Report(r) => r.content_len(),
                Small::Need(n) => n.content_len(),
                Small::Close(c) => c.content_len(),
            }
    }

    fn kind(&self) -> ChunkKind {
        match self {
            Small::Ack(_) => ChunkKind::Ack,
            Small::Ctl(f) => ChunkKind::Control { critical: false, stream: f.stream },
            Small::Ping(_) => ChunkKind::Ping,
            Small::Pong { .. } => ChunkKind::Pong,
            Small::PathChallenge(_) => ChunkKind::PathChallenge,
            Small::PathResponse(_) => ChunkKind::PathResponse,
            Small::FrameStatus(_) => ChunkKind::FrameStatus,
            Small::Report(_) => ChunkKind::Report,
            Small::Need(_) => ChunkKind::Need,
            Small::Close(_) => ChunkKind::Close,
        }
    }

    /// 写进包里。放不下返回 false（调度器按长度排过，正常不会发生）
    fn write(&self, w: &mut PacketWriter<'_>, now: Micros) -> bool {
        let r = match self {
            Small::Ack(a) => w.push(a),
            Small::Ctl(f) => w.push(f),
            Small::Ping(id) => w.push(&Ping(*id)),
            Small::Pong { ping_id, echo_ts, rx_at } => {
                let hold = (now.saturating_sub(*rx_at) / 10).min(u16::MAX as u64) as u16;
                w.push(&Pong { ping_id: *ping_id, echo_ts: *echo_ts, hold_10us: hold })
            }
            Small::PathChallenge(x) => w.push(&PathChallenge(*x)),
            Small::PathResponse(x) => w.push(&PathResponse(*x)),
            Small::FrameStatus(e) => w.push(&FrameStatusOwned { entries: vec![*e] }),
            Small::Report(r) => w.push(r),
            Small::Need(n) => w.push(n),
            Small::Close(c) => w.push(c),
        };
        r.is_ok()
    }

    /// 这个块要求包的 Class（规格 §5.1：3 输入/关键，2 音频/反馈/控制）
    fn class(&self) -> Class {
        match self {
            Small::Need(_) | Small::Close(_) => Class::Input,
            _ => Class::Media,
        }
    }
}

/// 正在发 CLOSE 的 3 份
#[derive(Clone, Debug)]
struct Closing {
    close: CloseOwned,
    t0: Micros,
    sent: usize,
}

/// 共用的块处理结果：调用方要另外处理的
pub(crate) enum Common<'a> {
    /// 已处理完
    Done,
    /// PONG 取到了 RTT 样本
    Rtt(Micros),
    /// 对端发来 CLOSE
    Close(u16),
    /// 不是共用的块，交给调用方
    Other(Chunk<'a>),
}

/// 一条已建立的会话（单路径，path_id = 0）
#[derive(Debug)]
pub(crate) struct Sess {
    pub cid: Cid,
    pub dir: Dir,
    pub path_id: u8,
    keys: TransportKeys,
    /// 下一个要发的 PN
    tx_pn: u64,
    pub rx: PathRx,
    pub peer: SocketAddr,
    pub cp: ControlPlane,
    pub rtt: RttEstimator,
    pub sched: Scheduler<Small>,
    next_ping: Micros,
    ping_id: u16,
    /// 最近一次收到认证通过的包
    pub last_rx: Micros,
    pub created: Micros,
    closing: Option<Closing>,
    /// 认证通过的 DATA 包数（REPORT 的 recv_cnt）
    pub recv_cnt: u64,
    pub stats: ConnStats,
}

impl Sess {
    pub fn new(cid: Cid, dir: Dir, keys: TransportKeys, rx: PathRx, peer: SocketAddr, now: Micros) -> Self {
        Self {
            cid,
            dir,
            path_id: 0,
            keys,
            tx_pn: 0,
            rx,
            peer,
            cp: ControlPlane::new(dir),
            rtt: RttEstimator::new(),
            sched: Scheduler::new(),
            next_ping: now,
            ping_id: 0,
            last_rx: now,
            created: now,
            closing: None,
            recv_cnt: 0,
            stats: ConnStats::default(),
        }
    }

    /// 用本会话的收方向密钥解一个 DATA（失败时 pkt 和 rx 都不变）
    pub fn open(&mut self, pkt: &mut [u8]) -> Result<OpenedMeta, DropReason> {
        try_open(pkt, &self.keys.rx, &mut self.rx)
    }

    /// 已经发了多少个包（下一个 PN）
    pub fn tx_pn(&self) -> u64 {
        self.tx_pn
    }

    fn prio(&self, s: &Small) -> Prio {
        prio_of(self.dir, s.kind()).unwrap_or(Prio::P6)
    }

    /// 排一个小块，立即可发
    pub fn queue(&mut self, s: Small, now: Micros) {
        self.queue_opts(s, SmallOpts { not_before: now, deadline: None, repeat: 1 });
    }

    pub fn queue_opts(&mut self, s: Small, opts: SmallOpts) {
        let p = self.prio(&s);
        let len = s.len();
        self.sched.push(p, len, s, opts);
    }

    pub fn send_control(&mut self, m: &ControlMsg) -> Result<(), crate::reliable::CtlError> {
        if self.closing.is_some() {
            return Err(crate::reliable::CtlError::NotReady);
        }
        self.cp.send(m)
    }

    /// PING 下一次的时刻往后挪到 at（握手刚完成时立即发一个）
    pub fn ping_at(&mut self, at: Micros) {
        self.next_ping = at;
    }

    pub fn is_closing(&self) -> bool {
        self.closing.is_some()
    }

    /// 3 份 CLOSE 都发完了，可以丢弃密钥
    pub fn close_done(&self) -> bool {
        self.closing.as_ref().is_some_and(|c| c.sent >= CLOSE_COPIES.len())
    }

    /// 开始发 3 份 CLOSE。之后不再发别的
    pub fn start_close(&mut self, code: u16, reason: &str, now: Micros) {
        if self.closing.is_some() {
            return;
        }
        self.sched.clear_video();
        self.sched.retain_small(|_| false);
        self.closing = Some(Closing { close: CloseOwned { code, reason: reason.to_owned() }, t0: now, sent: 0 });
    }

    /// 把到期的 PING、CONTROL、ACK 排进调度器。media：这一轮能不能发视频
    fn refill(&mut self, now: Micros, media: bool) {
        if now >= self.next_ping {
            let id = self.ping_id;
            self.ping_id = self.ping_id.wrapping_add(1);
            self.next_ping = now + PING_INTERVAL;
            self.queue(Small::Ping(id), now);
        }
        let rto = self.rtt.rto();
        while let Some((f, p)) = self.cp.poll(now, rto) {
            let s = Small::Ctl(f);
            let len = s.len();
            self.sched.push(p, len, s, SmallOpts { not_before: now, deadline: None, repeat: 1 });
        }
        // 反正要发包时 ACK 立即搭车；否则等到 2 ms 的期限
        let force = self.sched.small_due(now) || (media && self.sched.video_len() > 0);
        while let Some(a) = self.cp.take_ack(now, force) {
            self.queue(Small::Ack(a), now);
        }
    }

    /// 组一个包写进 out。limit：整包上限（下行 1232、上行 400）；video：None 表示这一轮不发视频
    pub fn build(
        &mut self,
        now: Micros,
        limit: usize,
        video: Option<&dyn SymbolSource>,
        out: &mut [u8],
    ) -> Option<Transmit> {
        if let Some(c) = &mut self.closing {
            if c.sent >= CLOSE_COPIES.len() || now < c.t0 + CLOSE_COPIES[c.sent] {
                return None;
            }
            c.sent += 1;
            let close = Small::Close(c.close.clone());
            return self.seal_packet(now, limit, std::slice::from_ref(&close), None, out);
        }
        self.refill(now, video.is_some());
        let packed = self.sched.pack(now, limit - DATA_OVERHEAD, video)?;
        let v = packed.video.and_then(|v| video.and_then(|src| src.symbol(v.hdr.fid, v.hdr.sym_idx)).map(|b| (v, b)));
        self.seal_packet(now, limit, &packed.small, v, out)
    }

    fn seal_packet(
        &mut self,
        now: Micros,
        limit: usize,
        small: &[Small],
        video: Option<(crate::sched::VideoCarrier, &[u8])>,
        out: &mut [u8],
    ) -> Option<Transmit> {
        let mut w = PacketWriter::new(out, limit, send_ts(now));
        let mut class = Class::Media;
        for s in small {
            if !s.write(&mut w, now) {
                self.stats.drops.bump(DropReason::SendQueueFull);
            }
            if s.class() == Class::Input {
                class = Class::Input;
            }
        }
        let mut tag = None;
        if let Some((v, body)) = video {
            if w.push_video_tail(&v.hdr, body).is_ok() {
                class = Class::Video;
                tag = Some((v.hdr.fid, v.hdr.sym_idx));
            } else {
                self.stats.drops.bump(DropReason::SendQueueFull);
            }
        }
        if w.is_empty() {
            return None;
        }
        let hdr = OuterHeader {
            ptype: PacketType::Data,
            key_phase: false,
            class,
            cid: self.cid,
            path_id: self.path_id,
            pn_trunc: (self.tx_pn & ((1u64 << PN_TRUNC_BITS) - 1)) as u32,
        };
        let pn = self.tx_pn;
        self.tx_pn += 1;
        let len = w.seal(&hdr, pn, &self.keys.tx);
        self.stats.tx_packets += 1;
        self.stats.tx_bytes += len as u64;
        Some(Transmit { dst: self.peer, len, video: tag })
    }

    /// 下一次要 poll 的时刻。调度器里有视频载体时是 Some(0)（随时可发）：
    /// 只有能发媒体的会话（网关上已确认、没在关闭的）才会排视频
    pub fn next_deadline(&self) -> Option<Micros> {
        if let Some(c) = &self.closing {
            return CLOSE_COPIES.get(c.sent).map(|d| c.t0 + d);
        }
        [Some(self.next_ping), self.cp.next_deadline(), self.sched.next_due()].into_iter().flatten().min()
    }

    /// 收到一个认证通过的包：计数、刷新时间
    pub fn on_rx(&mut self, now: Micros, len: usize) {
        self.last_rx = now;
        self.recv_cnt += 1;
        self.stats.rx_packets += 1;
        self.stats.rx_bytes += len as u64;
    }

    /// 两端共用的块：PING、PONG、PATH_*、CLOSE、CONTROL、ACK、PADDING、未知块
    pub fn common<'a>(&mut self, c: Chunk<'a>, send_ts: u32, now: Micros) -> Common<'a> {
        match c {
            Chunk::Padding => Common::Done,
            Chunk::Ping(id) => {
                self.queue(Small::Pong { ping_id: id, echo_ts: send_ts, rx_at: now }, now);
                Common::Done
            }
            Chunk::Pong(p) => match self.rtt.on_pong(now, &p) {
                Some(r) => {
                    self.stats.srtt_us = self.rtt.srtt();
                    Common::Rtt(r)
                }
                None => Common::Done,
            },
            Chunk::PathChallenge(x) => {
                self.queue(Small::PathResponse(x), now);
                Common::Done
            }
            // v1 单路径：PATH_RESPONSE 只说明路径通了，不另做处理
            Chunk::PathResponse(_) => Common::Done,
            Chunk::Close { code, .. } => Common::Close(code),
            Chunk::Control(f) => {
                if let Err(r) = self.cp.on_frag(&f, now) {
                    self.stats.drops.bump(r);
                }
                Common::Done
            }
            Chunk::Ack(a) => {
                if let Err(r) = self.cp.on_ack(&a, now) {
                    self.stats.drops.bump(r);
                }
                Common::Done
            }
            Chunk::Unknown { .. } => {
                self.stats.drops.bump(DropReason::UnknownChunk);
                Common::Done
            }
            other => Common::Other(other),
        }
    }

    /// 按序取出已收齐的 CONTROL 消息（解不开的、不认识的计数）
    pub fn pop_control(&mut self) -> Option<ControlMsg> {
        loop {
            match self.cp.pop()? {
                Ok(ControlMsg::Unknown { .. }) => self.stats.drops.bump(DropReason::CtlUnknownMsg),
                Ok(m) => return Some(m),
                Err(r) => self.stats.drops.bump(r),
            }
        }
    }

    /// REPORT 的每路径块里的累计计数（M2a 近似：lost = largest + 1 − recv，M2c 换成 loss::RxLoss）
    pub fn loss_counts(&self) -> (u32, u32, u32) {
        let largest = self.rx.largest.unwrap_or(0);
        let expect = self.rx.largest.map_or(0, |l| l + 1);
        let lost = expect.saturating_sub(self.recv_cnt);
        (largest as u32, self.recv_cnt as u32, lost as u32)
    }

    pub fn drops(&mut self) -> &mut DropCounters {
        &mut self.stats.drops
    }

    pub fn snapshot_stats(&self) -> ConnStats {
        let mut s = self.stats.clone();
        s.srtt_us = self.rtt.srtt();
        s
    }
}

/// 关闭原因的默认文字（日志用，不含任何环境信息）
pub(crate) fn close_text(code: u16) -> &'static str {
    match code {
        close_code::NORMAL => "normal",
        close_code::TAKEN_OVER => "taken over",
        close_code::PROTOCOL_ERROR => "protocol error",
        close_code::VERSION_MISMATCH => "version mismatch",
        close_code::GATEWAY_ENDED => "gateway ended session",
        close_code::GATEWAY_EXIT => "gateway exit",
        _ => "",
    }
}
