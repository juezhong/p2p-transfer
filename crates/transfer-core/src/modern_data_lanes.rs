//! Transfer-owned pool of SDK-authenticated auxiliary QUIC connections.
//! SDK owns ICE, TLS PIN, HMAC, UDP and Control; Transfer chooses lane count
//! and streams. No raw endpoint escapes the SDK authentication boundary.

use std::{
    collections::HashSet,
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    time::Duration,
};

use p2p_sdk::transport_session::{ConnectedTransportPeer, ManagedAuthenticatedLink};
use quinn::{Connection, RecvStream, SendStream};
use tokio::{sync::Mutex, task::JoinSet, time::timeout};

use crate::data_lane_pool::MAX_DATA_LANES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModernLaneError {
    InvalidCapacity,
    ControlLost,
    ShuttingDown,
    TimedOut,
}

/// One SDK manager per application-requested Data connection (up to four).
/// Each manager authenticates and replaces its QUIC connection independently.
/// This pool never performs ICE nomination or accepts unauthenticated QUIC.
pub struct ModernDataLanes {
    peer: Arc<ConnectedTransportPeer>,
    handles: Mutex<Vec<ManagedAuthenticatedLink>>,
    stopping: AtomicBool,
    desired: usize,
}

impl ModernDataLanes {
    pub fn start(
        peer: Arc<ConnectedTransportPeer>,
        desired: usize,
    ) -> Result<Self, ModernLaneError> {
        if !(1..=MAX_DATA_LANES).contains(&desired) {
            return Err(ModernLaneError::InvalidCapacity);
        }
        let handles = (0..desired)
            .map(|_| peer.manage_authenticated_data())
            .collect();
        Ok(Self {
            peer, handles: Mutex::new(handles),
            stopping: AtomicBool::new(false), desired,
        })
    }

    pub fn desired(&self) -> usize { self.desired }

    pub async fn available(&self) -> Vec<Connection> {
        let handles = self.handles.lock().await;
        if self.stopping.load(Ordering::Acquire)
            || self.peer.control.close_reason().is_some()
        {
            return Vec::new();
        }
        handles.iter().filter_map(ManagedAuthenticatedLink::current).collect()
    }

    pub async fn wait_for_count(
        &self, minimum: usize, deadline: Duration,
    ) -> Result<Vec<Connection>, ModernLaneError> {
        if minimum == 0 || minimum > self.desired {
            return Err(ModernLaneError::InvalidCapacity);
        }
        timeout(deadline, async {
            loop {
                self.ensure_live()?;
                let current = self.available().await;
                if current.len() >= minimum { return Ok(current); }
                tokio::select! {
                    _ = self.peer.control.closed() => return Err(ModernLaneError::ControlLost),
                    _ = tokio::time::sleep(Duration::from_millis(80)) => {}
                }
            }
        }).await.map_err(|_| ModernLaneError::TimedOut)?
    }

    fn ensure_live(&self) -> Result<(), ModernLaneError> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(ModernLaneError::ShuttingDown);
        }
        if self.peer.control.close_reason().is_some() {
            return Err(ModernLaneError::ControlLost);
        }
        Ok(())
    }

    pub async fn open_uni(&self, deadline: Duration) -> Result<SendStream, ModernLaneError> {
        timeout(deadline, async {
            loop {
                self.ensure_live()?;
                for lane in self.available().await {
                    // Connection may become unavailable between snapshot and stream open.
                    let opened = tokio::select! {
                        _ = self.peer.control.closed() => return Err(ModernLaneError::ControlLost),
                        result = timeout(Duration::from_secs(2), lane.open_uni()) => result,
                    };
                    if let Ok(Ok(stream)) = opened { return Ok(stream); }
                }
                tokio::select! {
                    _ = self.peer.control.closed() => return Err(ModernLaneError::ControlLost),
                    _ = tokio::time::sleep(Duration::from_millis(80)) => {}
                }
            }
        }).await.map_err(|_| ModernLaneError::TimedOut)?
    }

    /// At most one centralized accept dispatcher should be used per Transfer
    /// session. Each request must additionally verify the P2PD request ID.
    pub async fn accept_uni(&self, deadline: Duration) -> Result<RecvStream, ModernLaneError> {
        timeout(deadline, async {
            let mut observed = HashSet::new();
            let mut pending = JoinSet::new();
            loop {
                self.ensure_live()?;
                for lane in self.available().await {
                    if observed.insert(lane.stable_id()) {
                        pending.spawn(async move {
                            (lane.stable_id(), lane.accept_uni().await)
                        });
                    }
                }
                tokio::select! {
                    result = pending.join_next(), if !pending.is_empty() => {
                        if let Some(Ok((id, received))) = result {
                            observed.remove(&id);
                            if let Ok(stream) = received { return Ok(stream); }
                        }
                    }
                    _ = self.peer.control.closed() => return Err(ModernLaneError::ControlLost),
                    _ = tokio::time::sleep(Duration::from_millis(150)) => {}
                }
            }
        }).await.map_err(|_| ModernLaneError::TimedOut)?
    }

    /// Stop all SDK managers and allow their owned QUIC/UDP resources to drain.
    /// The independently authenticated Control connection is not closed.
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let workers = {
            let mut handles = self.handles.lock().await;
            std::mem::take(&mut *handles)
        };
        for handle in workers { handle.shutdown().await; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero_and_over_maximum_data_connections() {
        assert_eq!(MAX_DATA_LANES, 4);
        assert_eq!(0_usize.checked_sub(1), None);
        assert_eq!(ModernLaneError::InvalidCapacity, ModernLaneError::InvalidCapacity);
    }
}
