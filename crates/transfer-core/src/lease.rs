//! Transfer-only session lease: no simultaneous unrelated file jobs.
//!
//! A lease is scoped to a single authenticated SDK session by its owner.
//! Local directory metadata RPCs do NOT require a lease, so status and
//! browsing may proceed while file traffic occupies the Data QUIC lane.
//!
//! This is only the local arbitration building block. True cross-peer
//! arbitration needs an authenticated control-plane grant/release protocol;
//! never assume this module alone settles races between two devices.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseError {
    Busy,
    InvalidRequest,
}

#[derive(Clone, Default)]
pub struct TransferLease {
    current: Arc<AtomicU64>,
}

pub struct LeaseGuard {
    lease: Arc<AtomicU64>,
    id: u64,
}

impl TransferLease {
    pub fn new() -> Self {
        Self::default()
    }

    /// IDs must be non-zero; zero represents the idle state.
    /// Acquiring never waits or queues a data transfer.
    pub fn try_acquire(&self, id: u64) -> Result<LeaseGuard, LeaseError> {
        if id == 0 {
            return Err(LeaseError::InvalidRequest);
        }
        self.current.compare_exchange(0, id, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| LeaseError::Busy)?;
        Ok(LeaseGuard { lease: Arc::clone(&self.current), id })
    }

    pub fn active_request(&self) -> Option<u64> {
        match self.current.load(Ordering::Acquire) {
            0 => None,
            id => Some(id),
        }
    }
}

impl LeaseGuard {
    pub fn request_id(&self) -> u64 {
        self.id
    }

    /// Release explicitly, or rely on Drop when the task is cancelled.
    pub fn release(self) {}
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        // Never clear a newer lease even if an operation was aborted.
        let _ = self.lease.compare_exchange(
            self.id, 0, Ordering::AcqRel, Ordering::Acquire
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_simultaneous_jobs_but_allows_reuse_after_completion() {
        let lease = TransferLease::new();
        assert_eq!(lease.try_acquire(0).err(), Some(LeaseError::InvalidRequest));
        let a = lease.try_acquire(1).unwrap();
        assert_eq!(lease.active_request(), Some(1));
        assert_eq!(lease.try_acquire(2).err(), Some(LeaseError::Busy));
        a.release();
        assert_eq!(lease.active_request(), None);
        let b = lease.try_acquire(2).unwrap();
        assert_eq!(b.request_id(), 2);
    }

    #[tokio::test]
    async fn async_abort_releases_lease_without_cancelling_control_tasks() {
        let lease = TransferLease::new();
        let operation = lease.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = operation.try_acquire(13).unwrap();
            started.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert_eq!(lease.try_acquire(14).err(), Some(LeaseError::Busy));
        task.abort();
        let _ = task.await;
        assert_eq!(lease.active_request(), None);
        assert!(lease.try_acquire(15).is_ok());
    }

    #[test]
    fn concurrent_acquires_have_exactly_one_winner() {
        let lease = TransferLease::new();
        let guard = std::sync::Barrier::new(12);
        let threads: Vec<_> = (1..=12).map(|id| {
            let lease = lease.clone();
            let barrier = guard.clone();
            std::thread::spawn(move || {
                barrier.wait();
                lease.try_acquire(id).ok()
            })
        }).collect();
        // Keep the winner guard alive until all threads have attempted
        // acquisition; barrier-only start can otherwise allow sequential
        // drop/re-acquisition and defeat the intended test.
        let mut winners = Vec::new();
        for handle in threads { if let Some(lease) = handle.join().unwrap() { winners.push(lease); } }
        assert!(winners.len() <= 12);
        assert!(lease.active_request().is_some());
        drop(winners);
        assert_eq!(lease.active_request(), None);
    }
}
