# p2p-transfer v0.1.0-test.5 — 实机 LAN 反馈修复（开发预发布）

这份测试版修复用户在 Linux x86_64 ↔ RK3568 ARM64 LAN 真实打通 ICE/Control 和 4/4 认证 Data 后发现的交互及退出缺陷。**不是生产稳定版，也不代表已在真实设备上完成 2 GiB 性能与完整性验收。**

## 更新
- 对照 Go p2p-friend v0.16.4：PUT/GET 变为占据前台的任务，任务未完成时不会提示下一个普通命令；Ctrl-C 仍可取消。后台 Control RPC 不被阻塞。
- 传输中按秒报告当前文件进度、接收方实际落盘 ACK 字节数、百分比、平均 MiB/s、耗时与 ETA；status 显示同一时刻快照、租约占用者及请求编号。
- status 增加 ICE Control 双端 UDP、主动/被动角色、对端候选类型、STUN/网关映射、实际 Data QUIC 连接 ID/本机 IP/远端 UDP、远端真实共享根目录与相对目录。远端路径来自经 SDK 认证的 Control RPC，只涉及本次明确授权的路径。
- 一方正常退出，另一方提示已断开，阻止继续执行网络命令，由用户自行输入 quit；结束时取消任务、清理未完成 GET、关停认证 Data 托管连接与 SDK Control，避免将正常关闭误报成文件故障。
- 按 Go 版的交互简化：不再要求额外六位码/yes。**安全注意：首次配对不再提供独立身份核对**；必须通过可信、私密渠道交换完整 INVITE 和 REPLY，否则邀请码被替换时可能受到 MITM。SDK 仍强制 QUIC mTLS 证书 PIN、Session HMAC 与 ICE nomination，未加入 Relay/TURN。

## 本轮验证
由本 Release 的 GitHub Actions 在七目标中自动运行真实双进程 PUT/GET、递归、离线配对与一方主动退出的 CLI 回归、SDK 高层 QUIC 目录 RPC，以及写盘 ACK 进度统计单元测试。只有全目标成功才允许自动上传七份二进制和 SHA256SUMS.txt。下载后可执行 `--version` 确认 `0.1.0-test.5`。

## 仍需现场验证
1. 两机从 100 MiB 开始到 2 GiB 重测（双方不要同时发起另一任务），记录速度、ETA、SHA-256、数据速率。
2. 传输时 Ctrl-C 取消与另一端 quit；确认不会保留 .part、后台 socket 或无期限租约。
3. 验证远端实际路径和 Control/Data 状态，以及跨 LAN/不同路由器、IPv6 与 CGNAT 是否能经标准 ICE nomination 直连。
4. SDK 尚未公开**每条 Data 的本地独立 UDP owner 源端口**，本版状态对此只可显示远端 UDP、连接 ID 和本地 IP，不得把其余状态推测成实际源端口。
5. 尚未完成 Go v0.16.4 的未确认分块自动恢复、四路条带数据重发、完整远端 CANCEL 和 2 GiB 性能对标。

已有双机测试步骤见 [测试文档](https://github.com/juezhong/p2p-transfer/blob/main/docs/MANUAL_TEST_PREVIEW.md)。未经确认不要在公网分享 INVITE/REPLY 或敏感文件。
