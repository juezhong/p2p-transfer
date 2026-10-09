//! Persistent bidirectional interactive shell resembling p2p-friend.
//! File bytes travel exclusively on SDK Data QUIC; directory RPC and ACK
//! use SDK Control QUIC. Only one local user-initiated task at a time.
//! Recursive transfer, remote cancellation and resume are future gates.

use std::{
    collections::{HashMap, VecDeque},
    path::{Component, Path, PathBuf},
    time::Duration,
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc},
    thread,
};

use tokio::{sync::{mpsc, oneshot, Mutex}, task::JoinHandle};
use transfer_core::{
    recursive::{gather_directory, ManifestEntry},
    rpc::list_in_root,
    sdk_quic::{list_typed_via_sdk, list_via_sdk, mkdir_via_sdk, request_get_via_sdk, send_via_sdk, serve_control_stream, IncomingResult},
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
    println!("实验限制：目录递归已接入；尚不支持断点续传及真正的远程取消协议。");
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


type PendingDownloads = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;

/// A directory PUT keeps the whole batch under one CLI task lease. Every
/// file is still transferred through separate Control/Data QUIC streams,
/// with per-file SHA-256 and no-clobber commit.
async fn put_path(
    session: &p2p_sdk::verified_session::VerifiedManualSession,
    root: &SharedRoot,
    source: &Path,
    destination: &Path,
) -> CliResult<()> {
    let entries = match root.list(Some(source)) {
        Ok(_) => Some(gather_directory(root, source).map_err(|e| format!("{e:?}"))?),
        Err(_) => None,
    };
    match entries {
        Some(entries) => {
            for entry in entries {
                let (path, directory) = match &entry {
                    ManifestEntry::Directory(path) => (path, true),
                    ManifestEntry::File { relative, .. } => (relative, false),
                };
                let suffix = path.strip_prefix(source).map_err(|e| e.to_string())?;
                let remote = destination.join(suffix);
                if directory {
                    mkdir_via_sdk(session, relative_string(&remote)?, next_id())
                        .await.map_err(|e| format!("{e:?}"))?;
                } else {
                    let receipt = send_via_sdk(
                        session, root, path, &relative_string(&remote)?, next_id()
                    ).await.map_err(|e| format!("{e:?}"))?;
                    println!("[PUT] {}: {} 字节已校验提交", remote.display(), receipt.bytes);
                }
            }
            Ok(())
        }
        None => {
            let receipt = send_via_sdk(
                session, root, source, &relative_string(destination)?, next_id()
            ).await.map_err(|e| format!("{e:?}"))?;
            println!("[PUT] {} 字节已由接收端校验并提交", receipt.bytes);
            Ok(())
        }
    }
}

async fn get_one_file(
    session: &p2p_sdk::verified_session::VerifiedManualSession,
    source: &Path,
    destination: &Path,
    pending: &PendingDownloads,
) -> CliResult<()> {
    let id = next_id();
    let (done_tx, done_rx) = oneshot::channel();
    pending.lock().await.insert(id, done_tx);
    if let Err(err) = request_get_via_sdk(
        session, relative_string(source)?, relative_string(destination)?, id
    ).await {
        pending.lock().await.remove(&id);
        return Err(format!("{err:?}"));
    }
    tokio::select! {
        answer = done_rx => answer.map_err(|_| "接收文件完成通知中断".to_owned()),
        _ = tokio::time::sleep(Duration::from_secs(1800)) => {
            pending.lock().await.remove(&id);
            Err("等待文件接收校验超时".into())
        }
        _ = session.control().closed() => {
            pending.lock().await.remove(&id);
            Err("Control QUIC 在文件接收时断开".into())
        }
    }
}

/// Remote typed directory listing is authoritative: never guess whether an
/// unreadable remote object is a file. Only fallback to GET when remote
/// explicitly reports the initial path is not a listable directory.
async fn get_path(
    session: &p2p_sdk::verified_session::VerifiedManualSession,
    root: &SharedRoot,
    source: &Path,
    destination: &Path,
    pending: &PendingDownloads,
) -> CliResult<()> {
    let initial = list_typed_via_sdk(session, relative_string(source)?, next_id()).await;
    let Ok(first) = initial else {
        return get_one_file(session, source, destination, pending).await;
    };
    let mut todo = VecDeque::from([(source.to_path_buf(), destination.to_path_buf(), first, 0usize)]);
    let mut count = 0usize;
    while let Some((remote_dir, local_dir, entries, depth)) = todo.pop_front() {
        root.create_directory(&local_dir).map_err(|e| format!("{e:?}"))?;
        for entry in entries {
            count += 1;
            if count > 100_000 { return Err("递归目录超出条目上限".into()); }
            let remote_path = remote_dir.join(&entry.name);
            let local_path = local_dir.join(&entry.name);
            if entry.is_directory {
                if depth >= 64 { return Err("目录深度超出上限".into()); }
                let children = list_typed_via_sdk(
                    session, relative_string(&remote_path)?, next_id()
                ).await.map_err(|e| format!("{e:?}"))?;
                todo.push_back((remote_path, local_path, children, depth + 1));
            } else {
                get_one_file(session, &remote_path, &local_path, pending).await?;
                println!("[GET] {}: 完成并校验", local_path.display());
            }
        }
    }
    Ok(())
}

pub async fn run(peer: ActivePeer, root: Arc<SharedRoot>) -> CliResult<()> {
    let session = Arc::clone(&peer.session);
    let active = Arc::new(AtomicBool::new(false));
    let pending_downloads: PendingDownloads = Arc::new(Mutex::new(HashMap::new()));
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
                        let pending = Arc::clone(&pending_downloads);
                        tokio::spawn(async move {
                            // Separate control RPCs may remain usable while
                            // a data task is running; the protocol dispatcher
                            // never passes file bytes on Control QUIC.
                            match serve_control_stream(&session, &root, send, recv).await {
                                Ok(IncomingResult::Received(receipt)) => {
                                    println!("\n[文件] 已接收并验证 {} 字节（SHA-256 OK）", receipt.bytes);
                                    if let Some(done) = pending.lock().await.remove(&receipt.request_id) {
                                        let _ = done.send(());
                                    }
                                }
                                Ok(IncomingResult::ServedGet(receipt)) => {
                                    println!("\n[GET] 已回传 {} 字节", receipt.bytes);
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
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let activity = Arc::clone(&active);
                        task = Some(tokio::spawn(async move {
                            match put_path(&session, &root, &source, &remote).await {
                                Ok(()) => println!("\n[PUT] 文件或目录任务完成"),
                                Err(err) => eprintln!("\n[PUT] 失败：{err}"),
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
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let pending = Arc::clone(&pending_downloads);
                        let activity = Arc::clone(&active);
                        task = Some(tokio::spawn(async move {
                            match get_path(&session, &root, Path::new(&source),
                                Path::new(&destination), &pending).await
                            {
                                Ok(()) => println!("\n[GET] 文件或目录任务全部校验提交"),
                                Err(err) => eprintln!("\n[GET] 失败：{err}"),
                            }
                            activity.store(false, Ordering::SeqCst);
                        }));
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
