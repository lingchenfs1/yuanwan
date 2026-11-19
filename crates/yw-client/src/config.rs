//! 客户端配置：网关档案、本机密钥、配对串导入。
//! 配置目录在 `~/Library/Application Support/yuanwan/`（不受 iCloud 同步），文件一律 600。

use std::net::SocketAddr;
use std::path::PathBuf;
use ywp::crypto::Secret32;
use yw_media::Mode;

/// 一个网关的连接档案。地址、网卡都从配置文件读，不写死
#[derive(Clone, Debug)]
pub struct GatewayProfile {
    pub name: String,
    pub gw_pub: [u8; 32],
    pub psk: Secret32,
    pub gateway_id: [u8; 8],
    /// 网关入口，例如 192.0.2.10:9420
    pub entry: SocketAddr,
    /// 绑哪块网卡（例如 en0）；None 表示不绑。默认路由走隧道网卡时会提示指定它
    pub bind_if: Option<String>,
    pub bitrate_cap_kbps: Option<u32>,
    pub mode: Mode,
    /// 覆盖 J（ms）
    pub j_ms: Option<u8>,
    /// 连上后自动发 PS5_CMD START
    pub auto_start: bool,
}

/// `~/Library/Application Support/yuanwan/`
pub fn config_dir() -> PathBuf {
    todo!()
}

/// 生成本机静态密钥（私钥写进配置目录，600），返回公钥
pub fn keygen() -> Result<[u8; 32], String> {
    todo!()
}

/// 从 stdin 读配对串（不走命令行参数，免得进 shell 历史）；
/// 不回显配对串，只打印网关名和指纹（解码走 ywp::creds）
pub fn pair_import_stdin() -> Result<GatewayProfile, String> {
    todo!()
}
