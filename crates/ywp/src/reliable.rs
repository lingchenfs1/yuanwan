//! CONTROL 可靠层（B 组，规格 §5.5.5）：
//! - 每个流、每个会话 msg_seq 从 0 开始，16 位回绕；窗口 32 条（正好是 sack 能表示的范围），按序交付；
//! - 收端 2 ms 内回 ACK；重传整条消息；RTO = max(25 ms, SRTT + 4 × RTTVAR)，指数退避，封顶 1 s，
//!   以握手 RTT 作种子，不从 ACK 取样本（RTO 由调用方从 [`crate::rtt::RttEstimator`] 取来传进 poll）；
//! - 关键消息在首次发出后 +5 ms 再发一份（已经被确认的就不再发）；
//! - v1 只发单片（frag = 0x00；下行 ≤ 1100 B，上行 ≤ 240 B）；快速重传留到以后（v1 只靠 RTO 和关键消息双发）；
//! - 会话存续期间不放弃重传。
//!
//! 窗口满了的消息先进发送方的积压队列（最多 [`CTL_BACKLOG_MAX`] 条），窗口腾出来再编号发出；
//! 积压也满了才返回 [`CtlError::WindowFull`]。
//!
//! [`ControlPlane`] 把三个流的收发合在一起，并负责 [`ControlMsg`] 的编解码，端点直接用它。

#![warn(unused_variables, dead_code, clippy::new_without_default, clippy::ptr_arg)]

use crate::consts::{stream, CTL_CUM_INIT, CTL_FRAG_MAX_DOWN, CTL_FRAG_MAX_UP, CTL_SEQ_INIT, CTL_WINDOW};
use crate::error::DropReason;
use crate::msg::ControlMsg;
use crate::sched::{prio_of, ChunkKind, Dir, Prio};
use crate::time::{seq_lt, Micros, CTL_ACK_DELAY, CTL_CRITICAL_DUP, CTL_RTO_MAX, CTL_RTO_MIN};
use crate::wire::{Ack, ControlFrag, ControlFragOwned};
use std::collections::VecDeque;

/// 窗口满了以后，发送方最多再积压这么多条（之后 send 返回 WindowFull）
pub const CTL_BACKLOG_MAX: usize = 256;

const WINDOW: usize = CTL_WINDOW as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CtlError {
    /// M2 只支持单片，超长返回它
    TooLarge,
    /// 32 条的窗口满了（并且积压队列也满了）
    WindowFull,
    /// 流号不对
    BadStream,
    /// 会话已关闭或还没确认
    NotReady,
}

/// 发送侧计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TxStats {
    /// 首次发出的消息数
    pub sent: u64,
    /// 关键消息的第二份
    pub dups: u64,
    /// RTO 超时重传次数
    pub retransmits: u64,
    /// 已确认的消息数
    pub acked: u64,
    /// 被丢掉的 ACK（流号不对、cum 超出已发范围）
    pub bad_acks: u64,
}

#[derive(Clone, Debug)]
struct TxMsg {
    seq: u16,
    body: Vec<u8>,
    critical: bool,
    acked: bool,
    /// 首次发出的时刻（None = 还没发过）
    first_sent: Option<Micros>,
    /// 下一次 RTO 重传的时刻
    next_due: Micros,
    /// 关键消息的第二份还没发
    dup_pending: bool,
    /// 已重传次数（指数退避用）
    tries: u32,
}

/// poll 要做的事
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TxAction {
    First,
    Dup,
    Retransmit,
}

/// 一个流的发送侧
#[derive(Clone, Debug)]
pub struct StreamTx {
    stream: u8,
    frag_max: usize,
    dir: Dir,
    next_seq: u16,
    /// 窗口内的消息，seq 从队首起连续（队首 = 最早的未确认消息）
    inflight: VecDeque<TxMsg>,
    /// 窗口外积压的 (消息体, 关键)
    backlog: VecDeque<(Vec<u8>, bool)>,
    last_ack: Option<Micros>,
    stats: TxStats,
}

impl StreamTx {
    /// frag_max：下行 1100，上行 240。方向按 frag_max 推断：≤ 240 当作上行（客户端），否则下行（网关）；
    /// 要明确指定用 [`StreamTx::with_dir`]
    pub fn new(stream: u8, frag_max: usize) -> Self {
        let dir = if frag_max <= CTL_FRAG_MAX_UP { Dir::Up } else { Dir::Down };
        Self::with_dir(stream, frag_max, dir)
    }

    pub fn with_dir(stream: u8, frag_max: usize, dir: Dir) -> Self {
        Self {
            stream,
            frag_max,
            dir,
            next_seq: CTL_SEQ_INIT,
            inflight: VecDeque::new(),
            backlog: VecDeque::new(),
            last_ack: None,
            stats: TxStats::default(),
        }
    }

    /// 测试用：从指定的 msg_seq 开始编号（检验回绕）
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_start_seq(mut self, seq: u16) -> Self {
        assert!(self.inflight.is_empty() && self.backlog.is_empty());
        self.next_seq = seq;
        self
    }

    pub fn stream(&self) -> u8 {
        self.stream
    }

    pub fn send(&mut self, body: Vec<u8>, critical: bool) -> Result<(), CtlError> {
        if body.len() > self.frag_max {
            return Err(CtlError::TooLarge);
        }
        if self.inflight.len() >= WINDOW && self.backlog.len() >= CTL_BACKLOG_MAX {
            return Err(CtlError::WindowFull);
        }
        self.backlog.push_back((body, critical));
        self.fill_window();
        Ok(())
    }

    fn fill_window(&mut self) {
        while self.inflight.len() < WINDOW {
            let Some((body, critical)) = self.backlog.pop_front() else { break };
            let seq = self.next_seq;
            self.next_seq = self.next_seq.wrapping_add(1);
            self.inflight.push_back(TxMsg {
                seq,
                body,
                critical,
                acked: false,
                first_sent: None,
                next_due: 0,
                dup_pending: false,
                tries: 0,
            });
        }
    }

    pub fn on_ack(&mut self, a: &Ack, now: Micros) {
        if a.stream != self.stream {
            self.stats.bad_acks += 1;
            return;
        }
        let Some(front) = self.inflight.front() else { return };
        let base = front.seq;
        // 只有发过的消息才能被确认
        let sent = self.inflight.iter().take_while(|m| m.first_sent.is_some()).count();
        let before_base = base.wrapping_sub(1);
        // cum 落在 [base − 1, base + sent − 1] 才有效；比 base − 1 早的是迟到的旧 ACK，只看 sack；
        // 超出已发范围的是坏 ACK，整个丢掉
        let cum_off = a.cum.wrapping_sub(before_base) as usize;
        let cum_ack = if cum_off <= sent {
            cum_off
        } else if seq_lt(a.cum, before_base) {
            0
        } else {
            self.stats.bad_acks += 1;
            return;
        };
        self.last_ack = Some(now);
        for m in self.inflight.iter_mut().take(cum_ack) {
            m.acked = true;
        }
        for i in 0..32u16 {
            if a.sack & (1u32 << i) == 0 {
                continue;
            }
            let s = a.cum.wrapping_add(1).wrapping_add(i);
            let off = s.wrapping_sub(base) as usize;
            if off < sent {
                self.inflight[off].acked = true;
            }
        }
        while self.inflight.front().is_some_and(|m| m.acked) {
            self.inflight.pop_front();
            self.stats.acked += 1;
        }
        self.fill_window();
    }

    /// 下一件要做的事：按 msg_seq 从旧到新，第一条到期的（新消息、关键消息的第二份、RTO 重传）
    fn next_action(&self, now: Micros) -> Option<(usize, TxAction)> {
        for (i, m) in self.inflight.iter().enumerate() {
            if m.acked {
                continue;
            }
            match m.first_sent {
                None => return Some((i, TxAction::First)),
                Some(t0) => {
                    if m.dup_pending && now >= t0 + CTL_CRITICAL_DUP {
                        return Some((i, TxAction::Dup));
                    }
                    if now >= m.next_due {
                        return Some((i, TxAction::Retransmit));
                    }
                }
            }
        }
        None
    }

    fn prio_for(&self, critical: bool) -> Prio {
        prio_of(self.dir, ChunkKind::Control { critical, stream: self.stream }).unwrap_or(Prio::P6)
    }

    /// 下一片要发的消息的优先级（不改状态）
    pub fn peek_prio(&self, now: Micros) -> Option<Prio> {
        self.next_action(now).map(|(i, _)| self.prio_for(self.inflight[i].critical))
    }

    /// 取下一片要发的（新消息、重传或关键消息的第二份）。rto 是当前的 RTO，会被钳到 [25 ms, 1 s]；
    /// 第 n 次重传等 min(rto × 2ⁿ, 1 s)
    pub fn poll(&mut self, now: Micros, rto: Micros) -> Option<(ControlFragOwned, Prio)> {
        let (i, act) = self.next_action(now)?;
        let rto = rto.clamp(CTL_RTO_MIN, CTL_RTO_MAX);
        let m = &mut self.inflight[i];
        match act {
            TxAction::First => {
                m.first_sent = Some(now);
                m.next_due = now + rto;
                m.dup_pending = m.critical;
                self.stats.sent += 1;
            }
            TxAction::Dup => {
                m.dup_pending = false;
                self.stats.dups += 1;
            }
            TxAction::Retransmit => {
                m.tries = m.tries.saturating_add(1);
                let backoff = (rto << m.tries.min(20)).min(CTL_RTO_MAX);
                m.next_due = now + backoff;
                // 重传出去之后，关键消息的第二份就不再单独补发
                m.dup_pending = false;
                self.stats.retransmits += 1;
            }
        }
        let frag = ControlFragOwned { stream: self.stream, msg_seq: m.seq, frag: 0, data: m.body.clone() };
        let critical = m.critical;
        Some((frag, self.prio_for(critical)))
    }

    /// 下一次要 poll 的时刻：有没发过的消息时返回 Some(0)（立即）；没有未确认的消息时 None
    pub fn next_deadline(&self) -> Option<Micros> {
        let mut best: Option<Micros> = None;
        for m in self.inflight.iter().filter(|m| !m.acked) {
            let t = match m.first_sent {
                None => return Some(0),
                Some(t0) if m.dup_pending => (t0 + CTL_CRITICAL_DUP).min(m.next_due),
                Some(_) => m.next_due,
            };
            best = Some(best.map_or(t, |b: Micros| b.min(t)));
        }
        best
    }

    /// 窗口里的消息数（含已被 sack 确认、但前面还有空洞的）
    pub fn in_flight(&self) -> usize {
        self.inflight.len()
    }

    pub fn backlog(&self) -> usize {
        self.backlog.len()
    }

    /// 全部发完并确认了
    pub fn is_idle(&self) -> bool {
        self.inflight.is_empty() && self.backlog.is_empty()
    }

    pub fn last_ack(&self) -> Option<Micros> {
        self.last_ack
    }

    pub fn stats(&self) -> TxStats {
        self.stats
    }
}

/// 接收侧计数
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct RxStats {
    /// 按序交付的消息数
    pub delivered: u64,
    /// 重复收到的（关键消息的第二份、重传）
    pub duplicates: u64,
    /// 超出窗口的
    pub out_of_window: u64,
    /// 发出的 ACK 数
    pub acks: u64,
}

/// 一个流的接收侧
#[derive(Clone, Debug)]
pub struct StreamRx {
    stream: u8,
    /// 下一条要交付的 msg_seq（cum = expected − 1）
    expected: u16,
    /// 乱序先到的消息，下标 = msg_seq mod 32（窗口正好 32 条，不会冲突）
    slots: [Option<Vec<u8>>; WINDOW],
    ready: VecDeque<Vec<u8>>,
    /// ACK 最晚什么时候发（None = 没有要回的）
    ack_due: Option<Micros>,
    stats: RxStats,
}

impl StreamRx {
    pub fn new(stream: u8) -> Self {
        Self {
            stream,
            expected: CTL_CUM_INIT.wrapping_add(1),
            slots: std::array::from_fn(|_| None),
            ready: VecDeque::new(),
            ack_due: None,
            stats: RxStats::default(),
        }
    }

    /// 测试用：期望从指定的 msg_seq 开始（检验回绕）
    #[cfg(any(test, feature = "testkit"))]
    pub fn with_start_seq(mut self, seq: u16) -> Self {
        self.expected = seq;
        self
    }

    pub fn stream(&self) -> u8 {
        self.stream
    }

    fn want_ack(&mut self, now: Micros) {
        let due = now + CTL_ACK_DELAY;
        self.ack_due = Some(self.ack_due.map_or(due, |d| d.min(due)));
    }

    /// 收到一片 CONTROL。重复的、超出窗口的返回对应的丢弃原因（重复的仍会安排回 ACK，
    /// 发送方可能没收到上一个 ACK）
    pub fn on_frag(&mut self, f: &ControlFrag, now: Micros) -> Result<(), DropReason> {
        if f.stream != self.stream {
            return Err(DropReason::CtlBadStream);
        }
        if f.frag != 0 {
            return Err(DropReason::CtlMultiFrag);
        }
        let off = f.msg_seq.wrapping_sub(self.expected);
        if off >= 0x8000 {
            // 已交付过的
            self.stats.duplicates += 1;
            self.want_ack(now);
            return Err(DropReason::CtlDuplicate);
        }
        if off as usize >= WINDOW {
            self.stats.out_of_window += 1;
            self.want_ack(now);
            return Err(DropReason::CtlOutOfWindow);
        }
        self.want_ack(now);
        let slot = f.msg_seq as usize % WINDOW;
        if self.slots[slot].is_some() {
            self.stats.duplicates += 1;
            return Err(DropReason::CtlDuplicate);
        }
        self.slots[slot] = Some(f.data.to_vec());
        while let Some(body) = self.slots[self.expected as usize % WINDOW].take() {
            self.ready.push_back(body);
            self.expected = self.expected.wrapping_add(1);
            self.stats.delivered += 1;
        }
        Ok(())
    }

    /// 按序取出已收齐的消息体
    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.ready.pop_front()
    }

    /// 有要回的 ACK、并且到了 2 ms 的期限（force 时不等）就给出 ACK，否则 None
    pub fn take_ack(&mut self, now: Micros, force: bool) -> Option<Ack> {
        let due = self.ack_due?;
        if !force && now < due {
            return None;
        }
        self.ack_due = None;
        self.stats.acks += 1;
        Some(self.current_ack())
    }

    /// 当前的确认状态（不管有没有要回的）
    pub fn current_ack(&self) -> Ack {
        let mut sack = 0u32;
        for i in 0..WINDOW {
            let s = self.expected.wrapping_add(i as u16);
            if self.slots[s as usize % WINDOW].is_some() {
                sack |= 1 << i;
            }
        }
        Ack { stream: self.stream, cum: self.expected.wrapping_sub(1), sack }
    }

    /// 要回的 ACK 的期限
    pub fn next_deadline(&self) -> Option<Micros> {
        self.ack_due
    }

    pub fn stats(&self) -> RxStats {
        self.stats
    }
}

/// 三个流的收发合在一起，外加 ControlMsg 的编解码。端点里每个会话一个
#[derive(Clone, Debug)]
pub struct ControlPlane {
    tx: [StreamTx; stream::COUNT as usize],
    rx: [StreamRx; stream::COUNT as usize],
}

impl ControlPlane {
    /// dir 是本端的发送方向：网关 Down（每片 ≤ 1100 B），客户端 Up（每片 ≤ 240 B）
    pub fn new(dir: Dir) -> Self {
        let frag_max = match dir {
            Dir::Down => CTL_FRAG_MAX_DOWN,
            Dir::Up => CTL_FRAG_MAX_UP,
        };
        Self {
            tx: std::array::from_fn(|s| StreamTx::with_dir(s as u8, frag_max, dir)),
            rx: std::array::from_fn(|s| StreamRx::new(s as u8)),
        }
    }

    /// 编码并排进对应的流（流号、是否关键按消息类型定）
    pub fn send(&mut self, m: &ControlMsg) -> Result<(), CtlError> {
        self.send_raw(m.stream(), m.encode(), m.critical())
    }

    pub fn send_raw(&mut self, stream: u8, body: Vec<u8>, critical: bool) -> Result<(), CtlError> {
        self.tx.get_mut(stream as usize).ok_or(CtlError::BadStream)?.send(body, critical)
    }

    pub fn on_frag(&mut self, f: &ControlFrag, now: Micros) -> Result<(), DropReason> {
        self.rx.get_mut(f.stream as usize).ok_or(DropReason::CtlBadStream)?.on_frag(f, now)
    }

    pub fn on_ack(&mut self, a: &Ack, now: Micros) -> Result<(), DropReason> {
        self.tx.get_mut(a.stream as usize).ok_or(DropReason::CtlBadStream)?.on_ack(a, now);
        Ok(())
    }

    /// 按优先级取下一片：先比优先级，同级时流号小的先
    pub fn poll(&mut self, now: Micros, rto: Micros) -> Option<(ControlFragOwned, Prio)> {
        let best = (0..self.tx.len()).filter_map(|s| self.tx[s].peek_prio(now).map(|p| (p, s))).min()?;
        self.tx[best.1].poll(now, rto)
    }

    /// 取一个到期（force 时不等期限）的 ACK；多个流都有时一次给一个，按流号
    pub fn take_ack(&mut self, now: Micros, force: bool) -> Option<Ack> {
        self.rx.iter_mut().find_map(|r| r.take_ack(now, force))
    }

    /// 按序取出下一条消息并解码。解不开的返回 Err(CtlBadMsg)；不认识的 msg_type 照常返回 Unknown，
    /// 由调用方计 CtlUnknownMsg
    pub fn pop(&mut self) -> Option<Result<ControlMsg, DropReason>> {
        self.rx
            .iter_mut()
            .find_map(|r| r.pop())
            .map(|b| ControlMsg::decode(&b).map_err(|_| DropReason::CtlBadMsg))
    }

    /// 发送和 ACK 两边最早的期限
    pub fn next_deadline(&self) -> Option<Micros> {
        self.tx
            .iter()
            .filter_map(|t| t.next_deadline())
            .chain(self.rx.iter().filter_map(|r| r.next_deadline()))
            .min()
    }

    pub fn tx(&self, stream: u8) -> Option<&StreamTx> {
        self.tx.get(stream as usize)
    }

    pub fn rx(&self, stream: u8) -> Option<&StreamRx> {
        self.rx.get(stream as usize)
    }

    /// 所有流都发完并确认了
    pub fn is_idle(&self) -> bool {
        self.tx.iter().all(|t| t.is_idle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::ms;

    fn view(f: &ControlFragOwned) -> ControlFrag<'_> {
        ControlFrag { stream: f.stream, msg_seq: f.msg_seq, frag: f.frag, data: &f.data }
    }

    #[test]
    fn single_message_roundtrip() {
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_DOWN);
        let mut rx = StreamRx::new(0);
        tx.send(b"hello".to_vec(), false).unwrap();
        assert_eq!(tx.next_deadline(), Some(0));
        let (f, p) = tx.poll(0, ms(25)).unwrap();
        assert_eq!((f.msg_seq, f.frag, p), (0, 0, Prio::P6));
        assert!(tx.poll(0, ms(25)).is_none());
        rx.on_frag(&view(&f), 10_000).unwrap();
        assert_eq!(rx.pop().unwrap(), b"hello");
        // ACK 在 2 ms 内
        assert_eq!(rx.next_deadline(), Some(12_000));
        assert!(rx.take_ack(11_000, false).is_none());
        let a = rx.take_ack(12_000, false).unwrap();
        assert_eq!((a.cum, a.sack), (0, 0));
        tx.on_ack(&a, 20_000);
        assert!(tx.is_idle());
        assert_eq!(tx.next_deadline(), None);
    }

    #[test]
    fn initial_ack_state() {
        let rx = StreamRx::new(1);
        let a = rx.current_ack();
        assert_eq!((a.stream, a.cum, a.sack), (1, 0xFFFF, 0));
    }

    #[test]
    fn too_large_and_window_backlog() {
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_UP);
        assert_eq!(tx.send(vec![0; 241], false), Err(CtlError::TooLarge));
        for _ in 0..(WINDOW + CTL_BACKLOG_MAX) {
            tx.send(vec![1], false).unwrap();
        }
        assert_eq!(tx.in_flight(), WINDOW);
        assert_eq!(tx.backlog(), CTL_BACKLOG_MAX);
        assert_eq!(tx.send(vec![1], false), Err(CtlError::WindowFull));
        // 窗口里最多发 32 条，第 33 条要等确认
        let mut n = 0;
        while tx.poll(0, ms(25)).is_some() {
            n += 1;
        }
        assert_eq!(n, WINDOW);
        tx.on_ack(&Ack { stream: 0, cum: 9, sack: 0 }, 1);
        assert_eq!(tx.in_flight(), WINDOW);
        let (f, _) = tx.poll(1, ms(25)).unwrap();
        assert_eq!(f.msg_seq, 32);
    }

    #[test]
    fn critical_dup_at_5ms_unless_acked() {
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_DOWN);
        tx.send(vec![7], true).unwrap();
        let (f, p) = tx.poll(0, ms(25)).unwrap();
        assert_eq!(p, Prio::P1);
        assert!(tx.poll(4_999, ms(25)).is_none());
        assert_eq!(tx.next_deadline(), Some(5_000));
        let (f2, _) = tx.poll(5_000, ms(25)).unwrap();
        assert_eq!(f, f2);
        assert_eq!(tx.stats().dups, 1);
        assert_eq!(tx.next_deadline(), Some(25_000));

        // 5 ms 之前就确认了，就不再发第二份
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_UP);
        tx.send(vec![7], true).unwrap();
        let (_, p) = tx.poll(0, ms(25)).unwrap();
        assert_eq!(p, Prio::P2);
        tx.on_ack(&Ack { stream: 0, cum: 0, sack: 0 }, 2_000);
        assert!(tx.poll(5_000, ms(25)).is_none());
        assert_eq!(tx.stats().dups, 0);
    }

    #[test]
    fn rto_backoff_doubles_and_caps() {
        let mut tx = StreamTx::new(1, CTL_FRAG_MAX_DOWN);
        tx.send(vec![1], false).unwrap();
        let rto = ms(40);
        tx.poll(0, rto).unwrap();
        let mut t = 0;
        let mut gaps = Vec::new();
        for _ in 0..8 {
            let due = tx.next_deadline().unwrap();
            assert!(tx.poll(due - 1, rto).is_none());
            tx.poll(due, rto).unwrap();
            gaps.push(due - t);
            t = due;
        }
        assert_eq!(gaps, vec![ms(40), ms(80), ms(160), ms(320), ms(640), ms(1000), ms(1000), ms(1000)]);
        assert_eq!(tx.stats().retransmits, 8);
        // RTO 低于 25 ms 时按 25 ms
        let mut tx = StreamTx::new(1, CTL_FRAG_MAX_DOWN);
        tx.send(vec![1], false).unwrap();
        tx.poll(0, ms(1)).unwrap();
        assert_eq!(tx.next_deadline(), Some(ms(25)));
    }

    #[test]
    fn out_of_order_buffered_and_delivered_in_order() {
        let mut rx = StreamRx::new(0);
        let frags: Vec<ControlFragOwned> =
            (0..5u16).map(|s| ControlFragOwned { stream: 0, msg_seq: s, frag: 0, data: vec![s as u8] }).collect();
        for i in [3, 1, 4] {
            rx.on_frag(&view(&frags[i]), 0).unwrap();
        }
        assert!(rx.pop().is_none());
        let a = rx.take_ack(0, true).unwrap();
        // cum = 0xFFFF，sack 位 i ↔ msg_seq = cum + 1 + i：1、3、4
        assert_eq!(a.cum, 0xFFFF);
        assert_eq!(a.sack, 0b11010);
        rx.on_frag(&view(&frags[0]), 0).unwrap();
        assert_eq!(rx.pop().unwrap(), vec![0]);
        assert_eq!(rx.pop().unwrap(), vec![1]);
        assert!(rx.pop().is_none());
        rx.on_frag(&view(&frags[2]), 0).unwrap();
        let got: Vec<_> = std::iter::from_fn(|| rx.pop()).collect();
        assert_eq!(got, vec![vec![2], vec![3], vec![4]]);
        // 重复的计数，并且会再回 ACK
        assert_eq!(rx.on_frag(&view(&frags[1]), 100), Err(DropReason::CtlDuplicate));
        assert_eq!(rx.take_ack(100, true).unwrap().cum, 4);
    }

    #[test]
    fn sack_acks_holes_and_skips_them() {
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_DOWN);
        for i in 0..4u8 {
            tx.send(vec![i], false).unwrap();
        }
        for _ in 0..4 {
            tx.poll(0, ms(25)).unwrap();
        }
        // 0 丢了，1、2、3 到了：cum = 0xFFFF，sack 位 1、2、3
        tx.on_ack(&Ack { stream: 0, cum: 0xFFFF, sack: 0b1110 }, 10);
        assert_eq!(tx.in_flight(), 4);
        // RTO 到了只重传 0
        let (f, _) = tx.poll(ms(25), ms(25)).unwrap();
        assert_eq!(f.msg_seq, 0);
        assert!(tx.poll(ms(25), ms(25)).is_none());
        tx.on_ack(&Ack { stream: 0, cum: 3, sack: 0 }, ms(30));
        assert!(tx.is_idle());
    }

    #[test]
    fn bogus_and_stale_acks() {
        let mut tx = StreamTx::new(0, CTL_FRAG_MAX_DOWN);
        for _ in 0..3 {
            tx.send(vec![0], false).unwrap();
        }
        tx.poll(0, ms(25)).unwrap();
        tx.poll(0, ms(25)).unwrap();
        // 只发了 0、1；cum = 2 超出已发范围，整个丢掉
        tx.on_ack(&Ack { stream: 0, cum: 2, sack: 0 }, 1);
        assert_eq!(tx.stats().bad_acks, 1);
        assert_eq!(tx.in_flight(), 3);
        // 流号不对
        tx.on_ack(&Ack { stream: 1, cum: 0, sack: 0 }, 1);
        assert_eq!(tx.stats().bad_acks, 2);
        // 确认 0，再来一个迟到的旧 ACK（cum = 0xFFFF、sack 位 1 = msg 1）
        tx.on_ack(&Ack { stream: 0, cum: 0, sack: 0 }, 2);
        assert_eq!(tx.in_flight(), 2);
        tx.on_ack(&Ack { stream: 0, cum: 0xFFFF, sack: 0b10 }, 3);
        assert_eq!(tx.in_flight(), 1);
        assert_eq!(tx.stats().acked, 2);
    }

    #[test]
    fn seq_wraps_across_0xffff() {
        let start = 0xFFF0;
        let mut tx = StreamTx::new(1, CTL_FRAG_MAX_DOWN).with_start_seq(start);
        let mut rx = StreamRx::new(1).with_start_seq(start);
        let mut t = 0;
        for i in 0..100u32 {
            tx.send(i.to_be_bytes().to_vec(), false).unwrap();
        }
        let mut got = Vec::new();
        let mut n = 0u32;
        while !tx.is_idle() {
            // 每 3 片丢 1 片，靠 RTO 补
            while let Some((f, _)) = tx.poll(t, ms(25)) {
                if !n.is_multiple_of(3) {
                    let _ = rx.on_frag(&view(&f), t);
                }
                n += 1;
            }
            while let Some(b) = rx.pop() {
                got.push(u32::from_be_bytes(b.try_into().unwrap()));
            }
            if let Some(a) = rx.take_ack(t, true) {
                tx.on_ack(&a, t);
            }
            t += ms(30);
            assert!(t < ms(60_000), "没有收敛");
        }
        assert_eq!(got, (0..100).collect::<Vec<_>>());
        assert_eq!(rx.current_ack().cum, start.wrapping_add(99));
    }

    #[test]
    fn plane_routes_streams_and_priorities() {
        let mut gw = ControlPlane::new(Dir::Down);
        let mut cl = ControlPlane::new(Dir::Up);
        gw.send(&ControlMsg::AudioConfig(Default::default())).unwrap(); // 流 1、普通 → P6
        gw.send(&ControlMsg::Error { code: 1, text: "x".into() }).unwrap(); // 流 0、普通 → P6
        gw.send(&ControlMsg::VideoConfig(Default::default())).unwrap(); // 流 1、关键 → P1
        let order: Vec<(u8, Prio)> =
            std::iter::from_fn(|| gw.poll(0, ms(25))).map(|(f, p)| (f.stream, p)).collect();
        // 流 1 内部按序：AUDIO_CONFIG 在 VIDEO_CONFIG 前面，所以流 1 先给出的是 P6 的 AUDIO_CONFIG；
        // 同级时流号小的先
        assert_eq!(order, vec![(0, Prio::P6), (1, Prio::P6), (1, Prio::P1)]);

        assert_eq!(cl.send_raw(3, vec![], false), Err(CtlError::BadStream));
        let big = ControlMsg::Error { code: 1, text: "x".repeat(300) };
        assert_eq!(cl.send(&big), Err(CtlError::TooLarge));
        cl.send(&ControlMsg::IdrRequest { reason: 1 }).unwrap();
        let (f, p) = cl.poll(0, ms(25)).unwrap();
        assert_eq!((f.stream, p), (0, Prio::P2));
        gw.on_frag(&view(&f), 0).unwrap();
        assert_eq!(gw.pop().unwrap().unwrap(), ControlMsg::IdrRequest { reason: 1 });
        // 网关这边：流 0 的 ACK 在 2 ms 时到期，比各条消息的 RTO 和关键消息的第二份都早
        assert_eq!(gw.next_deadline(), Some(ms(2)));
        let a = gw.take_ack(ms(2), false).unwrap();
        cl.on_ack(&a, ms(3)).unwrap();
        // 关键消息的第二份还没到时间，但消息已确认
        assert!(cl.is_idle());
    }
}
