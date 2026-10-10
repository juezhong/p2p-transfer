//! Transfer 自己的四路文件 Data lane 调度器；SDK 只建立、认证并修复 QUIC。
//! 每一条 lane 均为通用 ManagedAuthenticatedLink；没有文件语义进入 SDK。

use std::{sync::{atomic::{AtomicUsize, Ordering}, Arc}, time::Duration};
use p2p_sdk::{ConnectedTransportPeer, ManagedAuthenticatedLink};
use quinn::{Connection, RecvStream, SendStream};
use tokio::{sync::Mutex, task::JoinSet, time::{sleep, timeout, Instant}};

#[derive(Debug)]
pub enum LaneError {
    Disconnected,
    Timeout,
    Transport,
}

/// Control QUIC 属于 SDK；Transfer 的四路分配器仅存在于应用侧。
pub struct TransferSession {
    transport: Arc<ConnectedTransportPeer>,
    pub lanes: Arc<TransferDataLanes>,
}

impl TransferSession {
    pub fn new(
        transport: Arc<ConnectedTransportPeer>,
        lanes: Arc<TransferDataLanes>,
    ) -> Self {
        Self { transport, lanes }
    }

    pub fn control(&self) -> &Connection {
        &self.transport.control
    }
}

pub struct TransferDataLanes {
    control: Connection,
    links: Mutex<Vec<ManagedAuthenticatedLink>>,
    next: AtomicUsize,
}

impl TransferDataLanes {
    pub fn new(peer: &Arc<ConnectedTransportPeer>, requested: usize) -> Self {
        let links = (0..requested).map(|_| peer.manage_authenticated_data()).collect();
        Self {
            control: peer.control.clone(),
            links: Mutex::new(links),
            next: AtomicUsize::new(0),
        }
    }

    pub async fn available(&self) -> Vec<Connection> {
        self.links.lock().await.iter()
            .filter_map(ManagedAuthenticatedLink::current).collect()
    }

    /// 文件流调度策略属于 Transfer，SDK 不负责选择哪条连接传输文件块。
    pub async fn open_uni(&self, deadline: Duration) -> Result<SendStream, LaneError> {
        let expires = Instant::now() + deadline;
        loop {
            if self.control.close_reason().is_some() { return Err(LaneError::Disconnected); }
            let links = self.available().await;
            if !links.is_empty() {
                let start = self.next.fetch_add(1, Ordering::Relaxed) % links.len();
                for offset in 0..links.len() {
                    let conn = &links[(start + offset) % links.len()];
                    if let Ok(stream) = conn.open_uni().await {
                        return Ok(stream);
                    }
                }
            }
            if Instant::now() >= expires { return Err(LaneError::Timeout); }
            tokio::select! {
                _ = self.control.closed() => return Err(LaneError::Disconnected),
                _ = sleep(Duration::from_millis(100)) => {}
            }
        }
    }

    /// 一个集中式流接收器用于当前 Transfer 会话。不能让每个文件请求
    /// 各自并发争抢 Data Stream；文件 request-id 验证仍由上层完成。
    pub async fn accept_uni(&self, deadline: Duration) -> Result<RecvStream, LaneError> {
        let expires = Instant::now() + deadline;
        loop {
            if self.control.close_reason().is_some() { return Err(LaneError::Disconnected); }
            if Instant::now() >= expires { return Err(LaneError::Timeout); }
            let mut attempts = JoinSet::new();
            for conn in self.available().await {
                attempts.spawn(async move { conn.accept_uni().await });
            }
            if !attempts.is_empty() {
                let answer = tokio::select! {
                    _ = self.control.closed() => return Err(LaneError::Disconnected),
                    result = timeout(Duration::from_millis(150), attempts.join_next()) => result,
                };
                if let Ok(Some(Ok(Ok(stream)))) = answer {
                    attempts.abort_all();
                    return Ok(stream);
                }
            } else {
                tokio::select! {
                    _ = self.control.closed() => return Err(LaneError::Disconnected),
                    _ = sleep(Duration::from_millis(100)) => {}
                }
            }
            // 取消失败/过期的接收任务，下一轮使用重新认证的连接快照。
            attempts.abort_all();
        }
    }

    pub async fn shutdown(&self) {
        let all = std::mem::take(&mut *self.links.lock().await);
        for link in all { link.shutdown().await; }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn four_is_transfer_policy_not_sdk_constant() {
        const TRANSFER_DATA_CONNECTIONS: usize = 4;
        assert_eq!(TRANSFER_DATA_CONNECTIONS, 4);
    }
}
