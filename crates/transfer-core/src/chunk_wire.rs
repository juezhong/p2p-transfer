//! Framing for request-bound, 1 MiB application Data chunks.
//!
//! Each authenticated Data QUIC lane can carry chunks of the same Transfer
//! request. The fixed header is decoded before allocating the data payload.
//! The receiver must still verify file-level SHA-256 and only ACK consecutive
//! blocks appended to its capability-root file handle.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::chunk_reassembly::CHUNK_BYTES;

const MAGIC: [u8; 4] = *b"P2DC";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 4 + 1 + 8 + 8 + 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkWireError {
    InvalidHeader,
    InvalidRequest,
    InvalidOffset,
    InvalidLength,
    Io,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedChunk {
    pub request_id: u64,
    pub offset: u64,
    pub payload: Vec<u8>,
}

fn validate_chunk(
    request_id: u64, offset: u64, len: usize, total: u64,
) -> Result<(), ChunkWireError> {
    if request_id == 0 { return Err(ChunkWireError::InvalidRequest); }
    if offset % CHUNK_BYTES as u64 != 0 { return Err(ChunkWireError::InvalidOffset); }
    let expected = total.checked_sub(offset).ok_or(ChunkWireError::InvalidOffset)?
        .min(CHUNK_BYTES as u64) as usize;
    if expected == 0 || len != expected { return Err(ChunkWireError::InvalidLength); }
    Ok(())
}

/// Write one complete chunk; bounded payload length and offset are validated
/// before sending. The transport may fail partway through a frame; on repair
/// the sender retransmits based on receiver ACK, not this write returning OK.
pub async fn write_chunk<W: AsyncWrite + Unpin>(
    writer: &mut W, request_id: u64, offset: u64,
    total: u64, bytes: &[u8],
) -> Result<(), ChunkWireError> {
    validate_chunk(request_id, offset, bytes.len(), total)?;
    let mut header = [0u8; HEADER_BYTES];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5..13].copy_from_slice(&request_id.to_be_bytes());
    header[13..21].copy_from_slice(&offset.to_be_bytes());
    header[21..25].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
    writer.write_all(&header).await.map_err(|_| ChunkWireError::Io)?;
    writer.write_all(bytes).await.map_err(|_| ChunkWireError::Io)?;
    Ok(())
}

/// Decode the fixed header and reject a bad peer request, offset or length
/// before allocating a payload. A file receiver checks the expected request
/// ID and target length derived from its already-authorized Control offer.
pub async fn read_chunk<R: AsyncRead + Unpin>(
    reader: &mut R, expected_id: u64, total: u64,
) -> Result<ReceivedChunk, ChunkWireError> {
    let mut header = [0u8; HEADER_BYTES];
    reader.read_exact(&mut header).await.map_err(|_| ChunkWireError::Io)?;
    if header[..4] != MAGIC || header[4] != VERSION {
        return Err(ChunkWireError::InvalidHeader);
    }
    let id = u64::from_be_bytes(header[5..13].try_into().unwrap());
    if id == 0 || id != expected_id { return Err(ChunkWireError::InvalidRequest); }
    let offset = u64::from_be_bytes(header[13..21].try_into().unwrap());
    let len = u32::from_be_bytes(header[21..25].try_into().unwrap()) as usize;
    validate_chunk(id, offset, len, total)?;
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await.map_err(|_| ChunkWireError::Io)?;
    Ok(ReceivedChunk { request_id: id, offset, payload })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncWriteExt};

    #[tokio::test]
    async fn four_out_of_order_lanes_can_use_the_same_authenticated_request() {
        let total = (CHUNK_BYTES * 4) as u64;
        let mut out = Vec::new();
        for index in [3u64, 1, 2, 0] {
            let (mut writer, mut reader) = duplex(CHUNK_BYTES + 64);
            let task = tokio::spawn(async move {
                write_chunk(&mut writer, 19, index * CHUNK_BYTES as u64,
                    total, &vec![index as u8; CHUNK_BYTES]).await.unwrap();
            });
            let chunk = read_chunk(&mut reader, 19, total).await.unwrap();
            assert_eq!(chunk.offset, index * CHUNK_BYTES as u64);
            out.push(chunk);
            task.await.unwrap();
        }
        let mut disk = Vec::new();
        let mut reorder = crate::chunk_reassembly::BoundedReassembly::new(
            total, CHUNK_BYTES * 4,
        ).unwrap();
        for chunk in out {
            reorder.receive(chunk.offset, chunk.payload,
                |block| { disk.extend_from_slice(block); Ok(disk.len() as u64) }).unwrap();
        }
        assert_eq!(disk.len() as u64, total);
        for index in 0..4 { assert_eq!(disk[index * CHUNK_BYTES], index as u8); }
    }

    #[tokio::test]
    async fn rejects_oversized_or_cross_request_header_without_payload_read() {
        let total = (CHUNK_BYTES * 2) as u64;
        let header = |request_id: u64, offset: u64, length: u32| {
            let mut bytes = [0u8; HEADER_BYTES];
            bytes[..4].copy_from_slice(&MAGIC);
            bytes[4] = VERSION;
            bytes[5..13].copy_from_slice(&request_id.to_be_bytes());
            bytes[13..21].copy_from_slice(&offset.to_be_bytes());
            bytes[21..25].copy_from_slice(&length.to_be_bytes());
            bytes
        };
        for (id, offset, length, want) in [
            (8, 0, CHUNK_BYTES as u32, ChunkWireError::InvalidRequest),
            (7, 0, (CHUNK_BYTES + 1) as u32, ChunkWireError::InvalidLength),
            (7, 1, CHUNK_BYTES as u32, ChunkWireError::InvalidOffset),
            (7, CHUNK_BYTES as u64 * 3, CHUNK_BYTES as u32, ChunkWireError::InvalidOffset),
        ] {
            let (mut a, mut b) = duplex(64);
            a.write_all(&header(id, offset, length)).await.unwrap();
            assert_eq!(read_chunk(&mut b, 7, total).await.err(), Some(want));
        }
        let (mut writer, mut reader) = duplex(128);
        assert_eq!(write_chunk(&mut writer, 4, 0, total, &[3, 4]).await,
            Err(ChunkWireError::InvalidLength));
        drop(writer);
        assert!(reader.read_u8().await.is_err());
    }

    #[tokio::test]
    async fn stream_break_resends_only_receiver_unacknowledged_bytes() {
        use sha2::{Digest, Sha256};
        use crate::chunk_reassembly::{BoundedReassembly, ResendWindow};

        let total = (2 * CHUNK_BYTES) as u64;
        let mut source = vec![0x21; CHUNK_BYTES];
        source.extend(vec![0x92; CHUNK_BYTES]);
        let expected_digest: [u8; 32] = Sha256::digest(&source).into();

        let mut sender = ResendWindow::new(total, 2 * CHUNK_BYTES).unwrap();
        let first = sender.next_chunk().unwrap();
        let second = sender.next_chunk().unwrap();
        assert_eq!(first.start, 0);
        assert_eq!(second.start, CHUNK_BYTES as u64);

        let mut receiver = BoundedReassembly::new(total, 2 * CHUNK_BYTES).unwrap();
        let mut part_file = Vec::new();
        let (mut broken_tx, mut broken_rx) = duplex(CHUNK_BYTES + 64);
        let original = source.clone();
        let remote = tokio::spawn(async move {
            write_chunk(&mut broken_tx, 71, first.start, total,
                &original[first.start as usize..first.end as usize]).await.unwrap();
            // A QUIC lane may close during a DATA payload. A partial header
            // or payload must never cause the receiver to advance its ACK.
            let mut header = [0u8; HEADER_BYTES];
            header[..4].copy_from_slice(&MAGIC);
            header[4] = VERSION;
            header[5..13].copy_from_slice(&71u64.to_be_bytes());
            header[13..21].copy_from_slice(&second.start.to_be_bytes());
            header[21..25].copy_from_slice(&(CHUNK_BYTES as u32).to_be_bytes());
            broken_tx.write_all(&header).await.unwrap();
            broken_tx.write_all(&original[CHUNK_BYTES..CHUNK_BYTES + 41]).await.unwrap();
        });
        let first_ok = read_chunk(&mut broken_rx, 71, total).await.unwrap();
        let ack = receiver.receive(first_ok.offset, first_ok.payload, |block| {
            part_file.extend_from_slice(block);
            Ok(part_file.len() as u64)
        }).unwrap();
        assert_eq!(ack, CHUNK_BYTES as u64);
        sender.ack(ack).unwrap();
        assert_eq!(read_chunk(&mut broken_rx, 71, total).await,
            Err(ChunkWireError::Io));
        remote.await.unwrap();
        assert_eq!(receiver.committed(), CHUNK_BYTES as u64);

        // A new authenticated lane asks for the receiver's real disk ACK;
        // never treat a successful QUIC write for the lost block as proof.
        sender.resume_at_receiver_ack(receiver.committed()).unwrap();
        let retry = sender.next_chunk().unwrap();
        assert_eq!(retry.start, CHUNK_BYTES as u64);
        let (mut fresh_tx, mut fresh_rx) = duplex(CHUNK_BYTES + 64);
        let data = source.clone();
        let retried = tokio::spawn(async move {
            write_chunk(&mut fresh_tx, 71, retry.start, total,
                &data[retry.start as usize..retry.end as usize]).await.unwrap();
        });
        let second_ok = read_chunk(&mut fresh_rx, 71, total).await.unwrap();
        let second_ack = receiver.receive(second_ok.offset, second_ok.payload, |block| {
            part_file.extend_from_slice(block);
            Ok(part_file.len() as u64)
        }).unwrap();
        retried.await.unwrap();
        sender.ack(second_ack).unwrap();

        assert_eq!(second_ack, total);
        assert!(sender.is_complete());
        assert!(receiver.is_complete());
        assert_eq!(part_file.len() as u64, total);
        assert_eq!(<[u8; 32]>::from(Sha256::digest(part_file)), expected_digest);
    }

    #[tokio::test]
    async fn truncated_payload_fails_instead_of_advancing_disk_ack() {
        let (mut writer, mut reader) = duplex(64);
        let mut header = [0u8; HEADER_BYTES];
        header[..4].copy_from_slice(&MAGIC);
        header[4] = VERSION;
        header[5..13].copy_from_slice(&12u64.to_be_bytes());
        header[21..25].copy_from_slice(&(CHUNK_BYTES as u32).to_be_bytes());
        writer.write_all(&header).await.unwrap();
        writer.write_all(&[1, 2, 3]).await.unwrap();
        drop(writer);
        assert_eq!(read_chunk(&mut reader, 12, CHUNK_BYTES as u64).await,
            Err(ChunkWireError::Io));
    }
}
