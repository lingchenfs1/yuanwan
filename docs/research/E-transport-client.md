# 方向 E 调研报告：抗弱网传输与客户端技术选型

> 调研日期：2026-11-19。全程只读，克隆和下载的内容都放在 本地临时目录（不随仓库发布）（`src/` 放源码，`web/` 放网页和文档，`scripts/fec_calc.py` 是 FEC 模型计算脚本）。
> 标注约定：**【已核实】** 表示有源码行号、官方文档或 API 返回作依据；**【推测】** 是根据证据做的推断或设计判断，需要实测；**【模型】** 是基于简化假设的计算结果。

---

## 0. 结论先行

1. **先弄清 PS5 自己怎么抗丢包**（以 chiaki-ng 源码为准，提交 `43728af2`，2026-11-03）：
   - 视频用**每帧 Cauchy Reed-Solomon FEC**，基于 Jerasure 实现；客户端**不对视频包做 NACK 重传**。
   - 出现丢帧时，客户端发 **CORRUPTFRAME 上报**，可选再发 **IDRREQUEST**。
   - 客户端每 **200 ms** 向 PS5 上报一次 received/lost 计数，chiaki-ng 默认把上报的丢包率**钳制在 5%**。
   - PS5 会下发 **CONNECTIONQUALITY**（含 target_bitrate、rtt、loss）。launchspec 里有 `bwKbpsSent / mtu / rtt` 和 `adaptiveStreamMode:"resize"`，视频包还带 `adaptive_stream_index`。这说明 **PS5 端自带码率和分辨率自适应**，具体策略没有公开。
2. **网关透传方案的关键杠杆不在传输层拥塞控制，而在 PS5 的上报回路**【推测】。网关在有线局域网里几乎不丢包，如果照实上报，PS5 会一直按满码率发，广域网一拥塞就排队或丢包。网关应把远端链路的丢包和带宽状况"代理"进它发给 PS5 的 Takion 拥塞包，借 PS5 内建的自适应降档。这一点必须实测验证（可观察 CONNECTIONQUALITY.target_bitrate 和档位切换）。
3. **媒体面推荐自研 UDP 协议**【推测，设计建议】：
   - 帧级 RS FEC：冗余比例自适应，并设"最小冗余包数"下限；
   - 截止期感知的 NACK：只有重传能赶在帧显示截止前到达才重传；
   - 经自建 VPS 时做**逐跳重传**；
   - 可选**双路径冗余**；
   - AEAD 加密，单向时延测量。
   - 控制面和输入复用成熟库：QUIC stream（quinn）或 KCP。手柄输入用"不可靠 + 冗余重复最近状态"。
4. **不推荐用作主媒体通道**的方案：
   - SRT / RIST：TSBPD 固定缓冲，Live 模式默认 120 ms，面向广播；
   - KCP：可靠有序字节流，有队头阻塞；
   - 首期上 WebRTC：体量大，jitter buffer 和 pacer 为会议场景设计。

   这些方案里可借鉴的思想有：SRT 的 `arq=onreq`（FEC 失败才重传）、socket group broadcast（多链路冗余），WebRTC 的 GCC/TWCC 与 W3C 统计口径。
5. **QUIC 是可行的备选**：quinn + RFC 9221 DATAGRAM + 自定义拥塞控制 + 应用层 FEC。但 **RFC 9221 §5.4 规定 DATAGRAM 受连接拥塞控制约束**，用默认 Cubic/BBR 会延迟或丢弃视频包，必须换成可插拔的"应用速率"控制器（quinn 支持 `congestion_controller_factory`）。Multipath QUIC 已于 **2026-11 发布为 RFC 10069**，但它不规定调度策略。
6. **模型计算**显示：20% 的固定 FEC 只能扛住**随机**丢包。遇到平均 4 包的**突发**丢包，帧失败率仍有 3~5%；两条独立路径全量冗余可以降低 1~2 个数量级。所以"FEC + 截止期 NACK + 多路径"三者要组合使用。
7. **客户端推荐 Rust 核心 + 各平台原生壳**【推测】：
   - Windows 用 D3D11VA + D3D11 flip 模型；
   - macOS / iOS 用 VideoToolbox + Metal；
   - Android 用 MediaCodec（KEY_LOW_LATENCY）+ SurfaceView；
   - Linux / Steam Deck 用 VAAPI + Vulkan（libplacebo）；
   - 桌面端可以先用 SDL3 + FFmpeg + libplacebo 一套代码覆盖三个系统（chiaki-ng、moonlight-qt 都验证过这条路）。
   - Flutter 不建议承载视频面，Qt 在 iOS 上有 LGPL 静态链接的合规麻烦。
8. **度量**：
   - 端到端延迟用"光电二极管或 240fps 以上高速相机 + 触发按键"测 click-to-photon（参考 NVIDIA LDAT、OpenLDAT），并与 PS5 直连电视做基线对比；
   - 卡顿采用 W3C webrtc-stats 的 freeze 定义：帧间隔 ≥ max(3×平均帧时长, 平均+150 ms)；
   - 软件里内置每帧时间线、飞行记录仪、链路测速和 netem 回放测试台。

---

## 1. 现有实现是怎么抗丢包的（源码证据）

### 1.1 PS Remote Play / Takion（chiaki-ng 实现）

仓库：`chiaki-ng/chiaki-ng`，AGPL-3.0，v1.10.1（2026-11-03），GitHub 上 `streetpea/chiaki-ng` 已重定向到这里。以下行号都基于提交 `43728af2`。

| 机制 | 证据 | 说明 |
|---|---|---|
| 视频 FEC | `lib/src/fec.c:12-15,59`：`cauchy_original_coding_matrix` / `jerasure_matrix_decode`；`third-party/` 下有 jerasure、gf-complete | 每帧 k 个数据单元 + m 个 FEC 单元；单元数取自包头 `units_in_frame_total / units_in_frame_fec`（`lib/src/frameprocessor.c:69-79`）【已核实】 |
| 视频无 NACK | `lib/src/takionsendbuffer.c:12-14`：重发只用于可靠数据通道（200 ms 超时，最多 25 次）；视频路径只有 FEC + 上报 | 【已核实】 |
| 丢帧上报 | `lib/src/videoreceiver.c:195-196`：检测到缺帧就发 `CORRUPTFRAME(start,end)`；`lib/src/streamconnection.c:1263-1281` | PS5 收到后如何恢复（参考帧失效还是 IDR）不明【推测待测】 |
| IDR 请求 | `lib/src/videoreceiver.c:9-11`（每 1000 ms 重试）、`:235-256`（FEC 失败时发 IDRREQUEST）；`gui/include/settings.h:302` 默认 **false** | 默认关闭，可能是因为 IDR 会带来码率尖峰【推测】 |
| 拥塞上报 | `lib/src/congestioncontrol.c:5`（每 200 ms）、`:24-39`（received/lost 计数，按 `packet_loss_max` 钳制） | 【已核实】 |
| 上报钳制默认 5% | `gui/src/settings.cpp:741-748` 返回 0.05；提交历史：2024-01 曾"禁用丢包上报"后又回滚，2024-05 改成上限 20%，2024-06 改成 5% | 作者没写原因。推测是如实上报高丢包会让 PS5 大幅降质【推测】 |
| PS5 反馈 | `lib/protobuf/takion.proto:285-292`：`ConnectionQualityPayload{target_bitrate, upstream_bitrate, upstream_loss, disable_upstream_audio, rtt, loss}`；`lib/src/streamconnection.c:692-705` 只打日志 | 可以用来观察 PS5 的自适应行为【已核实字段】 |
| 启动参数 | `lib/src/launchspec.c:23-29`（`network.bwKbpsSent / mtu / rtt`）、`:77`（PS5 加 `"adaptiveStreamMode":"resize"`）、`:78-83`（avc/hevc、SDR/HDR）；`lib/src/streamconnection.c:1000-1009`（bitrate 来自用户档位，mtu/rtt 来自预测试） | 【已核实】 |
| 档位切换 | `lib/src/videoreceiver.c:157-170`：包头 `adaptive_stream_index` 变化时切换 profile（分辨率和 SPS/PPS） | PS5 可以在会话中途切分辨率【已核实】 |
| 预测试 | `lib/src/senkusha.c:29`（端口 9297）、`:292` RTT 测试、`:391/:457` 入站和出站 MTU 测试 | 结果写入 launchspec【已核实】 |
| 档位 | `lib/src/session.c:95-120`：360p 2000 / 540p 6000 / 720p 10000 / 1080p 15000 kbps；`lib/include/chiaki/session.h:61-62` 最高 1080p | 【已核实】 |
| 重排 | `lib/src/takion.c:47`：`TAKION_AV_REORDER_TIMEOUT_US 16000`（约 1 帧） | 2026-04 PR #741 加入了按流分开的重排队列【已核实】 |
| 输入回传 | `lib/src/feedbacksender.c:8-9`：状态包最小间隔 8 ms、最大 200 ms；history 缓冲 + 重发（PR #823/#838，2026-10） | 输入走"不可靠 + 冗余"思路【已核实】 |
| 触觉 | `gui/src/streamsession.cpp:1790`：触觉 PCM 3000 Hz 重采样到 48000 Hz（4 声道），写入 DualSense 声卡（`:43-45` 按设备名匹配） | 只有 USB 下能用 HD 触觉，蓝牙退化为震动模拟（`RUMBLE_HAPTICS_*`）【已核实；"后两声道驱动触觉"属【推测】】 |

**对网关方案的含义**【推测】：
- 网关就是 Takion 客户端，能拿到 PS5 FEC 还原后的**完整帧**。这时不必转发 Takion 原始包，可以**按帧重新封装**，用自己的 FEC、NACK 和多路径策略。
- Takion 本身就是为互联网远程游玩设计的（Sony 官方的公网远程游玩用的是同一套协议），所以"直接用 VPS 转发 Takion UDP"是一条最简单的**基线方案**。网关方案的价值在于更强的 FEC、ARQ、多路径和可观测性。两者应做 A/B 对比。

### 1.2 Moonlight / Sunshine（参考实现，GPL-3.0）

- moonlight-common-c 提交 `94c65fd3`（2026-11-13），Sunshine 提交 `23faefde`（2026-11-18）。`LizardByte/Sunshine` 在 gh API 中重定向到 `SunshineStream/Sunshine`，最新 v2026.914.233613。
- 视频 FEC：RS（nanors 子模块，`src/RtpVideoQueue.c:269,332`）。Sunshine `fec_percentage` **默认 20**（`docs/configuration.md:2117-2142`）。GF(2^8) 每块最多 255 个分片，大帧会分多块（文档 `:2985-2989` 的公式里有 `25500/(100+fec_percentage)`）【已核实】。
- 音频 FEC：4 个数据分片 + 2 个冗余分片（`src/RtpAudioQueue.h:13-14`）【已核实】。
- **视频不做 NACK 重传**：在 `src/*.c` 里 grep NACK 无结果，重传只出现在 ENet 控制流【已核实】。
- 恢复手段：请求 IDR，不支持时退化为参考帧失效 RFI（`src/ControlStream.c:1708-1745`）【已核实】。
- 上报节奏：丢包统计每 50 ms、ping 每 100 ms（`src/ControlStream.c:316-317`）；帧丢失 ≥30% 判为差，≤5% 判为正常（`:140-142`）【已核实】。
- 控制通道用 ENet（`src/ControlStream.c:1998-2015`，子模块 cgutman/enet）【已核实】。
- 自适应码率：截至 2026-11，**上游主线没有**；只有分支里的实验实现（lesj0610/Sunshine 2026-10 实验性 `adaptive_bitrate`，只有 NVENC 能在运行中改码率）【已核实（搜索所得，来源为 GitHub 分支）】。
- 统计浮层字段（moonlight-qt `app/streaming/video/ffmpeg.cpp:912-958`）：入网帧率、解码帧率、渲染帧率、主机处理时延、网络丢帧率、抖动丢帧率、RTT 与方差、解码时间、帧队列延迟、渲染时间（含 VSync）【已核实】。这是我们统计面板的现成参考。

### 1.3 Parsec BUD（闭源，信息有限）

官方博客说 BUD 基于 UDP + DTLS 1.2，用自研拥塞控制，与采集、编码、解码流水线联动；因为视频侧"不设缓冲"，必须在拥塞发生前就检测到。没有公开 FEC 和重传细节【已核实（厂商博客）】。

---

## 2. 候选传输协议和库对比

### 2.1 总表

版本、许可证和最近推送时间来自 gh API 或 crates.io，查询时间 2026-11-19。"适合度"一列是本报告的评估【推测】。

| 方案 | 许可证 | 最新版本（日期） | 可靠性机制 | FEC | 拥塞控制 | 跨平台 | 对"PS5 透传游戏流"的适合度 |
|---|---|---|---|---|---|---|---|
| SRT（Haivision/srt） | MPL-2.0 | v1.5.7（2026-08-28） | ARQ + TSBPD 固定延迟 + 过期丢包 | 内置 XOR 行列 FEC（SMPTE 2022-1），`arq=always/onreq/never` | Live 模式只按 `INPUTBW×(1+OHEADBW 25%)` 限速，不做真正的自适应 | 桌面端 + 移动端可编译 | 低：默认 120 ms 固定缓冲，广播导向；bonding 和 onreq 思想可借鉴 |
| RIST（librist） | BSD-2-Clause | v0.2.20（2026-07-30） | NACK ARQ（位图/范围） | 库本身以 ARQ 为主 | 无交互式自适应 | Linux/macOS/Windows/FreeBSD/WASM（README 未列 iOS/Android） | 低：与 SRT 类似；多路径 bonding 可借鉴 |
| QUIC：quinn（Rust） | MIT OR Apache-2.0 | crate 0.11.12（2026-09）；GitHub main 已到 0.12.0 开发版 | 流可靠；DATAGRAM 不可靠 | 无，需应用层自做 | NewReno / Cubic / BBR（BBR 标注 Experimental），**可插拔** | 纯 Rust，全平台 | 中高：控制面首选；媒体面要换自定义 CC |
| QUIC：quiche（Cloudflare） | BSD-2-Clause | 0.30.1（2026-11-10） | 同上 | 无 | Reno / Cubic / BBR2（gcongestion） | C API，可用于移动端 | 中 |
| QUIC：msquic（微软） | MIT | v2.6.4（2026-11-10） | 同上 | 无 | Cubic / BBR | **官方只支持 Windows、Linux**，其它平台"不正式支持" | 中低：移动端不稳妥 |
| WebRTC：libwebrtc | BSD-3-Clause | 滚动发布 | NACK / RTX | ULPFEC / FlexFEC | GoogCC（延迟梯度 + loss-based BWE v2） | 全平台，但构建很重 | 中：功能最全，但为会议设计，需关 pacer、playout-delay 设 0 等调参 |
| WebRTC：pion（Go） | MIT | v4.2.26 稳定版；v5.0.0-rc.3（2026-11-10） | NACK（interceptor） | interceptor 带 FlexFEC-03 编解码 | GCC / TWCC / RFC 8888 | Go，可用 gomobile | 中：适合网关端，也可做浏览器客户端 |
| WebRTC：str0m（Rust，sans-IO） | MIT OR Apache-2.0 | 0.25.0（2026-11-03） | NACK、TWCC、BWE | README 特性表**没有 FEC** | 有 BWE | iOS/Android 能编译但未测试 | 中：后期做浏览器客户端时的网关端候选 |
| webrtc-rs | MIT/Apache-2.0 | crate 0.21.1（2026-11-07），sans-IO 的 `rtc` 核心 + async 层 | NACK 等 | — | — | Rust | 中 |
| libdatachannel（C++） | MPL-2.0 | v0.24.6（2026-09-26） | 媒体传输 / DataChannel | — | — | 全平台 | 中 |
| KCP（原版 C） | MIT | 2.1.1（2026-05-15） | **可靠有序**（快速重传，RTO×1.5） | 无 | 可关闭 | 纯算法 | 视频低（队头阻塞）；控制和输入高 |
| kcp-go | MIT | v5.7.3（2026-11-04） | 可靠有序 | 可选 RS FEC（data/parity 可配，接收端自动识别分片数） | — | Go | 同上；kcptun/UDPspeeder 类隧道可用来快速实验 |
| ENet | MIT | 标签 v1.3.18 | 可靠 / 不可靠 / 有序通道 | 无 | 简单节流 | C | 控制通道可用（Moonlight 就这么用） |
| GameNetworkingSockets（Valve） | BSD-3-Clause | 2026-10 仍活跃 | 可靠 / 不可靠消息、分片、加密 | 无 | 有带宽估计 | C++ | 控制面可选 |
| UDPspeeder / tinyfecVPN | MIT | UDPspeeder 2026-07 仍活跃 | — | RS FEC 隧道 | — | Linux 为主 | 实验工具：给 Takion 原样转发套一层 FEC 做基线 |
| **自研 UDP + FEC + NACK** | 自定 | — | 截止期 NACK、逐跳 ARQ | 帧级 RS（自适应） | 应用层 BWE，驱动 PS5 上报或编码器 | 全平台（Rust 核心） | **高（推荐）** |

### 2.2 关键事实与分析

**SRT**【已核实：`docs/API/API-socket-options.md`、`docs/features/packet-filtering-and-fec.md`】
- `SRTO_LATENCY` / `SRTO_RCVLATENCY` 在 Live 模式默认 **120 ms**（`:235`、`:1372`）。语义是接收端**最小**缓冲时延，按发送时刻 + 时延准时交付（TSBPD），过期的包直接丢弃（TLPKTDROP）。
- FEC 过滤器是 SMPTE 2022-1 的行列 XOR（`:53`、`:317`）。`arq` 有三种模式：always（与 FEC 并行重传）、onreq（FEC 失败才上报丢包）、never（`:110-122`）。
- 拥塞控制只有 "live" 和 "file" 两种（`:358`）。Live 模式按 `MAXBW = INPUTBW*(100+OHEADBW)/100` 限速，OHEADBW 默认 25%（`:245`、`:636-637`）。
- Socket group 有 Broadcast（所有链路同时发同一份）和 Main/Backup 两种，Balancing 仍在开发中（Debian 收录的 v1.5.4 文档）。
- 评估【推测】：交互游戏希望"网络好时时延接近 0，网络差时也只多付出必要的那部分"。SRT 的固定时延把每一帧都抬到配置值，跟这个目标冲突。它的 `arq=onreq` 和 broadcast bonding 是很好的设计参考。

**RIST**【已核实：librist README，VideoLAN GitLab API】
- 完整支持 TR-06-1 和 TR-06-2：PSK/SRP 加密、多路径（负载均衡或冗余 bonding）、多路复用和 TUN。TR-06-3 只做到 Baseline.Direct。
- 同样是广播导向的固定缓冲 + NACK 方案【推测】。

**QUIC**
- **RFC 9221 §5.4**【已核实】：DATAGRAM 帧使用连接的拥塞控制器，发送方 MUST 推迟发送或直接丢弃。所以拿 QUIC DATAGRAM 发视频时：
  - 默认 Cubic/BBR 出现丢包或估计下降时会**卡住或丢掉**视频包，而 PS5 那头的码率并不会随之下降，结果是排队或丢帧；
  - 对策：quinn 的 `TransportConfig::congestion_controller_factory`（`quinn-proto/src/config/transport.rs:342`）可以注入自定义控制器，按应用层 BWE 放行；quiche 有 Reno/Cubic/BBR2 可选（`quiche.h:256-258`），也提供 dgram API（`:282`、`:841`）。
- **RFC 10069（Multipath QUIC）**【已核实：RFC Editor JSON】：2026-11 发布，Proposed Standard，来自 draft-ietf-quic-multipath-21。摘要明确说**不规定地址发现和调度**，调度要应用自己做。quinn 和 quiche 的 Multipath 实现进度本次**没查**。
- 相关草案【已核实 datatracker】：
  - draft-ietf-avtcore-rtp-over-quic-14（2026-05-25，仍是草案）；
  - draft-ietf-moq-transport-23（2026-11-02，仍是草案）；
  - draft-ietf-rmcat-gcc-02 已**过期**（2017）。GCC 只以 libwebrtc 实现为事实标准。
- 评估【推测】：QUIC 的好处是现成的 TLS 1.3 握手、0-RTT、连接迁移、路径验证，以及标准化的多路径。控制面（会话管理、配置、统计上报、文件和日志拉取）直接用 QUIC stream 很合适。媒体面用 DATAGRAM 也可以，但需要自己做 FEC、截止期 NACK 和应用层 CC。工作量和自研 UDP 相差不大，好处是加密和 NAT 处理不用自己写。

**WebRTC**【已核实：str0m README 特性表；pion/interceptor 目录有 `flexfec / gcc / nack / twcc / jitterbuffer / rfc8888 / pacing`】
- GoogCC 的 loss-based BWE v2 用 Bernoulli 模型的最大似然估计区分"拥塞丢包"和"链路固有丢包"，最终估计取延迟估计与丢包估计的较小值（libwebrtc 源码和 str0m 移植的说明）。
- 云游戏圈的经验：WebRTC 承载游戏流要把 playout delay 设 0、关掉发送端 pacer 和接收端帧缓冲（discuss-webrtc 论坛帖，不是权威来源）。
- 评估【推测】：首期不选。原因有三：透传场景下 BWE 无法直接控制 PS5 编码器；libwebrtc 构建重；jitter buffer 和 pacer 需要大量反向调参。如果以后要做**浏览器客户端**（免安装），网关端可以用 str0m 或 pion 产出 WebRTC 流（注意浏览器端无法用自定义 FEC）。

**KCP / ENet**
- KCP 官方 README 说：以比 TCP 多浪费 10%~20% 带宽为代价，换取平均延迟降低 30%~40%、最大延迟降低到三分之一；它是纯算法实现，不负责收发【已核实】。
- kcp-go 是可靠有序字节流，可选 RS FEC，接收端能自动识别分片数，分片总数不超过 256【已核实】。
- 评估【推测】：视频帧一旦有一个包靠重传才能补回，后面的帧都要等它（队头阻塞），所以不适合视频。适合控制和输入：输入量小，可以开 nodelay 模式。

**自研 UDP（推荐）的要点**【推测，设计建议】
1. **帧级 FEC**：
   - 每帧（或大帧切成若干块）用 RS 编码，冗余比例随测得的丢包率和突发长度自适应；
   - 设"最小冗余包数"下限（Sunshine 也有 `minRequiredFecPackets`，见 `src/stream.cpp:1783`），因为小 P 帧最脆弱（见 §3.2）；
   - IDR / 大帧可以单独提高冗余。
2. **截止期感知 NACK**：只有在"检测时延 + RTT + 发送时间 < 剩余显示余量"时才请求重传。余量由自适应 jitter buffer 决定，目标 0~1 帧，弱网时可放宽到 2 帧。
3. **逐跳 ARQ**：经自建 VPS 中转时，客户端↔VPS 和 VPS↔网关两段各自做 NACK 和缓存重传。单段 RTT 越短，重传越可能赶得上截止期。
4. **多路径**：
   - 模式 A：主路径全量 + 副路径全量（同一份发两遍），类似 SRT broadcast 和 SMPTE 2022-7，流量翻倍；
   - 模式 B：主路径全量 + 副路径只发 FEC 冗余包，或只发关键帧 / 小帧；
   - 模式 C：按路径质量动态分配。
   - 收端按序号去重。
5. **加密与认证**：AEAD（ChaCha20-Poly1305 或 AES-GCM），每包 nonce。
6. **时间戳**：每包带网关单调时钟时间戳；用 ping/pong 估计时钟偏移，算单向时延和抖动（RFC 3550 的 interarrival jitter）。
7. **输入通道**：独立的高优先级小包。每包携带最近 N 个状态或事件，不靠重传（参考 chiaki-ng feedback history、Moonlight 音频 4+2 FEC 和 chiaki-ng 话筒帧冗余重发）。

---

## 3. FEC 算法和库

### 3.1 可用开源 FEC 库

| 库 | 语言 / 许可证 | 类型 | 版本或活跃度 | 性能（来源原文数据） | 评估 |
|---|---|---|---|---|---|
| nanors（Moonlight/Sunshine 在用） | C / MIT | Cauchy RS（GF(2^8)、GF(2^16)），另有 additive-FFT RS | 现仓库 `ursoid-tech/nanors`（2026-10 仍有推送；Moonlight 子模块仍指向 sleepybishop/nanors） | README 称单核可达"多 Gbit"，具体图表未取 | 小块、低延迟场景首选之一【已核实（README）】 |
| cm256（catid） | C++ / BSD-3 | GF(256) Cauchy MDS | 最后推送 2024-10 | 1296 B 分片、k=100：m=20 时编码约 224 µs，m=10 时约 111 µs（作者自注"数据过时、未严格校准"） | 【已核实（README）】 |
| leopard（catid） | C/C++ / BSD-3 | RS，O(n log n)，GF(2^8)/GF(2^16)，最多 65535 原始分片 | 2026-09 仍有推送；Leopard2 新 API，带 AVX2/AVX512/GFNI 后端 | README 只给"相对 Leopard1 加速约 1.5 倍"之类的数据 | 大块场景合适【已核实】 |
| reed-solomon-simd | Rust / MIT AND BSD-3 | 基于 Leopard-RS，O(n log n)，Neon/SSSE3/AVX2 运行时选择 | crate 3.1.0（2025-10） | 32:32 编码约 10.2 GiB/s，解码约 254 MiB/s；1000:100 编码约 5.6 GiB/s，解码约 1 GiB/s | Rust 核心首选【已核实】 |
| reed-solomon-erasure | Rust / MIT | 经典 RS | 6.0.0（2022-09），不活跃 | — | 不推荐（停更） |
| klauspost/reedsolomon | Go / MIT | RS，SIMD | v1.14.2（2026-08-19） | — | 如网关用 Go【已核实】 |
| raptorq（cberner） | Rust / Apache-2.0 | RaptorQ 喷泵码（RFC 6330），非 MDS：收到 K+h 包后恢复概率 1-1/256^(h+1) | 2.0.1（2026-03-09） | Ryzen 9 9950X3D、1280 B 符号：编码约 11~30 Gbit/s，解码约 2.4~8.7 Gbit/s；**树莓派 3B+ 编码约 200~258 Mbit/s** | 大块或需要无限冗余时用；Qualcomm 对 RFC 6330 有 IPR 声明（2013，datatracker IPR #1958）【已核实】 |
| wirehair（catid） | C / BSD-3 | 喷泵码，O(N)，平均需 N+0.02 包 | 2026-09 仍有推送 | README 自述 **N<128 时效率不如 Fecal** | 每帧几十包的场景不合适【已核实】 |
| OpenFEC | C / CeCILL-C（主体）+ CeCILL（LDPC-Staircase 部分） | RS、LDPC-Staircase | 老项目 | 官网性能表没取到 | 许可证复杂，不推荐 |
| Jerasure + GF-Complete（chiaki 在用） | — | Cauchy RS | 2014 年作者因 StreamScale 主张专利 US 8,683,296 撤下 2.0 版（Techdirt 报道、作者主页） | — | 分发时有专利风险，需要法律评估【已核实（事件）】 |

**算法选择**【推测】：
- 每帧几个到一百多个包的小块场景，**MDS 的 RS（Cauchy 或 Leopard 类）最合适**：收到任意 k 个就能恢复，计算量在微秒级。
- 跨多帧交织的大块或喷泉码能扛更长突发，但会引入跨帧等待时延，跟"每帧尽快显示"冲突。只有在显式开启"高延迟稳定模式"时才考虑。
- XOR 行列码（SMPTE 2022-1 / FlexFEC）实现简单，但恢复能力弱于 RS。
- LDPC 适合超大块，不适合本场景。

### 3.2 模型计算：固定 20% FEC 够不够

脚本：调研时的临时脚本 `fec_calc.py`（未随仓库发布）【模型】

- 参考：15 Mbps、60 fps 时平均每帧约 31 KB，按 1250 B 一包约 25 包；P 帧更小，IDR 更大。
- 冗余取 m = max(2, ceil(0.2k))。
- **独立丢包**时单帧无法恢复的概率：

| 数据包 k | 冗余 m | 丢包 1% | 丢包 3% | 丢包 5% |
|---|---|---|---|---|
| 10 | 2 | 2.1e-4（约 81 s 一次） | 4.9e-3（约 3 s 一次） | 2.0e-2（约 1 s 一次） |
| 25 | 5 | 4.8e-7 | 2.3e-4（约 72 s 一次） | 3.3e-3（约 5 s 一次） |
| 50 | 10 | ≈0 | 1.6e-6 | 1.7e-4（约 97 s 一次） |

- **突发丢包**（Gilbert-Elliott 模型，平均丢包 3%，平均突发 4 包，仿真）：k=10、25、50 时帧失败率分别为 5.5%、5.4%、3.3%，固定 20% 冗余基本挡不住。
- **两条独立路径全量冗余**（两条路径都是上面的 GE 参数）：帧失败率降到 1.1e-3、5.5e-4、8e-5。
- 结论【推测】：
  - 小帧需要更高的最小冗余数；
  - 突发丢包要靠 NACK（RTT 允许时）或多路径分集；
  - 冗余比例应由实测的丢包率和突发长度驱动，不能写死；
  - 用户实际链路的 GE 参数必须实测。

---

## 4. 拥塞控制对实时流的影响

- 传统 CC（Cubic、BBR）面向吞吐量，不认识"帧"和"截止期"。QUIC DATAGRAM 受 CC 约束（RFC 9221 §5.4）。BBR 在 quinn 里标注 Experimental（`quinn-proto/src/congestion/bbr/mod.rs`）【已核实】。
- 实时视频的 CC（GCC、SCReAM、NADA）通过延迟梯度或丢包估计带宽，**输出给编码器去改码率**：
  - GCC 草案已过期，事实标准是 libwebrtc GoogCC；
  - SCReAM 是 RFC 8298，Ericsson 正在推进 SCReAMv2 草案（加入 L4S），开源实现 `EricssonResearch/scream`（BSD-2，2026-11 仍活跃）【已核实】。
- **透传场景的特殊性**【推测】：我们控制不了 PS5 编码器，传输层 CC 只能"少发"，而少发等于丢帧或排队，所以**不能让传输层 CC 去压视频**。正确做法：
  1. 用 GCC 类延迟梯度 + 丢包估计算出可用带宽 B；
  2. 把 B 转化成 PS5 能理解的信号：Takion 拥塞包里的 received/lost，以及会话启动时 launchspec 的 `bwKbpsSent`；
  3. 传输层只做 pacing，平滑 IDR 带来的突发，避免把交换机或 Wi-Fi 打满。pacing 本身会增加 IDR 帧的传输时延，要权衡。
- 转码模式下（见 §5.7），BWE 可以直接驱动网关编码器，这时就是标准的实时视频 CC 问题。

---

## 5. 网关透传（不重新编码）时的自适应手段

| 手段 | 可行性 | 代价 / 风险 | 评估 |
|---|---|---|---|
| 1. 开会话前测速选档 | 高。launchspec 的 bitrate/分辨率/帧率/编码器都由客户端决定（`streamconnection.c:1005-1009`） | 只反映开局状态 | **必做**：网关↔客户端双向 UDP 探测（带宽、丢包、突发、RTT、MTU），选码率并留 30%~40% 余量【推测】 |
| 2. 运行中代理上报给 PS5 | 中。机制存在（200 ms 上报 received/lost；PS5 回 target_bitrate；支持中途切分辨率），但 PS5 的响应策略未知 | 上报过高可能导致 PS5 过度降质（chiaki-ng 默认钳制 5% 是旁证） | **首要实验项**：注入不同丢包率，观察 target_bitrate、档位切换和恢复速度【推测】 |
| 3. 丢帧时上报 corrupt frame / 请求 IDR | 高（chiaki-ng 已实现） | IDR 带来码率尖峰，弱网下可能连锁丢包；chiaki-ng 默认关闭 IDR-on-FEC-failure | 先报 corrupt frame，持续冻结（如超过 300 ms）再请求 IDR，并限频【推测】 |
| 4. 网关缓冲 + 选择性重传 | 高 | 重传至少多 1 个 RTT | 网关保留最近约 200~300 ms 的包；按截止期决定是否重传；关键帧优先；结合逐跳 ARQ【推测】 |
| 5. 多路径冗余 | 高（有多台 VPS 时） | 带宽翻倍，或只多发 FEC | 参考 SRT broadcast、RIST bonding、RFC 10069。模型显示全冗余效果显著（§3.2）。可选两台不同运营商的 VPS，或"直连 + 中转"并发【推测】 |
| 6. 远端长时间恶化时重建会话降档 | 中 | 重建要重新握手和预测试，耗时未知 | 作为最后手段，必须实测重建耗时【推测】 |
| 7. 网关硬件转码 | 中 | 解码 + 编码各增加数毫秒（**未找到可靠的 1080p 实测数据**；NVIDIA 论坛有"1080p 低延迟预设约 1 ms/帧"的个例，4K 论文测的是端到端 5~7 帧），还会二次损失画质，需要带 QSV/NVENC 的设备 | 第二阶段的可选"弱网模式"：能直接控码率、做帧内刷新/RFI/LTR、降分辨率。默认透传【推测】 |

- 网关的纯透传计算开销很小：cm256 / reed-solomon-simd 是微秒级，raptorq 在树莓派 3B+ 上也有 200 Mbit/s 以上的编码速度。ARM 小板即可胜任【推测，基于 README 基准】。
- 但如果要转码，社区来源称树莓派 5 只有 HEVC 硬解、没有 H.264 硬解，硬件编码能力本次未核实，不适合当转码网关。x86 迷你主机（Intel 核显 QSV）更合适，具体延迟要实测。
- 网关到 PS5 **必须用有线**。客户端侧的 Wi-Fi 是最大的抖动来源，客户端应检测并提示【推测】。

---

## 6. 客户端技术栈

### 6.1 各平台的解码、渲染、音频和手柄

| 平台 | 硬解 | 低延迟渲染 | 参考证据 |
|---|---|---|---|
| Windows | D3D11VA（首选）、DXVA2 | D3D11 `DXGI_SWAP_EFFECT_FLIP_DISCARD`；关 VSync 时用 `ALLOW_TEARING` | moonlight-qt `ffmpeg-renderers/d3d11va.cpp:557,592-602`【已核实】 |
| macOS / iOS | VideoToolbox | Metal（`vt_metal.mm`）或 AVSampleBufferDisplayLayer（`vt_avsamplelayer.mm`）；chiaki-ng 1.10.1 在 macOS 用 MoltenVK 1.4.2 跑 Vulkan，支持 HDR | moonlight-qt 渲染器目录、chiaki-ng 发布说明【已核实】 |
| Android | MediaCodec（NDK AMediaCodec） | SurfaceView 直出；`KEY_LOW_LATENCY`（Android 11+，`FEATURE_LowLatency`）加厂商键（`vendor.qti-ext-dec-low-latency.enable` 等） | moonlight-android `MediaCodecHelper.java:220-223,436-439,502-503`【已核实】 |
| Linux / Steam Deck | VAAPI（AMD/Intel），也可用 FFmpeg 6.1+ 的 Vulkan 硬解（H.264/HEVC/AV1） | Vulkan + libplacebo（chiaki-ng 要求 libplacebo ≥7.349.0，`CMakeLists.txt:95`；libplacebo 最新 v7.387.0，LGPL-2.1）；也可 DRM/EGL 直出 | FFmpeg Changelog 6.1 条目；chiaki-ng `qmlmainwindow.cpp:11,44`【已核实】 |

- **音频**：PS5 下发 Opus，声道数、采样率和帧长都在音频头里（`lib/src/audio.c:17-20`、`opusdecoder.c:52`）【已核实】。输出端应使用小缓冲：Windows 用 WASAPI 共享或独占低延迟模式，Apple 用 AudioUnit，Android 用 AAudio 低延迟模式；也可以统一用 SDL3 音频。另外要做按时间戳的音画同步和丢包隐藏（Opus PLC/FEC）【推测】。
- **手柄**：
  - SDL3（Zlib，release-3.4.20，2026-11-03）的 PS5 HIDAPI 驱动支持陀螺仪和加速度计（`SDL_hidapi_ps5.c:862-863`）、触摸板、灯条，以及原始效果报文 `SendJoystickEffect`（`:1149-1153`），效果结构里包含左右扳机效果字段（`:176-177`），可以下发自适应扳机。扳机震动接口不支持（`:1029-1031`）。HD 触觉需要像 chiaki-ng 那样走 USB 声卡（`:735` 的 "Disable audio haptics" 位印证了触觉走音频通道）【已核实】。
  - iOS / macOS：GameController 框架提供 `GCDualSenseGamepad` 和 `GCDualSenseAdaptiveTrigger`，触觉用 `GCController.haptics`（`GCDeviceHaptics` + Core Haptics）。有开发者论坛帖反映 macOS 上设置扳机模式不生效【已核实（Apple 文档和论坛）】。把 PS5 的 PCM 触觉流映射成 Core Haptics 的效果未知【推测】。
  - Android：截至 2021 年的资料，内核驱动和平台**没有自适应扳机和 HD 触觉的 API**，2025~2026 年的情况未确认。可能的出路是 USB Host 或原始 HID【推测】。
  - Steam Deck：chiaki-ng 有 `steamdeck_native` 触觉实现，可参考【已核实】。

### 6.2 整体方案比较【推测，结合证据】

| 方案 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| C/C++ 核心 + 各平台原生 UI | 生态最直接（FFmpeg、SDL、libplacebo、chiaki 都是 C）；moonlight-qt 和 chiaki-ng 已经验证 | 内存安全风险；跨平台胶水代码多 | 可行，但不如 Rust |
| **Rust 核心 + 原生 UI**（**推荐**） | 协议、FEC、jitter buffer、统计这类网络代码最怕内存错误和数据竞争，Rust 正好防这些；生态现成（quinn、reed-solomon-simd、raptorq、str0m，均为 MIT/Apache/BSD）；可以用 UniFFI 或 cbindgen 导出给 Swift/Kotlin/C++；平台 API 有 objc2、ndk、windows-rs 绑定 | 跟 FFmpeg 和平台解码器的 FFI 有工作量；团队要会 Rust | **推荐**：核心（传输、FEC、重排、帧组装、时钟同步、统计、日志）用 Rust；解码和渲染调用平台原生 API |
| Qt / QML | 一套代码覆盖桌面端（moonlight-qt、chiaki-ng 都是这条路） | iOS 上要静态链接，LGPL 合规有争议（Qt wiki）；Qt Quick 渲染循环和低延迟视频的集成要额外处理（chiaki-ng 用 QVulkanInstance + libplacebo） | 只做桌面端时可选，移动端不推荐 |
| Flutter + 原生插件 | UI 开发快 | 视频要经 Texture（可能有额外拷贝或帧延迟，证据较弱）或 PlatformView（已知会冻结画面的坑）；渲染时序不受控 | 不推荐承载视频面；最多只做菜单和设置，视频用原生视图叠加 |

**推荐落地方式**【推测】：
- **网关**：Linux（x86 迷你主机或 ARM），复用 chiaki-ng 的 lib（C）做 Takion 客户端；转发层用 Rust 实现，或者整个网关都用 Rust 通过 FFI 调用 chiaki lib。注意 AGPL（§9）。
- **桌面客户端**（Windows / macOS / Linux / Steam Deck）：Rust 核心 + SDL3（窗口、输入、音频、手柄）+ FFmpeg hwaccel + 原生渲染器或 libplacebo。
- **移动客户端**：iOS 用 SwiftUI 壳 + VideoToolbox/Metal + GameController，Android 用 Kotlin 壳 + MediaCodec/SurfaceView，两者都链接同一个 Rust 核心。

---

## 7. 延迟与稳定性度量

### 7.1 业内怎么测

- **click-to-photon / glass-to-glass**：
  - NVIDIA LDAT：改装鼠标的按键信号直连控制器，亮度传感器贴在屏幕上，测按键到画面变化的时间【已核实】；
  - OpenLDAT：开源的光电二极管方案；
  - Bachhuber & Steinbach 的 G2GDelay：精度约 0.5 ms；
  - 传统方法是高速相机逐帧数，费时且受相机帧率限制（早期最多 200 Hz）。
- 对我们的场景【推测】：
  - 用微控制器模拟手柄按键（或接到 DualSense 按键触点上），同时打时间戳；
  - 光电二极管贴在**客户端屏幕**上，测按键到画面亮度突变（找一个按键会导致大面积亮度变化的场景，例如打开系统菜单）；
  - 同一套装置贴在 **PS5 直连电视**上测一遍，作为基线；两者之差就是串流增加的延迟；
  - 也可以用 240fps 以上的高速相机同时拍两块屏。
- 厂商数据只能参考：Parsec 自测局域网管线约 4~8 ms，测试方法是软件计时；Moonlight 社区数据口径不一，有的明确说不含渲染时间。必须自建测试台。

### 7.2 卡顿和帧时间抖动的口径

- **freeze（卡顿）**：W3C webrtc-stats（候选推荐草案，2025-09-25）定义为相邻两帧的渲染间隔 ≥ max(3×最近 30 帧平均帧时长, 平均帧时长+150 ms)；配套指标有 freezeCount、totalFreezesDuration、pauseCount、framesDropped、jitterBufferDelay【已核实】。建议直接采用，方便和业界横向比较。
- **帧时间抖动**：呈现间隔的标准差和 P95/P99；VSync 错过次数；"网络丢帧率"和"抖动丢帧率"分开统计（照 moonlight-qt 的统计浮层）。

### 7.3 软件应内置的统计、日志和诊断【推测，设计建议】

1. **每帧时间线**（网关和客户端各自打点，经时钟同步对齐）：
   - 网关侧：PS5 首包到达 → 末包到达 → PS5 FEC 还原 → 网关发出；
   - 客户端侧：首包到达 → 帧完整 → FEC 或重传补全 → 送入解码 → 解码完成 → 呈现。
   - 汇总出：单向网络时延、组帧等待、解码时间、队列等待、渲染时间（含 VSync）。
2. **网络统计**：RTT 和方差；单向时延和抖动（RFC 3550 算法）；丢包率；突发长度分布（据此估计 GE 参数）；乱序率；FEC 恢复数和失败数；NACK 次数、重传成功数、迟到数；双路径各自的到达率和先到率；有效码率；PS5 回报的 target_bitrate 和档位切换事件。
3. **事件统计**：corrupt frame 上报、IDR 请求、冻结（W3C 口径）、会话重建、路径切换、Wi-Fi 信号和频段变化（客户端能拿到的话）。
4. **输入延迟**：输入包往返时间（网关回 ACK 时带时间戳），按键到网关转发给 PS5 的时延。
5. **飞行记录仪**：内存里环形保存最近约 30 s 的逐包和逐帧事件；发生冻结或断流时自动落盘，可一键导出诊断包（脱敏）。
6. **结构化日志**（JSON Lines）+ 每会话汇总报表（P50/P95/P99）+ 屏上统计浮层（多档详细程度）。
7. **内置链路测试**：会话前或手动触发，测各条路径（直连、各 VPS 中转）的 UDP 带宽、丢包、突发、RTT 和 MTU，推荐档位和路径。
8. **测试台**：Linux `tc netem` 的 `loss gemodel p r 1-h 1-k` 可模拟突发丢包，`slot` 可模拟 Wi-Fi 式成批发送，`rate` 可限速（tc-netem(8)）【已核实】。用实测链路参数回放做回归测试。

---

## 8. 推荐方案汇总（供开发大纲参考）【推测】

1. **阶段 0（基线）**：
   - 用 chiaki-ng 直连 PS5 测局域网基线；
   - 通过 VPS 原样转发 Takion UDP（可套一层 UDPspeeder FEC）测公网基线；
   - 同时把 click-to-photon 测试台和 netem 测试台搭起来。
2. **阶段 1（网关透传 MVP）**：
   - 网关跑 chiaki lib 拿到完整帧；
   - 自研媒体协议：帧级 RS FEC（reed-solomon-simd 或 nanors）、截止期 NACK、AEAD；
   - 控制面用 QUIC stream（quinn）；
   - 桌面客户端：SDL3 + FFmpeg 硬解；
   - 内置统计浮层和日志。
3. **阶段 2（抗弱网）**：
   - 开会话前测速选档；
   - 代理上报实验和策略（核心实验）；
   - 逐跳 ARQ（VPS 中继）；
   - 双路径冗余；
   - 自适应冗余比例和最小冗余数；
   - 冻结时的 IDR 策略。
4. **阶段 3（全平台）**：iOS / Android 原生壳；DualSense 扳机、触觉、陀螺仪各平台的能力分级。
5. **阶段 4（可选）**：网关硬件转码的弱网模式；浏览器客户端（WebRTC，网关端用 str0m 或 pion）。

---

## 9. 风险与需要用户决定的事项

- **许可证**：
  - chiaki-ng 是 **AGPL-3.0**（源文件 SPDX 头为 `LicenseRef-AGPL-3.0-only-OpenSSL`）。网关链接它的 lib 就要遵守 AGPL，自用影响不大，分发时必须提供源码。客户端不接触 Takion，可以不受影响。
  - Moonlight/Sunshine 是 GPL-3.0，只适合参考思路，不宜复制代码。
  - Qt 在 iOS 上的 LGPL 合规问题见 §6.2。
- **专利**：Jerasure / GF-Complete 的 StreamScale 专利主张（US 8,683,296）；RaptorQ 有 Qualcomm 的 IPR 声明。分发前需要法律评估（本报告不构成法律意见）。
- **未知的 PS5 行为**：上报的响应策略、FEC 冗余比例、IDR 周期、是否有帧内刷新、STREAMINFO 下发几个档位、会话重建耗时，都需要实测。

---

## 10. 主要来源

**源码**（克隆在临时目录，提交见正文）：
- chiaki-ng：https://github.com/chiaki-ng/chiaki-ng
- moonlight-common-c：https://github.com/moonlight-stream/moonlight-common-c
- Sunshine：https://github.com/SunshineStream/Sunshine
- moonlight-qt：https://github.com/moonlight-stream/moonlight-qt
- moonlight-android：https://github.com/moonlight-stream/moonlight-android
- SDL 3.4.20 PS5 驱动：https://github.com/libsdl-org/SDL/blob/release-3.4.20/src/joystick/hidapi/SDL_hidapi_ps5.c

**标准**：
- RFC 9221：https://www.rfc-editor.org/rfc/rfc9221
- RFC 10069：https://www.rfc-editor.org/info/rfc10069
- draft-ietf-rmcat-gcc：https://datatracker.ietf.org/doc/draft-ietf-rmcat-gcc/
- RTP over QUIC：https://datatracker.ietf.org/doc/draft-ietf-avtcore-rtp-over-quic/
- SCReAMv2：https://datatracker.ietf.org/doc/draft-johansson-ccwg-rfc8298bis-screamv2/
- W3C webrtc-stats：https://www.w3.org/TR/webrtc-stats/
- tc-netem(8)：https://man7.org/linux/man-pages/man8/tc-netem.8.html
- RFC 6330 IPR：https://datatracker.ietf.org/ipr/1958

**传输库**：
- SRT：https://github.com/Haivision/srt（`docs/API/API-socket-options.md`、`docs/features/packet-filtering-and-fec.md`、socket groups：https://sources.debian.org/src/srt/1.5.4-3/docs/features/socket-groups.md）
- librist：https://code.videolan.org/rist/librist
- quinn：https://github.com/quinn-rs/quinn
- quiche：https://github.com/cloudflare/quiche
- msquic：https://github.com/microsoft/msquic（`docs/Platforms.md`）
- pion：https://github.com/pion/webrtc、https://github.com/pion/interceptor
- str0m：https://github.com/algesten/str0m
- webrtc-rs：https://github.com/webrtc-rs/webrtc
- libdatachannel：https://github.com/paullouisageneau/libdatachannel
- libwebrtc GoogCC：https://webrtc.googlesource.com/src/+/main/modules/congestion_controller/goog_cc/
- KCP：https://github.com/skywind3000/kcp
- kcp-go：https://github.com/xtaci/kcp-go
- ENet：https://github.com/lsalzman/enet
- GameNetworkingSockets：https://github.com/ValveSoftware/GameNetworkingSockets
- UDPspeeder：https://github.com/wangyu-/UDPspeeder
- SCReAM：https://github.com/EricssonResearch/scream

**FEC 库**：
- raptorq：https://github.com/cberner/raptorq
- wirehair：https://github.com/catid/wirehair
- cm256：https://github.com/catid/cm256
- leopard：https://github.com/catid/leopard
- reed-solomon-simd：https://github.com/AndersTrier/reed-solomon-simd
- nanors：https://github.com/ursoid-tech/nanors
- klauspost/reedsolomon：https://github.com/klauspost/reedsolomon
- OpenFEC 许可证：https://openfec.inrialpes.fr/patents.html
- GF-Complete 专利事件：https://www.techdirt.com/articles/20141115/07113529155/patent-troll-kills-open-source-project-speeding-up-computation-erasure-codes.shtml

**其它**：
- Parsec BUD：https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007
- Parsec 240fps 延迟：https://parsec.app/blog/parsec-game-streaming-total-latency-at-240-frames-per-second-c0818cc0daa5
- NVIDIA LDAT：https://developer.nvidia.com/nvidia-latency-display-analysis-tool
- G2GDelay：https://github.com/cbachhuber/G2GDelay
- Apple GCDualSenseAdaptiveTrigger：https://developer.apple.com/documentation/gamecontroller/gcdualsenseadaptivetrigger
- Apple GCDeviceHaptics：https://developer.apple.com/documentation/gamecontroller/gcdevicehaptics
- Android DualSense 支持（2021）：https://www.xda-developers.com/android-ps5-controller-support-getting-better/
- Qt 移动端许可：https://wiki.qt.io/Licensing-talk-about-mobile-platforms
- Sunshine 自适应码率实验分支：https://github.com/lesj0610/Sunshine/releases/tag/fork-2026.10.06
- NVENC 低延迟论坛帖：https://forums.developer.nvidia.com/t/nvenc-hevc-ultra-low-latency-with-ffmpeg-libraries-what-should-be-my-expectations/143954
- FFmpeg Changelog：https://github.com/FFmpeg/FFmpeg/blob/master/Changelog


---

## 附录：本方向的关键论断与待确认问题（复核结果见 V1/V2/V3）

- [E1] (high,已核实) PS Remote Play（Takion）的视频只靠每帧 Cauchy Reed-Solomon FEC（chiaki-ng 用 Jerasure 实现）抗丢包，客户端不对视频包做 NACK 重传；可靠重传只用于数据通道（200 ms 超时，最多 25 次）
  src: chiaki-ng@43728af2 lib/src/fec.c:12-15,59；lib/src/frameprocessor.c:69-79；lib/src/takionsendbuffer.c:12-14
- [E2] (high,已核实) 客户端每 200 ms 向 PS5 发一次 received/lost 拥塞包；chiaki-ng 默认把上报丢包率钳制在 5%（可在设置里调），历史上还试过禁用上报、改成 20% 上限
  src: chiaki-ng lib/src/congestioncontrol.c:5,24-39；gui/src/settings.cpp:741-748；提交 75fa64ad/9add78e3/3328fa38/53cb1ff2
- [E3] (high,已核实) PS5 端自带自适应：launchspec 含 network.bwKbpsSent/mtu/rtt 和 adaptiveStreamMode:"resize"；PS5 回传 CONNECTIONQUALITY（target_bitrate、rtt、loss 等）；视频包带 adaptive_stream_index，可在会话中途切换分辨率档位。具体策略未公开
  src: chiaki-ng lib/src/launchspec.c:23-29,77；lib/protobuf/takion.proto:285-292；lib/src/videoreceiver.c:157-170；lib/src/streamconnection.c:692-705,1000-1009
- [E4] (high,推测) 网关在有线局域网里几乎不丢包，应把远端链路的丢包和带宽状况代理进发给 PS5 的 Takion 拥塞包，借 PS5 内建的自适应降码率或分辨率；必须实测 PS5 的响应（观察 target_bitrate 和档位切换）
  src: 基于 E2、E3 源码证据的推断
- [E5] (medium,已核实) chiaki-ng 支持缺帧时上报 CORRUPTFRAME，以及 FEC 失败时请求 IDR（每 1000 ms 重试）；IDR-on-FEC-failure 默认关闭
  src: chiaki-ng lib/src/videoreceiver.c:9-11,195-196,235-256；gui/include/settings.h:302；lib/src/streamconnection.c:1263-1300
- [E6] (medium,已核实) PS Remote Play 档位最高 1080p，预设码率为 360p 2000、540p 6000、720p 10000、1080p 15000 kbps；PS5 支持 avc/hevc 和 SDR/HDR
  src: chiaki-ng lib/src/session.c:95-120；lib/include/chiaki/session.h:61-62；lib/src/launchspec.c:78-83
- [E7] (medium,已核实) Moonlight/Sunshine：视频 RS FEC（nanors），默认 20%，GF(2^8) 每块最多 255 分片；音频 4+2 FEC；视频无 NACK；用 IDR/RFI 恢复；控制通道用 ENet；截至 2026-11 上游主线没有自适应码率（只有分支里的实验实现）
  src: Sunshine@23faefde docs/configuration.md:2117-2142,2985-2989；moonlight-common-c@94c65fd3 src/RtpVideoQueue.c:269,332、src/RtpAudioQueue.h:13-14、src/ControlStream.c:1708-1745；github.com/lesj0610/Sunshine/releases/tag/fork-2026.10.06
- [E8] (high,已核实) RFC 9221 §5.4 规定 QUIC DATAGRAM 受连接拥塞控制器约束（发送方须推迟或丢弃）；用 QUIC 承载视频必须换成自定义拥塞控制，quinn 提供 congestion_controller_factory 可插拔接口，其 BBR 标注 Experimental
  src: https://www.rfc-editor.org/rfc/rfc9221 §5.4；quinn quinn-proto/src/config/transport.rs:342；quinn-proto/src/congestion/bbr/mod.rs
- [E9] (medium,已核实) Multipath QUIC 已在 2026 年 11 月发布为 RFC 10069（Proposed Standard，来自 draft-21），但不规定地址发现和多路径调度，调度要应用自己做
  src: https://www.rfc-editor.org/rfc/rfc10069.json；datatracker API
- [E10] (high,已核实) SRT Live 模式默认 120 ms TSBPD 固定接收延迟；FEC 是 SMPTE 2022-1 行列 XOR，支持 arq=always/onreq/never；Live 拥塞控制只按 INPUTBW×(1+OHEADBW 25%) 限速。这些特性面向广播，不适合做交互游戏的主媒体通道，但 onreq 和 broadcast bonding 值得借鉴
  src: Haivision/srt docs/API/API-socket-options.md:235,245,636-637,1372；docs/features/packet-filtering-and-fec.md:53,110-122
- [E11] (medium,已核实) KCP 是可靠有序字节流（kcp-go 可选 RS FEC，分片总数不超过 256），承载视频会产生队头阻塞，适合控制和输入通道
  src: skywind3000/kcp README；xtaci/kcp-go README §7
- [E12] (high,推测) 模型计算：固定 20% RS FEC 时，小帧（k=10, m=2）在 3% 独立丢包下约每 3 秒失败一次；Gilbert-Elliott 突发丢包（平均 3%、平均突发 4 包）下帧失败率 3~5%；两条独立路径全量冗余可降 1~2 个数量级
  src: 调研时的临时脚本 fec_calc.py（未随仓库发布；独立同分布与 GE 仿真假设）
- [E13] (medium,已核实) FEC 库性能足够、计算不是瓶颈：cm256 在 k=100、m=20 时编码约 224 µs（作者旧数据）；reed-solomon-simd 是 O(n log n)，32:32 编码约 10 GiB/s；raptorq 在树莓派 3B+ 上编码约 200~258 Mbit/s；wirehair 自述 N<128 时效率差，不适合每帧几十包的场景
  src: catid/cm256、AndersTrier/reed-solomon-simd、cberner/raptorq、catid/wirehair 的 README
- [E14] (high,已核实) SDL3（3.4.20）的 PS5 驱动支持陀螺仪和加速度计、触摸板、原始效果报文（含左右自适应扳机字段）；DualSense HD 触觉需要走 USB 声卡（chiaki-ng 把 PS5 的 3 kHz 触觉 PCM 重采样到 48 kHz 输出），蓝牙下只能退化为震动模拟
  src: SDL release-3.4.20 src/joystick/hidapi/SDL_hidapi_ps5.c:176-177,735,862-863,1149-1153；chiaki-ng gui/src/streamsession.cpp:43-45,1790
- [E15] (medium,已核实) iOS/macOS 的 GameController 框架提供 GCDualSenseAdaptiveTrigger 和 GCDeviceHaptics（Core Haptics）；有开发者论坛帖反映 macOS 上设置扳机模式不生效
  src: https://developer.apple.com/documentation/gamecontroller/gcdualsenseadaptivetrigger；https://developer.apple.com/forums/thread/771756
- [E16] (medium,已核实) 低延迟解码和渲染有现成做法：Android 用 KEY_LOW_LATENCY / FEATURE_LowLatency（Android 11+）加厂商低延迟键；Windows 用 D3D11 FLIP_DISCARD + ALLOW_TEARING；FFmpeg 6.1 起有 Vulkan H.264/HEVC/AV1 硬解
  src: moonlight-android MediaCodecHelper.java:220-223,436-439,502-503；moonlight-qt d3d11va.cpp:557,592-602；FFmpeg Changelog 6.1 条目
- [E17] (medium,已核实) W3C webrtc-stats 的卡顿定义：相邻两帧渲染间隔 ≥ max(3×最近 30 帧平均帧时长, 平均帧时长+150 ms)，可作为内置卡顿统计的标准口径
  src: https://www.w3.org/TR/webrtc-stats/（候选推荐草案，2025-09-25）freezeCount
- [E18] (high,已核实) chiaki-ng 是 AGPL-3.0，网关若链接其 lib 须遵守 AGPL；它依赖的 Jerasure/GF-Complete 在 2014 年因 StreamScale 主张专利 US 8,683,296 被作者撤下 2.0 版，分发前需要法律评估
  src: gh api repos/chiaki-ng/chiaki-ng（license AGPL-3.0）；chiaki-ng third-party/jerasure、gf-complete；Techdirt 2014-11-15 报道
- [E19] (high,推测) 推荐方案：媒体面自研 UDP 协议（帧级自适应 RS FEC 加最小冗余下限、截止期感知 NACK、VPS 逐跳重传、可选双路径冗余、AEAD），控制面用 QUIC stream（quinn）或 KCP，输入走不可靠加冗余；客户端用 Rust 核心加各平台原生解码和渲染壳
  src: 综合 E1-E17 的设计判断
- [E20] (low,推测) Flutter 的外部纹理和 PlatformView 路径存在额外拷贝、帧延迟或画面冻结风险（证据较弱），不建议承载视频面；Qt 在 iOS 上有 LGPL 静态链接的合规争议
  src: https://github.com/flutter/flutter/issues/110431；https://wiki.qt.io/Licensing-talk-about-mobile-platforms

## open_questions
- PS5 收到 Takion 拥塞包里的 received/lost 后实际怎么反应（降码率还是降分辨率、阈值多少、恢复多快）？CONNECTIONQUALITY.target_bitrate 会不会随上报变化？这决定代理上报方案是否成立，需要用 chiaki-ng 注入丢包做实测。
- PS5 视频的 FEC 冗余比例、IDR 周期、是否用帧内刷新或多参考帧、IDR 请求后的码率尖峰有多大？CORRUPTFRAME 上报后 PS5 是参考帧失效还是发 IDR？
- PS5 在 STREAMINFO 里实际下发几个 profile（分辨率档）？adaptiveStreamMode:"resize" 在什么条件下触发切档？
- 网关重建 Takion 会话（从断开到重新出画面）要多少秒？决定"恶化时重建会话降档"能否作为自动策略。
- 实际部署链路（跨运营商、直连与 VPS 中转）的丢包分布（随机还是突发，Gilbert-Elliott 参数）、RTT、抖动和 MTU 如何？需要先做链路测量工具采集数据。
- 如果要接入已有的自定义线路协议：它是否已有 FEC、多路径或加密？媒体层建在它之上，还是独立实现？
- 网关硬件选型：x86 迷你主机（Intel 核显 QSV）还是 ARM（RK3588、树莓派）？是否需要硬件转码的弱网模式？Intel N100 QSV 和 RK3588 MPP 的 1080p60 编解码实际延迟没找到可靠数据，需要实测；树莓派 5 的硬件编码能力未核实。
- 是否需要浏览器客户端（免安装）？这决定后期是否引入 WebRTC（网关端用 str0m 或 pion）。
- 许可证路线：网关会不会对外分发？（chiaki-ng AGPL-3.0 的源码义务）客户端是否上架 App Store？（Qt LGPL 静态链接问题、GPL 代码不能直接复用）Jerasure 和 RaptorQ 的专利风险是否在意？需要用户决定，必要时咨询律师。
- iOS 上能否把 PS5 的触觉 PCM 流有效映射为 Core Haptics 效果？macOS 设置自适应扳机不生效的问题是否已修复？Android 在 2025~2026 年是否有 DualSense 扳机和触觉 API，还是只能走 USB Host 或原始 HID？
- quinn 和 quiche 对 RFC 10069 Multipath QUIC 的实现进度如何？本次没查。
- 手柄输入经网关转发给 PS5 的额外延迟有多少？网关和 PS5 之间输入包的最优发送频率是多少（chiaki-ng 最小间隔 8 ms）？
- "VPS 原样转发 Takion UDP"作为基线，和"网关重封装"相比稳定性差多少？需要 A/B 实测，决定网关方案是否值得多加一台硬件。
