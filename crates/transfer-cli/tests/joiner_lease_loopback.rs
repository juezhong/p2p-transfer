//! End-to-end black-box test of argument-free Rust P2P shell on one host.
//! CI models manual comparison by seeing BOTH generated values. Real users
//! must verify them through a trusted independent channel.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Peer { Creator, Joiner }

fn launch(exe: &str, cwd: &Path, peer: Peer, reports: Sender<(Peer, String)>) -> Child {
    let mut child = Command::new(exe)
        .current_dir(cwd)
        .env("P2P_TRANSFER_STUN", "off")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn().unwrap();
    for stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().unwrap()) as Box<dyn std::io::Read + Send>,
    ] {
        let tx = reports.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send((peer, line)).is_err() { break; }
            }
        });
    }
    child
}

fn enter(child: &mut Child, value: &str) {
    let stdin = child.stdin.as_mut().unwrap();
    stdin.write_all(value.as_bytes()).unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();
}

fn fixture() -> (PathBuf, PathBuf, PathBuf) {
    let mark = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let base = std::env::temp_dir().join(format!("p2p-shell-{}-{mark}", std::process::id()));
    let creator = base.join("A");
    let joiner = base.join("B");
    fs::create_dir_all(&creator).unwrap();
    fs::create_dir_all(&joiner).unwrap();
    (base, creator, joiner)
}

#[test]
fn joiner_receives_authoritative_lease_for_put_and_get() {
    let exe = env!("CARGO_BIN_EXE_p2p-transfer");
    let (base, a_root, b_root) = fixture();
    let content = "Rust 控制面和数据面严格分开！".repeat(12_000);
    fs::write(b_root.join("原始.txt"), content.as_bytes()).unwrap();
    let (tx, rx) = mpsc::channel::<(Peer, String)>();
    let mut a = launch(exe, &a_root, Peer::Creator, tx.clone());
    let mut b = launch(exe, &b_root, Peer::Joiner, tx);

    enter(&mut a, "1");
    enter(&mut a, "yes");
    enter(&mut b, "2");
    enter(&mut b, "yes");

    let mut invite = false;
    let mut reply = false;
    let mut first_code: Option<String> = None;
    let mut second_code: Option<String> = None;
    let mut confirmed = false;
    let mut ready = [false; 2];
    let mut put_sent = false;
    let mut get_sent = false;
    let mut get_done = false;
    let mut transcript = Vec::new();
    let end = Instant::now() + Duration::from_secs(75);
    while !get_done {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = a.kill();
            let _ = b.kill();
            panic!("Go-style shell timed out: {:?}", transcript);
        }
        let (peer, line) = rx.recv_timeout(remaining).unwrap_or_else(|_| {
            let _ = a.kill(); let _ = b.kill();
            panic!("Go-style shell disconnected: {:?}", transcript);
        });
        if let Some(pos) = line.find("P2PR-INV2-") {
            if !invite {
                enter(&mut b, &line[pos..]);
                invite = true;
            }
        }
        if let Some(pos) = line.find("P2PR-REP2-") {
            if !reply {
                enter(&mut a, &line[pos..]);
                reply = true;
            }
        }
        if let Some((_, code)) = line.split_once("双方独立核对 6 位校验码：") {
            match peer {
                Peer::Creator => first_code = Some(code.trim().to_owned()),
                Peer::Joiner => second_code = Some(code.trim().to_owned()),
            }
        }
        if let (Some(one), Some(two)) = (&first_code, &second_code) {
            if !confirmed {
                assert_eq!(one, two, "ICE/manual TLS transcript comparison mismatch");
                enter(&mut a, two);
                enter(&mut b, one);
                confirmed = true;
            }
        }
        if line.contains("已建立安全 P2P 连接") {
            ready[if peer == Peer::Creator { 0 } else { 1 }] = true;
        }
        if ready.iter().all(|x| *x) && !put_sent {
            enter(&mut b, "put 原始.txt 收到.txt");
            put_sent = true;
        }
        if put_sent && !get_sent && peer == Peer::Joiner
            && line.contains("[PUT] 文件或目录任务完成")
        {
            enter(&mut b, "ls");
            enter(&mut b, "get 收到.txt 回传.txt");
            get_sent = true;
        }
        if get_sent && peer == Peer::Joiner && line.contains("[文件] 已接收并验证") {
            get_done = true;
        }
        if line.contains("失败：") || line.contains("Transfer failed:") {
            let _ = a.kill();
            let _ = b.kill();
            panic!("Go-style shell failed: {:?}", transcript);
        }
        let secret = line.contains("P2PR-INV2-") || line.contains("P2PR-REP2-");
        if transcript.len() < 100 {
            transcript.push((if peer == Peer::Creator { "A" } else { "B" },
                if secret { "[redacted signal]".to_owned() } else { line.clone() }));
        }
    }
    enter(&mut a, "quit");
    enter(&mut b, "quit");
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    assert_eq!(fs::read(a_root.join("收到.txt")).unwrap(), content.as_bytes());
    assert_eq!(fs::read(b_root.join("回传.txt")).unwrap(), content.as_bytes());
    fs::remove_dir_all(base).unwrap();
}
