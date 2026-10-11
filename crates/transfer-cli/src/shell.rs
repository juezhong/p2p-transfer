//! Persistent bidirectional interactive shell resembling p2p-friend.
//! File bytes travel exclusively on SDK Data QUIC; directory RPC and ACK
//! use SDK Control QUIC. Only one local user-initiated task at a time.
//! Recursive transfers and authenticated cross-peer cancellation are supported;
//! interrupted chunk-level resume is a separate reliability gate.

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
    cancel::TransferCancels,
    lease::TransferLease,
    progress::{TransferProgress, ProgressSnapshot},
    recursive::{gather_directory, ManifestEntry},
    rpc::list_in_root,
    sdk_quic::{acquire_transfer_via_sdk, release_transfer_via_sdk, list_typed_via_sdk, list_via_sdk, root_info_via_sdk, starting_directory_via_sdk, mkdir_via_sdk, request_get_via_sdk, cancel_transfer_via_sdk, CancelAck, send_via_managed_sdk_cancellable, serve_control_stream_cancellable, IncomingResult, RemoteLeaseGrant},
    secure_io::SharedRoot,
};

use crate::{completion::{self, CompletionQuery, ShellCompleter}, interactive::ActivePeer, CliResult};
use transfer_core::modern_data_lanes::ModernDataLanes;
use transfer_core::sdk_quic::AuthenticatedSession;

// All task notices and live status are serialized with the input editor.
macro_rules! println {
    () => { crate::terminal::log(String::new()) };
    ($($arg:tt)*) => { crate::terminal::log(format!($($arg)*)) };
}
macro_rules! eprintln {
    ($($arg:tt)*) => { crate::terminal::error(format!($($arg)*)) };
}


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

/// Accept explicit absolute paths only inside the session-authorized root.
pub(crate) fn scoped_path(root: &Path, cwd: &Path, input: &str) -> CliResult<PathBuf> {
    let path = Path::new(input);
    if input == "/" { return Ok(PathBuf::new()); }
    if path.is_absolute() {
        // Windows canonicalize() often returns verbatim volume paths
        // (\\\\?\\C:\\...), while std::env::temp_dir(), pasted commands
        // and CLI arguments normally use DOS spelling (C:\\...).
        // Both forms refer to the SAME volume. Strip using Path component
        // boundaries rather than string prefix matching; still reject other
        // drives and normalize parent navigation below.
        #[cfg(windows)]
        let ordinary_root = root.to_str()
            .and_then(|text| text.strip_prefix(r"\\?\"))
            .map(Path::new);
        #[cfg(windows)]
        let stripped = path.strip_prefix(root).or_else(|_| {
            path.strip_prefix(ordinary_root.unwrap_or(root))
        });
        #[cfg(not(windows))]
        let stripped = path.strip_prefix(root);
        let stripped = stripped
            .map_err(|_| format!("绝对路径不在本次授权目录 {} 下", root.display()))?;
        return root_relative(Path::new(""), &stripped.to_string_lossy());
    }
    root_relative(cwd, input)
}

fn scoped_remote(root: Option<&str>, cwd: &Path, input: &str) -> CliResult<PathBuf> {
    let root = root.ok_or("尚未取得对端文件系统范围，无法确认文件路径")?;
    scoped_path(Path::new(root), cwd, input)
}

fn display_absolute(root: &Path, cwd: &Path) -> String {
    root.join(cwd).display().to_string()
}

pub(crate) fn relative_string(path: &Path) -> CliResult<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| "当前协议不支持非 UTF-8 路径".into())
}

fn introduction() {
    println!("远端：pwd / ls / cd    本地：lpwd / lls / lcd");
    println!("传输：put / get / cancel    会话：status / help / quit");
    println!("Tab 可补全命令及路径；Ctrl-C 可取消当前任务。");
}

fn help() {
    println!("════════════════ P2P Transfer 命令帮助 ════════════════");
    println!("  远端目录");
    println!("    pwd                   显示远端完整路径");
    println!("    ls [路径]             列出远端文件/目录");
    println!("    cd <路径|->           进入远端目录 / 返回上一次");
    println!("  本机目录");
    println!("    lpwd                  显示本地完整路径");
    println!("    lls [路径]            列出本地文件/目录");
    println!("    lcd <路径|->          进入本地目录 / 返回上一次");
    println!("  文件传输");
    println!("    put <本机> [远端]     上传文件或整个目录");
    println!("    get <远端> [本机]     下载文件或整个目录");
    println!("    cancel / Ctrl-C       双端取消当前任务，不断开");
    println!("  会话与编辑");
    println!("    status                显示连接和任务状态");
    println!("    help                  显示本帮助");
    println!("    quit / exit           断开并退出");
    println!("    Tab / ↑↓              补全路径 / 浏览历史");
    println!("════════════════════════════════════════════════════");
    println!("支持绝对路径、中文文件名；含空格的路径使用双引号。");
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
            line_editor.set_helper(Some(ShellCompleter { requests: completions.clone() }));
        }
        loop {
            let label = prompt_label.lock().map(|p| p.clone())
                .unwrap_or_else(|_| "p2p> ".to_owned());
            let line = if crate::terminal::supported() {
                match crate::terminal::read_line(&label, &completions) {
                    Ok(line) => line,
                    Err(_) => "quit".to_owned(),
                }
            } else if let Some(line_editor) = editor.as_mut() {
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

/// Cancellation safety: an aborted GET must not leave a pending response
/// sender behind while the Control session continues accepting commands.
struct PendingDownloadGuard {
    id: u64,
    requests: PendingDownloads,
}
impl Drop for PendingDownloadGuard {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.requests.try_lock() {
            entries.remove(&self.id);
        } else {
            let requests = Arc::clone(&self.requests);
            let id = self.id;
            tokio::spawn(async move { requests.lock().await.remove(&id); });
        }
    }
}

/// A directory PUT keeps the whole batch under one CLI task lease. Every
/// file is still transferred through separate Control/Data QUIC streams,
/// with per-file SHA-256 and no-clobber commit.
async fn put_path(
    session: &dyn AuthenticatedSession,
    lanes: &ModernDataLanes,
    root: &SharedRoot,
    source: &Path,
    destination: &Path,
    progress: &TransferProgress,
    cancels: &TransferCancels,
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
                    let receipt = send_via_managed_sdk_cancellable(
                        session, lanes, root, (path, &relative_string(&remote)?), next_id(), Some(progress), cancels
                    ).await.map_err(|e| format!("{e:?}"))?;
                    println!("[PUT] {}: {} 字节已校验提交", remote.display(), receipt.bytes);
                }
            }
            Ok(())
        }
        None => {
            let receipt = send_via_managed_sdk_cancellable(
                session, lanes, root, (source, &relative_string(destination)?), next_id(), Some(progress), cancels
            ).await.map_err(|e| format!("{e:?}"))?;
            println!("[PUT] {} 字节已由接收端校验并提交", receipt.bytes);
            Ok(())
        }
    }
}

async fn get_one_file(
    session: &dyn AuthenticatedSession,
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
    let _pending_guard = PendingDownloadGuard {
        id, requests: Arc::clone(pending),
    };
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
    session: &dyn AuthenticatedSession,
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
    session: &dyn AuthenticatedSession,
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

fn describe_link(peer: &ActivePeer) -> &'static str {
    let d = peer.session.diagnostic();
    let candidate = format!("{:?}", d.remote_candidate_kind).to_ascii_lowercase();
    if d.actual_remote_udp.is_ipv6() { return "IPv6 直连"; }
    if candidate.contains("portmap") { return "IPv4 端口映射直连"; }
    if candidate.contains("srflx") || candidate.contains("prflx")
        || candidate.contains("reflexive")
    { return "IPv4 NAT 打洞"; }
    if candidate.contains("host") {
        match d.actual_remote_udp.ip() {
            std::net::IpAddr::V4(ip) if ip.is_private() || ip.is_link_local() =>
                "IPv4 局域网直连",
            _ => "IPv4 Host 直连",
        }
    } else { "IPv4 ICE 直连" }
}

fn print_progress(p: &ProgressSnapshot) {
    let rate = p.bytes_per_second() / (1024.0 * 1024.0);
    let eta = p.eta().map(|d| format!("{}s", d.as_secs()))
        .unwrap_or_else(|| "计算中".to_owned());
    let msg = format!(
        "[{}] {} {:5.1}%  {}/{} MiB  {:.2} MiB/s  已用 {}s  剩余 {}",
        p.direction, p.file, p.percent(),
        p.written / 1_048_576, p.total / 1_048_576,
        rate, p.elapsed.as_secs(), eta,
    );
    crate::terminal::progress(Some(msg));
}

async fn cancel_current(
    session: &dyn AuthenticatedSession,
    worker: &mut Option<JoinHandle<()>>,
    batch: &mut Option<u64>,
    is_creator: bool,
    pending: &PendingDownloads,
    progress: &TransferProgress,
    registry: &TransferCancels,
) -> bool {
    // Collect IDs before abort: the GET guard removes pending state on Drop.
    let mut ids: Vec<u64> = pending.lock().await.keys().copied().collect();
    if let Some(p) = progress.snapshot() {
        if p.request_id != 0 && !ids.contains(&p.request_id) {
            ids.push(p.request_id);
        }
    }
    let mut interrupted = false;
    if let Some(handle) = worker.take() {
        if !handle.is_finished() { interrupted = true; handle.abort(); }
        let _ = handle.await;
    }
    for id in ids {
        interrupted = true;
        registry.cancel(id);
        match tokio::time::timeout(Duration::from_secs(5),
            cancel_transfer_via_sdk(session, id)).await
        {
            Ok(Ok(CancelAck::Stopped)) =>
                println!("\n[CANCEL] 对端文件任务 {id} 已停止"),
            Ok(Ok(CancelAck::Pending)) =>
                println!("\n[CANCEL] 对端已记录取消请求 {id}，尚未发现活动发送任务"),
            _ => eprintln!("\n[CANCEL] 对端任务停止确认超时或失败（任务 {id}）"),
        }
    }
    if let Some(id) = batch.take() {
        if !is_creator {
            let _ = tokio::time::timeout(Duration::from_secs(4),
                release_transfer_via_sdk(session, id)).await;
        }
    }
    progress.clear();
                crate::terminal::progress(None);
    interrupted
}

/// Always clean up auxiliary managers and the SDK transport on all exits.
pub async fn run(peer: ActivePeer, root: Arc<SharedRoot>) -> CliResult<()> {
    let session = Arc::clone(&peer.session);
    let lanes = Arc::clone(&peer.data_lanes);
    let result = run_shell(&peer, root).await;
    session.control.close(0u32.into(), b"user ended transfer shell");
    lanes.shutdown().await;
    drop(lanes);
    drop(peer);
    if let Ok(owned) = Arc::try_unwrap(session) { owned.shutdown().await; }
    crate::terminal::progress(None);
    result
}

async fn run_shell(peer: &ActivePeer, root: Arc<SharedRoot>) -> CliResult<()> {
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
    let prompt_label = Arc::new(StdMutex::new("远端路径同步中> ".to_owned()));
    introduction();
    // Defer the first input prompt until the peer's real startup directory
    // has been resolved over authenticated Control RPC. Otherwise a fast
    // typist could accidentally run "ls" against "/" before synchronization.
    let mut initial_input = Some((tx, ready_rx, completion_tx));
    let mut local_cwd = root.starting_relative()
        .map_err(|err| format!("初始目录不在本机访问范围内：{err:?}"))?;
    let mut remote_cwd = PathBuf::new();
    let mut previous_local = local_cwd.clone();
    let mut previous_remote = PathBuf::new();
    let mut task: Option<JoinHandle<()>> = None;
    let mut active_batch_id: Option<u64> = None;
    let progress = Arc::new(TransferProgress::default());
    let cancels = Arc::new(TransferCancels::default());
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut disconnected = false;
    let mut remote_root: Option<String> = None;
    let (root_tx, mut root_rx) = mpsc::unbounded_channel::<(String, String)>();
    let mut root_fetch_finished = false;
    let link_mode = describe_link(peer);
    let session_for_root = Arc::clone(&session);
    tokio::spawn(async move {
        // Both peers enter their Control accept loop before this resolves.
        // Root is the accessible filesystem scope (e.g. "/"); startup cwd
        // is a DIFFERENT path to display in the shell and use by default.
        if let Ok(Ok((scope, start))) = tokio::time::timeout(
            Duration::from_secs(8),
            async {
                tokio::try_join!(
                    root_info_via_sdk(&session_for_root, next_id()),
                    starting_directory_via_sdk(&session_for_root, next_id()),
                )
            },
        ).await {
            let _ = root_tx.send((scope, start));
        }
    });
    let mut incoming_workers = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            root_message = root_rx.recv(), if !root_fetch_finished => {
                root_fetch_finished = true;
                if let Some((scope, initial)) = root_message {
                    let relative = Path::new(&initial).strip_prefix(&scope).ok()
                        .and_then(|path| root_relative(Path::new(""), &path.to_string_lossy()).ok());
                    if let Some(relative) = relative {
                        previous_remote = relative.clone();
                        remote_cwd = relative;
                        println!("\n[目录] 对端初始目录：{initial}");
                        if let Ok(mut prompt) = prompt_label.lock() {
                            *prompt = format!("[{}] {}> ", link_mode, display_absolute(Path::new(&scope), &remote_cwd));
                        }
                        remote_root = Some(scope);
                    } else {
                        eprintln!("[目录] 对端初始目录不在有效共享范围内，拒绝目录同步");
                    }
                } else {
                    eprintln!("[目录] 无法取得对端初始工作目录；请确认双方使用兼容版本");
                }
                if let Some((sender, ready, queries)) = initial_input.take() {
                    input_thread(sender, ready, queries, Arc::clone(&prompt_label));
                }
            }
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
            _ = session.control().closed(), if !disconnected => {
                disconnected = true;
                if let Some(worker) = task.take() {
                    worker.abort();
                    let _ = worker.await;
                }
                active.store(false, Ordering::SeqCst);
                progress.clear();
                crate::terminal::progress(None);
                active_batch_id = None;
                println!("\n[连接] 对端已断开；本次会话不能继续传输，请输入 quit 退出。");
            }
            completed = incoming_workers.join_next(), if !incoming_workers.is_empty() => {
                if let Some(Err(err)) = completed {
                    if !disconnected { eprintln!("[远端请求] 任务异常终止：{err}"); }
                }
            }
            _ = tick.tick() => {
                if let Some(snapshot) = progress.snapshot() {
                    print_progress(&snapshot);
                } else {
                    crate::terminal::progress(None);
                }
                if task.as_ref().is_some_and(JoinHandle::is_finished) {
                    if let Some(done) = task.take() {
                        if let Err(err) = done.await { eprintln!("[任务] 已中止：{err}"); }
                    }
                    progress.clear();
                crate::terminal::progress(None);
                    active_batch_id = None;
                    active.store(false, Ordering::SeqCst);
                }
            }
            incoming = session.control().accept_bi(), if !disconnected => {
                match incoming {
                    Ok((send, recv)) => {
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let lanes = Arc::clone(&data_lanes);
                        let pending = Arc::clone(&pending_downloads);
                        let arbiter = arbiter.clone();
                        let remote_grant = Arc::clone(&remote_grant);
                        let progress = Arc::clone(&progress);
                        let cancels = Arc::clone(&cancels);
                        incoming_workers.spawn(async move {
                            // Separate control RPCs may remain usable while
                            // a data task is running; the protocol dispatcher
                            // never passes file bytes on Control QUIC.
                            let auth = if is_creator { Some((&arbiter, &remote_grant)) } else { None };
                            match serve_control_stream_cancellable(
                                &session, &root, (send, recv), auth, Some(&lanes), Some(&progress), Some(&cancels),
                            ).await {
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
                                Err(transfer_core::sdk_quic::SdkTransferError::Cancelled) => {
                                    progress.clear();
                crate::terminal::progress(None);
                                    println!("\n[CANCEL] 该文件任务已由一端取消，Data 流已停止");
                                },
                                Err(err) => {
                                    if session.control().close_reason().is_none() {
                                        eprintln!("\n[远端请求] 处理失败：{err:?}");
                                    }
                                },
                            }
                        });
                    }
                    Err(_) => {
                        disconnected = true;
                        if let Some(worker) = task.take() {
                            worker.abort();
                            let _ = worker.await;
                        }
                        progress.clear();
                crate::terminal::progress(None);
                        active.store(false, Ordering::SeqCst);
                        println!("\n[连接] 对端已断开；输入 quit 退出。");
                    },
                }
            }
            input = rx.recv() => {
                let _prompt_guard = PromptReady(prompt_ready.clone());
                let Some(line) = input else { return Ok(()); };
                let args = match shell_words::split(&line) {
                    Ok(args) => args,
                    Err(err) => { println!("命令引号或转义无效：{err}"); continue; }
                };
                if args.is_empty() { continue; }
                let cmd = args[0].to_lowercase();
                if (disconnected || session.control().close_reason().is_some())
                    && !matches!(cmd.as_str(), "quit" | "exit" | "bye") {
                    println!("[连接] 对端已经断开；任何操作都无法继续，请输入 quit 退出。");
                    continue;
                }
                match cmd.as_str() {
                    "quit" | "exit" | "bye" => {
                        if let Some(worker) = task.take() {
                            worker.abort();
                            let _ = worker.await;
                        }
                        return Ok(());
                    }
                    "help" | "?" => help(),
                    "status" => {
                        let d = session.diagnostic();
                        let lanes = data_lanes.statuses().await;
                        let connected = lanes.iter()
                            .filter(|lane| matches!(
                                lane.phase,
                                p2p_sdk::transport_session::ManagedLinkPhase::Connected,
                            )).count();
                        if remote_root.is_none() {
                            if let Ok(Ok(path)) = tokio::time::timeout(
                                Duration::from_secs(3),
                                root_info_via_sdk(&session, next_id()),
                            ).await { remote_root = Some(path); }
                        }
                        println!("\n══════════ 会话状态 ══════════");
                        println!("  连接方式    {}（无服务器中继）", link_mode);
                        println!("  设备角色    {}", peer.role);
                        println!("  Control     {} / {}",
                            if session.control().close_reason().is_none() {
                                "已连接"
                            } else { "已断开" },
                            if d.control_outbound { "主动拨号" } else { "被动接入" });
                        println!("  本机 UDP    {}", d.actual_local_udp);
                        println!("  对端 UDP    {}", d.actual_remote_udp);
                        println!("  ICE         {:?} / 对端候选 {}",
                            d.ice_role, format!("{:?}", d.remote_candidate_kind)
                                .replace("Some(", "").replace(')', ""));
                        println!("  STUN        {:?} / 网关 {:?} {:?}",
                            d.stun_consistency, d.gateway_method, d.gateway_mapping);
                        println!("  UDP 候选    {:?}", d.offered_host_candidates);
                        println!("  认证 prflx  {:?}", d.authenticated_peer_reflexive);
                        println!("────────── Data QUIC ({connected}/{}) ──────────", lanes.len());
                        for lane in lanes {
                            let state = if matches!(lane.phase,
                                p2p_sdk::transport_session::ManagedLinkPhase::Connected) {
                                "已连接"
                            } else { "重连中/不可用" };
                            println!("  链路 #{}    {}", lane.index, state);
                            if state != "已连接" {
                                println!("      代次 {}；错误 {}", lane.generation,
                                    lane.last_error.as_deref().unwrap_or("未知"));
                            }
                        }
                        println!("────────── 已授权目录 ──────────");
                        println!("  本机可访问  {}", root.display_root().display());
                        println!("  对端可访问  {}", remote_root.as_deref().unwrap_or("尚未取得"));
                        println!("  本机当前    {}", display_absolute(root.display_root(), &local_cwd));
                        println!("  对端当前    {}", display_absolute(
                            Path::new(remote_root.as_deref().unwrap_or("")), &remote_cwd
                        ));
                        println!("────────── 文件任务 ──────────");
                        let remote_id = remote_grant.lock().await.active_request();
                        if let Some(id) = remote_id {
                            println!("  租约        忙碌（对端发起，批次 {id}）");
                        } else if let Some(id) = arbiter.active_request() {
                            println!("  租约        忙碌（本机发起，批次 {id}）");
                        } else {
                            println!("  租约        空闲");
                        }
                        println!("  本机命令    {}", if active.load(Ordering::SeqCst) {
                            "执行中"
                        } else { "空闲" });
                        if let Some(snapshot) = progress.snapshot() {
                            println!("  文件 ID     {}", snapshot.request_id);
                            println!("  文件        {} {:.1}% · {}/{} MiB · {:.1} MiB/s",
                                snapshot.file, snapshot.percent(),
                                snapshot.written / 1_048_576, snapshot.total / 1_048_576,
                                snapshot.bytes_per_second() / 1_048_576.0);
                        } else {
                            println!("  文件        无进行中进度");
                        }
                        println!("══════════════════════════════");
                    }
                    "pwd" if args.len() == 1 => println!("{}", display_absolute(
                        Path::new(remote_root.as_deref().unwrap_or("")), &remote_cwd
                    )),
                    "lpwd" if args.len() == 1 => println!("{}", display_absolute(root.display_root(), &local_cwd)),
                    "lls" if args.len() <= 2 => {
                        let p = scoped_path(root.display_root(), &local_cwd, args.get(1).map(String::as_str).unwrap_or("."))?;
                        match list_in_root(&root, &relative_string(&p)?) {
                            Ok(names) => for name in names { println!("{name}"); },
                            Err(err) => println!("[LLS] {err:?}"),
                        }
                    }
                    "ls" if args.len() <= 2 => {
                        let p = scoped_remote(remote_root.as_deref(), &remote_cwd, args.get(1).map(String::as_str).unwrap_or("."))?;
                        match list_via_sdk(&session, relative_string(&p)?, next_id()).await {
                            Ok(names) => for name in names { println!("{name}"); },
                            Err(err) => println!("[LS] {err:?}"),
                        }
                    }
                    "lcd" if args.len() == 2 => {
                        let path = if args[1] == "-" { previous_local.clone() }
                            else { scoped_path(root.display_root(), &local_cwd, &args[1])? };
                        match list_in_root(&root, &relative_string(&path)?) {
                            Ok(_) => { previous_local = std::mem::replace(&mut local_cwd, path); }
                            Err(err) => println!("[LCD] {err:?}"),
                        }
                    }
                    "cd" if args.len() == 2 => {
                        let path = if args[1] == "-" { previous_remote.clone() }
                            else { scoped_remote(remote_root.as_deref(), &remote_cwd, &args[1])? };
                        match list_via_sdk(&session, relative_string(&path)?, next_id()).await {
                            Ok(_) => {
                                previous_remote = std::mem::replace(&mut remote_cwd, path);
                                if let Ok(mut prompt) = prompt_label.lock() {
                                    *prompt = format!("[{}] {}> ", link_mode, display_absolute(
                                            Path::new(remote_root.as_deref().unwrap_or("")), &remote_cwd
                                        ));
                                }
                            }
                            Err(err) => println!("[CD] {err:?}"),
                        }
                    }
                    "put" if (2..=3).contains(&args.len()) => {
                        let source = scoped_path(root.display_root(), &local_cwd, &args[1])?;
                        let suggested = source.file_name()
                            .ok_or("请输入有效的文件路径")?.to_string_lossy().to_string();
                        let remote = scoped_remote(remote_root.as_deref(), &remote_cwd, args.get(2).map(String::as_str).unwrap_or(&suggested))?;
                        if active.swap(true, Ordering::SeqCst) {
                            println!("[PUT] 当前已有文件任务");
                            continue;
                        }
                        let session = Arc::clone(&session);
                        let root = Arc::clone(&root);
                        let lanes = Arc::clone(&data_lanes);
                        let activity = Arc::clone(&active);
                        let lease = arbiter.clone();
                        let progress = Arc::clone(&progress);
                        let cancels = Arc::clone(&cancels);
                        progress.clear();
                crate::terminal::progress(None);
                        let batch_id = next_id();
                        active_batch_id = Some(batch_id);
                        task = Some(tokio::spawn(async move {
                            let result = with_batch_lease(&session, &lease, is_creator, batch_id,
                                put_path(&session, &lanes, &root, &source, &remote, &progress, &cancels)).await;
                            match result {
                                Ok(()) => println!("\n[PUT] 文件或目录任务完成"),
                                Err(err) => eprintln!("\n[PUT] 失败：{err}"),
                            }
                            activity.store(false, Ordering::SeqCst);
                        }));
                    }
                    "get" if (2..=3).contains(&args.len()) => {
                        let src = scoped_remote(remote_root.as_deref(), &remote_cwd, &args[1])?;
                        let suggested = src.file_name()
                            .ok_or("请输入有效的文件路径")?.to_string_lossy().to_string();
                        let local = scoped_path(root.display_root(), &local_cwd, args.get(2).map(String::as_str).unwrap_or(&suggested))?;
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
                        let progress = Arc::clone(&progress);
                        progress.clear();
                crate::terminal::progress(None);
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
                        let was_running = cancel_current(
                            &session, &mut task, &mut active_batch_id,
                            is_creator, &pending_downloads, &progress, &cancels,
                        ).await;
                        if was_running {
                            active.store(false, Ordering::SeqCst);
                            println!("[CANCEL] 取消流程已结束，会话保持连接");
                        } else {
                            println!("[CANCEL] 当前没有活动的文件任务");
                        }
                    }
                    _ => println!("命令不支持或参数错误；输入 help"),
                }
            }
            interrupt = tokio::signal::ctrl_c() => {
                match interrupt {
                    Ok(()) => {
                        let stopped = cancel_current(
                            &session, &mut task, &mut active_batch_id,
                            is_creator, &pending_downloads, &progress, &cancels,
                        ).await;
                        active.store(false, Ordering::SeqCst);
                        if stopped {
                            println!("\n[CANCEL] 取消流程已结束，会话继续保持");
                        } else {
                            println!("\n[CANCEL] 当前没有正在进行的传输，输入 quit 可退出");
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

    #[cfg(unix)]
    #[test]
    fn absolute_paths_are_scoped_to_explicit_share_root() {
        let root = Path::new("/home/test/approved");
        assert_eq!(
            scoped_path(root, Path::new("folder"), "/home/test/approved/file.bin").unwrap(),
            PathBuf::from("file.bin"),
        );
        assert!(scoped_path(root, Path::new(""), "/etc/passwd").is_err());
        assert_eq!(
            scoped_path(Path::new("/"), Path::new(""), "/etc/hosts").unwrap(),
            PathBuf::from("etc/hosts"),
        );
        assert!(scoped_remote(None, Path::new(""), "/outside").is_err());
        assert!(scoped_remote(None, Path::new(""), "relative").is_err());
    }
}
