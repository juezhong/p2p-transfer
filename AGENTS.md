# 新会话开发代理入口

**开始开发前必须先读**：本文件、`docs/WORKFLOW.md`、`docs/STATUS.md`；随后阅读 [Transfer PR #1](https://github.com/juezhong/p2p-transfer/pull/1) 的设计文档（记录 PR，**不要自动合并**），特别是其中的 `docs/MIGRATION.md`、`docs/ARCHITECTURE.md`、`docs/PROTOCOL.md`、`docs/ROADMAP.md`、`docs/TEST_PLAN.md`、`docs/TESTING.md`、`AGENTS.md`。同时查阅 [SDK PR #2](https://github.com/juezhong/p2p-sdk/pull/2) 的接口契约以及 SDK 主分支的 `AGENTS.md`。

## 不可违反的约束

- **全新 Rust** 实现；Go `p2p-friend` 仅作只读行为参考，锁定 [main@78c6b72](https://github.com/juezhong/p2p-friend/commit/78c6b72cd1024211db3cfc91af08b161f3a5b46d)（v0.16.4）。
- 不兼容旧 Go 线协议、不采用旧 Go 服务作为依赖，不修改 `p2p-friend`。
- 所有 STUN/ICE/NAT/IPv4/IPv6/Quinn/信令/安全认证/诊断由 Rust `p2p-sdk` 实现；Transfer 只负责文件系统、协议、任务、数据块、ACK/重传、SHA-256、权限和 UI。
- 功能不能退化：PUT/GET、目录浏览、递归传输、Unicode 路径、取消、单任务仲裁、.part、端到端校验、有界内存、数据链路恢复，最终恢复 1 条 control-only + 最多 4 条 data-only QUIC 的能力。
- 最终正式发布 **CLI/TUI/GUI 三种实际可用界面**，必须共享一个 `transfer-core`。
- 远端文件访问默认最小权限授权；不允许将链路认证等同于全盘访问授权。
- 只在独立 `feat/` 分支开发，用中文 PR 记录测试和已知限制；不自动合并两份记录用 PR。
- 对代码是否已实现/测试通过，要通过源码、CI 和手动结果核实；不得根据规划文档推断。

## 恢复开发

1. 读取 `docs/STATUS.md` 的下一步。
2. 查阅最新代码 PR 是否存在未合并工作，再选择第一项未完成任务。
3. 编写自动测试并根据 `docs/TEST_PLAN.md` 标注需要用户手测的 TC-xxx。
4. 开中文 PR，更新状态交接记录。
