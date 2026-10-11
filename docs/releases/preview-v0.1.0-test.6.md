# P2P Transfer v0.1.0-test.6 — authenticated cancel + CLI UX test

**仅用于真实双机测试，不是正式稳定版**。核心网络由 p2p-sdk #71 负责，文件协议由 Transfer 管理；Rust 和 Go v0.16.4 的线协议不互通。

- **修复主要缺陷**：接收方 Ctrl-C/cancel 现在通过已经 mTLS PIN/HMAC 认证的 Control QUIC 发送按文件 request ID 的 CancelTransfer RPC；发送方立即停止当前 Data Future，接收方丢弃部分文件，批次租约可释放，**不中断整体 P2P 会话**。防取消/请求竞态的有限取消墓碑表。
- 欢迎界面打印内置版本；远端当前真实授权目录进入命令提示符（不再显示 p2p 前缀），`pwd/lpwd` 是完整路径；`status` 分组显示 Control/ICE/四条 Data/授权路径/任务等。
- TTY 下实时进度使用**同一行回车重绘**；文件写盘 ACK 的百分比、速度、耗时和 ETA 仍在。非 TTY 每秒日志保持可解析。
- `help` 重排命令与功能。**和 Go v0.16.4 一样，以程序启动目录作为初始本机/对端工作目录，而不是沙盒边界**。Unix 默认直接访问该用户具有权限的整个文件系统，不再设置环境变量或要求共享目录 `yes`。可以 `cd ..`、`cd /other`、`put /absolute/file`、`get /absolute/file`；对端的初始 cwd 通过认证 Control RPC 获得，提示符显示完整真实路径。Windows 目前支持启动盘符，跨盘符待完善。
- Go 对标的可靠远端 CANCEL 已实现核心路径，但用户真实设备验证、2GiB 完整 SHA-256、重连与多链路条带未确认数据自动恢复仍需完成。
- 协议没有 TURN/Relay，首次 INVITE/REPLY 必须由可信、私密渠道交换（此版本仍省略独立六位核对）。

**本版仅会在主分支七目标编译和全部回归通过后由 Release CI 上传** Windows/macOS/Linux 七份二进制及 SHA256SUMS.txt。请使用 SHA 校验并报告 2 GiB 实际双机取消、清理与随后重新传输情况。

**高风险提示**：配对成功即把当前进程在 Unix 上有权限访问的整个文件系统开放给对端执行列表、读、上传、建目录等操作，**包括启动目录之外**。这按用户明确要求对标 Go，不能替代访问控制。只应与可信设备配对，不使用 root/管理员权限启动；INVITE/REPLY 必须走可信私密通道。
