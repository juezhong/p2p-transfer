//! Bind Transfer's independent control/data streams to an SDK-verified session.
//!
//! No QUIC endpoint, TLS verifier or ICE agent is created in this crate.
//! Only an already authorized `VerifiedManualSession` may be used.
//! The caller must enforce a SINGLE active Transfer operation per session
//! until multiplexed request-ID scheduling and repair are implemented.

use std::{
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use p2p_sdk::verified_session::VerifiedManualSession;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

use crate::{
    secure_io::SharedRoot,
    stream_transfer::{receive_file, send_file, TransferError, TransferReceipt},
};

const DATA_PREFACE: [u8; 4] = *b"P2PD";

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
    session: &VerifiedManualSession,
    root: &SharedRoot,
    source: &Path,
    remote_destination: &str,
    request_id: u64,
) -> Result<TransferReceipt, SdkTransferError> {
    let (control_send, control_recv) = session.control().open_bi().await
        .map_err(|_| SdkTransferError::ControlConnection)?;
    let mut control = ControlIo::new(control_recv, control_send);
    let mut data = session.data().open_uni().await
        .map_err(|_| SdkTransferError::DataConnection)?;
    AsyncWriteExt::write_all(&mut data, &DATA_PREFACE).await
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
    session: &VerifiedManualSession,
    root: &SharedRoot,
) -> Result<TransferReceipt, SdkTransferError> {
    let (control_send, control_recv) = session.control().accept_bi().await
        .map_err(|_| SdkTransferError::ControlConnection)?;
    let mut control = ControlIo::new(control_recv, control_send);
    let mut data = session.data().accept_uni().await
        .map_err(|_| SdkTransferError::DataConnection)?;
    let mut preface = [0u8; DATA_PREFACE.len()];
    AsyncReadExt::read_exact(&mut data, &mut preface).await?;
    if preface != DATA_PREFACE {
        return Err(SdkTransferError::InvalidDataStream);
    }
    receive_file(&mut control, &mut data, root).await.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

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
