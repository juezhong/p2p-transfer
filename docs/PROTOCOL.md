# Rust 文件传输线协议（设计约束，尚未定稿）

> **不兼容 Go p2p-friend 旧版线协议**。本文件不是已经实现的 wire specification，具体字节编码与版本号后续需在协议 PR 定稿。

## 原则

- 底层链路必须来自 `p2p-sdk` 的认证加密 QUIC Stream。
- Control 与 File Data 逻辑分离；主控制面可独立执行 RPC/status/cancel/ACK。
- 消息有明确 `protocol_version`、`session_id`、`request_id` / `transfer_id` / `file_id`、长度上限、能力协商和错误码。
- 不使用任意路径作为可信文件系统操作指令；目标路径须先授权并规范化。
- 接收端必须确认 entry ready，发送端才能推送相关数据。
- 文件块至少包含 `transfer_id`、`file_id`、`offset`、`payload_length`、内容；块长度、offset 和文件大小均须溢出检查。
- 接收端按 offset 去重与重排，只有连续有效数据能写入临时文件并推进累计 ACK。
- 发送方在有界的未确认窗口中缓存/可重读块；ACK 停滞或 lane 失败时重传未确认块。
- 发送端/接收端 SHA-256 对比后才将文件标记成功，接收端先 `.part` 后提交。
- 取消必须有幂等协议，取消业务传输不等于关闭已认证 P2P Session。
- 协议解析采取严格长度、计数、超时及版本约束，且有 fuzz 测试覆盖。

## 建议消息语义（暂非 wire 编码）

| 消息 | 语义 |
| --- | --- |
| `Hello/Capabilities` | 客户端协商应用协议世代、控制/数据能力 |
| `RpcRequest/RpcResponse` | 目录列举/切换/属性与授权检查 |
| `TransferAcquire/Release` | 单会话任务租约 |
| `TransferStart/EntryStart/EntryReady` | 开始任务、文件项元数据与写入准备 |
| `FileChunk` | file ID + offset + payload |
| `DataAck` | 某 file ID 的累计已连续写入 offset |
| `EntryEnd/TransferEnd/TransferResult` | 终止、校验与最终状态 |
| `Cancel/Error/Bye` | 幂等取消、错误和会话正常结束 |

## 未决定

- 二进制编码格式、端序、具体帧大小、错误码全集。
- Chunk/窗口初值、stripe 数量默认值和自适应算法。
- 完成时 fsync 和 rename 的跨平台精确语义。
- 目录快照一致性与冲突文件覆盖策略。

这些必须结合基准测试与安全审查再定稿，不能把旧 Go 参数逐字照搬视为最优解。
