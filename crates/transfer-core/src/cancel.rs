//! Application-level authenticated transfer cancellation (not QUIC shutdown).
//! A cancellation can race the original GET/PUT control offer: bounded
//! tombstones prevent the late offer from restarting an already canceled job.
use std::{collections::{HashMap, VecDeque, HashSet}, sync::Mutex};
use tokio::sync::watch;

#[derive(Default)]
pub struct TransferCancels {
    inner: Mutex<CancelState>,
}
#[derive(Default)]
struct CancelState {
    active: HashMap<u64, ActiveCancel>,
    tombstones: HashSet<u64>,
    order: VecDeque<u64>,
    finished: HashSet<u64>,
    finished_order: VecDeque<u64>,
}
struct ActiveCancel {
    cancel: watch::Sender<bool>,
    finished: watch::Sender<bool>,
}
pub struct CancellationGuard<'a> {
    id: u64,
    finished: watch::Sender<bool>,
    registry: &'a TransferCancels,
}
impl Drop for CancellationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.registry.inner.lock() {
            s.active.remove(&self.id);
            if s.finished.insert(self.id) {
                if s.finished_order.len() >= 128 {
                    if let Some(old) = s.finished_order.pop_front() {
                        s.finished.remove(&old);
                    }
                }
                s.finished_order.push_back(self.id);
            }
        }
        // Publish completion AFTER removing the live transfer entry.
        // A CANCEL ACK cannot race ahead of sender/receiver termination.
        let _ = self.finished.send(true);
    }
}
impl TransferCancels {
    pub fn register(&self, id: u64) -> Option<(CancellationGuard<'_>, watch::Receiver<bool>)> {
        if id == 0 { return None; }
        let mut s = self.inner.lock().ok()?;
        if s.active.contains_key(&id) { return None; }
        let canceled = s.tombstones.contains(&id);
        let (tx, rx) = watch::channel(canceled);
        let (finished, _finished_rx) = watch::channel(false);
        s.active.insert(id, ActiveCancel { cancel: tx, finished: finished.clone() });
        Some((CancellationGuard {id, finished, registry: self}, rx))
    }
    pub fn cancel(&self, id: u64) -> bool {
        if id == 0 { return false; }
        let Ok(mut s) = self.inner.lock() else { return false };
        if !s.tombstones.contains(&id) {
            if s.order.len() >= 128 {
                if let Some(old) = s.order.pop_front() { s.tombstones.remove(&old); }
            }
            s.order.push_back(id);
            s.tombstones.insert(id);
        }
        if let Some(active) = s.active.get(&id) { let _ = active.cancel.send(true); }
        true
    }

    /// In addition to recording CANCEL, return a completion watcher if the
    /// requested transfer was alive. A missing transfer is only a pending
    /// tombstone and must not be acknowledged as already stopped.
    pub fn was_finished(&self, id: u64) -> bool {
        self.inner.lock().is_ok_and(|s| s.finished.contains(&id))
    }
    pub fn cancel_with_completion(&self, id: u64) -> (bool, Option<watch::Receiver<bool>>) {
        if id == 0 { return (false, None); }
        let completed = self.inner.lock().ok().and_then(|s| {
            s.active.get(&id).map(|x| x.finished.subscribe())
        });
        (self.cancel(id), completed)
    }
}

pub async fn finished(mut receiver: watch::Receiver<bool>) -> bool {
    loop {
        if *receiver.borrow() { return true; }
        if receiver.changed().await.is_err() { return false; }
    }
}
pub async fn canceled(mut receiver: watch::Receiver<bool>) {
    loop {
        if *receiver.borrow() { return; }
        if receiver.changed().await.is_err() { return; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancellation_notifies_active_and_early_registration() {
        let registry = TransferCancels::default();
        let (_guard, receiver) = registry.register(13).unwrap();
        assert!(registry.cancel(13));
        tokio::time::timeout(std::time::Duration::from_secs(1), canceled(receiver)).await.unwrap();
        drop(_guard);
        assert!(registry.cancel(14));
        let (_guard, rx) = registry.register(14).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), canceled(rx)).await.unwrap();
        assert!(registry.register(0).is_none());
        let (guard, _rx) = registry.register(16).unwrap();
        let (accepted, finished_rx) = registry.cancel_with_completion(16);
        assert!(accepted);
        let finished_rx = finished_rx.unwrap();
        assert!(!*finished_rx.borrow());
        drop(guard);
        assert!(registry.was_finished(16));
        assert!(tokio::time::timeout(std::time::Duration::from_secs(1), finished(finished_rx)).await.unwrap());
        let (accepted, pending) = registry.cancel_with_completion(99);
        assert!(accepted);
        assert!(pending.is_none());
    }
}
