# chiaki-ng v1.10.1 三类报错的根因与影响（M0 冒烟实验）

## 结论速览

| 报错 | 根因 | 对网关终结 Takion 稳定性的实际影响 | 处理 |
|---|---|---|---|
| `Takion received data ack with size 10 != 0xc` | 日志格式是 `%zx`，**「10」是十六进制，即 0x10 = 16 字节**。PS5 发的是标准 SCTP 式变长 SACK（12 字节固定部分 + 1 个 gap block 或 1 个重复 TSN）。libchiaki 只认 12 字节，整条丢掉，连里面的累计确认也一起丢了 | **不会断会话**（M0 实测会话没断；上游维护者在 #342 里也没把它当成断流原因）。只有轻度副作用：白白重发最长约 5 s、Senkusha 等 ack 可能超时、产生「放弃重传」的误报、ERROR 日志刷屏 | 建议打一个小补丁（0004），**不紧急**。PS5 改有线后会少很多 |
| `Takion dropping data with seq num` | PS5 重发了我们已经交付的可靠数据（我们的 ack 在 2%~5% 丢包的 Wi-Fi 上行丢了），libchiaki 收到重复包时**不再回 ack**（违反 RFC 4960 §6.2），PS5 就一直重发 | 无（数据早已交付）；只多一点重发 | 同一个补丁里顺手修 |
| `Received Session Id is too short` | PS5 有时在 0x33 消息里发长度前缀 0x10 加字面串 **"InvalidSessionId"**（正常时首字节 0x4a = 74，后跟 74 个字符的 ID）。至少 2023-12 起就有，**与 14.23 固件无关** | 无。备用 ID 只填进 BIG 的 session_key，libchiaki 不校验 BANG 回显 | 不打补丁，只在网关里记成事件 |
| ctrl 未知消息 `00 00 00 00 02 01 00 00` | ctrl 类型 **0x41**，每个会话在 BANG 之后都来一条 8 字节，2023 年起的上游日志里都有 | 无（解密计数器照常递增，core 也忽略它） | 只改日志，打印类型号 |
| ctrl 未知消息 `ff ff ff dc 00 00 00 64` | 类型号未知（v1.10.1 把打印类型的那行注释掉了），上游日志里没找到同款 | 无（理由同上） | 同上；含义待抓类型号后再判断 |

**是否改用 chiaki-ng-core 或升级到 v20：现在不要。** 两种 Takion 版本的 DATA_ACK 格式完全一样，会话 ID 也不走 Takion，升级版本解决不了这几条报错。core 才建了 17 天，用 C++23，没有任何发布，Windows 上没编译过，接口也完全不同。我们的补丁 0001~0003 都得重新移植。建议做 PLAN §5.1 补丁第 7 项（版本协商）时再单独评估。

---

## 1. 现象 1：`data ack with size 10 != 0xc`

### 1.1 「10」就是 0x10
- `lib/src/takion.c:1467`：`CHIAKI_LOGE(..., "Takion received data ack with size %zx != %#x", buf_size, 0xc);`，`%zx` 输出十六进制且不带 0x，所以「10」就是 16 字节。
- 旁证：#342 用的是旧版代码，格式是 `%#x`，同一条报错打印为 `size 0x10 != 0xc`（https://github.com/chiaki-ng/chiaki-ng/issues/342 ，日志里共 23 次）。#425 里引用的旧代码也是 `%#x`。
- `msg.payload_size` 已经减掉了 4 字节的 chunk 头（`takion.c:1539-1545`），所以 16 是 DATA_ACK 载荷本身的长度。

### 1.2 DATA_ACK 的结构（类 SCTP SACK，RFC 4960 §3.3.4）
| 偏移 | 长度 | 字段 |
|---|---|---|
| 0 | 4 | cumulative TSN（累计确认到的序号） |
| 4 | 4 | a_rwnd（通告的接收窗口） |
| 8 | 2 | gap block 个数 G |
| 10 | 2 | 重复 TSN 个数 D |
| 12 | 4×G | gap block：start u16、end u16，都是相对 cumulative 的偏移 |
| 12+4G | 4×D | 重复收到的 TSN（u32） |

**0xc 就是 G = 0、D = 0 时的固定部分。** 16 字节只可能是 **G = 1、D = 0**（我们发的某个包丢了，后面的包先到了）或 **G = 0、D = 1**（我们重发了 PS5 已经收到的包）。
- libchiaki 自己发 ack 时，这两个计数永远写 0（`takion.c:712-731`）。
- chiaki-ng-core 的解码与上表完全一致（`src/core/net/protocol/takion/control_chunk_codec.hpp:48-51`、`control_chunk_codec.cpp:286-355`；`reliable_channel.hpp:67` 中 `gap_block_size = 4`），而且**不区分 Takion 版本**：v20 新增的 8 字节扩展头不用于控制包（`profile.hpp:174`：「Every family but control…」）。

### 1.3 libchiaki 的三处缺陷
1. **只收 12 字节**：`takion.c:1465-1469` 遇到 `buf_size != 0xc` 直接 return，整条 ack 被丢掉，**连 cumulative 字段也没用上**。
2. **死代码，而且规则不对**：`takion.c:1476` 原本想校验 `gap*4 + 0xc`，说明原作者预料到会有 gap block，但第 1465 行已经把所有非 12 字节的 ack 挡掉了；这条规则本身也漏掉了重复 TSN 的部分。上游主干与 v1.10.1 在这里完全一样（已用 `gh api` 取主干 takion.c 对比）。
3. **收到重复数据不回 ack**：`takion_flush_data_queue` 只有拉出了新数据才发 ack（`takion.c:1385-1434`，第 1432 行 `if(ack)`）。重复包在 `reorderqueue.c:71-79` 被丢弃，并打印 `takion.c:955-962` 的「dropping data」。

### 1.4 丢掉这些 ack 的后果（按对网关的重要性排序）
1. **白白重发，最长约 5~7.5 s**。发送缓冲每 100 ms 检查一次，超过 200 ms 未确认就重发，最多 25 次后放弃（`takionsendbuffer.c:12-14,234-266`）。如果 16 字节 ack 里带的是已推进的 cumulative（重复 TSN 那种情况），我们就一直重发 PS5 早已收到的包。每次重发 PS5 又回一条带重复 TSN 的 16 字节 ack，又被丢掉，形成自持的循环，直到下一条新的可靠消息（Takion 心跳 1 s 一次）换来一条干净的 12 字节 ack。消息都很小，带宽可以忽略。
2. **「放弃重传」误报**。#342 的日志正是这个过程：`0x2359fd7e` 重发 10 次（旧版上限是 10），每次重发后 50~100 ms 收到一条 0x10 的 ack，最后「Hit max retries … giving up」。PLAN §5.1 补丁第 5 项要做「可靠消息被放弃时上报事件」，**不修这个问题，这个事件就会对 PS5 其实已经收到的消息误报**。PLAN §2.5 第 9 条「IDR 请求可能悄悄丢失」也要据此修正：一部分「放弃」是假的。
3. **Senkusha 的 echo 等 ack 会超时**。Senkusha 等的是具体序号的 `CHIAKI_TAKION_EVENT_TYPE_DATA_ACK`（`senkusha.c:708-719,935-960`，超时 5 s）。PS5 如果用带 gap 或重复 TSN 的 SACK 回应，事件就不会触发，RTT/MTU 测试会失败，退回 1454 字节且不设 DF。只在测速阶段恰好丢包时才会发生，局域网里概率低。
4. **IDR 请求、CORRUPTFRAME 延迟：基本不受这个问题影响**。消息本身丢了时，修复时间取决于固定的 200~300 ms 重传定时器，和 ack 能不能解析无关。ack 丢掉只会让「已送达」的消息多重发几次。IDR 的主要延迟在 PS5 自己（E2 实测约 1 s 才出关键帧）。
5. **发送缓冲不会被占满**。v1.10.1 已扩到 1024 个（`takionsendbuffer.h:21`，上游提交 3e1a24ddb2，2026-10-31，说明里写的就是「丢包时的突发只保留 16 个」这个旧问题）。可靠消息速率很低，每个最多活 5 s，远不到 1024。
6. **日志**：每个 SACK 一条 ERROR。`tools/m0-remote.sh:36` 已经在过滤这一行，容易连真问题一起挡掉。

### 1.5 为什么 M0 在 13.6 s 一次刷出 64 条（推测，原始日志未随仓库发布，没有读取）
时间上吻合：冒烟会话约第 10 s 帧率掉到 18.8，30 s 内上报坏帧 81 次，每次 CORRUPTFRAME 都是一条可靠消息；PS5 那一段双向都有 2%~5% 丢包。一串 CORRUPTFRAME 里只要丢一个，后面每个到达的包都会换来一条 G=1 的 16 字节 SACK。200 ms 后整串重发，又会换来一批 D=1 的 16 字节 SACK。「dropping data」9 次则是 PS5 那边的重发：我们的 ack 丢了，而 libchiaki 收到重复包不回 ack。**这些和 14.23 固件无关**：#342 在 2024-05 的日志里就是同样的形态。

### 1.6 PS5 会不会永久卡在缺口上（推测）
#342 的日志里，放弃 `fd7e` 之后约 0.3 s，`fd7f`、`fd80` 就被确认了，此后重发计数回到 0/1。说明 PS5 的累计确认点越过了这个缺口：要么最后一次重发其实到了，要么 PS5 等一段时间后跳过缺口。core 的接收端有同样的「等约 2.5 s 后跳过缺口」策略（`reliable_channel.hpp:53`，`missing_sequence_expiry_short_us`），多半是照官方客户端的行为做的。所以不会出现永久停滞，但「消息约 2.5 s 没送达就被 PS5 跳过」这一点要记下来，网关层的 IDR 重试不能只依赖 Takion 的可靠性。

---

## 2. 现象 2：`Received Session Id is too short`
- 解析逻辑在 `ctrl.c:926-985`：首字节是长度（注释写的是「skip the size」，`ctrl.c:944` 期待 0x4a），去掉首字节后少于 24 个字符就报 too short（`ctrl.c:961-965`），然后用「单调时钟秒数 + base64(48 个随机字节)」生成备用 ID（`ctrl.c:807-845`）。
- **PS5 实际发的内容**（上游多个 issue 的日志 hexdump）：`10 49 6e 76 61 6c 69 64 53 65 73 73 69 6f 6e 49 64`，即长度 0x10 加 "InvalidSessionId"。
  - 2025-08、2024-06 的例子：#526、#553、#411、#357、#348。
  - 更早的版本会把 "InvalidSessionId" 原样当作 ID 使用，会话照常进行：#125（2023-12-20）、#243（2024-02-14）。
  - 2024-05-11 的提交 8937d6fb35「Check if received session id is too short」和 440b9e9ad0「Set fallback session id…」才改成过短时用备用 ID。
  - 正常时首字节 0x4a = 74，例如 #342 里的 `1716605755ML65VO…`，正好 74 个字符。
- 影响：ID 只填进 BIG 的 `session_key`（`streamconnection.c:1073`）。libchiaki 处理 BANG 时只检查 version_accepted 和 encrypted_key_accepted，不比对 session key（`streamconnection.c:746-850`）。core 也明确说明，用合成 ID 时接受 session key 不匹配的 BANG（`remote_play/session_id.hpp:63`）。core 的解析和备用 ID 规则与 libchiaki 一样（`remote_play/limits.hpp:47-53`）。
- 会话 ID 走 ctrl（TCP 9295，rpcrypt 加密），在 Takion 之前就到了，**与 Takion v12 还是 v20 无关**。PS5 什么时候发 "InvalidSessionId" 不清楚（推测与主机刚唤醒、PSN 状态有关）。可以在 E6 唤醒实验里顺带记录「刚唤醒后的第一个会话」是否更容易出现。
- 处理：不改代码。网关把「备用 ID」记成一个会话事件和计数即可。

## 3. 现象 3：ctrl 未知消息
- libchiaki 在分发之前就用 `crypt_counter_remote++` 解密了（`ctrl.c:849`），所以未知消息不会让后续 ctrl 消息解错。默认分支只打印 hexdump，打印类型号的那行被注释掉了（`ctrl.c:894-897`）。core 对未知类型同样忽略，返回 `CtrlIgnoredMessage`（`remote_play/ctrl_codec.cpp:147,153`）。
- `00 00 00 00 02 01 00 00`：**类型 0x41**。证据：#125 的旧版日志直接打印了 `Received Ctrl Message with unknown type 0x41`；#357、#526 打印的原始帧头是 `00 00 00 08 00 41 00 00`（长度 8，类型 0x0041）。它总在 BANG 之后、音频头之前出现，2023-01 到 2025-08 的上游日志里都有（#38、#39、#125、#342、#348、#357、#526、#553）。无害，含义未知。
- `ff ff ff dc 00 00 00 64`：上游日志里没找到。按两个大端 int32 读是 -36 和 100，推测可能是 Wi-Fi 信号（-36 dBm / 100%）之类的状态通知：上游用户多为有线，我们测试时 PS5 是无线接入。**置信度低**。补丁里改为打印类型号，下次实验就能确认。

## 4. chiaki-ng-core 的 Takion 实现（克隆的提交 6f109c38b5，2026-11-16，VERSION 2.0.0）
- **完整的选择性确认**：
  - 收到 ack 时同时处理 gap block 和重复 TSN（`control_chunk_codec.cpp:316-352`）；
  - 用 gap 确认缺口之后的包，并对缺口内的包立即重发一次，相当于快速重传（`reliable_channel.cpp:360-445`，第 428 行）；
  - 收到重复数据会再 ack（`reliable_channel.cpp:209-215`）；
  - 自己发的 ack 带 gap block，最多 136 个（`reliable_channel.cpp:335-357`）；
  - RTO 按实测 RTT 计算（默认 300 ms，见 `limits.hpp:38-46`），缺口超过 2.5 s 就跳过。
- **版本**：PS5 默认用 v20，协商列表是 {12, 15, 18, 20}（`versions.hpp:35,51`）。v20 和 v12 的差别在扩展包头、可靠消息前缀、AV 包头布局和 ECDH 曲线（v12 用 secp256k1，v13 起用 secp521r1，见 `profile.hpp:170-224`），**不在 DATA_ACK 和会话 ID 上**。
- **作为替代方案的成熟度**：
  - 2026-11-02 才建库，3 星，没有发布；
  - 用 C++23 和 boost::sml；
  - README 写明 macOS 测试充分、Linux 能编译，Windows 没编译过；
  - 接口是 DDD 风格的前端 API，和 libchiaki 完全不同。
  - README 特别警告：照着它翻译成其他语言也算衍生作品。网关按 D1 是自用或 AGPL，许可证上没问题，但如果把它的逻辑移植进 Rust 代码，那部分也必须用 AGPL。
- 结论：**只借鉴它的处理方式**（本补丁就是按它的行为写的），现在不迁移。

## 5. 上游 issue 和维护者的说法
- #342（https://github.com/chiaki-ng/chiaki-ng/issues/342 ，经 PSN 远程连接时几分钟后断开）：日志里有 23 条 `size 0x10` 和大量 `dropping data`，前面还有一串 `Takion failed to send raw: Resource temporarily unavailable`。维护者 streetpea 说：「I don't see anything that would be causing the stream connection to close on the ps side」。最后定位到网络和 NAT、STUN 问题，**没有讨论 data ack**。
- #425（https://github.com/chiaki-ng/chiaki-ng/issues/425 ，关闭视频）：第三方开发者 Entropy512（不是维护者）引用了这段代码，猜测 libchiaki 缺少一些流控和重传机制，并提到「主机端 remoteplay 服务在某条流的流控或重传积压后崩溃」，把手柄输入合并发送后少了很多。**维护者没有回应这一点，也没有证据**。
- 在仓库里搜「data ack with size」只命中这两个 issue。搜「Session Id is too short」命中 8 个 issue（#709 #526 #635 #553 #434 #411 #357 #348），都只是出现在日志里，没有人把它当成问题讨论。

## 6. 对「网关终结 Takion」稳定性的判断
- **不会断会话，也不影响媒体**：音视频不走可靠通道；ctrl 心跳走 TCP（30 s 超时），与此无关；Takion 心跳每 1 s 一次，仍然送得到。
- **真正的诱因是 PS5 那段 Wi-Fi 的丢包**。PS5 改有线后，SACK 带 gap 或重复 TSN 的情况会大幅减少（待改有线后实测）。
- **仍值得修**：
  - 它会污染我们计划依赖的「可靠消息放弃」事件（补丁第 5 项）、Senkusha 测速和日志告警；
  - 修复很小，只在控制面的可靠消息路径上，不碰 AV 热路径。

## 7. 建议
1. **新增补丁 `0004-yw-takion-sack.patch`**（全文见 patch_suggestion），改 4 个文件：
   - `takion.c` 的 `takion_handle_packet_message_data_ack`：接受 `12 + 4G + 4D` 的变长 ack；先处理 cumulative，再按 gap 区间从发送缓冲移除，对缺口里前 8 个包做一次快速重传（距上次发送不足 20 ms 就不发）；
   - `takion.c` 的 `takion_handle_packet_message_data`：已交付过的序号重复到达时，立即回一次 `cumulative = data_queue.begin - 1` 的 ack；
   - `takionsendbuffer.[ch]`：新增 `chiaki_takion_send_buffer_ack_range`、`chiaki_takion_send_buffer_resend_now`；
   - `ctrl.c` 的默认分支：用 INFO 级打印类型号和长度。
   - **分级**：前两项必做（风险极低）；快速重传可选，如果不想改行为可以删掉那一段。
2. **不要**单独把 `streamconnection.c:168` 的版本号从 12 改成 20：libchiaki 没有实现 v20 的报文格式，改了就连不上。版本协商留到 M1/M2 再评估 core。
3. 配套：
   - 补丁合入后去掉 `tools/m0-remote.sh:36` 里对「data ack with size」的过滤；
   - 网关统计四个计数器：带 gap 的 SACK、带重复 TSN 的 SACK、快速重传次数、对重复数据的再 ack 次数，用来区分 16 字节 ack 到底是 G=1 还是 D=1；
   - 补丁第 5 项（放弃事件）要在 0004 之后再做；
   - 网关层的 IDR 重试不能依赖 Takion 的可靠性，按「发出后 N ms 没看到关键帧就再发，并限频」来做。
4. 验证计划（**未实测**，等 PS5 上线）：
   - 用同样的 30 s、15 Mbps 冒烟流程在 Wi-Fi 下跑一次，确认 ERROR 消失、计数器有读数、会话和首帧时间与基线一致；
   - PS5 改有线后再跑一次对比；
   - 再连续跑 10 次短会话，看会话 ID 和 0x41 的情况。

## 附：本次做了什么、各文件在哪
- 只读：没有修改项目目录下的任何文件，没有连接网关机或 PS5，原始 live.log 没有读取。
- 在本地临时目录里（不随仓库发布）：浅克隆 chiaki-ng-core、保存相关 issue 原文、生成补丁（已整理为仓库里的 `third_party/patches/0004-yw-takion-sack.patch`）。
- 补丁检查：
  - 对 v1.10.1 当前源码 `patch --dry-run` 通过；
  - 三个文件用 clang `-Wall -Wextra` 做语法检查，没有新告警；
  - 一个临时的小测试程序（未随仓库发布）链接现有的 libchiaki.a，验证了区间确认（含 32 位回绕）、顺序保持和 SACK 长度规则，全部通过；
  - **真机未测**。
- 没有启动子代理。


---

## 附：建议补丁（已于 2026-11-19 采纳为 `third_party/patches/0004-yw-takion-sack.patch`）

建议作为 third_party/patches/0004-yw-takion-sack.patch（对 chiaki-ng v1.10.1，已在副本上 patch --dry-run 通过、clang -Wall -Wextra 语法检查无新告警、send buffer 区间确认小测试通过；真机未测）。前两项（变长 ack 和重复数据再 ack）必做；「快速重传」那一段可选，不想改重传行为就删掉；ctrl.c 那段只改日志。

补丁全文见 `third_party/patches/0004-yw-takion-sack.patch`。
