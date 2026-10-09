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


## 已创建的功能 PR（2026-10-09）

- [Transfer 开发 PR #2 — M0 Workspace、任务状态与四项测试](https://github.com/juezhong/p2p-transfer/pull/2)，**Draft，未合并**。
- 包含 `transfer-core`、`transfer-cli`、`transfer-tui`、`transfer-gui` Cargo crates。后三者**只是明确标示未实现的占位入口**，不是实际 CLI/TUI/GUI。
- Task 状态机包含开始、写入确认、校验阶段、完成、取消与失败；不具备真实文件 I/O、SHA-256、SDK 连接或上传下载。
- **未运行本地 Rust 编译/测试**：当前环境没有 rustc/cargo；下一 Agent 应核对 PR CI 并修正 fmt/clippy/test。
- 可并行的下一项：路径授权安全模型及文件协议编解码测试，避免在 Transfer 内重复实现 SDK 的网络模块。


## CI 故障处理记录（2026-10-09）

- [功能 PR #2](https://github.com/juezhong/p2p-transfer/pull/2) 初始 CI 因 `transfer-core/src/task.rs` 的 `cargo fmt --check` 失败。
- 已在同一功能分支修正格式；[最新成功的 GitHub Actions](https://github.com/juezhong/p2p-transfer/actions/runs/37899185667) 完成 fmt、clippy 与 cargo test。
- 仅验证 M0 Workspace/任务状态机；**文件传输、SDK 连接与 CLI/TUI/GUI 仍未实现**。切勿把绿色 CI 当成全部业务功能完成。


## 进展（2026-10-09）：M0 应用控制协议基础

- 已合并 M0 Workspace：[PR #2](https://github.com/juezhong/p2p-transfer/pull/2)，main squash commit `99ecfe0`。
- 开发中：[PR #3](https://github.com/juezhong/p2p-transfer/pull/3)：带版本、类型、请求 ID、1 MiB payload 上限的 Rust 控制帧编解码与单元测试。**不兼容旧 Go 线协议。**
- 尚无真实的 RPC、文件 I/O、身份认证、上传/下载；CLI/TUI/GUI 仍为占位。
- 下一步：目录访问权限、路径越界防护和文件传输 RPC 语义测试；在 SDK 完成安全会话后集成实际传输。


## 已完成并合并（2026-10-09）

- M0 Workspace：[PR #2](https://github.com/juezhong/p2p-transfer/pull/2) 已合并；提交 `99ecfe0`。已有 `transfer-core` 任务状态机、三个仅占位的 CLI/TUI/GUI 可执行入口及相应 CI。
- M0 协议基础：[PR #3](https://github.com/juezhong/p2p-transfer/pull/3) 已合并；提交 `2702a70`。Rust 新线协议的固定头、版本、FrameKind、request_id、1 MiB payload 上限与边界测试。最新 PR Actions fmt/Clippy/test 已通过；这只是 Frame codec，不是可用的 RPC 或文件传输。
- 仍**没有**实际网络连接、文件系统 RPC、上传/下载、.part、SHA-256、真正 TUI/GUI。不能按“可用版本”发布。
- 下一项：`transfer-core` 文件路径授权/越界阻断（FT-04/FT-09/TC-022）和目录操作；待 SDK 的真正 QUIC Session API 可用后接入完整文件收发。

## M2 最小文件访问授权准备（2026-10-09，开发 PR）

- 在 `transfer-core` 添加 `AuthorizedRoot`：通过显式共享根目录授权读取，拒绝绝对路径、`..`、符号链接和不存在的文件；支持 Unicode 路径和目录列表；有单元测试。
- **安全边界**：当前只是前置路径检查，不能抵御其他本地进程在检查与打开之间修改符号链接的 TOCTOU 竞态；**尚不提供安全文件写入 API**，在完善原子 descriptor-relative open 之前不能作为生产级远端读写沙箱。
- SDK 的 ICE/Quinn 连接和 Transfer 的实际 PUT/GET/GUI 均未在本 PR 实现；Transfer 面向用户的五架构发布要等到业务和 SDK 联网流程可用后再启用。
- 下一步：平台安全的 openat/handle-relative 打开，明确共享根目录和远端授权，目录 RPC + 文件读写 + SHA-256/.part 可靠提交，随后连接 SDK。
