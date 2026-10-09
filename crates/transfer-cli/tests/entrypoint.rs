//! Go-compatible startup behavior; all actual network transfers are tested
//! separately by interactive_loopback / joiner_lease_loopback / recursive_loopback.
use std::process::Command;

#[test]
fn help_and_version_need_no_peer_or_network() {
    let exe = env!("CARGO_BIN_EXE_p2p-transfer");
    for arg in ["-h", "--help", "help"] {
        let result = Command::new(exe).arg(arg).output().unwrap();
        assert!(result.status.success());
        let output = String::from_utf8(result.stdout).unwrap();
        assert!(output.contains("put <local> [remote]"));
        assert!(output.contains("get <remote> [local]"));
        assert!(output.contains("无需填写 IP/端口"));
    }
    for arg in ["-v", "--version", "version"] {
        let result = Command::new(exe).arg(arg).output().unwrap();
        assert!(result.status.success());
        assert!(String::from_utf8(result.stdout).unwrap().contains("p2p-transfer v"));
    }
}

#[test]
fn rejects_legacy_positional_send_receive_and_ip_arguments() {
    let exe = env!("CARGO_BIN_EXE_p2p-transfer");
    for args in [["send", "127.0.0.1:0"], ["receive", "127.0.0.1:0"]] {
        let result = Command::new(exe).args(args).output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(String::from_utf8(result.stderr).unwrap().contains("不带参数"));
    }
}
