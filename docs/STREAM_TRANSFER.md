# M2 实际文件传输内核（独立 Control / Data）

## 本轮实现

- `transfer-core::stream_transfer` 接受两个**已由 SDK 认证并且相互独立**的双向控制/单向数据 Async I/O 通道，绝不在 Transfer 里重新造 STUN/ICE/NAT/Quinn。
- Sender 在用户授权的共享根目录内安全打开并计算整个文件 SHA-256，发送有长度上限的文件请求元数据；Receiver 严格校验相对路径，使用 cap-std 句柄隔离和唯一 `.part` 文件。
- 分离的 Data Stream 按最大 128 KiB 分块发送；Receiver 按顺序写入磁盘文件句柄后才通过 Control Stream 返回累计 ACK。接收完成后做 SHA-256/长度校验、sync + no-clobber commit，只有提交成功才发送最终 DONE。
- 自动测试覆盖大于两个分块的 Unicode 文件、0 字节文件、跨目录写入、错误相对路径拒绝、元数据长度边界、接收端实际落盘与 SHA-256。
- **M2 正确性基线采用 1 个在途数据块**，内存有界；Go v0.16.4 中的多在途滑动窗口、限速/拥塞、重传、取消和多 data-only QUIC 恢复仍未完成。后续明确按 benchmark 调优，不能把目前的正确性基线描述为高吞吐最终实现。

## 严格边界

当前网络层由调用者供应 AsyncRead/AsyncWrite；本 PR 的模拟端点来自 tokio::io::duplex，**没有连接到真实 SDK VerifiedManualSession 也没有跨机器传输**。它是可用于后续 SDK QUIC 接入的真正文件业务核心，不是可向用户发布的 CLI。收发端必须由外层先完成设备身份验证、手动确认码和授权；否则此模块不能对不可信端点开放。

尚缺目录 RPC、下载方向请求协议、重传/恢复与取消、单任务仲裁、文件改动并发检测、流控窗口、磁盘异步池和跨平台数据量实测。写入 ACK 表示操作系统文件句柄的顺序写入完成，不表示磁盘已经 fsync。

## 下一批开发

1. 经 `p2p-sdk::VerifiedManualSession` 取得已认证双 QUIC 的 Control/Data Stream，并接上 `send_file` / `receive_file`；以 SDK 的真实 ICE nominated + mTLS 环回测试验证实际数据传输。
2. 补齐 Transfer CLI 的配对、授权根目录、远端目录浏览/PUT/GET 命令与单任务互斥，再实现真实跨 LAN/IPv4 NAT 测试。
3. 大文件滑动窗口应用级 ACK、重传/故障恢复、取消状态和 sha256/.part 保证；原 Go 参考仓库保持只读。
4. 最后由 Transfer 统一构建 Windows x64、macOS x86_64/arm64、Linux x86_64/arm64 的 Debug 和 Release。
