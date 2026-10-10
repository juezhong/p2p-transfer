//! Transfer-core integration against SDK's current high-level network session.
//! Unlike the historical manually assembled ICE+QUIC test, this exercises
//! JOIN passive negotiation, Creator-selected Control and authenticated
//! on-demand Data QUIC. Local loopback is NOT public NAT validation.

use std::{path::Path, sync::Arc, time::Duration};

use p2p_sdk::{direct_peer::{begin_creator, begin_joiner}, transport_session::ConnectedTransportPeer};
use transfer_core::{
    modern_data_lanes::ModernDataLanes,
    sdk_quic::{list_via_sdk, serve_control_stream, send_via_managed_sdk,
        serve_control_stream_with_lease, ControlIo, IncomingResult},
    secure_io::SharedRoot,
    stream_transfer::{receive_file, send_file, TransferReceipt},
};

async fn send_one(
    peers: (&ConnectedTransportPeer, &ConnectedTransportPeer),
    lanes: (&quinn::Connection, &quinn::Connection),
    roots: (&SharedRoot, &SharedRoot),
    name: &str, remote_name: &str, request: u64,
) -> (TransferReceipt, TransferReceipt) {
    let (sender, receiver) = peers;
    let (send_lane, recv_lane) = lanes;
    let (source_root, destination_root) = roots;
    tokio::join!(
        async {
            let (tx, rx) = sender.control.open_bi().await.unwrap();
            let mut control = ControlIo::new(rx, tx);
            let mut data = send_lane.open_uni().await.unwrap();
            // QUIC open_uni() alone does not send a STREAM frame.
            // Match Transfer's real P2PD + request-ID preface BEFORE
            // send_file waits for the receiver's initial Control ACK.
            let mut preface = [0u8; 12];
            preface[..4].copy_from_slice(b"P2PD");
            preface[4..].copy_from_slice(&request.to_be_bytes());
            data.write_all(&preface).await.unwrap();
            let result = send_file(
                &mut control, &mut data, source_root,
                Path::new(name), remote_name, request,
            ).await.unwrap();
            data.finish().unwrap();
            result
        },
        async {
            let (tx, rx) = receiver.control.accept_bi().await.unwrap();
            let mut control = ControlIo::new(rx, tx);
            let mut data = recv_lane.accept_uni().await.unwrap();
            let mut preface = [0u8; 12];
            data.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface[..4], b"P2PD", "Data stream must be tagged");
            assert_eq!(
                u64::from_be_bytes(preface[4..].try_into().unwrap()), request,
                "Data stream must match the authenticated Control request"
            );
            receive_file(&mut control, &mut data, destination_root)
                .await.unwrap()
        }
    )
}

#[test]
fn sdk_high_level_manual_session_transfers_bidirectional_files() {
    // Actual SDK ICE+Quinn futures need more stack than Rust's default test
    // harness thread on some native targets.
    std::thread::Builder::new()
        .name("modern-sdk-file-transfer".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(90), async {
                    let mut entropy = [0u8; 12];
                    getrandom::fill(&mut entropy).unwrap();
                    let dir = std::env::temp_dir().join(format!(
                        "transfer-modern-sdk-{}-{:?}", std::process::id(), entropy,
                    ));
                    std::fs::create_dir_all(dir.join("A")).unwrap();
                    std::fs::create_dir_all(dir.join("B")).unwrap();
                    let source = SharedRoot::authorize(&dir.join("A")).unwrap();
                    let destination = SharedRoot::authorize(&dir.join("B")).unwrap();
                    let payload = "安全的双向 QUIC 文件传输。".repeat(14000);
                    std::fs::write(dir.join("A/中文名.txt"), payload.as_bytes()).unwrap();
                    std::fs::write(dir.join("B/empty.bin"), []).unwrap();

                    let bind = "127.0.0.1:0".parse().unwrap();
                    let now = 1_800_000_000u64;
                    let (pending, invite) = begin_creator(&[bind], &[], now, 1200)
                        .await.unwrap();
                    let (joiner, reply) = begin_joiner(&invite, &[bind], &[], now)
                        .await.unwrap();
                    let creator = pending.receive_reply(&reply, now).unwrap();
                    let mut creator_ok = creator.confirmation().unwrap();
                    let mut joiner_ok = joiner.confirmation().unwrap();
                    assert_eq!(creator.comparison_code(), joiner.comparison_code());
                    creator_ok.confirm(&creator.comparison_code()).unwrap();
                    joiner_ok.confirm(&joiner.comparison_code()).unwrap();

                    let (creator, joiner) = tokio::join!(
                        creator.connect_transport(&creator_ok, now),
                        joiner.connect_transport(&joiner_ok, now),
                    );
                    let creator = Arc::new(creator.unwrap());
                    let joiner = Arc::new(joiner.unwrap());
                    let creator_diagnostic = creator.diagnostic();
                    let joiner_diagnostic = joiner.diagnostic();
                    assert!(creator_diagnostic.control_connected);
                    assert!(joiner_diagnostic.control_connected);
                    assert_eq!(
                        creator_diagnostic.actual_local_udp,
                        joiner_diagnostic.actual_remote_udp,
                    );
                    assert_eq!(
                        joiner_diagnostic.actual_local_udp,
                        creator_diagnostic.actual_remote_udp,
                    );
                    assert_eq!(
                        creator_diagnostic.control_outbound,
                        !joiner_diagnostic.control_outbound,
                    );

                    // Application, not SDK, owns the number of Data lanes.
                    let creator_pool = ModernDataLanes::start(Arc::clone(&creator), 1).unwrap();
                    let joiner_pool = ModernDataLanes::start(Arc::clone(&joiner), 1).unwrap();
                    let (creator_data, joiner_data) = tokio::join!(
                        creator_pool.wait_for_count(1, Duration::from_secs(20)),
                        joiner_pool.wait_for_count(1, Duration::from_secs(20)),
                    );
                    let creator_data = creator_data.unwrap().remove(0);
                    let joiner_data = joiner_data.unwrap().remove(0);

                    let (sent, received) = send_one(
                        (&creator, &joiner), (&creator_data, &joiner_data),
                        (&source, &destination), "中文名.txt", "收到 文件.txt", 11,
                    ).await;
                    assert_eq!(sent, received);
                    assert_eq!(sent.bytes, payload.len() as u64);
                    assert_eq!(
                        std::fs::read(dir.join("B/收到 文件.txt")).unwrap(),
                        payload.as_bytes(),
                    );

                    // The same authenticated Control/Data connections also
                    // support reverse-direction GET without re-pairing.
                    let (sent_back, received_back) = send_one(
                        (&joiner, &creator), (&joiner_data, &creator_data),
                        (&destination, &source), "empty.bin", "空文件.bin", 12,
                    ).await;
                    assert_eq!(sent_back, received_back);
                    assert_eq!(sent_back.bytes, 0);
                    assert_eq!(
                        std::fs::metadata(dir.join("A/空文件.bin")).unwrap().len(),
                        0,
                    );

                    // New high-level SDK Control supports the *same* directory
                    // RPC contract as the original SDK VerifiedManualSession.
                    // A sealed authentication boundary prevents raw QUIC from
                    // being accidentally accepted as a verified peer.
                    let (listing, response) = tokio::join!(
                        list_via_sdk(&creator, String::new(), 13),
                        async {
                            let (tx, rx) = joiner.control.accept_bi().await.unwrap();
                            serve_control_stream(&joiner, &destination, tx, rx).await
                        }
                    );
                    assert!(matches!(response.unwrap(), IncomingResult::DirectoryListed));
                    let names = listing.unwrap();
                    assert!(names.iter().any(|name| name == "收到 文件.txt"));
                    assert!(names.iter().any(|name| name == "empty.bin"));

                    // Test the production Transfer code path with SDK-managed,
                    // independently authenticated Data streams and a real
                    // Control RPC dispatcher (not raw test streams).
                    let (sent_again, incoming) = tokio::join!(
                        send_via_managed_sdk(
                            &creator, &creator_pool, &source, Path::new("中文名.txt"),
                            "第二份.txt", 14,
                        ),
                        async {
                            let (tx, rx) = joiner.control.accept_bi().await.unwrap();
                            serve_control_stream_with_lease(
                                &joiner, &destination, tx, rx, None, Some(&joiner_pool),
                            ).await
                        },
                    );
                    let received_again = match incoming.unwrap() {
                        IncomingResult::Received(receipt) => receipt,
                        _ => panic!("expected a committed file over SDK-managed Data"),
                    };
                    assert_eq!(sent_again.unwrap(), received_again);
                    assert_eq!(
                        std::fs::read(dir.join("B/第二份.txt")).unwrap(),
                        payload.as_bytes(),
                    );

                    creator_pool.shutdown().await;
                    joiner_pool.shutdown().await;
                    drop(creator_pool);
                    drop(joiner_pool);
                    Arc::try_unwrap(creator).ok().unwrap().shutdown().await;
                    Arc::try_unwrap(joiner).ok().unwrap().shutdown().await;
                    drop(source);
                    drop(destination);
                    std::fs::remove_dir_all(&dir).unwrap();
                }).await.expect("real high-level SDK ↔ Transfer PUT/GET timed out");
            });
        }).unwrap().join().unwrap();
}
