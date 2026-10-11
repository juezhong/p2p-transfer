//! Black-box regression: when either real CLI exits, its counterpart stays
//! interactive and reports the disconnect instead of terminating or trying
//! to keep serving Control RPCs. Does not log pairing secrets.

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Eq, PartialEq)]
enum Side { Creator, Joiner }

fn start(exe: &str, dir: &Path, side: Side, tx: Sender<(Side, String)>) -> Child {
    let mut child = Command::new(exe)
        .current_dir(dir).env("P2P_TRANSFER_STUN", "off")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().unwrap();
    for stream in [
        Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
        Box::new(child.stderr.take().unwrap()) as Box<dyn std::io::Read + Send>
    ] {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send((side, line)).is_err() { return; }
            }
        });
    }
    child
}

fn enter(child: &mut Child, command: &str) {
    let input = child.stdin.as_mut().unwrap();
    writeln!(input, "{command}").unwrap();
    input.flush().unwrap();
}

#[test]
fn peer_quit_reports_disconnect_and_requires_explicit_local_quit() {
    let exe = env!("CARGO_BIN_EXE_p2p-transfer");
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("transfer-disconnect-{}-{suffix}", std::process::id()));
    fs::create_dir_all(dir.join("A")).unwrap();
    fs::create_dir_all(dir.join("B")).unwrap();
    let (tx,rx)=mpsc::channel::<(Side,String)>();
    let mut a=start(exe,&dir.join("A"),Side::Creator,tx.clone());
    let mut b=start(exe,&dir.join("B"),Side::Joiner,tx);
    enter(&mut a,"1"); enter(&mut a,"yes");
    enter(&mut b,"2"); enter(&mut b,"yes");

    let mut invited=false;
    let mut replied=false;
    let (mut code_a,mut code_b)=(None::<String>,None::<String>);
    let mut confirmed=false;
    let mut ready=[false,false];
    let mut quit=false;
    let mut disconnected=false;
    let mut refused=false;
    let until=Instant::now()+Duration::from_secs(110);
    while !refused {
        let left=until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _=a.kill(); let _=b.kill();
            panic!("timed out validating disconnect");
        }
        let (side,line)=rx.recv_timeout(left).unwrap_or_else(|_| {
            let _=a.kill();let _=b.kill();
            panic!("CLI exited before peer disconnect was acknowledged")
        });
        if let Some(pos)=line.find("P2PR-INV2-") {
            if !invited {enter(&mut b,&line[pos..]); invited=true;}
        }
        if let Some(pos)=line.find("P2PR-REP2-") {
            if !replied {enter(&mut a,&line[pos..]); replied=true;}
        }
        if let Some((_,code))=line.split_once("双方设备应显示相同的 6 位配对核对码：") {
            match side {
                Side::Creator=>code_a=Some(code.trim().to_owned()),
                Side::Joiner=>code_b=Some(code.trim().to_owned()),
            }
        }
        if !confirmed {
            if let (Some(x),Some(y))=(&code_a,&code_b) {
                assert_eq!(x,y);
                enter(&mut a,"yes");enter(&mut b,"yes");
                confirmed=true;
            }
        }
        if line.contains("已建立安全 P2P 连接") {
            ready[if side==Side::Creator {0} else {1}]=true;
        }
        if ready.iter().all(|x| *x) && !quit {
            enter(&mut a,"quit");
            quit=true;
        }
        if quit && side==Side::Joiner && line.contains("对端已断开") {
            disconnected=true;
            enter(&mut b,"status");
        } else if disconnected && side==Side::Joiner
            && line.contains("无法继续") {
            refused=true;
        }
        assert!(!line.contains("Transfer failed:"), "unexpected CLI error: {line}");
    }
    enter(&mut b,"quit");
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    fs::remove_dir_all(dir).unwrap();
}
