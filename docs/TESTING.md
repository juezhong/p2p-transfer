# 自动化测试、故障注入与性能策略（草案 v0.1）

手工执行流程和记录模板见 [TEST_PLAN.md](TEST_PLAN.md)。本文件定义开发阶段必须自动化的用例及验收方式，**不意味着目前已经跑过任何测试**。

## 1. Rust 工具链门禁（代码出现后）

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
```

对网络、目录、权限相关逻辑加入 fuzz/property 测试。并行跨平台 CI 至少覆盖 Linux amd64、Windows amd64、macOS；arm64 通过原生测试或明确的交叉编译与机器手测补足。不得把纯 cross-compile 当成运行测试。

## 2. Unit test

- 消息 encode/decode 往返、版本不兼容拒绝、长度边界、fuzz 和随机输入不 panic。
- Unicode 路径和操作系统路径差异；非法 `..`、符号链接、Windows reparse point。
- ACK 单调性、offset/size 溢出、重复数据块、旧 file ID 的误投递。
- bounded reorder buffer、内存池回收、zero-byte file、最后一个短块。
- 租约并发冲突、反向主客端错误信息、取消幂等性。
- 哈希不同必须拒绝提交；测试文件 rename/sync 的平台差异。

## 3. Integration test

- 两个独立 Rust 进程通过 SDK 本地/远端 session，执行完整 PUT/GET/目录复制，并与 SHA-256 fixture 对照。
- 大流量 data stream 传输同时持续发 RPC、status 和 Cancel。
- 控制会话隔离：数据 QUIC 断开时应用继续响应控制操作。
- SDK 的手动/自动信令入口走同一 transfer-core API。
- 测试用例和业务追踪 ID 应与 `docs/MIGRATION.md`/`TEST_PLAN.md` 同步。

## 4. 故障注入

- 传输中断开一条数据连接、所有 data 连接、仅控制连接。
- ACK 超时、丢失、延迟、重复、乱序；文件内容修改、截断、写盘权限丢失。
- `Write` 成功后立即断开数据 QUIC，确认应用级 ACK 缺口触发安全重发。
- 在队列满、socket flow control 阻塞、worker 等待时触发取消，确认无死锁。
- 离线/在线、IPv6 丢失后回退 IPv4、设备休眠与网络切换。
- 出现无法连接的 NAT 或 UDP 被阻断时明确 `NoDirectPath`，**无中继**。
- 模拟磁盘空间不足、rename 失败、文件正被占用、符号链接竞争。

## 5. 性能与资源监测

- 文件规模：0 B、1 MiB、128 MiB、2 GiB，大小横跨多个 chunk 边界。
- 数据模型：单大文件、许多小文件、目录树、UTF-8 路径。
- 网络：Loopback、同 LAN（1G/2.5G 等明确接口速率）、公网 NAT 双机与受限 WAN；禁止将 loopback 得出的吞吐当成互联网速度。
- 指标：MiB/s、总耗时、P50/P95 RPC 延迟、CPU、峰值 RSS、分配/复制热点、重传字节、lane 重建耗时、取消耗时。
- 与 Go `p2p-friend@78c6b72` 在相同测试硬件/数据/网络重复 A/B 对照。Go 和 Rust 两个程序各自和同语言 peer 连接，不要求 Go↔Rust 会话。
- 原 Go v0.16.4 发布记录的单机 ABBA 吞吐不是通用性能门槛，不能直接声称 Rust 超越或低于 Go。
- 默认性能任务不得自动生成 2 GiB 文件；由手动 workflow/明确 opt-in 执行，并检查磁盘可用空间。

## 6. 发布门禁

- M2：TC-001、003~008；两机文件传输和 SHA-256。
- M3：TC-009~013、021、022、025 及对应故障注入。
- M4：TC-011、017~020、023；必须有不同 NAT/双栈测试或显示已知缺口。
- M5：TC-014~016；三界面与相同 core。
- M6：TC-024、026；正式二进制签名/校验和、平台发布记录和手测证据。

测试运行结果要归档到独立 `docs/test-reports/YYYY-MM-DD-...md`，包括平台与可重现上下文；本文件只定义测试要求。
