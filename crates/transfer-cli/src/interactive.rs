//! Go-style argument-free interactive pairing using the SDK's *high-level*
//! ICE / mTLS / Control-path negotiation and authenticated Data connections.
//! Transfer owns only user prompts, explicit directory access and file lanes.

use std::{net::SocketAddr, sync::Arc};

use p2p_sdk::{
    direct_peer::{begin_creator, begin_joiner},
    local_network::local_addresses,
    multi_interface::MAX_ACTIVE_INTERFACES,
    peer_pin::ManualConfirmation,
    transport_session::ConnectedTransportPeer,
};
use transfer_core::{
    modern_data_lanes::ModernDataLanes,
    secure_io::SharedRoot,
};

use crate::{debug_error, now_secs, prompt, CliResult};

pub struct ActivePeer {
    pub session: Arc<ConnectedTransportPeer>,
    pub data_lanes: Arc<ModernDataLanes>,
    pub role: &'static str,
}

fn verify_user(
    mut confirmation: ManualConfirmation, displayed_code: &str,
) -> CliResult<ManualConfirmation> {
    // Match Go v0.16.4's INVITE/REPLY-only user flow. No human SAS prompt.
    // A substituted first-time invitation may allow MITM, so only exchange
    // invitation/reply codes over a confidential, trusted out-of-band channel.
    println!("[配对] INVITE/REPLY 已验证，正在连接。");
    println!("[安全提示] 已省略六位人工核对；请确保两个连接码通过可信、私密渠道交换。");
    confirmation.confirm(displayed_code)
        .map_err(|_| "INVITE/REPLY 绑定校验失败，拒绝连接".to_owned())?;
    Ok(confirmation)
}

async fn default_stun() -> Vec<SocketAddr> {
    if std::env::var_os("P2P_TRANSFER_STUN").as_deref()
        == Some(std::ffi::OsStr::new("off"))
    {
        return Vec::new();
    }
    // Discovery only; offline LAN still works without third-party STUN.
    // SDK owns the probes and validates each candidate with ICE later.
    p2p_sdk::direct_peer::discover_default_stun().await
}

pub async fn run() -> CliResult<()> {
    println!("\nP2P Transfer v{}（Rust）— 交互模式", env!("CARGO_PKG_VERSION"));
    println!("1) 创建连接（生成 INVITE 邀请码）");
    println!("2) 加入连接（输入 INVITE 邀请码）");
    println!("3) 退出");
    let mode = loop {
        match prompt("请选择 [1/2/3]：")?.as_str() {
            "1" | "create" => break 1,
            "2" | "join" => break 2,
            "3" | "quit" | "exit" => return Ok(()),
            _ => println!("请输入 1、2 或 3"),
        }
    };
    let cwd = std::env::current_dir().map_err(debug_error)?;
    let root = Arc::new(SharedRoot::for_interactive_shell(&cwd)
        .map_err(debug_error)?);
    println!("\n本机初始目录：{}", root.starting_directory().display());
    #[cfg(unix)]
    println!("[文件访问] 与 Go 版一致：连接后可访问本机操作系统权限允许的文件和目录，\n             包括初始目录外的绝对路径；配对设备也能读写本机这些路径。");
    #[cfg(windows)]
    println!("[文件访问] 默认可浏览本机启动所在盘符内具备操作系统权限的目录；\n             跨盘符访问仍待单独适配。");
    println!("[安全提示] 请只与可信设备配对，不要以高权限账户运行传输程序。");
    let interfaces = local_addresses()
        .map_err(|err| format!("无法枚举本机网络接口：{err}"))?;
    if interfaces.is_empty() {
        return Err("没有可用的 IPv4/IPv6 网络接口".into());
    }
    let addresses: Vec<SocketAddr> = interfaces.into_iter()
        .take(MAX_ACTIVE_INTERFACES)
        .map(|ip| SocketAddr::new(ip, 0)).collect();
    println!("自动收集 {} 个本机 IPv4/IPv6 UDP 接口，无需手工填写 IP 或端口",
        addresses.len());
    let stun = default_stun().await;
    println!("STUN 探测服务器数量：{}（仅用于候选收集；局域网不依赖公网服务器）",
        stun.len());

    let peer = if mode == 1 {
        create_connection(&addresses, &stun).await?
    } else {
        join_connection(&addresses, &stun).await?
    };
    println!("已建立安全 P2P 连接。输入 help 查看命令；Ctrl-C 可请求取消活动任务。");
    crate::shell::run(peer, root).await
}

fn connected_peer(
    transport: ConnectedTransportPeer, role: &'static str,
) -> CliResult<ActivePeer> {
    let diagnostic = transport.diagnostic();
    if !diagnostic.bootstrap_connected {
        return Err("SDK 未完成经认证的 Control 连接".into());
    }
    let peer_address = diagnostic.actual_remote_udp;
    let local_address = diagnostic.actual_local_udp;
    let session = Arc::new(transport);
    let data_lanes = Arc::new(
        ModernDataLanes::start(Arc::clone(&session), 4).map_err(debug_error)?
    );
    println!("直连 ICE/QUIC 路径：{local_address} → {peer_address}，不使用服务器中继");
    println!("Control QUIC={}；应用认证 Data 链路上限={}",
        if diagnostic.bootstrap_outbound { "outbound" } else { "inbound" },
        data_lanes.desired());
    Ok(ActivePeer {
        session, data_lanes, role,
    })
}

async fn create_connection(
    addresses: &[SocketAddr], stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let (pending, code) = begin_creator(addresses, stun, now_secs()?, 1200)
        .await.map_err(debug_error)?;
    println!("把下面的 INVITE 邀请码私下发给对方：\n{code}");
    let reply = prompt("请粘贴对方返回的 REPLY 回传码：")?;
    let creator = pending.receive_reply(&reply, now_secs()?).map_err(debug_error)?;
    let confirmation = verify_user(
        creator.confirmation().map_err(debug_error)?,
        &creator.comparison_code(),
    )?;
    let now = now_secs()?;
    let connected = creator.connect_transport(&confirmation, now)
        .await.map_err(debug_error)?;
    connected_peer(connected, "创建方")
}

async fn join_connection(
    addresses: &[SocketAddr], stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let code = prompt("请粘贴创建方发送的 INVITE 邀请码：")?;
    let (joiner, reply) = begin_joiner(&code, addresses, stun, now_secs()?)
        .await.map_err(debug_error)?;
    println!("把下面的 REPLY 回传码私下发给创建方：\n{reply}");
    let confirmation = verify_user(
        joiner.confirmation().map_err(debug_error)?,
        &joiner.comparison_code(),
    )?;
    let connected = joiner.connect_transport(&confirmation, now_secs()?)
        .await.map_err(debug_error)?;
    connected_peer(connected, "加入方")
}
