# 调研原文索引（2026-11-19）

调研方法：用一个多代理工作流做 5 个方向的并行联网调研，用到 WebSearch、gh API 和浅克隆源码；然后把各方向提出的关键论断按重要度取前 16 条，分 3 组做对抗复核，每组由一个独立代理重新查证。中间结论只要有出入，都以复核结果（V1/V2/V3）为准。

| 文件 | 内容 |
|---|---|
| `A-chiaki-family.md` | Chiaki 家族（原版、chiaki-ng、chiaki-ng-core、各平台移植）的现状，稳定性 issue 归纳，许可证分析，两条开发路线的评估 |
| `B-protocol.md` | PS5 Remote Play 协议源码级细节：端口、发现与唤醒、注册、会话、Takion、FEC、拥塞控制、输入、超时，以及对网关架构的约束 |
| `C-other-impls.md` | Chiaki 之外的实现（pyremoteplay、Ripcord、PXPlay、Asobi、官方 App、PS Portal），可借鉴的 Moonlight/Sunshine/Parsec/Xbox 设计，中继和网关类项目 |
| `D-wan-relay.md` | 公网直连、多主机、CGNAT、IPv6、MTU、WireGuard/Tailscale/ZeroTier、VPS 中转工具对比、国内 UDP QoS、PSN 打洞原理、唤醒 |
| `E-transport-client.md` | 传输协议对比（SRT、RIST、QUIC、WebRTC、KCP、自研），FEC 库与模型计算，拥塞控制，网关透传时的自适应手段，客户端技术栈，延迟与卡顿度量 |
| `F-takion-ack.md` | 2026-11-19 补充：M0 里「Takion received data ack with size 10」报错的根因（16 字节 SACK）、会话 ID 过短、ctrl 未知消息 0x41；chiaki-ng-core 的 Takion 实现对照；补丁 0004 的由来 |
| `V1-verify-projects.md` | 复核：开源项目现状、许可证、其它实现（A、C 的论断） |
| `V2-verify-protocol.md` | 复核：协议细节（B 的论断） |
| `V3-verify-network.md` | 复核：公网、中转、传输选型（D、E 的论断） |

说明：
- 源码行号都对应 chiaki-ng 提交 `43728af`（2026-11-03，v1.10.1）。
- 调研时克隆和下载的材料都在本地临时目录里，没有随仓库发布。需要源码时按各报告里的版本重新浅克隆即可。
- Reddit 原帖本机打不开，相关说法只来自搜索摘要，可信度低。
