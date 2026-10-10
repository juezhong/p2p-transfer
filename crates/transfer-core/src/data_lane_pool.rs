//! Long-lived, individually authenticated Data QUIC lanes.
//!
//! The creator supervises up to four Data connections. A lost lane is replaced
//! through mutual TLS + the session-specific two-way HMAC challenge, never by
//! reusing an unauthenticated QUIC stream. The joiner accepts replacements
//! under the SAME replay guard used by the initial Control/Data session.
//! Control remains a separate connection and is never replaced here.
//!
//! This manages Data QUIC lifetimes on an already-validated UDP path. It does
//! NOT claim to implement ICE restart, fresh candidate discovery, or the file
//! transfer's chunk/ACK recovery protocol.

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{atomic::{AtomicUsize, Ordering}, Arc},
    time::Duration,
};

use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::{sync::{watch, RwLock}, task::JoinSet};

use p2p_sdk::{
    channel::ChannelRole,
    manual_pairing::ManualPairing,
    peer_pin::PeerCertificatePin,
    session_binding::{
        authenticate_initiator, authenticate_responder, ReplayGuard, SessionCredentials,
    },
    verified_session::VerifiedManualSession,
    udp_owner::UdpOwner,
    quinn_socket::{demux_endpoint_config, QuinnUdpAdapter},
};

pub const MAX_DATA_LANES: usize = 4;
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(6);
const RETRY_INITIAL: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanePoolError {
    InvalidCapacity,
    SessionMismatch,
    ShuttingDown,
    TimedOut,
    Transport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataLaneStatus {
    pub active: usize,
    pub desired: usize,
    /// Increments when a connection is installed or removed.
    pub generation: u64,
}

struct LaneState {
    control: Connection,
    lanes: RwLock<Vec<Option<Connection>>>,
    changed: watch::Sender<DataLaneStatus>,
    next_lane: AtomicUsize,
}

impl LaneState {
    async fn publish(&self) {
        let lanes = self.lanes.read().await;
        let count = lanes.iter().filter(|v| v.as_ref().is_some_and(|c| c.close_reason().is_none())).count();
        let old = *self.changed.borrow();
        self.changed.send_replace(DataLaneStatus {
            active: count,
            desired: old.desired,
            generation: old.generation.wrapping_add(1),
        });
    }

    async fn install(&self, connection: Connection, preferred: Option<usize>) -> bool {
        if self.control.close_reason().is_some() || connection.close_reason().is_some() {
            return false;
        }
        let mut lanes = self.lanes.write().await;
        let position = preferred.filter(|&i| i < lanes.len()
                && lanes[i].as_ref().is_none_or(|old| old.close_reason().is_some()))
            .or_else(|| lanes.iter().position(|c| c.as_ref().is_none_or(|c| c.close_reason().is_some())));
        if let Some(index) = position {
            lanes[index] = Some(connection);
            drop(lanes);
            self.publish().await;
            true
        } else {
            false
        }
    }

    async fn clear(&self, index: usize, stable_id: usize) {
        let mut lanes = self.lanes.write().await;
        if lanes.get(index).and_then(Option::as_ref)
            .is_some_and(|c| c.stable_id() == stable_id)
        {
            lanes[index] = None;
            drop(lanes);
            self.publish().await;
        }
    }

    async fn available(&self) -> Vec<Connection> {
        self.lanes.read().await.iter()
            .flatten()
            .filter(|conn| conn.close_reason().is_none())
            .cloned()
            .collect()
    }
}

/// Dropping the supervisor stops retry/accept tasks without forcibly closing
/// the Control link. Explicit shutdown also closes managed Data links.
pub struct ResilientDataLanes {
    state: Arc<LaneState>,
    stop: watch::Sender<bool>,
}

impl ResilientDataLanes {
    fn initialize(
        session: &VerifiedManualSession,
        pairing: &ManualPairing,
        desired: usize,
    ) -> Result<(Self, watch::Receiver<bool>), LanePoolError> {
        if !(1..=MAX_DATA_LANES).contains(&desired) {
            return Err(LanePoolError::InvalidCapacity);
        }
        if session.session_id() != pairing.credentials.session_id()
            || session.remote_certificate_sha256() != pairing.remote_tls_cert_sha256
            || session.control().close_reason().is_some()
            || session.data().close_reason().is_some()
        {
            return Err(LanePoolError::SessionMismatch);
        }
        let mut slots = vec![None; desired];
        slots[0] = Some(session.data().clone());
        let (changed, _) = watch::channel(DataLaneStatus {
            active: 1, desired, generation: 0,
        });
        let (stop, stopped) = watch::channel(false);
        let pool = Self {
            state: Arc::new(LaneState {
                control: session.control().clone(),
                lanes: RwLock::new(slots),
                changed,
                next_lane: AtomicUsize::new(0),
            }),
            stop,
        };
        Ok((pool, stopped))
    }

    /// The creator performs all outgoing Data dial/repair attempts. Each
    /// successful replacement is mutually verified against the original peer
    /// certificate pin and fresh session-specific HMAC proof.
    pub fn start_creator(
        session: &VerifiedManualSession,
        endpoint: Endpoint,
        remote: SocketAddr,
        pairing: &ManualPairing,
        desired: usize,
    ) -> Result<Self, LanePoolError> {
        let (pool, stopped) = Self::initialize(session, pairing, desired)?;
        let credentials = pairing.credentials.clone();
        let remote_pin = pairing.remote_tls_cert_sha256;
        for index in 0..desired {
            let state = Arc::clone(&pool.state);
            let endpoint = endpoint.clone();
            let credentials = credentials.clone();
            let stopped = stopped.clone();
            tokio::spawn(async move {
                maintain_creator_lane(state, stopped, endpoint, remote, credentials, remote_pin, index).await;
            });
        }
        Ok(pool)
    }


    /// Like start_creator, but each additional authenticated Data QUIC uses
    /// its own UDP socket and source port bound to the ICE-nominated local IP.
    /// This matches Go's independent 5-tuple preference and permits one lane
    /// to lose its NAT mapping without taking down the base Control socket.
    ///
    /// The initial Data connection remains on the originally nominated socket.
    /// Extra lanes reauthenticate with the existing verified peer certificate
    /// pin and fresh HMAC challenges. A new UDP port is *not* an ICE nomination;
    /// if the NAT blocks it, the pool continues with remaining valid lanes.
    pub fn start_creator_with_independent_udp(
        session: &VerifiedManualSession,
        endpoint: Endpoint,
        remote: SocketAddr,
        pairing: &ManualPairing,
        tls: quinn::ClientConfig,
        desired: usize,
    ) -> Result<Self, LanePoolError> {
        let local_ip = endpoint.local_addr().map_err(|_| LanePoolError::Transport)?.ip();
        if local_ip.is_unspecified() || local_ip.is_multicast()
            || local_ip.is_ipv4() != remote.is_ipv4()
        {
            return Err(LanePoolError::Transport);
        }
        let (pool, stopped) = Self::initialize(session, pairing, desired)?;
        let credentials = pairing.credentials.clone();
        let remote_pin = pairing.remote_tls_cert_sha256;
        for index in 0..desired {
            let state = Arc::clone(&pool.state);
            let credentials = credentials.clone();
            let stopped = stopped.clone();
            if index == 0 {
                let endpoint = endpoint.clone();
                tokio::spawn(async move {
                    maintain_creator_lane(state, stopped, endpoint, remote,
                        credentials, remote_pin, index).await;
                });
            } else {
                let tls = tls.clone();
                let shared = endpoint.clone();
                tokio::spawn(async move {
                    maintain_independent_creator_lane(state, stopped,
                        (local_ip, remote), credentials, remote_pin, (tls, shared), index).await;
                });
            }
        }
        Ok(pool)
    }

    /// The joiner keeps accepting freshly authenticated Data connections.
    /// Pass the SAME replay guard used in establish_responder; replay entries
    /// must not be reset when one lane drops.
    pub fn start_joiner(
        session: &VerifiedManualSession,
        endpoint: Endpoint,
        pairing: &ManualPairing,
        replay_guard: Arc<ReplayGuard>,
        desired: usize,
    ) -> Result<Self, LanePoolError> {
        let (pool, stopped) = Self::initialize(session, pairing, desired)?;
        let state = Arc::clone(&pool.state);
        let credentials = pairing.credentials.clone();
        let pin = pairing.remote_tls_cert_sha256;
        tokio::spawn(async move {
            accept_joiner_lanes(state, stopped, endpoint, credentials, pin, replay_guard).await;
        });
        // Monitor initial Data lane; accepted replacement lanes get monitors
        // inside the accept task after successful mutual authentication.
        let initial = session.data().clone();
        let state = Arc::clone(&pool.state);
        let stop = pool.stop.subscribe();
        tokio::spawn(watch_installed_lane(state, stop, 0, initial));
        Ok(pool)
    }

    pub fn subscribe(&self) -> watch::Receiver<DataLaneStatus> {
        self.state.changed.subscribe()
    }

    /// Open a new bulk-data stream on a live authenticated lane. A later
    /// stream can use another lane even if the previous lane was disconnected.
    /// Transfer remains responsible for retransmitting any unacknowledged
    /// file bytes from streams broken during the fault.
    pub async fn open_uni(&self, deadline: Duration)
        -> Result<SendStream, LanePoolError>
    {
        let mut changed = self.subscribe();
        tokio::time::timeout(deadline, async {
            loop {
                if self.state.control.close_reason().is_some() {
                    return Err(LanePoolError::ShuttingDown);
                }
                let lanes = self.available().await;
                if !lanes.is_empty() {
                    let index = self.state.next_lane.fetch_add(1, Ordering::Relaxed) % lanes.len();
                    for offset in 0..lanes.len() {
                        let lane = &lanes[(index + offset) % lanes.len()];
                        // Control failure invalidates this authenticated session.
                        // A stalled Data open must not hide it until the caller's
                        // whole deadline expires. A lane replacement also wakes
                        // us to reconsider the currently usable connections.
                        tokio::select! {
                            result = lane.open_uni() => {
                                if let Ok(stream) = result {
                                    return Ok(stream);
                                }
                            }
                            _ = self.state.control.closed() => {
                                return Err(LanePoolError::ShuttingDown);
                            }
                            update = changed.changed() => {
                                update.map_err(|_| LanePoolError::ShuttingDown)?;
                                continue;
                            }
                        }
                    }
                }
                changed.changed().await.map_err(|_| LanePoolError::ShuttingDown)?;
            }
        }).await.map_err(|_| LanePoolError::TimedOut)?
    }

    /// Receive one bulk-data stream from whichever independently authenticated
    /// lane the peer selected. For now a Transfer session must have exactly
    /// ONE centralized Data stream dispatcher; concurrent per-request callers
    /// could race and consume a different request's stream.
    pub async fn accept_uni(&self, deadline: Duration)
        -> Result<RecvStream, LanePoolError>
    {
        tokio::time::timeout(deadline, async {
            let mut changed = self.subscribe();
            let mut observed = HashSet::new();
            let mut accepts = JoinSet::new();
            loop {
                if self.state.control.close_reason().is_some() {
                    return Err(LanePoolError::ShuttingDown);
                }
                for lane in self.available().await {
                    let id = lane.stable_id();
                    if observed.insert(id) {
                        accepts.spawn(async move { (id, lane.accept_uni().await) });
                    }
                }
                tokio::select! {
                    biased;
                    received = accepts.join_next(), if !accepts.is_empty() => {
                        if let Some(Ok((id, result))) = received {
                            observed.remove(&id);
                            if let Ok(stream) = result { return Ok(stream); }
                        }
                    }
                    result = changed.changed() => {
                        result.map_err(|_| LanePoolError::ShuttingDown)?;
                    }
                    _ = self.state.control.closed() => {
                        return Err(LanePoolError::ShuttingDown);
                    }
                }
            }
        }).await.map_err(|_| LanePoolError::TimedOut)?
    }

    /// Snapshot returns live connections, not just those last counted before
    /// a fault. The caller may distribute new file streams across these lanes.
    pub async fn available(&self) -> Vec<Connection> {
        self.state.available().await
    }

    pub async fn wait_for_count(&self, minimum: usize, deadline: Duration)
        -> Result<Vec<Connection>, LanePoolError>
    {
        if minimum == 0 || minimum > self.state.changed.borrow().desired {
            return Err(LanePoolError::InvalidCapacity);
        }
        let mut status = self.subscribe();
        tokio::time::timeout(deadline, async {
            loop {
                // Never advertise a healthy Data pool after Control has died.
                // In particular, an already-full pool must not pass readiness.
                if self.state.control.close_reason().is_some() {
                    return Err(LanePoolError::ShuttingDown);
                }
                let active = self.available().await;
                if active.len() >= minimum {
                    return Ok(active);
                }
                tokio::select! {
                    update = status.changed() => {
                        update.map_err(|_| LanePoolError::ShuttingDown)?;
                    }
                    _ = self.state.control.closed() => {
                        return Err(LanePoolError::ShuttingDown);
                    }
                }
            }
        }).await.map_err(|_| LanePoolError::TimedOut)?
    }

    /// Stop background repair and close only Data connections; Control
    /// remains owned by the verified session.
    pub async fn shutdown(&self) {
        self.stop.send_replace(true);
        let mut lanes = self.state.lanes.write().await;
        for conn in lanes.iter_mut().filter_map(Option::take) {
            conn.close(0u32.into(), b"data pool stopped");
        }
        drop(lanes);
        self.state.publish().await;
    }
}

impl Drop for ResilientDataLanes {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

async fn watch_installed_lane(
    state: Arc<LaneState>,
    mut stopped: watch::Receiver<bool>,
    index: usize,
    lane: Connection,
) {
    tokio::select! {
        _ = lane.closed() => state.clear(index, lane.stable_id()).await,
        _ = state.control.closed() => {},
        _ = stopped.changed() => {},
    }
}

async fn maintain_creator_lane(
    state: Arc<LaneState>,
    mut stopped: watch::Receiver<bool>,
    endpoint: Endpoint,
    remote: SocketAddr,
    credentials: SessionCredentials,
    pin: [u8; 32],
    index: usize,
) {
    let mut delay = RETRY_INITIAL;
    loop {
        if *stopped.borrow() || state.control.close_reason().is_some() {
            break;
        }
        let current = state.lanes.read().await[index].clone();
        if let Some(connection) = current.filter(|c| c.close_reason().is_none()) {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = state.control.closed() => break,
                _ = connection.closed() => {
                    state.clear(index, connection.stable_id()).await;
                }
            }
            continue;
        }
        let attempt = async {
            let connection = endpoint.connect(remote, "localhost").map_err(|_| ())?
                .await.map_err(|_| ())?;
            if PeerCertificatePin::new(pin).map_err(|_| ())?
                .verify_connection(&connection).is_err()
            {
                connection.close(1u32.into(), b"wrong peer certificate");
                return Err(());
            }
            let clone = connection.clone();
            match authenticate_initiator(connection, &credentials, ChannelRole::Data, HANDSHAKE_DEADLINE).await {
                Ok(_) => Ok(clone),
                Err(_) => {
                    clone.close(1u32.into(), b"data session proof failed");
                    Err(())
                }
            }
        };
        let connected = tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            result = tokio::time::timeout(HANDSHAKE_DEADLINE, attempt) => result.ok().and_then(Result::ok),
        };
        if let Some(connection) = connected {
            if state.install(connection.clone(), Some(index)).await {
                delay = RETRY_INITIAL;
                continue;
            }
            connection.close(1u32.into(), b"data lane not needed");
        }
        tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            _ = tokio::time::sleep(delay) => {},
        }
        delay = delay.saturating_mul(2).min(RETRY_MAX);
    }
}

/// Build and release each independent UDP port with its Data lane.
/// On true disconnection the next attempt takes a fresh source port, so a
/// broken NAT mapping or stale 5-tuple cannot pin all lanes to one mapping.
async fn maintain_independent_creator_lane(
    state: Arc<LaneState>,
    mut stopped: watch::Receiver<bool>,
    addresses: (std::net::IpAddr, SocketAddr),
    credentials: SessionCredentials,
    pin: [u8; 32],
    transport: (quinn::ClientConfig, Endpoint),
    index: usize,
) {
    let (local_ip, remote) = addresses;
    let (tls, shared) = transport;
    let mut delay = RETRY_INITIAL;
    loop {
        if *stopped.borrow() || state.control.close_reason().is_some() {
            break;
        }
        let current = { state.lanes.read().await[index].clone() };
        if let Some(connection) = current.filter(|c| c.close_reason().is_none()) {
            tokio::select! {
                _ = stopped.changed() => break,
                _ = state.control.closed() => break,
                _ = connection.closed() => {
                    state.clear(index, connection.stable_id()).await;
                }
            }
            continue;
        }
        let result = async {
            let mut owner = UdpOwner::bind(SocketAddr::new(local_ip, 0))
                .await.map_err(|_| ())?;
            let adapter = QuinnUdpAdapter::from_owner(&mut owner).map_err(|_| ())?;
            let mut endpoint = Endpoint::new_with_abstract_socket(
                demux_endpoint_config(), None, Arc::new(adapter),
                quinn::default_runtime().ok_or(())?,
            ).map_err(|_| ())?;
            endpoint.set_default_client_config(tls.clone());
            let connection = endpoint.connect(remote, "localhost")
                .map_err(|_| ())?.await.map_err(|_| ())?;
            if PeerCertificatePin::new(pin).map_err(|_| ())?
                .verify_connection(&connection).is_err()
            {
                connection.close(1u32.into(), b"wrong peer certificate");
                return Err(());
            }
            let clone = connection.clone();
            match authenticate_initiator(
                connection, &credentials, ChannelRole::Data, HANDSHAKE_DEADLINE,
            ).await {
                Ok(_) => Ok((owner, endpoint, clone)),
                Err(_) => {
                    clone.close(1u32.into(), b"data session proof failed");
                    Err(())
                }
            }
        };
        let connected = tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            result = tokio::time::timeout(HANDSHAKE_DEADLINE, result) =>
                result.ok().and_then(Result::ok),
        };
        if let Some((_owner, _endpoint, connection)) = connected {
            if state.install(connection.clone(), Some(index)).await {
                delay = RETRY_INITIAL;
                // These values must stay alive for the entire QUIC lane.
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = state.control.closed() => break,
                    _ = connection.closed() => {}
                }
                state.clear(index, connection.stable_id()).await;
                continue;
            }
            connection.close(1u32.into(), b"data lane not needed");
        }

        // Go falls back to the known-good Control UDP path when a fresh
        // source port is blocked by NAT or a stateful firewall. The fallback
        // is still a NEW QUIC connection with its own mTLS and HMAC proof.
        // Never mark an unverified candidate or TLS-only link as available.
        let shared_attempt = async {
            let connection = shared.connect(remote, "localhost")
                .map_err(|_| ())?.await.map_err(|_| ())?;
            if PeerCertificatePin::new(pin).map_err(|_| ())?
                .verify_connection(&connection).is_err()
            {
                connection.close(1u32.into(), b"wrong peer certificate");
                return Err(());
            }
            let clone = connection.clone();
            if authenticate_initiator(
                connection, &credentials, ChannelRole::Data, HANDSHAKE_DEADLINE,
            ).await.is_err() {
                clone.close(1u32.into(), b"data session proof failed");
                return Err(());
            }
            Ok::<Connection, ()>(clone)
        };
        let fallback = tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            result = tokio::time::timeout(HANDSHAKE_DEADLINE, shared_attempt) =>
                result.ok().and_then(Result::ok),
        };
        if let Some(connection) = fallback {
            if state.install(connection.clone(), Some(index)).await {
                delay = RETRY_INITIAL;
                tokio::select! {
                    _ = stopped.changed() => break,
                    _ = state.control.closed() => break,
                    _ = connection.closed() => {}
                }
                state.clear(index, connection.stable_id()).await;
                continue;
            }
            connection.close(1u32.into(), b"data lane not needed");
        }
        tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            _ = tokio::time::sleep(delay) => {},
        }
        delay = delay.saturating_mul(2).min(RETRY_MAX);
    }
}

async fn accept_joiner_lanes(
    state: Arc<LaneState>,
    mut stopped: watch::Receiver<bool>,
    endpoint: Endpoint,
    credentials: SessionCredentials,
    pin: [u8; 32],
    guard: Arc<ReplayGuard>,
) {
    loop {
        let connecting = tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            connecting = endpoint.accept() => connecting,
        };
        let Some(connecting) = connecting else { break };
        let result = tokio::select! {
            _ = stopped.changed() => break,
            _ = state.control.closed() => break,
            result = tokio::time::timeout(HANDSHAKE_DEADLINE, async {
                let connection = connecting.await.map_err(|_| ())?;
                let clone = connection.clone();
                if PeerCertificatePin::new(pin).map_err(|_| ())?
                    .verify_connection(&connection).is_err()
                {
                    connection.close(1u32.into(), b"wrong peer certificate");
                    return Err(());
                }
                match authenticate_responder(
                    connection, &credentials, ChannelRole::Data, &guard, HANDSHAKE_DEADLINE
                ).await {
                    Ok(_) => Ok(clone),
                    Err(_) => {
                        clone.close(1u32.into(), b"data session proof failed");
                        Err(())
                    }
                }
            }) => result.ok().and_then(Result::ok),
        };
        let Some(connection) = result else { continue };
        // Ensure concurrent authenticated arrivals cannot exceed configured
        // capacity; installing a replacement only reuses a dead slot.
        let index = {
            let slots = state.lanes.read().await;
            slots.iter().position(|s| s.as_ref().is_none_or(|c| c.close_reason().is_some()))
        };
        if let Some(index) = index {
            if state.install(connection.clone(), Some(index)).await {
                tokio::spawn(watch_installed_lane(
                    Arc::clone(&state), stopped.clone(), index, connection,
                ));
                continue;
            }
        }
        connection.close(1u32.into(), b"data lane capacity reached");
    }
}
