//! Go-style, argument-free manual pairing UI; Rust-only new wire protocol.
//! Actual interface discovery, ICE, STUN and mTLS are owned by p2p-sdk.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use p2p_sdk::{
    ice_signaling::IceRole,
    local_network::local_addresses,
    multi_interface::{gather_interfaces, MAX_ACTIVE_INTERFACES},
    manual_ice_v2,
    manual_pairing::ManualPairing,
    peer_pin::ManualConfirmation,
    quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
    resilient_data::{ResilientDataLanes, MAX_DATA_LANES},
    session_binding::ReplayGuard,
    tls_identity::{authenticated_client_config, authenticated_server_config},
    udp_owner::UdpOwner,
    verified_session::{establish_initiator, establish_responder, VerifiedManualSession},
};
use transfer_core::secure_io::SharedRoot;

use crate::{debug_error, now_secs, prompt, trust_peer_der, CliResult};

pub struct ActivePeer {
    pub session: Arc<VerifiedManualSession>,
    pub data_lanes: Arc<ResilientDataLanes>,
    pub peer_address: SocketAddr,
    pub local_address: SocketAddr,
    pub role: &'static str,
    // The endpoint and receiver task MUST outlive both QUIC connections.
    _owner: UdpOwner,
    _endpoint: quinn::Endpoint,
}

fn verify_user(pairing: &ManualPairing) -> CliResult<ManualConfirmation> {
    let mut confirmation = ManualConfirmation::new(
        pairing.credentials.session_id(), pairing.comparison_code,
    ).map_err(debug_error)?;
    println!("双方设备应显示相同的 6 位配对核对码：{}", confirmation.comparison_code_text());
    println!("请通过另一可信渠道（例如语音）核对两台设备的数字是否一致。");
    println!("这不是邀请码的一部分；它用来防止首次配对被第三方替换。");
    let answer = prompt("已经与另一台设备核对且完全一致？输入 yes 继续，其余输入取消：")?;
    if answer != "yes" {
        return Err("用户没有确认双方配对核对码一致；拒绝建立连接".to_owned());
    }
    confirmation.confirm(&confirmation.comparison_code_text())
        .map_err(|_| "配对核对未通过，取消连接".to_owned())?;
    Ok(confirmation)
}

async fn default_stun() -> Vec<SocketAddr> {
    if std::env::var_os("P2P_TRANSFER_STUN").as_deref() == Some(std::ffi::OsStr::new("off")) {
        return Vec::new();
    }
    // These public servers see the UDP source IP of the STUN query. STUN
    // is optional; an offline LAN may still work without DNS or Internet.
    let hosts = ["stun.l.google.com:19302", "stun1.l.google.com:19302"];
    let mut servers = Vec::new();
    for hostname in hosts {
        if let Ok(Ok(resolved)) = tokio::time::timeout(
            Duration::from_secs(2), tokio::net::lookup_host(hostname)
        ).await {
            // Discover both IP families; each bound UDP Owner will probe
            // only a STUN endpoint of its own address family.
            let resolved: Vec<_> = resolved.collect();
            for family in [true, false] {
                if let Some(server) = resolved.iter().copied().find(|addr| addr.is_ipv4() == family) {
                    if !servers.contains(&server) { servers.push(server); }
                }
            }
        }
    }
    servers
}

pub async fn run() -> CliResult<()> {
    println!("\nP2P Transfer（Rust）— 交互模式");
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
    let dir = std::env::current_dir().map_err(debug_error)?;
    println!("\n本次连接将共享的本机目录：");
    println!("  {}", dir.display());
    println!("对端连接成功后，可以列出并下载该目录内的文件及子目录，");
    println!("也可以向该目录内上传文件或创建子目录（不会覆盖已有文件）。");
    println!("对端不能读取或写入此目录以外的路径；本次授权仅在当前会话有效。");
    let answer = prompt(&format!(
        "确认允许对端访问【{}】及其子目录？输入 yes 授权，其余输入取消：",
        dir.display()
    ))?;
    if answer != "yes" {
        return Err("未授权目录；会话未建立".into());
    }
    let root = Arc::new(SharedRoot::authorize(&dir).map_err(debug_error)?);
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

async fn create_connection(
    addresses: &[SocketAddr], stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let identity = rcgen::generate_simple_self_signed(vec!["client.local".into()])
        .map_err(debug_error)?;
    let certificate = identity.cert.der().clone();
    let gathered = gather_interfaces(
        addresses, stun, IceRole::Controlling, Duration::from_secs(3),
    ).await.map_err(debug_error)?;
    let offer = gathered.combined.clone();
    let (pending, code) = manual_ice_v2::invite_with_certificate(
        now_secs()?, 1200, certificate.as_ref(), &offer,
    ).map_err(debug_error)?;
    println!("把下面的 INVITE 邀请码私下发给对方：\n{code}");
    let reply = prompt("请粘贴对方返回的 REPLY 回传码：")?;
    let (pairing, remote, remote_cert) =
        pending.finish_with_certificate(&reply, now_secs()?).map_err(debug_error)?;
    let confirmation = verify_user(&pairing)?;
    let selected = gathered.nominate_first(&remote, Duration::from_secs(30))
        .await.map_err(debug_error)?;
    let mut owner = selected.owner;
    let nominated = selected.path;
    let local = owner.handle.local_address();
    println!("直连 ICE 提名：{} → {}，不使用服务器中继", nominated.local, nominated.remote);
    let tls = authenticated_client_config(
        vec![certificate], rustls::pki_types::PrivateKeyDer::Pkcs8(
            identity.signing_key.serialize_der().into()
        ), trust_peer_der(remote_cert)?,
    ).map_err(debug_error)?;
    let adapter = QuinnUdpAdapter::from_owner(&mut owner).map_err(debug_error)?;
    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        demux_endpoint_config(), None, Arc::new(adapter),
        quinn::default_runtime().ok_or("QUIC runtime unavailable")?,
    ).map_err(debug_error)?;
    endpoint.set_default_client_config(tls);
    let control = endpoint.connect(nominated.remote, "localhost")
        .map_err(debug_error)?.await.map_err(debug_error)?;
    let data = endpoint.connect(nominated.remote, "localhost")
        .map_err(debug_error)?.await.map_err(debug_error)?;
    let verified = establish_initiator(
        control, data, &pairing, &confirmation, now_secs()?,
        Duration::from_secs(10),
    ).await.map_err(debug_error)?;
    let session = Arc::new(verified);
    let data_lanes = Arc::new(ResilientDataLanes::start_creator(
        &session, endpoint.clone(), nominated.remote, &pairing, MAX_DATA_LANES,
    ).map_err(debug_error)?);
    Ok(ActivePeer {
        session, data_lanes, peer_address: nominated.remote,
        local_address: local, role: "创建方", _owner: owner, _endpoint: endpoint,
    })
}

async fn join_connection(
    addresses: &[SocketAddr], stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(debug_error)?;
    let certificate = identity.cert.der().clone();
    let code = prompt("请粘贴创建方发送的 INVITE 邀请码：")?;
    let _ = manual_ice_v2::inspect_invite_with_certificate(&code, now_secs()?)
        .map_err(debug_error)?;
    let gathered = gather_interfaces(
        addresses, stun, IceRole::Controlled, Duration::from_secs(3),
    ).await.map_err(debug_error)?;
    let answer = gathered.combined.clone();
    let (reply, pairing, remote, remote_cert) =
        manual_ice_v2::respond_with_certificate(
            &code, now_secs()?, certificate.as_ref(), &answer,
        ).map_err(debug_error)?;
    println!("把下面的 REPLY 回传码私下发给创建方：\n{reply}");
    let confirmation = verify_user(&pairing)?;
    let selected = gathered.nominate_first(&remote, Duration::from_secs(30))
        .await.map_err(debug_error)?;
    let mut owner = selected.owner;
    let nominated = selected.path;
    let local = owner.handle.local_address();
    println!("直连 ICE 提名：{} → {}，不使用服务器中继", nominated.local, nominated.remote);
    let tls = authenticated_server_config(
        vec![certificate], rustls::pki_types::PrivateKeyDer::Pkcs8(
            identity.signing_key.serialize_der().into()
        ), trust_peer_der(remote_cert)?,
    ).map_err(debug_error)?;
    let adapter = QuinnUdpAdapter::from_owner(&mut owner).map_err(debug_error)?;
    let endpoint = quinn::Endpoint::new_with_abstract_socket(
        demux_endpoint_config(), Some(tls), Arc::new(adapter),
        quinn::default_runtime().ok_or("QUIC runtime unavailable")?,
    ).map_err(debug_error)?;
    // Each QUIC connection is independently authenticated, not only Streams.
    let control = tokio::time::timeout(Duration::from_secs(45), endpoint.accept())
        .await.map_err(debug_error)?
        .ok_or("等待 Control QUIC 连接超时")?
        .await.map_err(debug_error)?;
    let data = tokio::time::timeout(Duration::from_secs(45), endpoint.accept())
        .await.map_err(debug_error)?
        .ok_or("等待 Data QUIC 连接超时")?
        .await.map_err(debug_error)?;
    // The replay window belongs to the entire long-lived session, not one
    // short-lived Data lane. Preserve it across replacements.
    let guard = Arc::new(ReplayGuard::new(4096).map_err(debug_error)?);
    let verified = establish_responder(
        control, data, &pairing, &confirmation, &guard, now_secs()?,
        Duration::from_secs(10),
    ).await.map_err(debug_error)?;
    let session = Arc::new(verified);
    let data_lanes = Arc::new(ResilientDataLanes::start_joiner(
        &session, endpoint.clone(), &pairing, Arc::clone(&guard), MAX_DATA_LANES,
    ).map_err(debug_error)?);
    Ok(ActivePeer {
        session, data_lanes, peer_address: nominated.remote,
        local_address: local, role: "加入方", _owner: owner, _endpoint: endpoint,
    })
}
