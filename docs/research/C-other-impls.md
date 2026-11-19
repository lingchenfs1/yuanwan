# 调研 C：Chiaki 家族之外的 PS Remote Play 实现，以及可借鉴的串流架构

> 调研日期：2026-11-19。只读调研，克隆和下载的内容都放在 本地临时目录（不随仓库发布），没有执行其中任何代码。
> 标注说明：**【已核实】**表示有原始出处（官方页面、源码文件和行号、项目自己的文档或发布说明）；**【推测】**表示由多个来源推断，或只有单一来源、未经复核。注意，「已核实」只说明信息有出处，**不代表我在真机上测过**。
> 源码快照：moonlight-common-c `94c65fd`（2026-11-13）、Sunshine `23faefd`（2026-11-18）、moonlight-qt `28629dc`（2026-11-15）、Apollo `adc5c5a`（2026-05-21）、portal-boost `6471f82`（2026-10-19）、atameric `4430daa`（2026-10-21）、Ripcord `76d3a0e`（2026-11-18）、xbox-xcloud-player `a6511a6`（2025-11-04）。chiaki-ng 只按需读了几个文件（main 分支，最近提交 `43728af2`，2026-11-03；最新发布 v1.10.1，2026-11-03）。

---

## 0. 结论速览

1. **还没有成熟的「PS5 Remote Play 公网中继/网关」开源项目。** 现有的「Portal Relay」类项目分两种：一种是给 Portal 登录 PSN 用的 HTTP CONNECT 代理，不承载媒体流；另一种只在局域网内做提码率的中间人。至于把 Remote Play 转成 Moonlight 协议，没有找到任何项目。**【已核实】**（搜索范围见 §3）
2. **最有价值的证据：L4 透明中继可行。** Portal Boost（Android，MIT）在用户态逐字节转发 TCP 9295 和 UDP 9296/9297，并代答 UDP 9302 发现包。Portal 把手机当成 PS5，会话照常跑通。可见 Remote Play 会话**不要求客户端与 PS5 在同一二层网络**，也不会因为对端 IP 是中继地址而失败。**【已核实】**（项目源码和文档；我未实测）
3. **Sony 官方 PS Remote Play（Win 9.5.0.08040 / Mac 9.5.0 / iOS 9.5.0）必须登录 PSN，按账号自动搜索主机**，官方文档里没有手动 IP 入口。所以官方客户端**不能用于「公网直连/自建中转」**。**【已核实】**（官方文档）；「没有手动 IP 功能」是根据文档推断的**【推测】**。
4. **Portal 用的是更新的 Takion 版本 20，chiaki-ng 对 PS5 仍用 12**（`lib/src/streamconnection.c:168`）。PXPlay 从 PC v3.1.1 起加了「关闭 Takion v20」的兼容开关，并多次修复「newer PS5 streaming protocol」的稳定性问题。可见 PS5 同时接受 v12 和 v20，新版协议的细节目前只有闭源客户端掌握。**【已核实】**
5. **码率：PS5 按客户端在会话开始时 LaunchSpec 里填的 `bitrate` 设定整场目标码率。** Portal 1080p High Quality 填 25000 kbps，改成 65000 或 100000 后，PS5 报告的目标约为 63 或 97 Mbps。会话中途由 PS5 自己闭环调节，会给客户端发 CONNECTIONQUALITY（含 target_bitrate、rtt、loss）。**【已核实】**（社区工具单机观测，加 chiaki-ng 源码）
6. **可借鉴的方案里，Moonlight/Sunshine 最透明：**视频不重传；用 Reed-Solomon FEC（默认 20%，小帧至少 2 个校验包）；丢帧时优先让参考帧失效，否则请求 IDR；码率故意固定；帧内发送有节奏控制；客户端用低延迟解码，按 vsync 丢帧，统计面板很全。**【已核实】**（源码行号见 §2.1）
7. **能移植到 PS5 方案的：**客户端侧的全部设计（抖动缓冲、帧节奏、vsync、解码低延迟、统计、QoS 标记），以及**中继隧道层**的二次 FEC/ARQ、DSCP 和小缓冲策略。**不能控制的：**PS5 编码器（GOP、FEC 比例、码率曲线、分辨率上限 1080p），只能设初始码率并如实上报丢包。**【推测】**（综合判断）
8. **风险：**Ripcord（2026-11-16 新建）的文档称，PS5 经 PSN 建立的外网会话改走 UDP 9303 控制加 UDP 9297 媒体，没有 TCP 9295。这尚无独立验证，但说明「局域网路线」与「PSN 外网路线」的协议形态可能不同。自建中转应选能在局域网路线上工作的方式（L3 隧道或 L4 端口中继），不要依赖 PSN 路线。**【推测】**

---

## 1. PS Remote Play 相关实现（Chiaki 家族之外）

### 1.1 开源实现一览

| 项目 | 语言/许可 | 状态（截至 2026-11-19） | 要点与评价 |
|---|---|---|---|
| [ktnrg45/pyremoteplay](https://github.com/ktnrg45/pyremoteplay) | Python，GPL-3.0 | **已归档**，最后推送 2022-10-27；PyPI 最新 0.7.6（2022-08-23） | 用 asyncio 写的 API 加可选 GUI，README 明说「只支持本地网络」，基于 Chiaki；已知问题有音频卡顿、不支持 HDR。**不宜作基础**，只适合当协议的 Python 参考。**【已核实】** |
| [kingcreek/ps4-remote-play](https://github.com/kingcreek/ps4-remote-play) | Python，Apache-2.0 | 2018 年后无更新 | 仅支持 PS4，按早期逆向文档写成，已过时。**【已核实】** |
| [altrhombus/ripcord](https://github.com/altrhombus/ripcord) | C#/.NET，Apache-2.0 | 2026-11-16 新建，还没有发布 | **独立重写（非 Chiaki 代码）**，自带协议规格文档 `docs/protocol/`，含 v1/v2 控制面、RUDP、WAN。自称在 Windows 上 1080p60、23 Mbps、丢包 0.4%、RTT 6.5 ms，能在双方都在 NAT 后、不做端口转发时经 PSN 打通 P2P；另有 PS3 移植。仓库里提交了从官方客户端提取的「互通常量」和 OAuth 客户端凭据，**有法律和合规风险**。适合拿来对照协议，不宜直接依赖。**【已核实】**（README/文档）；性能数据是作者自述**【推测】** |
| [saawant12/orbit-play-ps5](https://github.com/saawant12/orbit-play-ps5)（Orbit Play） | 客户端 AGPL-3.0，PS5 helper GPL-3.0 | v0.8.1 | **只支持越狱 PS5**（13.60 固件），PS5 端运行 helper。外网用自家的「Orbit Link」引荐服务器打洞直连、端到端加密，不需要 PSN；服务器开源、可自建，自建时**可开启流量中继**；官方服务器不中继。思路可参考（自己的信令替代 PSN），但前提是越狱，对一般用户不适用。**【已核实】** |
| Chiaki 衍生（仅列名） | 多为 AGPL-3.0 | 活跃 | [Pylux](https://github.com/ForWard-Technologies-LLC/Pylux)（v2.11.4，2026-08-14，上架各应用商店；README 里的「Internet Play」应指云串流**【推测】**）、[VitaRPS5](https://github.com/mauricio-gg/vitaki-vitarps5)（Vita，v4.2.2，2026-11-01）、[3dsrp](https://github.com/aurorastudios84/3dsrp)、[LunarNX](https://github.com/thinkzhou/LunarNX)（Switch，同时支持 Xbox）、PS-WRAP、chiaki-rust 等。说明 libchiaki 已是事实标准内核。**【已核实】** |

**纠正任务描述中的两处说法：**
- **PXPlay 不是 PS Vita 程序**，而是商业客户端 PSPlay 改名后的名字（iOS 上叫 MirrorPlay），由 streamingdv 发布（[托管仓库](https://github.com/streamingdv/PSPlay-Application-Hosting)）。Vita 上的开源 Remote Play 是 vitaki / VitaRPS5（基于 Chiaki），外网需要静态 IP 加端口转发，**不支持打洞**（[vitaki-fork README](https://github.com/ywnico/vitaki-fork)）。官方的 PS Vita 只能通过 PS4 Link 连 PS4。**【已核实】**
- **「Remote Play Whatever」是 Steam Remote Play Together 的工具**（[m4dEngi/RemotePlayWhatever](https://github.com/m4dEngi/RemotePlayWhatever)，1939 星），与 PlayStation 无关。**【已核实】**

### 1.2 商业闭源客户端

| 客户端 | 平台 / 版本 | 网络相关能力 | 口碑与备注 |
|---|---|---|---|
| **PXPlay**（原 PSPlay；iOS 叫 MirrorPlay） | Win/Linux/macOS PC v3.2.0（2026-11-11），Android v9.0.0；另有实验版 v3.2.0_PC_EXPERIMENTAL（2026-11-18） | 「PSN 模式」和「手动 IP 模式」可选（"Preferred default Remote Play mode: PSN or Manual IP"），UPnP 自动端口映射，「Takion protocol v20」兼容开关，持续修复「newer PS5 streaming protocol」的稳定性，「Better network resilience: fewer lost packets during brief network hiccups」 | 付费授权，PC 与 Android 分别买；Vulkan/libplacebo 渲染、插帧、DLSS/FSR 超分；社区普遍认为是功能最全的第三方客户端（有 r/PSPlay 社区）。**【已核实】**（发布说明）；口碑**【推测】** |
| **Asobi: Remote Play** | iOS/iPadOS/macOS/tvOS/visionOS，v2.0.1 | 支持外网 Remote Play，1.4.3 加入 **TURN 中继（beta，仅欧美）**，2.0 加入**自适应码率模式**，内置 NAT 类型检测，提供 Competition / Ultra Low latency 模式，统计面板显示抖动和帧预算 | App Store 4.6 分（261 条评分），免费加内购。值得注意：第三方客户端在 PSN 信令之外自建了 TURN。**【已核实】**（App Store 更新说明）；实现方式**【推测】** |
| **RemoteLight（遥光）** | iOS，$4.99，v1.2.8 | 可自定义串流 MTU，改进了 PSN 远程连接并显示各阶段耗时，支持 H.264 / H.265 / H.265 HDR | 评分数量不足。**【已核实】**（App Store） |
| **R-Play** | iOS | 只找到 PS4 时代的旧评测（[iMore](https://www.imore.com/ps4-remote-play-iphone-and-ipad)）：网络好时表现很好，网络差时不可靠 | 没有找到当前 PS5 版本的资料。**【已核实】**（旧评测） |

### 1.3 Sony 官方 PS Remote Play 的能力与限制

- **版本**（2026-11-19 直接抓取官方版本 JSON）：Windows `9.5.0.08040`（[rp-version-win.json](https://remoteplay.dl.playstation.net/remoteplay/module/win/rp-version-win.json)），Mac `9.5.0`（9.5.0 起不再支持 macOS Big Sur），iOS 9.5.0（需 iOS/iPadOS 16+，App Store 3.4 分，1.9 万条评分）。**【已核实】**
- **Windows 要求**：Windows 10（64 位）或 11、第 7 代 Intel Core 及以上、2 GB 内存；控制器用 USB 连接。网络「标准建议上下行 ≥5 Mbps，最佳 ≥15 Mbps」（[官方页](https://remoteplay.dl.playstation.net/remoteplay/lang/en/1100001.html)）。**【已核实】**
- **账号**：必须用 PS5 上的同一个 PSN 账号登录，应用「searches for the PS5 console that you signed in to using your account」；**很久没登录会搜不到主机**，官方建议出门前先在家里测一次。文档没有手动输入 IP 的步骤。**【已核实】**；由此推断官方客户端无法用于公网直连或自建中转**【推测】**
- **HDR**（Windows）：PS5 设置 HDR 为 Always On 或 On When Supported，安装 Microsoft HEVC Video Extension，开启 Windows HDR，并在应用中勾选 Enable HDR。**【已核实】**
- **分辨率**：PS5 和 PS4 Pro 由网络状况自动决定，最高 1080p；360p/540p/720p 和标准/高帧率等手动选项主要针对 PS4（Sony 支持文章与第三方指南，[示例](https://seekingtech.com/how-to-change-frame-rate-and-resolution-for-ps-remote-play-windows-and-mac/)）。**【推测】**（来源说法不完全一致）
- **限制**：需要 PS VR2、PS VR 或 PS Camera 的游戏不支持；Share Play、PS Plus Premium 云串流、光盘播放、音乐和视频服务不能与 Remote Play 同时使用；**PS5 CFI-7000（Pro）输出 8K 时**，还不能同时使用 Share Screen 和直播（[官方首页](https://remoteplay.dl.playstation.net/remoteplay/lang/en/index.html)）。**【已核实】**
- **移动数据**：需在应用里开启 Use Mobile Data；官方说明流量消耗远高于一般视频服务，且视运营商可能用不了。iOS 7.5.3（2024-12-12）修过「部分运营商网络下无法启动」的问题。**【已核实】**

### 1.4 PS Portal：协议、码率与 Wi-Fi

**协议**：从局域网中继工具的实现看，Portal 与 PS5 走的就是 Remote Play 协议族（[portal-boost HOW-IT-WORKS](https://github.com/Talgold3/portal-boost/blob/main/docs/HOW-IT-WORKS.md) 第 29–32 行）。**【已核实】**

| 端口 | 协议 | 作用 |
|---|---|---|
| 9302 | UDP | 发现（广播 SRCH）和唤醒 |
| 9295 | TCP | 会话建立与控制（可逐字节中继） |
| 9296 | UDP | Takion 流 |
| 9297 | UDP | Senkusha 测带宽 |

- **Takion 版本**：Portal 的首个 BIG 包里 client version 为 20（[atameric README](https://github.com/atameric/ps5-portal-high-bitrate) 第 96 行）；chiaki-ng 对 PS5 用 12，对 PS4 用 9（`lib/src/streamconnection.c:168`），解析分支只认 7、9、12（`lib/src/takion.c:201-213`）。**【已核实】**
- **码率**：LaunchSpec 是 JSON，经流加密后 base64 放进首个 BIG 包；明文偏移 334 处是 `25000`（kbps）。改码率只需对密文做 XOR，因为此时 GMAC 还是 0，PS5 不会发现。PS5「整场会话都遵从这个码率」。**【已核实】**（两个独立项目的文档和源码）
- **观测数据**（单机，Portal 固件 7.1.7）：原厂 25000 档实测约 20 Mbps。改 65000 后 PS5 报告目标约 63.1 Mbps，Portal 端约 58 Mbps；改 1e+05 后目标约 97.1 Mbps，峰值约 84 Mbps；改 2e+05 后目标约 158 Mbps，持续吞吐未确认。请求 4K 时，PS5 只提供 720p/540p/360p，画质反而下降。portal-boost 作者认为「PS5 编码器上限约 100 Mbps」。**【已核实】**（观测记录）；上限值**【推测】**
- **必须联网**：portal-boost 文档称「Portal 没有互联网就不启动 Remote Play」，因此在它自建的 Wi-Fi Direct 网络里加了一个 HTTP CONNECT 代理。**【已核实】**（项目文档）；是否所有固件都如此**【推测】**
- **官方固件中与网络、画质相关的更新**（[系统软件页](https://www.playstation.com/en-us/support/hardware/psportal/system-software-info/)）：
  - 3.0.0：可连公共 Wi-Fi（认证页用手机或电脑完成）；
  - 3.0.1：支持部分 5 GHz 公共网络；
  - 6.0.0：快速菜单可查看网络状态，可直接提交 Remote Play 反馈；
  - 7.0.0：新增最高分辨率选项「1080p High Quality」（[PlayStation Blog 2026-03-17](https://blog.playstation.com/?p=416772)，Sony 未公布码率）；
  - 当前最新 7.3.2。
  - Notebookcheck 报道 HQ 模式把最高码率从 15 提到 25 Mbit/s，与 LaunchSpec 里的 25000 吻合。**【已核实】**（Sony 页面）；码率数值**【推测】**（媒体报道加社区观测相互印证）
- **硬件**：Snapdragon 662，深度定制的 Android 13，8 英寸 1080p 60 Hz LCD（[Wikipedia](https://en.wikipedia.org/wiki/PlayStation_Portal)）。多篇评测称 Wi-Fi 只到 Wi-Fi 5（802.11ac），没有蜂窝网络。The Verge 评价它「lives and dies by your Wi-Fi」。**【已核实】**（Wikipedia）；Wi-Fi 规格**【推测】**（评测转述）
- **Wi-Fi 稳定性**：Sony 没有公开具体的优化手段，官方排障建议是 PS5 用有线、Portal 和 PS5 靠近路由器、更新固件。**社区实测经验**（portal-boost）：
  - 路由器 5 GHz 用固定的非 DFS 信道（36/40/44/48），频宽 40 MHz（80/160 MHz 出现延迟尖峰）；
  - 视频按 DSCP CS5 标记（对应 WMM AC_VI），输入和 ACK 按 CS6 标记（AC_VO），让输入插队；
  - UDP 缓冲限制在 256 KB（60 Mbps 下约 35 ms），宁可在中继端丢掉过期视频，也不转发上百毫秒的积压；
  - 作者称 Portal 和 chiaki-ng 的接收端只等缺失包约 1 帧（16 ms）就跳过（`Proxy.kt:210-220`）。
  - **【已核实】**（项目源码注释）；「只等 1 帧」是作者的描述**【推测】**

### 1.5 PS5 Pro 对 Remote Play 的影响

- Sony 在 2024-09-10 的官方博客中称 PS5 Pro 在支持的地区带 Wi-Fi 7（[blog](https://blog.playstation.com/2024/09/10/welcome-playstation-5-pro-the-most-visually-impressive-way-to-play-games-on-playstation/)）；Remote Play 官方页列出了 CFI-7000 输出 8K 时的额外限制。**【已核实】**
- 没有找到官方说明 Pro 提高了 Remote Play 的分辨率或码率上限。多篇第三方文章称 Remote Play 仍封顶 1080p，Pro 的收益只是编码器输入的源画面更干净，以及主机侧 Wi-Fi 更稳。**【推测】**
- 对我们的方案而言：可以把 Pro 当作普通 PS5。主机侧应**强烈建议用有线**，这样 Wi-Fi 7 的差别就无关紧要了。**【推测】**

---

## 2. 通用游戏串流方案中可借鉴的设计

### 2.1 Moonlight + Sunshine / Apollo（源码核实）

版本：Sunshine `v2026.914.233613`（2026-09-15）、moonlight-qt `v6.2.0`（2026-10-04）、Apollo `v0.4.6`（2025-07-13，仓库最后推送 2026-05-21）。**【已核实】**

**传输分层**：RTSP 握手协商参数（SDP 属性如 `x-nv-video[0].packetSize`）；RTP over UDP 承载视频和音频；控制和输入走 **ENet 可靠 UDP**，较新的服务端用 AES-GCM 加密（`moonlight-common-c/src/ControlStream.c`，`encryptedControlStream = APP_VERSION_AT_LEAST(7,1,431)`）。**【已核实】**

**FEC**：
- 视频用 GF(2^8) 上的 Reed-Solomon，每块数据加校验分片不超过 255 个，因此每块最多数据分片 = 255×100/(100+F)（`Sunshine/src/stream.cpp:809-817`）。
- 每帧最多 4 个 FEC 块；超过时跳过该帧的 FEC 并告警（`stream.cpp:1695-1708`）。
- Sunshine 默认 `fec_percentage = 20`（`src/config.cpp:817`）。
- 客户端要求小帧至少 2 个校验包，必要时比例可到 200%（`moonlight-common-c/src/SdpGenerator.c:207-211`）。客户端还主动关掉 GFE 的 BLL-FEC，理由是它在大帧上实际校验比例只有个位数、抗丢包更差（同文件 213-218 行）。
- 音频 FEC 固定 4 个数据片加 2 个校验片（`RtpAudioQueue.h:13-14`）。
- Sunshine 会按 FEC 比例和音频占用**从用户码率里扣除**，保证总带宽不超出用户设定（`src/rtsp.cpp:1205-1225`）。
- **【已核实】**

**丢包恢复（不重传视频）**：帧无法用 FEC 恢复时，优先发「参考帧失效（RFI）」区间；不支持或队列满时请求 IDR（`ControlStream.c:395-447`）。客户端把解码错误视为需要关键帧（无限 GOP 下必须如此，见 `moonlight-qt/app/streaming/video/ffmpeg.cpp:514-521`）。**【已核实】**

**反馈节奏与连接质量**：
- 丢包统计每 50 ms 上报一次，周期 ping 每 100 ms 一次（`ControlStream.c:316-317`）；
- 连接质量按 3 秒窗口统计：单个窗口丢帧 ≥30%，或连续两个窗口 ≥15%，判定为 poor；≤5% 恢复为 okay；开始的第一个窗口不报警（`ControlStream.c:140-143, 503-537`）。
- **【已核实】**

**码率**：
- Moonlight **故意固定码率**，把 min 和 max 设成同一个值，注释原文：「We don't support dynamic bitrate scaling properly (it tends to bounce between min and max and never settle on the optimal bitrate…)」（`SdpGenerator.c:364-375`）。
- 默认码率表：1080p 系数 10、1440p 系数 20、4K 系数 40，乘以 fps/30（60 fps 以上按平方根增长），即 1080p60 默认 20 Mbps（`moonlight-qt/app/settings/streamingpreferences.cpp:367-420`）。
- 在 Sunshine 和 Apollo 源码里没有找到自适应码率实现（grep 无匹配）。
- **【已核实】**

**发送节奏（pacing）与 QoS**：
- Sunshine 帧内按约 80% × 1 Gbps 的速率分组发送，每批不超过 64 包且小于 64 KB（避开 Windows 上的 SO_SNDBUF 问题和 Linux GSO 上限），并把上一帧最后一组计入（`stream.cpp:1739-1760, 1849-1863`）。
- DSCP 打标默认开启（`config.cpp:822`）。
- Moonlight **只在判定为局域网时才请求 QoS 标记**，因为公网上的 DSCP 标记可能被配置错误的运营商设备「黑洞」（`SdpGenerator.c:395-414`）。
- 视频包默认 1392 字节，判定路由经过 VPN 时降到 1024（`moonlight-qt/app/streaming/session.cpp:1681-1696`）。
- 加密默认值：局域网 never，公网 opportunistic（`config.cpp:819-820`）。
- **【已核实】**

**客户端解码与帧节奏**：
- 解码器始终设 `AV_CODEC_FLAG_LOW_DELAY`，允许输出损坏帧，`err_recognition = AV_EF_EXPLODE` 以便尽快请求关键帧；硬解单线程，软解用片级多线程（`ffmpeg.cpp:508-532`）。
- Android 端按芯片厂商开启低延迟解码：`vendor.qti-ext-dec-low-latency.enable`、`vendor.hisi-ext-low-latency-video-dec…`、`vendor.rtc-ext-dec-low-latency.enable`、`vendor.low-latency.enable` 等（moonlight-android `MediaCodecHelper.java:220-223`）。
- Pacer：最多排队 3 帧；每次 vsync 把积压丢到 1 帧，若 500 ms 历史窗口内曾回落到 ≤1 帧，则放宽到 3 帧；等新帧时留 3 ms 余量（`pacer/pacer.cpp:22-31, 232-296`）。
- **【已核实】**

**统计面板**（`ffmpeg.cpp:895-958`）：网络到达帧率、解码帧率、渲染帧率；主机处理延迟 min/max/avg；网络丢帧率与抖动丢帧率（分开统计）；RTT 及其方差；平均解码时间、队列等待时间、渲染时间（含 vsync）。这是一套成熟的分段计时模板。**【已核实】**

**Apollo**：Sunshine 的分支，主要卖点是服务端虚拟显示器（SudoVDA）自动匹配客户端分辨率和刷新率，与 PS5 方案关系不大。**【已核实】**

**补充**：Moonlight PS3 客户端的 README 提到主机 60.000 Hz 与客户端 59.94 Hz 的时钟漂移（每约 16.68 秒多出 1 帧），GameStream 可通过协商刷新率来消除。**PS5 这边无法协商，客户端必须自己处理漂移**（周期性丢帧或重复帧，音频做微小重采样）。**【已核实】**（README）；对 PS5 的适用性**【推测】**

### 2.2 Parsec BUD

据 [Parsec 博客](https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007/)：
- BUD 基于 UDP，用 DTLS 1.2（OpenSSL）加密；有 TCP 式的基础可靠性语义，加上高度调优的自研拥塞控制。
- **视频端没有任何缓冲**，所以必须在拥塞**发生之前**就检测到。
- 设计优先级依次是延迟、帧率、画质。
- 网络短暂波动时提示用户，严重中断时暂停、恢复后再追上；60 秒未恢复则结束会话。
- 设计假设：RTT 小于 50 ms，带宽约 30 Mb/s。
- 早期用 TCP，因不稳定网络下频繁报错（12007/12010）才改为 UDP。

FEC 的具体做法和拥塞信号没有公开。**【已核实】**（博客原文）；细节缺失

### 2.3 Steam Remote Play

- 端口：UDP 27031/27036，TCP 27036/27037（Valve 支持页，经搜索转述）。
- 性能信息里会显示走「Direct」还是「SDR」（Steam Datagram Relay 中继）。
- 带宽限制有 Automatic/Unlimited 等选项。
- Beta 客户端加入了 Pyrowave 编解码器，可手动设到 1 Gbit/s，Valve 自己提醒「more bandwidth increases latency」。
- 协议内部（FEC、码率算法）没有公开文档。Valve 开源的 [GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets) 提供可靠/不可靠消息、AES-GCM-256 加 Curve25519、ICE 打洞、可插拔信令，但 README 没说 Remote Play 用了它。
- **【推测】**（主要依据论坛和媒体）

### 2.4 Xbox Remote Play 与开源客户端（Greenlight / xbox-xcloud-player）

- [Greenlight](https://github.com/unknownskl/greenlight)：TypeScript，MIT，v2.4.2（2026-06-05），2944 星。其播放器库 [xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player)（最后推送 2025-11-04）走 **WebRTC**。**【已核实】**
- 4 条 DataChannel：`input`（protocol 为 '1.0'）、`control`（'controlV1'）、`message`（'messageV1'）、`chat`（'chatV1'），全部 `ordered: true` 且不限重传，即可靠有序（`src/channel/*.ts`）。**【已核实】**
- 主机侧给出 **Teredo** IPv6 候选，客户端从中解出内嵌的 IPv4 地址和端口，额外加两条 host 候选（其中一条固定端口 9002）（`src/lib/ice.ts:24-50`）。**【已核实】**
- 统计面板的 RTT 分级：40 ms 以下为好，40–75 ms 为警告，75 ms 以上为差（`src/render/overlay.ts:63`）。**【已核实】**
- 信令和鉴权依赖微软账号与服务（与 PSN 的地位相同）。**【推测】**

### 2.5 「稳定、低延迟」关键设计对照，以及能否移植到 PS5

| 设计点 | 参考实现 | PS5 方案能否移植（编码器不可控） |
|---|---|---|
| 抖动缓冲 | Moonlight 基本不设视频抖动缓冲，收齐即解码，靠 pacer 吸收；Portal 和 chiaki-ng 等缺包约 1 帧 | **可以，完全在客户端。** 建议 0~1 帧可调，并按 RTT 方差自适应。**【推测】** |
| 帧节奏 / vsync | moonlight-qt pacer（排队 ≤3 帧、vsync 丢帧、500 ms 历史窗口）；PXPlay macOS 提供「Lowest Latency / Smooth」两档 | **可以。** 另要处理 PS5 与显示器刷新率的漂移。**【推测】** |
| 动态码率 | Moonlight 固定码率；Parsec 自研拥塞控制；Asobi 有「自适应码率模式」 | **部分可以。** 初始码率由客户端在 LaunchSpec 里定，会话中由 PS5 按 CONNECTIONQUALITY 闭环调节；客户端能影响的是初值和如实上报丢包。会话中途改档大概率要重连（Portal Boost 称码率保持整场）。**【推测】** |
| FEC | Sunshine RS 20%、小帧至少 2 个校验包、每帧 ≤4 块 | **端到端不可改**（PS5 自带 FEC，比例未知）；**可以在中继隧道的公网段加第二层 FEC**（如 [UDPspeeder](https://github.com/wangyu-/UDPspeeder) 的 `-f20:10`，默认凑包超时 8 ms），代价是带宽和数毫秒延迟。**【推测】** |
| ARQ | BUD 在 UDP 上做部分可靠；Moonlight 视频不重传；KCP 一类 | PS5 视频是否有 NACK 重传未知；**中继逐跳 ARQ** 仅在单跳 RTT 远小于帧间隔时有意义。**【推测】** |
| 丢帧恢复 | RFI 优先，否则 IDR | 取决于 PS5 协议是否支持（chiaki-ng 方向另行核实）。**【推测】** |
| 发送节奏 | Sunshine 帧内按 1 ms 分组节流 | PS5 侧不可控；**中继端可以重新整形**，但会增加延迟，需谨慎。**【推测】** |
| QoS / DSCP | Sunshine 默认打标；Moonlight 只在局域网打标；portal-boost 视频 CS5、输入 CS6 | **可以。** 客户端和家庭中继在局域网与 Wi-Fi 段打标，公网段清除标记。**【推测】** |
| 包大小 / MTU | 默认 1392，VPN 下 1024 | 隧道封装会占掉 MTU；Takion 的 MTU 由 Senkusha 探测并写进 LaunchSpec（chiaki-ng 的 `launch_spec.mtu`）。**必须保证探测结果适配隧道**，避免 IP 分片。**【推测】** |
| 解码低延迟 | FFmpeg LOW_DELAY 加厂商低延迟 key；AV_EF_EXPLODE | **可以，客户端侧。** **【推测】** |
| 统计面板 | moonlight-qt 分段计时；portal-boost 按 Takion 明文序号统计逐跳丢包 | **可以。** 建议同时在客户端和中继端统计逐跳丢包、RTT 和码率。**【推测】** |
| 打洞 / 中继 | Parsec 自建、Steam SDR、Xbox Teredo/ICE、Orbit Link、Asobi TURN | 不越狱时 PS5 只配合 PSN 打洞，**纯自建只能走端口转发、隧道或中继**（见 §3）。**【推测】** |

---

## 3. 「PS5 Remote Play 中继 / 代理 / 网关」现状

搜索方式：`gh search repos`，关键词包括 "remote play relay/proxy/gateway"、"ps5 relay"、"playstation portal"、"chiaki relay/server/headless/sunshine/moonlight"、"remote play wireguard/tailscale"、"takion relay"、"ps5 holepunch"、"gamestream playstation"、"ps5 webrtc"；另做 WebSearch。**没有找到**把 Remote Play 转成 Moonlight/GameStream 协议的项目，也**没有找到**面向公网、可自建的 Takion 媒体中继。**【已核实】**（以上述搜索为限）

### 3.1 项目清单与评价

| 项目 | 形态 | 承载媒体？ | 适用范围 | 评价 |
|---|---|---|---|---|
| [Talgold3/portal-boost](https://github.com/Talgold3/portal-boost)（MIT，v1.0，2026-10-19） | **L4 用户态中继**：手机建 Wi-Fi Direct 热点，Portal 接入；代答 UDP 9302 发现包、转发唤醒；TCP 9295 逐字节转发；UDP 9296/9297 每个客户端一条上游 socket；首包改码率 | **是** | 局域网（经手机两跳 Wi-Fi） | **对我们最有价值的可行性证据。** Portal 显示的延迟从约 4 ms 升到 8–11 ms（两跳 Wi-Fi）；DSCP 和小缓冲策略可直接借鉴；只测过一部手机、一台 Portal、一台路由器。**【已核实】**（源码、文档） |
| [atameric/ps5-portal-high-bitrate](https://github.com/atameric/ps5-portal-high-bitrate)（MIT，v0.3.0-preview.3，2026-10-01；树莓派 4 中继 2026-10-21 跑通） | **ARP 欺骗做中间人**，只中继会话起始阶段（≤40 秒），改码率后恢复直连 | 只在起始阶段 | 局域网 | 本质是「提码率」而不是中继，需要 root 和原始套接字；有 watchdog 负责恢复网络。**【已核实】** |
| [ShawnZhou1993/ps5-portal-high-bitrate-docker](https://github.com/ShawnZhou1993/ps5-portal-high-bitrate-docker) | 上一项的 Docker 封装（macvlan，NET_RAW） | 同上 | 局域网 | 必须用 macvlan（要有自己的 MAC 地址）；强制停止容器后 Portal 的 ARP 表会长时间指向失效地址。**【已核实】** |
| [ChaNg1o1/PS-Portal-Relay](https://github.com/ChaNg1o1/PS-Portal-Relay)（TS） | Portal 系统代理指向它，**HTTP CONNECT 透明 TCP 隧道**，按域名区分 auth、signaling 等 | **否** | 解决「受限网络下 Portal 登录 PSN 和信令」 | 依赖 PSN 和 Sony 服务器，**不符合需求**。**【已核实】** |
| [hyson666/ps-portal-hop](https://github.com/hyson666/ps-portal-hop)（ISC） | 同类 HTTP CONNECT 正向代理，可串联上级代理 | **否**（README 明说「不是完整的 Remote Play 媒体中继」） | 同上 | 不符合需求。**【已核实】** |
| TGSAN/portal-proxy（C#） | 没有 README | 未知 | — | 信息不足。**【已核实】** |
| [aurorastudios84/3dsrp](https://github.com/aurorastudios84/3dsrp) Relay（AGPL） | **协议网关**：Android/Windows 伴侣端跑 chiaki-ng 并解码，再缩放、编码成 JPEG，经自定义 UDP（端口 29870，每片 1152 字节）发给老 3DS | 是（重新编码） | 局域网，传输不加密 | 是「把 Remote Play 转成别的协议」的唯一实例。代价是二次编码（画质和延迟），只适合低端终端。**【已核实】**（`relay/common/wire.h`） |
| [meyskens/ps-remote-play-vpn](https://github.com/meyskens/ps-remote-play-vpn) | **L2 方案**：GRETAP over WireGuard，把远端 Wi-Fi 桥接进家庭局域网 | 是（隧道） | 公网（4G） | 示范了用 L2 桥接让发现广播和局域网路线照常工作；面向 Vita 加 PS4。**【已核实】** |
| [corecompiled/ps5-remote-play-anywhere](https://github.com/corecompiled/ps5-remote-play-anywhere) | 指南加诊断 CLI（检测 CGNAT、端口、overlay 子网路由） | — | 公网 | 有参考价值，但端口表把 PS4 的 987 当成 PS5 的唤醒端口，与 chiaki-ng 文档不一致，使用需谨慎。**【已核实】** |
| [konraddzbik/ps-remote-checker](https://github.com/konraddzbik/ps-remote-checker) | NAT/UPnP/CGNAT 检测加实时 P2P 连通测试 | — | 公网（PSN 路线） | 诊断工具；作者称 Portal 外网视频走点对点 UDP。**【已核实】**（README） |
| Orbit Play「Orbit Link」 | 自建引荐服务器，可选中继 | 可选 | 公网 | 只支持越狱 PS5。**【已核实】** |
| Asobi TURN（闭源） | 在 PSN 打洞失败时走自家 TURN | 是 | 公网（欧美 beta） | 说明「PSN 只做信令，媒体走自建 TURN」的混合模式有人做成了。**【已核实】**（更新说明）；实现方式**【推测】** |
| Ripcord 文档 `ps5-wan-relay.md` | 研究记录：一次 WAN 抓包中，134 MB、约 13.1 万个 UDP 包全部发往同一个 Sony 地址，被判断为 IP 级透明转发 | — | — | 作者已注明该文档「部分是被取代的推测」。**【推测】** |

### 3.2 从这些项目得出的中继设计约束

1. **发现**：UDP 9302 是广播，过不了 L3。要么让客户端手动填 IP（chiaki-ng 本就支持），要么在客户端所在网段代答（portal-boost 缓存 PS5 的应答再回放）。**【已核实】**
2. **唤醒**：portal-boost 把唤醒包原样转发到 PS5 的 9302；chiaki-ng 文档给 PS5 列的外网端口是 9295（TCP/UDP）、9296、9297、9302（UDP）（[chiaki-ng 外网文档](https://chiaki-ng.github.io/chiaki-ng/setup/remoteconnection/)）。**【已核实】**；经 L3 隧道单播唤醒是否可靠**【推测】**
3. **会话绑定**：portal-boost 为每个客户端源地址单独开一个连到 PS5 的上游 UDP socket，PS5 看到的是中继地址，会话正常。这说明**协议载荷里没有会被 NAT 破坏的 IP 地址校验**，至少局域网路线如此。**【已核实】**（源码）；公网经 VPS 做 SNAT 后是否同样成立**【推测】**，需实测
4. **恢复的会话不重发起始包**（portal-boost 注释），所以「首包改码率」这类逻辑对续连会话无效。**【已核实】**
5. **Senkusha（9297）必须一起中继**，它决定 MTU 和 RTT，并写进 LaunchSpec（chiaki-ng `launch_spec.mtu/rtt`，`streamconnection.c:1000-1009`）。**【已核实】**
6. **chiaki-ng 的 PSN 远程连接只支持 IPv4**（文档原话：PS5 上的 Remote Play 本身只支持 IPv4）。**【已核实】**

### 3.3 四种中继形态比较（供大纲选型）

| 形态 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| L2 桥接 | GRETAP/VXLAN over WireGuard 或其他隧道 | 发现、唤醒、局域网路线全兼容 | 广播流量和 MTU 开销大；客户端一侧要有 L2 接入（如旅行路由器） |
| **L3 隧道** | 客户端到 VPS 到家中常驻设备或路由器，客户端直接连 PS5 的局域网 IP | 与协议无关，最简单；家里在 CGNAT 后也能由家中设备主动外连 VPS | 客户端要装隧道（手机、Portal 未必能装）；发现需改成手动 IP |
| **L4 端口中继** | VPS 公网 IP 的 9295/9296/9297(/9302) 经隧道转到 PS5；客户端手动 IP 填 VPS | 客户端零配置（任何支持手动 IP 的 Chiaki 系客户端都能用）；portal-boost 已证明语义可行 | VPS 端口对公网暴露（需访问控制）；要处理 UDP 会话表、超时和 MTU |
| 协议网关 | 家中设备解码 PS5 流后用自研协议重编码（3dsrp 式） | 可以完全自定义 FEC、ARQ、码率 | **二次编码**：画质损失，延迟增加 1 次编码加 1 次解码；需要家中常驻 GPU 设备；与「稳定优先」相悖，只作兜底 |

以上为**【推测】**（综合判断）。

---

## 4. 对后续设计的启示（供开发大纲参考，均为推测）

1. **客户端内核**：沿用 Takion 局域网路线（chiaki-ng 内核或自研兼容层）。重点投入客户端侧：可调抖动缓冲、Moonlight 式 pacer、解码低延迟、分段统计面板。
2. **网络层**：优先做「L3 隧道」和「L4 端口中继」两种自建模式，都不依赖 PSN；中继隧道可选开第二层 FEC/ARQ（参照 UDPspeeder、KCP 等），只在公网段启用。
3. **QoS**：局域网和 Wi-Fi 段打 DSCP（视频 CS5，输入 CS6），公网段清除；中继端缓冲要小，宁丢勿积。
4. **码率**：会话开始前按测得的带宽设定 LaunchSpec 初始码率（实测 PS5 至少接受到约 100 Mbps）；会话中依赖 PS5 闭环，同时如实上报丢包；需要换档时走「快速重连」。
5. **MTU**：隧道开销计入 MTU 预算，保证 Senkusha 探测结果不导致分片；参考 Moonlight 在 VPN 下用 1024 的做法。
6. **跟踪 Takion v20 和 v2 控制面的进展**（PXPlay、Ripcord），避免将来 PS5 固件弃用 v12 时被动。

---

## 5. 主要来源

- Sony 官方：[PS Remote Play 首页](https://remoteplay.dl.playstation.net/remoteplay/lang/en/index.html)、[Windows 页](https://remoteplay.dl.playstation.net/remoteplay/lang/en/1100001.html)、[rp-version-win.json](https://remoteplay.dl.playstation.net/remoteplay/module/win/rp-version-win.json)、[rp-version-mac.json](https://remoteplay.dl.playstation.net/remoteplay/module/mac/rp-version-mac.json)、[App Store 页](https://apps.apple.com/us/app/ps-remote-play/id1436192460)、[PS Portal 系统软件页](https://www.playstation.com/en-us/support/hardware/psportal/system-software-info/)、[PlayStation Blog 2026-03-17](https://blog.playstation.com/?p=416772)、[PS5 Pro 公告 2024-09-10](https://blog.playstation.com/2024/09/10/welcome-playstation-5-pro-the-most-visually-impressive-way-to-play-games-on-playstation/)
- PS Remote Play 相关项目：[pyremoteplay](https://github.com/ktnrg45/pyremoteplay)、[PXPlay 托管仓库与发布说明](https://github.com/streamingdv/PSPlay-Application-Hosting/releases)、[Ripcord](https://github.com/altrhombus/ripcord)、[Orbit Play](https://github.com/saawant12/orbit-play-ps5)、[Pylux](https://github.com/ForWard-Technologies-LLC/Pylux)、[vitaki-fork](https://github.com/ywnico/vitaki-fork)、[VitaRPS5](https://github.com/mauricio-gg/vitaki-vitarps5)、[portal-boost](https://github.com/Talgold3/portal-boost)、[atameric/ps5-portal-high-bitrate](https://github.com/atameric/ps5-portal-high-bitrate)、[ps5-portal-high-bitrate-docker](https://github.com/ShawnZhou1993/ps5-portal-high-bitrate-docker)、[PS-Portal-Relay](https://github.com/ChaNg1o1/PS-Portal-Relay)、[ps-portal-hop](https://github.com/hyson666/ps-portal-hop)、[3dsrp](https://github.com/aurorastudios84/3dsrp)、[ps-remote-play-vpn](https://github.com/meyskens/ps-remote-play-vpn)、[ps5-remote-play-anywhere](https://github.com/corecompiled/ps5-remote-play-anywhere)、[ps-remote-checker](https://github.com/konraddzbik/ps-remote-checker)、[chiaki-ng 外网文档](https://chiaki-ng.github.io/chiaki-ng/setup/remoteconnection/)、chiaki-ng 源码 `lib/src/streamconnection.c`、`lib/src/takion.c`、`lib/src/session.c`
- 商业客户端：[Asobi App Store 页](https://apps.apple.com/us/app/-/id6754204833)、[RemoteLight App Store 页](https://apps.apple.com/app/id6763110430)、[iMore 的 R-Play 旧评测](https://www.imore.com/ps4-remote-play-iphone-and-ipad)、[Orbit Play 指南](https://orbitplay.dev/guides/ps5-remote-play-outside-home-network/)
- 通用串流：[moonlight-common-c](https://github.com/moonlight-stream/moonlight-common-c)、[Sunshine](https://github.com/LizardByte/Sunshine)、[moonlight-qt](https://github.com/moonlight-stream/moonlight-qt)、[moonlight-android MediaCodecHelper.java](https://github.com/moonlight-stream/moonlight-android/blob/master/app/src/main/java/com/limelight/binding/video/MediaCodecHelper.java)、[Apollo](https://github.com/ClassicOldSong/Apollo)、[Parsec BUD 博客](https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007/)、[GameNetworkingSockets](https://github.com/ValveSoftware/GameNetworkingSockets)、[Steam Pyrowave 报道](https://mixed-news.com/en/steam-client-beta-pyrowave-1-gbit-manual-bitrate-reset-latency/)、[Greenlight](https://github.com/unknownskl/greenlight)、[xbox-xcloud-player](https://github.com/unknownskl/xbox-xcloud-player)、[UDPspeeder](https://github.com/wangyu-/UDPspeeder)、[KCP](https://github.com/skywind3000/kcp)
- 其它：[Wikipedia: PlayStation Portal](https://en.wikipedia.org/wiki/PlayStation_Portal)、[Notebookcheck 关于 Portal HQ 模式](https://www.notebookcheck.net/PS-Portal-is-becoming-more-appealing-Sony-finally-fixes-one-of-its-biggest-shortcomings.1256141.0.html)


---

## 附录：本方向的关键论断与待确认问题（复核结果见 V1/V2/V3）

- [C1] (high,已核实) PS Portal 与 PS5 走与 Chiaki 相同的 Remote Play 协议族：UDP 9302 发现/唤醒，TCP 9295 会话控制，UDP 9296 Takion 流，UDP 9297 Senkusha 测带宽。Portal Boost 在用户态逐字节中继这些端口（每个客户端一条上游 socket，PS5 看到的是中继地址），会话照常跑通，证明 L4 透明中继可行
  src: https://github.com/Talgold3/portal-boost docs/HOW-IT-WORKS.md:29-32；android/app/src/main/java/dev/portalboost/Proxy.kt:20-25,228-262（commit 6471f82）
- [C2] (high,已核实) Portal 发出的 Takion BIG 包 client version 为 20；chiaki-ng（2026-11-03 的 main 分支）对 PS5 用 protocol_version 12，对 PS4 用 9，解析分支只支持 7/9/12；PXPlay PC 加了关闭「Takion protocol v20」的兼容开关。可见 PS5 同时接受 v12 和 v20
  src: atameric/ps5-portal-high-bitrate README.md:96；chiaki-ng lib/src/streamconnection.c:168、lib/src/takion.c:201-213；PXPlay 发布说明 https://github.com/streamingdv/PSPlay-Application-Hosting/releases
- [C3] (high,已核实) PS5 按客户端在会话开始时 LaunchSpec 里填的 bitrate 设定整场目标码率。Portal（固件 7.1.7，1080p HQ）填 25000 kbps，实测约 20 Mbps；改成 65000 或 1e+05 后，PS5 报告目标约 63.1 或 97.1 Mbps。LaunchSpec 经流加密，起始阶段 GMAC 为 0，所以可以对密文做 XOR 改值
  src: atameric/ps5-portal-high-bitrate README（Observed results、How it works）；portal-boost docs/HOW-IT-WORKS.md:7,42-45；chiaki-ng lib/src/streamconnection.c:1009（launch_spec.bw_kbps_sent）
- [C4] (medium,推测) PS5 Remote Play 编码器的实际码率上限约 100 Mbps，分辨率上限 1080p：请求 4K 时 PS5 只提供 720p/540p/360p；PS5 Pro 也没有提高 Remote Play 的分辨率上限
  src: portal-boost README（"the PS5 encoder tops out around 100"）；atameric README（4K 实验）；第三方报道（tech-insider 等）
- [C5] (high,已核实) Sony 官方 PS Remote Play（Win 9.5.0.08040 / Mac 9.5.0 / iOS 9.5.0，iOS 需 16+）必须登录 PSN，按账号自动搜索主机，很久没登录会搜不到；文档没有手动 IP 步骤，因此不能用于公网直连或自建中转。官方网络建议为上下行 ≥5 Mbps，最佳 ≥15 Mbps
  src: https://remoteplay.dl.playstation.net/remoteplay/lang/en/1100001.html；https://remoteplay.dl.playstation.net/remoteplay/module/win/rp-version-win.json；https://apps.apple.com/us/app/ps-remote-play/id1436192460
- [C6] (high,已核实) Moonlight/Sunshine 视频不重传，靠 Reed-Solomon FEC：Sunshine 默认 20%，每块不超过 255 个分片，每帧最多 4 个 FEC 块；客户端要求小帧至少 2 个校验包，并关掉 GFE 的 BLL-FEC；音频固定 4 个数据片加 2 个校验片。帧无法恢复时优先发 RFI（参考帧失效），否则请求 IDR
  src: Sunshine src/config.cpp:817、src/stream.cpp:809-817,1695-1708；moonlight-common-c src/SdpGenerator.c:207-218、src/ControlStream.c:395-447、src/RtpAudioQueue.h:13-14
- [C7] (medium,已核实) Moonlight 故意固定码率（min=max），理由是动态码率会在上下限之间来回跳；Sunshine 会按 FEC 比例和音频占用从用户码率中扣除；Sunshine 和 Apollo 源码中没有自适应码率实现
  src: moonlight-common-c src/SdpGenerator.c:364-375；Sunshine src/rtsp.cpp:1205-1225；grep Sunshine/Apollo src 无匹配
- [C8] (medium,已核实) moonlight-common-c 丢包统计每 50 ms 上报，ping 每 100 ms；连接质量按 3 秒窗口判定：单窗口丢帧 ≥30% 或连续两窗口 ≥15% 判为 poor，≤5% 恢复
  src: moonlight-common-c src/ControlStream.c:140-143,316-317,503-537（commit 94c65fd）
- [C9] (medium,已核实) Sunshine 帧内按约 80%×1Gbps 节奏分组发送（每批 ≤64 包且 <64KB），DSCP 默认开启；Moonlight 只在判定为局域网时请求 QoS 标记（防止公网上 DSCP 包被黑洞）；默认包大小 1392 字节，经 VPN 时降到 1024
  src: Sunshine src/stream.cpp:1739-1760、src/config.cpp:822；moonlight-common-c src/SdpGenerator.c:395-414；moonlight-qt app/streaming/session.cpp:1681-1696
- [C10] (medium,已核实) moonlight-qt 客户端：解码器 AV_CODEC_FLAG_LOW_DELAY 加 AV_EF_EXPLODE（解码出错即请求关键帧，用于无限 GOP）；pacer 最多排队 3 帧，每次 vsync 丢到 1 帧（500 ms 历史窗口内放宽到 3 帧）；统计面板分别给出网络丢帧、抖动丢帧、RTT 与方差、解码、排队、渲染耗时
  src: moonlight-qt app/streaming/video/ffmpeg.cpp:508-532,895-958；app/streaming/video/ffmpeg-renderers/pacer/pacer.cpp:22-31,232-296
- [C11] (high,已核实) PS5 在会话中向客户端发送 CONNECTIONQUALITY 消息（target_bitrate、upstream_bitrate、upstream_loss、rtt、loss），即码率在 PS5 侧闭环控制；客户端只能决定初始码率，并通过上报影响调节
  src: chiaki-ng lib/src/streamconnection.c:693-706（消息字段）；「客户端只能影响」为推断
- [C12] (high,已核实) 没有找到把 PS Remote Play 转成 Moonlight/GameStream 协议的项目，也没有可自建的公网 Takion 媒体中继。唯一的协议网关实例是 3dsrp Relay：伴侣端跑 chiaki-ng 解码后编码为 JPEG，经自定义 UDP（端口 29870，每片 1152 字节，不加密）发给 3DS
  src: gh search repos 多组关键词（见报告 §3）；https://github.com/aurorastudios84/3dsrp relay/common/wire.h
- [C13] (high,已核实) 现有的 PS-Portal-Relay、ps-portal-hop 都是 HTTP CONNECT 代理，只隧道转发 Portal 访问 Sony 的登录和信令 TLS，不承载 UDP 媒体流且依赖 PSN，不符合「不用 Sony 转发」的需求；atameric 及其 docker 版只在局域网内做 ARP 中间人改码率，并非中继
  src: https://github.com/ChaNg1o1/PS-Portal-Relay README；https://github.com/hyson666/ps-portal-hop README；atameric 与 ShawnZhou1993 README
- [C14] (medium,推测) Ripcord（2026-11-16 新建的独立实现）文档称：经 PSN 建立的外网会话不用 TCP 9295，控制走 UDP 9303 上的可靠关联、A/V 走 UDP 9297；还存在 PS5 专有的 v2 控制面（ECDH 加 AES-GCM、会话密钥轮换）；一次 WAN 抓包中，媒体经 Sony 中继做 IP 级透明转发
  src: https://github.com/altrhombus/ripcord docs/protocol/ps5-v2-control-plane.md §15、docs/protocol/README.md、ps5-wan-relay.md（commit 76d3a0e）
- [C15] (medium,已核实) Portal Wi-Fi 稳定性的社区经验：PS5 有线；路由器 5 GHz 用固定非 DFS 信道（36/40/44/48）、40 MHz 频宽；视频 DSCP CS5（AC_VI）、输入 CS6（AC_VO）；中继 UDP 缓冲 256 KB 以丢弃过期视频；作者称 Portal 和 chiaki-ng 接收端只等缺包约 1 帧
  src: portal-boost README（Why the router channel matters）；Proxy.kt:210-220
- [C16] (medium,已核实) PS Portal 官方固件：3.0.0/3.0.1 支持公共 Wi-Fi（含部分 5 GHz），6.0.0 可查看网络状态，7.0.0 新增「1080p High Quality」（Sony 未公布码率；Notebookcheck 称从 15 提到 25 Mbit/s，与 LaunchSpec 25000 吻合），最新 7.3.2；硬件为 Snapdragon 662、Android 13 定制系统，无蜂窝网络
  src: https://www.playstation.com/en-us/support/hardware/psportal/system-software-info/；https://blog.playstation.com/?p=416772；https://en.wikipedia.org/wiki/PlayStation_Portal；Notebookcheck 报道
- [C17] (medium,已核实) PXPlay 是 PSPlay 改名后的闭源付费客户端（iOS 上叫 MirrorPlay），不是 PS Vita 程序：PC v3.2.0（2026-11-11）、Android v9.0.0，支持 PSN 模式与手动 IP 模式、UPnP 自动端口映射。Vita 上对应的开源客户端是 vitaki/VitaRPS5（v4.2.2，2026-11-01），外网只能静态 IP 加端口转发。「Remote Play Whatever」是 Steam 工具，与 PlayStation 无关
  src: https://github.com/streamingdv/PSPlay-Application-Hosting README 与 releases；https://github.com/ywnico/vitaki-fork；https://github.com/mauricio-gg/vitaki-vitarps5；https://github.com/m4dEngi/RemotePlayWhatever
- [C18] (low,已核实) pyremoteplay 已归档（最后推送 2022-10-27，PyPI 0.7.6 发布于 2022-08-23），只支持本地网络，基于 Chiaki，不宜作为开发基础
  src: gh api repos/ktnrg45/pyremoteplay（archived=true）；https://pypi.org/pypi/pyremoteplay/json；README
- [C19] (low,已核实) Parsec BUD：基于 UDP，用 DTLS 1.2 加密，带 TCP 式可靠性语义和自研拥塞控制；视频端无缓冲，必须在拥塞发生前检测；网络中断时暂停、恢复后追上，60 秒未恢复则结束会话；FEC 与拥塞信号未公开
  src: https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007/
- [C20] (low,已核实) Xbox 家用串流（Greenlight / xbox-xcloud-player）走 WebRTC：input、control、message、chat 四条 DataChannel 都是可靠有序；主机给出 Teredo IPv6 候选，客户端解出内嵌的 IPv4 地址和端口，另加两条 host 候选（其中一条固定端口 9002）；信令依赖微软服务
  src: https://github.com/unknownskl/xbox-xcloud-player src/channel/*.ts、src/lib/ice.ts:24-50（commit a6511a6）

## open_questions
- 经 VPS 做 L4 端口中继（DNAT/SNAT）时，PS5 能否接受来自中继地址的局域网路线会话（TCP 9295 + UDP 9296/9297）？Portal Boost 只证明了局域网用户态中继，chiaki-ng 只证明了公网端口转发；两者结合（客户端到 VPS 到隧道到 PS5）需要实测
- 经隧道或中继后，Senkusha 的 MTU/RTT 探测能否得到正确且不分片的 MTU？PS5 接受的最小 MTU 是多少？隧道封装开销与 Takion 包大小如何配合，需要抓包实测
- PS5 端视频 FEC 的比例、是否有 NACK 重传、是否支持参考帧失效或只能整帧 IDR？这决定中继层是否值得加第二层 FEC/ARQ（与 chiaki-ng 方向的调研交叉核对）
- PS5 的 CONNECTIONQUALITY 闭环在丢包时降码率有多快、能降到多低？会话中能否不重连就调整码率？Portal Boost 称码率保持整场，需实测确认
- Takion v20 相比 v12 改了什么，对稳定性有何影响？PS5 将来的固件会不会弃用 v12？chiaki-ng 有没有支持 v20 的计划？
- Ripcord 关于 v2 控制面、外网走 UDP 9303/9297、Sony 中继为 IP 级透明转发的结论需要独立复核；该项目还提交了从官方客户端提取的常量和 OAuth 凭据，借鉴其文档时要评估合规风险
- 经 L3 隧道向处于待机（rest mode）的 PS5 单播发送 UDP 9302 唤醒包是否可靠？是否需要家中常驻设备代发唤醒？待机时 PS5 有线网口的 ARP 行为如何？
- 需要用户决定：是否接受「PSN 只做信令，媒体走自建 TURN」的混合模式（Asobi 已做 TURN beta）？还是坚持完全不碰 PSN（只走端口转发、隧道或中继）？
- 需要用户决定：目标客户端是否包括 PS Portal？Portal 无法装 VPN，据称没有互联网就不启动 Remote Play，若要支持只能用 L2 桥接或家用旅行路由器；若只做自研客户端（PC/手机），L3/L4 方案就足够
- 需要用户决定：不越狱时，注册配对需要 PSN Account ID（一次性获取）以及 PS5 上的 8 位配对码，这是否可以接受？离线账号方案（idlesauce/ps5-remoteplay-get-pin、Orbit Play）都依赖越狱
- 需要用户决定：若基于 chiaki-ng（AGPL-3.0）开发，整个客户端必须开源，这是否可以接受？还是要像 Ripcord 那样独立重写协议栈？
- PS5 Pro（CFI-7000）在 Remote Play 下有没有更高的码率上限或更好的编码质量？需要用 Pro 与普通 PS5 对照实测
- Steam Remote Play 和 Parsec 的 FEC、码率算法没有公开资料，本报告对它们的描述停留在厂商博客和论坛层面
