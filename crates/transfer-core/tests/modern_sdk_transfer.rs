//! Transfer-core integration against SDK's current high-level network session.
//! Unlike the historical manually assembled ICE+QUIC test, this exercises
//! JOIN passive negotiation, Creator-selected Control and authenticated
//! on-demand Data QUIC. Local loopback is NOT public NAT validation.

use std::{path::Path, sync::Arc, time::Duration};

use p2p_sdk::{direct_peer::{begin_creator, begin_joiner}, transport_session::ConnectedTransportPeer};
use transfer_core::{
    sdk_quic::ControlIo, secure_io::SharedRoot,
    stream_transfer::{receive_file, send_file, TransferReceipt},
};

async fn wait_data(
    managed: &p2p_sdk::transport_session::ManagedAuthenticatedLink,
) -> quinn::Connection {
    let mut changed = managed.subscribe();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(connection) = managed.current() {
                return connection;
            }
            changed.changed().await.expect("managed Data link must not stop");
        }
    }).await.expect("authenticated Data QUIC link timed out")
}

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
                    let creator_managed = creator.manage_authenticated_data();
                    let joiner_managed = joiner.manage_authenticated_data();
                    let (creator_data, joiner_data) = tokio::join!(
                        wait_data(&creator_managed), wait_data(&joiner_managed)
                    );

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

                    creator_managed.shutdown().await;
                    joiner_managed.shutdown().await;
                    Arc::try_unwrap(creator).ok().unwrap().shutdown().await;
                    Arc::try_unwrap(joiner).ok().unwrap().shutdown().await;
                    drop(source);
                    drop(destination);
                    std::fs::remove_dir_all(&dir).unwrap();
                }).await.expect("real high-level SDK ↔ Transfer PUT/GET timed out");
            });
        }).unwrap().join().unwrap();
}
