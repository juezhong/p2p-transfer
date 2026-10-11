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
    active: HashMap<u64, watch::Sender<bool>>,
    tombstones: HashSet<u64>,
    order: VecDeque<u64>,
}
pub struct CancellationGuard<'a> {
    id: u64,
    registry: &'a TransferCancels,
}
impl Drop for CancellationGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.registry.inner.lock() {
            s.active.remove(&self.id);
        }
    }
}
impl TransferCancels {
    pub fn register(&self, id: u64) -> Option<(CancellationGuard<'_>, watch::Receiver<bool>)> {
        if id == 0 { return None; }
        let mut s = self.inner.lock().ok()?;
        if s.active.contains_key(&id) { return None; }
        let canceled = s.tombstones.contains(&id);
        let (tx, rx) = watch::channel(canceled);
        s.active.insert(id, tx);
        Some((CancellationGuard {id, registry: self}, rx))
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
        if let Some(active) = s.active.get(&id) { let _ = active.send(true); }
        true
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
    }
}
