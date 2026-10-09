# p2p-transfer v0.1.0-test.3 — QUIC 空闲连接修复与中文路径补全

**NAT 现场测试预发布**。此版本修复 RK3568 ↔ Linux x86_64 局域网在没有任何命令/传输情况下，双方完成 ICE/mTLS/双 QUIC 后异常断开的疑似原因；不等于全部 Go v0.16.4 功能完成。

## 变化
- 固定 SDK commit `c54acfe7dd8f33bfabfebf2a39037ee15723d487`：Control 和 Data 独立认证 QUIC 统一设置 10s keep-alive、120s 有限空闲超时；SDK 真正等待 >30s 的双端闲置回归已通过。
- CLI 真实双进程黑盒回归：连接成功后静置 38s，再进行 `help/status/put/get`。Control/ Data 异常断开将打印原始 QUIC close reason 便于定位原因。
- 修复同步输入线程抢先打印下一 `p2p>` 的问题：当前命令输出完成后才打印下一提示符，并在提示符展示远端当前目录。
- 使用 UTF-8 Unicode 终端编辑器，对本地 `lcd/lls/put`、远端 `cd/ls/get`及第二路径参数提供按 Tab 的路径补全，中文和带空格目录保持引号；远端候选通过已认证 Control RPC 读取，不允许越过已授权共享根。
- 原先 6 位核对码仍由 SDK 生成：需要双方**独立渠道**确认数字一致后输入 `yes`，不再重复抄写数字；不得仅凭本机屏幕判断一致。
- 保留原有 Windows、macOS、Linux GNU/glibc 和 ARM64/x86_64 静态 musl 共七种可执行文件及 SHA-256 清单。RK3568 建议下载 `p2p-transfer-linux-aarch64-musl`。

## 测试
```sh
chmod +x p2p-transfer-linux-aarch64-musl
./p2p-transfer-linux-aarch64-musl --version
./p2p-transfer-linux-aarch64-musl
```

A/B 在同一 LAN 进入菜单配对并核对六位码，之后不要执行命令或传输，等待 **60—120 秒**，确认两端仍停留在命令提示符，再试 `status`、`ls`、`cd 中文目录`、Tab 中文路径补全及双向 `put/get`。

## 未完成与风险
- 新版 QUIC ping 仅保持健康路径/有限超时，不是 ICE consent freshness，也不保证断网再连接。确实断线需要 ICE restart / Data QUIC 重新认证恢复。
- SDK 尚未达到 Go 参考版的多网卡 IPv4/IPv6 同时竞速、动态 prflx punch、网关映射发现/续期、状态诊断全部能力。
- Transfer 尚未达到 Go 的最多 4 条独立 Data QUIC、1 MiB 动态分条/有界重排、断点修复、远端 CANCEL、全部 CLI 路径和覆盖模型。远端 Tab 补全和中文路径需用户设备真测，尤其 Windows。
- 邀请码长度仍受完整 DER X.509 证书及 ICE 信令携带开销影响，目前暂未切换协议格式。**不会**以取消认证或取消独立身份核对换取更短码。
- 真实 WAN/NAT 和 2 GiB 文件传输仍需用户测试反馈；本版本不是稳定版，也没有 TURN/业务文件中继。

进度留在 [Transfer 文档 PR #1](https://github.com/juezhong/p2p-transfer/pull/1) 与 [SDK 文档 PR #2](https://github.com/juezhong/p2p-sdk/pull/2)。
