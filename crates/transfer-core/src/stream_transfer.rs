//! Bounded file transfer over two ALREADY authenticated, independent streams.
//!
//! Control messages (offers, cumulative disk-written ACKs, final commit/errors)
//! never carry file bytes. The data stream carries only bounded file chunks.
//! SDK authenticates the QUIC Control/Data sessions; this module intentionally
//! knows nothing about ICE, STUN, credentials or network path selection.
//!
//! M2 correctness baseline: one outstanding chunk, bounded memory. The
//! Go-parity adaptive sliding window, retry/resume and multiplexing are later
//! features, NOT claimed here.

use std::{io::Read, path::Path};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{
    protocol::{CodecError, Frame, FrameKind, HEADER_LEN, MAGIC, MAX_PAYLOAD},
    secure_io::{FileAccessError, SharedRoot},
    task::{Task, TaskId},
};

const CHUNK: usize = 128 * 1024;
const MAX_PATH_BYTES: usize = 1024;
const OFFER_FIXED: usize = 8 + 32 + 2;

#[derive(Debug)]
pub enum TransferError {
    Io(std::io::Error),
    Codec(CodecError),
    Filesystem(FileAccessError),
    Protocol(&'static str),
    RemoteRejected,
}

impl From<std::io::Error> for TransferError {
    fn from(value: std::io::Error) -> Self { Self::Io(value) }
}
impl From<CodecError> for TransferError {
    fn from(value: CodecError) -> Self { Self::Codec(value) }
}
impl From<FileAccessError> for TransferError {
    fn from(value: FileAccessError) -> Self { Self::Filesystem(value) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferReceipt {
    pub request_id: u64,
    pub bytes: u64,
    pub sha256: [u8; 32],
}

struct Offer {
    bytes: u64,
    sha256: [u8; 32],
    destination: String,
}

impl Offer {
    fn encode(&self) -> Result<Vec<u8>, TransferError> {
        let name = self.destination.as_bytes();
        if name.is_empty() || name.len() > MAX_PATH_BYTES || name.contains(&0) {
            return Err(TransferError::Protocol("invalid file destination"));
        }
        let mut bytes = Vec::with_capacity(OFFER_FIXED + name.len());
        bytes.extend_from_slice(&self.bytes.to_be_bytes());
        bytes.extend_from_slice(&self.sha256);
        bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        bytes.extend_from_slice(name);
        Ok(bytes)
    }

    fn decode(buf: &[u8]) -> Result<Self, TransferError> {
        if buf.len() < OFFER_FIXED {
            return Err(TransferError::Protocol("short offer"));
        }
        let bytes = u64::from_be_bytes(buf[..8].try_into().unwrap());
        let sha256 = buf[8..40].try_into().unwrap();
        let n = u16::from_be_bytes(buf[40..42].try_into().unwrap()) as usize;
        if n == 0 || n > MAX_PATH_BYTES || buf.len() != OFFER_FIXED + n {
            return Err(TransferError::Protocol("invalid offer path size"));
        }
        let name = std::str::from_utf8(&buf[42..])
            .map_err(|_| TransferError::Protocol("invalid UTF-8 destination"))?;
        if name.contains('\0') {
            return Err(TransferError::Protocol("invalid destination"));
        }
        Ok(Self { bytes, sha256, destination: name.to_owned() })
    }
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    to: &mut W,
    kind: FrameKind,
    request_id: u64,
    payload: Vec<u8>,
) -> Result<(), TransferError> {
    let bytes = Frame { kind, request_id, payload }.encode()?;
    to.write_all(&bytes).await?;
    to.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(from: &mut R) -> Result<Frame, TransferError> {
    let mut header = [0u8; HEADER_LEN];
    from.read_exact(&mut header).await?;
    if header[..4] != MAGIC { return Err(TransferError::Protocol("invalid frame magic")); }
    let payload_len = u32::from_be_bytes(header[16..20].try_into().unwrap()) as usize;
    if payload_len > MAX_PAYLOAD { return Err(TransferError::Codec(CodecError::OversizedPayload)); }
    let mut packet = Vec::with_capacity(HEADER_LEN + payload_len);
    packet.extend_from_slice(&header);
    packet.resize(HEADER_LEN + payload_len, 0);
    from.read_exact(&mut packet[HEADER_LEN..]).await?;
    Ok(Frame::decode(&packet)?)
}

fn expect_ack(frame: Frame, request_id: u64, expected: u64) -> Result<(), TransferError> {
    if frame.request_id != request_id { return Err(TransferError::Protocol("wrong request ID")); }
    if frame.kind == FrameKind::Error { return Err(TransferError::RemoteRejected); }
    if frame.kind != FrameKind::Ack || frame.payload.len() != 8 {
        return Err(TransferError::Protocol("invalid disk-written ACK"));
    }
    let observed = u64::from_be_bytes(frame.payload.try_into().unwrap());
    if observed != expected { return Err(TransferError::Protocol("ACK offset mismatch")); }
    Ok(())
}

/// Send one explicitly authorized local file over a dedicated data stream,
/// receiving disk-write ACKs over a distinct control stream.
///
/// The caller MUST supply streams belonging to an SDK-verified peer/session.
/// Blocking filesystem reads will later be moved to a dedicated I/O executor.
pub async fn send_file<C, D>(
    control: &mut C,
    data: &mut D,
    root: &SharedRoot,
    source: &Path,
    remote_destination: &str,
    request_id: u64,
) -> Result<TransferReceipt, TransferError>
where
    C: AsyncRead + AsyncWrite + Unpin,
    D: AsyncWrite + Unpin,
{
    // Hash one opened source handle before offering it. Reopening occurs
    // inside the same capability root; a changed file fails receiver SHA.
    let mut file = root.open_read(source)?;
    let bytes = file.metadata()?.len();
    let mut hasher = Sha256::new();
    let mut buf = [0u8; CHUNK];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    let sha256: [u8; 32] = hasher.finalize().into();
    let offer = Offer { bytes, sha256, destination: remote_destination.to_owned() };
    write_frame(control, FrameKind::TransferControl, request_id, offer.encode()?).await?;
    expect_ack(read_frame(control).await?, request_id, 0)?;

    let mut file = root.open_read(source)?;
    let mut task = Task::new(TaskId(request_id), bytes);
    task.start().map_err(|_| TransferError::Protocol("task state error"))?;
    let mut sent = 0u64;
    while sent < bytes {
        let n = CHUNK.min((bytes - sent) as usize);
        file.read_exact(&mut buf[..n])?;
        data.write_all(&(n as u32).to_be_bytes()).await?;
        data.write_all(&buf[..n]).await?;
        data.flush().await?;
        sent += n as u64;
        expect_ack(read_frame(control).await?, request_id, sent)?;
        task.acknowledge_written(sent)
            .map_err(|_| TransferError::Protocol("task acknowledgement failed"))?;
    }
    // The receiver checks SHA, fsyncs, and atomically publishes before DONE.
    let final_frame = read_frame(control).await?;
    if final_frame.kind == FrameKind::Error {
        return Err(TransferError::RemoteRejected);
    }
    if final_frame.kind != FrameKind::TransferControl
        || final_frame.request_id != request_id
        || final_frame.payload.as_slice() != b"DONE"
    {
        return Err(TransferError::Protocol("missing verified commit"));
    }
    task.begin_verification().map_err(|_| TransferError::Protocol("invalid verify state"))?;
    task.complete().map_err(|_| TransferError::Protocol("invalid commit state"))?;
    Ok(TransferReceipt { request_id, bytes, sha256 })
}

/// Receive a single file within an explicitly user-authorized capability root.
/// ACK advances only AFTER successful sequential write to the destination.
/// The final DONE is sent only AFTER SHA-256 and atomic no-clobber commit.
pub async fn receive_file<C, D>(
    control: &mut C,
    data: &mut D,
    root: &SharedRoot,
) -> Result<TransferReceipt, TransferError>
where
    C: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + Unpin,
{
    let frame = read_frame(control).await?;
    receive_file_after_offer(frame, control, data, root).await
}

/// Receive a transfer after a control-plane dispatcher has read and verified
/// the first bounded Frame. This allows one session to handle both file
/// offers and directory/GET RPC without consuming the wrong stream.
pub async fn receive_file_after_offer<C, D>(
    frame: Frame,
    control: &mut C,
    data: &mut D,
    root: &SharedRoot,
) -> Result<TransferReceipt, TransferError>
where
    C: AsyncRead + AsyncWrite + Unpin,
    D: AsyncRead + Unpin,
{
    if frame.kind != FrameKind::TransferControl {
        return Err(TransferError::Protocol("expected transfer offer"));
    }
    let request_id = frame.request_id;
    let offer = match Offer::decode(&frame.payload) {
        Ok(offer) => offer,
        Err(err) => {
            write_frame(control, FrameKind::Error, request_id, b"bad offer".to_vec()).await?;
            return Err(err);
        }
    };
    let mut sink = match root.receive_part(Path::new(&offer.destination), offer.bytes) {
        Ok(sink) => sink,
        Err(err) => {
            write_frame(control, FrameKind::Error, request_id, b"access denied".to_vec()).await?;
            return Err(err.into());
        }
    };
    write_frame(control, FrameKind::Ack, request_id, 0u64.to_be_bytes().to_vec()).await?;
    let mut left = offer.bytes;
    let mut buf = [0u8; CHUNK];
    while left > 0 {
        let n = data.read_u32().await? as usize;
        if n == 0 || n > CHUNK || n as u64 > left {
            write_frame(control, FrameKind::Error, request_id, b"invalid chunk".to_vec()).await?;
            return Err(TransferError::Protocol("invalid data chunk size"));
        }
        data.read_exact(&mut buf[..n]).await?;
        let written = sink.append(&buf[..n])?;
        left -= n as u64;
        write_frame(control, FrameKind::Ack, request_id, written.to_be_bytes().to_vec()).await?;
    }
    if let Err(err) = sink.verify_and_commit(offer.sha256) {
        write_frame(control, FrameKind::Error, request_id, b"integrity or commit failure".to_vec()).await?;
        return Err(err.into());
    }
    write_frame(control, FrameKind::TransferControl, request_id, b"DONE".to_vec()).await?;
    Ok(TransferReceipt { request_id, bytes: offer.bytes, sha256: offer.sha256 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};
    use tokio::io::duplex;

    fn fixture() -> (PathBuf, SharedRoot, PathBuf, SharedRoot) {
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).unwrap();
        let base = std::env::temp_dir().join(format!(
            "p2p-transfer-stream-{}-{:?}", std::process::id(), id
        ));
        let src = base.join("source");
        let dst = base.join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dst.join("子目录")).unwrap();
        let source = SharedRoot::authorize(&src).unwrap();
        let dest = SharedRoot::authorize(&dst).unwrap();
        (base, source, dst, dest)
    }

    #[tokio::test]
    async fn transfers_unicode_and_multi_chunk_file_and_verifies_disk_commit() {
        let (base, src, dst_path, dst) = fixture();
        let bytes = vec![42u8; CHUNK * 2 + 57];
        fs::write(base.join("source/大文件.bin"), &bytes).unwrap();
        let (mut a_ctrl, mut b_ctrl) = duplex(256 * 1024);
        let (mut a_data, mut b_data) = duplex(256 * 1024);
        let (send, receive) = tokio::join!(
            send_file(&mut a_ctrl, &mut a_data, &src, Path::new("大文件.bin"),
                      "子目录/接收.bin", 7),
            receive_file(&mut b_ctrl, &mut b_data, &dst),
        );
        let sent = send.unwrap();
        let received = receive.unwrap();
        assert_eq!(sent, received);
        assert_eq!(sent.bytes, bytes.len() as u64);
        assert_eq!(fs::read(dst_path.join("子目录/接收.bin")).unwrap(), bytes);
        assert_eq!(crate::secure_io::hash_open_file(
            dst.open_read(Path::new("子目录/接收.bin")).unwrap()
        ).unwrap(), sent.sha256);
        drop(src);
        drop(dst);
        fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn transfers_zero_byte_file_with_full_integrity_commit() {
        let (base, src, dst_path, dst) = fixture();
        fs::write(base.join("source/empty.txt"), b"").unwrap();
        let (mut a_ctrl, mut b_ctrl) = duplex(1024);
        let (mut a_data, mut b_data) = duplex(1024);
        let (send, recv) = tokio::join!(
            send_file(&mut a_ctrl, &mut a_data, &src, Path::new("empty.txt"), "empty.txt", 8),
            receive_file(&mut b_ctrl, &mut b_data, &dst),
        );
        assert_eq!(send.unwrap(), recv.unwrap());
        assert_eq!(fs::read(dst_path.join("empty.txt")).unwrap(), b"");
        drop(src);
        drop(dst);
        fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn refuses_path_escape_without_publishing_and_responds_on_control() {
        let (base, src, _path, dst) = fixture();
        fs::write(base.join("source/source.txt"), b"secret").unwrap();
        let (mut a_ctrl, mut b_ctrl) = duplex(1024);
        let (mut a_data, mut b_data) = duplex(1024);
        let (send, recv) = tokio::join!(
            send_file(&mut a_ctrl, &mut a_data, &src, Path::new("source.txt"), "../escape", 9),
            receive_file(&mut b_ctrl, &mut b_data, &dst),
        );
        assert!(matches!(send, Err(TransferError::RemoteRejected)));
        assert!(matches!(recv, Err(TransferError::Filesystem(FileAccessError::InvalidRelativePath))));
        assert!(!base.join("escape").exists());
        drop(src);
        drop(dst);
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn rejects_overlong_and_invalid_metadata() {
        let offer = Offer { bytes: 1, sha256: [0; 32], destination: "x".repeat(MAX_PATH_BYTES + 1) };
        assert!(matches!(offer.encode(), Err(TransferError::Protocol(_))));
        assert!(Offer::decode(&[]).is_err());
        let valid = Offer { bytes: 2, sha256: [1; 32], destination: "ok".into() }.encode().unwrap();
        assert!(Offer::decode(&valid[..valid.len()-1]).is_err());
    }
}
