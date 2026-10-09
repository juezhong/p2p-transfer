//! Experimental manual direct P2P file transfer CLI.
//!
//! Both parties use one INVITE / one REPLY and separately verify their
//! six-digit comparison code; no signaling server or file relay is used.
//! The SDK exclusively owns ICE, UDP, TLS and Control/Data QUIC connections.
//! Not yet Go-parity: no recursive directory transfer, resume or multi-lane
//! streaming windows. Do not call this a stable release.

use std::{
    env,
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use p2p_sdk::{
    ice_gather::gather,
    ice_multi::nominate_direct_candidates,
    ice_signaling::IceRole,
    manual_ice_v2,
    manual_pairing::ManualPairing,
    peer_pin::ManualConfirmation,
    quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
    session_binding::ReplayGuard,
    tls_identity::{authenticated_client_config, authenticated_server_config},
    udp_owner::UdpOwner,
    verified_session::{establish_initiator, establish_responder},
};
use transfer_core::{
    sdk_quic::{receive_via_sdk, send_via_sdk},
    secure_io::SharedRoot,
};

type CliResult<T> = Result<T, String>;

fn debug_error<E: std::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

fn now_secs() -> CliResult<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)
        .map_err(debug_error)?.as_secs())
}

fn prompt(label: &str) -> CliResult<String> {
    print!("{label}");
    io::stdout().flush().map_err(debug_error)?;
    let mut buf = String::new();
    if io::stdin().read_line(&mut buf).map_err(debug_error)? == 0 {
        return Err("stdin closed before input was supplied".into());
    }
    Ok(buf.trim().to_owned())
}

fn confirm(pairing: &ManualPairing) -> CliResult<ManualConfirmation> {
    let mut gate = ManualConfirmation::new(
        pairing.credentials.session_id(), pairing.comparison_code,
    ).map_err(debug_error)?;
    println!("Your six-digit comparison code: {}", gate.comparison_code_text());
    println!("Compare it through a trusted independent channel with the other person.");
    let seen = prompt("Enter the six-digit code independently confirmed on the OTHER device: ")?;
    gate.confirm(&seen).map_err(|_| "pairing comparison rejected; aborting".to_owned())?;
    Ok(gate)
}

fn parse_bind(s: &str) -> CliResult<SocketAddr> {
    let addr: SocketAddr = s.parse().map_err(|_| "expected explicit local IP:port".to_owned())?;
    if addr.ip().is_unspecified() || addr.ip().is_multicast() {
        return Err("wildcard/multicast bind not supported yet; choose a real LAN IP".into());
    }
    Ok(addr)
}

fn parse_stun(args: &[String]) -> CliResult<Vec<SocketAddr>> {
    if args.len() > 8 { return Err("at most 8 STUN servers".into()); }
    args.iter().map(|s| s.parse().map_err(|_| format!("invalid STUN IP:port: {s}"))).collect()
}

fn trust_peer_der(bytes: Vec<u8>) -> CliResult<Arc<rustls::RootCertStore>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(bytes.into()).map_err(debug_error)?;
    Ok(Arc::new(roots))
}

fn usage() {
    eprintln!("Experimental p2p-transfer manual P2P CLI (not stable / no relay)\n\
        Send:    p2p-transfer send <LOCAL_IP:PORT> <SOURCE_ROOT> <SOURCE_RELATIVE> <REMOTE_RELATIVE> [STUN_IP:PORT ...]\n\
        Receive: p2p-transfer receive <LOCAL_IP:PORT> <DESTINATION_ROOT> [STUN_IP:PORT ...]\n\
        Both sides MUST choose their real LAN interface IP (no 0.0.0.0 yet).\n\
        LAN can run without STUN; for WAN specify an accessible STUN IPv4/IPv6 IP.\n\
        Never share pairing codes publicly: they include ICE credentials and IPs.");
}

// Windows' default 1 MiB main thread stack is insufficient for the
// combined ICE/QUIC handshake futures. Run the SDK on a dedicated thread
// with an explicit 16 MiB stack, without changing security or I/O behavior.
fn main() {
    let worker = std::thread::Builder::new()
        .name("p2p-transfer-network".to_owned())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Tokio runtime initialization failed");
            runtime.block_on(async_main());
        })
        .expect("cannot start Transfer network thread");
    worker.join().expect("Transfer network thread panicked");
}

async fn async_main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let action = args.first().map(String::as_str);
    let result = match action {
        Some("send") if args.len() >= 5 => {
            async {
                let bind = parse_bind(&args[1])?;
                let root = PathBuf::from(&args[2]);
                let source = PathBuf::from(&args[3]);
                let destination = args[4].clone();
                let stun = parse_stun(&args[5..])?;
                send(bind, &root, &source, &destination, &stun).await
            }.await
        }
        Some("receive") if args.len() >= 3 => {
            async {
                let bind = parse_bind(&args[1])?;
                let root = PathBuf::from(&args[2]);
                let stun = parse_stun(&args[3..])?;
                receive(bind, &root, &stun).await
            }.await
        }
        _ => { usage(); std::process::exit(2) }
    };
    match result {
        Ok(()) => println!("Transfer completed and verified."),
        Err(err) => {
            eprintln!("Transfer failed: {err}");
            std::process::exit(1);
        }
    }
}

/// First side of the manual exchange: produce INVITE, read REPLY,
/// verify the peer certificate and negotiate direct ICE pair.
async fn send(
    bind: SocketAddr, root: &Path, source: &Path,
    remote_destination: &str, stun: &[SocketAddr],
) -> CliResult<()> {
    let shared = SharedRoot::authorize(root).map_err(debug_error)?;
    let own = rcgen::generate_simple_self_signed(vec!["client.local".into()])
        .map_err(debug_error)?;
    let cert = own.cert.der().clone();
    let mut owner = UdpOwner::bind(bind).await.map_err(debug_error)?;
    let local_ice = gather(
        &owner.handle, stun, IceRole::Controlling, Duration::from_secs(3),
    ).await.map_err(debug_error)?.description;
    let (pending, invite) = manual_ice_v2::invite_with_certificate(
        now_secs()?, 1200, cert.as_ref(), &local_ice,
    ).map_err(debug_error)?;
    println!("Send this INVITE privately to the receiving device:");
    println!("{invite}");
    let reply = prompt("Paste receiving device REPLY: ")?;
    let (pairing, remote_ice, server_cert) = pending
        .finish_with_certificate(&reply, now_secs()?)
        .map_err(debug_error)?;
    let confirmation = confirm(&pairing)?;
    println!("Checking direct ICE candidates, with LAN paths preferred...");
    let nominated = nominate_direct_candidates(
        &mut owner, &local_ice, &remote_ice, Duration::from_secs(20),
    ).await.map_err(debug_error)?;
    println!("ICE nominated local={} remote={} (no relay)",
        nominated.local, nominated.remote);

    let tls = authenticated_client_config(
        vec![cert], rustls::pki_types::PrivateKeyDer::Pkcs8(
            own.signing_key.serialize_der().into(),
        ), trust_peer_der(server_cert)?,
    ).map_err(debug_error)?;
    let socket = QuinnUdpAdapter::from_owner(&mut owner).map_err(debug_error)?;
    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        demux_endpoint_config(), None, Arc::new(socket),
        quinn::default_runtime().ok_or("Tokio QUIC runtime unavailable")?,
    ).map_err(debug_error)?;
    endpoint.set_default_client_config(tls);
    // The server's SAN is `localhost` even when its verified ICE address
    // is a private or public IP. Its certificate is explicitly pinned.
    let control = endpoint.connect(nominated.remote, "localhost")
        .map_err(debug_error)?.await.map_err(debug_error)?;
    let data = endpoint.connect(nominated.remote, "localhost")
        .map_err(debug_error)?.await.map_err(debug_error)?;
    let session = establish_initiator(
        control, data, &pairing, &confirmation, now_secs()?,
        Duration::from_secs(10),
    ).await.map_err(debug_error)?;
    println!("Peer authenticated; sending file over independent Data QUIC...");
    let receipt = send_via_sdk(
        &session, &shared, source, remote_destination, 1,
    ).await.map_err(debug_error)?;
    println!("Receiver committed {} bytes (SHA-256 verified)", receipt.bytes);
    session.control().close(0u32.into(), b"transfer acknowledged");
    Ok(())
}

/// Second side: paste INVITE, generate REPLY and wait for the authenticated
/// QUIC Control/Data connections on the exact same nominated UDP socket.
async fn receive(bind: SocketAddr, root: &Path, stun: &[SocketAddr]) -> CliResult<()> {
    let shared = SharedRoot::authorize(root).map_err(debug_error)?;
    let own = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(debug_error)?;
    let cert = own.cert.der().clone();
    let mut owner = UdpOwner::bind(bind).await.map_err(debug_error)?;
    let invite = prompt("Paste sender's INVITE: ")?;
    let _ = manual_ice_v2::inspect_invite_with_certificate(&invite, now_secs()?)
        .map_err(debug_error)?;
    let local_ice = gather(
        &owner.handle, stun, IceRole::Controlled, Duration::from_secs(3),
    ).await.map_err(debug_error)?.description;
    let (reply, pairing, remote_ice, client_cert) =
        manual_ice_v2::respond_with_certificate(
            &invite, now_secs()?, cert.as_ref(), &local_ice,
        ).map_err(debug_error)?;
    println!("Send this REPLY privately to the sender:");
    println!("{reply}");
    let confirmation = confirm(&pairing)?;
    println!("Checking direct ICE candidates, with LAN paths preferred...");
    let nominated = nominate_direct_candidates(
        &mut owner, &local_ice, &remote_ice, Duration::from_secs(20),
    ).await.map_err(debug_error)?;
    println!("ICE nominated local={} remote={} (no relay)",
        nominated.local, nominated.remote);

    let cfg = authenticated_server_config(
        vec![cert], rustls::pki_types::PrivateKeyDer::Pkcs8(
            own.signing_key.serialize_der().into(),
        ), trust_peer_der(client_cert)?,
    ).map_err(debug_error)?;
    let socket = QuinnUdpAdapter::from_owner(&mut owner).map_err(debug_error)?;
    let endpoint = quinn::Endpoint::new_with_abstract_socket(
        demux_endpoint_config(), Some(cfg), Arc::new(socket),
        quinn::default_runtime().ok_or("Tokio QUIC runtime unavailable")?,
    ).map_err(debug_error)?;
    let control = tokio::time::timeout(Duration::from_secs(30), endpoint.accept())
        .await.map_err(debug_error)?
        .ok_or("timed out waiting for first QUIC connection")?
        .await.map_err(debug_error)?;
    let data = tokio::time::timeout(Duration::from_secs(30), endpoint.accept())
        .await.map_err(debug_error)?
        .ok_or("timed out waiting for second QUIC connection")?
        .await.map_err(debug_error)?;
    let replay = ReplayGuard::new(16).map_err(debug_error)?;
    let session = establish_responder(
        control, data, &pairing, &confirmation, &replay, now_secs()?,
        Duration::from_secs(10),
    ).await.map_err(debug_error)?;
    println!("Peer authenticated; receiving into the explicitly authorized root...");
    let receipt = receive_via_sdk(&session, &shared)
        .await.map_err(debug_error)?;
    println!("Verified and committed {} bytes; SHA-256 OK", receipt.bytes);
    // Do not close the Endpoint before the sender has read final DONE.
    let _ = tokio::time::timeout(Duration::from_secs(10), session.control().closed()).await;
    Ok(())
}
