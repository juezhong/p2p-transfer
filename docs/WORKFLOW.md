# p2p-transfer 标准 Agent 工作流程

## 每次新会话要做

1. 检查本仓库与 `p2p-sdk` 的 `main`、打开的 PR 和最近提交；**不可仅根据对话中的旧结论继续开发**。
2. 读取根目录 `AGENTS.md`、`docs/STATUS.md`；再读取设计记录 [Transfer PR #1](https://github.com/juezhong/p2p-transfer/pull/1) 的迁移、架构、协议、安全、路线图、测试计划文件，以及 [SDK PR #2](https://github.com/juezhong/p2p-sdk/pull/2) 的 SDK 接口契约。两份 PR 只作记录，不自动合并。
3. 对照 `p2p-friend@78c6b72`（只读），按 `docs/MIGRATION.md` 的 FT/UI 需求追踪 ID 找出下一项未实现的功能。
4. SDK 代码实际可用之前，可独立实现文件系统、传输任务状态机、协议编解码和 UI 事件基础；**不重复造 NAT/QUIC**。
5. 在 `feat/` 分支进行小步修改，附上单元/集成测试，更新 `docs/STATUS.md` 的 Gate/测试证据/后续任务，开中文 PR。
6. 不能以 CI 环回测试替代实际 IPv4/IPv6/NAT 手测；手工测试按 `docs/TEST_PLAN.md` 的 TC-001~026 逐项记录并将结果归档。

## 一次任务的完成条件

- 说明变更对应的 M0-M6 里程碑、FT/UI ID 和 TC 测试编号。
- 本地或 CI 的 `cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` 可复现；没有执行的只能标 `NOT TESTED`。
- 网络功能必须使用经过身份认证的 SDK；传输取消不能带走会话；文件完成必须校验和成功提交。
- 保持 `README.md` 的实现状态描述准确，不把尚未开发的 CLI/TUI/GUI 写成已完成。

## 开发恢复提示词

> 检查 juezhong/p2p-transfer 和 juezhong/p2p-sdk 的 main、打开的 PR，阅读两个仓库的 AGENTS.md、docs/WORKFLOW.md、docs/STATUS.md。再阅读 p2p-transfer PR #1（设计记录/手工测试）和 p2p-sdk PR #2（接口约束），不要合并记录 PR。锁定 Go 旧版只读基线，继续 STATUS.md 的首个未完成任务，创建功能分支、编写测试并提交中文开发 PR，更新状态文档。不要将未完成代码声称为可用程序。
