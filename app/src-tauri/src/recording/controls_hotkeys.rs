//! Optional recording shortcuts. Empty preferences never register a key.
#![cfg_attr(not(windows), allow(dead_code))]
mod gate;
mod keymap;
mod prefs;
mod registry;
mod service;
#[cfg(test)]
mod tests;
mod types;
#[cfg(windows)]
mod windows;
use super::RecordingConfig;
pub(crate) use gate::StartPermit;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::mpsc,
    thread::JoinHandle,
};
use tauri::{AppHandle, WebviewWindow};
pub(crate) use types::*;

#[derive(Clone)]
pub(crate) struct SessionTarget {
    pub session_id: String,
    pub control_revision: u64,
    pub can_pause: bool,
    pub can_resume: bool,
    pub stop: Arc<AtomicBool>,
}
pub(crate) struct StartAttempt {
    pub token: String,
    pub config: RecordingConfig,
    pub permit: StartPermit,
    pub cancel: Arc<AtomicBool>,
}
pub(crate) enum ControlIntent {
    Start(Box<StartAttempt>),
    Pause {
        owner_token: String,
        operation_id: String,
        session_id: String,
        control_revision: u64,
    },
    Resume {
        owner_token: String,
        operation_id: String,
        session_id: String,
        control_revision: u64,
    },
}
pub(crate) trait ControlsBackend: Send + Sync {
    fn is_idle(&self) -> bool;
    fn validate_draft(&self, config: RecordingConfig) -> Result<RecordingConfig, String>;
    fn dispatch(&self, intent: ControlIntent) -> Result<(), String>;
    fn replace_owner(&self, owner_token: Option<String>);
}
#[derive(Clone)]
pub(crate) struct ControlsManager {
    shared: Arc<Shared>,
}
struct Shared {
    gate: Mutex<gate::ArmGate>,
    backend: Mutex<Option<Arc<dyn ControlsBackend>>>,
    app: Mutex<Option<AppHandle>>,
    shutting_down: AtomicBool,
    worker_stopping: AtomicBool,
    cleanup_error: Mutex<Option<String>>,
    effects: Mutex<Effects>,
    commands: Mutex<Option<mpsc::SyncSender<WorkerCommand>>>,
    worker: Mutex<Option<JoinHandle<Result<(), String>>>>,
    hotkeys: Mutex<BTreeMap<i32, registry::Entry>>,
    pending: Mutex<VecDeque<registry::Entry>>,
}
impl Default for ControlsManager {
    fn default() -> Self {
        Self {
            shared: Arc::new(Shared {
                gate: Mutex::new(gate::ArmGate::default()),
                backend: Mutex::new(None),
                app: Mutex::new(None),
                shutting_down: AtomicBool::new(false),
                worker_stopping: AtomicBool::new(false),
                cleanup_error: Mutex::new(None),
                effects: Mutex::new(Effects::default()),
                commands: Mutex::new(None),
                worker: Mutex::new(None),
                hotkeys: Mutex::new(BTreeMap::new()),
                pending: Mutex::new(VecDeque::new()),
            }),
        }
    }
}
impl ControlsManager {
    /// Capture before any slow UI validation. A subsequent native invalidation
    /// makes this exact permit stale even if assignment has already ended.
    pub(crate) fn ui_start_permit(&self, expected: UiStartContext) -> Result<StartPermit, String> {
        let mut g = self
            .shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        g.owner(&expected.owner_token)?;
        if g.invalidation_epoch != expected.invalidation_epoch
            || g.minimize_cycle != expected.minimize_cycle
        {
            return Err("UI Start context was retired before arrival.".into());
        }
        if self.shared.shutting_down.load(Ordering::Acquire)
            || g.stopped
            || g.assignment_suppressed
            || g.assignment.is_some()
            || g.session.is_some()
            || g.attempt.is_some()
        {
            return Err("Recording admission is busy.".into());
        }
        g.advance_start()?;
        let token = g.token()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let permit = StartPermit {
            shared: Arc::downgrade(&self.shared),
            token: token.clone(),
            arm_token: String::new(),
            epoch: g.invalidation_epoch,
            cycle: g.minimize_cycle,
            owner: g.owner.clone(),
            ui: true,
            consumed: Arc::new(AtomicBool::new(false)),
            cancel: cancel.clone(),
        };
        g.attempt = Some(gate::AttemptState {
            token,
            session_id: None,
            cancel,
            admitted: false,
            hud_visible: true,
        });
        Ok(permit)
    }
}
impl StartPermit {
    pub(crate) fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
}

struct Effects {
    bindings: Bindings,
    rows: Rows,
    entries: BTreeMap<Action, registry::Entry>,
}
impl Default for Effects {
    fn default() -> Self {
        let row = || BindingRow {
            configured: None,
            registered: None,
            dispatch_enabled: false,
            status: RowStatus::Unset,
            reason: None,
        };
        Self {
            bindings: Bindings::default(),
            rows: Rows {
                start: row(),
                pause: row(),
                resume: row(),
                stop: row(),
            },
            entries: BTreeMap::new(),
        }
    }
}
enum WorkerCommand {
    Request(Box<ControlsRequest>, mpsc::Sender<ControlsReply>),
    Refresh,
    HudVisible { attempt: String, session: String },
    Finish { attempt: String },
}
#[tauri::command]
pub(crate) async fn recording_controls(
    window: WebviewWindow,
    state: tauri::State<'_, ControlsManager>,
    request: ControlsRequest,
) -> Result<ControlsReply, String> {
    if window.label() != "main" {
        return Err("Recording controls require the main window.".into());
    }
    state.handle(request).await
}
#[tauri::command]
pub(crate) fn recording_controls_normalize(
    window: WebviewWindow,
    state: tauri::State<'_, ControlsManager>,
    owner_token: String,
    assignment_token: String,
    event: KeyCapture,
) -> Result<Chord, String> {
    if window.label() != "main" {
        return Err("Recording controls require the main window.".into());
    }
    state.normalize(&owner_token, &assignment_token, event)
}
