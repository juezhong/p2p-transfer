//! M2 SDK end-to-end integration on localhost.
//!
//! It actually transfers bytes over standard ICE nominated UDP sockets,
//! mutually authenticated separate Control/Data QUIC connections, then
//! verifies file I/O at the destination. NOT a cross-NAT user test.

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use p2p_sdk::{
    ice_gather::gather,
    ice_multi::nominate_direct_candidates,
    ice_signaling::IceRole,
    manual_ice_v2,
    peer_pin::ManualConfirmation,
    quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
    session_binding::ReplayGuard,
    tls_identity::{authenticated_client_config, authenticated_server_config},
    udp_owner::UdpOwner,
    verified_session::{establish_initiator, establish_responder},
};
use transfer_core::{
    sdk_quic::{receive_via_sdk, send_via_sdk, serve_control_stream, list_via_sdk, request_get_via_sdk, IncomingResult},
    secure_io::SharedRoot,
};

#[tokio::test]
async fn real_sdk_manual_pairing_ice_mtls_dual_quic_transfers_file_to_disk() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).unwrap();
        let temp = std::env::temp_dir().join(format!(
            "p2p-quic-test-{}-{:?}", std::process::id(), id
        ));
        std::fs::create_dir_all(temp.join("source")).unwrap();
        std::fs::create_dir_all(temp.join("receiver")).unwrap();
        let content = vec![77u8; 280_001];
        std::fs::write(temp.join("source/文件.bin"), &content).unwrap();
        let src = SharedRoot::authorize(&temp.join("source")).unwrap();
        let dst = SharedRoot::authorize(&temp.join("receiver")).unwrap();

        let client_identity =
            rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
        let server_identity =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let client_cert = client_identity.cert.der().clone();
        let server_cert = server_identity.cert.der().clone();

        let mut client_owner = UdpOwner::bind(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ).await.unwrap();
        let mut server_owner = UdpOwner::bind(
            "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        ).await.unwrap();
        let client_address = client_owner.handle.local_address();
        let server_address = server_owner.handle.local_address();
        let client_ice = gather(
            &client_owner.handle, &[], IceRole::Controlling, Duration::from_secs(1),
        ).await.unwrap().description;
        let server_ice = gather(
            &server_owner.handle, &[], IceRole::Controlled, Duration::from_secs(1),
        ).await.unwrap().description;

        let (pending, invite) = manual_ice_v2::invite_with_certificate(
            100, 600, client_cert.as_ref(), &client_ice,
        ).unwrap();
        let (reply, server_pairing, exchanged_client, peer_client_cert) =
            manual_ice_v2::respond_with_certificate(
                &invite, 101, server_cert.as_ref(), &server_ice,
            ).unwrap();
        let (client_pairing, exchanged_server, peer_server_cert) =
            pending.finish_with_certificate(&reply, 102).unwrap();

        // Test explicitly models an independent user comparison. A deployed
        // CLI must never silently confirm a code without real user input.
        assert_eq!(
            client_pairing.comparison_code, server_pairing.comparison_code
        );
        let shared_code = client_pairing.comparison_code_text();
        let mut client_confirmation = ManualConfirmation::new(
            client_pairing.credentials.session_id(), client_pairing.comparison_code,
        ).unwrap();
        let mut server_confirmation = ManualConfirmation::new(
            server_pairing.credentials.session_id(), server_pairing.comparison_code,
        ).unwrap();
        client_confirmation.confirm(&shared_code).unwrap();
        server_confirmation.confirm(&shared_code).unwrap();

        let (client_path, server_path) = tokio::join!(
            nominate_direct_candidates(
                &mut client_owner, &client_ice, &exchanged_server,
                Duration::from_secs(5),
            ),
            nominate_direct_candidates(
                &mut server_owner, &server_ice, &exchanged_client,
                Duration::from_secs(5),
            ),
        );
        assert_eq!(client_path.unwrap().remote, server_address);
        assert_eq!(server_path.unwrap().remote, client_address);

        let mut client_roots = rustls::RootCertStore::empty();
        client_roots.add(peer_server_cert.into()).unwrap();
        let mut server_roots = rustls::RootCertStore::empty();
        server_roots.add(peer_client_cert.into()).unwrap();

        let server_config = authenticated_server_config(
            vec![server_cert],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                server_identity.signing_key.serialize_der().into(),
            ),
            Arc::new(server_roots),
        ).unwrap();
        let client_config = authenticated_client_config(
            vec![client_cert],
            rustls::pki_types::PrivateKeyDer::Pkcs8(
                client_identity.signing_key.serialize_der().into(),
            ),
            Arc::new(client_roots),
        ).unwrap();
        let server_socket = QuinnUdpAdapter::from_owner(&mut server_owner).unwrap();
        let server = quinn::Endpoint::new_with_abstract_socket(
            demux_endpoint_config(), Some(server_config),
            Arc::new(server_socket), quinn::default_runtime().unwrap(),
        ).unwrap();
        let client_socket = QuinnUdpAdapter::from_owner(&mut client_owner).unwrap();
        let mut client = quinn::Endpoint::new_with_abstract_socket(
            demux_endpoint_config(), None,
            Arc::new(client_socket), quinn::default_runtime().unwrap(),
        ).unwrap();
        client.set_default_client_config(client_config);
        assert_eq!(client.local_addr().unwrap(), client_address);
        assert_eq!(server.local_addr().unwrap(), server_address);

        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
        let responder = tokio::spawn(async move {
            let control = server.accept().await.unwrap().await.unwrap();
            let data = server.accept().await.unwrap().await.unwrap();
            let replay_guard = ReplayGuard::new(16).unwrap();
            let session = establish_responder(
                control, data, &server_pairing, &server_confirmation,
                &replay_guard, 103, Duration::from_secs(5),
            ).await.unwrap();
            let receipt = receive_via_sdk(&session, &dst).await.unwrap();

            // The Control QUIC session stays alive and can serve ls + GET.
            let (tx, rx) = session.control().accept_bi().await.unwrap();
            assert!(matches!(
                serve_control_stream(&session, &dst, tx, rx).await.unwrap(),
                IncomingResult::DirectoryListed
            ));
            let (tx, rx) = session.control().accept_bi().await.unwrap();
            assert!(matches!(
                serve_control_stream(&session, &dst, tx, rx).await.unwrap(),
                IncomingResult::ServedGet(_)
            ));
            done_rx.await.unwrap();
            server.close(0u32.into(), b"done");
            receipt
        });
        let control = client.connect(server_address, "localhost").unwrap()
            .await.unwrap();
        let data = client.connect(server_address, "localhost").unwrap()
            .await.unwrap();
        let session = establish_initiator(
            control, data, &client_pairing, &client_confirmation,
            103, Duration::from_secs(5),
        ).await.unwrap();
        let sent = send_via_sdk(
            &session, &src, Path::new("文件.bin"), "收到.bin", 42,
        ).await.unwrap();
        assert_eq!(sent.bytes, content.len() as u64);

        let remote_names = list_via_sdk(&session, "".into(), 43).await.unwrap();
        assert!(remote_names.iter().any(|name| name == "收到.bin"));
        request_get_via_sdk(&session, "收到.bin".into(), "回传.bin".into(), 44)
            .await.unwrap();
        let returned = receive_via_sdk(&session, &src).await.unwrap();
        assert_eq!(returned.bytes, content.len() as u64);
        assert_eq!(std::fs::read(temp.join("source/回传.bin")).unwrap(), content);
        done_tx.send(()).unwrap();
        let received = responder.await.unwrap();
        assert_eq!(sent, received);
        assert_eq!(std::fs::read(temp.join("receiver/收到.bin")).unwrap(), content);
        client.close(0u32.into(), b"done");
        drop(src);
        std::fs::remove_dir_all(temp).unwrap();
    }).await.expect("SDK-integrated real file transfer on localhost timed out");
}
