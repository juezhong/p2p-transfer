//! Black-box CLI handshake with two real OS processes on localhost.
//! Programmatic comparison-code entry models two human confirmations ONLY
//! in CI; deployed CLI must require real independent human verification.
//! This is NOT a WAN/NAT test.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side { Sender, Receiver }

fn launch(
    exe: &str, args: &[&str], side: Side,
    tx: Sender<(Side, String)>,
) -> Child {
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn transfer CLI");
    let stdout = child.stdout.take().unwrap();
    let tx1 = tx.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            match line {
                Ok(line) => { let _ = tx1.send((side, line)); }
                Err(_) => break,
            }
        }
    });
    let stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            match line {
                Ok(line) => { let _ = tx.send((side, format!("stderr: {line}"))); }
                Err(_) => break,
            }
        }
    });
    child
}

fn send_line(child: &mut Child, line: &str) {
    let stdin = child.stdin.as_mut().expect("stdin available");
    stdin.write_all(line.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn temp_fixture() -> (PathBuf, PathBuf, PathBuf) {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let base = std::env::temp_dir().join(format!(
        "p2p-cli-integration-{}-{stamp}", std::process::id()
    ));
    let source = base.join("source");
    let receiver = base.join("receiver");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&receiver).unwrap();
    (base, source, receiver)
}

#[test]
fn two_cli_processes_exchange_codes_and_transfer_a_real_file_over_ice_quic() {
    let exe = env!("CARGO_BIN_EXE_p2p-transfer");
    let (base, source, receiver) = temp_fixture();
    let payload = "hello from verified QUIC: 文件 ✅".repeat(10_000);
    fs::write(source.join("原始.txt"), payload.as_bytes()).unwrap();
    let (tx, rx): (Sender<(Side, String)>, Receiver<(Side, String)>) = mpsc::channel();

    let mut recv = launch(
        exe, &["receive", "127.0.0.1:0", receiver.to_str().unwrap()],
        Side::Receiver, tx.clone(),
    );
    let mut send = launch(
        exe, &["send", "127.0.0.1:0", source.to_str().unwrap(),
            "原始.txt", "收到.txt"],
        Side::Sender, tx,
    );

    let mut inviter_code: Option<String> = None;
    let mut receiver_reply: Option<String> = None;
    let mut sender_code: Option<String> = None;
    let mut receiver_code: Option<String> = None;
    let mut confirmed = false;
    let mut successes = [false, false];
    let mut transcript = Vec::new();
    let until = Instant::now() + Duration::from_secs(45);
    while !successes[0] || !successes[1] {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = send.kill();
            let _ = recv.kill();
            panic!("CLI test timeout: {:?}", transcript);
        }
        let (side, line) = rx.recv_timeout(left).unwrap_or_else(|_| {
            let _ = send.kill();
            let _ = recv.kill();
            panic!("CLI stopped responding: {:?}", transcript);
        });
        // Do not print full INVITE/REPLY (ICE secrets/IPs) in logs.
        let is_secret = line.starts_with("P2PR-INV2-") || line.starts_with("P2PR-REP2-");
        transcript.push((match side { Side::Sender => "sender", Side::Receiver => "receiver" },
            if is_secret { "[redacted pairing code]".to_owned() } else { line.clone() }));
        if line.starts_with("P2PR-INV2-") && inviter_code.is_none() {
            inviter_code = Some(line.clone());
            send_line(&mut recv, &line);
        }
        if line.starts_with("P2PR-REP2-") && receiver_reply.is_none() {
            receiver_reply = Some(line.clone());
            send_line(&mut send, &line);
        }
        if let Some((_, value)) = line.split_once("Your six-digit comparison code: ") {
            match side {
                Side::Sender => sender_code = Some(value.trim().to_owned()),
                Side::Receiver => receiver_code = Some(value.trim().to_owned()),
            }
        }
        if !confirmed {
            if let (Some(a), Some(b)) = (&sender_code, &receiver_code) {
                assert_eq!(a, b, "handshake comparison mismatch");
                send_line(&mut send, b);
                send_line(&mut recv, a);
                confirmed = true;
            }
        }
        if line.contains("Transfer completed and verified.") {
            successes[if side == Side::Sender { 0 } else { 1 }] = true;
        }
        if line.contains("Transfer failed:") {
            let _ = send.kill();
            let _ = recv.kill();
            panic!("CLI failed: {:?}", transcript);
        }
    }
    assert!(send.wait().unwrap().success(), "sender process failed");
    assert!(recv.wait().unwrap().success(), "receiver process failed");
    assert_eq!(fs::read(receiver.join("收到.txt")).unwrap(), payload.as_bytes());
    fs::remove_dir_all(base).unwrap();
}
