//! gateway.conf 和命令行的合并。
//!
//! - gateway.conf 优先于命令行：命令行写死在 LaunchDaemon 的 plist 里，改它要 sudo；PS5 换 IP、改端口只改这个文件、重启网关；
//! - 格式：每行 `key=value`，`#` 开头的行是注释，前后空白忽略；文件不存在时全部取命令行和默认值；
//! - 每一项都记下来源（默认、命令行、gateway.conf），写进启动日志；
//! - 不认识的 key、取值不合法、重复的 key 都记一条警告：不合法的那一项按没写处理，重复的以第一次为准（和 M1 读 host 的行为一致）。

use std::path::Path;

/// 一项配置的来源
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Default,
    Cli,
    Conf,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Default => "默认值",
            Source::Cli => "命令行",
            Source::Conf => "gateway.conf",
        }
    }
}

/// 保持期满时做什么
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldAction {
    /// 只结束会话（PS5 按自己的节能设置待机）
    None,
    /// 顺便让 PS5 休眠
    Sleep,
}

/// 命令行里给了的、gateway.conf 也能改的项
#[derive(Clone, Debug, Default)]
pub struct Cli {
    pub host: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub loss_max: Option<f64>,
    pub hold_minutes: Option<u32>,
    /// --hold-sleep
    pub hold_sleep: bool,
}

/// YWP 默认端口 UDP 9420
pub const DEFAULT_YWP_PORT: u16 = ywp::consts::DEFAULT_PORT;
pub const DEFAULT_BITRATE_KBPS: u32 = 15000;
pub const DEFAULT_LOSS_MAX: f64 = 0.03;

#[derive(Clone, Debug)]
pub struct GatewayConf {
    /// PS5 地址；命令行和 gateway.conf 都没给时为 None
    pub host: Option<String>,
    pub bitrate_kbps: u32,
    /// 局域网码率上限（M2d 用）
    pub lan_max_kbps: Option<u32>,
    pub loss_max: f64,
    /// 没有客户端时保持多久（分钟），0 表示一直持有。
    /// None 表示哪里都没写：M2a 仍按 M1 的行为不限时，M2d 起改成默认 10 分钟
    pub hold_minutes: Option<u32>,
    pub hold_action: HoldAction,
    pub ywp_port: u16,
    /// YWP socket 绑哪块网卡；空串表示不绑
    pub ywp_bind_if: String,
    /// 第二台客户端能不能不经确认直接接管（默认不能，回 BUSY）
    pub ywp_takeover: bool,
    /// 不认识的 key、不合法的取值、重复的 key
    pub warnings: Vec<String>,
}

impl GatewayConf {
    /// 读 gateway.conf（不存在也可以）并和命令行合并。返回值的第二项是每一项的来源
    pub fn load(path: &Path, cli: &Cli) -> (Self, Vec<(String, Source)>) {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::merge(&text, cli),
            Err(e) => {
                let (mut c, src) = Self::merge("", cli);
                if e.kind() != std::io::ErrorKind::NotFound {
                    c.warnings.push(format!("读不了 {}：{e}（全部按命令行和默认值）", path.display()));
                }
                (c, src)
            }
        }
    }

    /// 按 gateway.conf 的文本合并（load 的核心，单测直接用它）
    pub fn merge(text: &str, cli: &Cli) -> (Self, Vec<(String, Source)>) {
        let mut warnings = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut conf = ConfFile::default();
        for (no, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                warnings.push(format!("gateway.conf 第 {} 行不是 key=value，忽略", no + 1));
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            if seen.iter().any(|s| s == k) {
                warnings.push(format!("gateway.conf 第 {} 行：{k} 重复，以第一次为准", no + 1));
                continue;
            }
            seen.push(k.to_string());
            if let Err(why) = conf.set(k, v) {
                warnings.push(format!("gateway.conf 第 {} 行：{why}", no + 1));
            }
        }

        let mut src = Vec::new();
        let mut pick = |key: &str, from_conf: bool, from_cli: bool| {
            let s = if from_conf {
                Source::Conf
            } else if from_cli {
                Source::Cli
            } else {
                Source::Default
            };
            src.push((key.to_string(), s));
        };
        pick("host", conf.host.is_some(), cli.host.is_some());
        pick("bitrate_kbps", conf.bitrate_kbps.is_some(), cli.bitrate_kbps.is_some());
        pick("lan_max_kbps", conf.lan_max_kbps.is_some(), false);
        pick("loss_max", conf.loss_max.is_some(), cli.loss_max.is_some());
        pick("hold_minutes", conf.hold_minutes.is_some(), cli.hold_minutes.is_some());
        pick("hold_action", conf.hold_action.is_some(), cli.hold_sleep);
        pick("ywp_port", conf.ywp_port.is_some(), false);
        pick("ywp_bind_if", conf.ywp_bind_if.is_some(), false);
        pick("ywp_takeover", conf.ywp_takeover.is_some(), false);

        let c = GatewayConf {
            host: conf.host.or_else(|| cli.host.clone()),
            bitrate_kbps: conf.bitrate_kbps.or(cli.bitrate_kbps).unwrap_or(DEFAULT_BITRATE_KBPS),
            lan_max_kbps: conf.lan_max_kbps,
            loss_max: conf.loss_max.or(cli.loss_max).unwrap_or(DEFAULT_LOSS_MAX),
            hold_minutes: conf.hold_minutes.or(cli.hold_minutes),
            hold_action: conf.hold_action.unwrap_or(if cli.hold_sleep { HoldAction::Sleep } else { HoldAction::None }),
            ywp_port: conf.ywp_port.unwrap_or(DEFAULT_YWP_PORT),
            ywp_bind_if: conf.ywp_bind_if.unwrap_or_default(),
            ywp_takeover: conf.ywp_takeover.unwrap_or(false),
            warnings,
        };
        (c, src)
    }

    /// 启动日志用：每一项的取值和来源（不含任何密钥）
    pub fn summary(&self, src: &[(String, Source)]) -> String {
        let label = |k: &str| src.iter().find(|(x, _)| x == k).map(|(_, s)| s.label()).unwrap_or("?");
        let opt = |v: Option<u32>| v.map(|v| v.to_string()).unwrap_or_else(|| "未设".into());
        let items = [
            ("host", self.host.clone().unwrap_or_else(|| "未设".into())),
            ("bitrate_kbps", self.bitrate_kbps.to_string()),
            ("lan_max_kbps", opt(self.lan_max_kbps)),
            ("loss_max", self.loss_max.to_string()),
            ("hold_minutes", opt(self.hold_minutes)),
            ("hold_action", if self.hold_action == HoldAction::Sleep { "sleep" } else { "none" }.into()),
            ("ywp_port", self.ywp_port.to_string()),
            ("ywp_bind_if", if self.ywp_bind_if.is_empty() { "不绑".into() } else { self.ywp_bind_if.clone() }),
            ("ywp_takeover", u8::from(self.ywp_takeover).to_string()),
        ];
        items.iter().map(|(k, v)| format!("{k}={v}（{}）", label(k))).collect::<Vec<_>>().join("，")
    }
}

/// gateway.conf 里写了的项
#[derive(Default)]
struct ConfFile {
    host: Option<String>,
    bitrate_kbps: Option<u32>,
    lan_max_kbps: Option<u32>,
    loss_max: Option<f64>,
    hold_minutes: Option<u32>,
    hold_action: Option<HoldAction>,
    ywp_port: Option<u16>,
    ywp_bind_if: Option<String>,
    ywp_takeover: Option<bool>,
}

impl ConfFile {
    fn set(&mut self, k: &str, v: &str) -> Result<(), String> {
        let bad = |what: &str| Err(format!("{k}={v} 不合法（{what}），按没写处理"));
        match k {
            "host" => {
                if v.is_empty() || v.contains(char::is_whitespace) {
                    return bad("要一个地址或主机名");
                }
                self.host = Some(v.to_string());
            }
            "bitrate_kbps" => match v.parse() {
                Ok(n) => self.bitrate_kbps = Some(n),
                Err(_) => return bad("要整数 kbps"),
            },
            "lan_max_kbps" => match v.parse() {
                Ok(n) => self.lan_max_kbps = Some(n),
                Err(_) => return bad("要整数 kbps"),
            },
            "loss_max" => match v.parse::<f64>() {
                Ok(x) if (0.0..=1.0).contains(&x) => self.loss_max = Some(x),
                _ => return bad("要 0~1 的小数"),
            },
            "hold_minutes" => match v.parse() {
                Ok(n) => self.hold_minutes = Some(n),
                Err(_) => return bad("要整数分钟，0 表示一直持有"),
            },
            "hold_action" => match v {
                "none" => self.hold_action = Some(HoldAction::None),
                "sleep" => self.hold_action = Some(HoldAction::Sleep),
                _ => return bad("要 none 或 sleep"),
            },
            "ywp_port" => match v.parse::<u16>() {
                Ok(n) if n != 0 => self.ywp_port = Some(n),
                _ => return bad("要 1~65535"),
            },
            "ywp_bind_if" => {
                // 空串表示不绑网卡；网卡名最长 15 个字符（IFNAMSIZ − 1）
                if v.len() > 15 || !v.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return bad("要网卡名，例如 en0；留空表示不绑");
                }
                self.ywp_bind_if = Some(v.to_string());
            }
            "ywp_takeover" => match v {
                "0" => self.ywp_takeover = Some(false),
                "1" => self.ywp_takeover = Some(true),
                _ => return bad("要 0 或 1"),
            },
            _ => return Err(format!("不认识的 key「{k}」，忽略")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(src: &[(String, Source)], key: &str) -> Source {
        src.iter().find(|(k, _)| k == key).map(|(_, s)| *s).unwrap()
    }

    #[test]
    fn defaults_without_file_or_cli() {
        let (c, src) = GatewayConf::merge("", &Cli::default());
        assert_eq!(c.host, None);
        assert_eq!(c.bitrate_kbps, DEFAULT_BITRATE_KBPS);
        assert_eq!(c.loss_max, DEFAULT_LOSS_MAX);
        assert_eq!(c.hold_minutes, None);
        assert_eq!(c.hold_action, HoldAction::None);
        assert_eq!(c.ywp_port, 9420);
        assert_eq!(c.ywp_bind_if, "");
        assert!(!c.ywp_takeover);
        assert!(c.warnings.is_empty());
        assert_eq!(src.len(), 9);
        assert!(src.iter().all(|(_, s)| *s == Source::Default));
    }

    #[test]
    fn conf_overrides_cli() {
        let cli = Cli {
            host: Some("192.0.2.10".into()),
            bitrate_kbps: Some(10000),
            loss_max: Some(0.05),
            hold_minutes: Some(0),
            hold_sleep: true,
        };
        let text = "# 注释\n\n host = 192.0.2.20 \nbitrate_kbps=20000\nhold_action=none\nywp_port=9500\nywp_bind_if=en0\nywp_takeover=1\nlan_max_kbps=50000\n";
        let (c, src) = GatewayConf::merge(text, &cli);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.host.as_deref(), Some("192.0.2.20"));
        assert_eq!(source(&src, "host"), Source::Conf);
        assert_eq!(c.bitrate_kbps, 20000);
        assert_eq!(c.loss_max, 0.05);
        assert_eq!(source(&src, "loss_max"), Source::Cli);
        assert_eq!(c.hold_minutes, Some(0));
        assert_eq!(source(&src, "hold_minutes"), Source::Cli);
        assert_eq!(c.hold_action, HoldAction::None, "gateway.conf 的 none 优先于 --hold-sleep");
        assert_eq!(source(&src, "hold_action"), Source::Conf);
        assert_eq!(c.ywp_port, 9500);
        assert_eq!(c.ywp_bind_if, "en0");
        assert!(c.ywp_takeover);
        assert_eq!(c.lan_max_kbps, Some(50000));
        assert_eq!(source(&src, "lan_max_kbps"), Source::Conf);
    }

    #[test]
    fn cli_used_when_conf_silent() {
        let cli = Cli { host: Some("192.0.2.10".into()), hold_sleep: true, hold_minutes: Some(30), ..Cli::default() };
        let (c, src) = GatewayConf::merge("ywp_port=9420\n", &cli);
        assert_eq!(c.host.as_deref(), Some("192.0.2.10"));
        assert_eq!(source(&src, "host"), Source::Cli);
        assert_eq!(c.hold_action, HoldAction::Sleep);
        assert_eq!(c.hold_minutes, Some(30));
    }

    #[test]
    fn host_only_file_like_m1() {
        // M1 的 gateway.conf 只有 host=
        let (c, src) = GatewayConf::merge("host=192.0.2.30\n", &Cli { host: Some("192.0.2.10".into()), ..Cli::default() });
        assert_eq!(c.host.as_deref(), Some("192.0.2.30"));
        assert_eq!(source(&src, "host"), Source::Conf);
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn invalid_values_warn_and_fall_back() {
        let cli = Cli { host: Some("192.0.2.10".into()), ..Cli::default() };
        let text = "host=\nhost=192.0.2.40\nywp_port=0\nloss_max=1.5\nhold_action=maybe\nywp_bind_if=en 0\nywp_takeover=yes\nbitrate_kbps=fast\nwake_on_start=1\njust some text\n";
        let (c, src) = GatewayConf::merge(text, &cli);
        // 空的 host 不合法，按没写处理；它已经占了「第一次」，后面那行算重复
        assert_eq!(c.host.as_deref(), Some("192.0.2.10"));
        assert_eq!(source(&src, "host"), Source::Cli);
        assert_eq!(c.ywp_port, DEFAULT_YWP_PORT);
        assert_eq!(c.loss_max, DEFAULT_LOSS_MAX);
        assert_eq!(c.hold_action, HoldAction::None);
        assert_eq!(c.ywp_bind_if, "");
        assert!(!c.ywp_takeover);
        assert_eq!(c.bitrate_kbps, DEFAULT_BITRATE_KBPS);
        assert_eq!(c.warnings.len(), 10, "{:#?}", c.warnings);
        assert!(c.warnings.iter().any(|w| w.contains("wake_on_start")));
        assert!(c.warnings.iter().any(|w| w.contains("第 10 行不是 key=value")));
    }

    #[test]
    fn host_with_space_rejected_like_m1() {
        let (c, _) = GatewayConf::merge("host=192.0.2.1 # 注释\n", &Cli::default());
        assert_eq!(c.host, None);
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn duplicate_key_first_wins() {
        let (c, _) = GatewayConf::merge("ywp_port=9501\nywp_port=9502\n", &Cli::default());
        assert_eq!(c.ywp_port, 9501);
        assert_eq!(c.warnings.len(), 1);
    }

    #[test]
    fn empty_bind_if_means_unbound() {
        let (c, src) = GatewayConf::merge("ywp_bind_if=\n", &Cli::default());
        assert_eq!(c.ywp_bind_if, "");
        assert_eq!(source(&src, "ywp_bind_if"), Source::Conf);
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn summary_lists_every_key_with_source() {
        let (c, src) = GatewayConf::merge("ywp_bind_if=en0\n", &Cli { host: Some("192.0.2.10".into()), ..Cli::default() });
        let s = c.summary(&src);
        assert!(s.contains("host=192.0.2.10（命令行）"), "{s}");
        assert!(s.contains("ywp_bind_if=en0（gateway.conf）"), "{s}");
        assert!(s.contains("ywp_port=9420（默认值）"), "{s}");
        assert!(s.contains("hold_minutes=未设（默认值）"), "{s}");
        assert_eq!(s.matches('=').count(), 9);
    }

    #[test]
    fn load_missing_file_is_fine() {
        let dir = std::env::temp_dir().join(format!("yw-conf-test-{}", std::process::id()));
        let (c, _) = GatewayConf::load(&dir.join("没有这个文件.conf"), &Cli::default());
        assert!(c.warnings.is_empty());
        assert_eq!(c.ywp_port, DEFAULT_YWP_PORT);
    }
}
