# 开发代理与贡献约束

## 当前项目状态

`p2p-transfer` 正处于 **Rust 重构设计阶段**。除原有 LICENSE 和开发文档外，不可宣称文件传输、GUI、TUI 或 ICE/QUIC 已实现。

## 确定的技术决策

- **全部新代码使用 Rust**，Rust stable、Cargo Workspace、Tokio；不将 Go 代码作为新实现或必需 runtime。
- `p2p-sdk` 是 NAT/ICE/STUN/IPv4/IPv6/PCP/NAT-PMP/UPnP、QUIC、身份与诊断的唯一实现；Transfer 不重复开发这些模块。
- 参考 `juezhong/p2p-friend@78c6b72cd1024211db3cfc91af08b161f3a5b46d`（Go v0.16.4），但旧仓库只读参考。
- **不支持与旧 Go 版本线协议互通**。Rust 全新信令/文件线协议必须有版本和能力协商，不许静默混用旧 ALPN、P2PF 连接码。
- 保留旧版可见文件功能/可靠性：PUT/GET、目录操作、SHA-256、.part、有界缓存、单传输仲裁、取消、应用 ACK 与恢复、最多 4 条独立 data-only QUIC 能力。
- 三种 UI 是最终发布的硬性要求：**CLI / TUI / GUI**，一个 `transfer-core` 实现业务，三个前端不得各自实现协议。
- 不夸大 Rust 迁移性能；以同机同网络可重复基准测试与真实双机测试报告为准。

## 工作顺序

遵循 `docs/ROADMAP.md` 的 M0-M6 与 `docs/TEST_PLAN.md` 的用例。发现需求不明时将决策写入文档/ADR；未测试的行为不得声称通过。

每个实现 PR：
1. 标明 `FT-xx` / `UI-xx` 需求和 `TC-xxx` 用例。
2. 同时更新协议、安全、测试与 README 的受影响章节。
3. 运行 Rust fmt、clippy、cargo test 与平台相关测试；只报告真实执行结果。
4. 明确取消、超时、内存上限、错误分类及文件提交语义。
5. 不修改 `p2p-friend` 参考仓库，不复制 Go 的历史 bug，也不静默降低旧版可靠性。

## 安全和许可

远端文件系统按最小权限授权，防越权、符号链接逃逸、目录穿越、覆盖冲突与协议滥用。TLS/认证归 SDK，不能在应用层绕过。若涉及原代码表达式迁移或许可证改动先审查版权和兼容性。

## 文档入口

`docs/MIGRATION.md`、`docs/ARCHITECTURE.md`、`docs/PROTOCOL.md`、`docs/SECURITY.md`、`docs/ROADMAP.md`、`docs/TEST_PLAN.md`、`docs/TESTING.md`。
