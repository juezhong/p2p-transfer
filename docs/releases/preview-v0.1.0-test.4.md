# p2p-transfer v0.1.0-test.4 — SDK #71 网络连接 + 实际文件收发现场测试版

**这是开发阶段 Pre-release，不是稳定正式版。** 从本次版本开始，**正式交互式 CLI** 的建连流程使用已合并的新版 [p2p-sdk #71](https://github.com/juezhong/p2p-sdk/pull/71)；不只是测试库代码使用新 SDK。本版本用于两台真实设备的 LAN、NAT/CGNAT、IPv6 防火墙和跨平台现场验证。

## 相比 v0.1.0-test.3 的关键变化

- Transfer 依赖固定在 SDK `24a9b5521f682eaf70eb9453780ccb16853e20c2`，包括双向 QUIC、JOIN 提前被动接纳、动态 peer-reflexive 来源的 ICE nominated 复验、双方一致的 Creator Control 选路，以及 mTLS 证书 PIN/会话 HMAC。
- **正式 CLI** 通过 `begin_creator`/`begin_joiner`、人工核对六位配对码、`connect_transport` 建立经过完整认证的 Control；不再自行实现旧的低层 gather→ICE→Quinn 建链流程。
- Transfer 负责最多 **四条由 SDK 单独认证并托管修复的 Data QUIC**；业务文件数据走 Data，目录/RPC/ACK 走 Control。应用的 `P2PD`+request ID 前导帧与磁盘 SHA-256 完整性校验保持有效。
- 保留 Go 式无参数菜单、INVITE/REPLY 离线交换、明确的共享目录授权、PUT/GET、递归目录、Unicode/中文路径与 Tab 补全、单批租约与本地取消能力。
- 采用 Transfer [#30](https://github.com/juezhong/p2p-transfer/pull/30)、[#31](https://github.com/juezhong/p2p-transfer/pull/31)、[#32](https://github.com/juezhong/p2p-transfer/pull/32)、[#33](https://github.com/juezhong/p2p-transfer/pull/33) 的已合并代码。

## 自动验证与构建

- 上述 CLI 迁移最终提交已通过 [Rust Checks](https://github.com/juezhong/p2p-transfer/actions/runs/38072373041)、[五目标 Debug](https://github.com/juezhong/p2p-transfer/actions/runs/38072373033)、[七目标 Preview](https://github.com/juezhong/p2p-transfer/actions/runs/38072373086)，含本机 **两个真实 p2p-transfer 进程** 配对、空闲 38s 后双向 PUT/GET、递归/租约和 SHA-256 测试。
- 本 Release 工作流会**重新构建并运行七目标完整测试**，只有全部成功才附上七个可执行文件与 `SHA256SUMS.txt`；发布标签指向这次构建所用的准确 commit。
- 附件：`p2p-transfer-windows-x86_64.exe`、`p2p-transfer-macos-x86_64`、`p2p-transfer-macos-aarch64`、`p2p-transfer-linux-x86_64`、`p2p-transfer-linux-aarch64`，以及对应的 `-musl` 两个静态 Linux 构建。RK3568/ARM64 Linux 可优先尝试 `p2p-transfer-linux-aarch64-musl`。

## 如何开展双机实测

1. 在两台设备的**独立测试共享目录**中放入适配架构的程序，macOS/Linux 如需先执行 `chmod +x ./p2p-transfer-*`。程序**无参数**启动；`--version` 应输出 `0.1.0-test.4`。
2. A 选 **1 创建**、B 选 **2 加入**。双方确认对本机当前目录授权；私下发送 A 的 INVITE 和 B 的 REPLY，使用**独立可信渠道**核对显示的相同六位码并分别输入 `yes`。**禁止贴到公开日志里**（包含网络候选地址及临时认证信息）。
3. 两端进入交互 Shell 后，执行 `status`、`ls`、`put "test.txt" "received.txt"`、`get "received.txt" "back.txt"`；查看接收端文件字节数及 SHA-256。建议再测 UTF-8/中文文件名、空文件、带空格目录、递归文件夹、静置 60–120s 后发送。
4. 先同一 LAN，再不同路由器、运营商及 IPv6；离线 LAN 可设置 `P2P_TRANSFER_STUN=off`，**跨公网不要设置**。记录两个设备的 OS/架构、网络类型、`status`、是否成功、耗时、脱敏错误和文件 SHA-256；不要公开完整邀请码、外网 IP 或私人文件。
5. 下载二进制后可用 `SHA256SUMS.txt` 验证附件未损坏。未签名开发预览，操作系统可能出现安全提示，请确认来源是本仓库 Release。

更多步骤见 [双机预览测试说明](https://github.com/juezhong/p2p-transfer/blob/main/docs/MANUAL_TEST_PREVIEW.md)。

## 已知限制

- GitHub Actions 的本机/本 runner 多进程测试并不能证明真实 CGNAT/对称 NAT、IPv6 stateful firewall、复杂多网卡或 24/72 小时长运行稳定性；**无 TURN/Relay，无法直连时会失败**。
- 文件传输未达到 Go `p2p-friend v0.16.4` 的全部可靠性/性能对等：远端可靠 CANCEL、未确认分块的断线自动续传、四 Data 链路的故障情况下分条重发及 2 GiB 基准仍需逐项验收。
- 本版本只供现场测试，**不宣称 SDK 或 Transfer 已达到生产稳定级别**。请将经过脱敏的测试结果记录在 Transfer [设计/验收跟踪 PR #1](https://github.com/juezhong/p2p-transfer/pull/1)。
