use std::sync::{Arc, OnceLock};

use tauri::async_runtime::Mutex;

// Wait for a lane asynchronously before occupying a blocking-pool thread.
// Short filesystem requests must not queue behind a long media probe.
static MEDIA: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static FILES: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static FRAME_TIMING: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static SOURCE_FRAME_INDEX: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

async fn dispatch<T: Send + 'static>(
    lane: &OnceLock<Arc<Mutex<()>>>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let permit = lane
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
        .lock_owned()
        .await;
    tauri::async_runtime::spawn_blocking(move || {
        // Keep the permit until work actually ends, even if its caller leaves.
        let _permit = permit;
        work()
    })
    .await
    .map_err(|error| format!("Backend command failed: {error}"))?
}

pub async fn media<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    dispatch(&MEDIA, work).await
}

pub async fn files<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    dispatch(&FILES, work).await
}

pub async fn frame_timing<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    dispatch(&FRAME_TIMING, work).await
}

async fn dispatch_index<T: Send + 'static>(
    lane: &OnceLock<Arc<Mutex<()>>>,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    // A stale scan may run to its deadline, but new calls never queue behind
    // it or occupy blocking-pool threads while waiting for an index permit.
    let permit = lane
        .get_or_init(|| Arc::new(Mutex::new(())))
        .clone()
        .try_lock_owned()
        .map_err(|_| {
            "Source frame indexing is busy. Retry when the current index finishes.".to_string()
        })?;
    tauri::async_runtime::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| "Source frame indexing could not finish. Retry indexing the source.".to_string())?
}

pub async fn source_frame_index<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    dispatch_index(&SOURCE_FRAME_INDEX, work).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn frame_index_lane_rejects_busy_calls_and_survives_abandoned_callers() {
        use std::sync::mpsc;
        use std::time::Duration;
        let lane = Arc::new(OnceLock::new());
        let first_lane = lane.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = tauri::async_runtime::spawn(async move {
            dispatch_index(&first_lane, move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        first.abort();
        tauri::async_runtime::block_on(async {
            assert!(
                dispatch_index(&lane, || -> Result<(), String> {
                    panic!("busy work must never queue or run")
                })
                .await
                .unwrap_err()
                .contains("busy")
            );
            // The independent existing frame-step lane remains available.
            assert_eq!(frame_timing(|| Ok(42)).await.unwrap(), 42);
        });
        release_tx.send(()).unwrap();
        // Waiting for this specific guard to be released is bounded; the call
        // itself never waits or queues behind the old scan.
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while lane.get().unwrap().try_lock().is_err() {
            assert!(std::time::Instant::now() < until);
            std::thread::yield_now();
        }
        tauri::async_runtime::block_on(async {
            assert_eq!(dispatch_index(&lane, || Ok(7)).await.unwrap(), 7);
        });
    }

    #[test]
    fn abandoned_caller_keeps_lane_until_blocking_work_finishes() {
        use std::sync::mpsc;
        use std::time::Duration;

        let lane = Arc::new(OnceLock::new());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_lane = lane.clone();
        let first = tauri::async_runtime::spawn(async move {
            dispatch(&first_lane, move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            })
            .await
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        first.abort();

        let (queued_tx, queued_rx) = mpsc::channel();
        let (ran_tx, ran_rx) = mpsc::channel();
        let second = tauri::async_runtime::spawn(async move {
            queued_tx.send(()).unwrap();
            dispatch(&lane, move || {
                ran_tx.send(()).unwrap();
                Ok(())
            })
            .await
        });
        queued_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            ran_rx.recv_timeout(Duration::from_millis(40)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(()).unwrap();
        tauri::async_runtime::block_on(second).unwrap().unwrap();
        ran_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn queued_commands_bound_blocking_work_and_release_lane_after_errors() {
        let lane = Arc::new(OnceLock::new());
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        tauri::async_runtime::block_on(async {
            let mut tasks = Vec::new();
            for index in 0..8 {
                let lane = lane.clone();
                let running = running.clone();
                let peak = peak.clone();
                tasks.push(tauri::async_runtime::spawn(async move {
                    dispatch(&lane, move || {
                        let count = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(count, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        running.fetch_sub(1, Ordering::SeqCst);
                        if index == 0 {
                            Err("expected".to_string())
                        } else {
                            Ok(index)
                        }
                    })
                    .await
                }));
            }
            for (index, task) in tasks.into_iter().enumerate() {
                assert_eq!(
                    task.await.unwrap(),
                    if index == 0 {
                        Err("expected".to_string())
                    } else {
                        Ok(index)
                    }
                );
            }
        });
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        assert_eq!(running.load(Ordering::SeqCst), 0);
    }
}
