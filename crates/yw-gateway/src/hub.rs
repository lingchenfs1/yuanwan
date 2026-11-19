//! Hub：PS5 会话（chiaki）和 YWP 服务循环之间的共享点（M2a）。
//!
//! - PsLink 保存当前的 PS5 会话（`Box<dyn PsSession>`）和 IdrGovernor；
//! - Hub 实现 yw_serve::PsControl，ywp 线程经它调 PS5 会话；
//! - 参数集、音频头、码流信息按 epoch 存「最新值槽」：chiaki 只在会话开始时交一次参数集和音频头，
//!   通知（MediaMsg）丢了也能从槽里补取；
//! - last_key_us：最近一次收到关键帧的时刻（chiaki 回调在关键帧到达时写，不拿 PsLink 的锁），
//!   监督线程的 idr_tick 拿它判断 IDR 请求有没有等到；
//! - 控制口 status 的 YWP 摘要由 yw-serve 自己维护（ServeHandle::status），不放在这里。
//!
//! 锁的规则：
//! - 锁序固定为 `PsLink.idr → PsLink.session → chiaki 内部锁`，不能反过来；
//! - chiaki 的回调里不拿 PsLink 的任何锁，只拿 Hub.latest 这一把短锁，而且只做拷贝；
//! - 监督线程结束会话时先 `take()` 把会话取出来，再在锁外 `stop()` 和 drop（drop 会等 chiaki 的线程退出）。
//!
//! 建会话的顺序：`Session::new`（还没开始连）→ `install` 得到 epoch → 告诉回调这个 epoch → `start`。

use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard};
use yw_chiaki_sys::{Controller, Session};
use yw_serve::idr::IdrStats;
use yw_serve::{AudioHeader, IdrDecision, IdrGovernor, IdrReason, PsControl, StreamInfo};
use ywp::time::Micros;
use ywp::wire::InputState;

/// 一个 PS5 会话。只要求 Send：yw_chiaki_sys::Session 只有 Send（chiaki 的会话接口内部有锁）
pub trait PsSession: Send {
    fn start(&self) -> Result<(), String>;
    fn stop(&mut self);
    fn set_controller_full(&self, c: &InputState);
    fn set_controller_idle(&self);
    fn request_idr(&self) -> Result<(), String>;
    fn goto_bed(&self) -> Result<(), String>;
    /// 控制口 inject：上报给 PS5 的丢包率（-1 恢复）
    fn set_inject_loss(&self, loss: f64);
}

/// YWP 的 STATE（规格 §5.5.3）→ 外壳的手柄状态。按键位表两边相同（0 × … 15 PS），摇杆都是 i16。
/// 触摸和体感要等外壳的 set_controller_full（M2b），现在只传按键、扳机和摇杆
fn to_controller(c: &InputState) -> Controller {
    Controller {
        buttons: c.buttons & 0xFFFF,
        l2: c.l2,
        r2: c.r2,
        lx: c.sticks[0],
        ly: c.sticks[1],
        rx: c.sticks[2],
        ry: c.sticks[3],
    }
}

impl PsSession for Session {
    fn start(&self) -> Result<(), String> {
        Session::start(self)
    }
    fn stop(&mut self) {
        Session::stop(self)
    }
    fn set_controller_full(&self, c: &InputState) {
        self.set_controller(&to_controller(c))
    }
    fn set_controller_idle(&self) {
        Session::set_controller_idle(self)
    }
    fn request_idr(&self) -> Result<(), String> {
        Session::request_idr(self)
    }
    fn goto_bed(&self) -> Result<(), String> {
        Session::goto_bed(self)
    }
    fn set_inject_loss(&self, loss: f64) {
        Session::set_inject_loss(self, loss)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 网关每次启动时取的 16 位随机数（session_epoch 的高 16 位，规格 §4.5）
pub fn random_boot_id() -> u16 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (RandomState::new().hash_one(t) & 0xFFFF) as u16
}

/// 当前的 PS5 会话和 IDR 限频
pub struct PsLink {
    idr: Mutex<IdrGovernor>,
    session: Mutex<Option<Box<dyn PsSession>>>,
    boot_id: u16,
    /// 本次启动以来装过几个会话
    installs: AtomicU32,
    /// 当前会话的 epoch（0 表示还没有过会话）
    epoch: AtomicU32,
    /// PS5 会话的 request_idr 返回错误的次数
    idr_errors: AtomicU64,
}

impl PsLink {
    pub fn new(boot_id: u16) -> Self {
        Self {
            idr: Mutex::new(IdrGovernor::new()),
            session: Mutex::new(None),
            boot_id,
            installs: AtomicU32::new(0),
            epoch: AtomicU32::new(0),
            idr_errors: AtomicU64::new(0),
        }
    }

    /// 持锁时间 = 一次 chiaki 调用。没有会话时返回 None
    pub fn with<R>(&self, f: impl FnOnce(&dyn PsSession) -> R) -> Option<R> {
        lock(&self.session).as_deref().map(f)
    }

    /// 装上新会话，返回新 epoch = (boot_id << 16) | 本次启动以来的会话序号。
    /// 未决的 IDR 请求作废（新会话从关键帧开始）。槽里如果还有旧会话（调用方忘了 take），在锁外结束它
    pub fn install(&self, s: Box<dyn PsSession>) -> u32 {
        let old = {
            let mut idr = lock(&self.idr);
            let mut slot = lock(&self.session);
            idr.reset();
            let n = self.installs.fetch_add(1, Relaxed).wrapping_add(1);
            let epoch = ((self.boot_id as u32) << 16) | (n & 0xFFFF);
            self.epoch.store(epoch, Relaxed);
            slot.replace(s)
        };
        if let Some(mut o) = old {
            o.stop();
        }
        self.epoch.load(Relaxed)
    }

    /// 取出会话，调用方在锁外 stop / drop
    pub fn take(&self) -> Option<Box<dyn PsSession>> {
        let mut idr = lock(&self.idr);
        let s = lock(&self.session).take();
        idr.reset();
        s
    }

    pub fn has_session(&self) -> bool {
        lock(&self.session).is_some()
    }

    /// 当前（或最近一个）会话的 epoch；还没有过会话时为 None
    pub fn epoch(&self) -> Option<u32> {
        Some(self.epoch.load(Relaxed)).filter(|&e| e != 0)
    }

    /// 经 IdrGovernor 限频后向 PS5 请求 IDR。没有会话时不记账（下一个会话本来就从关键帧开始），返回 Coalesced
    pub fn request_idr(&self, now: Micros, why: IdrReason) -> IdrDecision {
        let mut idr = lock(&self.idr);
        if !self.has_session() {
            return IdrDecision::Coalesced;
        }
        let d = idr.request(now, why);
        if d == IdrDecision::Requested {
            self.send_idr();
        }
        d
    }

    /// 监督线程每拍调：1.2 s 重发、5 s Escalate
    pub fn idr_tick(&self, now: Micros, last_key_us: Micros) -> Option<IdrDecision> {
        let mut idr = lock(&self.idr);
        let d = idr.tick(now, last_key_us);
        if d == Some(IdrDecision::Requested) {
            self.send_idr();
        }
        d
    }

    pub fn idr_stats(&self) -> IdrStats {
        lock(&self.idr).stats()
    }

    pub fn idr_errors(&self) -> u64 {
        self.idr_errors.load(Relaxed)
    }

    /// 调用方已持有 idr 锁（锁序 idr → session）
    fn send_idr(&self) {
        if let Some(Err(_)) = self.with(|s| s.request_idr()) {
            self.idr_errors.fetch_add(1, Relaxed);
        }
    }
}

/// 按 epoch 保存的最新值。只存当前 epoch 的；换 epoch 时其余项一起清掉
#[derive(Clone, Debug, Default)]
pub struct LatestSlots {
    pub epoch: u32,
    pub params: Option<Arc<[u8]>>,
    pub audio: Option<AudioHeader>,
    pub stream: Option<StreamInfo>,
}

impl LatestSlots {
    fn for_epoch(&mut self, epoch: u32) -> &mut Self {
        if self.epoch != epoch {
            *self = LatestSlots { epoch, ..LatestSlots::default() };
        }
        self
    }
}

/// PS5 会话的状态（监督线程写）。M2a 里 ywp 线程经 MediaMsg::PsChanged 拿到状态，这里只给控制口 status 读；
/// M2d 的 SESSION_STATUS（说明文字、已持续时长、pause_kind）从这里取，字段到时再补
#[derive(Clone, Debug, Default)]
pub struct PsStatus {
    /// ywp::consts::ps_state
    pub ps_state: u8,
    pub epoch: u32,
    /// 进入这个状态的时刻
    pub since_us: Micros,
    /// 说明文字（M2d 填进 SESSION_STATUS 的 TLV 4）
    #[allow(dead_code)]
    pub text: String,
}

impl Hub {
    /// 当前状态的拷贝
    pub fn status(&self) -> PsStatus {
        lock(&self.status).clone()
    }
}

pub struct Hub {
    pub link: PsLink,
    pub status: Mutex<PsStatus>,
    pub status_ver: AtomicU64,
    pub latest: Mutex<LatestSlots>,
    /// 最近一次收到关键帧的时刻（yw_serve::clock 的时钟），0 表示还没有
    pub last_key_us: AtomicU64,
}

impl Hub {
    pub fn new(boot_id: u16) -> Self {
        Self {
            link: PsLink::new(boot_id),
            status: Mutex::new(PsStatus::default()),
            status_ver: AtomicU64::new(0),
            latest: Mutex::new(LatestSlots::default()),
            last_key_us: AtomicU64::new(0),
        }
    }

    /// chiaki 回调里调：先在锁外拷贝，锁里只换指针
    pub fn set_params(&self, epoch: u32, data: &[u8]) {
        let a: Arc<[u8]> = Arc::from(data);
        lock(&self.latest).for_epoch(epoch).params = Some(a);
    }

    pub fn set_audio_header(&self, epoch: u32, h: AudioHeader) {
        lock(&self.latest).for_epoch(epoch).audio = Some(h);
    }

    /// 监督线程建会话时调（编码、帧率、分辨率来自建会话的参数）
    pub fn set_stream_info(&self, epoch: u32, s: StreamInfo) {
        lock(&self.latest).for_epoch(epoch).stream = Some(s);
    }

    /// 收到关键帧（只增不减）
    pub fn note_keyframe(&self, now: Micros) {
        self.last_key_us.fetch_max(now, Relaxed);
    }

    /// 监督线程每拍调
    pub fn idr_tick(&self, now: Micros) -> Option<IdrDecision> {
        self.link.idr_tick(now, self.last_key_us.load(Relaxed))
    }

    pub fn set_status(&self, s: PsStatus) {
        *lock(&self.status) = s;
        self.status_ver.fetch_add(1, Relaxed);
    }
}

impl PsControl for Hub {
    fn request_idr(&self, now: Micros, why: IdrReason) -> IdrDecision {
        self.link.request_idr(now, why)
    }

    fn set_controller_full(&self, c: &InputState) {
        self.link.with(|s| s.set_controller_full(c));
    }

    fn set_controller_idle(&self) {
        self.link.with(|s| s.set_controller_idle());
    }

    fn params(&self, epoch: u32) -> Option<Arc<[u8]>> {
        let l = lock(&self.latest);
        if l.epoch == epoch {
            l.params.clone()
        } else {
            None
        }
    }

    fn audio_header(&self, epoch: u32) -> Option<AudioHeader> {
        let l = lock(&self.latest);
        if l.epoch == epoch {
            l.audio
        } else {
            None
        }
    }

    fn stream_info(&self, epoch: u32) -> Option<StreamInfo> {
        let l = lock(&self.latest);
        if l.epoch == epoch {
            l.stream
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ywp::time::ms;

    type Calls = Arc<Mutex<Vec<String>>>;

    struct FakeSession {
        calls: Calls,
        fail_idr: bool,
    }

    impl FakeSession {
        fn boxed(calls: &Calls) -> Box<dyn PsSession> {
            Box::new(FakeSession { calls: calls.clone(), fail_idr: false })
        }
        fn log(&self, s: &str) {
            self.calls.lock().unwrap().push(s.to_string());
        }
    }

    impl PsSession for FakeSession {
        fn start(&self) -> Result<(), String> {
            self.log("start");
            Ok(())
        }
        fn stop(&mut self) {
            self.log("stop");
        }
        fn set_controller_full(&self, c: &InputState) {
            self.log(&format!("full:{:x}", c.buttons));
        }
        fn set_controller_idle(&self) {
            self.log("idle");
        }
        fn request_idr(&self) -> Result<(), String> {
            self.log("idr");
            if self.fail_idr {
                Err("x".into())
            } else {
                Ok(())
            }
        }
        fn goto_bed(&self) -> Result<(), String> {
            self.log("bed");
            Ok(())
        }
        fn set_inject_loss(&self, loss: f64) {
            self.log(&format!("inject:{loss}"));
        }
    }

    fn state(buttons: u32, l2: u8, r2: u8, sticks: [i16; 4]) -> InputState {
        use ywp::wire::Touch;
        InputState { buttons, l2, r2, sticks, touch: [Touch::NONE; 2], gyro: [0; 3], accel: [0; 3], orient: 0, flags: 0 }
    }

    fn calls(c: &Calls) -> Vec<String> {
        c.lock().unwrap().clone()
    }

    #[test]
    fn epochs_carry_boot_id_and_count_up() {
        let link = PsLink::new(0xABCD);
        assert_eq!(link.epoch(), None);
        let c = Calls::default();
        assert_eq!(link.install(FakeSession::boxed(&c)), 0xABCD_0001);
        let s = link.take().expect("有会话");
        drop(s);
        assert_eq!(link.install(FakeSession::boxed(&c)), 0xABCD_0002);
        assert_eq!(link.epoch(), Some(0xABCD_0002));
    }

    #[test]
    fn install_over_existing_stops_old_outside_lock() {
        let link = PsLink::new(1);
        let c1 = Calls::default();
        let c2 = Calls::default();
        link.install(FakeSession::boxed(&c1));
        link.install(FakeSession::boxed(&c2));
        assert_eq!(calls(&c1), ["stop"]);
        assert!(calls(&c2).is_empty());
    }

    #[test]
    fn idr_goes_through_governor() {
        let hub = Hub::new(1);
        // 没有会话：不记账
        assert_eq!(hub.request_idr(ms(1_000), IdrReason::NoFrames), IdrDecision::Coalesced);
        assert_eq!(hub.link.idr_stats().sent, 0);
        let c = Calls::default();
        hub.link.install(FakeSession::boxed(&c));
        assert_eq!(hub.request_idr(ms(2_000), IdrReason::NoFrames), IdrDecision::Requested);
        assert_eq!(hub.request_idr(ms(2_100), IdrReason::DecoderError), IdrDecision::Coalesced);
        assert_eq!(calls(&c), ["idr"]);
        // 1.2 s 没等到关键帧：监督线程的 tick 重发
        assert_eq!(hub.idr_tick(ms(3_100)), None);
        assert_eq!(hub.idr_tick(ms(3_200)), Some(IdrDecision::Requested));
        assert_eq!(calls(&c), ["idr", "idr"]);
        // 关键帧来了：未决结束，不再重发
        hub.note_keyframe(ms(3_500));
        hub.note_keyframe(ms(3_400));
        assert_eq!(hub.last_key_us.load(Relaxed), ms(3_500), "只增不减");
        assert_eq!(hub.idr_tick(ms(3_600)), None);
        assert_eq!(hub.idr_tick(ms(10_000)), None);
        assert_eq!(calls(&c).len(), 2);
        assert_eq!(hub.link.idr_stats().satisfied, 1);
    }

    #[test]
    fn take_drops_pending_idr() {
        let hub = Hub::new(1);
        let c = Calls::default();
        hub.link.install(FakeSession::boxed(&c));
        hub.request_idr(ms(1_000), IdrReason::Watchdog);
        let mut s = hub.link.take().unwrap();
        s.stop();
        assert!(!hub.link.has_session());
        assert_eq!(hub.idr_tick(ms(9_000)), None, "取走会话后不再重发、不报 Escalate");
        assert_eq!(calls(&c), ["idr", "stop"]);
    }

    #[test]
    fn idr_errors_counted() {
        let link = PsLink::new(1);
        let c = Calls::default();
        link.install(Box::new(FakeSession { calls: c.clone(), fail_idr: true }));
        assert_eq!(link.request_idr(ms(1_000), IdrReason::NoFrames), IdrDecision::Requested);
        assert_eq!(link.idr_errors(), 1);
    }

    #[test]
    fn controller_calls_reach_session() {
        let hub: Arc<Hub> = Arc::new(Hub::new(1));
        let ctl: Arc<dyn PsControl> = hub.clone();
        let st = state(0x8001, 0, 0, [0; 4]);
        ctl.set_controller_full(&st); // 没有会话：什么都不做
        let c = Calls::default();
        hub.link.install(FakeSession::boxed(&c));
        ctl.set_controller_full(&st);
        ctl.set_controller_idle();
        assert_eq!(calls(&c), ["full:8001", "idle"]);
    }

    #[test]
    fn controller_mapping_keeps_button_bits() {
        let st = state(0xFFFF_FFFF, 7, 9, [1, -2, 3, -4]);
        let c = to_controller(&st);
        assert_eq!(c.buttons, 0xFFFF, "16~31 位不传");
        assert_eq!((c.l2, c.r2, c.lx, c.ly, c.rx, c.ry), (7, 9, 1, -2, 3, -4));
        assert_eq!(yw_chiaki_sys::buttons::PS, 1 << 15);
        assert_eq!(yw_chiaki_sys::buttons::CROSS, 1 << 0);
    }

    #[test]
    fn latest_slots_by_epoch() {
        let hub = Hub::new(1);
        let ah = AudioHeader { channels: 2, bits: 16, rate: 48000, frame_size: 480 };
        hub.set_params(5, &[1, 2, 3]);
        hub.set_audio_header(5, ah);
        hub.set_stream_info(5, StreamInfo { codec: 2, fps: 60, width: 1920, height: 1080 });
        assert_eq!(hub.params(5).as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(hub.audio_header(5), Some(ah));
        assert_eq!(hub.stream_info(5).map(|s| s.height), Some(1080));
        assert_eq!(hub.params(4), None, "旧 epoch 不给");
        // 新会话：旧的值一起清掉
        hub.set_params(6, &[9]);
        assert_eq!(hub.params(5), None);
        assert_eq!(hub.audio_header(6), None);
        assert_eq!(hub.stream_info(6), None);
        assert_eq!(hub.params(6).as_deref(), Some(&[9u8][..]));
    }

    #[test]
    fn status_version_bumps() {
        let hub = Hub::new(1);
        hub.set_status(PsStatus { ps_state: 4, epoch: 7, since_us: 1, text: "串流中".into() });
        assert_eq!(hub.status_ver.load(Relaxed), 1);
        assert_eq!(lock(&hub.status).ps_state, 4);
    }

    #[test]
    fn boot_id_varies() {
        // 不是严格的随机性测试：连取 8 次至少有两个不同
        let v: Vec<u16> = (0..8).map(|_| random_boot_id()).collect();
        assert!(v.iter().any(|&x| x != v[0]));
    }
}
