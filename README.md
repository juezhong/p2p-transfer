# p2p-transfer

面向 Linux、Windows、macOS 的 **Rust P2P 文件与目录传输应用**。基于 [p2p-sdk](https://github.com/juezhong/p2p-sdk) 实现 IPv4/IPv6、ICE NAT 穿透、QUIC 加密通信，并提供 **CLI、TUI、GUI 三种完整形式**。

> **当前状态：设计/迁移准备阶段。** 目前没有可用 Rust 可执行程序，所有功能均为待实施目标；不能将本文档当作已发布功能说明。

## 迁移原则

- 参考基线：`p2p-friend` [v0.16.4 main@78c6b72](https://github.com/juezhong/p2p-friend/commit/78c6b72cd1024211db3cfc91af08b161f3a5b46d)（2026-10-09）。
- **只保留功能与可靠性行为**：Rust 全新实现，不要求与旧 Go 版本交换邀请码、建立协议会话或互传文件。
- 旧 `p2p-friend` 仓库只作为参考、故障回归与性能基线，不修改旧仓库。
- P2P 穿透、信令、身份、QUIC、网络诊断全部由 `p2p-sdk` 实现。Transfer 负责文件业务、路径权限、应用级 ACK/重传、文件校验及三种 UI。
- 第一个可发布版本的交付标准是功能验证 + CLI/TUI/GUI 可用 + 跨平台可靠性测试；未经测试不可声称性能优于 Go。

## 文档

- [完整迁移与功能对照](docs/MIGRATION.md)
- [Rust 应用架构与文件传输状态机](docs/ARCHITECTURE.md)
- [新一代文件协议草案](docs/PROTOCOL.md)
- [开发路线与验收 Gate](docs/ROADMAP.md)
- [安全与远程文件访问授权](docs/SECURITY.md)
- [手动测试大纲、逐步用例与测试记录模板](docs/TEST_PLAN.md)
- [自动化测试、性能与故障注入](docs/TESTING.md)
- [仓库开发规范](AGENTS.md)

## 强制发布平台

Linux amd64/arm64、Windows amd64、macOS amd64/arm64；均需评估 CLI、TUI、GUI 交付形式。无图形显示设备的服务器可以只安装 CLI/TUI，但正式桌面发行包必须提供 GUI。

## 许可

沿用仓库现有 `LICENSE`，如涉及从 Go 参考代码迁移受保护表达，需核对源仓库许可证与贡献版权；不擅自转换许可。
