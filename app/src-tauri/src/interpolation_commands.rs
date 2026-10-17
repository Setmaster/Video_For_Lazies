use crate::activity::ActivityCoordinator;
use crate::interpolation_components::{self, InterpolationStatus};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State, Window};

#[derive(Default)]
pub(crate) struct InterpolationInstaller {
    next_id: AtomicU64,
    current: Mutex<Option<InstallHandle>>,
}

struct InstallHandle {
    operation_id: u64,
    cancel: Arc<AtomicBool>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct InstallEvent {
    operation_id: u64,
    model_id: String,
    state: String,
    completed_bytes: u64,
    total_bytes: u64,
    message: String,
}

impl InterpolationInstaller {
    pub(crate) fn cancel_active(&self) {
        if let Ok(current) = self.current.lock()
            && let Some(handle) = current.as_ref()
        {
            handle.cancel.store(true, Ordering::Release);
        }
    }

    fn clear(&self, operation_id: u64) {
        if let Ok(mut current) = self.current.lock()
            && current
                .as_ref()
                .is_some_and(|h| h.operation_id == operation_id)
        {
            *current = None;
        }
    }
}

fn validate_model_id(model_id: &str) -> Result<(), String> {
    if matches!(model_id, "rife-v4.25" | "rife-v4.26") {
        Ok(())
    } else {
        Err("Choose a supported interpolation model.".into())
    }
}

#[tauri::command]
pub(crate) async fn interpolation_status(
    app: AppHandle,
    activity: State<'_, ActivityCoordinator>,
    model_id: String,
) -> Result<InterpolationStatus, String> {
    validate_model_id(&model_id)?;
    let lease = activity.begin("model check")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        interpolation_components::status(&app, &model_id)
    })
    .await
    .map_err(|_| "The interpolation check stopped. Try again.".to_string())?
}

#[tauri::command]
pub(crate) fn start_interpolation_install(
    window: Window,
    state: State<'_, InterpolationInstaller>,
    activity: State<'_, ActivityCoordinator>,
    model_id: String,
) -> Result<u64, String> {
    validate_model_id(&model_id)?;
    let lease = activity.begin("model installation")?;
    let previous_id = state
        .next_id
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
            id.checked_add(1)
                .filter(|next| *next <= 9_007_199_254_740_991)
        })
        .map_err(|_| "Restart the app before another model installation.".to_string())?;
    let operation_id = previous_id + 1;
    let cancel = Arc::new(AtomicBool::new(activity.is_closing()));
    {
        let mut current = state
            .current
            .lock()
            .map_err(|_| "The model installer is unavailable. Restart the app.".to_string())?;
        if current.is_some() {
            return Err("A model installation is already running.".into());
        }
        *current = Some(InstallHandle {
            operation_id,
            cancel: cancel.clone(),
        });
    }
    let app = window.app_handle().clone();
    let recovery_app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("interpolation-install".into())
        .spawn(move || {
            let mut total = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                interpolation_components::install(&app, &model_id, &cancel, |update| {
                    total = update.total_bytes;
                    // Only the completed operation owns the terminal event. In
                    // particular Ready cannot precede released resource leases.
                    if matches!(update.state.as_str(), "ready" | "canceled" | "failed") {
                        return;
                    }
                    let _ = window.emit(
                        "interpolation-install-progress",
                        InstallEvent {
                            operation_id,
                            model_id: model_id.clone(),
                            state: update.state,
                            completed_bytes: update.completed_bytes,
                            total_bytes: update.total_bytes,
                            message: update.message,
                        },
                    );
                })
            }))
            .unwrap_or_else(|_| Err("The model installation stopped. Try again.".into()));
            let (state, completed, message) = match result {
                Ok(status) if status.state == "ready" => {
                    ("ready", status.download_bytes, status.message)
                }
                Ok(status) => ("failed", 0, status.message),
                Err(_) if cancel.load(Ordering::Acquire) => {
                    ("canceled", 0, "Model installation canceled.".into())
                }
                Err(message) => ("failed", 0, message),
            };
            app.state::<InterpolationInstaller>().clear(operation_id);
            drop(lease);
            let _ = window.emit(
                "interpolation-install-progress",
                InstallEvent {
                    operation_id,
                    model_id,
                    state: state.into(),
                    completed_bytes: completed,
                    total_bytes: total,
                    message,
                },
            );
        });
    if spawned.is_err() {
        recovery_app
            .state::<InterpolationInstaller>()
            .clear(operation_id);
        return Err("Could not start model installation. Try again.".into());
    }
    Ok(operation_id)
}

#[tauri::command]
pub(crate) fn cancel_interpolation_install(
    state: State<'_, InterpolationInstaller>,
    operation_id: u64,
) -> Result<(), String> {
    let current = state
        .current
        .lock()
        .map_err(|_| "The model installer is unavailable. Restart the app.".to_string())?;
    if let Some(handle) = current.as_ref()
        && handle.operation_id == operation_id
    {
        handle.cancel.store(true, Ordering::Release);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn remove_interpolation_model(
    app: AppHandle,
    activity: State<'_, ActivityCoordinator>,
    model_id: String,
) -> Result<InterpolationStatus, String> {
    validate_model_id(&model_id)?;
    let lease = activity.begin("model removal")?;
    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        interpolation_components::remove(&app, &model_id)
    })
    .await
    .map_err(|_| "The model removal stopped. Try again.".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_install_clear_and_cancel_do_not_release_another_operation() {
        let state = InterpolationInstaller::default();
        let cancel = Arc::new(AtomicBool::new(false));
        *state.current.lock().unwrap() = Some(InstallHandle {
            operation_id: 7,
            cancel: cancel.clone(),
        });
        state.clear(6);
        assert!(state.current.lock().unwrap().is_some());
        state.cancel_active();
        assert!(cancel.load(Ordering::Acquire));
        state.clear(7);
        assert!(state.current.lock().unwrap().is_none());
    }
}
