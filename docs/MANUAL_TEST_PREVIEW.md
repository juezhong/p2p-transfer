# 双机手动直连传文件：实验 CLI 预览测试计划

> **状态：五平台原生构建和本机双进程传文件自动测试已通过**（[Actions #37928856150](https://github.com/juezhong/p2p-transfer/actions/runs/37928856150)）；可开始实验性质的两台机器 LAN 测试，但尚无真实跨 NAT 成功手工证据。并非 Go v0.16.4 的功能等价版本：仍缺目录递归、远端浏览、GET、滑动窗口、故障恢复、TUI/GUI。

## 下载与平台

从 `p2p-transfer` 仓库的 Actions → `Transfer debug builds (five targets)` → **成功运行** → Artifacts 下载：

- Windows x86_64: `p2p-transfer-debug-windows-x86_64`
- macOS Intel x86_64: `p2p-transfer-debug-macos-x86_64`
- macOS Apple Silicon arm64: `p2p-transfer-debug-macos-aarch64`
- Linux x86_64: `p2p-transfer-debug-linux-x86_64`
- Linux arm64: `p2p-transfer-debug-linux-aarch64`

这只是 Debug 可执行程序，不是稳定 Release。macOS 本地执行前可能需要授予程序执行权限；不要关闭系统防火墙来“证明 NAT 穿透”。

## 首轮：同一 LAN 上两台机器

确保两端能通过各自实际网卡 IP 相互通信（不要使用 `0.0.0.0`），UDP 不被本机防火墙完全阻断。

设备 B（接收方），例如：

```sh
./p2p-transfer receive 192.168.1.22:0 /home/user/p2p-inbox
```

设备 A（发送方），例如：

```sh
./p2p-transfer send 192.168.1.11:0 /home/user/share "文件.txt" "收到/文件.txt"
```

注意：接收端 `/home/user/p2p-inbox/收到` 必须事先存在；最终目标文件不能已存在（安全 no-clobber）。源文件相对 `SOURCE_ROOT`。

1. A 输出 INVITE，**私下**发给 B。
2. B 输入 INVITE 后输出 REPLY，私下发回 A。
3. A 输入 REPLY。A、B 会显示相同的六位校验码；**通过独立可信方式**核对，然后两边各自输入从另一台机器核对得到的校验码。切勿只看本机数字就直接自动确认。
4. 双方开始 ICE candidate checks，优先选实际可达的 LAN host candidate。成功后输出本地/远端 UDP 端点、TLS/会话认证状态，并使用独立 Data QUIC 传输文件；Control QUIC 返回应用级累积 ACK 与最后 SHA-256/提交结果。
5. 两端均报完成后，对比文件大小和 SHA-256，并将测试结果记录到 TC-001 / TC-003。

**不要公开 INVITE/REPLY**：其中包括 LAN/IP 地址、ICE 短期凭据、临时公钥和公开证书；不要截图分享完整连接码。

## 第二轮：两个不同网络

在上述 LAN 测试通过后，添加一个或多个可达 STUN **IP 地址**和端口，例如：

```sh
./p2p-transfer receive 192.168.10.22:0 /home/user/p2p-inbox 203.0.113.10:3478
./p2p-transfer send 192.168.20.11:0 /home/user/share "文件.txt" "文件.txt" 203.0.113.10:3478
```

示例 `203.0.113.10` 是**文档保留地址、不可当真实服务器使用**。请用自己的真实 STUN 服务器 IP 替换。IPv6 使用 `[地址]:端口`，且需双方绑定真实 IPv6 本地地址。

STUN 观测不保证 NAT 穿透；不同 NAT、CGNAT、终端防火墙/IPv6 stateful firewall 可能无法建立任何直连路径，这时应报告错误而不是回退到服务器转发文件。

## 必须收集的结果

记录系统架构、两台设备同 LAN/跨公网、IPv4/IPv6、是否能建立 ICE/QUIC、接收文件大小与校验、控制/数据链路是否同时健康、失败原因。**不得分享识别码、密码、完整私有地址或文件内容。**

## 已知未完成及安全边界

- 本 CLI 当前限定两端各自指定具体单网卡 IP，不支持 wildcard 绑定/自动探测全部网卡、多个地址族同时选择；候选仍会按可用性尝试，但不保证在每个网络优先选择 LAN。
- 单文件单任务，128 KiB 一块且每块等待控制 ACK，适合优先验证正确性，不适合高速大文件性能测试。
- ICE consent freshness / restart / NAT 映射变更恢复、Data QUIC 自动恢复、端口映射 PCP/NAT-PMP/UPnP 仍未实现。
- 用户密码只与未来可选消息服务器登录有关；手动配对仍无需服务器和密码，但双方必须核对六位比较码。
- 与 Go p2p-friend 旧线协议不兼容；无 TUI/GUI 文件业务；本阶段不标为稳定 1.0 版本。
