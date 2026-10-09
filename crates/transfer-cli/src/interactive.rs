//! Go-style, argument-free manual pairing UI; Rust-only new wire protocol.
//! Actual interface discovery, ICE, STUN and mTLS are owned by p2p-sdk.

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use p2p_sdk::{
    ice_gather::gather,
    ice_multi::nominate_direct_candidates,
    ice_signaling::IceRole,
    local_network::default_bind_address,
    manual_ice_v2,
    manual_pairing::ManualPairing,
    peer_pin::ManualConfirmation,
    quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
    session_binding::ReplayGuard,
    tls_identity::{authenticated_client_config, authenticated_server_config},
    udp_owner::UdpOwner,
    verified_session::{establish_initiator, establish_responder, VerifiedManualSession},
};
use transfer_core::secure_io::SharedRoot;

use crate::{debug_error, now_secs, prompt, trust_peer_der, CliResult};

pub struct ActivePeer {
    pub session: Arc<VerifiedManualSession>,
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
    println!("双方独立核对 6 位校验码：{}", confirmation.comparison_code_text());
    let code = prompt("请输入经另一台设备独立核对确认的 6 位校验码：")?;
    confirmation.confirm(&code)
        .map_err(|_| "校验码不匹配，取消连接".to_owned())?;
    Ok(confirmation)
}

async fn default_stun(ipv4: bool) -> Vec<SocketAddr> {
    if std::env::var_os("P2P_TRANSFER_STUN").as_deref() == Some(std::ffi::OsStr::new("off")) {
        return Vec::new();
    }
    // These public servers see the UDP source IP of the STUN query. STUN
    // is optional; an offline LAN may still work without DNS or Internet.
    let hosts = if ipv4 {
        ["stun.l.google.com:19302", "stun1.l.google.com:19302"]
    } else {
        ["stun.l.google.com:19302", "stun1.l.google.com:19302"]
    };
    let mut servers = Vec::new();
    for hostname in hosts {
        if let Ok(Ok(resolved)) = tokio::time::timeout(
            Duration::from_secs(2), tokio::net::lookup_host(hostname)
        ).await {
            if let Some(server) = resolved.into_iter().find(|addr| addr.is_ipv4() == ipv4)
            {
                if !servers.contains(&server) { servers.push(server); }
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
    println!("当前目录：{}", dir.display());
    println!("安全限制：对端只能在该目录下执行已授权的文件读取、目录浏览或写入。");
    if prompt("允许本次会话访问上述共享根目录？输入 yes 确认：")? != "yes" {
        return Err("未授权目录；会话未建立".into());
    }
    let root = Arc::new(SharedRoot::authorize(&dir).map_err(debug_error)?);
    let bind = default_bind_address()
        .map_err(|err| format!("无法自动找到适合的本机网络接口：{err}"))?;
    println!("自动选择本机 UDP 接口 {}（无需手动填写 IP 或端口）", bind.ip());
    let stun = default_stun(bind.is_ipv4()).await;
    println!("STUN 探测服务器数量：{}（仅用于候选收集；局域网不依赖公网服务器）", stun.len());

    let peer = if mode == 1 {
        create_connection(bind, &stun).await?
    } else {
        join_connection(bind, &stun).await?
    };
    println!("已建立安全 P2P 连接。输入 help 查看命令；Ctrl-C 可请求取消活动任务。");
    crate::shell::run(peer, root).await
}

async fn create_connection(
    bind: SocketAddr, stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let identity = rcgen::generate_simple_self_signed(vec!["client.local".into()])
        .map_err(debug_error)?;
    let certificate = identity.cert.der().clone();
    let mut owner = UdpOwner::bind(bind).await.map_err(debug_error)?;
    let local = owner.handle.local_address();
    let offer = gather(
        &owner.handle, stun, IceRole::Controlling, Duration::from_secs(3)
    ).await.map_err(debug_error)?.description;
    let (pending, code) = manual_ice_v2::invite_with_certificate(
        now_secs()?, 1200, certificate.as_ref(), &offer,
    ).map_err(debug_error)?;
    println!("把下面的 INVITE 邀请码私下发给对方：\n{code}");
    let reply = prompt("请粘贴对方返回的 REPLY 回传码：")?;
    let (pairing, remote, remote_cert) =
        pending.finish_with_certificate(&reply, now_secs()?).map_err(debug_error)?;
    let confirmation = verify_user(&pairing)?;
    let nominated = nominate_direct_candidates(
        &mut owner, &offer, &remote, Duration::from_secs(30),
    ).await.map_err(debug_error)?;
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
    Ok(ActivePeer {
        session: Arc::new(verified), peer_address: nominated.remote,
        local_address: local, role: "创建方", _owner: owner, _endpoint: endpoint,
    })
}

async fn join_connection(
    bind: SocketAddr, stun: &[SocketAddr],
) -> CliResult<ActivePeer> {
    let identity = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(debug_error)?;
    let certificate = identity.cert.der().clone();
    let mut owner = UdpOwner::bind(bind).await.map_err(debug_error)?;
    let local = owner.handle.local_address();
    let code = prompt("请粘贴创建方发送的 INVITE 邀请码：")?;
    let _ = manual_ice_v2::inspect_invite_with_certificate(&code, now_secs()?)
        .map_err(debug_error)?;
    let answer = gather(
        &owner.handle, stun, IceRole::Controlled, Duration::from_secs(3),
    ).await.map_err(debug_error)?.description;
    let (reply, pairing, remote, remote_cert) =
        manual_ice_v2::respond_with_certificate(
            &code, now_secs()?, certificate.as_ref(), &answer,
        ).map_err(debug_error)?;
    println!("把下面的 REPLY 回传码私下发给创建方：\n{reply}");
    let confirmation = verify_user(&pairing)?;
    let nominated = nominate_direct_candidates(
        &mut owner, &answer, &remote, Duration::from_secs(30),
    ).await.map_err(debug_error)?;
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
    let guard = ReplayGuard::new(16).map_err(debug_error)?;
    let verified = establish_responder(
        control, data, &pairing, &confirmation, &guard, now_secs()?,
        Duration::from_secs(10),
    ).await.map_err(debug_error)?;
    Ok(ActivePeer {
        session: Arc::new(verified), peer_address: nominated.remote,
        local_address: local, role: "加入方", _owner: owner, _endpoint: endpoint,
    })
}
