# p2p-transfer (Rust)

基于 [p2p-sdk](https://github.com/juezhong/p2p-sdk) 的 **仅直连** Rust P2P 文件和目录传输工具；Go [p2p-friend](https://github.com/juezhong/p2p-friend) 仅作为只读的功能参考，不兼容旧 Go 协议。

## 使用方法

**不需要命令行地址、端口或发送/接收参数**。打开终端进入你希望共享的目录，直接运行 `p2p-transfer`。程序会显示「创建连接 / 加入连接 / 退出」，双方私下交换 INVITE/REPLY 并核对六位码后，在同一个交互 shell 中使用 `ls/cd/pwd`、`lls/lcd/lpwd`、`put/get`、`status/cancel/quit`。可以传输文件及递归目录。

每次运行需明确授权当前共享根目录；远端文件系统操作被限制在授权目录之内。这与 Go 旧版本默认可访问全部用户可读写路径的行为**有意不同**，不能为模仿旧行为而破坏默认安全性。

## Linux ARM64 / RK3568 兼容性

`v0.1.0-test.1` 的 GNU ARM64 构建依赖较新 glibc，旧 rootfs 可能提示 `GLIBC_2.38 not found`。从 `v0.1.0-test.2` 开始另外提供 `p2p-transfer-linux-aarch64-musl` 与 `p2p-transfer-linux-x86_64-musl`：静态编译、无动态 glibc 依赖，更适合嵌入式 Linux。原 GNU/glibc 版本仍保留。ARM64 使用：

```sh
chmod +x p2p-transfer-linux-aarch64-musl
./p2p-transfer-linux-aarch64-musl --version
./p2p-transfer-linux-aarch64-musl
```

静态 musl 不保证兼容任何内核，仍需在目标板实测。**不要替换板上 glibc**。

当前 `help/status`、远程 `pwd/ls/cd`、本地 `lpwd/lls/lcd`、递归 `put/get` 有实现；**Tab 本地/远程补全与 Go v0.16.4 传输可靠性模型尚未全量实现**，不能把当前测试预览当作完整对等版。

## 网络结构和状态

SDK 负责 ICE/STUN、手动配对、双方设备证书 mTLS、独立 Control QUIC / Data QUIC；Transfer 负责文件/目录命令、4MiB 有界应用确认窗口、.part/SHA-256、权限与 UI。控制消息不会走数据载荷通道，也不提供 TURN/文件中继。

**尚处开发阶段，未发布稳定功能完整版本。** Go 对等的 Data QUIC 自动重建/未确认块重传、四条数据 QUIC、长期 ICE consent/restart、网关自动发现/续期、Tab 补全、2GiB 实测和跨 NAT 手工验收仍需完成；TUI/GUI 延后。

- [当前功能状态与下一步](docs/STATUS.md)
- [无参数交互 CLI 操作说明](docs/INTERACTIVE_CLI.md)
- [两台机器的详细测试方案](docs/MANUAL_TEST_PREVIEW.md)
- [Agent 交接工作流](docs/WORKFLOW.md)
- [长期迁移设计 PR #1（不合并）](https://github.com/juezhong/p2p-transfer/pull/1)
