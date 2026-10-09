# p2p-friend → p2p-transfer Rust 完整迁移清单（v0.1）

## 固定来源与不兼容决策

- 只读行为参考：[juezhong/p2p-friend @ 78c6b72](https://github.com/juezhong/p2p-friend/commit/78c6b72cd1024211db3cfc91af08b161f3a5b46d)（v0.16.4，2026-10-09）。
- `p2p-sdk`：新 Rust ICE + QUIC 通用网络实现；`p2p-transfer`：全 Rust 文件应用实现。
- **不实现 Go↔Rust 线协议兼容**：不复用旧 P2PF 邀请码、ALPN、旧 control/data frame wire。旧仓库不随迁移改动。相同功能由新协议独立实现。
- 固定 Go commit 防止迁移中目标漂移；以后旧仓库的新变化要单独评审。

## 功能对照与接受标准

| 旧版能力（v0.16.4） | Rust 目标 | 类别/约束 | 追踪 ID |
| --- | --- | --- | --- |
| `put` / `get` 双向上传下载 | 保留，CLI/TUI/GUI 可发起 | 必需 | FT-01 |
| 文件及目录递归传输 | 保留层级、空目录、元信息安全处理 | 必需 | FT-02 |
| `pwd`/`ls`/`cd` 与 `lpwd`/`lls`/`lcd` | 保留等价用户功能 | 必需 | FT-03 |
| 绝对路径、空格、中文、Unicode | 保留，在所有平台测试 | 必需 | FT-04 |
| CLI Tab 补全、路径引用处理 | 保留；TUI/GUI 使用适合各自界面的导航 | 必需 | FT-05 |
| 会话级单任务仲裁 | 同时只执行一个文件/目录传输任务，显示占用方 | 初版兼容行为 | FT-06 |
| 取消但不中断 Session | 主动端/远端可取消并清理资源 | 必需 | FT-07 |
| 发送/接收 SHA-256 | 校验一致且失败不提交完成文件 | 必需 | FT-08 |
| `.part` 临时文件 + 成功后提交 | 无损恢复/清理策略需明确，按旧版先清理失败文件 | 必需 | FT-09 |
| 传输中控制面仍可用 | status/RPC/目录请求可响应 | 必需 | FT-10 |
| control-only QUIC + 4 条 data-only QUIC | **最终能力为 1 主控制 + 最多 4 数据连接**；Rust 初期可单连接多 Stream 验证 | 最终必须对照 | FT-11 |
| data lane 重建、ACK 缺口重传 | 不把 QUIC write-success 当作对端接收确认 | 必需 | FT-12 |
| 有界队列/重排/缓存 | 内存上限不随文件大小线性增长 | 必需 | FT-13 |
| 平台与链路自适应 lane/chunk/window | 保留可观测参数和行为，重新基准调优 | 必需 | FT-14 |
| `status` 链路、候选、映射、连接数量 | 统一调用 SDK Diagnostics；应用叠加任务信息 | 必需 | FT-15 |
| INVITE / REPLY 双向手动配对 | SDK 新格式，用户体验等价 | 必需 | FT-16 |
| 可选纯信令 server | SDK 支持后接入，不允许业务中继 | 新增 | FT-17 |
| 命令行 CLI | Rust clap + 交互指令 + 非交互命令 | 必需 | UI-01 |
| 交互 TUI | 独立完整终端界面 | 新增且强制 | UI-02 |
| 原生桌面 GUI | 独立完整桌面界面 | 新增且强制 | UI-03 |

Go 版当前只有交互式 CLI，不将 TUI/GUI 误标记为“旧版已有”。

## 参考源码映射

| Go 文件 | 新版 Rust 所属 |
| --- | --- |
| `network.go`、`portmap.go`、`quic_signal.go`、`quic_connect.go`、`quic_types.go`、`quic_stripe.go`、`connection_status.go` | **p2p-sdk**；ICE/身份/QUIC/诊断 |
| `session.go`、`protocol.go`、`types.go` | Transfer 应用会话、RPC、文件传输协议；仅通用连接身份/生命周期放入 SDK |
| `transfer.go`、`resilient_data.go`、`data.go`、`transfer_lease.go` | Transfer 流水线、累计 ACK、四数据通道调度、文件任务仲裁 |
| `fs.go` | Transfer 文件系统与路径安全 |
| `lineedit.go`、`main.go`、`console_ready_unix.go`、`terminal_*.go` | 新 CLI + TUI + GUI（共享 application core） |
| `main_test.go`、`perf_2gib_test.go`、`docs/releases/v0.16.4.md` | 功能、故障回归、跨平台性能对照 |

特别关注旧版 v0.16.4 修复：发生 data lane 故障时必须 **先取消可能阻塞的 worker，再等待任务退出**；接收端避免多余的 1 MiB 块整体复制。新的 Rust 实现必须用故障注入测试和 profile 验证类似问题不会再次出现。

## 变更与验收策略

- 追踪 ID 与 `docs/TEST_PLAN.md` 的测试用例保持映射关系。
- 实现阶段用 issue/PR 关联对应 ID；代码合并前说明新增/变更/退化的旧功能。
- 默认不能删掉旧功能以加速开发。若安全需求改变默认行为，应提供明确可配置策略与迁移说明。
- 旧版吞吐仅作可比较的基线，必须对照相同机器、同网络、同文件内容、重复测试条件。
