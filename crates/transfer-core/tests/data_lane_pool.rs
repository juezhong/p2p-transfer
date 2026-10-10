//! Full localhost integration of manual ICE v2, standard nominated path,
//! mutual TLS peer certs, human confirmation and independent authenticated
//! Control/Data QUIC sessions. This is a private CI test, not a user CLI.

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    use p2p_sdk::{
        ice_gather::gather,
        ice_multi::nominate_direct_candidates,
        ice_signaling::IceRole,
        manual_ice_v2,
        peer_pin::{ManualConfirmation, PeerCertificatePin},
        quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
        session_binding::ReplayGuard,
        tls_identity::{authenticated_client_config, authenticated_server_config},
        udp_owner::UdpOwner,
        verified_session::{establish_initiator, establish_responder},
    };

    use transfer_core::data_lane_pool::{ResilientDataLanes, LanePoolError};

    #[tokio::test]
    async fn manual_code_to_real_authenticated_dual_quic_stream_echo_on_same_ice_socket() {
        tokio::time::timeout(Duration::from_secs(45), async {
            let client_identity =
                rcgen::generate_simple_self_signed(vec!["client.local".into()]).unwrap();
            let server_identity =
                rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
            let client_cert = client_identity.cert.der().clone();
            let server_cert = server_identity.cert.der().clone();
            let client_pin = PeerCertificatePin::from_certificate_der(client_cert.as_ref())
                .unwrap().fingerprint();
            let server_pin = PeerCertificatePin::from_certificate_der(server_cert.as_ref())
                .unwrap().fingerprint();

            // User's manual codes contain complete ICE descriptions from the
            // SAME UDP owner that will transport Quinn traffic later.
            let mut client_owner = UdpOwner::bind(
                "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            ).await.unwrap();
            let mut server_owner = UdpOwner::bind(
                "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            ).await.unwrap();
            let client_ip_port = client_owner.handle.local_address();
            let server_ip_port = server_owner.handle.local_address();
            let client_ice = gather(
                &client_owner.handle, &[], IceRole::Controlling, Duration::from_secs(1)
            ).await.unwrap().description;
            let server_ice = gather(
                &server_owner.handle, &[], IceRole::Controlled, Duration::from_secs(1)
            ).await.unwrap().description;

            let (pending, invite) = manual_ice_v2::invite_with_certificate(
                100, 600, client_cert.as_ref(), &client_ice,
            ).unwrap();
            let (reply, server_pairing, exchanged_client_ice, peer_client_cert) =
                manual_ice_v2::respond_with_certificate(
                    &invite, 101, server_cert.as_ref(), &server_ice,
                ).unwrap();
            let (client_pairing, exchanged_server_ice, peer_server_cert) =
                pending.finish_with_certificate(&reply, 102).unwrap();
            assert_eq!(client_pairing.remote_tls_cert_sha256, server_pin);
            assert_eq!(server_pairing.remote_tls_cert_sha256, client_pin);
            assert_eq!(exchanged_client_ice, client_ice);
            assert_eq!(exchanged_server_ice, server_ice);
            assert_eq!(client_pairing.comparison_code, server_pairing.comparison_code);

            let mut client_confirmation = ManualConfirmation::new(
                client_pairing.credentials.session_id(), client_pairing.comparison_code
            ).unwrap();
            let mut server_confirmation = ManualConfirmation::new(
                server_pairing.credentials.session_id(), server_pairing.comparison_code
            ).unwrap();
            // Test models explicit human verification. In a real app, this
            // must be a separate trusted user interaction, NEVER automatic.
            let shared_code = client_pairing.comparison_code_text();
            client_confirmation.confirm(&shared_code).unwrap();
            server_confirmation.confirm(&shared_code).unwrap();

            let (client_path, server_path) = tokio::join!(
                nominate_direct_candidates(
                    &mut client_owner, &client_ice, &exchanged_server_ice,
                    Duration::from_secs(5),
                ),
                nominate_direct_candidates(
                    &mut server_owner, &server_ice, &exchanged_client_ice,
                    Duration::from_secs(5),
                ),
            );
            assert_eq!(client_path.unwrap().remote, server_ip_port);
            assert_eq!(server_path.unwrap().remote, client_ip_port);

            let mut client_roots = rustls::RootCertStore::empty();
            client_roots.add(peer_server_cert.into()).unwrap();
            let mut server_roots = rustls::RootCertStore::empty();
            server_roots.add(peer_client_cert.into()).unwrap();

            let server_cfg = authenticated_server_config(
                vec![server_cert],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    server_identity.signing_key.serialize_der().into()
                ),
                Arc::new(server_roots),
            ).unwrap();
            let client_cfg = authenticated_client_config(
                vec![client_cert],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    client_identity.signing_key.serialize_der().into()
                ),
                Arc::new(client_roots),
            ).unwrap();

            // No fresh UDP binds: both endpoints retain nominated ICE ports.
            let server_socket = QuinnUdpAdapter::from_owner(&mut server_owner).unwrap();
            let server = quinn::Endpoint::new_with_abstract_socket(
                demux_endpoint_config(), Some(server_cfg), Arc::new(server_socket),
                quinn::default_runtime().unwrap(),
            ).unwrap();
            let client_socket = QuinnUdpAdapter::from_owner(&mut client_owner).unwrap();
            let mut client = quinn::Endpoint::new_with_abstract_socket(
                demux_endpoint_config(), None, Arc::new(client_socket),
                quinn::default_runtime().unwrap(),
            ).unwrap();
            client.set_default_client_config(client_cfg.clone());
            assert_eq!(client.local_addr().unwrap(), client_ip_port);
            assert_eq!(server.local_addr().unwrap(), server_ip_port);

            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let server_task = tokio::spawn(async move {
                let control = server.accept().await.unwrap().await.unwrap();
                let data = server.accept().await.unwrap().await.unwrap();
                let guard = Arc::new(ReplayGuard::new(128).unwrap());
                let secure = establish_responder(
                    control, data, &server_pairing, &server_confirmation,
                    &guard, 103, Duration::from_secs(5),
                ).await.expect("responder verified manual session");
                assert_ne!(secure.control().stable_id(), secure.data().stable_id());
                let pool = ResilientDataLanes::start_joiner(
                    &secure, server.clone(), &server_pairing, Arc::clone(&guard), 4,
                ).unwrap();

                let (mut response, mut request) = secure.control().accept_bi().await.unwrap();
                assert_eq!(request.read_to_end(64).await.unwrap(), b"control message");
                response.write_all(b"control ack").await.unwrap();
                response.finish().unwrap();

                let (mut response, mut request) = secure.data().accept_bi().await.unwrap();
                assert_eq!(request.read_to_end(64).await.unwrap(), b"data message");
                response.write_all(b"data ack").await.unwrap();
                response.finish().unwrap();

                // All four independent Data QUICs have completed TLS and
                // per-connection HMAC proof before the initial lane fails.
                let active = pool.wait_for_count(4, Duration::from_secs(8)).await.unwrap();
                // Four independently authenticated data connections must
                // arrive from four distinct client UDP source ports, while
                // the Control QUIC remains on its original source port.
                let sources = active.iter().map(|conn| conn.remote_address())
                    .collect::<std::collections::HashSet<_>>();
                assert_eq!(sources.len(), 4);
                // Initial Data shares Control's validated UDP socket; three
                // additional lanes use independent bound source ports.
                assert!(sources.contains(&secure.control().remote_address()));
                secure.data().closed().await;
                let replacement = pool.wait_for_count(4, Duration::from_secs(8))
                    .await.unwrap().into_iter().next().unwrap();
                assert_ne!(replacement.stable_id(), secure.data().stable_id());
                let (mut response, mut request) = replacement.accept_bi().await.unwrap();
                assert_eq!(request.read_to_end(64).await.unwrap(), b"repaired data");
                response.write_all(b"repaired ack").await.unwrap();
                response.finish().unwrap();
                let mut next_stream = pool.accept_uni(Duration::from_secs(8))
                    .await.expect("receive from authenticated repaired pool");
                assert_eq!(next_stream.read_to_end(64).await.unwrap(), b"pool data");

                let (mut response, mut request) = secure.control().accept_bi().await.unwrap();
                assert_eq!(request.read_to_end(64).await.unwrap(), b"control after data close");
                response.write_all(b"still alive").await.unwrap();
                response.finish().unwrap();

                done_rx.await.unwrap();
                assert!(secure.control().close_reason().is_none());
                server.close(0u32.into(), b"completed");
            });

            let control = client.connect(server_ip_port, "localhost").unwrap()
                .await.unwrap();
            let data = client.connect(server_ip_port, "localhost").unwrap()
                .await.unwrap();
            let secure = establish_initiator(
                control, data, &client_pairing, &client_confirmation,
                103, Duration::from_secs(5),
            ).await.expect("initiator verified manual session");
            assert_ne!(secure.control().stable_id(), secure.data().stable_id());
            let pool = ResilientDataLanes::start_creator_with_independent_udp(
                &secure, client.clone(), server_ip_port, &client_pairing,
                client_cfg, 4,
            ).unwrap();
            assert_eq!(pool.wait_for_count(4, Duration::from_secs(8)).await.unwrap().len(), 4);

            async fn rpc(conn: &quinn::Connection, payload: &[u8], expected: &[u8]) {
                let (mut send, mut receive) = conn.open_bi().await.unwrap();
                send.write_all(payload).await.unwrap();
                send.finish().unwrap();
                assert_eq!(receive.read_to_end(64).await.unwrap(), expected);
            }
            rpc(secure.control(), b"control message", b"control ack").await;
            rpc(secure.data(), b"data message", b"data ack").await;
            let mut monitor = secure.watch_link_termination();
            secure.close_data();
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), monitor.recv()).await.unwrap(),
                Some(p2p_sdk::verified_session::SessionLinkEvent::DataDisconnected),
            );
            secure.data().closed().await;
            // Pool must replace the exact dead lane, not merely count stale
            // connections, without disturbing control or other Data lanes.
            let recovered = pool.wait_for_count(4, Duration::from_secs(8))
                .await.unwrap();
            assert!(!recovered.iter().any(|c| c.stable_id() == secure.data().stable_id()));
            rpc(&recovered[0], b"repaired data", b"repaired ack").await;
            let mut outgoing = pool.open_uni(Duration::from_secs(8))
                .await.expect("open stream from authenticated 4-lane pool");
            outgoing.write_all(b"pool data").await.unwrap();
            outgoing.finish().unwrap();
            assert!(monitor.try_recv().is_err());
            rpc(secure.control(), b"control after data close", b"still alive").await;
            assert!(secure.control().close_reason().is_none());
            done_tx.send(()).unwrap();
            server_task.await.unwrap();
            secure.control().closed().await;
            // The pool must fail closed immediately once Control is gone:
            // even previously healthy Data connections are not permission
            // to keep transferring outside the verified session.
            assert!(matches!(
                pool.wait_for_count(1, Duration::from_secs(1)).await,
                Err(LanePoolError::ShuttingDown),
            ));
            assert!(matches!(
                pool.open_uni(Duration::from_secs(1)).await,
                Err(LanePoolError::ShuttingDown),
            ));
            client.close(0u32.into(), b"completed");
        }).await.expect("full manual-to-QUIC secure session test timeout");
    }
}
