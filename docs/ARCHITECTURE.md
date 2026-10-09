# Rust 应用架构（目标设计 v0.1）

## 分层

```text
p2p-transfer/
  crates/
    transfer-core/        文件、RPC、分块、ACK、恢复、任务状态、业务事件
    transfer-cli/         clap + 兼容旧交互指令 + 非交互模式
    transfer-tui/         Ratatui + Crossterm，完整双栏浏览/任务管理
    transfer-gui/         egui/eframe，完整桌面文件管理与传输
  docs/                   需求、协议、安全、测试、发布指导
  tests/ / benches/

transfer-core -> p2p-sdk -> Tokio / ICE / Quinn / UDP
                 ^ 不反向依赖 transfer-core
```

## 领域模块

- `filesystem`：本地/远端路径规范化、访问授权、列目录、递归枚举、空目录、符号链接策略。
- `protocol`：Rust 新版本的文件 RPC 与帧编码；数据长度上限、版本、错误码。
- `session`：Transfer 业务会话和远端 RPC；不重复管理 ICE 和 QUIC TLS。
- `scheduler`：最大一个传输任务的 lease、lane 与块调度、吞吐状态。
- `pipeline`：有界内存块池/队列/offset reorder、背压和磁盘异步操作。
- `recovery`：应用级累计 ACK、缺口超时、重复块过滤、lane 重建后的窗口重发。
- `event`：统一的任务事件流；CLI/TUI/GUI 只订阅状态，不重复业务实现。

## 控制面与数据面

```text
已认证 p2p-sdk Session
├── Control QUIC（RPC / 元数据 / ACK / 取消 / keepalive）
│   └── Transfer Session State
└── Data Transport
    ├── 首个原型：单 QUIC 的多个 Stream
    └── 最终要求：最多 4 条独立 data-only QUIC（分别经过可靠连通性验证）
        └── Chunk Dispatcher -> Bounded Reorder -> .part -> SHA-256
```

最终要保留 Go v0.16.4 **一条纯控制连接 + 最多四条 data-only QUIC** 的能力。该结构属于应用对 SDK 的连接要求，实际多 QUIC 的认证、建连、UDP 端口/NAT 路径修复交由 SDK；任务和窗口调度归 Transfer。

## 发送状态机

```text
Idle -> AcquireLease -> Negotiate -> Enumerate -> AwaitEntryReady
     -> Sending -> AwaitWrittenAck -> VerifyRemoteResult
     -> Commit/NextEntry -> Complete -> ReleaseLease
                  | cancel/error
                  v
            Cancel -> Cleanup -> ReleaseLease
```

## 接收状态机

```text
Authorize -> ReserveTarget -> OpenPart -> Receive/Reorder
          -> SequentialWrite -> CumulativeWrittenAck
          -> VerifySHA256 -> AtomicCommit -> ReportResult
                     | mismatch/cancel/error
                     v
               Cleanup/Rollback -> ReportError
```

ACK 表示应用协议定义的连续已写入确认，**不自动等同于持久化到物理介质**。最终成功的校验、flush/sync 和 rename 语义需要针对平台设计，特别是 Windows 文件 rename/句柄锁与 Linux/macOS 目录 fsync 差异。严禁不经确认将临时文件作为成功结果暴露。

## 存储安全

- 会话授权应限制远端读取、写入和列目录范围（默认选择受限根目录；需要全盘访问时显式授权）。
- 拒绝 `..` 越权、非法绝对路径、符号链接逃逸与资源耗尽；处理 TOCTOU，能用目录句柄限制访问的系统优先采用此方案。
- 保留中文/空格/合法绝对路径支持，但“路径可解析”不等于“已授权访问”。
- 禁止未知设备自动获得全盘读写能力。
- 取消与出错时删除不完整临时文件，保留错误日志但脱敏敏感路径/凭据。
- 多文件任务中已经成功提交的文件是否回滚，由显式目录任务语义定义，不偷偷承诺完整目录事务。

## UI 约束

三套 UI 作为独立可执行程序，但必须调用同一 `transfer-core`。最低共同能力：手动/服务器配对、远端/本地文件浏览、PUT/GET、目录传输、进度、取消、覆盖确认、错误说明、诊断入口。

建议显示：
- CLI：交互 Shell + 非交互命令；可脚本化 JSON 输出/退出码。
- TUI：双栏本地/远端文件面板、快捷键、传输队列与连接状态。
- GUI：原生桌面界面、文件选择、拖拽（可选）、传输进度、错误及诊断面板。

## 并发和生命周期

- Tokio 任务设置取消 token、超时、worker 生命周期；先取消阻塞 I/O，再 join/回收。
- 强制对任务、块缓存、RPC payload、目录项数量设置上限。
- control 不被文件 payload 填满；文件吞吐策略不影响关键取消/RPC。
- TUI/GUI 的显示事件采用节流和快照，不允许展示速度反过来阻塞网络读写。
