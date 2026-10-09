# Rust 交互式 CLI 迁移阶段（Go 行为基线）

对应只读参考：`juezhong/p2p-friend` main @ `78c6b72`。此文档明确新 Rust UI 的真实进展，而非宣布最终 Go 功能对等。

## 当前交互方式

运行 `p2p-transfer`（**不带启动参数**）：

1. 显示「创建连接 / 加入连接 / 退出」菜单。
2. 使用 SDK `local_network` 枚举本机可用 IP 并从 OS 路由选择一个绑定地址、自动分配本地 UDP 端口；可通过 DNS 选择两个公共 STUN 服务器探测同一个 UDP Socket，失败自动只做 LAN。（单网卡单地址仍不是 Go 的完整 IPv4/IPv6 多接口并行）。
3. 默认共享根目录取运行程序时的当前目录，**必须输入 yes 明确授权**；不再向用户索要启动参数路径，但出于安全拒绝访问共享根以外的路径，区别于旧 Go 的全文件系统访问行为。
4. 交换手动 INVITE/REPLY，双方必须通过可信独立渠道核对六位码；自动 ICE 提名/双向 mTLS/独立 Control/Data QUIC。
5. 进入持续的远程文件命令 shell。使用 `pwd/ls/cd`、`lpwd/lls/lcd`、`put/get`、`status`、`cancel` 和 `quit`，可以重复传文件，目录 RPC 保持 Control QUIC，文件只走 Data QUIC。
6. `P2P_TRANSFER_STUN=off` 仅用于 LAN/offline 诊断测试，用户正常启动无需配置。CLI 遗留的 send/receive 参数仅给 CI 兼容测试使用，不属于正式使用说明。

## 尚未达到的功能 / 安全门槛

- 仅支持文件和目录枚举，**未支持目录递归 PUT/GET、单一 Transfer Session 完整 lease 仲裁、文件分块滑动窗口与中断重传、可靠远程取消/文件恢复、Tab 补全以及覆盖策略切换**。目前 `cancel` 只会中止本端主动发送任务，不等同于完整的协议级取消。
- 自动网络接口只选一个推荐 IP；多网卡同时竞速、IPv4+IPv6 双栈路径共同交换及 NAT 直接贯通仍欠缺。主动 STUN 会联系第三方服务器，依赖网络可用；没有 TURN/业务数据 Relay。
- 文件系统被沙箱根限制，不能像 Go 一样访问任意绝对路径；这个差异是为了默认远端最小授权，由用户决定共享根，后续可增加交互式授权根切换。
- TUI/GUI 暂缓，只做最终稳定 CLI 后再发布。即使五平台自动化两进程成功，也**不代表真实跨公网 NAT 穿透成功**。
- 用户要求所有 Go 的核心功能完成后再提供正式版；本 PR 不打 Release tag，也不能称为完整或稳定发布版。

## 自动测试

`crates/transfer-cli/tests/interactive_loopback.rs` 启动两个真实无参数 CLI 进程，模拟菜单、显式根授权、手动 INVITE/REPLY、双方校验码、双 QUIC PUT、远端列表、双向 GET 后验证磁盘数据。五平台本地构建与测试为本阶段的自动 Gate。真实跨 NAT 最终由用户在两个不同网络的设备上测试。

