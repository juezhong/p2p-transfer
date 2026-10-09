# NAT 实测记录（预发布 v0.1.0-test.1）

本文件只记录**真实设备**跨网络结果；GitHub Actions 的 `127.0.0.1` 双进程自动测试属于另一条证据，不能将其填成公网穿透成功。

## 实测清单（待用户反馈）

| Case | 网络组合 | 设备/OS | ICE 是否提名 | Control/Data QUIC | PUT/GET SHA-256 | 结果 |
| --- | --- | --- | --- | --- | --- | --- |
| NAT-01 | 同一 Wi-Fi IPv4 LAN | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-02 | 同 LAN 双栈 IPv6/IPv4 | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-03 | 不同家庭路由器 IPv4 NAT | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-04 | 家庭网络 ↔ 手机热点/CGNAT | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-05 | 两端公网 IPv6 + 有状态防火墙 | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-06 | 企业/校园网络严格 UDP 出站限制 | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-07 | STUN 不可达但仍有 LAN | 待测 | 待测 | 待测 | 待测 | 未执行 |
| NAT-08 | 连续大文件/目录、网络切换或断连 | 待测 | 待测 | 待测 | 待测 | 未执行 |

## 每次反馈记录

- 下载版本/tag 与二进制文件 SHA-256：
- A/B 操作系统、CPU 架构、是否公网 IPv6、是否 CGNAT：
- A/B 网络拓扑（LAN / 不同 NAT / 热点 / VPN）：
- INVITE/REPLY 与人工六位确认是否完成：**只记录是/否，不记录识别码原文**。
- STUN 候选数量、ICE nominated 地址类型（Host/srflx/portmapped），状态和报错：
- Control/Data 是否均建立、是否异常断开：
- 传输方向 PUT/GET、单文件/目录、大小及本地双方校验结果：
- 如果失败，操作步骤、程序错误文本和发生阶段：
- 敏感内容必须脱敏（公网 IP、邀请码、私有文件名/内容）。

## 当前阻塞交付功能

持续 ICE consent 和 restart；网关自动发现/续租与 UPnP；多网卡 IPv4/IPv6 并发路径竞速；可靠远端取消；Data QUIC 自动重建和未确认块重传；四条 Data lane；CLI Tab 补全；2 GiB 性能验收；TUI/GUI（暂缓）。

证据应汇总到 [Transfer PR #1](https://github.com/juezhong/p2p-transfer/pull/1) 和 [SDK PR #2](https://github.com/juezhong/p2p-sdk/pull/2) 留档。跨网络不可直连场景必须返回失败，不允许添加中继。
