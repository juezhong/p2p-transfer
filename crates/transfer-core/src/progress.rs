//! Transfer-owned live progress reported on confirmed disk-write ACKs.
//! No progress byte is counted solely because it entered a QUIC send buffer.

use std::{sync::Mutex, time::{Duration, Instant}};

#[derive(Clone, Debug)]
pub struct ProgressSnapshot {
    pub direction: &'static str,
    pub file: String,
    pub written: u64,
    pub total: u64,
    pub elapsed: Duration,
}

impl ProgressSnapshot {
    pub fn percent(&self) -> f64 {
        if self.total == 0 { 100.0 } else {
            100.0 * self.written as f64 / self.total as f64
        }
    }
    pub fn bytes_per_second(&self) -> f64 {
        self.written as f64 / self.elapsed.as_secs_f64().max(0.001)
    }
    pub fn eta(&self) -> Option<Duration> {
        let rate = self.bytes_per_second();
        if rate < 1.0 { None } else {
            Some(Duration::from_secs_f64(
                self.total.saturating_sub(self.written) as f64 / rate
            ))
        }
    }
}

struct ActiveFile {
    direction: &'static str,
    file: String,
    written: u64,
    total: u64,
    started: Instant,
}

#[derive(Default)]
pub struct TransferProgress {
    active: Mutex<Option<ActiveFile>>,
}

impl TransferProgress {
    pub fn begin(&self, direction: &'static str, file: String, total: u64) {
        if let Ok(mut guard) = self.active.lock() {
            *guard = Some(ActiveFile {
                direction, file, written: 0, total, started: Instant::now(),
            });
        }
    }
    /// Updates are absolute byte offsets, never speculative queued bytes.
    pub fn written(&self, bytes: u64) {
        if let Ok(mut guard) = self.active.lock() {
            if let Some(file) = guard.as_mut() {
                file.written = file.written.max(bytes).min(file.total);
            }
        }
    }
    pub fn snapshot(&self) -> Option<ProgressSnapshot> {
        self.active.lock().ok()?.as_ref().map(|x| ProgressSnapshot {
            direction: x.direction, file: x.file.clone(), written: x.written,
            total: x.total, elapsed: x.started.elapsed(),
        })
    }
    pub fn clear(&self) {
        if let Ok(mut guard) = self.active.lock() { *guard = None; }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_verified_not_speculative_and_eta_is_finite() {
        let p = TransferProgress::default();
        assert!(p.snapshot().is_none());
        p.begin("PUT", "2GB.bin".into(), 100);
        p.written(20);
        p.written(10);
        let s = p.snapshot().unwrap();
        assert_eq!(s.written, 20);
        assert_eq!(s.percent(), 20.0);
        assert!(s.eta().is_some());
        p.written(200);
        assert_eq!(p.snapshot().unwrap().written, 100);
        p.clear();
        assert!(p.snapshot().is_none());
    }
}
