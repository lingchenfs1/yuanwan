//! 日志：人读的写到 stderr，机器读的写成 JSONL 文件（超过 50 MB 轮转一份 .1）。
//! 两边的写错误都忽略：磁盘满时网关照常工作（eprintln! 写失败会 panic，所以不用它）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ROTATE_BYTES: u64 = 50 * 1024 * 1024;
/// JSONL 打不开时，隔多久重试一次
const REOPEN_INTERVAL: Duration = Duration::from_secs(30);

struct JsonFile {
    file: Option<File>,
    last_try: Instant,
}

pub struct Logger {
    path: PathBuf,
    json: Mutex<JsonFile>,
    /// 为 true 时 stderr 只写 E、W 级别（LaunchDaemon 下 stderr.log 不轮转，详细日志看 JSONL）
    quiet_stderr: AtomicBool,
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn epoch_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn local_time() -> String {
    // 只用 std：按 UTC+8 显示（网关和用户都在东八区）
    let secs = (epoch_ms() / 1000) as u64 + 8 * 3600;
    let ms = (epoch_ms() % 1000) as u64;
    let s = secs % 86400;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600, (s / 60) % 60, s % 60, ms)
}

impl Logger {
    pub fn open(path: PathBuf) -> Logger {
        let file = Self::open_file(&path);
        Logger { path, json: Mutex::new(JsonFile { file, last_try: Instant::now() }), quiet_stderr: AtomicBool::new(false) }
    }

    pub fn set_quiet_stderr(&self, quiet: bool) {
        self.quiet_stderr.store(quiet, Relaxed);
    }

    fn open_file(path: &PathBuf) -> Option<File> {
        OpenOptions::new().create(true).append(true).open(path).ok()
    }

    fn write_json(&self, json: &str) {
        let mut j = self.json.lock().unwrap_or_else(|e| e.into_inner());
        if j.file.is_none() && j.last_try.elapsed() >= REOPEN_INTERVAL {
            j.last_try = Instant::now();
            j.file = Self::open_file(&self.path);
        }
        let Some(f) = j.file.as_mut() else { return };
        let _ = writeln!(f, "{json}");
        if f.metadata().map(|m| m.len() > ROTATE_BYTES).unwrap_or(false) {
            let rotated = self.path.with_extension("jsonl.1");
            let _ = std::fs::rename(&self.path, rotated);
            j.last_try = Instant::now();
            j.file = Self::open_file(&self.path);
        }
    }

    /// 一条事件日志。level：'E' 'W' 'I' 'V'。
    pub fn log(&self, level: char, kind: &str, msg: &str) {
        if !self.quiet_stderr.load(Relaxed) || level == 'E' || level == 'W' {
            let _ = writeln!(std::io::stderr().lock(), "{} {} [{}] {}", local_time(), level, kind, msg);
        }
        self.write_json(&format!(
            "{{\"ts\":{},\"lvl\":\"{}\",\"kind\":\"{}\",\"msg\":\"{}\"}}",
            epoch_ms(),
            level,
            escape(kind),
            escape(msg)
        ));
    }

    pub fn info(&self, kind: &str, msg: &str) {
        self.log('I', kind, msg);
    }

    pub fn warn(&self, kind: &str, msg: &str) {
        self.log('W', kind, msg);
    }

    pub fn error(&self, kind: &str, msg: &str) {
        self.log('E', kind, msg);
    }

    /// 只写 JSONL 的结构化记录（fields 是已经拼好的 `"k":v,...`）。
    pub fn record(&self, kind: &str, fields: &str) {
        self.write_json(&format!("{{\"ts\":{},\"kind\":\"{}\",{}}}", epoch_ms(), escape(kind), fields));
    }
}
