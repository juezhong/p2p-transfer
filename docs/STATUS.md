# 开发状态交接 — p2p-transfer

> 通过本仓库 `main` 和最新打开的功能 PR 核实状态。这里是任务索引，不是测试结果。

## 已确认的设计

- Go `p2p-friend@78c6b72`（v0.16.4）只是只读参考；**Rust 新版本不兼容旧 Go 线协议**。
- Transfer 使用 SDK 的 ICE/信令/QUIC/身份/网络诊断，不能重复网络实现。
- 必须迁移 Go 所有可观察到的文件功能及可靠性行为，最后提供完整 CLI/TUI/GUI。
- 控制面和数据面分离；最终具备 1 条 control-only + 最多 4 条 data-only QUIC；文件 ACK/重传由 Transfer 负责。

## 两份长期设计记录 PR（用户要求保留、不合并）

- [Transfer PR #1：迁移架构、路线图、26 项手工测试](https://github.com/juezhong/p2p-transfer/pull/1)
- [SDK PR #2：Transfer 对 SDK 的功能契约](https://github.com/juezhong/p2p-sdk/pull/2)

## 进度（实际源码/CI 优先）

- M0：Rust Workspace、文件业务基础类型、单元测试与 CI —— 开发启动阶段。
- M1：等待 SDK 的真实 ICE/Quinn 原型 —— **未完成**。
- M2：文件/目录 PUT/GET —— **未完成**。
- M3：SHA-256/.part/ACK/重传/取消可靠性 —— **未完成**。
- M4：四数据 QUIC 与故障恢复 —— **未完成**。
- M5：CLI/TUI/GUI 三界面 —— **未完成**。
- M6：跨平台测试、2 GiB 基准与发行 —— **未完成**。

## 下一步

1. 查看最新开发 PR，核实 M0 已实现的具体代码和测试结果。
2. 在 SDK 达到可用前，先完成 `transfer-core` 状态机、路径权限与文件协议的测试驱动开发。
3. SDK 真实 Session API 完成后开展 TC-001/TC-003 双进程传输。
