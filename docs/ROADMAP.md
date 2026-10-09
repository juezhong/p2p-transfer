# Rust 完整迁移路线与 Gate（草案 v0.1）

以 `p2p-friend` Go v0.16.4 `main@78c6b72` 为功能基线；Rust 新版不兼容 Go 线协议。所有阶段需满足 `docs/TEST_PLAN.md` 和 `docs/TESTING.md` 的对应 Gate；**未通过的功能不得标记为完成**。

| 里程碑 | 仓库 | 交付 | 验收门槛 |
| --- | --- | --- | --- |
| M0 | Transfer | 功能映射、路径安全决策、协议约束、测试样本 | FT/UI 映射齐全，建立可复现 Go 对照 |
| M1 | SDK | Rust ICE + Quinn、手动配对、加密 Session | 两台机器安全直连与 Stream echo；无 relay |
| M2 | Transfer | 文件 RPC、文件/目录 PUT/GET | 两个 Rust 客户端能传输并校验文件/目录 |
| M3 | Transfer | SHA-256、.part、累计 ACK、重传、取消、会话任务仲裁 | 故障注入、校验错误与取消回归全部通过 |
| M4 | SDK + Transfer | control-only 主 QUIC + 最多 4 数据 QUIC，lane repair，自适应与诊断 | lane 故障不断主控制，数据端口可达性验证，实际恢复 |
| M5 | Transfer | CLI、TUI、GUI 完整功能与共享 core | 三个实际 UI 都能完成传输、目录浏览、状态/取消 |
| M6 | 全部 | Linux/Windows/macOS 发布、2 GiB 性能对照、手测报告 | 平台、安全、完整性和性能报告，明确未达项目 |

## 依赖关系

- SDK M1 是 Transfer M2 的集成前提，但 Transfer 的 `filesystem`/`protocol`/`transfer-core`、UI 原型、测试夹具可以并行建设。
- SDK M4 的多数据 QUIC 能力不能仅在 Transfer 内通过重复实现 NAT/QUIC 绕过。
- 强制三界面交付，但可以按阶段先 CLI、再 TUI、最后 GUI；正式版本三界面缺一不可。

## 阶段完成定义（DoD）

每个里程碑的 PR 必须记录：功能追踪 ID、自动化测试结果、受影响手测编号、系统环境/网络情况、内存/性能基线、回退方式及文档变更。跨网络与跨平台测试无法进行时，显式标注 `Not Tested`。

## 参考规则

不要自动跟随旧 `p2p-friend/main` 的后续变化；必须用单独的差异评审调整锁定的基线。Go 仅用于只读回归对照。
