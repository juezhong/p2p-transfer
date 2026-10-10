//! p2p-transfer command-line entry, intentionally aligned with Go p2p-friend.
//!
//! No mandatory command-line options, local address, port, path or direction.
//! Creating/joining a connection, browsing and PUT/GET live in the persistent
//! interactive shell. SDK exclusively owns P2P/ICE/TLS/QUIC networking.

mod completion;
mod interactive;
mod shell;

use std::io::{self, Write};

type CliResult<T> = Result<T, String>;

fn debug_error<E: std::fmt::Debug>(e: E) -> String {
    format!("{e:?}")
}

fn prompt(label: &str) -> CliResult<String> {
    print!("{label}");
    io::stdout().flush().map_err(debug_error)?;
    let mut buf = String::new();
    if io::stdin().read_line(&mut buf).map_err(debug_error)? == 0 {
        return Err("输入结束，操作已取消".into());
    }
    Ok(buf.trim().to_owned())
}

fn help() {
    println!(
        "p2p-transfer v{} (Rust 开发预览)\n\
         直接运行 p2p-transfer，选择创建连接或加入连接，无需填写 IP/端口/目录参数。\n\
         会话建立后：\n\
           pwd / ls / cd        远端目录\n\
           lpwd / lls / lcd     本地目录\n\
           put <local> [remote] 上传文件或目录\n\
           get <remote> [local] 下载文件或目录\n\
           status / cancel / help / quit\n\
         文件访问限定于本次明确授权的共享根目录；无文件数据中继。\n\
         TUI/GUI、完整故障恢复和公网 NAT 验证仍在开发。",
        env!("CARGO_PKG_VERSION")
    );
}

// Explicit stack is needed on some Windows targets for the ICE/QUIC async
// handshake futures; no weakening of TLS validation or ICE checks.
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {}
        [arg] if matches!(arg.as_str(), "-h" | "--help" | "help") => {
            help();
            return;
        }
        [arg] if matches!(arg.as_str(), "-v" | "--version" | "version") => {
            println!("p2p-transfer v{} (Rust 开发预览)", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {
            eprintln!(
                "与 Go p2p-friend 一样：请不带参数直接运行 p2p-transfer（或使用 --help）。"
            );
            std::process::exit(2);
        }
    }
    if let Err(err) = interactive::run().await {
        eprintln!("Transfer failed: {err}");
        std::process::exit(1);
    }
}
