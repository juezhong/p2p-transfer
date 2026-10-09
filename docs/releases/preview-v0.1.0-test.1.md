# p2p-transfer v0.1.0-test.1 — 跨网络实测预览版

**这是供用户验证不同 LAN/IPv6/IPv4 NAT 环境的预发布版本，不是稳定正式版，也不保证所有 NAT 可穿透。**

## 运行
- Windows x64 / macOS x64、Apple Silicon / Linux x64、ARM64：下载对应单文件二进制，无须单独安装 SDK。
- 终端进入计划授权的文件目录，直接执行 `p2p-transfer`（Windows 执行 `p2p-transfer.exe`）。
- 双方选择创建/加入，交换私密 INVITE/REPLY，独立核对六位码并明确确认目录授权。连接后使用 `ls/cd/put/get/status/cancel/quit`；无 `send/receive`、IP/端口启动参数。
- 同网络、不同路由器、移动热点、IPv6、UDP 受限环境分别测试。记录链路是否成功、ICE 选择的候选、失败报错，且不要公布邀请码、完整公网 IP 或私人文件内容。
- 应用不使用 TURN/中继，因此对称 NAT、企业防火墙等环境下直连可能失败，不能把失败伪装成成功。
- 二进制未签名，没有自动更新。文件无覆盖写入（`.part` + SHA-256 校验提交）。不要用敏感生产数据做故障注入。

## 当前明确缺失与验收限制
- 尚未达到 Go v0.16.4 全部功能：真正远端 CANCEL、断线后 DATA QUIC 自动重建、未确认块应用层重传与续传、多条 DATA QUIC lane、ICE consent/restart、UPnP、Tab 路径补全、不同 NAT 的现场成功率和 2GiB 实测仍需后续 PR 完成。
- TUI/GUI 暂缓；CLI 和 SDK 作为验证对象。
- 本预览只有五平台 Actions 各原生运行 workspace tests 与 loopback 文件收发的自动化证据，真实网络结论以用户现场反馈为准。

详见 [双机测试手册](https://github.com/juezhong/p2p-transfer/blob/main/docs/MANUAL_TEST_PREVIEW.md) 和 [长期开发进度记录](https://github.com/juezhong/p2p-transfer/pull/1)。
