//! One atomic admission claim for operations which use or replace media resources.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct ActivityCoordinator {
    active: Arc<Mutex<Option<ActiveActivity>>>,
    next_id: Arc<AtomicU64>,
    closing: Arc<AtomicBool>,
}

struct ActiveActivity {
    id: u64,
    label: &'static str,
}

pub(crate) struct ActivityLease {
    coordinator: ActivityCoordinator,
    id: u64,
}

/// Allocate a positive ID without wrapping or advancing an exhausted counter.
pub(crate) fn allocate_operation_id(counter: &AtomicU64, maximum: u64) -> Option<u64> {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1).filter(|next| *next <= maximum)?;
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(next),
            Err(actual) => current = actual,
        }
    }
}

impl ActivityCoordinator {
    /// Reserve admission before the WebView can dispatch any commands.
    pub(crate) fn startup_pending() -> Self {
        Self {
            active: Arc::new(Mutex::new(Some(ActiveActivity {
                id: 0,
                label: "update startup check",
            }))),
            ..Self::default()
        }
    }

    pub(crate) fn begin(&self, label: &'static str) -> Result<ActivityLease, String> {
        self.begin_inner(label, false)
    }

    pub(crate) fn begin_startup(&self) -> Result<ActivityLease, String> {
        self.begin_inner("update startup check", true)
    }

    fn begin_inner(
        &self,
        label: &'static str,
        allow_startup: bool,
    ) -> Result<ActivityLease, String> {
        let mut active = self.active.lock().map_err(|_| {
            "The app is temporarily unavailable. Restart and try again.".to_string()
        })?;
        if self.closing.load(Ordering::Acquire) {
            return Err("The app is closing.".to_string());
        }
        if let Some(current) = active.as_ref()
            && !(allow_startup && current.id == 0)
        {
            return Err(format!("Wait for the current {} to finish.", current.label));
        }
        let id = allocate_operation_id(&self.next_id, u64::MAX)
            .ok_or_else(|| "Restart the app before starting another operation.".to_string())?;
        *active = Some(ActiveActivity { id, label });
        Ok(ActivityLease {
            coordinator: self.clone(),
            id,
        })
    }

    pub(crate) fn shutdown(&self) {
        // Serialize with admission so a new claim cannot slip past shutdown.
        let _active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        self.closing.store(true, Ordering::Release);
    }

    pub(crate) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
}

impl Drop for ActivityLease {
    fn drop(&mut self) {
        if let Ok(mut active) = self.coordinator.active.lock()
            && active.as_ref().is_some_and(|current| current.id == self.id)
        {
            *active = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ids_stop_at_the_limit_without_wrapping_or_advancing() {
        for maximum in [9_007_199_254_740_991, u64::MAX] {
            let counter = AtomicU64::new(maximum - 1);
            assert_eq!(allocate_operation_id(&counter, maximum), Some(maximum));
            assert_eq!(allocate_operation_id(&counter, maximum), None);
            assert_eq!(counter.load(Ordering::Relaxed), maximum);
        }
    }

    #[test]
    fn concurrent_operation_ids_remain_unique_and_contiguous() {
        let counter = Arc::new(AtomicU64::new(0));
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let counter = counter.clone();
                std::thread::spawn(move || {
                    (0..32)
                        .map(|_| allocate_operation_id(&counter, u64::MAX).unwrap())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut ids: Vec<_> = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=128).collect::<Vec<_>>());
    }

    #[test]
    fn resource_mutations_and_exports_share_one_atomic_claim() {
        let coordinator = ActivityCoordinator::default();
        let cloned = coordinator.clone();
        let export = coordinator.begin("export").unwrap();
        for operation in [
            "model installation",
            "model removal",
            "model check",
            "app update",
        ] {
            assert!(matches!(cloned.begin(operation), Err(message) if message.contains("export")));
        }
        drop(export);
        let installation = cloned.begin("model installation").unwrap();
        assert!(coordinator.begin("export").is_err());
        drop(installation);
        assert!(coordinator.begin("export").is_ok());
    }

    #[test]
    fn shutdown_never_reopens_when_an_old_claim_finishes() {
        let coordinator = ActivityCoordinator::default();
        let lease = coordinator.begin("model installation").unwrap();
        coordinator.shutdown();
        drop(lease);
        assert!(coordinator.is_closing());
        assert!(coordinator.begin("export").is_err());
    }

    #[test]
    fn startup_reservation_precedes_and_serializes_all_commands() {
        let coordinator = ActivityCoordinator::startup_pending();
        assert!(coordinator.begin("model check").is_err());
        assert!(coordinator.begin("export").is_err());
        let startup = coordinator.begin_startup().unwrap();
        assert!(coordinator.begin_startup().is_err());
        assert!(coordinator.begin("model check").is_err());
        drop(startup);
        assert!(coordinator.begin("model check").is_ok());
    }
}
