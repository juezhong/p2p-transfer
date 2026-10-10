//! SDK 高层配对和 Transfer 自有四路 Data 的真实 localhost 集成测试。
//! 本测试验证两进程等价的握手与文件 I/O，不冒充公网复杂 NAT 验收。

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};
use p2p_sdk::{begin_creator, begin_joiner};
use transfer_core::{
    sdk_quic::{
        receive_via_sdk, send_via_sdk, serve_control_stream, list_via_sdk,
        request_get_via_sdk, IncomingResult,
    },
    secure_io::SharedRoot,
    transport_lanes::{TransferDataLanes, TransferSession},
};

#[test]
fn real_sdk_manual_pairing_ice_mtls_dual_quic_transfers_file_to_disk() {
    std::thread::Builder::new().name("sdk-transfer-integration".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all().build().unwrap();
            runtime.block_on(async {
                tokio::time::timeout(Duration::from_secs(80), async {
                    let mut id = [0u8; 12];
                    getrandom::fill(&mut id).unwrap();
                    let temp = std::env::temp_dir().join(format!(
                        "p2p-quic-test-{}-{:?}", std::process::id(), id,
                    ));
                    std::fs::create_dir_all(temp.join("source")).unwrap();
                    std::fs::create_dir_all(temp.join("receiver")).unwrap();
                    let content = vec![77u8; 280_001];
                    std::fs::write(temp.join("source/文件.bin"), &content).unwrap();
                    let src = SharedRoot::authorize(&temp.join("source")).unwrap();
                    let dst = SharedRoot::authorize(&temp.join("receiver")).unwrap();

                    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
                    let (pending, invite) = begin_creator(&[addr], &[], 100, 600)
                        .await.unwrap();
                    let (joiner, reply) = begin_joiner(&invite, &[addr], &[], 101)
                        .await.unwrap();
                    let creator = pending.receive_reply(&reply, 102).unwrap();

                    // 人工核对步骤由两端分别执行，SDK 不默许未经确认的配对。
                    let code = creator.comparison_code();
                    assert_eq!(code, joiner.comparison_code());
                    let mut cc = creator.confirmation().unwrap();
                    let mut jc = joiner.confirmation().unwrap();
                    cc.confirm(&code).unwrap();
                    jc.confirm(&code).unwrap();

                    let (left, right) = tokio::join!(
                        creator.connect_transport(&cc, 103),
                        joiner.connect_transport(&jc, 103),
                    );
                    let left = Arc::new(left.unwrap());
                    let right = Arc::new(right.unwrap());
                    let left_lanes = Arc::new(TransferDataLanes::new(&left, 4));
                    let right_lanes = Arc::new(TransferDataLanes::new(&right, 4));
                    let left_session = Arc::new(TransferSession::new(
                        Arc::clone(&left), Arc::clone(&left_lanes),
                    ));
                    let right_session = Arc::new(TransferSession::new(
                        Arc::clone(&right), Arc::clone(&right_lanes),
                    ));

                    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
                    let responder = tokio::spawn(async move {
                        let receipt = receive_via_sdk(&right_session, &dst).await.unwrap();
                        let (tx, rx) = right_session.control().accept_bi().await.unwrap();
                        assert!(matches!(
                            serve_control_stream(&right_session, &dst, tx, rx).await.unwrap(),
                            IncomingResult::DirectoryListed,
                        ));
                        let (tx, rx) = right_session.control().accept_bi().await.unwrap();
                        assert!(matches!(
                            serve_control_stream(&right_session, &dst, tx, rx).await.unwrap(),
                            IncomingResult::ServedGet(_),
                        ));
                        done_rx.await.unwrap();
                        right_lanes.shutdown().await;
                        receipt
                    });
                    let sent = send_via_sdk(
                        &left_session, &src, Path::new("文件.bin"), "收到.bin", 42,
                    ).await.unwrap();
                    assert_eq!(sent.bytes, content.len() as u64);
                    let remote_names = list_via_sdk(&left_session, "".into(), 43)
                        .await.unwrap();
                    assert!(remote_names.iter().any(|name| name == "收到.bin"));
                    request_get_via_sdk(&left_session, "收到.bin".into(), "回传.bin".into(), 44)
                        .await.unwrap();
                    let returned = receive_via_sdk(&left_session, &src).await.unwrap();
                    assert_eq!(returned.bytes, content.len() as u64);
                    assert_eq!(std::fs::read(temp.join("source/回传.bin")).unwrap(), content);
                    done_tx.send(()).unwrap();
                    let received = responder.await.unwrap();
                    assert_eq!(sent, received);
                    assert_eq!(std::fs::read(temp.join("receiver/收到.bin")).unwrap(), content);
                    left_lanes.shutdown().await;
                    left.control.close(0u32.into(), b"test complete");
                    drop(src);
                    std::fs::remove_dir_all(temp).unwrap();
                }).await.expect("SDK-connected Transfer loopback timed out");
            });
        }).expect("spawn test thread").join().expect("integration test panicked");
}
