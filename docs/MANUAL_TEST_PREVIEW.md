# Rust Transfer 双机联机测试：无参数 Go 风格 CLI（开发预览）

> **目前尚未达到 Go v0.16.4 完整功能对等，不是稳定 Release。** 目录浏览、PUT/GET、递归目录、手动配对/ICE、双向 mTLS、控制/数据独立 QUIC 已通过五平台本机双进程自动测试；真正不同路由器的公网 NAT 穿透还没有用户现场结果。

## 获取五平台构建

打开 [p2p-transfer GitHub Actions](https://github.com/juezhong/p2p-transfer/actions/workflows/debug.yml)，选择最新 **全部成功** 的 `Transfer debug builds (five targets)`，从 Artifacts 下载：

- Windows x86_64：`p2p-transfer-debug-windows-x86_64`
- macOS Intel x86_64：`p2p-transfer-debug-macos-x86_64`
- macOS Apple Silicon ARM64：`p2p-transfer-debug-macos-aarch64`
- Linux x86_64：`p2p-transfer-debug-linux-x86_64`
- Linux ARM64：`p2p-transfer-debug-linux-aarch64`

打包的只是 Debug 程序，未签名。实际可用性要看对应分支、架构的 CI 和真实网络测试，不能依据仅有构建文件判断。

## 与 Go 版一致：直接运行，无需任何参数

设备 A 和 B 分别进入要共享的工作目录（每次会话需明确授权），例如 Linux/macOS：

```sh
cd ~/p2p-share
./p2p-transfer
```

Windows：

```powershell
cd C:\Users\Public\p2p-share
.\p2p-transfer.exe
```

程序显示：

```text
1) 创建连接
2) 加入连接
3) 退出
```

1. A 选择 `1`、确认当前目录共享授权；B 选择 `2`、授权其目录。SDK 会自动选择本机地址和 UDP 端口，并优先尝试局域网 Host 连通性检查；不需要用户输入 IP、端口、收发方向或命令行文件路径。
2. A 输出 `P2PR-INV2-` 邀请码，私下发给 B；B 粘贴后生成 `P2PR-REP2-` 回传码，私下发回 A。
3. 两人通过可信的独立渠道核对显示的六位确认码，分别确认。**不要公开 INVITE/REPLY 原文**：内含私有 IP 和短期 ICE 密码。
4. 真正通过 ICE nomination、mTLS 和双 QUIC 会话认证后，双方进入持续交互 shell。

## 交互命令

```text
pwd
ls
cd 子目录
lpwd
lls
lcd 子目录
put "文件名.txt" ["对端文件名.txt"]
get "对端文件名.txt" ["本地文件名.txt"]
put "整个目录"
get "整个目录"
status
cancel
help
quit
```

目录传输包含多级文件和空目录，使用安全根目录和独立 Control/Data QUIC；当前默认不会覆写接收端同名文件。文件每个分块的 ACK 以已顺序写入目标文件为准，每个文件校验 SHA-256 后才从 `.part` 提交。

如果仅需 LAN 离线测试，可以设置 `P2P_TRANSFER_STUN=off`，这样跳过公网 STUN 查询；保持程序无参数启动。跨公网测试时不要禁用 STUN。

## 实测顺序与反馈

1. 两台设备在同一 LAN 上传/下载小文件、Unicode 文件、递归目录、空目录，并核对双方 SHA-256。
2. 分别验证 Windows/macOS/Linux 及跨平台混用的双机连接、目录授权拒绝、相同文件不覆盖。
3. LAN 成功后让 A/B 连接**不同网络/路由器**，观察 IPv4 STUN server-reflexive / IPv6 直连是否被标准 ICE 成功提名，再实际传输文件。
4. 测试强制断开 Data 网络链路、Ctrl-C、重复传输时当前实现可能仍会失败；**不要因此误认为具备完整断点恢复**。

只需回报操作系统架构、LAN 或不同 NAT 类型、ICE 是否选到可达路径、控制/数据连接是否成功、文件大小/哈希及错误文本。请隐藏真实外部 IP、完整连接码和私人文件内容。

## 尚缺的功能

当前 SDK 仍需多网卡并行候选、ICE consent / restart、PCP/NAT-PMP 自动网关发现/续期、UPnP，以及各 NAT 真实网络验证。Transfer 仍需断线后的未确认块恢复、最多四条 Data QUIC 的动态修复、Tab 自动补全、2GiB 五平台性能回归。TUI/GUI 依用户要求后置，**这些验收完成前不创建稳定 Release**。
