//! Persistent bidirectional interactive shell resembling p2p-friend.
//! File bytes travel exclusively on SDK Data QUIC; directory RPC and ACK
//! use SDK Control QUIC. Only one local user-initiated task at a time.
//! Recursive transfer, remote cancellation and resume are future gates.

use std::{
    future::Future,
    collections::{HashMap, VecDeque},
    path::{Component, Path, PathBuf},
    time::Duration,
    sync::{atomic::{AtomicBool, AtomicU64, Ordering}, Arc, Mutex as StdMutex},
    thread,
};

use tokio::{sync::{mpsc, oneshot, Mutex}, task::JoinHandle};
use rustyline::{Editor, error::ReadlineError, history::DefaultHistory};
use transfer_core::{
    lease::TransferLease,
    recursive::{gather_directory, ManifestEntry},
    rpc::list_in_root,
    sdk_quic::{acquire_transfer_via_sdk, release_transfer_via_sdk, list_typed_via_sdk, list_via_sdk, mkdir_via_sdk, request_get_via_sdk, send_via_managed_sdk, serve_control_stream_with_lease, IncomingResult, RemoteLeaseGrant},
    secure_io::SharedRoot,
};

use crate::{completion::{self, CompletionQuery, ShellCompleter}, interactive::ActivePeer, CliResult};
use transfer_core::transport_lanes::TransferDataLanes;

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_id() -> u64 { NEXT_REQUEST.fetch_add(1, Ordering::Relaxed) }

pub(crate) fn root_relative(base: &Path, input: &str) -> CliResult<PathBuf> {
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

pub(crate) fn relative_string(path: &Path) -> CliResult<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| "当前协议不支持非 UTF-8 路径".into())
}

fn help() {
    println!("远端目录：pwd / ls [path] / cd <path|->");
    println!("本地目录：lpwd / lls [path] / lcd <path|->");
    println!("文件传输：put <local> [remote] / get <remote> [local]");
    println!("其他：status / cancel / help / quit");
    println!("Tab：支持命令、本地和远端中文路径补全，带空格路径自动加引号。");
    println!("路径相对本次明确授权的目录根；路径中有空格请使用双引号。");
    println!("实验限制：目录递归已接入；尚不支持断点续传及真正的远程取消协议。");
}

/// The next prompt is displayed only after the current command was printed.
/// The OS thread waits independently of the Tokio Control/Data session.
fn input_thread(
    tx: mpsc::UnboundedSender<String>,
    ready: std::sync::mpsc::Receiver<()>,
    completions: mpsc::UnboundedSender<CompletionQuery>,
    prompt_label: Arc<StdMutex<String>>,
) {
    thread::spawn(move || {
        let mut editor = Editor::<ShellCompleter, DefaultHistory>::new().ok();
        if let Some(line_editor) = editor.as_mut() {
            line_editor.set_helper(Some(ShellCompleter { requests: completions }));
        }
        loop {
            let label = prompt_label.lock().map(|p| p.clone())
                .unwrap_or_else(|_| "p2p> ".to_owned());
            let line = if let Some(line_editor) = editor.as_mut() {
                match line_editor.readline(&label) {
                    Ok(line) => {
                        if !line.trim().is_empty() {
                            let _ = line_editor.add_history_entry(line.as_str());
                        }
                        line
                    }
                    Err(ReadlineError::Interrupted) => "cancel".to_owned(),
                    Err(_) => "quit".to_owned(),
                }
            } else {
                match crate::prompt(&label) {
                    Ok(line) => line,
                    Err(_) => "quit".to_owned(),
                }
            };
            if tx.send(line).is_err() || ready.recv().is_err() { break; }
        }
    });
}

struct PromptReady(std::sync::mpsc::Sender<()>);
impl Drop for PromptReady {
    fn drop(&mut self) { let _ = self.0.send(()); }
}


type PendingDownloads = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;

/// A directory PUT keeps the whole batch under one CLI task lease. Every
/// file is still transferred through separate Control/Data QUIC streams,
/// with per-file SHA-256 and no-clobber commit.
async fn put_path(
    session: &transfer_core::transport_lanes::TransferSession,
    lanes: &TransferDataLanes,
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
                    let receipt = send_via_managed_sdk(
                        session, lanes, root, path, &relative_string(&remote)?, next_id()
                    ).await.map_err(|e| format!("{e:?}"))?;
                    println!("[PUT] {}: {} 字节已校验提交", remote.display(), receipt.bytes);
                }
            }
            Ok(())
        }
        None => {
            let receipt = send_via_managed_sdk(
                session, lanes, root, source, &relative_string(destination)?, next_id()
            ).await.map_err(|e| format!("{e:?}"))?;
            println!("[PUT] {} 字节已由接收端校验并提交", receipt.bytes);
            Ok(())
        }
    }
}

async fn get_one_file(
    session: &transfer_core::transport_lanes::TransferSession,
    source: &Path,
    destination: &Path,
    pending: &PendingDownloads,
    grant: &RemoteLeaseGrant,
    is_creator: bool,
) -> CliResult<()> {
    let id = next_id();
    // 创建方 GET 的回传必须匹配已登记请求，任务取消即撤销授权。
    let _expected_get = if is_creator {
        Some(grant.lock().await.expect_local_get(id)
            .ok_or("GET 请求授权失败或 ID 重复")?)
    } else {
        None
    };
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
    session: &transfer_core::transport_lanes::TransferSession,
    root: &SharedRoot,
    source: &Path,
    destination: &Path,
    pending: &PendingDownloads,
    grant: &RemoteLeaseGrant,
    is_creator: bool,
) -> CliResult<()> {
    let initial = list_typed_via_sdk(session, relative_string(source)?, next_id()).await;
    let Ok(first) = initial else {
        return get_one_file(session, source, destination, pending, grant, is_creator).await;
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
                get_one_file(session, &remote_path, &local_path, pending, grant, is_creator).await?;
                println!("[GET] {}: 完成并校验", local_path.display());
            }
        }
    }
    Ok(())
}

/// The creator is the one authoritative lease arbitrator for this pair.
/// Joiner asks over already authenticated Control QUIC. The returned
/// guard stays alive for a whole directory transfer, not each individual
/// file, so unrelated transfer tasks cannot overlap the Data QUIC lane.
async fn with_batch_lease<F>(
    session: &transfer_core::transport_lanes::TransferSession,
    arbiter: &TransferLease,
    is_creator: bool,
    batch_id: u64,
    operation: F,
) -> CliResult<()>
where
    F: Future<Output = CliResult<()>>,
{
    let local = if is_creator {
        Some(arbiter.try_acquire(batch_id)
            .map_err(|_| "对端或本机已有文件传输任务".to_owned())?)
    } else {
        acquire_transfer_via_sdk(session, batch_id)
            .await.map_err(|e| format!("远端已占用文件传输租约：{e:?}"))?;
        None
    };
    let result = operation.await;
    if !is_creator {
        let release = release_transfer_via_sdk(session, batch_id).await;
        if result.is_ok() {
            release.map_err(|e| format!("远端租约释放失败：{e:?}"))?;
        }
    }
    drop(local);
    result
}

pub async fn run(peer: ActivePeer, root: Arc<SharedRoot>) -> CliResult<()> {
    let session = Arc::clone(&peer.session);
    let data_lanes = Arc::clone(&peer.data_lanes);
    let active = Arc::new(AtomicBool::new(false));
    let arbiter = TransferLease::new();
    let remote_grant: RemoteLeaseGrant = Arc::new(Mutex::new(Default::default()));
    let is_creator = peer.role == "创建方";
    let pending_downloads: PendingDownloads = Arc::new(Mutex::new(HashMap::new()));
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let (prompt_ready, ready_rx) = std::sync::mpsc::channel();
    let (completion_tx, mut completion_rx) = mpsc::unbounded_channel();
    let prompt_label = Arc::new(StdMutex::new("p2p[remote:/]> ".to_owned()));
    help();
    input_thread(tx, ready_rx, completion_tx, Arc::clone(&prompt_label));
    let mut local_cwd = PathBuf::new();
    let mut remote_cwd = PathBuf::new();
    let mut previous_local = PathBuf::new();
    let mut previous_remote = PathBuf::new();
    let mut task: Option<JoinHandle<()>> = None;
    let mut active_batch_id: Option<u64> = None;

    loop {
        tokio::select! {
            query = completion_rx.recv() => {
                if let Some(query) = query {
                    // Tab RPC 不能阻塞本机 Control accept loop，否则双方
                    // 同时请求远端补全时可能互相等待。
                    let session = Arc::clone(&session);
                    let root = Arc::clone(&root);
                    let local = local_cwd.clone();
                    let remote = remote_cwd.clone();
                    tokio::spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(3),
                            completion::resolve(query, &session, &root, &local, &remote)
                        ).await;
                    });
                }
            }
            incoming = session.control().accept_bi() => {
                match incoming {
                    Ok((send, recv)) => {
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let lanes = Arc::clone(&data_lanes);
                        let pending = Arc::clone(&pending_downloads);
                        let arbiter = arbiter.clone();
                        let remote_grant = Arc::clone(&remote_grant);
                        tokio::spawn(async move {
                            // Separate control RPCs may remain usable while
                            // a data task is running; the protocol dispatcher
                            // never passes file bytes on Control QUIC.
                            let auth = if is_creator { Some((&arbiter, &remote_grant)) } else { None };
                            match serve_control_stream_with_lease(&session, &root, send, recv, auth, Some(&lanes)).await {
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
                    Err(_) => {
                        return Err(format!(
                            "Control QUIC 连接已断开：{:?}；Data 活跃连接：{}",
                            session.control().close_reason(),
                            data_lanes.available().await.len(),
                        ));
                    },
                }
            }
            input = rx.recv() => {
                let _prompt_ready = PromptReady(prompt_ready.clone());
                let Some(line) = input else { return Ok(()); };
                let args = match shell_words::split(&line) {
                    Ok(args) => args,
                    Err(err) => { println!("命令引号或转义无效：{err}"); continue; }
                };
                if args.is_empty() { continue; }
                let cmd = args[0].to_lowercase();
                match cmd.as_str() {
                    "quit" | "exit" | "bye" => {
                        data_lanes.shutdown().await;
                        session.control().close(0u32.into(), b"bye");
                        return Ok(());
                    }
                    "help" | "?" => help(),
                    "status" => {
                        let actual = data_lanes.available().await.len();
                        println!("已认证 Data QUIC 活跃连接：{}/4", actual);
                        println!("角色={}；Control QUIC={}；Data 状态={}；UDP {} -> {}",
                            peer.role,
                            if session.control().close_reason().is_none() { "connected" } else { "closed" },
                            if actual > 0 { "connected" } else { "reconnecting" },
                            peer.local_address, peer.peer_address);
                        println!("文件任务活动={}；本地共享根目录已授权", active.load(Ordering::SeqCst));
                        println!("当前远端目录=/{}；当前本地相对目录=/{}",
                            remote_cwd.display(), local_cwd.display());
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
                            Ok(_) => {
                                previous_remote = std::mem::replace(&mut remote_cwd, path);
                                if let Ok(mut prompt) = prompt_label.lock() {
                                    *prompt = format!("p2p[remote:/{}]> ", remote_cwd.display());
                                }
                            }
                            Err(err) => println!("[CD] {err:?}"),
                        }
                    }
                    "put" if (2..=3).contains(&args.len()) => {
                        let source = root_relative(&local_cwd, &args[1])?;
                        let suggested = source.file_name()
                            .ok_or("请输入有效的文件路径")?.to_string_lossy().to_string();
                        let remote = root_relative(&remote_cwd, args.get(2).map(String::as_str).unwrap_or(&suggested))?;
                        if active.swap(true, Ordering::SeqCst) {
                            println!("[PUT] 当前已有文件任务");
                            continue;
                        }
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let lanes = Arc::clone(&data_lanes);
                        let activity = Arc::clone(&active);
                        let lease = arbiter.clone();
                        let batch_id = next_id();
                        active_batch_id = Some(batch_id);
                        task = Some(tokio::spawn(async move {
                            let result = with_batch_lease(&session, &lease, is_creator, batch_id,
                                put_path(&session, &lanes, &root, &source, &remote)).await;
                            match result {
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
                        let grant = Arc::clone(&remote_grant);
                        let activity = Arc::clone(&active);
                        let lease = arbiter.clone();
                        let batch_id = next_id();
                        active_batch_id = Some(batch_id);
                        task = Some(tokio::spawn(async move {
                            let result = with_batch_lease(&session, &lease, is_creator, batch_id,
                                get_path(&session, &root, Path::new(&source),
                                    Path::new(&destination), &pending, &grant, is_creator)).await;
                            match result {
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
                                if !is_creator {
                                    if let Some(id) = active_batch_id.take() {
                                        let session = Arc::clone(&session);
                                        tokio::spawn(async move {
                                            let _ = handle.await;
                                            let _ = release_transfer_via_sdk(&session, id).await;
                                        });
                                    }
                                }
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
                            if !is_creator {
                                if let Some(id) = active_batch_id.take() {
                                    let session = Arc::clone(&session);
                                    tokio::spawn(async move {
                                        let _ = handle.await;
                                        let _ = release_transfer_via_sdk(&session, id).await;
                                    });
                                }
                            }
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
