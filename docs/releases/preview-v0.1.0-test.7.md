# p2p-transfer v0.1.0-test.7 — 双端任务取消与终端交互改进

开发预发布版本，基于真实 Linux x86_64 ↔ RK3568 ARM64 联机反馈。

- 经认证的 CancelTransfer 按文件 request ID 处理，活动发送或接收 Future 实际结束后才返回 STOPPED；尚未启动的任务返回 PENDING，不能把仅登记取消视为已经停止。最近完成请求的状态短期保留以处理 STOP_SENDING 早于取消 RPC 的竞态。
- 取消、完成和失败时按请求 ID 清理进度。发送流被对端 STOP(0) 停止时不再当作整体 Control QUIC 失败。
- Unix 交互式终端采用同步双行重绘：进度占据输入提示符的上一行，可继续执行 status、ls、help、cancel 等非传输命令，同时保留 UTF-8、Tab 补全、命令历史。非 TTY CI 日志仍逐行；Windows TTY 暂沿用 rustyline。
- status 和提示符采用 ICE 实际选路分类：IPv4 局域网直连、IPv4 NAT 打洞、IPv6 直连或显式端口映射；Data QUIC 默认显示四条简单状态，异常时才展开错误原因。初始欢迎信息精简，help 分组对齐。
- 保留 Go 风格当前目录启动和基于操作系统权限的绝对路径访问；首次连接码仅应通过可信私密渠道交换，尤其不建议用 root 账号与不可信设备配对。

## 验收重点

在 RK3568 上 GET 2GiB，Ctrl-C 后检查 PC 端确实停止本次 Data 发送，不继续显示旧进度；双方任务租约释放，不留下部分文件，原会话仍可重新发起传输。对端若只返回 PENDING，仅表明取消已登记，不得报告为任务已停止。

GitHub Actions 的七目标测试全部通过后才发布对应 Windows、macOS、Linux 二进制及 SHA256SUMS.txt。自动化 localhost 不能替代真实双机 NAT、CGNAT、IPv6 及完整 2GiB SHA-256 性能验收。尚不支持断点续传、掉线未确认块重发、TURN/Relay。
