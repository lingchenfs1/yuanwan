# 远玩（yuanwan）

自研的 PS5 远程游玩软件，目标是**稳定第一**。只用于三种场景：局域网、公网直连、经自建服务器中转；**不使用 Sony/PSN 的转发、中继或打洞**。

> 与 Sony Interactive Entertainment 没有任何关系。PlayStation、PS5、DualSense 是 Sony 的商标。

## 现状

- **M0（实验）**：已完成能做的部分，包括 PS5 码率与丢包上报的关系、IDR 响应、会话冷却、唤醒、MTU 等。
- **M1（家庭网关原型，macOS）**：已能以 LaunchDaemon 长期持有 PS5 会话，带唤醒、看门狗、退避重试、持久暂停，以及本地控制口。
- **M2（自研传输协议 YWP + 桌面客户端）**：进行中。**目前还没有可以拿来玩的客户端。**

## 架构

「家庭网关终结型」：

```
PS5 ──Takion（局域网）── 家庭网关 yw-gateway ──YWP（局域网 / 公网 / 中转）── 客户端
```

- 网关放在 PS5 那一侧，链接 [chiaki-ng](https://github.com/chiaki-ng/chiaki-ng) 的 C 库 `libchiaki`（v1.10.1 加我们的补丁），在局域网里终结 PS5 的 Takion 会话。心跳、确认、重传这些对时间敏感的事，都在局域网里完成。
- 网关和客户端之间用自研协议 **YWP**，规格见 [`docs/protocol/ywp.md`](docs/protocol/ywp.md)：Noise_IKpsk2 加密、每帧 FEC、按截止期 NACK、输入冗余、断线保持与重连。
- 客户端不链接、不移植 chiaki 的代码。

## 目录

| 路径 | 内容 |
|---|---|
| `crates/yw-chiaki-sys/` | libchiaki 的 C 外壳和 Rust FFI |
| `crates/yw-gateway/` | 家庭网关（只用标准库）：监督状态机、看门狗、控制口、JSONL 日志 |
| `third_party/patches/` | 对 chiaki-ng v1.10.1 的补丁。chiaki-ng 源码不在仓库里，由 `tools/fetch-chiaki.sh` 拉取并打补丁 |
| `tools/` | 构建脚本、实验工具（`m0-probe`、链路测量 `yw-probe`）、部署脚本 |
| `deploy/macos/` | 网关的 LaunchDaemon 模板和安装脚本 |
| `docs/protocol/` | YWP 协议规格 |
| `docs/research/` | 前期调研（开源方案、协议要点、中转与传输设计） |

## 构建（macOS，Apple Silicon）

```bash
tools/fetch-chiaki.sh        # 拉取 chiaki-ng v1.10.1 并打补丁
tools/build-deps-macos.sh    # 编静态依赖（OpenSSL、libevent 等）
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release -p yw-gateway
```

需要 Homebrew 的 Rust、cmake、ninja、pkg-config、protobuf。`build.rs` 会调用 `tools/build-shim-macos.sh` 编 libchiaki 外壳，编之前会校验 chiaki 源码和补丁一致。

## 部署网关（macOS）

1. 网关必须以 **LaunchDaemon** 运行。用户级 LaunchAgent 会被 macOS 15 的本地网络隐私拦截，连不上 PS5（见 Apple TN3179）。
2. 在网关机上准备：
   - PS5 凭据 `~/.config/yw/ps5.cred`：可以用 `tools/import-consoleremote.py` 从已注册过远程游玩的客户端导出文件转换，只取远程游玩凭据，不读 PSN 令牌；
   - `~/yw-gw/gateway.conf`：一行 `host=<PS5 地址>`。
3. 在开发机上：把 `tools/local.env.example` 复制成 `tools/local.env`，填好网关机登录信息，然后运行 `tools/deploy-gateway.sh`。
4. 在网关机上执行一次 `bash ~/yw-gw/deploy/install.sh`（需要 sudo）。
5. 控制口：`echo status | nc -U ~/yw-gw/control.sock`。命令有 status、idr、snap、record、inject、sleep、pause、resume、restart、quit。

**凭据（regist_key、morning、PSN 账号、PIN）绝不进仓库、不进日志。**

## 许可证

[AGPL-3.0-only](LICENSE)。chiaki-ng 本身是 AGPL-3.0-only（附 OpenSSL 例外），这里只以补丁形式引用，不包含它的源码。

开发过程中的环境记录（测试场地的网络、实验日志、进度笔记）不在这个仓库里。
