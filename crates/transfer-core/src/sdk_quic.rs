//! Bind Transfer's independent control/data streams to an SDK-verified session.
//!
//! No QUIC endpoint, TLS verifier or ICE agent is created in this crate.
//! Only an already authorized `VerifiedManualSession` may be used.
//! The caller must enforce a SINGLE active Transfer operation per session
//! until multiplexed request-ID scheduling and repair are implemented.

use std::{
    collections::{HashSet, VecDeque},
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
    sync::{Arc, Mutex},
    time::Duration,
};

use p2p_sdk::{
    transport_session::ConnectedTransportPeer,
    verified_session::VerifiedManualSession,
};

/// Only SDK-authenticated sessions may enter Transfer's Control RPC layer.
/// This sealed trait does not permit arbitrary raw Quinn connections to
/// bypass the SDK's mTLS certificate PIN + session HMAC checks.
mod private {
    use super::{Arc, ConnectedTransportPeer, VerifiedManualSession};
    pub trait Sealed {}
    impl Sealed for VerifiedManualSession {}
    impl Sealed for ConnectedTransportPeer {}
    impl<T: Sealed + ?Sized> Sealed for Arc<T> {}
}

/// Control is always required. The old dual-QUIC SDK session supplies its
/// already-authenticated initial Data connection; the modern Control-only SDK
/// session must use an explicitly authenticated Transfer-managed Data lane.
/// This is a protocol boundary, NOT a raw Quinn Connection adapter.
pub trait AuthenticatedSession: private::Sealed + Send + Sync {
    fn control(&self) -> &quinn::Connection;
    fn initial_data(&self) -> Option<&quinn::Connection>;
}

impl AuthenticatedSession for VerifiedManualSession {
    fn control(&self) -> &quinn::Connection {
        VerifiedManualSession::control(self)
    }
    fn initial_data(&self) -> Option<&quinn::Connection> {
        Some(VerifiedManualSession::data(self))
    }
}

impl AuthenticatedSession for ConnectedTransportPeer {
    fn control(&self) -> &quinn::Connection { &self.control }
    fn initial_data(&self) -> Option<&quinn::Connection> { None }
}

impl<T: AuthenticatedSession + ?Sized> AuthenticatedSession for Arc<T> {
    fn control(&self) -> &quinn::Connection { T::control(self) }
    fn initial_data(&self) -> Option<&quinn::Connection> {
        T::initial_data(self)
    }
}
use crate::data_lane_pool::ResilientDataLanes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    lease::LeaseGuard,
    protocol::{FrameKind, Frame},
    rpc::{self, RpcRequest},
    secure_io::SharedRoot,
    stream_transfer::{
        read_frame, receive_file_after_offer, send_file,
        write_frame, TransferError, TransferReceipt,
    },
};

const DATA_PREFACE: [u8; 4] = *b"P2PD";
const DATA_STREAM_ID_LEN: usize = DATA_PREFACE.len() + 8;

fn data_preface(request_id: u64) -> [u8; DATA_STREAM_ID_LEN] {
    let mut bytes = [0u8; DATA_STREAM_ID_LEN];
    bytes[..4].copy_from_slice(&DATA_PREFACE);
    bytes[4..].copy_from_slice(&request_id.to_be_bytes());
    bytes
}

fn matches_data_preface(bytes: &[u8; DATA_STREAM_ID_LEN], expected_id: u64) -> bool {
    expected_id != 0 && *bytes == data_preface(expected_id)
}

#[derive(Debug)]
pub enum SdkTransferError {
    ControlConnection,
    DataConnection,
    InvalidDataStream,
    Io(io::Error),
    Transfer(TransferError),
}

impl From<TransferError> for SdkTransferError {
    fn from(value: TransferError) -> Self {
        Self::Transfer(value)
    }
}
impl From<io::Error> for SdkTransferError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// A Quinn bidirectional stream is represented by independent SendStream
/// and RecvStream handles. This wrapper allows the existing transfer engine
/// to operate on the control connection without owning QUIC itself.
pub struct ControlIo<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> ControlIo<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }
}

impl<R: AsyncRead + Unpin, W: Unpin> AsyncRead for ControlIo<R, W> {
    fn poll_read(
        mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(cx, buf)
    }
}

impl<R: Unpin, W: AsyncWrite + Unpin> AsyncWrite for ControlIo<R, W> {
    fn poll_write(
        mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(cx)
    }
}

/// Initiator opens one Control QUIC bi-stream and a different Data QUIC
/// uni-stream. The small magic prefix wakes the data acceptor BEFORE
/// waiting for the first control ACK, avoiding an opening deadlock.
pub async fn send_via_sdk(
    session: &dyn AuthenticatedSession,
    root: &SharedRoot,
    source: &Path,
    remote_destination: &str,
    request_id: u64,
) -> Result<TransferReceipt, SdkTransferError> {
    send_on_managed_lane(session, None, root, source, remote_destination, request_id).await
}

/// Choose a fresh authenticated Data lane for each new file operation. The
/// SDK maintains the four QUIC connections and heals broken ones; Transfer
/// still owns chunk ACK/retransmit and cannot yet resume an interrupted file.
pub async fn send_via_managed_sdk(
    session: &dyn AuthenticatedSession,
    lanes: &ResilientDataLanes,
    root: &SharedRoot,
    source: &Path,
    remote_destination: &str,
    request_id: u64,
) -> Result<TransferReceipt, SdkTransferError> {
    send_on_managed_lane(session, Some(lanes), root, source, remote_destination, request_id).await
}

async fn send_on_managed_lane(
    session: &dyn AuthenticatedSession,
    lanes: Option<&ResilientDataLanes>,
    root: &SharedRoot,
    source: &Path,
    remote_destination: &str,
    request_id: u64,
) -> Result<TransferReceipt, SdkTransferError> {
    let (control_send, control_recv) = session.control().open_bi().await
        .map_err(|_| SdkTransferError::ControlConnection)?;
    let mut control = ControlIo::new(control_recv, control_send);
    let mut data = if let Some(pool) = lanes {
        pool.open_uni(Duration::from_secs(30)).await
            .map_err(|_| SdkTransferError::DataConnection)?
    } else {
        session.initial_data().ok_or(SdkTransferError::DataConnection)?
            .open_uni().await.map_err(|_| SdkTransferError::DataConnection)?
    };
    AsyncWriteExt::write_all(&mut data, &data_preface(request_id)).await
        .map_err(SdkTransferError::Io)?;
    let receipt = send_file(
        &mut control, &mut data, root, source, remote_destination, request_id,
    ).await?;
    data.finish().map_err(|_| SdkTransferError::DataConnection)?;
    Ok(receipt)
}

/// Responder only accepts streams belonging to this *already-verified*
/// Control/Data session. This revision supports one outstanding transfer;
/// request-id routing and concurrent streams will follow.
pub async fn receive_via_sdk(
    session: &dyn AuthenticatedSession,
    root: &SharedRoot,
) -> Result<TransferReceipt, SdkTransferError> {
    let (control_send, control_recv) = session.control().accept_bi().await
        .map_err(|_| SdkTransferError::ControlConnection)?;
    let mut control = ControlIo::new(control_recv, control_send);
    let offer = read_frame(&mut control).await?;
    let mut data = read_data_preface(session, None, offer.request_id).await?;
    receive_file_after_offer(offer, &mut control, &mut data, root)
        .await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn data_stream_is_bound_to_its_control_request_id() {
        let id = 77;
        let preface = data_preface(id);
        assert!(matches_data_preface(&preface, id));
        assert!(!matches_data_preface(&preface, id + 1));
        assert!(!matches_data_preface(&preface, 0));
        let mut corrupted = preface;
        corrupted[2] ^= 1;
        assert!(!matches_data_preface(&corrupted, id));
    }

    #[tokio::test]
    async fn wraps_separate_control_input_and_output_handles() {
        let (socket_a, socket_b) = duplex(4096);
        let (read_a, write_a) = tokio::io::split(socket_a);
        let (mut read_b, mut write_b) = tokio::io::split(socket_b);
        let mut control = ControlIo::new(read_a, write_a);
        control.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        read_b.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        write_b.write_all(b"world").await.unwrap();
        control.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"world");
    }
}


#[derive(Debug)]
pub enum IncomingResult {
    Received(TransferReceipt),
    ServedGet(TransferReceipt),
    DirectoryListed,
}

async fn read_data_preface(
    session: &dyn AuthenticatedSession,
    lanes: Option<&ResilientDataLanes>,
    request_id: u64,
) -> Result<quinn::RecvStream, SdkTransferError> {
    let mut data = if let Some(pool) = lanes {
        pool.accept_uni(Duration::from_secs(120)).await
            .map_err(|_| SdkTransferError::DataConnection)?
    } else {
        session.initial_data().ok_or(SdkTransferError::DataConnection)?
            .accept_uni().await.map_err(|_| SdkTransferError::DataConnection)?
    };
    let mut preface = [0u8; DATA_STREAM_ID_LEN];
    AsyncReadExt::read_exact(&mut data, &mut preface).await?;
    // A Data stream from a different Control request must never be passed
    // into a file sink, even when both streams are from the same TLS peer.
    if !matches_data_preface(&preface, request_id) {
        return Err(SdkTransferError::InvalidDataStream);
    }
    Ok(data)
}

/// Grants are retained for the entire remote batch. An Arc is shared by
/// all concurrent Control RPC handlers on the authoritative creator.
/// A bounded cancellation tombstone makes release-before-acquire safe:
/// an aborted client may send Release while its earlier Acquire RPC is still
/// in flight on another Control stream. That Acquire must never create a
/// stranded remote grant after the cancellation.
#[derive(Default)]
pub struct RemoteLeaseState {
    active: Option<LeaseGuard>,
    canceled: VecDeque<u64>,
    expected_local_gets: Arc<Mutex<HashSet<u64>>>,
}

/// A pending locally initiated GET authorizes only its own response ID.
/// Dropping the task guard revokes access even on Ctrl-C or async abort.
pub struct ExpectedLocalGet {
    request_id: u64,
    pending: Arc<Mutex<HashSet<u64>>>,
}

impl Drop for ExpectedLocalGet {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.request_id);
        }
    }
}

pub type RemoteLeaseGrant = std::sync::Arc<tokio::sync::Mutex<RemoteLeaseState>>;

impl RemoteLeaseState {
    /// Register before sending a GET RPC so its response cannot race us.
    /// The returned RAII guard must live until verification or cancellation.
    pub fn expect_local_get(&self, request_id: u64) -> Option<ExpectedLocalGet> {
        if request_id == 0 {
            return None;
        }
        let mut pending = self.expected_local_gets.lock().ok()?;
        if !pending.insert(request_id) {
            return None;
        }
        Some(ExpectedLocalGet {
            request_id,
            pending: Arc::clone(&self.expected_local_gets),
        })
    }

    fn permits_local_get(&self, request_id: u64) -> bool {
        self.expected_local_gets.lock()
            .map(|pending| pending.contains(&request_id))
            .unwrap_or(false)
    }

    fn permits_inbound_file(&self) -> bool {
        self.active.is_some()
    }

    fn acquire(&mut self, arbiter: &crate::lease::TransferLease, id: u64) -> bool {
        if id == 0 || self.active.is_some() || self.canceled.contains(&id) {
            return false;
        }
        if let Ok(grant) = arbiter.try_acquire(id) {
            self.active = Some(grant);
            true
        } else {
            false
        }
    }

    /// Unknown Releases still install a cancellation tombstone to prevent
    /// an earlier inflight Acquire from winning a race with the Release.
    fn release(&mut self, id: u64) -> bool {
        if id == 0 {
            return false;
        }
        if let Some(active) = &self.active {
            if active.request_id() != id { return false; }
        }
        self.active = None;
        if !self.canceled.contains(&id) {
            if self.canceled.len() == 128 { self.canceled.pop_front(); }
            self.canceled.push_back(id);
        }
        true
    }
}

/// The creator can receive a peer's PUT under a remote grant OR receive
/// the peer's data response to a locally initiated GET with a matching
/// outstanding request ID. A local lease alone must never authorize an
/// unrelated inbound PUT. Remote batch grants still need per-file binding.
fn inbound_file_authorized(
    arbiter: &crate::lease::TransferLease,
    remote: &RemoteLeaseState,
    request_id: u64,
) -> bool {
    remote.permits_inbound_file()
        || (arbiter.active_request().is_some() && remote.permits_local_get(request_id))
}

/// Dispatch one already accepted Control QUIC stream. This makes an ongoing
/// Control session capable of answering directory and GET requests while a
/// separate Data QUIC transport handles file payload.
pub async fn serve_control_stream(
    session: &dyn AuthenticatedSession,
    root: &SharedRoot,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
) -> Result<IncomingResult, SdkTransferError> {
    serve_control_stream_with_lease(session, root, send, recv, None, None).await
}

/// Creator-side dispatcher enforces one remote transfer grant at a time.
/// A regular responder retains the standard functionality with no arbiter.
pub async fn serve_control_stream_with_lease(
    session: &dyn AuthenticatedSession,
    root: &SharedRoot,
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    lease: Option<(&crate::lease::TransferLease, &RemoteLeaseGrant)>,
    lanes: Option<&ResilientDataLanes>,
) -> Result<IncomingResult, SdkTransferError> {
    let mut control = ControlIo::new(recv, send);
    let first = read_frame(&mut control).await?;
    match first.kind {
        FrameKind::TransferControl => {
            // The authoritative creator must reject unsolicited file payload
            // even if the peer passed TLS/session authentication. A matching
            // transfer lease is an additional application authorization gate.
            if let Some((arbiter, grants)) = lease {
                // Incoming DATA may answer an outstanding creator GET.
                // A local lease without a matching request ID is not enough.
                let authorized = inbound_file_authorized(
                    arbiter, &*grants.lock().await, first.request_id,
                );
                if !authorized {
                    write_frame(
                        &mut control, FrameKind::Error, first.request_id,
                        b"transfer lease required".to_vec(),
                    ).await?;
                    return Err(SdkTransferError::InvalidDataStream);
                }
            }
            let mut data = read_data_preface(session, lanes, first.request_id).await?;
            Ok(IncomingResult::Received(
                receive_file_after_offer(first, &mut control, &mut data, root).await?
            ))
        }
        FrameKind::RpcRequest => {
            let request_id = first.request_id;
            let parsed = RpcRequest::decode(&first.payload);
            match parsed {
                Ok(RpcRequest::AcquireTransfer) => {
                    let granted = if request_id == 0 {
                        false
                    } else if let Some((arbiter, remote_grant)) = lease {
                        let mut guard = remote_grant.lock().await;
                        guard.acquire(arbiter, request_id)
                    } else { false };
                    if granted {
                        write_frame(&mut control, FrameKind::RpcResponse, request_id, b"OK".to_vec()).await?;
                    } else {
                        write_frame(&mut control, FrameKind::Error, request_id, b"busy".to_vec()).await?;
                    }
                    Ok(IncomingResult::DirectoryListed)
                }
                Ok(RpcRequest::ReleaseTransfer) => {
                    let released = if let Some((_, remote_grant)) = lease {
                        let mut guard = remote_grant.lock().await;
                        guard.release(request_id)
                    } else { false };
                    if released {
                        write_frame(&mut control, FrameKind::RpcResponse, request_id, b"OK".to_vec()).await?;
                    } else {
                        write_frame(&mut control, FrameKind::Error, request_id, b"unknown lease".to_vec()).await?;
                    }
                    Ok(IncomingResult::DirectoryListed)
                }
                Ok(RpcRequest::List { directory }) => {
                    match rpc::list_in_root(root, &directory)
                        .and_then(|names| rpc::encode_listing(&names))
                    {
                        Ok(body) => {
                            write_frame(&mut control, FrameKind::RpcResponse, request_id, body).await?;
                            Ok(IncomingResult::DirectoryListed)
                        }
                        Err(_) => {
                            write_frame(&mut control, FrameKind::Error, request_id, b"listing denied".to_vec()).await?;
                            Err(SdkTransferError::InvalidDataStream)
                        }
                    }
                }
                Ok(RpcRequest::ListTypes { directory }) => {
                    match rpc::list_typed_in_root(root, &directory)
                        .and_then(|entries| rpc::encode_typed_listing(&entries))
                    {
                        Ok(body) => {
                            write_frame(&mut control, FrameKind::RpcResponse, request_id, body).await?;
                            Ok(IncomingResult::DirectoryListed)
                        }
                        Err(_) => {
                            write_frame(&mut control, FrameKind::Error, request_id, b"typed listing unavailable".to_vec()).await?;
                            // Files are not directories. A failed directory
                            // probe is a normal protocol response; the client
                            // may attempt a regular authorized GET instead.
                            Ok(IncomingResult::DirectoryListed)
                        }
                    }
                }
                Ok(RpcRequest::MakeDirectory { directory }) => {
                    match rpc::make_directory_in_root(root, &directory) {
                        Ok(()) => {
                            write_frame(&mut control, FrameKind::RpcResponse, request_id, b"OK".to_vec()).await?;
                            Ok(IncomingResult::DirectoryListed)
                        }
                        Err(_) => {
                            write_frame(&mut control, FrameKind::Error, request_id, b"mkdir denied".to_vec()).await?;
                            Err(SdkTransferError::InvalidDataStream)
                        }
                    }
                }
                Ok(RpcRequest::Get { source, destination }) => {
                    // Directory browsing is independent of the transfer
                    // lease, but serving file DATA requires a live remote
                    // grant on the authoritative creator.
                    if let Some((_, grants)) = lease {
                        let authorized = grants.lock().await.permits_inbound_file();
                        if !authorized {
                            write_frame(
                                &mut control, FrameKind::Error, request_id,
                                b"transfer lease required".to_vec(),
                            ).await?;
                            return Err(SdkTransferError::InvalidDataStream);
                        }
                    }
                    if rpc::authorize_get(root, &source).is_err() {
                        write_frame(&mut control, FrameKind::Error, request_id, b"file denied".to_vec()).await?;
                        return Err(SdkTransferError::InvalidDataStream);
                    }
                    write_frame(&mut control, FrameKind::RpcResponse, request_id, b"OK".to_vec()).await?;
                    Ok(IncomingResult::ServedGet(
                        send_on_managed_lane(session, lanes, root, Path::new(&source), &destination, request_id).await?
                    ))
                }
                Err(_) => {
                    write_frame(&mut control, FrameKind::Error, request_id, b"invalid RPC".to_vec()).await?;
                    Err(SdkTransferError::InvalidDataStream)
                }
            }
        }
        _ => Err(SdkTransferError::InvalidDataStream),
    }
}

async fn request_rpc(
    session: &dyn AuthenticatedSession,
    request: RpcRequest,
    request_id: u64,
) -> Result<Frame, SdkTransferError> {
    let (send, recv) = session.control().open_bi().await
        .map_err(|_| SdkTransferError::ControlConnection)?;
    let mut io = ControlIo::new(recv, send);
    let payload = request.encode().map_err(|_| SdkTransferError::InvalidDataStream)?;
    write_frame(&mut io, FrameKind::RpcRequest, request_id, payload).await?;
    let response = read_frame(&mut io).await?;
    if response.request_id != request_id { return Err(SdkTransferError::InvalidDataStream); }
    if response.kind == FrameKind::Error { return Err(SdkTransferError::Transfer(TransferError::RemoteRejected)); }
    if response.kind != FrameKind::RpcResponse { return Err(SdkTransferError::InvalidDataStream); }
    Ok(response)
}

/// Remote ls/cd's directory probe: never reveals any path outside the
/// explicitly authorized root on the remote peer.
pub async fn list_via_sdk(
    session: &dyn AuthenticatedSession,
    directory: String,
    request_id: u64,
) -> Result<Vec<String>, SdkTransferError> {
    let response = request_rpc(session, RpcRequest::List { directory }, request_id).await?;
    rpc::decode_listing(&response.payload).map_err(|_| SdkTransferError::InvalidDataStream)
}

/// Ask the other authenticated peer to send one named file back. The incoming
/// file then arrives on a NEW separate Control/Data stream pair, handled by
/// serve_control_stream() in the local session's incoming accept loop.
pub async fn request_get_via_sdk(
    session: &dyn AuthenticatedSession,
    source: String,
    destination: String,
    request_id: u64,
) -> Result<(), SdkTransferError> {
    let response = request_rpc(session, RpcRequest::Get { source, destination }, request_id).await?;
    if response.payload.as_slice() != b"OK" {
        return Err(SdkTransferError::InvalidDataStream);
    }
    Ok(())
}

pub async fn list_typed_via_sdk(
    session: &dyn AuthenticatedSession,
    directory: String,
    request_id: u64,
) -> Result<Vec<rpc::RemoteEntry>, SdkTransferError> {
    let response = request_rpc(session, RpcRequest::ListTypes { directory }, request_id).await?;
    rpc::decode_typed_listing(&response.payload).map_err(|_| SdkTransferError::InvalidDataStream)
}

pub async fn mkdir_via_sdk(
    session: &dyn AuthenticatedSession,
    directory: String,
    request_id: u64,
) -> Result<(), SdkTransferError> {
    let response = request_rpc(session, RpcRequest::MakeDirectory { directory }, request_id).await?;
    if response.payload.as_slice() != b"OK" {
        return Err(SdkTransferError::InvalidDataStream);
    }
    Ok(())
}

/// Joiner must obtain a server-authoritative grant before an entire PUT/GET
/// batch, not once per individual file. This uses Control QUIC only.
pub async fn acquire_transfer_via_sdk(
    session: &dyn AuthenticatedSession,
    id: u64,
) -> Result<(), SdkTransferError> {
    if id == 0 { return Err(SdkTransferError::InvalidDataStream); }
    let result = request_rpc(session, RpcRequest::AcquireTransfer, id).await?;
    if result.payload != b"OK" { return Err(SdkTransferError::InvalidDataStream); }
    Ok(())
}

pub async fn release_transfer_via_sdk(
    session: &dyn AuthenticatedSession,
    id: u64,
) -> Result<(), SdkTransferError> {
    if id == 0 { return Err(SdkTransferError::InvalidDataStream); }
    let result = request_rpc(session, RpcRequest::ReleaseTransfer, id).await?;
    if result.payload != b"OK" { return Err(SdkTransferError::InvalidDataStream); }
    Ok(())
}

#[cfg(test)]
mod transfer_lease_rpc_tests {
    use super::*;

    #[test]
    fn creator_may_receive_its_own_get_reply_without_remote_grant() {
        let lease = crate::lease::TransferLease::new();
        let remote = RemoteLeaseState::default();
        assert!(!inbound_file_authorized(&lease, &remote, 77));
        let local = lease.try_acquire(77).unwrap();
        assert!(!inbound_file_authorized(&lease, &remote, 77));
        let expected = remote.expect_local_get(77).unwrap();
        assert!(inbound_file_authorized(&lease, &remote, 77));
        assert!(!inbound_file_authorized(&lease, &remote, 78));
        drop(expected);
        assert!(!inbound_file_authorized(&lease, &remote, 77));
        let expected = remote.expect_local_get(77).unwrap();
        drop(local);
        assert!(!inbound_file_authorized(&lease, &remote, 77));
        drop(expected);
    }

    #[test]
    fn local_get_permit_is_unique_and_revoked_on_drop() {
        let remote = RemoteLeaseState::default();
        assert!(remote.expect_local_get(0).is_none());
        let first = remote.expect_local_get(12).unwrap();
        assert!(remote.expect_local_get(12).is_none());
        assert!(remote.permits_local_get(12));
        drop(first);
        assert!(!remote.permits_local_get(12));
        let second = remote.expect_local_get(12).unwrap();
        drop(second);
        assert!(!remote.permits_local_get(12));
    }

    #[test]
    fn inbound_put_get_require_live_remote_grant() {
        let lease = crate::lease::TransferLease::new();
        let mut remote = RemoteLeaseState::default();
        assert!(!remote.permits_inbound_file());
        assert!(remote.acquire(&lease, 41));
        assert!(remote.permits_inbound_file());
        assert!(!remote.release(42));
        assert!(remote.permits_inbound_file());
        assert!(remote.release(41));
        assert!(!remote.permits_inbound_file());
        assert!(!remote.acquire(&lease, 41)); // Released grant cannot be replayed.
        assert!(!remote.permits_inbound_file());
    }

    #[test]
    fn cancellation_before_acquire_prevents_stranded_grant() {
        let lease = crate::lease::TransferLease::new();
        let mut remote = RemoteLeaseState::default();
        assert!(remote.release(33)); // Reordered Control QUIC streams.
        assert!(!remote.acquire(&lease, 33));
        assert_eq!(lease.active_request(), None);
        assert!(remote.acquire(&lease, 34));
        assert_eq!(lease.active_request(), Some(34));
        assert!(!remote.release(35));
        assert_eq!(lease.active_request(), Some(34));
        assert!(remote.release(34));
        assert_eq!(lease.active_request(), None);
        assert!(!remote.acquire(&lease, 34)); // Replay blocked.
    }

    #[test]
    fn remote_grant_blocks_creator_and_releases_on_abort() {
        let lease = crate::lease::TransferLease::new();
        let mut remote = RemoteLeaseState::default();
        assert!(remote.acquire(&lease, 100));
        assert!(lease.try_acquire(101).is_err());
        assert!(!remote.acquire(&lease, 102));
        assert!(remote.release(100));
        assert!(lease.try_acquire(101).is_ok());
        assert!(!remote.acquire(&lease, 100));
    }

    #[test]
    fn rejected_or_zero_request_ids_never_grant_a_lease() {
        let lease = crate::lease::TransferLease::new();
        let mut remote = RemoteLeaseState::default();
        assert!(!remote.acquire(&lease, 0));
        assert!(!remote.release(0));
        assert_eq!(lease.active_request(), None);
        for id in 1..=150 { assert!(remote.release(id)); }
        assert_eq!(remote.canceled.len(), 128);
    }
}
