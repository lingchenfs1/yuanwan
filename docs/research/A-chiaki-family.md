# Chiaki 家族现状与稳定性问题调研报告（方向 A）

> 调研日期：2026-11-19。只读调研，所有克隆和下载都放在本地临时目录（不随仓库发布），没有执行其中任何构建脚本。
> 标注约定：**【已核实】** 指本人读过源码、API 返回或原始 issue 文本；**【推测】** 指根据现象推断或项目自述、未经独立验证。
> 源码行号对应的版本：chiaki-ng `main` @ `43728af2`（2026-11-03，即 v1.10.1 之后的 HEAD），原版 Chiaki sourcehut `master` @ `ab55cf4`（2026-01-05），chiaki-ng-core `main` @ `6f109c3`（2026-11-16）。

---

## 0. 结论摘要

1. **主线已经换人维护**。原版 Chiaki 自己声明进入「仅维护模式」，实际在维护的是 **chiaki-ng**。它的仓库已从 `streetpea/chiaki-ng` 迁到 `chiaki-ng/chiaki-ng`，旧地址会重定向。最新正式版 v1.10.1 发布于 2026-11-03，约 3120 star，许可证 AGPL-3.0-only 加 OpenSSL 例外。【已核实】
2. **2026-11 有一个新变化**：chiaki-ng 组织新建了 **chiaki-ng-core**（用 C++23 重写的协议库，版本 2.0.0）和 **chiaki-ng-swift**（macOS 原生客户端），两者都在 2026-11-02 创建。它们的许可证是 AGPL-3.0-or-later，加 OpenSSL 例外，再加 **App Store 分发附加许可**，并要求贡献者签 CLA。原 chiaki-ng 仓库**没有** App Store 例外。【已核实】
3. **维护者在积极维权**。他们向 Apple 提了 DMCA，建了 `asobi-agpl-audit` 审计仓库。chiaki-ng-core 的 README 写明：用 AI 把它翻译成另一种语言，也算衍生作品。所以「只参考代码、换语言重写」**并不能自动规避 AGPL**。【已核实】
4. **码率控制在 PS5 端，客户端能做的有限**。客户端每 200ms 向主机上报收包数和丢包数，主机据此降码率、降分辨率（launchspec 里申请的是 `adaptiveStreamMode: resize`）。从日志样本看，PS5 每帧只带约 1 个 FEC 冗余单元（如 18+1、30+1），chiaki-ng 的实现里也没有音视频包的 NACK 重传。所以一旦突发丢包，基本只能靠「报告坏帧，请求关键帧（IDR）」来恢复。【已核实，FEC 比例来自日志样本】
5. **很多「稳定性」问题直到 2026 年才修**，而且不少是原版 Chiaki 遗留下来的：
   - 可靠包发送缓冲只有 16 个，网络短暂中断就溢出、无法恢复。直到 2026-11 的 v1.10.1 才改成 1024。
   - 音视频包原来完全没有重排序，2026-04 的 v1.10.0 才加上，超时只有 16ms。
   - 等关键帧时会永久冻结，v1.10.1 才修。
   - 重排序队列有释放后使用（use-after-free）的 bug，2026-10-31 才修。
   
   这说明这套代码的传输健壮性还在成熟过程中。【已核实】
6. **大量「卡顿」其实是客户端渲染问题，不是网络问题**。例如 v1.10.0 的帧节奏回归（#743），以及至今仍 open 的 +50ms 显示延迟回归（#807）。Steam Deck 上的周期性冻结，很大一部分来自 SteamOS 的 Wi-Fi 问题（ValveSoftware/SteamOS#1256，2023 年至今 open）。【已核实】
7. **对用户三种场景都友好，但有固定约束**：
   - 直连用的端口写死：TCP/UDP 9295、UDP 9296、UDP 9297，PS5 的发现和唤醒用 UDP 9302。
   - 主机不支持 IPv6 远程游玩。
   - 经 WireGuard 等路由隧道跨网段连接可行，但广播发现不跨网段，需要手动填 IP。
   
   【已核实】

---

## 1. 项目谱系与维护状态

### 1.1 主干项目

| 项目 | 地址 | 最近提交 | 最近 release | Star | 许可证 | 状态 |
|---|---|---|---|---|---|---|
| 原版 Chiaki | https://git.sr.ht/~thestr4ng3r/chiaki（GitHub 镜像 https://github.com/thestr4ng3r/chiaki 已归档，最后推送 2021-03-19） | 2026-01-05（`ab55cf4` Rewrite switch host management） | tag v2.2.0（提交日期 2023-08-20） | 镜像约 2223 | AGPL-3.0-only + OpenSSL 例外（`LICENSES/AGPL-3.0-only-OpenSSL.txt`） | README 自述功能已完成、只做维护，并引导用户去 chiaki-ng【已核实】 |
| chiaki4deck → chiaki-ng | https://github.com/chiaki-ng/chiaki-ng（`streetpea/chiaki4deck` 和 `streetpea/chiaki-ng` 都重定向到这里），文档 https://chiaki-ng.github.io/chiaki-ng/ | 2026-11-03（main），2026-11-14 有推送 | **v1.10.1（2026-11-03）**，之前是 v1.10.0（2026-04-03）、v1.9.9（2025-08-10）；另有 weekly canary（最近一次 2026-11-09） | 约 3118–3120 | AGPL-3.0-only + OpenSSL 例外（源码 SPDX 为 `LicenseRef-AGPL-3.0-only-OpenSSL`） | 活跃，但节奏是突发式的（见 1.4）【已核实】 |
| chiaki-ng-core | https://github.com/chiaki-ng/chiaki-ng-core | 2026-11-17 | 无 release，`VERSION.txt` 写 2.0.0 | 3 | AGPL-3.0-or-later + OpenSSL + **App Store 附加许可** | 2026-11-02 新建，C++23 重写。README 平台表：macOS 26+ 测试充分；iOS、Linux 能编译能跑；Switch（HorizonOS）测试充分；**Windows、Android 未编译未测试**【已核实】 |
| chiaki-ng-swift | https://github.com/chiaki-ng/chiaki-ng-swift | 2026-11-17 | 无 | 4 | 同 core（含 App Store 附加许可） | 2026-11-02 新建，目标 macOS 26+ Apple Silicon。维护者在 #878 里说 iOS 和 macOS 正在 TestFlight 测试，并计划上 App Store【已核实】 |
| akira（Switch） | https://github.com/chiaki-ng/akira | 2026-11-17 | v0.7.0（2026-11-16） | 264 | AGPL-3.0 | 活跃。用的是 chiaki-ng 的分支，**内置 WireGuard**，支持跨子网单播发现【已核实，README】 |

### 1.2 各平台移植（用 gh search repos 加 WebSearch 汇总）

| 平台 | 项目 | 最近推送 / release | Star | 许可证 | 备注 |
|---|---|---|---|---|---|
| Android | 原版 Chiaki 的 Android 版（F-Droid `com.metallic.chiaki`） | 随原版进入维护模式 | — | AGPL | 原版 README 写明从 F-Droid 安装【已核实】 |
| Android | chiaki-ng 仓库里的 `android/` 目录 | 有代码，但 v1.10.1 发布资产里**没有 Android 包** | — | AGPL | 有十几个 Android PR 积压未合（#819–#831、#854）；#853 在讨论把 Android 工作放进 core 2.0【已核实】 |
| Android | Geocld/PeaSyo | 2026-09-15 / v2.0.0（2026-06-04） | 392 | AGPL-3.0 | 仓库里的 v1 代码含 chiaki 的 `lib/src`。README 自称 **v2 已用 Rust 完全重写，不再依赖 Chiaki 协议实现**，但 PeaSyo-rs 仓库只有 README、没有源码；iOS 版在 App Store 收费【已核实仓库内容；「完全重写」属项目自述，**推测**】 |
| Android | yazanj082/momiji | 2026-11-15 / v1.1.3 | 18 | AGPL-3.0 | 最高 1080p60，H.264/HEVC，最高 100 Mbps，支持 PSN 远程、低延迟解码模式【README】 |
| Android | Axixi2233/chiaki-android、gameblabla/chiaki-up 等 | 2026-03 / 2026-01 | 81 / 9 | GPL-3.0 / AGPL | 小型分支 |
| 多平台 | ForWard-Technologies-LLC/Pylux | 2026-08-14 / v2.11.4 | 95 | AGPL-3.0-only-OpenSSL | 基于 chiaki-ng；已上 Google Play、App Store、Mac App Store、Flathub；还做了 PS 云游戏（Internet Play）【README】 |
| iOS/tvOS | Teuns/Chiaki-4-iOS | 2023-08 停更 | 36 | 未声明 | 早期移植，已停更 |
| iOS/macOS | chiaki-ng-swift；ab300819/chiaki-apple | 2026-11 | — | AGPL | ab300819 已表示会停掉自己的版本，转去贡献官方 swift 版（#878）；chiaki-ng 也已合入 iOS/tvOS 编译支持（PR #712）【已核实】 |
| Nintendo Switch | chiaki-ng/akira；原版 Chiaki 的 `switch/`；thinkzhou/LunarNX | 2026-11 / 2026-01 / 2026-11 | 264 / — / 43 | AGPL / AGPL / 其他 | v1.10.1 发布说明指明 Switch 用户去用 akira |
| PS Vita / PSTV | ywnico/vitaki-fork（已停止大改）→ **mauricio-gg/vitaki-vitarps5（VitaRPS5）** | 2026-02 / **2026-11-01 v4.2.2** | 79 / 120 | AGPL-3.0 | vitaki-fork 不支持打洞，远程靠公网 IP 加端口转发；VitaRPS5 为主要后继，README 自述大量借助 AI 开发【README】 |
| Xbox Series（UWP Dev Mode） | Ibbolufc/XPort、yoni101087/psxbox | 2026-10 | 0 / 0 | AGPL-3.0 | 都处于早期：XPort 自述还不能发现、配对或串流【README】 |
| Web / WASM | CSystem77/Chiaki-NG-Web（上游曾合入 PR #815，又被 #834 回滚） | 2026-10-29 / v1.10.4 | 8 | AGPL-3.0 | 把 chiaki-lib 用 Emscripten 编成 WASM，所有 socket 调用经 Node 代理（WebSocket）转发，浏览器端用 WebCodecs 解码；不支持 PSN 打洞【README】 |
| LG webOS | renjfk/hotaru-webos、TacticalBill/chiaki-lg | 2026-11 / 2026-10 | 14 / 18 | AGPL / 未声明 | 早期 beta |
| 破解版 PS5 | saawant12/orbit-play-ps5 | 2026-11-14 | 31 | AGPL | 面向越狱 PS5，号称无需 PSN 登录，场景不同 |
| 闭源同类 | PXPlay（原 PSPlay，streamingdv/PSPlay-Application-Hosting 仅放发布包） | v3.2.0（2026-11-11） | 352 | 闭源 | 作者 grill2010 是 chiaki-ng PSN 打洞协议规格的主要贡献者（chiaki-ng-core README 致谢） |

### 1.3 有争议的闭源商用客户端（与许可证直接相关）

- **Asobi**（Mac App Store，付费闭源）：#725。开发者先否认用了 Chiaki 代码。维护者 2026-10-11 发长文质证，指出 Asobi 早期版本附带的开发笔记自称是 Chiaki 协议的纯 Swift 版，日志里引用了 chiaki-ng 的文件名和行号，还有 chiaki-ng 特有的端口猜测参数、密钥位置算法等实现细节；并建了 https://github.com/chiaki-ng/asobi-agpl-audit 。【已核实，issue 原文】
- **Portal: Remote Play**（App Store 订阅制）：#790，二进制里有 57 个 `iris_*` 类型名与 `chiaki_*` 一一对应；据 issue 补充，开发者已在 Reddit 承认含 Chiaki 代码。【已核实 issue 内容；「已承认」系转述】
- **Console Remote**（Mac App Store，7.99 美元）：#884（2026-11-17），据称把 `chiaki_` 批量改名为 `psmirror_`。【已核实 issue 内容】

### 1.4 chiaki-ng 维护健康度

- 贡献者提交数：streetpea 1313、thestr4ng3r（原作者）601、nowrep 135，其余都在 35 以下。巴士因子很低，基本靠 streetpea 一个人。【已核实，GitHub contributors API】
- 近 12 个月每月提交数：2025-12 16，2026-01 5，2026-02 1，2026-03 135，04 40，05 41，06 2，07 1，08 11，09 12，10 25，11 87。节奏高度突发。【已核实】
- 从 1.10.0 到 1.10.1 隔了 7 个月，用户在 #851 抱怨正式版落后 main 107 个提交。【已核实】
- 当前 open issue 35 个，open PR 约 30 个（大量 Android PR 积压）。Discussions 未开启，社区在 Discord。【已核实】

---

## 2. 许可证分析（重点）

### 2.1 各代码库的许可证

| 代码 | 许可证 | App Store 例外 |
|---|---|---|
| 原版 Chiaki | AGPL-3.0-**only** + OpenSSL 例外 | 无 |
| chiaki-ng（含 `lib/`，即 libchiaki） | AGPL-3.0-only + OpenSSL 例外（`COPYING` 末尾只有 OpenSSL 一条附加许可） | **无** |
| chiaki-ng-core / chiaki-ng-swift | AGPL-3.0-**or-later** + OpenSSL + App Store 附加许可（`LICENSE` 第 662 行起） | **有** |
| 第三方依赖 | jerasure / gf-complete（BSD 类，streetpea 的分支）、nanopb（zlib）、FFmpeg（LGPL/GPL，看编译选项）、libplacebo（LGPL-2.1）、SDL（zlib）、Qt 6（LGPL/GPL/商业）、OpenSSL、curl、json-c、miniupnpc、libevent | — |

【已核实：COPYING、LICENSE、.gitmodules、CMakeLists.txt】

### 2.2 AGPL-3.0 对「复用、闭源分发、上架」的具体影响

1. **只自己用、不分发**：AGPL 第 2 条允许对不转让（convey）的作品自由修改和运行，没有附加义务（见 `COPYING:152`）。如果这个软件只给用户自己用，复用 chiaki-ng 代码在许可上几乎没有成本。【已核实条文；适用性需用户确认使用范围】
2. **分发给他人（包括免费分发）**：整个组合作品（客户端加链接进来的 libchiaki）必须以 AGPL 发布，并向接收者提供完整对应源码。**不能闭源分发**。收费是允许的。【已核实条文】
3. **第 13 条网络交互**：如果修改过的程序通过网络和用户交互（比如 Chiaki-NG-Web 那种「服务端 Node 代理加浏览器」形态，或者服务器上跑着含 chiaki 代码、终结协议的中转程序，并且有他人通过网络使用），就要向这些远程用户提供源码（`COPYING:540-551`）。**纯 L3/L4 转发的中转服务器**（WireGuard、UDP 端口转发、用户自研的线路协议）不包含 chiaki 代码，不触发这一条。【条文已核实；具体是否构成「交互」属法律判断，**推测**】
4. **聚合作品**：独立程序之间只通过 socket 或进程边界通信，属于「aggregate」，AGPL 不会传染到其他部分（`COPYING:227-231`）。也就是说，自研的隧道或中转进程可以用任何许可证，前提是不复制 chiaki 代码。【条文已核实；边界划分属法律判断】
5. **App Store**：FSF 的立场是 App Store 的使用条款构成 GPL 禁止的「进一步限制」。2011 年 VLC 就因此被下架（见 Computerworld、Engadget 2011 年报道）。原 chiaki-ng 代码没有 App Store 例外，所以严格说不能上 App Store。chiaki-ng-core 在 2026-11 专门加了第 7 条附加许可，允许经 App Store / TestFlight 分发，但前提仍是**开源、AGPL，并向接收者提供源码**。【已核实条文；VLC 案出自媒体报道】
6. **CLA**：向 chiaki-ng 组织（akira 和几个第三方分支除外）贡献代码，要签 chiaki-ng Fiduciary License Agreement 2.1（https://gist.github.com/streetpea/d97e8adea343ab183cb31ab78139fcd8 ），把经济权利授予 streetpea。维护者在 #878 说，已经征得「旧 core 合作者」同意改成可上 App Store 的许可。**原作者 Florian Märkl（601 次提交）是否同意，没有找到明确证据**。【已核实 CLA 文本；改许可的完整授权链**待确认**】
7. **「换语言重写」的风险**：chiaki-ng-core 的 README 明确举例，用 AI 把代码翻译成另一种语言，即使没有链接任何原代码，也属于衍生作品。Asobi 一案中，维护者把非协议必需的实现选择当作抄袭证据，比如 STUN 服务器列表、端口猜测公式、密钥位置的 `size + size % 16` 调整。【已核实】

### 2.3 对本项目的含义（给主代理的输入）

- 如果只是自用，或者打算以 AGPL 开源发布：直接基于 chiaki-ng（或 chiaki-ng-core）是最省事的。
- 如果想闭源、商用或上 App Store 闭源：不能复用任何 chiaki 代码，协议实现要做「净室」流程（一组人写协议规格，另一组人照规格实现），并留存开发记录。即便如此，chiaki-ng 维护者仍可能质疑，参考 Asobi 案。
- 稳定性改造的大头可以放在**独立进程或独立服务器**里（自研隧道、中转、FEC/ARQ），它和 AGPL 客户端是聚合关系，许可证可以自选。【**推测**，需法律确认】

---

## 3. chiaki-ng 功能清单与技术栈

### 3.1 功能清单（v1.10.1 / main @ 43728af2）

| 功能 | 支持情况 | 出处 |
|---|---|---|
| PS4 / PS5 | 都支持，含 PS5 Pro（#404，2024-11 关闭） | 源码、issue |
| 编码 | H.264、H.265、H.265 HDR（PS5 专属） | `lib/include/chiaki/common.h:98-100` |
| 分辨率预设 | 360p / 540p / 720p / 1080p，没有 1440p/4K。Sony 官方说明 PS5 远程游玩最高 1080p | `lib/include/chiaki/session.h:59-62`；Sony 支持文章 SX365201 |
| 默认码率 | 360p 2000、540p 6000、720p 10000、1080p 15000（kbps）；PS5 本地默认 1080p，远程默认 720p | `lib/src/session.c:92-136`；`gui/src/settings.cpp:457-459` |
| 码率滑块 | 2–100 Mbps（这是「申请值」，PS5 不一定给，也不是恒定码率；akira README 也是这么说的） | `gui/src/qml/SettingsDialog.qml:1348-1349` |
| 帧率 | 30 / 60 | `session.h:67-68` |
| HDR | 需选 H265 HDR，并且主机连接 HDR 显示器（无头 HDR 有变通方法，见 #686）。macOS 在 1.10.1 改回 Vulkan 后恢复 HDR | `docs/setup/configuration.md:318-334`；1.10.1 发布说明 |
| DualSense 触觉 / 自适应扳机 | 支持：USB 有触觉和扳机，蓝牙只有扳机（Linux 蓝牙触觉 PR #858、#872 未合）；Windows 上不正常（#759 open） | `docs/updates/done.md`；issue |
| 陀螺仪 / 触控板 | 支持（含 Steam Deck 原生陀螺仪接口） | `done.md` |
| 麦克风 | 支持（Opus 编码，可选 speex 回声消除）；1.10.1 起每个麦克风帧在后两个包里重发 | `lib/src/audiosender.c`、`opusencoder.c`；1.10.1 发布说明 |
| 远程唤醒 | 支持：向 9302/987 发送 `WAKEUP` 加 user-credential；1.10.1 起在等待主机唤醒期间重发 | `lib/src/discovery.c:76-83,436` |
| PSN 远程（打洞） | 支持（UPnP → STUN → UDP 打洞，含可配置的端口猜测）。**本项目不用** | `lib/src/remote/holepunch.c`（6348 行） |
| 手动 IP / 公网直连 | 支持（手动添加主机；主机地址可用 FQDN，见 PR #631） | `docs/setup/remoteconnection.md` |
| 自定义端口 | **不支持**：9295/9296/9297 在源码里写死 | `session.c:29`、`ctrl.c:32`、`streamconnection.c:31`、`senkusha.c:29` |
| IPv6 | 客户端代码地址里带冒号就按 IPv6 解析，但**主机不支持 IPv6 远程游玩** | `session.c:229-237`；`remoteconnection.md:5`；#319 |
| 代理（SOCKS5 等） | 不支持；#594 维护者说工作量大，已因不活跃关闭 | #594 |
| 统计叠加层 | 码率、平均队列深度、等待帧龄、丢包、丢帧、丢失帧 | PR #741 |
| 平台 | Linux（Flatpak、AppImage x86_64/arm64）、Windows x64/arm64、macOS 15+（仅 Apple Silicon）；Switch 走 akira | v1.10.1 发布资产 |

### 3.2 技术栈

- **核心库 `lib/`**：C 语言，约 2.7 万行（`lib/src` 18974 行，加 `lib/src/remote` 8155 行），原版约 1.37 万行。依赖：OpenSSL（可换 mbedTLS）、nanopb（protobuf）、jerasure / gf-complete（FEC）、FFmpeg（解码）、Opus、curl、json-c、miniupnpc、libevent（后几项用于 PSN 打洞）。【已核实，`wc -l`、CMakeLists.txt、.gitmodules】
- **GUI `gui/`**：Qt 6 + QML（另有可选的 WebEngine，用于 PSN 登录）。渲染用 libplacebo（要求 ≥7.349.0），后端是 Vulkan，1.10.0 起加了 OpenGL 后备，1.10.1 起支持 `--opengl`。输入用 SDL（1.10.1 起 AppImage 自带 SDL3，也有 sdl2-compat）。【已核实】
- **硬件解码**（FFmpeg hwaccel）：vaapi、vulkan、cuda、d3d11va、videotoolbox，有分支在尝试 v4l2m2m（PR #880）。自动选择逻辑在 `gui/src/qmlbackend.cpp:990-1055`。【已核实】
- **协议流程**：UDP 发现 → 注册（UDP 搜索 9295，加 TCP 9295 HTTP；需要 PIN 和 PSN Account ID）→ 会话请求（TCP 9295）→ ctrl（加密 TCP 9295）→ Senkusha（UDP 9297，测 RTT 和 MTU）→ Takion 流（UDP 9296，类 SCTP 的可靠数据通道加不可靠音视频，GKCrypt 加密，ECDH）。【已核实，源码】

---

## 4. 稳定性问题归纳（高频问题、根因与状态）

> issue 链接统一写成 https://github.com/chiaki-ng/chiaki-ng/issues/编号 。我在约 40 个关键词下检索到 462 个去重 issue，按评论数排序后精读了约 50 个。

### 4.1 断流与卡顿

| 现象 | 根因 | 状态 | issue |
|---|---|---|---|
| 玩 5~20 分钟后冻结或卡死（2023–2024 年高频） | (a) Takion 可靠通道发送缓冲只有 16 个包，网络短暂中断时重传堆积并溢出，之后新包也被丢弃；原版代码里还留着 `TODO: check tries and disconnect`。(b) Steam Deck 从睡眠唤醒后 Wi-Fi 异常，切换一次 Wi-Fi 就好 | (a) v1.10.1 改为 1024 个（提交 `3e1a24dd`）；(b) 属 SteamOS 问题，未解决 | #39、#46、#103、#130、#230；SteamOS#1256（open） |
| 画质越玩越差、「要稳定 5 分钟」 | 客户端上报丢包后 PS5 主动降码率和分辨率（`adaptiveStreamMode: resize`）。chiaki-ng 从 1.7.2 起把上报的丢包率截顶（默认 5%）来「骗」主机少降一点。维护者还推测，一次劣质会话会影响主机 RP 服务端后续的状态，要让主机进入休眠才能复位【后者为**推测**】 | 截顶已实现；根本机制在主机端，客户端改变不了 | #230、#516 |
| v1.10.0 在 60Hz 屏上卡在 53–55 FPS 并持续顿挫 | 1.10.0 新的 VSync 显示计时用了毫秒精度的 QTimer，有舍入误差；帧混合和帧节奏重构 | 1.10.1 改为默认直接帧映射、关闭 VSync 和帧混合，修了计时 bug；有用户确认修好（2026-11-04），issue 仍 open | #743 |
| 显示延迟比 1.10.0 多约 50ms（120fps 摄像机实测 7 帧→13 帧） | 2026-04 起的显示层重构改为「按屏幕时钟调度」，队列常驻约 2 帧，再加延迟交换线程 | **open**；维护者给了 direct frame 测试版，1.10.1 默认开启，是否解决未确认 | #807 |
| Deck OLED 玩 10 分钟后严重变慢 | 1.10.0 之后的 weekly 有回归，另有 Wi-Fi 省电问题 | 1.10.1 后用户反馈好转，仍有「连接差」短卡；**open** | #775 |
| 有线 LAN 也丢包、卡 | 软解码在高码率（如 100 Mbps）下 CPU 不够；NVIDIA d3d11va 和 Vulkan 有驱动问题 | 只给了规避建议 | #726 |

### 4.2 花屏与马赛克

- **FEC 恢复能力很弱**：日志显示每帧只有约 1 个冗余单元（#785：`16+1 / 18+1 units`；PR #697：`20+0 / 30+1 units`）。同一帧丢 2 个以上单元，FEC 就失败。日志格式定义见 `lib/src/frameprocessor.c:208`。【已核实日志样本；「主机总是只给 1 个冗余单元」属**推测**】
- FEC 失败后客户端有一个「参考帧替换」技巧：直接改写码流里的参考帧索引，指向更早的帧（`videoreceiver.c:297-322`）。但 HEVC 解码器经常报 `Could not find ref with POC`，画面会花到下一个关键帧为止（#785 日志）。
- 2026-04 之前音视频包完全不做重排序，到达即处理，乱序到达就花屏（PR #741 的问题描述）。现在有重排序，但超时 16ms（`takion.c:47`）。
- 渲染相关：Wayland 下的图形噪点（#681，建议改用 X11 或 OpenGL）；Windows 花屏（#789 open，改用 OpenGL 可缓解）；Intel 的 vaapi/Vulkan dmabuf 互通问题（#411）。
- 弱 Wi-Fi 会出现白屏或绿屏闪烁、声音断续，官方建议降到 720p 或 30fps（`configuration.md:449-467`）。

### 4.3 掉线与重连

- **桌面端没有会话中途自动重连**：`resume_session` 只在客户端设备休眠后唤醒时触发（`gui/src/qmlbackend.cpp:572-579`）。Android 分支（neilcorp2kx）自己实现了「掉线、切换 Wi-Fi 或移动网络后自动重连」，尚未进上游（#853）。【已核实，grep 加代码阅读】
- **主机端 RP 服务卡死**：「Remote Play on console has crashed」或「already in use」之后，只有让主机进入休眠才能复位，客户端无法自救（#38、#662）。
- PSN 通道：1.10.1 起在遇到坏包时保持 ctrl 不断，并定时发心跳回复。代码注释说明主机 30 秒收不到心跳回复就结束会话（`lib/src/ctrl.c:36-45`）。
- 可靠包重传：每 200ms 一次，最多 25 次，约 5 秒（`takionsendbuffer.c:12-14`）；原版是 10 次。

### 4.4 丢包与高延迟

- 拥塞反馈：每 200ms 发送一次收/丢包计数（`congestioncontrol.c:5,21-41`），上报的丢包率被截到 `packet_loss_max`（默认 0.05，见 `settings.cpp:740-747`）。
- 主机会下发 `CONNECTIONQUALITY` 消息（含 target_bitrate、rtt、loss），客户端只记日志，不据此调节（`streamconnection.c:693-707`）。
- 之前默认开 VSync 会增加延迟，2026-03 起改为默认关闭（#728 评论）。

### 4.5 注册失败

- 跨子网或 VPN：注册时的 UDP 搜索（广播或单播 9295）超时，先在局域网注册、再手动加内网 IP 即可（#286）。
- Windows 上「Bad file descriptor」，HOST 头默认填了 10.0.2.1x（#867 open）。
- 注册需要 PIN 和 PSN Account ID（`lib/include/chiaki/regist.h:28-38`）。账号 ID 可通过一次 PSN 登录获得，或用公开资料查询（PR #484）。注册之后不再依赖 Sony 服务器。

### 4.6 PS5 固件更新导致的兼容问题

- 2026-03 的 PS5 固件（26.02-13.00）弄坏了 PSN 远程通道，一天内在 weekly 修复（#728、#731）。
- 2026-09，PS5 Pro 固件 13.60 返回的 `customData1` 变成 17 字节（#810），同样只影响 PSN 打洞路径。
- 本次检索到的固件相关故障**都集中在 PSN 路径**，局域网和手动直连路径受影响较少。【**推测**，样本有限】

### 4.7 远程（公网）连接失败

- CGNAT 无法做端口转发（文档有说明）；主机不支持 IPv6（#319）。
- MTU 问题：Senkusha 探测范围 576–1454（`senkusha.c:265,272`），失败时回退到 1454（`session.c:641-645`）。#252 中 MTU 设错，加上 9296 回包收不到，导致「Unknown Error in Stream Connection」。**走隧道时（WireGuard 默认 MTU 1420）必须让探测生效，或者调低 MTU**。【已核实】
- VPN 或 Tailscale：先在局域网注册、再手动加内网 IP 即可连上；广播发现不跨子网（#286）。

### 4.8 硬解问题

- vaapi 和 Vulkan 零拷贝在部分 Intel 驱动上黑屏（#411）。
- NVIDIA d3d11va 和 Vulkan 的驱动问题（#726）。
- macOS 上 MoltenVK 与 Qt 版本不匹配导致崩溃（#729、#746），1.10.1 改为自带 MoltenVK 1.4.2。
- AppImage 自带的 NSS 或 SDL 和系统冲突（#855、#850，1.10.1 已修）。

### 4.9 Steam Deck 与其他掌机

- Wi-Fi：SteamOS#1256 自 2023-11 至今 open；维护者建议关闭 Wi-Fi 省电，或用插件锁定 Wi-Fi、禁止后台扫描（#230、#775）。PR #879 想在串流期间禁止 Wi-Fi 扫描，仍是 WIP。
- 掌机启动黑屏，如 Ayaneo（#521 open，51 条评论）；SteamOS 3.6 升级后崩溃（#444）；游戏模式下崩溃（#553）。

---

## 5. 现有缓解手段与局限

| 手段 | 实现位置 | 局限 |
|---|---|---|
| FEC（Cauchy RS，jerasure） | `lib/src/fec.c:5,14`；`frameprocessor.c` | 冗余比例由主机决定，样本中约 1 单元/帧，扛不住突发丢包 |
| 坏帧报告（corrupt frame） | `videoreceiver.c:195-198,229-262` | 依赖主机响应 |
| FEC 失败时立即请求 IDR（「Fast IDR」） | PR #697（2026-01），`videoreceiver.c:235-262` | **默认关闭**（`gui/include/settings.h:302`）；1.10.1 起每 1 秒重发一次 IDR 请求（`videoreceiver.c:9-11`） |
| 参考帧替换 | `videoreceiver.c:297-322` | 常导致 HEVC 参考帧错误、出现花屏 |
| 拥塞上报与丢包截顶 | `congestioncontrol.c` | 只能间接影响主机编码器 |
| 音视频重排序队列 | `takion.c:45-47,974-1090`（PR #741，2026-04） | 超时固定 16ms，公网抖动大时可能把迟到的包当丢包【**推测**】 |
| 可靠通道重传 | `takionsendbuffer.c`、`takionsendbuffer.h:21` | 1024 的缓冲是 2026-11 才改的 |
| Senkusha 测 MTU 和 RTT | `senkusha.c` | 失败就回退 1454 |
| 自动降档（video_profile_auto_downgrade） | `ctrl.c:1498` | 只在主机不支持所请求档位时生效 |
| 码率、分辨率、帧率可调 | GUI | 码率只是「申请值」 |
| 统计叠加层 | PR #741 | 主要用于诊断 |

**结构性限制（客户端改不了的）**：编码器的码率控制、FEC 冗余比例、关键帧间隔、自适应降分辨率都在 PS5 端。chiaki-ng 实现里也没有音视频包的 NACK 重传（grep 未发现）。所以要显著提升公网稳定性，最可行的地方是**网络层**：自研隧道的 FEC/ARQ、多路径、抖动缓冲，以及客户端的自适应重排序超时、IDR 策略、自动重连。【**推测**，但有上面的事实支撑】

---

## 6. 至今未解决或存疑的问题（截至 2026-11-19）

- #807 显示延迟回归（open）；#775 Deck OLED 变慢（open）；#743 名义上仍 open（已有用户确认修复）。
- #709 Mac 上冻结（open，日志显示 `InvalidSessionId`）；#746 macOS 1.10.0 多次崩溃（open）；#789 Windows 花屏（open）；#521 Ayaneo 黑屏（open）；#759 Windows 上触觉和扳机异常（open）。
- 没有通用自动重连；不支持代理；主机不支持 IPv6；主机 RP 服务卡死只能靠主机休眠复位。
- chiaki-ng-core 2.0 才刚起步（Windows、Android 未编译），C 版 lib 与 C++ core 之后是否双线维护还不清楚。

---

## 7. 路线评估

### 路线 A：以 chiaki-ng（或 chiaki-ng-core）为基础二次开发

- **工作量**：最小。PS5、HEVC、HDR、触觉、扳机、麦克风、唤醒、注册都是现成的，可以把精力全放在稳定性上：隧道、中转、自动重连、IDR 策略、抖动缓冲、渲染低延迟模式。
- **许可**：必须 AGPL 开源（自用可以不公开）。想上 App Store，应基于 chiaki-ng-core，因为它有 App Store 例外；但 core 新、平台覆盖不全，授权链也有待确认（见开放问题）。
- **可维护性**：
  - 利：能持续受益于社区对 PS5 固件变化的快速跟进（如 #728 一天修好）。
  - 弊：上游节奏突发、维护基本靠一个人；C 版 lib 正在被 C++23 core 取代，基座在移动；合并上游会有冲突，Pylux 的 README 也坦言 Qt 桌面端合并痛苦。
- **稳定性改造空间**：中等偏高。传输层和渲染层都能改，但受限于 PS5 端协议。上游最近一年才修掉好几个基础性 bug，说明代码里可能还有隐藏问题，需要自己做充分的故障注入测试。

### 路线 B：只参考协议，从零实现核心库

- **工作量**：大。要实现发现和唤醒、注册（rpcrypt 密钥派生，原文件 2428 行）、会话和 ctrl 加密、Takion（含 GKCrypt、ECDH、可靠通道、重排序）、Senkusha、protobuf 消息、H.264/HEVC 切片处理和 FEC、Opus、手柄反馈和历史、触觉、扳机、麦克风。光 C 核心就有约 2.7 万行可作参照，PSN 打洞部分可以省掉。PeaSyo v2 自称做了 Rust 全量重写，说明可行，但其源码未公开，真实性无法核实。
- **许可**：理论上可以自选许可证、闭源。但必须严格走净室流程；协议常量和密钥表这类「事实」能否自由使用存在法律灰区；chiaki-ng 维护者有主动追究的先例（Asobi）。【**推测**】
- **可维护性**：PS5 固件变化要自己跟进，失去社区的快速修复；但架构完全自主，比如统一异步 I/O、可测试的状态机、内置重连和多路径。
- **稳定性改造空间**：最高，但改不了 PS5 端，所以上限和路线 A 在协议层是一样的。真正的增益来自架构质量和网络层。

### 路线 C（折中，供主代理参考）：AGPL 客户端加独立的自研网络层

- 客户端：直接用 chiaki-ng（或 core）的协议库，以 AGPL 开源，或仅自用。
- 自研隧道或中转：独立进程或服务器，负责 FEC、ARQ、多路径、加密，可以用任何许可证。它对 PS5 来说是透明的三层或四层链路。
- 已有自建 VPS 和自定义线路时，这条路线最容易落地。【**推测**，是建议，不是事实】

---

## 8. 对后续设计的直接输入

1. 主机端口固定：TCP 9295、UDP 9295、9296、9297、9302（PS4 为 987）。中转方案要么透明转发这些端口，要么做三层隧道。客户端只能填 IP 或 FQDN，不能填端口（chiaki-ng 现状）。
2. 隧道 MTU：Senkusha 探测上限 1454，隧道 MTU 不够时要确保探测生效，或在客户端强制设低。
3. PS5 不支持 IPv6 远程游玩，隧道内部要走 IPv4。
4. 注册只需在局域网做一次（PIN 加 Account ID），之后完全不需要 PSN。
5. 主机 30 秒收不到心跳回复会结束会话（代码注释），可靠包重传约 5 秒后放弃。链路切换或故障转移要控制在这个窗口内，越快越好。
6. 音视频包没有重传，突发丢包靠 IDR 恢复，所以隧道层的 FEC/ARQ 是提升公网稳定性的关键。
7. 渲染管线要提供「到达即呈现」的低延迟模式（#807 的教训）。

---

## 9. 主要来源

- chiaki-ng 仓库：https://github.com/chiaki-ng/chiaki-ng ；发布说明 https://github.com/chiaki-ng/chiaki-ng/releases/tag/v1.10.1 、https://github.com/chiaki-ng/chiaki-ng/releases/tag/v1.10.0
- 文档：https://chiaki-ng.github.io/chiaki-ng/ （源码 `docs/setup/remoteconnection.md`、`configuration.md`、`docs/updates/done.md`）
- 原版：https://git.sr.ht/~thestr4ng3r/chiaki ；https://github.com/thestr4ng3r/chiaki
- chiaki-ng-core：https://github.com/chiaki-ng/chiaki-ng-core ；chiaki-ng-swift：https://github.com/chiaki-ng/chiaki-ng-swift ；akira：https://github.com/chiaki-ng/akira ；CLA：https://gist.github.com/streetpea/d97e8adea343ab183cb31ab78139fcd8 ；审计仓库：https://github.com/chiaki-ng/asobi-agpl-audit
- 关键 PR：#741（音视频重排序）、#697（FEC 失败请求 IDR）、#868（1.10.1 发版）；关键提交：`3e1a24dd`、`8134f458`、`727917c0`
- 关键 issue：#39、#46、#103、#130、#230、#252、#286、#319、#342、#404、#411、#516、#594、#662、#681、#709、#725、#726、#728、#731、#743、#746、#759、#775、#785、#789、#790、#807、#810、#851、#853、#867、#878、#884（统一为 https://github.com/chiaki-ng/chiaki-ng/issues/编号）
- Steam Deck Wi-Fi：https://github.com/ValveSoftware/SteamOS/issues/1256
- 移植：https://github.com/Geocld/PeaSyo 、https://github.com/Geocld/PeaSyo-rs 、https://github.com/ForWard-Technologies-LLC/Pylux 、https://github.com/mauricio-gg/vitaki-vitarps5 、https://github.com/ywnico/vitaki-fork 、https://github.com/Ibbolufc/XPort 、https://github.com/yoni101087/psxbox 、https://github.com/CSystem77/Chiaki-NG-Web 、https://github.com/renjfk/hotaru-webos 、https://github.com/yazanj082/momiji 、https://github.com/Teuns/Chiaki-4-iOS 、https://github.com/streamingdv/PSPlay-Application-Hosting
- Sony 分辨率说明：https://www.sony.ca/en/electronics/support/articles/SX365201
- GPL 与 App Store：https://www.computerworld.com/article/1347653/gplv2-blocks-vlc-from-apple-s-app-store.html 、https://www.engadget.com/2011-01-09-the-gpl-the-app-store-and-you.html
- 原始材料（issue 原文、提交列表、源码浅克隆）只在调研时的本地临时目录里，没有随仓库发布，需要时按上文重新获取。

本次调研没有起子代理，只用了 gh API、git 浅克隆和 WebSearch（4 次）。


---

## 附录：本方向的关键论断与待确认问题（复核结果见 V1/V2/V3）

- [A1] (high,已核实) chiaki-ng 是当前唯一活跃的主线：仓库已迁到 github.com/chiaki-ng/chiaki-ng（streetpea/chiaki-ng 和 streetpea/chiaki4deck 都重定向过去），最新正式版 v1.10.1 发布于 2026-11-03，约 3120 star，许可证为 AGPL-3.0-only 加 OpenSSL 例外
  src: gh api repos/chiaki-ng/chiaki-ng 与 releases；chiaki-ng/COPYING；lib/src/session.c:1 的 SPDX
- [A2] (medium,已核实) 原版 Chiaki 自述进入仅维护模式，最后一个 tag 是 v2.2.0（2023-08-20），sourcehut 上最后一次提交在 2026-01-05，GitHub 镜像在 2021 年归档；原版没有麦克风和 PSN 远程功能
  src: chiaki-orig/README.md；git log；lib/src 目录中没有 audiosender.c 和 remote/
- [A3] (high,已核实) chiaki-ng-core（C++23 重写，VERSION 2.0.0）和 chiaki-ng-swift 都在 2026-11-02 创建，许可证为 AGPL-3.0-or-later 加 OpenSSL 例外加 App Store/TestFlight 分发附加许可，贡献需签 FLA 2.1 CLA；原 chiaki-ng 仓库没有 App Store 例外；core 的 Windows 和 Android 版尚未编译测试
  src: ng-core/LICENSE:662-704；ng-core/README.md；chiaki-ng/COPYING 末尾；CLA gist d97e8adea343ab183cb31ab78139fcd8
- [A4] (high,已核实) chiaki-ng 维护者在积极执行 AGPL：就 Asobi 向 Apple 提了 DMCA、建了 asobi-agpl-audit 审计仓库，并针对 Portal: Remote Play 和 Console Remote 立了 issue；core 的 README 明确把「用 AI 翻译成其他语言」视为衍生作品
  src: issues #725、#790、#884；ng-core/README.md；github.com/chiaki-ng/asobi-agpl-audit
- [A5] (medium,已核实) PS5 远程游玩最高 1080p；chiaki-ng 的分辨率预设为 360p/540p/720p/1080p，帧率 30/60，码率滑块 2–100 Mbps，编码支持 H.264、H.265、H.265 HDR；码率只是向主机申请的值
  src: lib/include/chiaki/session.h:59-68；lib/src/session.c:92-136；gui/src/qml/SettingsDialog.qml:1348-1349；common.h:98-100；Sony 支持文章 SX365201；akira README
- [A6] (high,已核实) 码率和分辨率控制在 PS5 端：客户端每 200ms 只上报收/丢包数，chiaki-ng 默认把上报的丢包率截到 5%，以减少主机降画质；launchspec 向主机申请 adaptiveStreamMode=resize；主机下发的 CONNECTIONQUALITY 消息客户端只记日志、不做调节
  src: lib/src/congestioncontrol.c:5,21-41；gui/src/settings.cpp:740-747；lib/src/launchspec.c:77；lib/src/streamconnection.c:693-707；issue #230
- [A7] (high,已核实) PS5 视频 FEC 冗余很小（日志样本为每帧 18+1、30+1 个单元），chiaki-ng 中没有音视频包的 NACK 重传；恢复链路是 FEC（jerasure Cauchy RS）→ 坏帧报告 →（可选，默认关闭的）IDR 请求 → 参考帧替换，最后一步常引发 HEVC 花屏
  src: issue #785 日志；PR #697 日志；lib/src/frameprocessor.c:208；lib/src/fec.c:14；videoreceiver.c:229-322；gui/include/settings.h:302；grep 未发现 nack
- [A8] (high,已核实) 若干基础性传输缺陷直到 2026 年才修复：可靠包发送缓冲从原版继承的 16 个改为 1024 个（v1.10.1，提交 3e1a24dd）；等待 IDR 时永久冻结（8134f458）；重排序队列释放后使用（727917c0，2026-10-31）；音视频重排序在 2026-04 的 v1.10.0 才加入，超时固定 16ms
  src: chiaki-orig/lib/src/takion.c:38；chiaki-ng/lib/include/chiaki/takionsendbuffer.h:21；lib/src/takion.c:45-47；PR #741；gh api commits
- [A9] (high,已核实) 大量「卡顿」报告的根因是客户端渲染或帧节奏回归，不是网络：#743（1.10.0 的 60Hz 计时舍入，1.10.1 已修）、#807（显示层重构带来约 +50ms 延迟，仍 open）
  src: issues #743、#807 及维护者评论
- [A10] (medium,已核实) Steam Deck 上周期性冻结和「连接差」卡顿，很大一部分来自 SteamOS 的 Wi-Fi 问题（ValveSoftware/SteamOS#1256，2023-11 至今 open）以及 Wi-Fi 省电和后台扫描
  src: issues #230、#775 中维护者的回复；gh api ValveSoftware/SteamOS/issues/1256
- [A11] (high,已核实) 直连端口在源码中写死：TCP/UDP 9295（注册、会话、ctrl）、UDP 9296（Takion 流）、UDP 9297（Senkusha）、UDP 9302（PS5 发现和唤醒；PS4 为 987），不支持自定义端口；PS5 不支持 IPv6 远程游玩
  src: session.c:29；ctrl.c:32；streamconnection.c:31；senkusha.c:29；discovery.h:24-28；docs/setup/remoteconnection.md:5,99-113；issue #319
- [A12] (high,已核实) 经 WireGuard、Tailscale 等路由隧道跨子网连接可行：先在局域网完成注册，再手动添加内网 IP 即可连上；广播发现不跨子网；akira（Switch）已内置 WireGuard 并支持跨子网单播发现
  src: issue #286；chiaki-ng/akira README
- [A13] (medium,已核实) Senkusha 探测 MTU 的范围是 576–1454，探测失败回退到 1454；MTU 不匹配曾导致公网连接报「Unknown Error in Stream Connection」，走隧道时必须处理 MTU
  src: lib/src/senkusha.c:265,272；lib/src/session.c:641-645；issue #252
- [A14] (medium,已核实) 桌面端没有会话中途自动重连（resume 只在客户端设备休眠唤醒后触发）；主机端 RP 服务进入卡死状态（already in use 或 crashed）后，只能让主机进入休眠来复位
  src: gui/src/qmlbackend.cpp:572-579（grep 未发现其他重连逻辑）；issues #38、#662
- [A15] (medium,已核实) 主机 30 秒收不到心跳回复就会结束会话（代码注释）；Takion 可靠包每 200ms 重传一次、最多 25 次（约 5 秒后放弃），这限定了链路故障转移的时间窗口
  src: lib/src/ctrl.c:36-45；lib/src/takionsendbuffer.c:12-14
- [A16] (medium,推测) PS5 固件更新多次破坏 PSN 远程路径（2026-03 的 13.00 固件见 #728/#731，2026-09 的 customData1 长度变化见 #810），社区在一天到数周内修复；检索到的样本中，局域网和手动直连路径受固件影响较少
  src: issues #728、#731、#810
- [A17] (medium,已核实) chiaki-ng 维护高度依赖单人（streetpea 1313 次提交，原作者 601 次，其他人都在 135 次以下），发版节奏突发：1.10.0 到 1.10.1 相隔 7 个月，积压 107 个以上提交
  src: gh api contributors；按月统计提交数；issue #851
- [A18] (low,推测) PeaSyo v2 自称已用 Rust 全量重写、不再依赖 Chiaki 协议实现，并已上架 App Store，但 PeaSyo-rs 仓库只有 README、没有源码，无法验证；可作为「从零实现」路线可行的旁证
  src: Geocld/PeaSyo README；gh api repos/Geocld/PeaSyo-rs/contents

## open_questions
- 本软件是只给自己用，还是要分发、收费或上 App Store？这决定了能否直接复用 chiaki-ng 的 AGPL 代码（自用几乎没有义务；分发必须 AGPL 开源；闭源只能走净室自研）。
- chiaki-ng-core 改为 AGPL-or-later 加 App Store 例外时，原作者 Florian Märkl（601 次提交）以及其他历史贡献者是否全部同意？授权链的完整性和法律效力没有找到证据。
- PS5 端协议是否支持音视频包重传（NACK）、可调 FEC 冗余比例或固定码率？chiaki-ng 中没有这些实现，主机端能力未知，需要逆向或实测。
- AV 重排序 16ms 的固定超时，在公网或隧道抖动较大（大于 16ms）时会不会把迟到的包当成丢包、反而加剧花屏？需要用网络损伤工具（netem）实测，并评估改成自适应抖动缓冲的收益。
- v1.10.1 默认开启的 direct frame mapping 是否完全修复了 #807 的约 50ms 显示延迟回归？issue 仍 open，需要实测。
- 会话中客户端的源 IP 或端口变化时（例如中转切换、多路径、Wi-Fi 切到 5G），PS5 会怎么处理？会话能否保持，还是必须重新建立？这直接影响中转故障转移的设计，需要实测。
- PS5 是否对 Senkusha 测得的 RTT 或 MTU 设有拒绝阈值（例如高 RTT 中转线路会不会被拒绝或被强制降档）？需要在自建 VPS 中转链路上实测。
- 用户目标平台有哪些（Windows、macOS、iOS、Android、Linux 或掌机）？这会影响选 C 版 libchiaki 还是刚起步的 C++23 chiaki-ng-core（Windows、Android 未编译测试）。
- chiaki-ng 1.10.1 的发布说明提到「fix connecting by ports」，具体含义未查清（可能和 PSN 端口猜测有关），是否意味着将来会支持自定义端口待确认。
- Sony 服务条款或反规避条款对自研 Remote Play 客户端的法律风险，本次没有调研。
- PeaSyo v2（Rust 重写）的「更稳定」说法缺少公开源码和测试数据，无法验证；它的实现细节和许可证状态待查。
