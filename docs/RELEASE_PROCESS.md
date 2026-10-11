# p2p-transfer 稳定版规划与自动发布流程

## 版本基线

- **v0.1.0**：首个稳定 CLI，基于 test.8 的已接受功能。保留历史版本，不回写或覆盖旧 Release。
- **v0.1.1**：本次稳定修复版本，包含 PR #40 的**仅任务发起方可取消**、`status` 双端一致展示任务状态和发起设备、正常退出保护。发布源为已合并上述修复的 main，并同步 CLI `Cargo.toml` 与 `Cargo.lock` 的 `0.1.1` 版本。
- **v0.1.2**：增加本地 SHA-256/MD5 校验命令、关闭 GET 发送侧重复回传字节日志；文件传输协议不变。
- **v0.1.3**：先合并 SDK #73 的通用认证 Link API，再由 Transfer 明确把 bootstrap Link 用作业务 Control、四条可选 Link 用作业务 Data；新增 `checksum <本机文件>` 单次流式读取的双摘要命令。两台设备双向传输和关闭恢复尚须实机验证。
- **v0.1.4**：从已验证的 main 提交 `c2f1715` 发布 CLI 稳定性维护版。仅变更版本号及发行说明，使用之前合并的 PR #46：校验任务等待传输结束、计算期间独占输入提示符、进度界面与失败说明优化；不改动 SDK 或传输核心。`Filesystem(Io)` 的具体 OS 失败原因尚未定位，不宣称已经根治。
- **未来 v0.1.x**：兼容性修复、可靠性/安全修复优先递增补丁版本，例如 `v0.1.5`；新增向后兼容功能可按 SemVer 选择 `v0.2.0`。重大不兼容协议/交互变更必须写明双端最低兼容版本及迁移路径。
- 废弃 `v0.1.0-test.N` 预发布系列，不通过普通源码合并擅自改版本号或生成无版本变更的 Release。

## 一次正式发布的步骤

1. 在专门的 release PR 中更新 **`crates/transfer-cli/Cargo.toml` 的 version 和 `Cargo.lock` 中的 transfer-cli version**，保持一致；核心库/TUI/GUI 维持独立版本策略。
2. 在 `docs/releases/vX.Y.Z.md` 记录变更、兼容性、已知风险、人工验收情况。更新必要的 README/STATUS，并在 release PR 评论中注明下一版计划。
3. PR 的 `Stable CLI release (versioned)` 只验证：CI 临时工作区 `cargo fmt --all` / `cargo fmt --all -- --check`、Clippy、工作区测试、7 目标测试及发布编译。PR 阶段**不创建 tag 或 GitHub Release**。
4. 上述检查通过并合并 `main` 后，只要本次推送包含 CLI 版本变更或发布工作流变更，自动执行相同的七目标构建与验证，随后 **发布非 prerelease 的 `vX.Y.Z` Release**，包含七个新编译二进制与 `SHA256SUMS.txt`。标签指向该次 main 发布提交，程序 `--version` 应显示 `X.Y.Z`。
5. 已有 Git 标签/Release 不覆盖、不删除、不悄悄替换附件；失败时修复后可在 Actions 手动重跑 `Stable CLI release (versioned)`，或通过经评审的后续版本进行恢复。

## 兼容性与验收

- v0.1.1 使用新的 `TransferStatus` Control RPC，为避免状态同步与命令行为不一致，**要求连接双方均运行 v0.1.1**，不混用旧 test.8/v0.1.0。
- GitHub Actions 的双进程本机环回测试不能替代 Linux x86_64 ↔ RK3568 上的 2 GiB PUT/GET、非发起方取消拒绝、发起方取消后的双端空闲、再次传输成功及终端退出恢复验收。
- 稳定 CLI 仍无 TURN/Relay，未确认块断线重传、多 Data lane 并行 striping及跨 NAT/CGNAT 现场验收仍是后续任务。
- Unix 配对设备可能按当前进程权限访问本机全文件系统；不要以 root 账户连接不可信设备，勿公开 INVITE/REPLY。
