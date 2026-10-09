//! Bounded, request-local chunk reconstruction for 4-way Data QUIC transfer.
//!
//! Each Data lane can deliver complete 1 MiB chunks out of order. ACKs may
//! advance only after consecutive chunks have been appended to the receiver's
//! capability-scoped PartWriter. The final file must still pass SHA-256 and
//! no-clobber commit. A lost lane requires retransmitting ranges not covered
//! by the last receiver-confirmed committed offset.
//!
//! This is the state engine, not yet the new v2 networking/wire dispatcher.

use std::{collections::BTreeMap, ops::Range};

pub const CHUNK_BYTES: usize = 1024 * 1024;
pub const MAX_REORDER_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkError {
    InvalidConfiguration,
    MisalignedOffset,
    InvalidChunkLength,
    ExceedsTotal,
    OutsideWindow,
    ConflictingDuplicate,
    BufferedLimit,
    InvalidAck,
    DiskWrite,
}

/// Receiver buffers no more than MAX_REORDER_BYTES and only acknowledges
/// blocks once a real disk append succeeds. The caller owns a PartWriter.
pub struct BoundedReassembly {
    total: u64,
    committed: u64,
    buffered_bytes: usize,
    limit: usize,
    queued: BTreeMap<u64, Vec<u8>>,
}

impl BoundedReassembly {
    pub fn new(total: u64, memory_limit: usize) -> Result<Self, ChunkError> {
        if memory_limit < CHUNK_BYTES || memory_limit > MAX_REORDER_BYTES
            || memory_limit % CHUNK_BYTES != 0
        {
            return Err(ChunkError::InvalidConfiguration);
        }
        Ok(Self { total, committed: 0, buffered_bytes: 0, limit: memory_limit,
            queued: BTreeMap::new() })
    }

    pub fn committed(&self) -> u64 { self.committed }
    pub fn buffered_bytes(&self) -> usize { self.buffered_bytes }
    pub fn is_complete(&self) -> bool { self.committed == self.total }

    /// Returns latest durable-to-the-writer (not crash-fsynced) ACK offset.
    /// A duplicate confirmed block is harmless; a conflicting queued block
    /// is rejected. The append callback must update the PartWriter atomically
    /// per complete block before returning its new written offset.
    pub fn receive<F>(&mut self, offset: u64, payload: Vec<u8>, mut append: F)
        -> Result<u64, ChunkError>
    where
        F: FnMut(&[u8]) -> Result<u64, ChunkError>,
    {
        if offset % CHUNK_BYTES as u64 != 0 {
            return Err(ChunkError::MisalignedOffset);
        }
        let expected_len = self.total.checked_sub(offset)
            .ok_or(ChunkError::ExceedsTotal)?
            .min(CHUNK_BYTES as u64) as usize;
        if expected_len == 0 || payload.len() != expected_len {
            return Err(ChunkError::InvalidChunkLength);
        }
        if offset < self.committed {
            // The sender may replay an earlier acknowledged chunk after
            // losing its last Control ACK. Never write the same bytes twice.
            return Ok(self.committed);
        }
        let high = self.committed.saturating_add(self.limit as u64);
        if offset >= high {
            return Err(ChunkError::OutsideWindow);
        }
        if let Some(existing) = self.queued.get(&offset) {
            if existing != &payload { return Err(ChunkError::ConflictingDuplicate); }
            return Ok(self.committed);
        }
        if self.buffered_bytes + payload.len() > self.limit {
            return Err(ChunkError::BufferedLimit);
        }
        self.buffered_bytes += payload.len();
        self.queued.insert(offset, payload);
        while let Some(block) = self.queued.get(&self.committed) {
            // Do not remove a block before the append succeeds. If disk I/O
            // fails, no ACK advances and the transfer must fail closed.
            let written = append(block)?;
            let expected = self.committed + block.len() as u64;
            if written != expected { return Err(ChunkError::DiskWrite); }
            let count = block.len();
            self.queued.remove(&self.committed);
            self.buffered_bytes -= count;
            self.committed = expected;
        }
        Ok(self.committed)
    }
}

/// Sender tracks bounded ranges in flight rather than keeping a second copy
/// of the file in RAM. Re-read these ranges from an authorized open file
/// descriptor after a lane drops or an ACK is lost.
pub struct ResendWindow {
    total: u64,
    next_sent: u64,
    acknowledged: u64,
    window: u64,
    outstanding: BTreeMap<u64, u32>,
}

impl ResendWindow {
    pub fn new(total: u64, window_bytes: usize) -> Result<Self, ChunkError> {
        if window_bytes < CHUNK_BYTES || window_bytes > MAX_REORDER_BYTES
            || window_bytes % CHUNK_BYTES != 0
        {
            return Err(ChunkError::InvalidConfiguration);
        }
        Ok(Self { total, next_sent: 0, acknowledged: 0,
            window: window_bytes as u64, outstanding: BTreeMap::new() })
    }

    pub fn acknowledged(&self) -> u64 { self.acknowledged }
    pub fn in_flight(&self) -> u64 { self.next_sent - self.acknowledged }
    pub fn is_complete(&self) -> bool { self.acknowledged == self.total }

    /// Reserve a new chunk only when the bounded sender window has capacity.
    pub fn next_chunk(&mut self) -> Option<Range<u64>> {
        if self.next_sent == self.total { return None; }
        let n = self.total.saturating_sub(self.next_sent)
            .min(CHUNK_BYTES as u64);
        if self.in_flight().saturating_add(n) > self.window { return None; }
        let start = self.next_sent;
        self.next_sent += n;
        self.outstanding.insert(start, n as u32);
        Some(start..self.next_sent)
    }

    /// ACK originates from receiver-side complete disk writes, not QUIC
    /// stream write completion. Reject jumps across a partial chunk.
    pub fn ack(&mut self, offset: u64) -> Result<(), ChunkError> {
        if offset < self.acknowledged || offset > self.next_sent {
            return Err(ChunkError::InvalidAck);
        }
        if offset == self.acknowledged { return Ok(()); }
        if offset != self.total && offset % CHUNK_BYTES as u64 != 0 {
            return Err(ChunkError::InvalidAck);
        }
        self.outstanding.retain(|&start, &mut n| start + n as u64 > offset);
        self.acknowledged = offset;
        Ok(())
    }

    /// Called after the receiver explicitly reports its retained PartWriter
    /// offset across a new authenticated Data QUIC. This *must not* guess
    /// that previously sent bytes were written; only peer-confirmed bytes
    /// may be excluded from retransmission.
    pub fn resume_at_receiver_ack(&mut self, verified_offset: u64)
        -> Result<(), ChunkError>
    {
        if verified_offset < self.acknowledged || verified_offset > self.next_sent
            || (verified_offset != self.total
                && verified_offset % CHUNK_BYTES as u64 != 0)
        {
            return Err(ChunkError::InvalidAck);
        }
        self.acknowledged = verified_offset;
        self.next_sent = verified_offset;
        self.outstanding.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorders_four_lanes_and_acks_only_consecutive_writes() {
        let total = (CHUNK_BYTES * 4) as u64;
        let mut received = BoundedReassembly::new(total, CHUNK_BYTES * 4).unwrap();
        let mut stored = Vec::new();
        for offset in [2, 3, 1, 0] {
            let bytes = vec![offset as u8; CHUNK_BYTES];
            let ack = received.receive(offset as u64 * CHUNK_BYTES as u64, bytes,
                |block| { stored.extend_from_slice(block); Ok(stored.len() as u64) }).unwrap();
            if offset != 0 { assert_eq!(ack, 0); }
        }
        assert_eq!(received.committed(), total);
        assert_eq!(received.buffered_bytes(), 0);
        assert!(received.is_complete());
        for i in 0..4 { assert_eq!(stored[i * CHUNK_BYTES], i as u8); }
        assert_eq!(received.receive(0, vec![0; CHUNK_BYTES], |_| {
            panic!("duplicate must never write again")
        }).unwrap(), total);
    }

    #[test]
    fn refuses_out_of_window_conflicting_chunks_and_disk_failure() {
        let mut recv = BoundedReassembly::new((CHUNK_BYTES * 5) as u64, CHUNK_BYTES * 2).unwrap();
        assert_eq!(recv.receive((CHUNK_BYTES * 4) as u64, vec![4; CHUNK_BYTES],
            |_| unreachable!()), Err(ChunkError::OutsideWindow));
        recv.receive(CHUNK_BYTES as u64, vec![1; CHUNK_BYTES], |_| unreachable!()).unwrap();
        assert_eq!(recv.receive(CHUNK_BYTES as u64, vec![7; CHUNK_BYTES],
            |_| unreachable!()), Err(ChunkError::ConflictingDuplicate));
        assert_eq!(recv.receive(0, vec![0; CHUNK_BYTES],
            |_| Err(ChunkError::DiskWrite)), Err(ChunkError::DiskWrite));
        assert_eq!(recv.committed(), 0);
        assert_eq!(recv.buffered_bytes(), CHUNK_BYTES * 2);
    }

    #[test]
    fn handles_final_partial_chunk_and_prevents_bad_offsets() {
        let total = (CHUNK_BYTES + 71) as u64;
        let mut recv = BoundedReassembly::new(total, CHUNK_BYTES * 2).unwrap();
        let mut bytes = Vec::new();
        assert_eq!(recv.receive(CHUNK_BYTES as u64, vec![9; 71], |block| {
            bytes.extend_from_slice(block); Ok(bytes.len() as u64)
        }).unwrap(), 0);
        assert_eq!(recv.receive(1, vec![8; CHUNK_BYTES], |_| unreachable!()),
            Err(ChunkError::MisalignedOffset));
        // A physically received chunk must not be acknowledged out of order.
        assert_eq!(recv.receive(0, vec![8; CHUNK_BYTES], |block| {
            bytes.extend_from_slice(block); Ok(bytes.len() as u64)
        }).unwrap(), total);
        assert!(recv.is_complete());
        assert_eq!(bytes.len() as u64, total);
        assert_eq!(bytes[0], 8);
        assert_eq!(bytes[CHUNK_BYTES], 9);
    }

    #[test]
    fn ack_resume_never_trusts_just_issued_quic_writes() {
        let mut send = ResendWindow::new((CHUNK_BYTES * 6) as u64, CHUNK_BYTES * 4).unwrap();
        for _ in 0..4 { send.next_chunk().unwrap(); }
        assert!(send.next_chunk().is_none());
        assert_eq!(send.in_flight(), (CHUNK_BYTES * 4) as u64);
        assert!(send.ack(CHUNK_BYTES as u64 - 1).is_err());
        send.ack(CHUNK_BYTES as u64).unwrap();
        assert_eq!(send.acknowledged(), CHUNK_BYTES as u64);
        assert!(send.resume_at_receiver_ack((CHUNK_BYTES * 5) as u64).is_err());
        send.resume_at_receiver_ack((CHUNK_BYTES * 2) as u64).unwrap();
        assert_eq!(send.in_flight(), 0);
        assert_eq!(send.next_chunk().unwrap().start, (CHUNK_BYTES * 2) as u64);
        assert!(send.ack((CHUNK_BYTES * 6) as u64).is_err());
    }

    #[test]
    fn accepts_large_offsets_without_u32_truncation() {
        let total = 5 * 1024 * 1024 * 1024u64;
        let mut send = ResendWindow::new(total, CHUNK_BYTES * 4).unwrap();
        for i in 0..5120u64 {
            let next = send.next_chunk().unwrap();
            assert_eq!(next.start, i * CHUNK_BYTES as u64);
            send.ack(next.end).unwrap();
        }
        assert!(send.is_complete());
        assert_eq!(send.acknowledged(), total);
        assert!(send.next_chunk().is_none());
    }
}
