# 开发状态交接 — p2p-transfer

> 通过本仓库 `main` 和最新打开的功能 PR 核实状态。这里是任务索引，不是测试结果。

## 已确认的设计

- Go `p2p-friend@78c6b72`（v0.16.4）只是只读参考；**Rust 新版本不兼容旧 Go 线协议**。
- Transfer 使用 SDK 的 ICE/信令/QUIC/身份/网络诊断，不能重复网络实现。
- 必须迁移 Go 所有可观察到的文件功能及可靠性行为，最后提供完整 CLI/TUI/GUI。
- 控制面和数据面分离；最终具备 1 条 control-only + 最多 4 条 data-only QUIC；文件 ACK/重传由 Transfer 负责。

## 两份长期设计记录 PR（用户要求保留、不合并）

- [Transfer PR #1：迁移架构、路线图、26 项手工测试](https://github.com/juezhong/p2p-transfer/pull/1)
- [SDK PR #2：Transfer 对 SDK 的功能契约](https://github.com/juezhong/p2p-sdk/pull/2)

## 进度（实际源码/CI 优先）

- M0：Rust Workspace、文件业务基础类型、单元测试与 CI —— 开发启动阶段。
- M1：等待 SDK 的真实 ICE/Quinn 原型 —— **未完成**。
- M2：文件/目录 PUT/GET —— **未完成**。
- M3：SHA-256/.part/ACK/重传/取消可靠性 —— **未完成**。
- M4：四数据 QUIC 与故障恢复 —— **未完成**。
- M5：CLI/TUI/GUI 三界面 —— **未完成**。
- M6：跨平台测试、2 GiB 基准与发行 —— **未完成**。

## 下一步

1. 查看最新开发 PR，核实 M0 已实现的具体代码和测试结果。
2. 在 SDK 达到可用前，先完成 `transfer-core` 状态机、路径权限与文件协议的测试驱动开发。
3. SDK 真实 Session API 完成后开展 TC-001/TC-003 双进程传输。


## 已创建的功能 PR（2026-10-09）

- [Transfer 开发 PR #2 — M0 Workspace、任务状态与四项测试](https://github.com/juezhong/p2p-transfer/pull/2)，**Draft，未合并**。
- 包含 `transfer-core`、`transfer-cli`、`transfer-tui`、`transfer-gui` Cargo crates。后三者**只是明确标示未实现的占位入口**，不是实际 CLI/TUI/GUI。
- Task 状态机包含开始、写入确认、校验阶段、完成、取消与失败；不具备真实文件 I/O、SHA-256、SDK 连接或上传下载。
- **未运行本地 Rust 编译/测试**：当前环境没有 rustc/cargo；下一 Agent 应核对 PR CI 并修正 fmt/clippy/test。
- 可并行的下一项：路径授权安全模型及文件协议编解码测试，避免在 Transfer 内重复实现 SDK 的网络模块。


## CI 故障处理记录（2026-10-09）

- [功能 PR #2](https://github.com/juezhong/p2p-transfer/pull/2) 初始 CI 因 `transfer-core/src/task.rs` 的 `cargo fmt --check` 失败。
- 已在同一功能分支修正格式；[最新成功的 GitHub Actions](https://github.com/juezhong/p2p-transfer/actions/runs/37899185667) 完成 fmt、clippy 与 cargo test。
- 仅验证 M0 Workspace/任务状态机；**文件传输、SDK 连接与 CLI/TUI/GUI 仍未实现**。切勿把绿色 CI 当成全部业务功能完成。


## 进展（2026-10-09）：M0 应用控制协议基础

- 已合并 M0 Workspace：[PR #2](https://github.com/juezhong/p2p-transfer/pull/2)，main squash commit `99ecfe0`。
- 开发中：[PR #3](https://github.com/juezhong/p2p-transfer/pull/3)：带版本、类型、请求 ID、1 MiB payload 上限的 Rust 控制帧编解码与单元测试。**不兼容旧 Go 线协议。**
- 尚无真实的 RPC、文件 I/O、身份认证、上传/下载；CLI/TUI/GUI 仍为占位。
- 下一步：目录访问权限、路径越界防护和文件传输 RPC 语义测试；在 SDK 完成安全会话后集成实际传输。


## 已完成并合并（2026-10-09）

- M0 Workspace：[PR #2](https://github.com/juezhong/p2p-transfer/pull/2) 已合并；提交 `99ecfe0`。已有 `transfer-core` 任务状态机、三个仅占位的 CLI/TUI/GUI 可执行入口及相应 CI。
- M0 协议基础：[PR #3](https://github.com/juezhong/p2p-transfer/pull/3) 已合并；提交 `2702a70`。Rust 新线协议的固定头、版本、FrameKind、request_id、1 MiB payload 上限与边界测试。最新 PR Actions fmt/Clippy/test 已通过；这只是 Frame codec，不是可用的 RPC 或文件传输。
- 仍**没有**实际网络连接、文件系统 RPC、上传/下载、.part、SHA-256、真正 TUI/GUI。不能按“可用版本”发布。
- 下一项：`transfer-core` 文件路径授权/越界阻断（FT-04/FT-09/TC-022）和目录操作；待 SDK 的真正 QUIC Session API 可用后接入完整文件收发。

## M2 最小文件访问授权准备（2026-10-09，开发 PR）

- 在 `transfer-core` 添加 `AuthorizedRoot`：通过显式共享根目录授权读取，拒绝绝对路径、`..`、符号链接和不存在的文件；支持 Unicode 路径和目录列表；有单元测试。
- **安全边界**：当前只是前置路径检查，不能抵御其他本地进程在检查与打开之间修改符号链接的 TOCTOU 竞态；**尚不提供安全文件写入 API**，在完善原子 descriptor-relative open 之前不能作为生产级远端读写沙箱。
- SDK 的 ICE/Quinn 连接和 Transfer 的实际 PUT/GET/GUI 均未在本 PR 实现；Transfer 面向用户的五架构发布要等到业务和 SDK 联网流程可用后再启用。
- 下一步：平台安全的 openat/handle-relative 打开，明确共享根目录和远端授权，目录 RPC + 文件读写 + SHA-256/.part 可靠提交，随后连接 SDK。

## M2 基于目录句柄的真实安全文件 I/O（2026-10-09 开发中）

- 新功能分支 `feat/m2-capability-safe-file-io` 使用成熟的跨平台 Rust `cap-std` 做真实目录句柄相对打开，防止只靠 canonicalize/字符串检查导致 TOCTOU 根目录逃逸；这是之前 `AuthorizedRoot` 仅预检 API 的替代方向。
- 新增接收 `.part` 随机唯一文件、顺序流式写入与已写字节 ACK、SHA-256 校验、文件 `sync_all`、同目录 hard-link 原子 no-clobber 发布和 Drop/取消清理；完全位于 Transfer Core，不涉及 SDK 网络栈。
- Linux/Unix 单元测试包括 Unicode 文件、路径穿越、符号链接逃逸、拒绝覆盖、校验失败不发布。尚未完成真正文件 RPC/远端授权、递归传输、磁盘故障与中途断电恢复；文件句柄 fsync 不保证所有文件系统的目录元数据持久化。须等 CI 实际通过再记录完成。
- 用户最终五平台 Debug/Release 由 Transfer 发布；SDK 不发布独立 Debug。

## 用户验收与发行入口确定（2026-10-09）

- 按用户决定，五平台最终 Debug/Release 全部由 Transfer 提供（Windows x86_64、macOS x86_64/aarch64、Linux x86_64/aarch64）；SDK 不单独分发 Debug。Transfer CLI 必须能够显示已认证 ICE 提名候选及 QUIC 连接诊断，让真实两机传文件同时验证 SDK。
- SDK #20/#21/#22/#23 已合并且相关 CI 通过，但尚无真实公网 NAT 打洞证明；Transfer 现已有 Workspace/协议帧/授权目录/.part/SHA-256 基础，真正网络 PUT/GET、ACK/重传、可用 CLI/TUI/GUI 和跨平台发行仍待开发。
- 优先并行推进 Transfer 协议/目录操作和 SDK 真实 Session 集成；第一个可发布用户测试版门槛是**两台机器安全配对、ICE 选路、独立控制数据 QUIC、实际传输并校验 SHA-256**。真实 NAT 环境要用户最终测试。进度优先评论长期记录 PR #1，不合并该 PR。

## M2 真正文件收发业务内核（开发 PR / 未经 CI 验证）

- 新功能分支 `feat/m2-transfer-stream-engine`：`transfer-core::stream_transfer` 通过**独立控制与数据 Async Stream** 实现文件 Offer、顺序数据块、落盘后的累计 ACK、最终 SHA-256 / 原子 no-clobber commit；直接复用 cap-std 安全共享根目录。详见 `docs/STREAM_TRANSFER.md`。
- 添加 tokio::io::duplex 自动化收发测试，含 >2 个块、Unicode、0 字节、目录越界拒绝和报文边界。CI 通过前不得标记完成。
- **尚未可两机手测**：尚未绑定 SDK VerifiedManualSession 的 QUIC Stream，CLI 仍为占位；M3 sliding window/重传/取消、TUI/GUI、五平台产物仍未实现。不发测试版。
- 下一功能批次首要任务是 SDK 认证双 QUIC 与本 Stream Core 连接并执行真实 QUIC 文件发送/落盘集成测试，之后再打通手动 LAN 配对 CLI。

## 首个双机手动传文件 CLI（功能 PR 待验证）

- Transfer [PR #6](https://github.com/juezhong/p2p-transfer/pull/6) CI 通过并合并：独立控制/数据流上的真实文件收发核心。Transfer [PR #7](https://github.com/juezhong/p2p-transfer/pull/7) 通过 [CI #37927466032](https://github.com/juezhong/p2p-transfer/actions/runs/37927466032) 并合并（814d380）：通过 SDK VerifiedManualSession 的独立 Control/Data QUIC 完成 localhost 标准 ICE 提名、mTLS、实际文件落盘、ACK 和 SHA-256。
- 新功能分支 feat/m2-manual-ice-cli-send-receive：新增实验 CLI send/receive，要求双方显式指定本机网卡 IP、授权目录；手动 INVITE/REPLY + 可信通道比较码确认，再经 SDK 完整 ICE/mTLS/双 QUIC 调用 Transfer Core 真实文件收发。
- 新增五架构 native Debug 构建/测试工作流及 docs/MANUAL_TEST_PREVIEW.md。**最新 Rust CI 和五平台构建均通过之前，不声称可以手动双机测试。**
- 未完成：远端浏览/GET、目录递归、取消/单任务调度、滑动窗口与恢复、TUI/GUI，以及真实跨 NAT 和 ICE restart/consent。Debug 只是早期预览，不是稳定 Release。

## 首个五平台双机手动传输预览已通过自动化测试（2026-10-09）

- **[Transfer PR #6](https://github.com/juezhong/p2p-transfer/pull/6) 已合并**（08a4cfe）：真正的文件块收发、分离控制/数据 Stream、接收方磁盘写入后累计 ACK、.part/SHA-256/no-clobber 提交。Tokio 双端模拟单元/集成测试成功。
- **[Transfer PR #7](https://github.com/juezhong/p2p-transfer/pull/7) 已合并**（814d380）：直接使用 SDK VerifiedManualSession 的两条独立 Control/Data QUIC Stream；[CI #37927466032](https://github.com/juezhong/p2p-transfer/actions/runs/37927466032) 通过真实 SDK 手动 v2 + ICE nomination + mTLS 端到端文件落盘测试（localhost）。
- **[Transfer PR #8](https://github.com/juezhong/p2p-transfer/pull/8) 已合并**（f1f7575）：提供可实际运行的 CLI send/receive；通过手动 INVITE/REPLY、独立人工核对六位码、ICE 直连提名、双向 mTLS、独立 Control/Data QUIC 发送文件；也增加五平台 Debug Artifact 工作流及 LAN/NAT 手工测试文档。
- 最新 PR #8 [Rust Checks #37928856036](https://github.com/juezhong/p2p-transfer/actions/runs/37928856036) **success**；[五平台 Debug #37928856150](https://github.com/juezhong/p2p-transfer/actions/runs/37928856150) **success**：Windows x86_64、macOS x86_64/aarch64、Linux x86_64/aarch64 全部原生构建、执行包含两进程交互式配对+文件传输的完整 Workspace 测试、上传五份 Artifact。曾修复 Windows capability-handle 清理和 CLI 主线程栈问题。
- **现在可以进行实验性质的双机 LAN 手工测试**，参见 [docs/MANUAL_TEST_PREVIEW.md](MANUAL_TEST_PREVIEW.md)；同网段测试通过后才尝试 STUN/跨 NAT。未得到用户真实 LAN/NAT 结果，不能宣称公网 NAT 打洞已验证。
- **仍未完成 Go v0.16.4 等价功能**：远端目录浏览、主动 GET、文件夹递归、多文件批量、滑动窗口/重传恢复、Data QUIC 断后自动重连、取消和任务 lease、TUI/GUI。当前单文件一在途块，不适合性能评估；CLI 为实验版，不是稳定 Release。
- 下一批开发需优先接入远端目录及 GET、滑动窗口 ACK/重传和故障恢复、更多自动化安全测试；SDK 还需 consent freshness、ICE restart、PCP/NAT-PMP/UPnP、多网卡与实际跨 NAT 优化，保持直连无 relay。两份长期设计 PR 不合并。

## M3 跨双 QUIC 的目录 RPC 与双向 GET（功能 PR 待 CI，2026-10-09）

- 新增 Transfer 专用有界 RPC：远端目录列表及 GET 请求，保留 Unicode 路径；通过 SDK VerifiedManualSession 的 Control QUIC 请求和响应，文件内容仍仅走 Data QUIC。
- 新增对 Control Stream 首帧的安全分派：文件 PUT offer 或目录/GET RPC，允许一条连接进行多次请求；GET 通过**反向独立 Data QUIC** 将文件写入发起方已授权的目录。
- 真实 SDK 手动 ICE v2 + mTLS localhost 集成测试扩展为 PUT→LS→GET→双方磁盘字节相同。CI 绿之前不视为验收完成。
- 还缺 Go 式交互 CLI、递归传输、取消/抢占、滑动窗口、恢复及公网 NAT 手测；不创建正式 Release。长期记录 PR #1 不合并。

## Go 式无参数交互 CLI（开发中，须验收 CI）

- [Transfer PR #9](https://github.com/juezhong/p2p-transfer/pull/9) 已通过最新 Rust Checks 与五平台 native Debug CI 并合并（`465003a`）：SDK 已认证 Control QUIC 上的远端 LIST / GET RPC，文件数据通过独立 Data QUIC 双向传输；真实 ICE/mTLS localhost PUT→LS→GET 测试通过。
- 新功能分支 `feat/m3-go-style-interactive-cli` 将程序无参数启动转换成 Go 风格菜单「创建/加入/退出」，SDK 自动查找本机实际 IPv4/IPv6 网卡、自动 UDP 端口及可选 STUN，再提供持续命令 shell：pwd/ls/cd、lpwd/lls/lcd、put/get、status/cancel/quit；含跨两个真实无参数 CLI 进程的交互和 PUT/GET 测试。详见 docs/INTERACTIVE_CLI.md。**以最新 CI 结果为准，尚未自动完成。**
- 与 Go 差异：必须确认共享根（默认 cwd），拒绝访问范围以外的绝对路径；单网卡 UDP Owner、文件不支持目录递归、无窗口/重传/恢复、cancel 仅初步本机中断、无 Tab 补全、未发布正式版本。五平台真实网络 NAT 穿透未验证。

## 2026-10-09 交互 CLI 与 4MiB 窗口整合

- [PR #11](https://github.com/juezhong/p2p-transfer/pull/11) 已通过 Rust checks 和五平台 Debug CI 合并，Go 风格无参数交互菜单、目录导航、PUT/GET、自动网卡候选。相比旧 Go，仍有限制共享根和缺少递归/Tab 等。
- [PR #12](https://github.com/juezhong/p2p-transfer/pull/12) 旧 head 已通过 CI，但因为 #11 先合并而冲突，不能直接合并。现在在 [PR #13](https://github.com/juezhong/p2p-transfer/pull/13) 基于最新 main 重放带实际写盘累计 ACK 的 4MiB 有界窗口和测试。
- 后续必须补足：目录递归、多文件、真正会话级取消/仲裁、断线重传/数据 QUIC 恢复、可选网关映射与公网 NAT 实测、CLI Tab 补全。TUI/GUI 按用户要求延后。**未达到 Go 功能等价和真实 NAT 手测验收前不创建用户正式 Release**。

## M3 安全目录递归清单（功能 PR 待 CI）

- `transfer-core::recursive` 新增有界（100000 项、64 层、最大 4096 UTF-8 名字）目录遍历，使用 `SharedRoot` capability-based 目录/文件句柄，保留 Unicode 文件名、空目录、文件大小，拒绝路径逃逸、不可读项、链接越界；测试含混合树和 symlink。
- 这仍只是递归清单模块，没有与网络 PUT/GET 目录分批及远端 mkdir RPC 整合，不能宣称用户已经能递归传目录。
- 下一步真正目录批次协议、目的端安全 mkdir 与逐文件 SHA 提交、取消和传输任务锁；之后继续数据面重传/故障恢复。TUI/GUI 暂缓；发布条件不变。

## M3 目录递归网络 PUT/GET（功能分支测试中）

- 受限递归清单和 `SharedRoot::create_directory` 已经由 [#14](https://github.com/juezhong/p2p-transfer/pull/14) 通过五平台 CI 并合并（`937b4ae`）。
- 此分支新增 RPC `ListTypes`（远端条目文件/目录类型）与 `MakeDirectory`（cap-std 受限建目录），并连接 CLI `put/get`：递归 PUT 按清单逐目录/文件发送；递归 GET 使用类型化列表逐层请求，等待**每个文件真正写盘和 SHA-256 提交**后才进入下一文件。仍保留独立 Control/Data QUIC。
- 新增真正两个无参数 CLI 进程的目录 PUT→GET 自动测试，覆盖 Unicode、嵌套目录、空目录及 SHA-256 数据一致。以最新 CI 与五平台测试为准；**真实公网 NAT 还没验证**。
- 尚缺故障后的重传/数据面恢复、完整单会话跨端任务仲裁、可靠远端取消、Tab 补全、PCP/NAT-PMP/UPnP、SDK consent/restart。暂不发布正式版本，TUI/GUI 延后。

## 2026-10-09 Go 风格 CLI/目录递归阶段验证完成

- [PR #11](https://github.com/juezhong/p2p-transfer/pull/11) squash `39388a1`：无启动参数的交互式创建/加入、自动网卡、目录导航与双向 PUT/GET，五平台验证。
- [PR #13](https://github.com/juezhong/p2p-transfer/pull/13) squash `5516f3a`：在交互 CLI 上整合最高 4MiB 在途的累计写盘 ACK，最新 Rust + 五平台 Actions success。
- [PR #14](https://github.com/juezhong/p2p-transfer/pull/14) squash `937b4ae`：cap-std 安全受限递归清单、深度/条目上限和安全 mkdir，五平台 Actions success。
- [PR #15](https://github.com/juezhong/p2p-transfer/pull/15) squash `d081269`：[Rust CI #37936828907](https://github.com/juezhong/p2p-transfer/actions/runs/37936828907) 与 [五平台 Debug/双进程测试 #37936828883](https://github.com/juezhong/p2p-transfer/actions/runs/37936828883) 均 success。Control QUIC 目录类型 / mkdir RPC，Data QUIC 逐文件传输；CLI 的目录 PUT/GET 可遍历多层结构、保留空目录和 Unicode，GET 每文件等待实际校验提交，回归单文件测试修正正常的目录探测拒绝处理。
- 当前仍**不符合旧 Go v0.16.4 完整功能对等**：远程可靠 CANCEL、双方 lease 仲裁、Data QUIC 断后重建与未确认块重传、最多四条 Data QUIC、高负载/2GiB 跨平台基准、Tab 补全、SDK NAT consent/restart/端口映射和公网双机手测尚未完成。按用户要求继续优先 CLI/SDK；TUI/GUI 暂缓，不创建冒充完成的正式 Release。

## M3 会话传输租约基础（2026-10-09 功能分支待 CI）

- 新增 `transfer-core::lease::TransferLease` 的 RAII 独占任务 lease；同一会话本地不可有两个独立活动的文件任务，异步取消会 Drop 并释放 lease；目录/状态 RPC 不必独占 Data QUIC。增加并发争用、异常取消、重新获得租约的自动化测试。
- **没有宣称跨设备租约仲裁完成**：此模块仅是本地构件，下一步要在认证的 Control QUIC 上实现双端申请/授予/释放与并发冲突一致决策，随后整合到 send/receive/GET、目录批次以及真正远端取消；没接入前 CLI 仍存在 A/B 同时发起的竞态风险。
- 其他主要未完成：未确认块的重传、Data QUIC 恢复、最多四条独立 Data QUIC、Tab 补全、ICE consent/restart/网关端口映射、完整性能验收。稳定 Release 继续暂缓；TUI/GUI 依用户要求后置。
