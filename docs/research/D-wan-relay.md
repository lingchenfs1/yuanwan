# 调研报告 D：PS5 Remote Play 的公网直连、VPN 与自建中转（不用 Sony / PSN 中转）

调研日期：2026-11-19。只读调研，所有材料放在临时目录 本地临时目录（不随仓库发布），没有执行下载内容里的任何脚本。

**主要依据（按可信度排序）**
- chiaki-ng 源码：仓库已从 `streetpea/chiaki-ng` 迁到 `chiaki-ng/chiaki-ng`，读的是 main HEAD `43728af`（提交日期 2026-11-03，对应最新正式版 v1.10.1，发布于 2026-11-03）。
- chiaki-ng 官方文档 `docs/setup/remoteconnection.md`。
- GitHub issue 里开发者 streetpea 的回复。
- frp v0.72.0、Tailscale v1.104.7 的源码。
- Sony、Tailscale、WireGuard、Hysteria 的官方页面。
- 社区帖子（Reddit 本机无法直接访问，只能借助搜索摘要，可信度标为低）。

**标注约定**：【已核实】表示有源码、官方文档或开发者原话作依据；【推测】表示由已核实事实推出，或只有单一或匿名来源。

---

## 0. 结论速览

1. **原生 Remote Play 协议的端口是写死的。** 客户端固定连 PS5 的 TCP 9295、UDP 9296/9297/9302（PS4 用 987 代替 9302）。chiaki-ng 到 v1.10.1 也不支持按主机设置端口，所以一个公网 IP 后面挂两台主机，用端口映射无法同时远程。【已核实】
2. **PS5 不听 IPv6 上的 Remote Play。** chiaki-ng 开发者实测得出这一结论，PXPlay 手册也这样写。只有 IPv6 的家庭网络，必须在家里放一台设备做 v6→v4 转发，或者走隧道。【已核实，依据是开发者结论】
3. **没找到 PS5 拒绝非本地客户端（检查 TTL 或子网）的证据。** 端口映射时 PS5 看到的是客户端的公网 IP，走路由型 VPN 时看到的是别的网段，都能正常串流。真正的坑在这几处：①发现广播过不了三层；②MTU；③回程路由不对称。【前半句是推测（没有找到证据），后半句已核实】
4. **MTU 是 VPN 和中转方案最隐蔽的稳定性杀手。** senkusha 握手阶段（走 UDP 9297）在 576–1454 字节之间探测 MTU，失败了不会中止，而是回退到 1454。WireGuard 跑在 IPv4 上要多 60 字节，所以任何 1500 的链路都装不下不分片的 1454 包；Tailscale 默认 TUN MTU 只有 1280。社区已有「低 MTU 路径加 senkusha 失败导致串流冻结，手动改成 1280 后正常」的实例。【已核实】
5. **唤醒包是单播 UDP**（`WAKEUP * HTTP/1.1`，带 regist key 生成的凭据，发往 9302），不是 WoL 魔术包，也不是广播。因此可以经端口映射或路由型 VPN 送达。主机侧要开启待机时「保持连接到互联网」和「允许通过网络开启 PS5」。【已核实】
6. **PSN 打洞的信令完全走 PSN**（sessionManager 和推送 WebSocket），主机端只认 PSN。自建信令无法让 PS5「配合打洞」，所以 CGNAT 或纯 IPv6 环境下，家里必须有一台常开设备（路由器、NAS、树莓派等）做 VPN 端点或网关。【信令走 PSN 已核实；「只能放家庭网关」是推测，但依据很强】
7. **frp 的 UDP 代理是把 UDP 包塞进一条可靠有序的工作连接**（默认 TCP）。丢包时会队头阻塞，不适合实时串流。iptables DNAT 或 WireGuard 内核转发最好。【封装方式已核实，影响是推测】
8. **国内运营商的 UDP QoS 只有社区和厂商文档层面的说法**（Hysteria 文档、udp2raw 文档），没有权威数据，必须在用户自己的线路上做晚高峰实测。【推测】

---

## 1. 公网直连

### 1.1 端口表（区分 PS5 与 PS4、TCP 与 UDP，并对应源码）

| 端口 | 协议 | 用途 | PS5 | PS4 | 依据 |
|---|---|---|---|---|---|
| 9295 | TCP | 会话请求（`GET /sie/ps5/rp/sess/init`）和整个会话期间的 ctrl 控制通道 | 需要 | 需要 | `lib/src/session.c:29`、`lib/src/ctrl.c:32` |
| 9295 | UDP | 注册时的搜索包（`SRC3`/`RES3`，PS4 是 `SRC2`） | 只在远程注册时需要 | 同左 | `lib/src/regist.c:26,465-475` |
| 9296 | UDP | Takion 音视频流和手柄、反馈、麦克风上行 | 需要 | 需要 | `lib/src/streamconnection.c:31` |
| 9297 | UDP | senkusha：测 RTT 和双向 MTU | 强烈建议 | 强烈建议 | `lib/src/senkusha.c:29` |
| 9302 | UDP | 发现（`SRCH`）和唤醒（`WAKEUP`） | 需要（要唤醒或判断状态时） | 不用 | `lib/include/chiaki/discovery.h:26` |
| 987 | UDP | 发现和唤醒 | 不用 | 需要 | `discovery.h:24` |
| 9303–9319 | UDP | **客户端本地**绑定的发现端口，**不用在家里映射** | – | – | `discovery.h:28-29`、`discovery.c:155-199` |

- 官方文档的端口表与上面一致：PS5 是 9295 UDP/TCP、9296、9297、9302 UDP；PS4 用 987 代替 9302（`docs/setup/remoteconnection.md:97-113`）。PXPlay（闭源商业客户端）的手册也一样：<https://streamingdv.github.io/pxplay>。【已核实】
- 客户端源端口是临时随机端口。Takion 的 UDP socket 调用了 `connect()`（`lib/src/takion.c:430`），PS5 只回包到客户端的源地址和端口，对 conntrack 或 NAT 友好。开发者在 #428 也明确说本地是随机临时端口：<https://github.com/chiaki-ng/chiaki-ng/issues/428>。【已核实】
- **8572、9303、9308 等端口是 Sony 官方 App 走 PSN 打洞时的端口**，不属于手动直连。Reddit r/PS5 有人反映 PS5 会用 UPnP 自动映射 UDP 8572/9297/9303/9308（只能看到搜索摘要，原帖 <https://www.reddit.com/r/PS5/comments/kx8mhy/> 本机打不开）。手动直连不需要这些端口，同时开 UPnP 和手动映射在部分路由器上会冲突（streetpea 在 #343 提到过）。【推测，来源为社区】
- 原版 chiaki 作者 thestr4ng3r 在 2019 年说过：PS4 用 987 做发现，9303/9304 根本不用；串流需要 TCP 9295 和 UDP 9296/9297。见 <https://github.com/thestr4ng3r/chiaki/issues/67>。【已核实】

### 1.2 一个公网 IP 下有多台主机

- chiaki-ng 的端口都是编译期常量（`SESSION_PORT 9295`、`STREAM_CONNECTION_PORT 9296`、`SENKUSHA_PORT 9297`），GUI 里也没有端口字段。v1.10.1 仍不支持自定义端口。【已核实】
- 功能请求 #242「Allow custom ports」（2024-02）后来因长期无人跟进被自动关闭，#391 被标为重复。用户实测：官方 App 和 chiaki 用的是同一组端口，两台主机同时远程时，一台或两台都会失败。见 <https://github.com/chiaki-ng/chiaki-ng/issues/242>。【已核实】
- #58 还发现：端口映射指向 PS5 时去连 PS4，会报「RP version is different」。PS4 和 PS5 共用 9295，映射同一时间只能指向一台主机。见 <https://github.com/chiaki-ng/chiaki-ng/issues/58>。【已核实】
- 解决思路：
  1. 自研客户端支持按主机配置外部端口，路由器把外部 19295 映射到 PS5-B 的 9295。PS5 的会话请求头里有 `Host: <地址>:<端口>`（`session.c:887-895`），端口改了 PS5 是否在意，**需要实测**。
  2. 用 VPN 或家庭网关直接访问每台主机的局域网 IP，这样根本不会有端口冲突。
  3. 每台主机配一个公网 IP，或者经 VPS 给每台主机分一个出口 IP。

### 1.3 CGNAT

- 在 CGNAT 后面，路由器上做端口映射无效。判断方法：比较路由器 WAN 口 IP 和 whatismyip 看到的 IP，或者看 WAN IP 是不是私网地址或 100.64/10。开发者在 #343 和 #470 反复给出这套判断方法。官方文档的建议是：要么走 PSN 打洞，要么在家里搭 VPN（`remoteconnection.md:157`）。【已核实】
- 用户的场景排除了 PSN，所以 CGNAT 下只剩两条路：「家里的常开设备主动连出去（连 VPS 或 VPN）」，或者「家里设备有公网 IPv6，做 v6→v4 转发」。

### 1.4 IPv6

- 文档写明 PSN 远程连接只支持 IPv4，原因在 PlayStation 一侧（`remoteconnection.md:5`）。【已核实】
- streetpea 在 #323（2024-05-31）和 #343 里说：测试发现 PS5 没有在 IPv6 socket 上监听 Remote Play 请求。见 <https://github.com/chiaki-ng/chiaki-ng/issues/323>。PXPlay 手册也写了 Remote Play 不支持 IPv6。【已核实，属于开发者实测结论，没有找到 Sony 官方说法】
- 打洞代码里 `ENABLE_IPV6 false`（`lib/src/remote/holepunch.c:96`）。【已核实】
- 社区的纯 IPv6 解法（#366，中文帖，2024-06）：家里一台电脑与客户端组 ZeroTier 虚拟 IPv4 网络，再用 CCproxy 把 TCP/UDP 9295、9296、9297、9302 转发到 PS5。用 Windows `netsh portproxy` 时，PS5 会提示「正在远程游玩」，但一直没有画面，原因是 netsh 只转 TCP、不转 UDP。见 <https://github.com/chiaki-ng/chiaki-ng/issues/366>。这说明 PS5 接受由家里设备做 SNAT 代理过来的连接。【已核实，属单一用户报告】

### 1.5 chiaki-ng 手动远程连接的实际可用性（社区反馈）

- 能用。#286 里有人系统地测了几种组合：外网、关 VPN、开端口映射时成功；外网、开 VPN、手动用内网 IP 添加主机时也成功。见 <https://github.com/chiaki-ng/chiaki-ng/issues/286>。#58 里 PS5 经端口映射可以远程，但当时没映射 9302，唤醒不了。【已核实】
- 常见失败原因：
  1. CGNAT；
  2. 主机 IP 变了，映射失效（文档要求做 DHCP 保留）；
  3. 只映射了唤醒端口：表现为「唤醒成功，串流失败」，#470 里主机被唤醒了但连不上，开发者判断是 CGNAT；
  4. 旁路由造成回程不对称（见 4.2）；
  5. 家庭公网 IP 变化。【已核实】
- PS4 的限制：文档说远程手动连接时 PS4 的发现不可用，只能手动唤醒后再连（`remoteconnection.md:153`）。原版 chiaki #67 的抓包显示，PS4 的发现响应是从**随机源端口**发出的。如果属实，这种响应过不了 NAT，可能就是原因所在。【现象已核实，原因是推测】
- 稳定性数据：没找到可控对比的延迟测量。社区只有零散数字，比如 CSDN 帖子说同城 5G 约 40–90 ms，跨城 100–180 ms，可信度低。

### 1.6 安全

- 端口映射会把 PS5 的 TCP 9295 暴露到公网。#286 的作者说映射后收到大量扫描。会话需要 regist key，但 PS5 服务端的攻击面不可控。建议：只允许白名单源 IP，或者改走 VPN 或中转。【前半句已核实，建议属推测】

---

## 2. PS5 端有没有拒绝非本地客户端的机制

### 2.1 TTL 和子网检查
- 搜索英文和中文社区都没找到 PS5 在会话阶段检查 TTL 或同网段的证据。反证有两条：①普通路由器的端口映射只做 DNAT 不做 SNAT，PS5 看到的是客户端公网 IP，仍能串流；②经 WireGuard 或站点到站点 VPN，客户端处在另一个网段，手动添加主机后也能串流（#286）。【「接受非本地源地址」已核实；「不存在任何 TTL 检查」属推测（没找到证据）】
- 真正「只认本地」的是**发现**。Sony 官方 App 靠广播发现、不能手填 IP，所以路由型（TUN）VPN 下官方 App 找不到主机，chiaki 填 IP 就行；用 OpenVPN TAP 二层桥接后，官方 App 也能用。见 <https://github.com/williampiat3/ImprovingPSRemotePlay>（内容主要写于 2020–2022 年）。Sony 官方页面要求手动配对时主机和电脑在同一网络：<https://www.playstation.com/en-us/support/games/playstation-remote-play-on-pc-and-mac/>。【已核实】
- 我们自研客户端，所以要关心的只是 PS5 服务端的行为，不受官方 App 的限制。

### 2.2 senkusha（RTT 与 MTU 探测）
- 流程（`lib/src/senkusha.c`）：连 UDP 9297 → 10 次 ping 求平均 RTT（:252）→ 入向 MTU 测试，范围 576–1454、每档重试 3 次、超时取 RTT×5 并限制在 5–500 ms（:259-265）→ 出向 MTU 测试（:272）。发包时设置 DF（:153）。【已核实】
- 结果的用途：`launch_spec.mtu = mtu_in`、`launch_spec.rtt = rtt_us/1000`，都告诉了 PS5（`streamconnection.c:1001-1002`）。PS5 会按这个 MTU 切分视频包。【已核实】
- **失败不会中止**：senkusha 失败后回退到 MTU 1454、RTT 1 ms、`dontfrag=false`，继续建流（`session.c:638-646`）。【已核实】
- 真实事故（#88，2023-10 至 12 月）：PS5 固件更新后 senkusha 普遍失败，回退到 1454。有用户的路径最小 MTU 是 1280，结果串流很快冻结；他把默认 MTU 改成 1280 后恢复正常，而且他反映改 PS5 自身的 MTU 不起作用，PS5 照样发 1454 的包。开发者称已在 main 分支修复。见 <https://github.com/chiaki-ng/chiaki-ng/issues/88>。【已核实，「改 PS5 MTU 无效」是单一用户观察】
- 没找到 PS5 因 RTT 过高而拒绝连接的证据。RTT 会通过 launch spec 告诉 PS5，PS5 怎么用它不清楚。【推测，待实测】

### 2.3 MTU 计算对 VPN 和中转的含义
- WireGuard 外层开销为 20（IPv4）+ 8（UDP）+ 32（WG）= 60 字节，IPv6 外层是 80 字节。phantun 文档给出的 WG 开销正是 32 字节：<https://github.com/dndx/phantun>。所以 1454 的 Takion 包要求外层 1514 字节，**在 1500 的链路上，任何 WG 隧道都装不下**。【已核实，由算术推出】
- Tailscale 默认 TUN MTU 是 1280（`tailscale/net/tstun/mtu.go:79` 的 `safeTUNMTU = 1280`，`DefaultTUNMTU()` 不设调试变量时就返回它；版本 v1.104.7，2026-11-12）。【已核实】
- ZeroTier 虚拟网卡默认 MTU 2800，内部自己做分片。移动网络上常有人需要把它降到 1280 左右才稳定（来源 <https://discuss.zerotier.com/t/zerotier-mtu-issue/9504> 等，属社区说法）。【推测】
- 结论：经隧道时，senkusha 必须成功（UDP 9297 要能通），**或者客户端主动强制一个保守的 MTU**（比如 1280–1380），否则会出现分片（丢一片等于丢整包）或黑洞。【推测，依据很强】

### 2.4 其他会让远程会话中断的机制
- 源码注释写着：主机连续 30 秒收不到心跳回复就结束会话（`lib/src/ctrl.c:42`）。手动直连时 ctrl 走 TCP 9295，所以**客户端换网（WiFi 切 4G）会让 TCP 断掉、会话结束**，除非中间有一条能保持内层地址不变的隧道。WireGuard 有内置漫游：双方总是向最近一次认证成功的源地址发包，见 <https://www.wireguard.com/>。【注释和漫游特性已核实；「经 WG 可以在换网时不断流」属推测，待实测】

---

## 3. VPN 方案实测与分析

### 3.1 WireGuard（三层路由）
- 可行。在 chiaki 系客户端里手动按 PS5 的局域网 IP 添加主机后可用（#286）。在 ImprovingPSRemotePlay 的对比里，WireGuard 是 TUN 类 VPN 中串流效果最好的。【已核实】
- 树莓派吞吐实测（同一仓库）：OpenVPN 桥接在 Pi 3b / Pi 4 上分别是 61 / 80 Mb/s，WireGuard 是 80 / 86 Mb/s；Pi Zero 上只有 11 和 25 Mb/s。按 chiaki 默认 1080p 15 Mbps 计算，Pi 3 级别的设备够用。【已核实】
- 坑：
  1. 广播发现过不去，要手动添加主机。手动添加的主机会收到**单播** SRCH（`gui/src/discoverymanager.cpp:322-427`），所以待机或运行状态的判断仍然有效。
  2. 如果 WG 端点不是主路由，要么在路由器上加一条到 WG 网段的静态路由，要么在端点上做 MASQUERADE，否则回程不对称。
  3. MTU，见 2.3。【已核实】

### 3.2 Tailscale（subnet router、DERP、Peer Relay）
- subnet router：家里要有一台常开设备宣告局域网网段，Linux 上要开 IP 转发，**默认做 SNAT**，所以 PS5 看到的是 subnet router 的局域网 IP，回程自然正确。见 <https://tailscale.com/kb/1019/subnets>。【已核实】
- 社区反馈：经 Tailscale 可以串流，前提是在家先按局域网 IP 手动添加主机（#286）。#713（2026-02）有人在 Tailscale 或 PSN 远程下出现爆音，换开发版构建后解决，不是网络本身的问题。【已核实】
- DERP 是走 HTTPS 流（TCP）的中继，见 <https://tailscale.com/blog/how-tailscale-works>。实时 UDP 流走 TCP 会队头阻塞，不适合串流。Tailscale 在 2025-10 推出 UDP Peer Relay（beta），直连失败时会先试 Peer Relay，再退回 DERP。官方说法是 Peer Relay 通常比 DERP 快，但没有给出数值。见 <https://tailscale.com/blog/peer-relays-beta>、<https://tailscale.com/docs/reference/derp-servers>。【已核实】
- 对稳定性的影响：直连时接近 WireGuard；一旦落到 DERP，延迟和抖动明显变差（推测）；MTU 1280 必须注意（已核实）。小黑盒的 ZeroTier 教程有同样的经验：显示 RELAY 时不适合串流。

### 3.3 ZeroTier
- 二层虚拟以太网，可以在家里设备上桥接到局域网，这样广播发现也能过去【推测，没找到 PS5 实测】。#366 用 ZeroTier 加用户态端口转发跑通了。中继走 ZeroTier 的根服务器，延迟受影响。默认 MTU 2800 加内部分片，是额外的变数。

### 3.4 OpenVPN TAP 二层桥接
- ImprovingPSRemotePlay 的主推方案：远程设备直接进入局域网，官方 App 也能发现主机。代价是二层广播也要过隧道，而且 OpenVPN 性能低于 WireGuard。自研客户端可以单播发现和唤醒，**不需要二层**。【已核实】

### 3.5 发现和唤醒能否穿越

| 方案 | 广播发现 | 单播发现（手动主机） | 单播唤醒 | 典型 MTU 风险 |
|---|---|---|---|---|
| 端口映射 | 否 | 是（需映射 9302） | 是（需映射 9302） | 低（1500 链路） |
| WireGuard / Tailscale（L3） | 否 | 是 | 是 | 高（1420 / 1280） |
| ZeroTier 桥接、OpenVPN TAP（L2） | 是 | 是 | 是 | 中（取决于外层） |

上表综合源码（发现和唤醒都是单播 UDP）与 #286、#343 的报告，【已核实】。

---

## 4. 自建中转（VPS）

### 4.1 需要中转的流量
- 必须中转：TCP 9295（整个会话都在）、UDP 9296（主流量，下行是大头）、UDP 9297（建流时的探测）、UDP 9302（发现和唤醒，流量很小）。只有远程注册时才需要 UDP 9295。【已核实】
- 带宽：chiaki 预设码率 360p、540p、720p、1080p 分别为 2、6、10、15 Mbps（`session.c:97-116`）；上行很小。Sony 官方建议上下行至少 5 Mbps，最佳 15 Mbps。【已核实】
- 视频流自带 FEC 单元（`lib/src/frameprocessor.c:69-79`），客户端每 200 ms 向 PS5 报告一次丢包率，PS5 据此调整码率（`lib/src/congestioncontrol.c:5,27-39`）。所以协议本身对轻度丢包有一定容忍度。【已核实】

### 4.2 两种拓扑
- **A. 家里有公网 IPv4**：VPS 上做 DNAT 加 MASQUERADE，把 VPS 的 9295/9296/9297/9302 转发到家里公网 IP，家里路由器再映射到 PS5。用途是绕开客户端到家里之间的烂路由，代价是多一跳 RTT。必须做 SNAT，否则 PS5 的回包会直接回客户端，与 conntrack 不匹配。【推测，属标准 NAT 原理】
- **B. 家里没有公网（CGNAT 或纯 IPv6）**：家里常开设备主动连 VPS 建隧道（WireGuard 加 PersistentKeepalive，或者自研协议），VPS 把流量 DNAT 进隧道，家里设备再转发并 MASQUERADE 给 PS5。这本质上就是「家庭网关」方案，是 CGNAT 下唯一不依赖 PSN 的路。【推测，依据很强】
- **回程不对称的坑**：CSDN 有一例，PS5 的网关指向旁路由后，公网就找不到也唤醒不了主机了；作者改为主路由 DMZ 指向软路由、软路由开放端口后解决（2024-06-03）。见 <https://blog.csdn.net/qq_44999366/article/details/139377025>。【已核实，属个案】

### 4.3 工具对比

| 工具 | UDP 实现 | 对串流的影响 | 依据 |
|---|---|---|---|
| iptables / nftables DNAT | 内核 conntrack | 开销最低、无额外排队；多主机要靠多 IP 或自定义端口 | 一般原理【推测】 |
| WireGuard 内核转发 | UDP 封装 | 低延迟；MTU 要处理好；支持漫游 | wireguard.com【已核实】 |
| realm v2.9.6 | 内置 UDP 中继，`--udp-timeout` 默认 30 s，Linux 上支持 batched-udp | 用户态 L4 中继，目标必须可达；适合 A 拓扑 | realm README【已核实】 |
| socat | `UDP-LISTEN,fork`，每个对端一个进程 | 能用；高码率下用户态开销更大 | 一般经验【推测】 |
| gost v3.3.3 | 支持 UDP 端口转发和隧道 | 本次没逐项核实 | 【未核实】 |
| frp v0.72.0 UDP 代理 | 每个 UDP 代理**同一时间只有一条工作连接**（`server/proxy/udp.go:50`），UDP 包封装后放进这条连接，默认是 TCP（可选 kcp、quic、websocket，`pkg/config/v1/client.go:106`）；frpc 侧按对端地址 `DialUDP`，读超时 30 s（`pkg/proto/udp/udp.go:79-128`） | **UDP over 可靠流**，丢包时会重传、队头阻塞，抖动放大；但 PS5 看到的是 frpc 主机的地址，功能上可用 | 源码【封装已核实，影响属推测】 |
| 自写转发或自研协议 | 可做 FEC、端口跳跃、伪 TCP、多路径、码率感知 | 灵活性最高 | – |

- ps5web（<https://github.com/xiaobai9978/ps5web>，2026-11-13 创建，1 星，可信度低）走的是另一条路：家里的 NAS 用 libchiaki 终结 Remote Play，再用一条 TCP WebSocket 把画面推给浏览器，对 frp 友好。作者自称公网常见延迟 60–120 ms。这说明「**协议终结型家庭网关**」可行，但 TCP 传输的延迟代价不小。【推测】

### 4.4 中转带来的延迟和丢包
- 延迟约等于 RTT(客户端→VPS) + RTT(VPS→家) + 转发处理时间。内核转发处理时间可以忽略，用户态中转在高码率下可能增加抖动。没找到 PS5 串流的可控实测。【推测】
- 丢包：两段链路的丢包会叠加；Takion 自带的 FEC 和 PS5 的码率自适应能吸收一部分。

### 4.5 国内运营商的 UDP QoS 与应对
- 现状（证据质量偏低）：
  - Hysteria 官方文档说，中国用户有时反映运营商会阻断或限速长时间持续的 UDP 连接；端口跳跃只对按端口的限制有效，对整个 UDP 的限制无效，后者文档建议用 Mimic（伪装成 TCP）。见 <https://v2.hysteria.network/docs/advanced/Port-Hopping/>。
  - udp2raw 中文文档称，它可以给 UDP 包加上 TCP 或 ICMP 头，绕过 UDP 屏蔽或 QoS。见 <https://github.com/wangyu-/udp2raw>。
  - 也有社区实测（X 和 CSDN，2026）认为，所谓「限速」不少是 KCP 这类工具不做 pacing、突发冲破令牌桶造成的，并非运营商一刀切限速。【推测】
- 与 Remote Play 的关系：单个五元组上持续数小时、10–15 Mbps 的下行 UDP，正好符合传说中容易被限的「长时间高码率 UDP」特征。PS5 那一段在局域网里，能加手段的只有「家庭网关到 VPS 再到客户端」这一段。【推测】
- 应对手段（按代价从低到高）：
  1. 先测：在晚高峰用 iperf3 UDP 以目标码率连续跑 30 分钟以上，双向，分运营商、分端口；
  2. 降码率，依靠 PS5 的码率自适应；
  3. 中转段做端口跳跃或多端口；
  4. 伪 TCP（phantun v0.8.1 额外开销只有 12 字节，udp2raw，Hysteria Mimic）；
  5. 额外 FEC（UDPspeeder，Reed-Solomon，用带宽换丢包，见 <https://github.com/wangyu-/UDPspeeder>；注意 Takion 已有 FEC，叠加时要算带宽）；
  6. 最后才是 TCP 或 WebSocket 回退（队头阻塞，延迟明显变高）。
  - 这些手段都要求中转段运行我们自己的封装，所以和「家庭网关加 VPS」架构天然契合。【推测】

---

## 5. PSN 远程连接（holepunch）的原理，以及能否自建替代

- 流程（`lib/include/chiaki/remote/holepunch.h:4-23`，`holepunch.c`）：
  1. 客户端用 PSN OAuth 令牌列出设备（`web.np.playstation.com/api/cloudAssistedNavigation/...`）；
  2. 在 sessionManager 上创建 remotePlaySession，并连上 PSN 推送 WebSocket；
  3. 向主机发命令，主机经它自己的 PSN 长连接收到通知；
  4. 双方经 PSN 的 sessionMessage 交换 OFFER、RESULT、ACCEPT，里面带候选地址（STATIC、LOCAL、STUN、DERIVED，`holepunch.c:266-271`）；
  5. 客户端用 STUN（Google STUN 等，`stun.h`）、UPnP 映射，以及针对对称 NAT 的端口猜测（`RANDOM_ALLOCATION_GUESSES_NUMBER 75`，v1.10.0 起可以配置）打通两个 UDP socket；
  6. 控制通道改走 SCE RUDP（代替局域网的 TCP 9295，`rudp.h`），数据通道仍是 Takion。数据是直连的，PSN 只负责信令。【已核实】
- 网络兼容性：不是所有 NAT 都能打通。开发者说完全随机分配端口的 NAT 大约只有四成成功率（#343，2024-06）；家里是对称 NAT 加 CGNAT 时只能走 VPN（#470）。【已核实】
- **能否用自建信令代替 PSN**：主机端只会从 PSN 接收 OFFER（经推送通道），我们没有任何接口给 PS5 下发候选地址或触发它去打洞；PS5 在非 PSN 模式下只在局域网固定端口被动监听。所以自建信令只能用在「客户端 ↔ 家庭网关」之间（相当于自建一个 Tailscale），**PS5 本身没法配合**。【推测，依据很强】
- 补充：PSN 打洞只用 Sony 做信令、数据不经 Sony 转发。用户说的「不用 Sony 转发」是否也排除这种只用信令的方式，需要用户决定（见开放问题）。

---

## 6. PS5 待机唤醒

- 主机设置（Sony 官方）：设置 > 系统 > 节电 > 待机模式中可用的功能，打开「保持连接到互联网」和「允许通过网络开启 PS5」，同时启用 Remote Play。见 <https://www.playstation.com/en-us/support/games/playstation-remote-play-on-pc-and-mac/>。【已核实】
- 唤醒包格式：发往 PS5 的 UDP 9302（PS4 是 987）的单播文本包 `WAKEUP * HTTP/1.1`，带 `user-credential` 和协议版本 `00030010`（`lib/src/discovery.c:76-84,436-504`）。凭据是 regist key 按十六进制解析成的 uint64（`gui/src/discoverymanager.cpp:287`）。不是 WoL 魔术包，也不需要广播。【已核实】
- 送达方式：
  - 有公网 IPv4：映射 UDP 9302 即可。#343 的用户经 4G 外网确认唤醒包能送到。【已核实】
  - 路由型 VPN：直接单播到 PS5 的局域网 IP 就行。
  - CGNAT 且没有 VPN：必须由家里的常开设备代发。**不需要**路由器转发广播。【推测，由「唤醒是单播」推出】
- 时序：v1.10.1 改为在主机唤醒过程中重发唤醒包。ps5web 作者自称发送唤醒后 10–30 秒 9295 才开始监听，冷待机到出画面约 43 秒（单一来源）。设计上应该做到「发唤醒、轮询 SRCH 或 9295、超时重发」。【前者已核实，时间数据属推测】
- 未知点：断电恢复后主机是否自动进入可网络唤醒的待机状态（#569 有断电后连不上的报告，但没有结论）；长时间待机后网络是否还保持。这些需要实测。

---

## 7. 各方案对比（侧重「稳定」）

| 方案 | 适用场景 | 优点 | 缺点 / 风险 | 对稳定性的影响 |
|---|---|---|---|---|
| 局域网直连 | 在家 | 无中间环节；可以广播发现 | 仍受 WiFi 质量影响 | 最好 |
| 公网 IPv4 加端口映射 | 家有公网 IP、单台主机 | 零中间设备、延迟最低 | 9295 暴露在公网；多主机冲突；IP 变化要用 DDNS；客户端换网会断流；senkusha 不通时低 MTU 路径会出问题 | 好，但脆弱点多 |
| WireGuard 到家里常开设备 | 有公网 IP，或者能自建 VPS 汇聚 | 加密；能访问多台主机；单播发现和唤醒都可用；有漫游 | 要处理 MTU；需要常开设备；回程路由要配好 | 好（MTU 处理得当的前提下） |
| Tailscale subnet router | 不想自建 | 配置最省事；默认 SNAT | 落到 DERP 时性能下降；MTU 1280；依赖第三方控制面 | 直连好，中继差 |
| ZeroTier / OpenVPN TAP | 需要二层（官方 App） | 广播能过 | 性能和 MTU 都更差 | 中 |
| VPS 两跳转发（拓扑 A） | 家有公网，但客户端到家路由差 | 能优化线路 | 多一跳；VPS 的带宽和流量费 | 看线路 |
| VPS 加家庭网关隧道（拓扑 B） | CGNAT 或纯 IPv6 | 不依赖 PSN；能加 FEC、端口跳跃、伪 TCP；能支持多主机和唤醒 | 要开发和运维；家里要有常开设备 | **自研可控性最强，是最适合「稳定」目标的主路线** |
| frp UDP | 临时凑合 | 部署简单 | UDP 走可靠流，队头阻塞 | 差（丢包时抖动大） |
| PSN 打洞（只作对照） | – | 不用映射端口 | 依赖 Sony，部分 NAT 不支持 | 用户已排除 |

---

## 8. 对自研设计的直接启示（推测，供大纲参考）

1. 客户端必须支持：手动填 IP 或域名、**按主机设置外部端口**（多主机）、**强制或上限 MTU**、单播发现和唤醒加重试、senkusha 失败时给出明确提示、会话断开后自动重连。
2. 引入「家庭网关 agent」（OpenWrt、NAS、树莓派、迷你主机都可以），负责：
   - 局域网发现和唤醒；
   - 向 VPS 或客户端维持隧道；
   - 对 PS5 做 SNAT 转发，保证回程正确；
   - MSS 和 MTU 钳制；
   - 多台主机复用；
   - IPv6 入口转成 IPv4。
3. 传输层分两段：PS5 ↔ 网关是原生 Remote Play；网关 ↔ VPS ↔ 客户端用自定义线路协议，按需加 FEC、端口跳跃、伪 TCP。进阶路线是网关直接终结 Remote Play 协议（像 ps5web 那样基于 libchiaki），完全掌控传输，但要自己把远端的丢包和带宽反馈给 PS5 的码率控制。
4. 隧道保持内层地址不变，可以扛住客户端换网（PS5 允许约 30 秒没有心跳），这是「稳定」的关键能力之一。

---

## 9. 来源清单
- chiaki-ng 源码与文档：<https://github.com/chiaki-ng/chiaki-ng>（commit 43728af，v1.10.1，2026-11-03），文档 <https://streetpea.github.io/chiaki-ng/setup/remoteconnection/>
- chiaki-ng issue：#242、#391、#323、#343、#366、#286、#88、#713、#428、#58、#470、#342、#569（<https://github.com/chiaki-ng/chiaki-ng/issues/编号>）
- 原版 chiaki：<https://github.com/thestr4ng3r/chiaki/issues/67>、<https://github.com/thestr4ng3r/chiaki/issues/170>
- Sony：<https://www.playstation.com/en-us/support/games/playstation-remote-play-on-pc-and-mac/>
- PXPlay 手册：<https://streamingdv.github.io/pxplay>
- ImprovingPSRemotePlay：<https://github.com/williampiat3/ImprovingPSRemotePlay>
- Tailscale：<https://tailscale.com/blog/how-tailscale-works>、<https://tailscale.com/kb/1019/subnets>、<https://tailscale.com/docs/reference/derp-servers>、<https://tailscale.com/blog/peer-relays-beta>、源码 `net/tstun/mtu.go`
- WireGuard：<https://www.wireguard.com/>
- frp v0.72.0 源码：<https://github.com/fatedier/frp>；realm v2.9.6：<https://github.com/zhboner/realm>
- Hysteria 端口跳跃：<https://v2.hysteria.network/docs/advanced/Port-Hopping/>；udp2raw：<https://github.com/wangyu-/udp2raw>；UDPspeeder：<https://github.com/wangyu-/UDPspeeder>；phantun：<https://github.com/dndx/phantun>
- ps5-tailscale（适用于越狱主机，2026-10）：<https://github.com/holdmysocks/ps5-tailscale>；ps5web：<https://github.com/xiaobai9978/ps5web>
- CSDN 旁路由个案：<https://blog.csdn.net/qq_44999366/article/details/139377025>；gameapp.club：<https://gameapp.club/post/2024-09-30-steamdeck-to-ps5/>
- netgate 论坛：<https://forum.netgate.com/post/1082716>；Reddit r/PS5 端口帖（只看到搜索摘要）
- 注意：chiaki-ng.com 是非官方粉丝站（页面自己声明），本报告不把它当依据。


---

## 附录：本方向的关键论断与待确认问题（复核结果见 V1/V2/V3）

- [D1] (high,已核实) 手动直连（不经 PSN）时客户端固定连 PS5 的 TCP 9295（会话加 ctrl）、UDP 9296（音视频流）、UDP 9297（senkusha）、UDP 9302（发现和唤醒）；UDP 9295 只在注册搜索时用；PS4 用 UDP 987 代替 9302；9303–9319 是客户端本地端口，不用映射
  src: chiaki-ng 源码 lib/src/session.c:29、ctrl.c:32、streamconnection.c:31、senkusha.c:29、regist.c:26、include/chiaki/discovery.h:24-29；docs/setup/remoteconnection.md:97-113
- [D2] (high,已核实) chiaki-ng 到 v1.10.1（2026-11-03）端口仍写死、不支持按主机设置端口；同一公网 IP 后的多台主机无法同时经端口映射远程（#242 被自动关闭，#58 实测 PS4/PS5 互相冲突）
  src: session.c:29/805、streamconnection.c:161；https://github.com/chiaki-ng/chiaki-ng/issues/242 、/issues/58
- [D3] (high,已核实) PS5 不在 IPv6 上监听 Remote Play 请求，远程游玩只支持 IPv4
  src: streetpea 在 #323（2024-05-31）和 #343 的实测结论；remoteconnection.md:5；PXPlay 手册；holepunch.c:96 ENABLE_IPV6 false
- [D4] (high,已核实) PS5 接受非本地源地址的会话：端口映射时看到客户端公网 IP、经路由型 VPN 时看到别的网段，都能串流；需要同网段的只有广播发现
  src: https://github.com/chiaki-ng/chiaki-ng/issues/286 的组合测试；#58；ImprovingPSRemotePlay README
- [D5] (medium,推测) 没找到 PS5 在会话阶段检查 TTL 或子网、从而拒绝远程客户端的证据
  src: 中英文搜索都没有结果，并结合 D4 的反证
- [D6] (high,已核实) senkusha 走 UDP 9297，测 RTT 并在 576–1454 之间带 DF 探测双向 MTU，结果通过 launch_spec 告诉 PS5；失败不会中止，而是回退到 MTU 1454、RTT 1 ms
  src: senkusha.c:153,252-275；session.c:428,618-646；streamconnection.c:1001-1002
- [D7] (high,已核实) senkusha 失败且路径 MTU 低于 1454 时串流会冻结；有用户把默认 MTU 改成 1280 后恢复（2023 年 PS5 固件变更导致 senkusha 普遍失败，后来已修复）
  src: https://github.com/chiaki-ng/chiaki-ng/issues/88
- [D8] (high,已核实) WireGuard 跑在 IPv4 上额外占 60 字节，1500 的链路装不下不分片的 1454 包；Tailscale 默认 TUN MTU 是 1280。经隧道必须确保 senkusha 能通或者强制较小的 MTU
  src: tailscale net/tstun/mtu.go:79,133-151（v1.104.7）；phantun README 给出的 WG 开销 32 字节；由算术推出
- [D9] (high,已核实) 唤醒包是发往 9302（PS4 为 987）的单播 UDP 文本包 'WAKEUP * HTTP/1.1'，带 regist key 生成的 user-credential，不是 WoL 也不是广播，能经端口映射或路由型 VPN 送达；主机需开启「保持连接到互联网」和「允许通过网络开启 PS5」
  src: discovery.c:76-84,436-504；gui/src/discoverymanager.cpp:287；Sony 官方页面 playstation-remote-play-on-pc-and-mac；#343 用户确认外网唤醒能送达
- [D10] (medium,已核实) PSN 打洞的信令完全经 PSN（sessionManager 加推送 WebSocket）交换 OFFER/RESULT/ACCEPT 候选地址，主机只认 PSN 下发的信令
  src: lib/include/chiaki/remote/holepunch.h:4-23；holepunch.c:102-110,254-256,266-271,1549-1697
- [D11] (high,推测) 不用 PSN 时，CGNAT 或纯 IPv6 环境下只能在家里放常开设备（VPN 端点或网关）；自建信令无法让 PS5 配合打洞
  src: 由 D10 和 D3 推出；chiaki-ng 文档 remoteconnection.md:157 也建议 CGNAT 下用家庭 VPN
- [D12] (medium,已核实) frp UDP 代理每个代理同一时间只有一条工作连接，UDP 包封装后放进这条连接，默认是 TCP（可选 kcp/quic/websocket），属于 UDP over 可靠流，丢包时会队头阻塞，不适合实时串流
  src: frp v0.72.0 server/proxy/udp.go:50,110-205；pkg/proto/udp/udp.go:79-128；pkg/config/v1/client.go:106（影响是推测）
- [D13] (medium,已核实) Tailscale subnet router 默认做 SNAT；DERP 是走 HTTPS 流（TCP）的中继，UDP Peer Relay（2025-10 beta）优先级高于 DERP，官方称通常比 DERP 快
  src: https://tailscale.com/kb/1019/subnets ；https://tailscale.com/blog/how-tailscale-works ；https://tailscale.com/docs/reference/best-practices/performance
- [D14] (medium,推测) 主机连续 30 秒收不到 ctrl 心跳回复就结束会话；手动直连时 ctrl 走 TCP 9295，客户端换网会断流，经 WireGuard 这类会漫游、内层地址不变的隧道有望扛住
  src: chiaki-ng lib/src/ctrl.c:42 注释；https://www.wireguard.com/ 的内置漫游（「扛住换网」属推测）
- [D15] (medium,已核实) Takion 视频流自带 FEC 单元，客户端每 200 ms 上报一次丢包、PS5 据此调整码率；chiaki 预设码率 360p/540p/720p/1080p 分别为 2/6/10/15 Mbps
  src: frameprocessor.c:69-79；congestioncontrol.c:5,27-39；session.c:97-116
- [D16] (medium,已核实) 回程不对称（PS5 网关指向旁路由、端口映射却做在主路由上）会导致公网无法发现、唤醒和连接，中转方案必须保证 SNAT 或回程路由正确
  src: https://blog.csdn.net/qq_44999366/article/details/139377025（2024-06-03 个案）；Tailscale 关闭 SNAT 时要求加回程路由
- [D17] (medium,已核实) 家里设备做用户态 TCP 和 UDP 端口转发（ZeroTier 加 CCproxy）后 PS5 能正常串流；只转 TCP 的 netsh portproxy 会出现「提示远程游玩但没有画面」
  src: https://github.com/chiaki-ng/chiaki-ng/issues/366（单一用户报告）
- [D18] (high,推测) 国内运营商对长时间持续 UDP 的阻断或限速只有社区和厂商文档层面的说法（Hysteria、udp2raw 文档），也有实测认为是突发流量冲破令牌桶；没有权威数据，必须在用户线路上做晚高峰实测
  src: https://v2.hysteria.network/docs/advanced/Port-Hopping/ ；udp2raw 中文 README；CSDN/X 的实测文章（2026，可信度低）
- [D19] (low,推测) 远程手动连接时 PS4 的发现不可用，只能手动唤醒；原因可能是 PS4 的发现响应从随机源端口发出，过不了 NAT
  src: remoteconnection.md:153（现象已核实）；thestr4ng3r/chiaki#67 的抓包（原因属推测）
- [D20] (low,推测) 8572、9303、9308 等端口属于 Sony 官方 App 和 PSN 路径（PS5 用 UPnP 自动映射），手动直连不需要；同时开 UPnP 和手动映射在部分路由器上会冲突
  src: Reddit r/PS5 kx8mhy 帖的搜索摘要（无法直接访问）；netgate 论坛；streetpea 在 #343 的建议

## open_questions
- 用户家里的宽带是哪种：有公网 IPv4、只有公网 IPv6，还是 CGNAT？这决定了主路线是端口映射、IPv6 入口转发，还是 VPS 加家庭网关隧道。
- 家里能否有一台常开设备（OpenWrt 路由、NAS、树莓派、迷你主机）？CGNAT 或纯 IPv6 下不用 PSN 时这是必需的，也是唤醒、多主机和 MTU 钳制的落脚点。
- PSN 打洞只用 Sony 做信令、数据直连。用户说的「不使用 Sony 转发」是否也排除这种只用信令的方式（可以作为兜底，或者完全不碰）？需要用户决定。
- 自研客户端改用自定义外部端口（例如 19295 映射到 PS5:9295）时，PS5 会不会因为会话请求头 Host 里的端口不一致而拒绝？需要实测。
- 经 WireGuard（1420）或 Tailscale（1280）时，senkusha 在 v1.10.1 上能否稳定测出正确 MTU？PS5 的流包有没有设 DF、会不会处理 ICMP frag-needed？客户端强制 1280 或 1380 时画面是否正常？需要抓包实测。
- PS5 会不会因为 RTT 高（launch_spec.rtt）而限制码率或分辨率，或者拒绝连接？没找到资料，需要用不同 RTT 实测。
- 客户端换网（WiFi 切 4G）时，经 WireGuard 漫游能否在 30 秒心跳窗口内保住会话，TCP 9295 会不会断？需要实测。
- PS5 长时间待机、断电恢复后，能否保持 DHCP 和网络可唤醒状态；唤醒到 9295 开始监听的实际耗时和重试策略；需要实测。
- 用户实际线路（家庭运营商、常用客户端网络、VPS 所在机房和运营商）在晚高峰对 10–15 Mbps 持续 UDP 的丢包、抖动和限速情况，单端口与多端口、伪 TCP 的对比；需要 iperf3 实测。
- 家里是否有多台 PS5 或 PS4 要同时远程？是否需要支持 PS4？PS4 远程发现不可用，也许会影响范围。
- 架构选择：网关只做包级隧道（原样转发 Takion），还是做协议终结型网关（家里运行 libchiaki、自研传输，需要把远端丢包反馈给 PS5 的码率控制）？前者简单，后者可控性强、开发量大。
- 安全策略：能否接受 TCP 9295 暴露在公网（加源 IP 白名单），还是一律走隧道？
- Reddit 原帖本机无法访问（403），社区实测只看到搜索摘要；如有需要，可请用户提供代理或镜像补全。gost 的 UDP 行为本次没逐项核实。
