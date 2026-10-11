# p2p-transfer v0.1.0 — 首个稳定 CLI 版本

基于已验收的 [v0.1.0-test.8](https://github.com/juezhong/p2p-transfer/releases/tag/v0.1.0-test.8) 功能基线（\`e78f2083b9807c7fdc1864e4c4cd9ea824ca0c62\`）。**仅更改 CLI 版本号、文档和发布配置，没有新增或改动文件传输业务代码。**

## 主要功能

- 无必填启动参数的创建/加入交互菜单与可信渠道 INVITE/REPLY 交换。
- 已认证 ICE/QUIC 直连和独立 Control/Data 通道；双向文件与递归目录 PUT/GET、目录浏览、SHA-256 文件校验。
- 任意一端的 \`cancel\`/Ctrl-C 可取消在途传输，不阻塞 Control；显示真实绝对路径、写盘 ACK 进度、预计剩余时间；修复 Unix 终端退出恢复。

## 平台

Linux x86_64/aarch64（glibc 和静态 musl）、macOS x86_64/aarch64、Windows x86_64 共 **7 个二进制**，附 \`SHA256SUMS.txt\`；流水线进行新版本号的 Release 重编译、跨目标测试和完整性校验。

## 已知限制

- “稳定”仅指当前已验收 CLI 功能基线，**不等于宣称所有网络环境下的生产级可靠性或完整 Go 功能对等**。仍需完成 2 GiB 整文件 SHA 验收、跨 NAT/CGNAT/IPv6 测试及断线故障注入。
- 仅支持直连，没有 TURN/Relay；未确认块断线重传、四 Data lane 并行 striping 尚未完成；TUI/GUI 尚未正式交付。
- **权限警告：** Unix 配对对端按当前运行账户权限可访问整个本机文件系统（Windows 当前限启动盘符）。不要以 root 或高权限账户与不可信设备配对，INVITE/REPLY 必须通过可信私密渠道交换。当前流程没有独立的首次人工六位 SAS 校验步骤。
