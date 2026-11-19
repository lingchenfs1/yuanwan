//! libchiaki 外壳（`shim/yw_shim.c`）的 Rust 绑定。
//!
//! 链接了 chiaki-ng（AGPL-3.0-only + OpenSSL 例外），所以本 crate 和用到它的网关都按 AGPL 发布。
//! 客户端不要依赖本 crate（见 PLAN §7 许可证隔离）。

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

/// 手柄按键位，与 chiaki 的 `CHIAKI_CONTROLLER_BUTTON_*` 一致。
pub mod buttons {
    pub const CROSS: u32 = 1 << 0;
    pub const CIRCLE: u32 = 1 << 1;
    pub const SQUARE: u32 = 1 << 2;
    pub const TRIANGLE: u32 = 1 << 3;
    pub const DPAD_LEFT: u32 = 1 << 4;
    pub const DPAD_RIGHT: u32 = 1 << 5;
    pub const DPAD_UP: u32 = 1 << 6;
    pub const DPAD_DOWN: u32 = 1 << 7;
    pub const L1: u32 = 1 << 8;
    pub const R1: u32 = 1 << 9;
    pub const L3: u32 = 1 << 10;
    pub const R3: u32 = 1 << 11;
    pub const OPTIONS: u32 = 1 << 12;
    pub const CREATE: u32 = 1 << 13;
    pub const TOUCHPAD: u32 = 1 << 14;
    pub const PS: u32 = 1 << 15;
}

/// 日志级别位，与 chiaki 一致。
pub mod log_level {
    pub const ERROR: u32 = 1 << 0;
    pub const WARNING: u32 = 1 << 1;
    pub const INFO: u32 = 1 << 2;
    pub const VERBOSE: u32 = 1 << 3;
    pub const DEBUG: u32 = 1 << 4;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Controller {
    pub buttons: u32,
    pub l2: u8,
    pub r2: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

#[repr(C)]
struct RawSession {
    _private: [u8; 0],
}

type LogFn = extern "C" fn(*mut c_void, c_int, *const c_char);

#[repr(C)]
struct RawCallbacks {
    user: *mut c_void,
    on_log: Option<LogFn>,
    on_event: Option<extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    on_video: Option<extern "C" fn(*mut c_void, *const u8, usize, i32, c_int) -> c_int>,
    on_audio_header: Option<extern "C" fn(*mut c_void, u32, u32, u32, u32)>,
    on_audio: Option<extern "C" fn(*mut c_void, *const u8, usize)>,
    on_haptics: Option<extern "C" fn(*mut c_void, *const u8, usize)>,
}

#[repr(C)]
struct RawParams {
    host: *const c_char,
    regist_key: [u8; 16],
    morning: [u8; 16],
    height: u32,
    fps: u32,
    bitrate_kbps: u32,
    codec: c_int,
    packet_loss_max: f64,
    ls_mtu: u32,
    ls_rtt_ms: u32,
    console_pin: u32,
    log_mask: u32,
}

extern "C" {
    fn yw_error_string(err: c_int) -> *const c_char;
    fn yw_quit_reason_string(code: c_int) -> *const c_char;
    fn yw_quit_reason_is_in_use(code: c_int) -> bool;
    fn yw_quit_reason_is_stopped(code: c_int) -> bool;
    fn yw_quit_reason_is_remote_shutdown(code: c_int) -> bool;
    fn yw_session_new(p: *const RawParams, cb: *const RawCallbacks, err: *mut c_char, err_size: usize) -> *mut RawSession;
    fn yw_session_start(s: *mut RawSession) -> c_int;
    fn yw_session_set_controller(s: *mut RawSession, c: *const Controller);
    fn yw_session_set_controller_idle(s: *mut RawSession);
    fn yw_session_request_idr(s: *mut RawSession) -> c_int;
    fn yw_session_set_inject_loss(s: *mut RawSession, loss: f64);
    fn yw_session_goto_bed(s: *mut RawSession) -> c_int;
    fn yw_session_stop(s: *mut RawSession);
    fn yw_session_free(s: *mut RawSession);
    fn yw_wakeup(host: *const c_char, regist_key: *const u8, log_mask: u32, on_log: Option<LogFn>, user: *mut c_void) -> c_int;
    fn yw_install_signal_handlers();
    fn yw_shutdown_requested() -> bool;
}

fn cstr_lossy(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// chiaki 错误码的文字说明。
pub fn error_string(err: i32) -> String {
    cstr_lossy(unsafe { yw_error_string(err) })
}

fn check(err: c_int) -> Result<(), String> {
    if err == 0 {
        Ok(())
    } else {
        Err(error_string(err))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264 = 0,
    H265 = 1,
    H265Hdr = 2,
}

/// 会话结束的信息。
#[derive(Clone, Debug)]
pub struct QuitInfo {
    pub code: i32,
    pub reason: String,
    pub text: Option<String>,
    /// PS5 返回「远程游玩正在使用中」（IN_USE）
    pub in_use: bool,
    /// 是我们自己正常结束的
    pub stopped: bool,
    /// PS5 主动结束串流（关机、待机、在 PS5 上退出远程游玩）
    pub remote_shutdown: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Connected,
    Quit(QuitInfo),
    LoginPin { incorrect: bool },
    Rumble { left: u8, right: u8 },
    TriggerEffects,
    FecFailure,
    Other(i32),
}

/// 会话回调。都在 chiaki 的内部线程上调用，要快、不能阻塞太久。
pub trait Handler: Send + Sync + 'static {
    fn on_log(&self, _level: u32, _msg: &str) {}
    fn on_event(&self, _ev: Event) {}
    /// 返回 false 表示没收下这一帧（比如队列满）：chiaki 不把它记作参考帧，下一帧开始时向 PS5 报 CORRUPTFRAME，
    /// 引用它的 P 帧由 chiaki 改参考（recovered）或丢弃。回调 panic 也按 false 处理
    fn on_video(&self, _data: &[u8], _frames_lost: i32, _recovered: bool) -> bool {
        true
    }
    fn on_audio_header(&self, _channels: u32, _bits: u32, _rate: u32, _frame_size: u32) {}
    fn on_audio(&self, _data: &[u8]) {}
    fn on_haptics(&self, _data: &[u8]) {}
}

#[derive(Clone, Debug)]
pub struct SessionParams {
    pub host: String,
    pub regist_key: [u8; 16],
    pub morning: [u8; 16],
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    pub packet_loss_max: f64,
    pub ls_mtu: u32,
    pub ls_rtt_ms: u32,
    pub console_pin: u32,
    pub log_mask: u32,
}

type HandlerBox = Arc<dyn Handler>;

fn handler<'a>(user: *mut c_void) -> &'a HandlerBox {
    unsafe { &*(user as *const HandlerBox) }
}

extern "C" fn tr_log(user: *mut c_void, level: c_int, msg: *const c_char) {
    let msg = cstr_lossy(msg);
    let _ = catch_unwind(AssertUnwindSafe(|| handler(user).on_log(level as u32, &msg)));
}

extern "C" fn tr_event(user: *mut c_void, ty: c_int, code: c_int, text: *const c_char) {
    let ev = match ty {
        1 => Event::Connected,
        2 => Event::Quit(QuitInfo {
            code,
            reason: cstr_lossy(unsafe { yw_quit_reason_string(code) }),
            text: if text.is_null() { None } else { Some(cstr_lossy(text)) },
            in_use: unsafe { yw_quit_reason_is_in_use(code) },
            stopped: unsafe { yw_quit_reason_is_stopped(code) },
            remote_shutdown: unsafe { yw_quit_reason_is_remote_shutdown(code) },
        }),
        3 => Event::LoginPin { incorrect: code != 0 },
        4 => Event::Rumble { left: ((code >> 8) & 0xff) as u8, right: (code & 0xff) as u8 },
        5 => Event::TriggerEffects,
        6 => Event::FecFailure,
        _ => Event::Other(code),
    };
    let _ = catch_unwind(AssertUnwindSafe(|| handler(user).on_event(ev)));
}

extern "C" fn tr_video(user: *mut c_void, buf: *const u8, size: usize, frames_lost: i32, recovered: c_int) -> c_int {
    if buf.is_null() {
        // chiaki 不会交空指针；万一有，按收下处理，不影响它的参考帧记账
        return 1;
    }
    let data = unsafe { std::slice::from_raw_parts(buf, size) };
    let ok = catch_unwind(AssertUnwindSafe(|| handler(user).on_video(data, frames_lost, recovered != 0))).unwrap_or(false);
    ok as c_int
}

extern "C" fn tr_audio_header(user: *mut c_void, ch: u32, bits: u32, rate: u32, fs: u32) {
    let _ = catch_unwind(AssertUnwindSafe(|| handler(user).on_audio_header(ch, bits, rate, fs)));
}

extern "C" fn tr_audio(user: *mut c_void, buf: *const u8, size: usize) {
    if buf.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(buf, size) };
    let _ = catch_unwind(AssertUnwindSafe(|| handler(user).on_audio(data)));
}

extern "C" fn tr_haptics(user: *mut c_void, buf: *const u8, size: usize) {
    if buf.is_null() {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(buf, size) };
    let _ = catch_unwind(AssertUnwindSafe(|| handler(user).on_haptics(data)));
}

/// 一个 PS5 会话。Drop 时会正常结束会话、等待 chiaki 的线程退出，然后才释放回调对象。
pub struct Session {
    raw: *mut RawSession,
    handler: *mut HandlerBox,
}

// chiaki 的会话接口内部有锁，可以从另一个线程调用；我们只在监督线程里持有它。
unsafe impl Send for Session {}

impl Session {
    pub fn new(p: &SessionParams, h: Arc<dyn Handler>) -> Result<Session, String> {
        let host = CString::new(p.host.clone()).map_err(|_| "主机地址里有 \\0".to_string())?;
        let raw_params = RawParams {
            host: host.as_ptr(),
            regist_key: p.regist_key,
            morning: p.morning,
            height: p.height,
            fps: p.fps,
            bitrate_kbps: p.bitrate_kbps,
            codec: p.codec as c_int,
            packet_loss_max: p.packet_loss_max,
            ls_mtu: p.ls_mtu,
            ls_rtt_ms: p.ls_rtt_ms,
            console_pin: p.console_pin,
            log_mask: p.log_mask,
        };
        let hb: *mut HandlerBox = Box::into_raw(Box::new(h));
        let cb = RawCallbacks {
            user: hb as *mut c_void,
            on_log: Some(tr_log),
            on_event: Some(tr_event),
            on_video: Some(tr_video),
            on_audio_header: Some(tr_audio_header),
            on_audio: Some(tr_audio),
            on_haptics: Some(tr_haptics),
        };
        let mut err = [0 as c_char; 256];
        let raw = unsafe { yw_session_new(&raw_params, &cb, err.as_mut_ptr(), err.len()) };
        if raw.is_null() {
            drop(unsafe { Box::from_raw(hb) });
            return Err(cstr_lossy(err.as_ptr()));
        }
        Ok(Session { raw, handler: hb })
    }

    pub fn start(&self) -> Result<(), String> {
        check(unsafe { yw_session_start(self.raw) })
    }

    pub fn set_controller(&self, c: &Controller) {
        unsafe { yw_session_set_controller(self.raw, c) }
    }

    pub fn set_controller_idle(&self) {
        unsafe { yw_session_set_controller_idle(self.raw) }
    }

    pub fn request_idr(&self) -> Result<(), String> {
        check(unsafe { yw_session_request_idr(self.raw) })
    }

    /// 补丁 0001：按指定丢包率（0~1）向 PS5 上报；小于 0 恢复正常上报。
    pub fn set_inject_loss(&self, loss: f64) {
        unsafe { yw_session_set_inject_loss(self.raw, loss) }
    }

    /// 让 PS5 进入待机。
    pub fn goto_bed(&self) -> Result<(), String> {
        check(unsafe { yw_session_goto_bed(self.raw) })
    }

    /// 正常结束会话并等待线程退出。可以重复调用。
    pub fn stop(&mut self) {
        unsafe { yw_session_stop(self.raw) }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            yw_session_free(self.raw); // 内部先 stop + join，之后不会再有回调
            drop(Box::from_raw(self.handler));
        }
    }
}

/// 向 PS5 发送唤醒包。
pub fn wakeup(host: &str, regist_key: &[u8; 16]) -> Result<(), String> {
    let host = CString::new(host).map_err(|_| "主机地址里有 \\0".to_string())?;
    check(unsafe { yw_wakeup(host.as_ptr(), regist_key.as_ptr(), 0, None, std::ptr::null_mut()) })
}

/// 安装 SIGTERM / SIGINT 处理（只置标志），并忽略 SIGPIPE。
pub fn install_signal_handlers() {
    unsafe { yw_install_signal_handlers() }
}

pub fn shutdown_requested() -> bool {
    unsafe { yw_shutdown_requested() }
}
