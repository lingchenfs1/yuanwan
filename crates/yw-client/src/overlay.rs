//! 统计浮层（M2a 最小版，M2e 完整版）：用 SDL_RenderDebugText 显示 fps、RTT、丢包、冻结次数、W 帧数；
//! 音画差注明「视频按交帧时刻，未含帧展开」（0013 之前）。
//!
//! SDL 的调试字体只有 ASCII，所以浮层上的字用英文缩写；同样的内容另有一行中文给日志用。
//! 冻结次数由调用方填（M2a 接上 yw-media 的 FreezeMeter 之前，`--play` 只统计呈现间隔）。

use ywp::time::Micros;

/// 一组时长样本（微秒），算分位数用
#[derive(Clone, Debug, Default)]
pub struct Samples {
    v: Vec<u64>,
    sorted: bool,
}

impl Samples {
    pub fn push(&mut self, us: u64) {
        self.v.push(us);
        self.sorted = false;
    }

    pub fn len(&self) -> usize {
        self.v.len()
    }

    pub fn is_empty(&self) -> bool {
        self.v.is_empty()
    }

    fn sort(&mut self) {
        if !self.sorted {
            self.v.sort_unstable();
            self.sorted = true;
        }
    }

    /// 第 p 百分位（最近秩法）；没有样本时为 0
    pub fn pct(&mut self, p: f64) -> u64 {
        if self.v.is_empty() {
            return 0;
        }
        self.sort();
        let rank = ((p / 100.0) * self.v.len() as f64).ceil() as usize;
        self.v[rank.clamp(1, self.v.len()) - 1]
    }

    pub fn max(&self) -> u64 {
        self.v.iter().copied().max().unwrap_or(0)
    }

    pub fn clear(&mut self) {
        self.v.clear();
        self.sorted = true;
    }
}

/// 每秒的帧率
#[derive(Clone, Debug, Default)]
pub struct FpsCounter {
    window_start: Option<Micros>,
    count: u32,
    fps: f32,
}

impl FpsCounter {
    /// 记一帧；跨过 1 s 时更新帧率并返回 true
    pub fn tick(&mut self, now: Micros) -> bool {
        let start = *self.window_start.get_or_insert(now);
        self.count += 1;
        let el = now.saturating_sub(start);
        if el >= 1_000_000 {
            self.fps = self.count as f32 * 1e6 / el as f32;
            self.window_start = Some(now);
            self.count = 0;
            return true;
        }
        false
    }

    pub fn fps(&self) -> f32 {
        self.fps
    }
}

/// 浮层上显示的量（没有的填 None）
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct OverlayStats {
    pub fps: f32,
    pub rtt_ms: Option<f32>,
    /// 下行丢包率（%）
    pub loss_pct: Option<f32>,
    pub freezes: u32,
    pub w_frames: u32,
    /// 解码（送入到出帧）P95，ms
    pub dec_p95_ms: f32,
    pub frames: u64,
    /// 「找不到参考」日志行数（正常现象，单独列出）
    pub missing_ref: u64,
    pub dec_errors: u64,
    pub size: (u32, u32),
}

impl OverlayStats {
    /// 浮层文字（ASCII）
    pub fn lines(&self) -> Vec<String> {
        let opt = |v: Option<f32>, unit: &str| v.map_or_else(|| "-".to_string(), |x| format!("{x:.1}{unit}"));
        vec![
            format!("fps {:.1}  {}x{}  frames {}", self.fps, self.size.0, self.size.1, self.frames),
            format!("rtt {}  loss {}", opt(self.rtt_ms, "ms"), opt(self.loss_pct, "%")),
            format!("freezes {}  W {}  dec p95 {:.2}ms", self.freezes, self.w_frames, self.dec_p95_ms),
            format!("missing-ref {}  dec-err {}", self.missing_ref, self.dec_errors),
        ]
    }

    /// 日志里的一行（中文）
    pub fn log_line(&self) -> String {
        let opt = |v: Option<f32>, unit: &str| v.map_or_else(|| "无".to_string(), |x| format!("{x:.1} {unit}"));
        format!(
            "帧率 {:.1}，{}×{}，累计 {} 帧；RTT {}，丢包 {}；冻结 {} 次，W 帧 {}；解码 P95 {:.2} ms；找不到参考 {} 行，解码报错 {}",
            self.fps,
            self.size.0,
            self.size.1,
            self.frames,
            opt(self.rtt_ms, "ms"),
            opt(self.loss_pct, "%"),
            self.freezes,
            self.w_frames,
            self.dec_p95_ms,
            self.missing_ref,
            self.dec_errors,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::default();
        assert_eq!(s.pct(50.0), 0);
        for v in 1..=100 {
            s.push(v);
        }
        assert_eq!(s.pct(50.0), 50);
        assert_eq!(s.pct(95.0), 95);
        assert_eq!(s.pct(100.0), 100);
        assert_eq!(s.pct(0.0), 1);
        assert_eq!(s.max(), 100);
    }

    #[test]
    fn fps_counter() {
        let mut f = FpsCounter::default();
        let mut t = 0;
        let mut updated = false;
        for _ in 0..=60 {
            updated |= f.tick(t);
            t += 16_667;
        }
        assert!(updated);
        assert!((f.fps() - 60.0).abs() < 1.0, "{}", f.fps());
    }

    #[test]
    fn overlay_text_is_ascii() {
        let s = OverlayStats { fps: 59.9, rtt_ms: Some(19.2), ..Default::default() };
        for l in s.lines() {
            assert!(l.is_ascii(), "{l}");
        }
        assert!(s.log_line().contains("RTT 19.2 ms"));
    }
}
