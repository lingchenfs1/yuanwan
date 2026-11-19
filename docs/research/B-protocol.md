# PS5 Remote Play 协议源码级调研报告（方向 B）

## 0. 调研范围与版本

| 项目 | 版本 / 时间 | 说明 |
|---|---|---|
| chiaki-ng | 提交 `43728af`（2026-11-03），最新 release `v1.10.1`（2026-11-03 发布） | 仓库已从 `streetpea/chiaki-ng` 重定向到 https://github.com/chiaki-ng/chiaki-ng，仍在活跃维护（最近 push 2026-11-14） |
| pyremoteplay | 提交 `a127422`（0.7.6，2022-08-22），仓库**已归档** | https://github.com/ktnrg45/pyremoteplay，只用来交叉核对 |
| 源码获取方式 | 把上面两个仓库浅克隆到本地临时目录（不随仓库发布），检出上表的提交 | 只读，没有运行任何构建脚本 |

约定：下文「`文件:行号`」若不特别注明，都指 chiaki-ng@43728af。**已核实** = 源码或官方 / 维护者原话能直接证明；**推测** = 根据源码推断或只有社区说法、还需实测。

> 许可证提醒：chiaki-ng 源文件头是 `SPDX-License-Identifier: LicenseRef-AGPL-3.0-only-OpenSSL`（如 `lib/src/fec.c:1`）。如果直接复用代码，网关和客户端都要按 AGPL-3.0 履行义务（包括以网络服务形式提供时公开源码）。这一点需要用户决定（见 open_questions）。

---

## 1. 端口与传输

| 端口 | 协议 | PS4 | PS5 | 用途 | 出处 |
|---|---|---|---|---|---|
| 987 | UDP | ✔ | – | 发现 / 唤醒（DDP），协议版本 `00020020` | `lib/include/chiaki/discovery.h:24-25` |
| 9302 | UDP | – | ✔ | 发现 / 唤醒（DDP），协议版本 `00030010` | `discovery.h:26-27` |
| 9303–9319 | UDP（客户端本地） | | ✔ | 客户端发现套接字依次尝试绑定 9303..9319，都失败才用随机端口；pyremoteplay 注释写「Necessary for PS5」 | `discovery.h:28-29`、`lib/src/discovery.c:155-200`；pyremoteplay `const.py:27` |
| 9295 | UDP | ✔ | ✔ | 注册前的搜索包：PS5 发 `SRC3` 收 `RES3`，PS4 发 `SRC2` 收 `RES2` | `lib/src/regist.c:26,465-474,556` |
| 9295 | TCP | ✔ | ✔ | ①注册 POST `/sie/ps5/rp/sess/rgst`；②会话请求 GET `/sie/ps5/rp/sess/init`（Connection: close）；③ctrl 长连接 GET `/sie/ps5/rp/sess/ctrl`（keep-alive，整个会话期间保持） | `regist.c:103-112`、`session.c:29,887-902`、`ctrl.c:32,1325-1349` |
| 9296 | UDP | ✔ | ✔ | Takion 主流：音视频、输入、控制数据 | `lib/src/streamconnection.c:31,161` |
| 9297 | UDP | ✔ | ✔ | Senkusha 测速会话（独立的 Takion v7，不加密） | `lib/src/senkusha.c:29,148-158` |

- **已核实**：PS4 与 PS5 的区别只在发现端口（987 / 9302）、发现协议版本、搜索包（SRC2 / SRC3）、HTTP 路径（`/sie/ps4/...`、`/sie/ps5/...`）和 Takion 版本（PS4 为 9，PS5 为 12，`streamconnection.c:168`）。9295/9296/9297 两者相同。
- **已核实**：chiaki-ng 官方文档给出的 PS5 端口转发表是 9295 UDP/TCP、9296 UDP、9297 UDP、9302 UDP（`docs/setup/remoteconnection.md:97-113`）。
- Sony 没有公开的官方端口表。网上流传的 8572 / 9303 / 9308 等说法来源是二手的，**未核实**（[Reddit 汇总](https://redlib.belloworld.it/r/PS5/comments/kx8mhy/remote_play_from_internet_firewall_ports)、[第三方指南](https://support.inmoxr.com/air3/guides/ps5-remote-ultimate-wan/)）。

## 2. 发现与唤醒（DDP）

格式是类 HTTP 的纯文本 UDP 包，**行尾是 `\n` 而不是 `\r\n`**（`discovery.c:74-85`）：

```
SRCH * HTTP/1.1
device-discovery-protocol-version:00030010

WAKEUP * HTTP/1.1
client-type:vr
auth-type:R
model:w
app-type:r
user-credential:<十进制 uint64>
device-discovery-protocol-version:00030010
```

- 响应状态码：`200` 表示开机（ready），`620` 表示待机（standby）。响应头包括 `host-id`、`host-name`、`host-type`、`system-version`、`host-request-port`、`running-app-titleid`、`running-app-name`（`discovery.c:102-135`）。PS5 判定方式：协议版本为 `00030010`，且 system-version ≥ 8050001（`discovery.c:35-53`）。
- **唤醒凭据**：只需要 regist key，不需要账号 ID 或 morning。regist key 的字节本身是 ASCII 十六进制字符，按 16 进制解析成 uint64，再以十进制写进 `user-credential`（`gui/src/discoverymanager.cpp:274-292`，第 287 行 `toULongLong(&ok, 16)`；pyremoteplay `util.py:133-135` 做两次 hex 解码，结果一致）。**已核实**
- 唤醒包没有任何加密或 MAC，谁拿到 regist key 谁就能唤醒（据源码推断）。chiaki 的唤醒是**单播**发到目标 IP 的 9302 端口（`discovery.c:440-499`）；SRCH 可以广播也可以单播。
- PS5 端要先打开 `设置 → 系统 → 节能 → 待机模式中可用的功能`，勾选「保持连接到互联网」和「允许通过网络开启 PS5」（`docs/setup/configuration.md:31-34`）。

## 3. 注册（配对）

### 3.1 流程（本地或直连模式，`regist.c:231-449`）
1. UDP 9295 发送 `SRC3\0`（广播或单播），在 3 秒内等到 `RES3`（`regist.c:28-30,465-528`）。如果收到 ECONNREFUSED，说明 PS5 上没有打开「连接设备」界面（`regist.c:510`）。
2. 等待 100 ms（注释写「PS4 doesn't accept requests immediately」），然后用 TCP 连接 9295（超时 3 秒）。
3. 发送请求：`POST /sie/ps5/rp/sess/rgst HTTP/1.1`、`HOST: 10.0.2.15`（固定占位地址，维护者在 #867 确认这是正常行为）、`User-Agent: remoteplay Windows`、`Connection: close`、`Content-Length`、`RP-Version: 1.0`（`regist.c:103-166`）。
4. Body：先用 0x1e0 字节的 `'A'` 填充；在 0xc7 和 0x191 处嵌入由随机 ambassador 派生的 `aeropause`；之后是加密的内层头 `Client-Type: dabfa2ec873de5839bee8d3f4c0239c4282c07c25c6077a2931afcf0adc0d34f` 与 `Np-AccountId: <base64(8 字节账号 ID)>`（`regist.c:133-229`）。加密方式：从内置表中按 `key_0_off = buf[0x18D] & 0x1F` 取出 bright，再把 **8 位 PIN** 异或到 bright 的最后 4 字节（`rpcrypt.c:2091-2096`）。
5. 响应解密后是 HTTP 头形式，包含 `PS5-RegistKey`（十六进制）、`RP-Key`（16 字节十六进制，也就是 **morning**）、`RP-KeyType`、`PS5-Mac`、`PS5-Nickname`、`AP-Ssid/AP-Bssid/AP-Key/AP-Name`、`RP-SupportCmd`。其中 RegistKey、RP-Key、Mac 三项必须存在（`regist.c:830-949`）。

### 3.2 PIN
- 在 PS5 上进入 `设置 → 系统 → 遥控游玩 → 连接设备` 获取，注册期间这个界面必须保持打开（`docs/setup/configuration.md:116-118`、`regist.c:510`）。有第三方指南称 PIN 有效期 300 秒，**未核实**。
- 另有一个可选的「Console PIN」，指主机的登录密码。会话时 ctrl 通道会发来 `LOGIN_PIN_REQ (0x4)`，客户端回复 `0x8004`（`ctrl.c:51-52,1013-1034`、`session.c:535-566`），与注册无关。

### 3.3 PSN account ID
- 它是 64 位整数，以小端 8 字节做 base64 后放进 `Np-AccountId`（`regist.c:211-220`；pyremoteplay `oauth.py:192`）。注册时 PS5 用它把这次注册绑定到主机上的某个 PSN 用户；相关错误码有 `INVALID_PSN_ID 0x80108b02`（`session.h:28`）。至于主机端怎样校验它，属于**推测**。
- 获取方式有两种：① 通过 PSN OAuth 登录（`gui/src/psnaccountid.cpp:15`，回调地址是 `remoteplay.dl.playstation.net`）；② 第三方公开查询 `https://psn.flipscreen.games/search.php?username=`（`gui/src/qml/PSNLoginDialog.qml:421`）。另外也可以从用户已知的数字 ID 手工转换。
- **客户端是否必须登录 PSN：不必须。** 本地注册路径不访问任何 Sony 服务器（`regist.c` 的非 PSN 分支只与主机通信），只需要一次性拿到账号 ID。**已核实（客户端侧）**。注册那一刻 PS5 自己是否需要联网，**未核实**。

### 3.4 长期凭据
- `ChiakiRegisteredHost` 保存 `rp_regist_key[16]`、`rp_key[16]`（即 morning）、`server_mac`、`server_nickname`、`rp_key_type`、`console_pin`（`regist.h:49-62`），GUI 把它们持久化到配置文件（`gui/src/host.cpp:57`）。
- 会话时只用到 regist_key、morning 和主机 IP：`RP-Registkey` 用于会话请求，`RP-Auth` 是用 rpcrypt 加密后的 regist_key（`session.c:245-246,893,904-914`、`ctrl.c:1246-1253`）。**直连会话不需要账号 ID，也不需要 PSN。已核实**
- **能否离线长期复用**：源码里没有任何刷新或过期逻辑，社区用法也是注册一次长期使用（据源码推断）。**PS5 是否会让凭据失效**：Sony 官方页只说有「连接历史」，没有提到删除或撤销（[Sony 支持页](https://www.playstation.com/en-sg/support/games/remote-play-settings/)）。恢复出厂、删除用户、系统大版本更新之后凭据是否还有效，**未找到证据，需要实测**。

## 4. 会话建立

### 4.1 会话请求（TCP 9295）`session.c:887-1103`
```
GET /sie/ps5/rp/sess/init HTTP/1.1
Host: <ip>:9295
User-Agent: remoteplay Windows
Connection: close
Content-Length: 0
RP-Registkey: <hex(regist_key)>
Rp-Version: 1.0
```
- 成功时返回 200 和 `RP-Nonce`（base64 编码的 16 字节）。失败时返回 `RP-Application-Reason`（十六进制），可能的值有 `0x80108b10` 正在使用中、`0x80108b15` 崩溃、`0x80108b11` 版本不符、`0x80108b09` 注册失败（`session.h:27-32`）。版本不符时，客户端改用服务器给出的 `RP-Version` 最多重试两次（`session.c:489-511`）。
- 协议版本：PS5 用 `RP-Version: 1.0`，PS4 用 8.0 / 9.0 / 10.0（`session.c:58-73`）。Takion 版本：PS5 为 12，PS4 为 9，Senkusha 为 7（`streamconnection.c:168`、`senkusha.c:158`）。

### 4.2 rpcrypt（ctrl 通道和 launchspec 用的对称加密）
- `chiaki_rpcrypt_init_auth(target, nonce, morning)`（`session.c:515`、`rpcrypt.c:1950-1954`）。
- PS5 派生方式：`ambassador[i] = (nonce[i]-0x2d-i) ^ keys_a[(nonce[0]>>3)*0x70 + i]`，`bright[i] = (morning[i]+0x18+i) ^ nonce[i] ^ keys_b[(nonce[7]>>3)*0x70 + i]`（`rpcrypt.c:1499-1530`）。
- 加密算法是 AES-128-CFB128：key = bright，IV = HMAC-SHA256(固定的 hmac_key_ps5, ambassador ‖ counter_be64) 的前 16 字节；两个方向各自维护递增计数器（`rpcrypt.c:2240,2259-2342`）。

### 4.3 ctrl 长连接（TCP 9295）`ctrl.c:1207-1541`
- 请求头：`RP-Auth: b64(enc(regist_key, ctr0))`、`RP-Did: b64(enc(DID32, ctr1))`（DID 由固定前缀 `00 18 00 00 00 07 00 40 00 80`、随机中段和 6 字节 0 组成，`session.c:252-256`）、`RP-ControllerType: 3`、`RP-ClientType: 11`、`RP-OSType: b64(enc("Win10.0.0"))`、`RP-ConPath: 1`、`RP-PadProcNo: 3`（只给 PS5；不带的话部分扳机效果格式会不对，`ctrl.c:1356-1358`）、`RP-StartBitrate: b64(enc(00000000))`、`RP-StreamingType: b64(enc(1=AVC / 2=HEVC / 3=HEVC HDR))`。
- 响应头：`RP-Server-Type`（加密，0 = PS4，1 = PS4 Pro，2 = PS5）、`RP-Prohibit`（`ctrl.c:1176-1205,1479-1522`）。
- 之后的帧格式：`[len u32 BE][type u16 BE][0 u16][payload]`，payload 用 rpcrypt 加密（`ctrl.c:441-459,683-762,845-855`）。主要消息类型：SESSION_ID 0x33（payload 以 0x4a 开头，内容为 24–78 位字母数字）、HEARTBEAT_REQ 0xfe 与 REP 0x1fe、GOTO_BED 0x50、开启 DualSense 功能 0x13 加 0x11、麦克风 0x30 / 0x36、显示禁播通知 0x1 / 0x16、SWITCH_TO_STREAM 0x34（只在 PSN 模式使用）（`ctrl.c:47-70`）。如果收不到 session id，客户端会自己生成一个 fallback id（`ctrl.c:807-843`）。

### 4.4 Takion 握手、ECDH 与密钥派生
1. Takion 四次握手：INIT → INIT_ACK（带 32 字节 cookie）→ COOKIE → COOKIE_ACK（`takion.c:860-953`）。
2. 客户端用随机的 16 字节 `handshake_key` 和 **secp256k1** 生成 ECDH 密钥对（`session.c:667-679`、`ecdh.c:59`）。
3. 发送 **BIG**（protobuf）：`client_version = 12`、`session_key = session id`、`launch_spec`（JSON 与 rpcrypt 计数器 0 的密钥流做 XOR 后再 base64）、`encrypted_key = 00000000`、`ecdh_pub_key`（65 字节未压缩点）、`ecdh_sig = HMAC-SHA256(handshake_key, pubkey)`。BIG 超过 MTU-50 时拆成 DATA + DATA_cont 发送（`streamconnection.c:995-1115`、`ecdh.c:163-166`）。
4. 收到 **BANG**：包含 `version_accepted`、`encrypted_key_accepted`，以及服务端的 pub key 和 sig，客户端据此算出 32 字节共享密钥。chiaki **不校验对端签名**（`ecdh.c:192-240` 没有使用 remote_sig 参数）（`streamconnection.c:745-854`）。
5. **GKCrypt 密钥派生**：`key_base ‖ iv = HMAC-SHA256(key = ecdh_secret, msg = 01 ‖ index ‖ 00 ‖ handshake_key ‖ 01 00)`，客户端发送方向 index = 2，PS5 发送方向 index = 3（`gkcrypt.c:111-164`、`streamconnection.c:725-740`）。
   - 载荷加密：AES-128-CTR，计数器 = iv + key_pos/16（`gkcrypt.c:217-279`）。
   - 完整性：对整个 UDP 包算 AES-GCM 的 GMAC，截断为 **4 字节**；计算时 MAC 字段清零，control 和 congestion 包的 key_pos 字段也清零（`takion.c:533-571`、`gkcrypt.c:358-400`）。
   - GMAC 密钥每 45000 字节 key_pos 轮换一次（`gkcrypt.h:20-21`、`gkcrypt.c:181-200,365`）。
6. **STREAMINFO**：PS5 发来分辨率列表（每个档位带 video_header，即 SPS/PPS）和 audio_header。客户端回复 STREAMINFOACK（channel 9）、CONTROLLERCONNECTION（DualSense 或 DS4），以及麦克风用的 STREAMINFO（16 bit 单声道 48 kHz，帧长 480）（`streamconnection.c:903-983,1117-1202`）。

## 5. Takion 传输层

源码注释原话：「VERY similar to SCTP, see RFC 4960」（`takion.c:39`）。

- **包类型**（首字节低 4 位）：0 CONTROL，1 FEEDBACK_HISTORY，2 VIDEO，3 AUDIO，4 HANDSHAKE，5 CONGESTION，6 FEEDBACK_STATE，8 CLIENT_INFO（`takion.c:61-70`）。各类型的 MAC 与 key_pos 偏移见 `takion.c:75-108`。
- **CONTROL 包**：`type(1) | tag(4) | gmac(4) | key_pos(4) | chunk_type(1) | flags(1) | len(2, 等于 payload+4) | payload`（`takion.c:1508-1553`）。chunk 类型有 DATA 0、INIT 1、INIT_ACK 2、DATA_ACK 3、COOKIE 0xa、COOKIE_ACK 0xb。
- **DATA chunk**：`TSN u32 | channel u16 | 0 u16 | data_type u8 | data`。data_type 取值：0 protobuf、7 rumble、9 pad info、11 trigger effects（`takion.c:613-661`、`takion.h:27-32`）。
- **可靠消息**：只有 DATA chunk 可靠。发送缓冲最多 1024 个包，每 200 ms 原样重发一次，最多 25 次（约 5 秒）（`takionsendbuffer.c:12-14`、`takionsendbuffer.h:21`）。接收端把包按序放出后回 DATA_ACK，内容为累计 TSN、a_rwnd（0x19000）和 gap 块数量，chiaki 发出的 gap 数为 0（`takion.c:712-731,1385-1499`）。数据重排队列只有 16 项（`takion.c:45`）。
- **不可靠消息**：音视频、输入（feedback）、拥塞报告都**没有 ack，也没有重传**。视频重排队列 64 项，队首缺包最多等 16 ms 就跳过（`takion.c:46-47,978-1060`）。
- **MTU 与分片**：Takion 不对音视频分片，一帧视频切成若干 unit，每个 unit 是一个 UDP 包。Senkusha 先做 RTT 测试（10 次 ping），再做 MTU 入向和出向二分测试（范围 576–1454，每次重试 3 次，超时 = 5×RTT 并限制在 5–500 ms），结果填进 launchspec 的 `mtu` / `rtt` 字段（`senkusha.c:252-275`、`streamconnection.c:1001-1002`）。Senkusha 失败时回退为 MTU 1454、RTT 1 ms，并关闭 DF 位（`session.c:641-646`）。默认开启 DF / PMTUDISC_DO（`takion.c:281-341`），接收缓冲按单包 1500 字节分配（`takion.c:1173`）。
- **心跳**：客户端每 1 秒在 Takion 上发一次 protobuf HEARTBEAT（可靠）（`streamconnection.c:35,314-325`）；ctrl 通道上由 PS5 发 HEARTBEAT_REQ，客户端回复（`ctrl.c:991-999`）。

## 6. 视频与音频

- **编码**：H.264 或 HEVC，也可以选 HEVC HDR（ctrl 的 `RP-StreamingType` 与 launchspec 的 `videoCodec` / `dynamicRange`，`launchspec.c:74-84`）。PS4 只支持 H.264（`ctrl.c:1510-1516`）。
- **分辨率与帧率档位**：360p / 540p / 720p / 1080p，对应默认码率 2 / 6 / 10 / 15 Mbps；帧率 30 / 60（`session.c:92-136`、`session.h:57-69`）。GUI 允许把码率设成 2–100 Mbps（`gui/src/qml/SettingsDialog.qml` 约 1401–1450 行）。Sony 官方说法是 PS5 遥控游玩会根据网络自动选择，最高 1080p（[Sony 支持](https://www.sony.ca/en/electronics/support/articles/SX365201)、[PSU](https://psu.com/news/playstation-remote-play-app-offers-up-to-1080p-resolution-for-the-ps5)）。PS5 Pro 不需要额外改动（维护者在 #404 的回复）。
- **码率由谁决定**：客户端在 launchspec 的 `bwKbpsSent` 里给出初始期望值；会话中由 PS5 根据客户端的丢包报告**自行下调或调整**。维护者在 #230 的原话大意是：主机会根据客户端上报的丢包降低码率。PS5 还会下发 `CONNECTIONQUALITY`，其中有 `target_bitrate`、`upstream_bitrate`、`rtt`、`loss`（`takion.proto:285-292`、`streamconnection.c:693-707`）。协议里还有 `INFO`、`FPSCHANGE`、`DEBUG/BITRATE` 等消息，chiaki 都没有使用（`takion.proto:102-116,143-145,247-250`）。客户端能否在会话中主动修改码率，**未知**。
- **分辨率自适应**：launchspec 带 `"adaptiveStreamMode": "resize"`（`launchspec.c:77`），PS5 在 STREAMINFO 里给出多个档位。每个视频包有 3 bit 的 `adaptive_stream_index`，客户端据此切换解码头（`takion.c:1811`、`videoreceiver.c:157-175`）。也就是说，**PS5 可以在会话中途切换分辨率**。**已核实（机制层面）**
- **视频包头**（v12）：`packet_index u16 | frame_index u16 | dword(unit_index 11 bit, units_total-1 11 bit, units_fec 10 bit) | codec u8 | gmac | key_pos | 未知 1 字节 | word_at_0x18 u16 | adaptive_stream_index(高 3 bit) [| 3 字节 NALU info]`（`takion.c:1758-1848`）。
- **单元结构**：每个 unit 解密后，前 2 字节（大端）是 padding 长度扩展，所有 unit 补齐到相同长度（`frameprocessor.c:81-91,252-266`）。把各源 unit 去掉 2 字节头后拼接，就得到**完整的 Annex-B 访问单元**（带起始码）。一帧最多 512 个 unit（`frameprocessor.c:37,273-318`）。
- **FEC**：每帧独立做 **Cauchy Reed-Solomon**（jerasure 库，`cauchy_original_coding_matrix`，w = 8）。k = 源 unit 数，m = FEC unit 数，由 PS5 逐帧决定并写在包头里（`fec.c:12-75`、`fec.h:17`）。FEC 作用在**解密后的明文 unit** 上：每个包先按自己的 key_pos 解密，再做 FEC（`streamconnection.c:1235`、`frameprocessor.c:206-271`）。冗余比例由 PS5 决定，客户端无法设置；典型比例**未知，需抓包统计**。
- **丢帧恢复**：
  - 帧号出现跳跃时，客户端发 `CORRUPTFRAME(start, end)`（channel 2）（`videoreceiver.c:191-199`）。
  - FEC 失败时同样上报 CORRUPTFRAME；可选再发 `IDRREQUEST`，之后每秒重发一次，直到收到 I slice，期间跳过 P 帧。这个选项默认关闭（`videoreceiver.c:222-295`、`gui/include/settings.h:302`；提交 `31a469180`、`8134f458b`）。
  - 客户端还有一个技巧：如果 P slice 引用了丢失的帧，就改写 slice header，让它引用一个仍然存在的更旧参考帧，最多回看 16 个参考（`videoreceiver.c:297-325`、`bitstream.c:156-225,302+`）。由此可以推断 PS5 编码器会保留多个参考帧。
  - PS5 收到 CORRUPTFRAME 后具体怎么做（帧内刷新还是切换参考），**推测**。
  - 协议里有 `PACKETLOSS(start, end, packets_received)` 消息，但 chiaki 没有使用。PS5 是否支持重传，**未知**。
- **音频**：Opus（codec 字节为 5，`audioreceiver.c:88`）。参数来自 STREAMINFO 的 audio_header（声道、位深、采样率、帧长）。音频包里的 "fec unit" 其实是**前 N 帧的重复副本**，不是纠删码（`audioreceiver.c:121-135`）；单元数量编码在 `units_in_frame_fec` 里（`takion.h:55-57`）。v12 版本的音频包可以用标记 0x02 表示 DualSense **触觉（haptics）流**（`takion.c:1837-1842`）。

## 7. 拥塞控制

- 客户端每 200 ms 发一个 CONGESTION 包（类型 5，共 15 字节）：`word_0 = 0 | received u16 | lost u16 | 0 | key_pos`，带 GMAC（`congestioncontrol.c:5-40`、`takion.c:733-753`）。
- 统计口径：视频按每帧「实际收到 unit 数 / 应收 unit 数」，音频按序号缺口（`frameprocessor.c:199-204`、`packetstats.c:58-83`）。
- chiaki-ng 会把上报的丢包率**封顶**在 `packet_loss_max`，默认 5%（`congestioncontrol.c:28-34`、`gui/src/settings.cpp:740-747`）。原因是 PS5 会根据丢包降码率甚至降分辨率；维护者在 #230 说封顶后「prioritize bitrate over stream connection」。**已核实：PS5 会根据客户端上报调整码率**（以维护者原话为据）。具体的控制曲线**未知**。

## 8. 手柄输入与反馈

- **FEEDBACK_STATE**（类型 6）：摇杆、陀螺仪（±30）、加速度（±5）、压缩后的姿态四元数，v12 共 0x1c 字节（`feedback.c:16-80`）。状态变化时立即发送，没有变化时至少每 200 ms 发一次。最小间隔 8 ms 只是常量，代码里还是 TODO，并未执行（`feedbacksender.c:8-9,284-302`）。
- **FEEDBACK_HISTORY**（类型 1）：按键、扳机、触摸板事件。环形缓冲 16 项，每个包都重复携带最近 8 个事件，并带序号；主机每个包只应用一个新事件。没有新事件时，最后一个包会在 20 / 50 / 100 / 200 ms 再补发，防止「按键卡住」（`feedbacksender.c:10-27`、`feedback.h:89-100`、`takion.c:841-858`）。整个过程是不可靠 UDP。
- 以上输入包都用 gkcrypt_local 加密并带 GMAC，而且共用**同一个单调递增的 key_pos 计数器**（`takion.c:477-501,755-785`）。
- **PS5 发给客户端的反馈**：
  - rumble（data type 7，3 字节）；
  - 扳机效果（type 11，左右各有 type 字节和 10 字节参数）；
  - pad info（type 9：灯条 RGB、玩家序号、姿态重置、触觉和扳机强度）（`streamconnection.c:463-640`）；
  - haptics 走音频流。
  - 开启 DualSense 功能要发 ctrl 0x13 和 0x11（`ctrl.c:901-924`）。
- **麦克风**：先发 ctrl MIC_CONNECT 0x30 和 TOGGLE 0x36，再发一个声明麦克风格式的 STREAMINFO。数据格式是 Opus、48 kHz、单声道、10 ms 一帧，每个包带 3 个 unit（含冗余），加密后发出（`streamconnection.c:1148-1181`、`audiosender.c:19,126`、`takion.c:809-839`）。

## 9. 超时与断线

| 阶段 | 阈值 | 出处 |
|---|---|---|
| 会话请求的连接和响应 | 各 5 s | `session.c:31,840,986` |
| ctrl 请求 | 5 s，超时后重连重发 1 次 | `ctrl.c:34,1400-1413` |
| 等待 ctrl 启动或 session id | 10 s | `session.c:33,526` |
| Senkusha | 连接 30 s，期待消息 5 s，pong 1 s | `senkusha.c:31-35` |
| Takion INIT / COOKIE | 5 s × 3 次 | `takion.c:55-57` |
| BANG / STREAMINFO | 各 5 s | `streamconnection.c:33,250,274` |
| 可靠 DATA 重传 | 200 ms × 25 次 | `takionsendbuffer.c:12-14` |
| 客户端 Takion 心跳 | 1 s | `streamconnection.c:35` |
| 主机侧心跳超时 | ctrl 心跳回复中断 30 s，主机结束会话 | chiaki-ng 提交 `021244547` 的说明、`ctrl.c:42-45` |
| feedback state 保活和拥塞报告 | 200 ms | 见上文 |

- chiaki 的 Takion 接收循环**没有「无数据超时」**（`takion.c:1132-1202`），只能依赖 PS5 发来的 DISCONNECT 或 socket 错误来判断断线。
- **断线后快速恢复**：实现中没有任何会话恢复或续期令牌。重连就是完整走一遍 init → ctrl → senkusha → ECDH，每次都有新的 nonce 和新的密钥（据源码推断）。
- 同一时间只允许一个会话，第二个客户端会收到 `IN_USE 0x80108b10`。
- 社区报告过异常断开后需要重启 PS5 才能恢复（#485，表现为 9297 打不开），PSN 模式下还有一段「冷却期」（#404）。

## 10. PS5 是否限制非同网段客户端

- **直连路径的代码里没有 TTL 或子网相关逻辑**。`IP_TTL` 只出现在 PSN 打洞代码里：打洞时设成 2，之后恢复为 64（`remote/holepunch.c:4168-4176,4485-4596`）。注册请求的 Host 头是写死的占位地址 10.0.2.15，PS5 不校验（#867 维护者回复）。
- **实际案例**：
  - 官方文档说明可以通过端口转发做手动远程连接（`docs/setup/remoteconnection.md:83-161`）；
  - 跨 VLAN 或 WireGuard VPN 环境下，手动填内网 IP 可以连接（#286）；
  - 通过 Tailscale 远程游玩可用（#713）；
  - 通过 ZeroTier 加 CCproxy 用户态端口映射可用（#366）。最后这个案例说明 **PS5 接受来自内网代理 IP 的连接**，协议不绑定客户端 IP。
  - 结论：**已核实（多源社区证据加源码）**。
- 广播发现跨不了子网，这是正常的 L2 行为；单播发现和单播唤醒可以跨网段。
- **Senkusha 的真实作用**：测 RTT 和双向 MTU，结果填进 launchspec 的 `mtu` / `rtt`。它不加密，看不出起门禁作用。chiaki 在 Senkusha 失败时仍会用默认值继续连接（`session.c:635-646`）。但维护者在 #672 把「9297 没转发」认定为 Stream Connection 失败的原因，所以实际部署中应当保证 9297 可达。PS5 是否用 Senkusha 测出的 RTT 限制远程连接，**未找到证据**。
- IPv6：chiaki 文档称 PSN 模式只支持 IPv4（`remoteconnection.md:5`），#366 也说 PS5 不支持通过 IPv6 遥控游玩。直连模式下 PS5 是否监听 IPv6，**未核实**。
- Sony 官方要求「配对时连接同一网络」（[Sony 支持页](https://www.playstation.com/en-sg/support/games/remote-play-settings/)）。跨网段单播注册在 #286 中有人报告成功，但证据较弱，**推测可行**。

## 11. 架构评估：「局域网网关终结 Takion，再用自研抗弱网协议转发到远端」

**总体结论**：这个架构和协议特性**高度契合**。PS5 那一侧所有对时间敏感的环节（Takion ack、ctrl 心跳、Takion 心跳、200 ms 的 feedback 保活、拥塞报告、IDR 请求）都可以由网关在局域网 RTT 下完成，广域网只需要传「明文码流、输入事件、少量控制消息」。

| 约束 | 评估 | 依据 |
|---|---|---|
| 对时间敏感的 ack | 只有 DATA chunk 需要 ack，且由网关在局域网内处理；音视频不需要 ack。广域网与 PS5 的重传计时完全解耦。 | `takion.c:1385-1434`、`takionsendbuffer.c` |
| 加密绑定 | 每个会话的密钥来自 ECDH 加 handshake_key，GMAC 只有 4 字节且依赖 key_pos 状态。**不建议把加密的 Takion 包原样转发给远端**：要同步密钥和 key_pos，输入方向还得共用同一个 key_pos 计数器。网关解密后用自有协议重新加密即可。远端客户端不需要知道 regist_key 或 morning（凭据只存在网关上，更安全）。 | `gkcrypt.c`、`takion.c:477-501` |
| 网关不解码就能拿到完整 NAL | **可以**。FEC 恢复并拼接 unit 后，直接得到 Annex-B 访问单元；SPS/PPS 来自 STREAMINFO 的各档位头；I/P 判断只需解析 slice header，开销很小。注意切换 `adaptive_stream_index` 时，要把新的头转发给远端。 | `frameprocessor.c:273-318`、`videoreceiver.c:157-175`、`bitstream.c` |
| FEC 能否原样透传 | 技术上可以：网关把解密后的源 unit 和校验 unit 一起转发，远端用同样的 jerasure Cauchy RS 解码。但冗余比例由 PS5 按「网关这一跳」的丢包决定，局域网丢包接近 0，冗余很可能很低，不足以对付广域网。**建议广域网重新做 FEC，并配合选择性重传（ARQ）**。 | `fec.c`、`frameprocessor.c:76-79` |
| MTU | 网关拿到的是明文帧，可以按广域网隧道 MTU 重新切片，不受 PS5 包大小影响。也可以考虑在 launchspec 里报一个较小的 mtu，让 PS5 直接切小包；PS5 是否按这个值打包属于**推测**。 | `streamconnection.c:1001`、`launchspec.c:26` |
| 码率控制 | PS5 唯一能看到的拥塞信号就是网关上报的 received / lost。网关可以把**广域网的带宽和丢包估计「注入」拥塞报告**，驱动 PS5 编码码率，再用 `CONNECTIONQUALITY.target_bitrate` 观察效果，形成闭环。这是最关键的调节手段，但 PS5 的响应曲线需要实测。 | `congestioncontrol.c`、#230 |
| 远端丢帧时如何让 PS5 出 IDR | 远端报告无法恢复的帧，网关就发 `CORRUPTFRAME(start, end)`，必要时再发 `IDRREQUEST`（并像 chiaki 那样每秒重试）。IDR 帧很大，会引起码率尖峰，所以应当作为最后手段：先靠广域网 ARQ 和 FEC，再试参考帧回退，最后才请求 IDR。PS5 的 GOP 结构（是否只在开始和请求时出 IDR）**未知**。 | `streamconnection.c:1263-1302`、`videoreceiver.c` |
| 输入延迟 | 总延迟 = 远端到网关的广域网单程时延 + 局域网时延。网关收到后要立即转换成 FEEDBACK_STATE / HISTORY 包，不能攒批；HISTORY 每个事件要单独一个包，序号和尾部补发都在网关本地完成；广域网侧建议每个包都带完整手柄状态，并做冗余或可靠投递。 | `feedbacksender.c`、`feedback.h:89-100` |
| 会话韧性 | 远端掉线时，网关继续给 PS5 发心跳和中性手柄状态，保持会话不断；远端重连后网关请求一次 IDR 即可恢复画面，**省掉重新握手的若干秒**，也避开 IN_USE 冲突和冷却期问题。这是提升「稳定」最大的单项收益。长时间空闲是否会触发 PS5 的 AFK 超时，**未知**。 | `session.h:29`、第 9 节 |
| 端口暴露 | 网关只需在局域网内访问 PS5 的 9295/9296/9297/9302，PS5 不需要对公网做端口转发；公网只暴露自研协议端口，安全性更好。 | 第 1 节 |
| 音频、触觉、麦克风 | Opus 帧可以直接转发，广域网侧自己做冗余；扳机效果、灯条、rumble 是小消息，走可靠通道；麦克风反向发 Opus 即可。 | 第 6、8 节 |

## 12. 来源
- chiaki-ng 源码：https://github.com/chiaki-ng/chiaki-ng （提交 43728af）
- chiaki-ng 文档：`docs/setup/remoteconnection.md`、`docs/setup/configuration.md`
- chiaki-ng issue：[#230](https://github.com/chiaki-ng/chiaki-ng/issues/230)、[#286](https://github.com/chiaki-ng/chiaki-ng/issues/286)、[#366](https://github.com/chiaki-ng/chiaki-ng/issues/366)、[#404](https://github.com/chiaki-ng/chiaki-ng/issues/404)、[#485](https://github.com/chiaki-ng/chiaki-ng/issues/485)、[#672](https://github.com/chiaki-ng/chiaki-ng/issues/672)、[#713](https://github.com/chiaki-ng/chiaki-ng/issues/713)、[#836](https://github.com/chiaki-ng/chiaki-ng/issues/836)、[#867](https://github.com/chiaki-ng/chiaki-ng/issues/867)
- chiaki-ng 提交说明：`021244547`（心跳 30 s）、`31a469180` 与 `8134f458b`（IDR 请求）
- pyremoteplay：https://github.com/ktnrg45/pyremoteplay （已归档）
- Sony：[遥控游玩设置](https://www.playstation.com/en-sg/support/games/remote-play-settings/)、[Sony 支持 SX365201](https://www.sony.ca/en/electronics/support/articles/SX365201)；[PSU 报道](https://psu.com/news/playstation-remote-play-app-offers-up-to-1080p-resolution-for-the-ps5)
- 社区端口说法（未核实）：[Reddit 汇总](https://redlib.belloworld.it/r/PS5/comments/kx8mhy/remote_play_from_internet_firewall_ports)、[inmoxr 指南](https://support.inmoxr.com/air3/guides/ps5-remote-ultimate-wan/)


---

## 附录：本方向的关键论断与待确认问题（复核结果见 V1/V2/V3）

- [B1] (high,已核实) PS5 直连所需端口：TCP 9295（注册 / 会话请求 / ctrl 长连接）、UDP 9295（注册前的 SRC3/RES3 搜索）、UDP 9296（Takion 主流）、UDP 9297（Senkusha 测速）、UDP 9302（发现与唤醒）；PS4 的区别是发现端口为 987。
  src: chiaki-ng lib/include/chiaki/discovery.h:24-29; lib/src/regist.c:26,465-474; session.c:29; ctrl.c:32; streamconnection.c:31; senkusha.c:29; docs/setup/remoteconnection.md:97-113
- [B2] (medium,已核实) 唤醒包 WAKEUP 是明文 UDP，只需要 regist key：把它的 ASCII 十六进制内容解析成 uint64 后，以十进制写进 user-credential；包里没有加密或 MAC。
  src: discovery.c:76-85,440-499; gui/src/discoverymanager.cpp:287; pyremoteplay util.py:133-135
- [B3] (high,已核实) 注册是一次性的本地流程：需要 PS5「连接设备」界面上的 8 位 PIN 和 PSN 账号 ID（8 字节小端后 base64），客户端不访问任何 Sony 服务器，得到 PS5-RegistKey 和 RP-Key（即 morning）。
  src: regist.c:168-229,830-949; rpcrypt.c:2091-2096
- [B4] (high,已核实) PSN 账号 ID 可以通过 PSN OAuth 登录或第三方公开查询（psn.flipscreen.games）获得；客户端只在注册时需要它，此后的会话不需要 PSN 登录，也不需要账号 ID。
  src: gui/src/psnaccountid.cpp:15; gui/src/qml/PSNLoginDialog.qml:421; session.c:245-246,887-914
- [B5] (high,推测) 直连会话只需 regist_key、morning 和主机 IP；chiaki 代码里没有凭据刷新或过期逻辑，可以离线长期复用。PS5 端是否会让凭据失效（恢复出厂、删除用户、系统更新等情况）没有找到证据。
  src: regist.h:49-62; session.c:245-246; ctrl.c:1246-1253; Sony 遥控游玩设置页
- [B6] (high,已核实) 会话建立流程：先 GET /sie/ps5/rp/sess/init（RP-Registkey、Rp-Version: 1.0）拿到 RP-Nonce；再用 nonce 和 morning 派生 rpcrypt（AES-128-CFB，IV = HMAC-SHA256）；接着 ctrl 长连接；之后是 Senkusha；最后在 Takion v12 上交换 BIG/BANG（secp256k1 ECDH，签名为 HMAC-SHA256(handshake_key, pub)）。
  src: session.c:487-682,887-1103; rpcrypt.c:1499-1530,2259-2342; ctrl.c:1325-1364; streamconnection.c:745-854,995-1115; ecdh.c:59,163-166
- [B7] (high,已核实) 音视频和输入载荷用 AES-128-CTR 加密（key_base 和 iv 由 HMAC-SHA256(ecdh_secret, 01||index||00||handshake_key||0100) 派生，客户端发送 index 为 2、PS5 发送为 3），每个包带 4 字节 GMAC，GMAC 密钥每 45000 字节 key_pos 轮换；所有发出的包共用一个单调递增的 key_pos。
  src: gkcrypt.c:111-164,181-200,358-400; gkcrypt.h:17-21; takion.c:477-501,533-571; streamconnection.c:725-740
- [B8] (high,已核实) Takion 类似 SCTP：只有 DATA chunk 可靠（累计 TSN 的 DATA_ACK，客户端每 200 ms 重发、最多 25 次）；音视频、输入、拥塞报告都是不可靠 UDP，没有 ack 和重传；chiaki 也没有使用协议里的 PACKETLOSS 消息。
  src: takion.c:39,61-70,613-731,1385-1499; takionsendbuffer.c:12-14; takion.proto:137-141
- [B9] (high,已核实) 视频帧被切成等长 unit（每个 unit 一个 UDP 包，开头 2 字节是 padding 头），PS5 逐帧决定 k 个源 unit 加 m 个校验 unit，做 Cauchy Reed-Solomon（jerasure，w = 8）；FEC 作用在逐包解密后的明文上，冗余比例客户端无法控制。
  src: frameprocessor.c:67-148,206-318; fec.c:12-75; fec.h:17; streamconnection.c:1235; takion.c:1785-1797
- [B10] (high,已核实) FEC 恢复并拼接后得到完整的 Annex-B 访问单元，SPS/PPS 由 STREAMINFO 的各分辨率档位给出，所以网关不需要解码就能拿到完整 NAL，判断 I/P 帧只需解析 slice header。
  src: frameprocessor.c:292-317; streamconnection.c:863-901; bitstream.c:156-225
- [B11] (high,已核实) 客户端每 200 ms 上报 received/lost 拥塞包，PS5 据此调整（下调）码率甚至分辨率；chiaki-ng 默认把上报丢包率封顶在 5%；PS5 还下发 CONNECTIONQUALITY（target_bitrate、rtt、loss）。
  src: congestioncontrol.c:5-40; gui/src/settings.cpp:740-747; streamconnection.c:693-707; chiaki-ng issue #230 维护者回复
- [B12] (medium,已核实) launchspec 带 adaptiveStreamMode: resize 时，PS5 会给出多个分辨率档位，并可以在会话中途通过视频包里 3 bit 的 adaptive_stream_index 切换；最高 1080p，帧率 30 或 60；初始码率由 bwKbpsSent 给出。
  src: launchspec.c:9-91; takion.c:1811; videoreceiver.c:157-175; session.c:92-136; Sony 支持 SX365201
- [B13] (high,已核实) 丢帧恢复手段：上报 CORRUPTFRAME(start, end)、发 IDRREQUEST（PS5 会出关键帧，chiaki 每秒重试，此选项默认关闭），以及客户端改写 P slice 的参考帧索引、回退到仍存在的旧参考帧（最多 16 个）。
  src: videoreceiver.c:11,191-199,222-325; streamconnection.c:1263-1302; gui/include/settings.h:302; 提交 31a469180、8134f458b
- [B14] (high,已核实) 输入：FEEDBACK_STATE（摇杆、陀螺仪、加速度、姿态）在变化时发送，至少每 200 ms 一次；FEEDBACK_HISTORY（按键、扳机、触摸）每个事件单独一个包，携带最近 8 个事件，并在 20/50/100/200 ms 尾部补发；全部是不可靠 UDP。
  src: feedbacksender.c:8-27,230-355; feedback.c:16-80; feedback.h:89-100; takion.c:787-858
- [B15] (high,已核实) PS5 在 30 秒内收不到 ctrl 心跳回复就结束会话；客户端每 1 秒发一次 Takion 心跳；会话没有恢复机制，重连要完整重新握手，且同一时间只允许一个会话（IN_USE 0x80108b10）。
  src: ctrl.c:42-45,991-999; 提交 021244547; streamconnection.c:35,314-325; session.h:29
- [B16] (high,已核实) 直连路径中 PS5 不限制非同网段客户端：代码里没有 TTL 或子网逻辑；端口转发、跨 VLAN/VPN、Tailscale 以及内网用户态端口代理都有成功案例。
  src: remote/holepunch.c:4168-4176（TTL 仅用于 PSN 打洞）; docs/setup/remoteconnection.md:83-161; issue #286、#366、#713、#867
- [B17] (medium,已核实) Senkusha 的作用是测 RTT（10 次 ping）和双向 MTU（576–1454 二分），结果写进 launchspec 的 mtu/rtt；chiaki 在它失败时用 MTU 1454、RTT 1 ms 继续连接，但维护者认为 9297 必须可达。
  src: senkusha.c:31-35,148-158,252-275; session.c:618-646; streamconnection.c:1001-1002; issue #672
- [B18] (high,推测) 网关架构可行：PS5 侧所有对时间敏感的 ack、心跳、保活、拥塞上报都能在局域网内由网关完成；网关可以把广域网的丢包和带宽估计注入拥塞包来驱动 PS5 码率，也能在远端掉线时保持 PS5 会话、远端重连后只需请求 IDR。
  src: 根据第 5、7、9、11 节源码分析推断

## open_questions
- PS5 收到 CORRUPTFRAME 后的具体行为（帧内刷新、切换参考帧还是出 IDR）以及 IDR 响应延迟是多少？需要抓包实测。
- PS5 视频的 FEC 冗余比例（m/k）有多大？它会不会随上报丢包率变化？需要在不同的上报丢包率下统计 units_in_frame_fec。
- PS5 的码率控制曲线：bwKbpsSent 最高能被采纳到多少（UI 允许 100 Mbps）？拥塞上报的 received/lost 怎样映射到 target_bitrate 和分辨率切换？
- PS5 是否会按 launchspec 里的 mtu 字段打包？报一个较小的 MTU 能否让 PS5 直接输出适合广域网隧道的小包？
- PS5 的 GOP 结构（是否只在会话开始和收到请求时出 IDR、IDR 帧典型大小）需要实测，这关系到网关怎样设计 GOP 缓存和 ARQ。
- 协议里的 PACKETLOSS 消息（start、end、packets_received）PS5 是否支持，能否触发重传？
- 网关保持会话、远端长时间不在时，PS5 是否有 AFK 或空闲超时（launchspec 里 afkTimeout、startTimeout 的含义）？
- regist key 和 morning 的长期有效性：PS5 系统大版本更新、删除用户、恢复出厂、反复重新配对后，旧凭据是否失效？PS5 能否手动删除已配对设备？连接设备数量有没有上限？
- 注册那一刻 PS5 本身是否必须联网或登录 PSN？跨网段单播注册（不在同一 L2）是否稳定可用？
- PS5 直连模式是否监听 IPv6（chiaki 文档只说 PSN 模式仅支持 IPv4）？
- PS5 心跳请求（HEARTBEAT_REQ）的发送间隔、以及 Takion 层「多久收不到客户端数据就断开」的阈值，源码里看不出来，需要实测。
- 需要用户决定：是否直接复用 chiaki-ng 代码（AGPL-3.0-only 加 OpenSSL 例外，网关作为网络服务也有公开源码义务），还是参考协议另行实现（clean-room 重写）。
- 远端客户端的形态（PC / 手机 / 掌机）和解码能力，会决定网关是否要转码或改写参考帧，需要用户确定目标平台。
