use std::sync::{Arc, OnceLock};

use tauri::async_runtime::Mutex;

// Wait for a lane asynchronously before occupying a blocking-pool thread.
// Short filesystem requests must not queue behind a long media probe.
static MEDIA: OnceLock<Arc<Mutex<()>>> = OnceLock::new();
static FILES: OnceLock<Arc<Mutex<()>>> = OnceLock::new();

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
