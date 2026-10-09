# p2p-transfer v0.1.0-test.2 — ARM64 musl 兼容性测试版

这是继续验证不同网络 NAT/ICE 的 **预发布测试版**，并非 SDK/Transfer 的 Go v0.16.4 全功能等价正式版。

## 改动
- Linux 增加两份 **静态 musl** 可执行程序：`p2p-transfer-linux-aarch64-musl` 和 `p2p-transfer-linux-x86_64-musl`；GNU/glibc 版本全部保留。构建阶段使用 ELF readelf 检查 musl 版不包含动态解释器和 NEEDED 库。
- 修复 RK3568 等老 glibc rootfs 运行 glibc 较新二进制报 `GLIBC_2.38 not found` 的可移植性问题：优先试用 aarch64-musl 版本，不要替换系统 `libc.so.6`。
- 对本次共享根目录的输入 yes 提示明确显示本机目录绝对路径、远端可列出/下载/上传/创建目录的权限、不会覆盖文件和会话限定。
- 其余自动选址、ICE、STUN、证书校验、独立 Control/Data QUIC 文件传输、`help`/`status`/`ls`/`cd`/`put`/`get` 等保持当前行为。所有版本使用 Rust 新协议，不与旧 Go 识别码互通。

## 如何测试
```sh
chmod +x ./p2p-transfer-linux-aarch64-musl
./p2p-transfer-linux-aarch64-musl --version
./p2p-transfer-linux-aarch64-musl
```
两端不带参数进入创建/加入；私下交换 INVITE/REPLY，独立核对六位比较码。仅在选择的本机共享目录下进行安全测试。出现问题记录 `status`、路径类型、操作系统/架构、`uname -r`、`ldd --version`（对静态程序不要求 ldd 成功），脱敏错误日志。

## 尚未实现或验证
- SDK 没有完成全网卡/IPv4+IPv6 并发 ICE checks、持续 consent freshness、ICE restart、自动 PCP/NAT-PMP/UPnP 网关映射维护，也未有跨实际 NAT 用户现场验收。
- Transfer 没有完成断线 Data QUIC 自动重建、未确认块应用级重传/可靠续传、多 Data lane、可靠远端 CANCEL 和 Go 同等 Tab 本地/远端路径补全；当前 `status` 为基础连接信息，不是 Go 全部诊断字段。TUI/GUI 暂缓。
- 无服务端数据中继，不保证对称 NAT/CGNAT/严格防火墙可直连。
- 原 `P2PR-INV2-/REP2-` 识别码包括完整 X.509 公钥证书、ICE 候选和短期凭据，因此较长；缩短码不能丢弃身份认证和可达信息，本版本暂不改变识别码格式。
- musl 静态消除新版 glibc 依赖，但仍要求目标内核具有所需 syscall/UDP/权限，并须在真实 RK3568 rootfs 上验证。

## 自动验证
五平台原生 GNU/macOS/Windows 构建测试 + 新增 x86_64/aarch64 musl 静态构建；CI 结果以此次发布工作流为准。用户测试结果记录到 [NAT 现场测试表](https://github.com/juezhong/p2p-transfer/blob/main/docs/NAT_FIELD_TEST_RESULTS.md) 和 [长期设计 PR #1](https://github.com/juezhong/p2p-transfer/pull/1)。
