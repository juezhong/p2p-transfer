//! Persistent bidirectional interactive shell resembling p2p-friend.
//! File bytes travel exclusively on SDK Data QUIC; directory RPC and ACK
//! use SDK Control QUIC. Only one local user-initiated task at a time.
//! Recursive transfer, remote cancellation and resume are future gates.

use std::{
    path::{Component, Path, PathBuf},
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc},
    thread,
};

use tokio::{sync::mpsc, task::JoinHandle};
use transfer_core::{
    rpc::list_in_root,
    sdk_quic::{list_via_sdk, request_get_via_sdk, send_via_sdk, serve_control_stream, IncomingResult},
    secure_io::SharedRoot,
};

use crate::{interactive::ActivePeer, CliResult};

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 { NEXT_REQUEST.fetch_add(1, Ordering::Relaxed) }

fn root_relative(base: &Path, input: &str) -> CliResult<PathBuf> {
    let mut components: Vec<_> = base.components()
        .filter_map(|part| match part { Component::Normal(name) => Some(name.to_os_string()), _ => None })
        .collect();
    if input == "/" { return Ok(PathBuf::new()); }
    for part in Path::new(input).components() {
        match part {
            Component::Normal(name) => components.push(name.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if components.pop().is_none() {
                    return Err("不能离开已授权的根目录".into());
                }
            }
            _ => return Err("绝对路径不在授权范围内；请使用共享根目录内的相对路径".into()),
        }
    }
    Ok(components.iter().collect())
}

fn relative_string(path: &Path) -> CliResult<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| "当前协议不支持非 UTF-8 路径".into())
}

fn help() {
    println!("远端目录：pwd / ls [path] / cd <path|->");
    println!("本地目录：lpwd / lls [path] / lcd <path|->");
    println!("文件传输：put <local> [remote] / get <remote> [local]");
    println!("其他：status / cancel / help / quit");
    println!("路径相对本次明确授权的目录根；路径中有空格请使用双引号。");
    println!("实验限制：当前仅单文件，不支持目录递归、断点续传或真正的远程取消协议。");
}

fn input_thread(tx: mpsc::UnboundedSender<String>) {
    thread::spawn(move || loop {
        let line = crate::prompt("p2p> ");
        let Ok(line) = line else {
            let _ = tx.send("quit".into());
            break;
        };
        if tx.send(line).is_err() { break; }
    });
}

pub async fn run(peer: ActivePeer, root: Arc<SharedRoot>) -> CliResult<()> {
    let session = Arc::clone(&peer.session);
    let active = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    input_thread(tx);
    help();
    let mut local_cwd = PathBuf::new();
    let mut remote_cwd = PathBuf::new();
    let mut previous_local = PathBuf::new();
    let mut previous_remote = PathBuf::new();
    let mut task: Option<JoinHandle<()>> = None;

    loop {
        tokio::select! {
            incoming = session.control().accept_bi() => {
                match incoming {
                    Ok((send, recv)) => {
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let activity = Arc::clone(&active);
                        tokio::spawn(async move {
                            // Separate control RPCs may remain usable while
                            // a data task is running; the protocol dispatcher
                            // never passes file bytes on Control QUIC.
                            match serve_control_stream(&session, &root, send, recv).await {
                                Ok(IncomingResult::Received(receipt)) => {
                                    println!("\n[文件] 已接收并验证 {} 字节（SHA-256 OK）", receipt.bytes);
                                    activity.store(false, Ordering::SeqCst);
                                }
                                Ok(IncomingResult::ServedGet(receipt)) => {
                                    println!("\n[GET] 已回传 {} 字节", receipt.bytes);
                                    activity.store(false, Ordering::SeqCst);
                                }
                                Ok(IncomingResult::DirectoryListed) => {}
                                Err(err) => eprintln!("\n[远端请求] 处理失败：{err:?}"),
                            }
                        });
                    }
                    Err(_) => return Err("Control QUIC 连接已断开".into()),
                }
            }
            input = rx.recv() => {
                let Some(line) = input else { return Ok(()); };
                let args = match shell_words::split(&line) {
                    Ok(args) => args,
                    Err(err) => { println!("命令引号或转义无效：{err}"); continue; }
                };
                if args.is_empty() { continue; }
                let cmd = args[0].to_lowercase();
                match cmd.as_str() {
                    "quit" | "exit" | "bye" => {
                        session.control().close(0u32.into(), b"bye");
                        session.data().close(0u32.into(), b"bye");
                        return Ok(());
                    }
                    "help" | "?" => help(),
                    "status" => {
                        println!("角色={}；Control QUIC={}；Data QUIC={}；UDP {} -> {}",
                            peer.role,
                            if session.control().close_reason().is_none() { "connected" } else { "closed" },
                            if session.data().close_reason().is_none() { "connected" } else { "closed" },
                            peer.local_address, peer.peer_address);
                        println!("文件任务活动={}；本地共享根目录已授权", active.load(Ordering::SeqCst));
                    }
                    "pwd" if args.len() == 1 => println!("/{}", remote_cwd.display()),
                    "lpwd" if args.len() == 1 => println!("/{}", local_cwd.display()),
                    "lls" if args.len() <= 2 => {
                        let p = root_relative(&local_cwd, args.get(1).map(String::as_str).unwrap_or("."))?;
                        match list_in_root(&root, &relative_string(&p)?) {
                            Ok(names) => for name in names { println!("{name}"); },
                            Err(err) => println!("[LLS] {err:?}"),
                        }
                    }
                    "ls" if args.len() <= 2 => {
                        let p = root_relative(&remote_cwd, args.get(1).map(String::as_str).unwrap_or("."))?;
                        match list_via_sdk(&session, relative_string(&p)?, next_id()).await {
                            Ok(names) => for name in names { println!("{name}"); },
                            Err(err) => println!("[LS] {err:?}"),
                        }
                    }
                    "lcd" if args.len() == 2 => {
                        let path = if args[1] == "-" { previous_local.clone() }
                            else { root_relative(&local_cwd, &args[1])? };
                        match list_in_root(&root, &relative_string(&path)?) {
                            Ok(_) => { previous_local = std::mem::replace(&mut local_cwd, path); }
                            Err(err) => println!("[LCD] {err:?}"),
                        }
                    }
                    "cd" if args.len() == 2 => {
                        let path = if args[1] == "-" { previous_remote.clone() }
                            else { root_relative(&remote_cwd, &args[1])? };
                        match list_via_sdk(&session, relative_string(&path)?, next_id()).await {
                            Ok(_) => { previous_remote = std::mem::replace(&mut remote_cwd, path); }
                            Err(err) => println!("[CD] {err:?}"),
                        }
                    }
                    "put" if (2..=3).contains(&args.len()) => {
                        let source = root_relative(&local_cwd, &args[1])?;
                        let suggested = source.file_name()
                            .ok_or("请输入有效的文件路径")?.to_string_lossy().to_string();
                        let remote = root_relative(&remote_cwd, args.get(2).map(String::as_str).unwrap_or(&suggested))?;
                        let destination = relative_string(&remote)?;
                        if active.swap(true, Ordering::SeqCst) {
                            println!("[PUT] 当前已有文件任务");
                            continue;
                        }
                        let id = next_id();
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let activity = Arc::clone(&active);
                        task = Some(tokio::spawn(async move {
                            match send_via_sdk(&session, &root, &source, &destination, id).await {
                                Ok(receipt) => println!("\n[PUT] {} 字节已由接收端校验并提交", receipt.bytes),
                                Err(err) => eprintln!("\n[PUT] 失败：{err:?}"),
                            }
                            activity.store(false, Ordering::SeqCst);
                        }));
                    }
                    "get" if (2..=3).contains(&args.len()) => {
                        let src = root_relative(&remote_cwd, &args[1])?;
                        let suggested = src.file_name()
                            .ok_or("请输入有效的文件路径")?.to_string_lossy().to_string();
                        let local = root_relative(&local_cwd, args.get(2).map(String::as_str).unwrap_or(&suggested))?;
                        let source = relative_string(&src)?;
                        let destination = relative_string(&local)?;
                        if active.swap(true, Ordering::SeqCst) {
                            println!("[GET] 当前已有文件任务");
                            continue;
                        }
                        let id = next_id();
                        match request_get_via_sdk(&session, source, destination, id).await {
                            Ok(()) => println!("[GET] 对端已接受请求，正在传输..."),
                            Err(err) => {
                                active.store(false, Ordering::SeqCst);
                                println!("[GET] 请求失败：{err:?}");
                            }
                        }
                    }
                    "cancel" if args.len() == 1 => {
                        if let Some(handle) = task.take() {
                            if !handle.is_finished() {
                                handle.abort();
                                active.store(false, Ordering::SeqCst);
                                println!("[CANCEL] 已中止本地任务；Control QUIC 保持连接");
                            } else {
                                println!("[CANCEL] 当前没有正在运行的本地任务");
                            }
                        } else {
                            println!("[CANCEL] 当前没有可直接中止的本地任务");
                        }
                    }
                    _ => println!("命令不支持或参数错误；输入 help"),
                }
            }
            interrupt = tokio::signal::ctrl_c() => {
                match interrupt {
                    Ok(()) => {
                        if let Some(handle) = task.take() {
                            handle.abort();
                            active.store(false, Ordering::SeqCst);
                            println!("\n[CANCEL] 当前本地任务已中断，会话仍然保持");
                        } else {
                            println!("\n没有活动的本地发送任务，输入 quit 退出");
                        }
                    }
                    Err(_) => return Err("无法监听 Ctrl-C".into()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stays_within_authorized_root_and_supports_parent_navigation() {
        let root = PathBuf::from("子目录/二级");
        assert_eq!(root_relative(&root, "../文件.txt").unwrap(), PathBuf::from("子目录/文件.txt"));
        assert_eq!(root_relative(&root, "/").unwrap(), PathBuf::new());
        assert!(root_relative(Path::new(""), "../../escape").is_err());
        assert!(root_relative(Path::new(""), "/etc/passwd").is_err());
    }
}
