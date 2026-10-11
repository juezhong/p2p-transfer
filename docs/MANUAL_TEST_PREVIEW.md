# Rust Transfer 双机联机测试：无参数 Go 风格 CLI（v0.1.0-test.6 预览）

> **目前尚未达到 Go v0.16.4 完整功能对等，不是稳定 Release。** 目录浏览、PUT/GET、递归目录、手动配对/ICE、双向 mTLS、控制/数据独立 QUIC 已通过五平台本机双进程自动测试；真正不同路由器的公网 NAT 穿透还没有用户现场结果。

## 获取五平台构建

优先从 [GitHub Release v0.1.0-test.6](https://github.com/juezhong/p2p-transfer/releases/tag/v0.1.0-test.6) 的 **Assets** 下载所属架构的正式预览二进制和 `SHA256SUMS.txt`。如果本次七目标发布工作流尚未结束，请不要下载旧的 v0.1.0-test.3 进行新版 SDK 测试；可在 [Release Actions](https://github.com/juezhong/p2p-transfer/actions/workflows/preview-release.yml) 中检查进度。

- Windows x86_64：`p2p-transfer-windows-x86_64.exe`
- macOS Intel x86_64：`p2p-transfer-macos-x86_64`
- macOS Apple Silicon ARM64：`p2p-transfer-macos-aarch64`
- Linux x86_64：`p2p-transfer-linux-x86_64`（GNU/glibc）或 `p2p-transfer-linux-x86_64-musl`（静态）
- Linux ARM64：`p2p-transfer-linux-aarch64`（GNU/glibc）或 `p2p-transfer-linux-aarch64-musl`（静态，RK3568 优先尝试）

这些附件来自经七目标测试的优化构建，**仍为未签名的测试版**；不代表已经在真实公网 NAT/CGNAT 上验证成功。

## 与 Go 版一致：直接运行，无需任何参数

设备 A 和 B 分别进入希望作为**初始位置**的工作目录即可（不是文件访问边界，无需共享目录 yes）。例如 Linux/macOS：

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

1. A 选择 `1`、B 选择 `2`。不需要输入目录授权 `yes`，也不需要设置环境变量。SDK 自动收集地址和 UDP 端口，优先进行局域网 ICE Host 检查。
2. A 输出 `P2PR-INV2-` 邀请码，私下发给 B；B 粘贴后生成 `P2PR-REP2-` 回传码，私下发回 A。
3. 双方交换完整 INVITE/REPLY 后自动进行认证建连，不再显示六位人工核对步骤。**务必通过可信、私密渠道传递完整连接码**，否则首次设备身份可能受到中间人替换。
4. 经过 ICE nomination、mTLS PIN 和会话 HMAC 验证后进入交互 shell。**Unix 默认让配对对端读写当前程序账号能访问的全文件系统，超出启动目录也一样**；避免在 root 账号下与不可信设备配对。Windows 当前支持启动盘符内路径。

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
2. 分别验证 Windows/macOS/Linux 及跨平台混用的双机连接、相同文件不覆盖、绝对路径 PUT/GET 与 `cd ..`；Windows 跨盘符能力仍是已知限制。
3. LAN 成功后让 A/B 连接**不同网络/路由器**，观察 IPv4 STUN server-reflexive / IPv6 直连是否被标准 ICE 成功提名，再实际传输文件。
4. 测试强制断开 Data 网络链路、Ctrl-C、重复传输时当前实现可能仍会失败；**不要因此误认为具备完整断点恢复**。

只需回报操作系统架构、LAN 或不同 NAT 类型、ICE 是否选到可达路径、控制/数据连接是否成功、文件大小/哈希及错误文本。请隐藏真实外部 IP、完整连接码和私人文件内容。

## 尚缺的功能

当前 SDK 仍需多网卡并行候选、ICE consent / restart、PCP/NAT-PMP 自动网关发现/续期、UPnP，以及各 NAT 真实网络验证。Transfer 仍需断线后的未确认块恢复、最多四条 Data QUIC 的动态修复、Tab 自动补全、2GiB 五平台性能回归。TUI/GUI 依用户要求后置，**这些验收完成前不创建稳定 Release**。

## test.5 交互行为

- 输入 INVITE/REPLY 后按 Go v0.16.4 直接连接，**不再提示六位码或要求 yes，启动时也不再要求目录授权 yes**。安全代价：首次设备身份不再通过独立的比较码确认；只能在可信且私密的渠道交换完整连接码，否则可能出现首次配对的中间人替换。
- `status`：显示 ICE/Control 的双方 UDP IP/端口、拨号方向、候选类型、STUN/网关信息、每条 Data QUIC 的连接标识、本地 IP 与远端 UDP 地址，以及双方**本次明确授权的真实共享根目录**和会话租约拥有方。SDK 尚未公开 Data owner 的本地端口，本版无法完整显示每一条 Data 的本地 UDP 源端口。
- `put/get`：命令行暂停下一提示符直至任务结束/取消；每秒主动打印基于接收端实际写盘 ACK 的百分比、MiB、速率、已用时间与 ETA；其他 RPC 不被暂停。Ctrl-C 可取消当前本地任务；远端可靠 CANCEL 与断点续传仍未完成。
- 一方 `quit` 时，另一方提示对端已断开，保留本地 shell 只接受 `quit` 退出；不会因正常 QUIC 关闭打印误导性网络错误。请用两台设备反复验证退出过程没有残留 `.part`、Data socket 或占用端口。

## test.6 现场重点

- 启动欢迎屏打印版本号；终端使用一行持续刷新的进度条，stdout 重定向日志时仍按行输出。
- 在 RK3568 运行 `get 2GB.bin`，传输达到 5–10% 后按 Ctrl-C。应收到远端取消确认；x86_64 发送方应**立即终止这次文件发送**，没有继续上升的进度；Control 和 4 条 Data 保持认证连接，稍后可重新发起新的 GET。
- 两台设备都输入 `status`，检查 Transfer 租约最终空闲、暂存文件删除；然后断开并退出。这里验证远端取消不需要把整个 P2P 会话关闭。
- 与 Go v0.16.4 一样，**从当前目录启动就能直接使用绝对路径**，无需环境变量和额外的目录授权 yes；`cd ..` 可以离开启动目录，`cd /etc`、`get /home/user/file.txt`、`put /var/tmp/data.bin` 等命令受操作系统权限约束。双方默认显示各自真实**启动工作目录**，不是文件访问的上限。**安全代价：Unix 配对方可以访问运行账户有权限访问的全部文件/目录，务必只与可信设备配对，避免使用 root 身份。** Windows 当前仅支持启动盘符，跨盘符路径另行适配。
- 命令提示符只显示远端授权路径（例如 `/root/p2p-transfer>`），不再附加 `p2p[remote:...]` 前缀；`pwd` 和 `lpwd` 展示完整路径。

## 最新使用方式（test.6）

在需要开始的目录中运行可执行程序，选择 1 创建或 2 加入，交换 INVITE/REPLY 即可进入 shell。**不再输入共享目录授权 yes、没有六位码确认，也不设置 P2P_TRANSFER_SHARE_ROOT。** `pwd` 和提示符显示远端启动目录，`lpwd` 显示本机启动目录；使用 `cd /absolute/dir`、`lcd /absolute/dir` 自由切换，或直接 `get /absolute/file` / `put /absolute/file`。请只与可信设备互联。
