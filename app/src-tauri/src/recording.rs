//! Recording owns its native process, activity lease and recovery files through
//! publication. A separate leased lane delivers only bounded preview thumbnails;
//! full capture frames and PCM remain in the native helper.
use crate::{
    activity::{ActivityCoordinator, ActivityLease},
    process, video,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow as Window};

const PROTOCOL_LIMIT: u64 = 65_536;
const JOURNAL_VERSION: u32 = 2;
const RESERVE_BYTES: u64 = 128 * 1024 * 1024;

// The writer and checked reader are shared with the separate native helper.
#[allow(dead_code)]
mod active_clock;
pub(crate) mod audio_control_state;
pub(crate) mod audio_controls;
#[allow(dead_code)]
mod audio_envelope;
mod audio_finalize;
pub(crate) mod audio_manager;
#[allow(dead_code)]
mod audio_mixer;
#[allow(dead_code)]
pub(crate) mod audio_reconcile_wire;
mod audio_suppression;
#[allow(dead_code)]
mod audio_types;
mod clock_status;
pub(crate) mod controls_hotkeys;
#[allow(dead_code)]
mod preview;
pub(crate) mod preview_manager;
pub(crate) mod session_controls;
use audio_types::{AudioStatus, Microphone, RecordingAudioConfig, RenderAudio, Suppression};
mod source;
#[cfg(test)]
use source::Region;
pub(crate) use source::{Geometry, Source};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordingConfig {
    pub(crate) request_id: String,
    pub(crate) source: Source,
    pub(crate) fps: u32,
    pub(crate) quality: String,
    pub(crate) countdown_seconds: u32,
    pub(crate) show_cursor: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) audio: Option<RecordingAudioConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    system_audio_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    microphone_device_id: Option<String>,
    pub(crate) output_directory: String,
}

impl RecordingConfig {
    fn public_snapshot_config(&self) -> Self {
        let mut config = self.clone();
        config.audio = Some(self.audio_config());
        config.system_audio_device_id = None;
        config.microphone_device_id = None;
        config
    }

    fn audio_config(&self) -> RecordingAudioConfig {
        self.audio.clone().unwrap_or_else(|| RecordingAudioConfig {
            render: self
                .system_audio_device_id
                .as_ref()
                .map_or(RenderAudio::Off, |id| RenderAudio::System {
                    device_id: id.clone(),
                }),
            microphone: self.microphone_device_id.as_ref().map(|id| Microphone {
                device_id: id.clone(),
                suppression: Suppression::Off,
            }),
            ..RecordingAudioConfig::default()
        })
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Recovery {
    session_id: String,
    created_at: String,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    session_id: Option<String>,
    sequence: u64,
    phase: String,
    elapsed_ms: u64,
    bytes_written: u64,
    countdown_remaining: Option<u32>,
    output_path: Option<String>,
    message: Option<String>,
    recoveries: Vec<Recovery>,
    config: Option<RecordingConfig>,
    selection_request_id: Option<String>,
    resolved_geometry: Option<Geometry>,
    audio: Option<AudioStatus>,
    control_revision: u64,
    pause: Option<audio_controls::PauseSeal>,
    audio_control_ledger: Option<audio_controls::AudioLedger>,
    pending_transition: Option<session_controls::PendingTransition>,
    resolved_role_mask: u8,
    clock_epoch: Option<clock_status::ClockEpoch>,
    last_pause_clock: Option<clock_status::PauseClock>,
}

impl Snapshot {
    fn matches_transition(&self, operation: &str, revision: Option<u64>) -> bool {
        self.pending_transition
            .as_ref()
            .is_some_and(|p| p.operation_id == operation && Some(p.control_revision) == revision)
    }
    fn apply_transition_timeout(&mut self, timeout: &session_controls::TransitionTimeout) -> bool {
        if self.session_id.as_deref() != Some(&timeout.session_id)
            || !self.pending_transition.as_ref().is_some_and(|p| {
                p.operation_id == timeout.operation_id
                    && p.control_revision == timeout.control_revision
            })
        {
            return false;
        }
        self.pending_transition = None;
        self.phase = if timeout.code == "resumeTimeout" {
            "paused"
        } else {
            "stopping"
        }
        .into();
        self.message=Some(if timeout.code=="resumeTimeout"{"Resume expired before synchronization completed. Use a fresh Resume after synchronization."}else{"The recording transition timed out. Stopping and saving the retained prefix."}.into());
        true
    }
    fn retain_clock(
        &mut self,
        session: &str,
        kind: &str,
        value: Value,
        revision: u64,
    ) -> Result<bool, String> {
        if self.session_id.as_deref() != Some(session) {
            return Ok(false);
        }
        match kind {
            "initial" => {
                let epoch = clock_status::ClockEpoch::read(value)?;
                if epoch.active_sample != "0" {
                    return Err("Initial recording clock must start at sample zero.".into());
                }
                if let Some(old) = &self.clock_epoch {
                    return if *old == epoch {
                        Ok(false)
                    } else {
                        Err("Initial recording clock changed.".into())
                    };
                }
                self.clock_epoch = Some(epoch);
            }
            "pause" => {
                let epoch = self
                    .clock_epoch
                    .as_ref()
                    .ok_or("Recording clock epoch is unavailable.")?;
                let pause = clock_status::PauseClock::read(value, epoch, revision)?;
                if let Some(old) = &self.last_pause_clock
                    && old.pause_revision >= revision
                {
                    return if *old == pause {
                        Ok(false)
                    } else {
                        Err("Pause clock revision regressed.".into())
                    };
                }
                self.last_pause_clock = Some(pause);
            }
            "resume" => {
                let epoch = clock_status::ClockEpoch::read(value)?;
                let pause = self
                    .last_pause_clock
                    .as_ref()
                    .ok_or("Paused recording clock is unavailable.")?;
                if pause.pause_revision != revision {
                    return Err("Resumed clock belongs to another Pause.".into());
                }
                epoch.resumed_after(pause)?;
                if self.clock_epoch.as_ref() == Some(&epoch) {
                    return Ok(false);
                }
                self.clock_epoch = Some(epoch);
            }
            _ => return Err("Unknown recording clock event.".into()),
        }
        self.sequence = self.sequence.saturating_add(1);
        Ok(true)
    }
    fn sync_audio_control(&mut self, audio: audio_controls::ControlSnapshot) -> bool {
        let changed = self.audio_control_ledger != audio.ledger
            || self
                .pause
                .as_ref()
                .is_some_and(|p| p.audio != audio.pause_audio);
        if changed {
            if let Some(pause) = &mut self.pause {
                pause.audio = audio.pause_audio;
            }
            self.audio_control_ledger = audio.ledger;
            self.sequence = self.sequence.saturating_add(1);
        }
        changed
    }
    fn set_recovery_source(&mut self, journal: &Journal) {
        self.session_id = Some(journal.session_id.clone());
        self.config = Some(journal.config.public_snapshot_config());
        // Recovery finalizes retained media; it has no live audio-meter session.
        self.audio = None;
        self.pause = None;
        self.pending_transition = None;
        self.audio_control_ledger = None;
        self.resolved_role_mask = 0;
        self.clock_epoch = None;
        self.last_pause_clock = None;
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            session_id: None,
            sequence: 0,
            phase: "idle".into(),
            elapsed_ms: 0,
            bytes_written: 0,
            countdown_remaining: None,
            output_path: None,
            message: None,
            recoveries: vec![],
            config: None,
            selection_request_id: None,
            resolved_geometry: None,
            audio: None,
            control_revision: 0,
            pause: None,
            audio_control_ledger: None,
            pending_transition: None,
            resolved_role_mask: 0,
            clock_epoch: None,
            last_pause_clock: None,
        }
    }
}
fn active(phase: &str) -> bool {
    matches!(
        phase,
        "preparing"
            | "countdown"
            | "starting"
            | "recording"
            | "pausing"
            | "paused"
            | "resuming"
            | "stopping"
            | "finalizing"
    )
}

#[derive(Default)]
struct Inner {
    snapshot: Snapshot,
    request_id: Option<String>,
    stop: Option<Arc<AtomicBool>>,
    selecting: Option<(String, Arc<AtomicBool>)>,
    loaded: bool,
    recovery_revision: u64,
    start_attempt: Option<String>,
}
impl Inner {
    fn replace_recoveries(&mut self, recoveries: Vec<Recovery>) {
        self.snapshot.recoveries = recoveries;
        self.recovery_revision = self.recovery_revision.saturating_add(1);
    }
}
#[derive(Clone, Default)]
pub(crate) struct RecordingManager {
    inner: Arc<Mutex<Inner>>,
    startup_load: Arc<Mutex<()>>,
    preview: Arc<preview_manager::PreviewManager>,
    audio: Arc<audio_manager::AudioManager>,
    pub(crate) controls: Arc<session_controls::SessionControls>,
}

impl RecordingManager {
    pub(crate) fn needs_close_guard(&self) -> bool {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        active(&i.snapshot.phase) || i.selecting.is_some()
    }
    pub(crate) fn shutdown(&self) {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stop) = &i.stop {
            stop.store(true, Ordering::Release);
        }
        if let Some((_, stop)) = &i.selecting {
            stop.store(true, Ordering::Release);
        }
        drop(i);
        self.preview.shutdown();
    }
    fn snapshot(&self) -> Snapshot {
        let snapshot = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone();
        let context = self.controls.context();
        self.audio.set_context(context.clone());
        if snapshot.session_id == context.session_id {
            let audio = self.audio.snapshot_control();
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if i.snapshot.session_id == snapshot.session_id
                && i.snapshot.sequence == snapshot.sequence
            {
                i.snapshot.sync_audio_control(audio);
            }
            return i.snapshot.clone();
        }
        snapshot
    }
    fn update(&self, app: &AppHandle, id: &str, change: impl FnOnce(&mut Snapshot)) {
        self.update_inner(app, id, None, change);
    }
    fn update_recoveries(
        &self,
        app: &AppHandle,
        id: &str,
        recoveries: Vec<Recovery>,
        change: impl FnOnce(&mut Snapshot),
    ) {
        self.update_inner(app, id, Some(recoveries), change);
    }
    fn update_inner(
        &self,
        app: &AppHandle,
        id: &str,
        recoveries: Option<Vec<Recovery>>,
        change: impl FnOnce(&mut Snapshot),
    ) {
        let audio = self.audio.snapshot_control();
        let audio_session = self.controls.context().session_id;
        let snapshot = {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if i.snapshot.session_id.as_deref() != Some(id) {
                return;
            }
            if let Some(recoveries) = recoveries {
                i.replace_recoveries(recoveries);
            }
            change(&mut i.snapshot);
            if audio_session.as_deref() == Some(id) {
                if let Some(pause) = &mut i.snapshot.pause {
                    pause.audio = audio.pause_audio;
                }
                i.snapshot.audio_control_ledger = audio.ledger;
            }
            self.controls.phase(id, &i.snapshot.phase);
            i.snapshot.sequence = i.snapshot.sequence.saturating_add(1);
            i.snapshot.clone()
        };
        self.publish_control_target(app);
        let _ = app.emit("recording-status", snapshot);
    }
    fn publish_control_target(&self, app: &AppHandle) {
        let Some(service) = app.try_state::<controls_hotkeys::ControlsManager>() else {
            return;
        };
        let target = {
            let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            match (&i.snapshot.session_id, &i.stop) {
                (Some(id), Some(stop)) if active(&i.snapshot.phase) => {
                    Some(controls_hotkeys::SessionTarget {
                        session_id: id.clone(),
                        control_revision: i.snapshot.control_revision,
                        can_pause: i.snapshot.phase == "recording",
                        can_resume: i.snapshot.phase == "paused"
                            && i.snapshot.pending_transition.is_none()
                            && !i
                                .snapshot
                                .pause
                                .as_ref()
                                .and_then(|p| p.audio.as_ref())
                                .is_some_and(|a| {
                                    a.fence.state
                                        == audio_controls::FencePhase::ReplacementReviewRequired
                                }),
                        stop: stop.clone(),
                    })
                }
                _ => None,
            }
        };
        service.set_session(target);
    }
    fn load(&self, app: &AppHandle) -> Result<(), String> {
        self.load_with(|| find_recoveries(app))
    }
    fn load_with(
        &self,
        mut discover: impl FnMut() -> Result<Vec<Recovery>, String>,
    ) -> Result<(), String> {
        // One startup worker owns discovery. Concurrent polls return current
        // state without caching a partial scan or accumulating blocked workers.
        let _load = match self.startup_load.try_lock() {
            Ok(load) => load,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("Recording recovery discovery is unavailable.".into());
            }
        };
        // Directory discovery and whole-file publication checks never own the
        // session mutex or the native UI thread. Stop/Close remain responsive.
        loop {
            let revision = {
                let i = self
                    .inner
                    .lock()
                    .map_err(|_| "Recording state is unavailable.")?;
                if i.loaded {
                    return Ok(());
                }
                i.recovery_revision
            };
            let recovered = discover()?;
            let mut i = self
                .inner
                .lock()
                .map_err(|_| "Recording state is unavailable.")?;
            if i.recovery_revision != revision {
                // A terminal save/recovery/discard refreshed the list while
                // this scan ran. Never overwrite it with an older discovery.
                continue;
            }
            i.replace_recoveries(recovered);
            i.loaded = true;
            i.snapshot.sequence += 1;
            return Ok(());
        }
    }
}

pub(crate) fn validate_start_draft(
    app: &AppHandle,
    manager: &RecordingManager,
    config: RecordingConfig,
) -> Result<RecordingConfig, String> {
    validate_start_draft_cancel(app, manager, config, Arc::new(AtomicBool::new(false)))
}
fn validate_start_draft_cancel(
    app: &AppHandle,
    manager: &RecordingManager,
    mut config: RecordingConfig,
    cancel: Arc<AtomicBool>,
) -> Result<RecordingConfig, String> {
    if cancel.load(Ordering::Acquire) {
        return Err("Recording preparation was canceled.".into());
    }
    config = config.public_snapshot_config();
    validate_config(&config)?;
    helper_path(app)?;
    let native_audio = manager
        .audio
        .resolve_audio_config("draft-validation", &config.audio_config())?;
    let pinned = if config.audio_config().enabled() {
        let resolved_audio = helper_call(
            app,
            json!({"operation":"resolve-audio","config":native_audio}),
            cancel.clone(),
            Some(Duration::from_secs(10)),
            |_| {},
        )?;
        serde_json::from_value::<audio_types::NativeAudioConfig>(resolved_audio).map_err(error)?
    } else {
        native_audio
    };
    if let Some(audio) = &mut config.audio {
        if let (
            RenderAudio::System { device_id },
            audio_types::NativeRender::System {
                device_id: pinned_id,
            },
        ) = (&mut audio.render, &pinned.render)
        {
            *device_id = pinned_id.clone();
        }
        if let (Some(mic), Some(pinned_mic)) = (&mut audio.microphone, &pinned.microphone) {
            mic.device_id = pinned_mic.device_id.clone();
        }
    }
    let output =
        fs::canonicalize(&config.output_directory).map_err(|e| format!("Recording folder: {e}"))?;
    if !output.is_dir() {
        return Err("Choose a recording output folder.".into());
    }
    regular_ancestors(&output)?;
    ensure_local_space(&output, RESERVE_BYTES)?;
    config.output_directory = output.to_string_lossy().into_owned();
    let source = manager.preview.native_source(&config.source)?;
    let resolved = helper_call(
        app,
        json!({"operation":"resolve-source","source":source}),
        cancel.clone(),
        Some(Duration::from_secs(10)),
        |_| {},
    )?;
    let geometry: Geometry = serde_json::from_value(resolved["geometry"].clone()).map_err(error)?;
    geometry.validate()?;
    if cancel.load(Ordering::Acquire) {
        return Err("Recording preparation was canceled.".into());
    }
    Ok(config)
}

struct ControlsBackend {
    app: AppHandle,
}
impl controls_hotkeys::ControlsBackend for ControlsBackend {
    fn is_idle(&self) -> bool {
        !self.app.state::<RecordingManager>().needs_close_guard()
    }
    fn validate_draft(&self, config: RecordingConfig) -> Result<RecordingConfig, String> {
        validate_start_draft(
            &self.app,
            self.app.state::<RecordingManager>().inner(),
            config,
        )
    }
    fn replace_owner(&self, owner: Option<String>) {
        let manager = self.app.state::<RecordingManager>();
        manager.controls.replace_owner_option(owner.clone());
        manager.audio.set_context(manager.controls.context());
        let context = manager.controls.context();
        if let Some(session) = context.session_id {
            manager.update(&self.app, &session, |s| {
                s.control_revision = context.control_revision
            });
        }
    }
    fn dispatch(&self, intent: controls_hotkeys::ControlIntent) -> Result<(), String> {
        let app = self.app.clone();
        let manager = app.state::<RecordingManager>().inner().clone();
        match intent {
            controls_hotkeys::ControlIntent::Start(attempt) => {
                let activity = app.state::<ActivityCoordinator>().inner().clone();
                tauri::async_runtime::spawn(async move {
                    let token = attempt.token;
                    let service = app
                        .state::<controls_hotkeys::ControlsManager>()
                        .inner()
                        .clone();
                    let checked = {
                        let app = app.clone();
                        let manager = manager.clone();
                        let cancel = attempt.cancel.clone();
                        let config = attempt.config;
                        tauri::async_runtime::spawn_blocking(move || {
                            validate_start_draft_cancel(&app, &manager, config, cancel)
                        })
                        .await
                        .map_err(error)
                        .and_then(|r| r)
                    };
                    let config = match checked {
                        Ok(config) => config,
                        Err(reason) => {
                            service.fail_attempt(&token, &reason);
                            return;
                        }
                    };
                    if let Err(reason) = start_internal(
                        app.clone(),
                        manager,
                        activity,
                        config,
                        StartAdmission {
                            permit: attempt.permit,
                            cancel: attempt.cancel,
                            attempt: Some(token.clone()),
                        },
                    )
                    .await
                    {
                        eprintln!("Recording shortcut Start: {reason}");
                        service.fail_attempt(&token, &reason);
                    }
                });
            }
            controls_hotkeys::ControlIntent::Pause {
                owner_token,
                operation_id,
                session_id,
                control_revision,
            } => {
                let request = session_controls::TransitionRequest {
                    owner_token,
                    operation_id,
                    session_id,
                    expected_control_revision: control_revision,
                    reconcile_fence_token: None,
                };
                if let audio_controls::ControlResult::Err { failure, .. } =
                    request_transition(&app, &manager, request, "pause")
                {
                    return Err(failure.error.message);
                }
            }
            controls_hotkeys::ControlIntent::Resume {
                owner_token,
                operation_id,
                session_id,
                control_revision,
            } => {
                let request = session_controls::TransitionRequest {
                    owner_token,
                    operation_id,
                    session_id,
                    expected_control_revision: control_revision,
                    reconcile_fence_token: None,
                };
                if let audio_controls::ControlResult::Err { failure, .. } =
                    request_transition(&app, &manager, request, "resume")
                {
                    return Err(failure.error.message);
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn controls_backend(app: AppHandle) -> Arc<dyn controls_hotkeys::ControlsBackend> {
    Arc::new(ControlsBackend { app })
}

fn main_window(window: &Window) -> Result<(), String> {
    let url = window.url().map_err(error)?;
    if window.label() == "main" && trusted_origin(&url) {
        Ok(())
    } else {
        Err("Recording is available from the main window only.".into())
    }
}
fn trusted_origin(url: &tauri::Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str(), url.port()),
        ("tauri", Some("localhost"), None) | ("http", Some("tauri.localhost"), None)
    ) || cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str() == Some("localhost")
        && url.port() == Some(1422)
}
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn helper_path(app: &AppHandle) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err("Recording is coming to Linux after Windows.".into());
    }
    let name = "recording-sidecar/vfl_recording_host.exe";
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join(name));
    }
    if let Ok(resources) = app.path().resource_dir() {
        paths.push(resources.join(name));
    }
    paths.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "The bundled recording helper is missing. Restore the complete app folder.".into()
    })
}

/// A bounded protocol and a watchdog sit outside the native media call stack.
fn helper_call(
    app: &AppHandle,
    request: Value,
    stop: Arc<AtomicBool>,
    timeout: Option<Duration>,
    progress: impl FnMut(&Value),
) -> Result<Value, String> {
    helper_call_stream(app, request, stop, timeout, progress, HostTaps::default())
}
#[derive(Default)]
struct HostTaps {
    preview: Option<preview_manager::Tap>,
    audio: Option<audio_manager::Tap>,
    session: Option<RecordingManager>,
}
fn control_line(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value).map_err(error)?;
    if bytes.len() >= audio_types::CONTROL_BYTES {
        return Err("Recording control exceeded its 512-byte wire limit.".into());
    }
    bytes.push(b'\n');
    Ok(bytes)
}
fn helper_call_stream(
    app: &AppHandle,
    mut request: Value,
    stop: Arc<AtomicBool>,
    timeout: Option<Duration>,
    mut progress: impl FnMut(&Value),
    taps: HostTaps,
) -> Result<Value, String> {
    let HostTaps {
        preview: tap,
        audio,
        session,
    } = taps;
    if stop.load(Ordering::Acquire) {
        return Err("Recording preparation was canceled.".into());
    }
    let path = helper_path(app)?;
    regular_ancestors(&path)?;
    let command = Command::new(&path);
    let mut child = process::ManagedChild::spawn_with_stdin(command).map_err(|e| {
        format!(
            "Cannot start Windows recording components: {e}. Windows N may need Media Feature Pack."
        )
    })?;
    if stop.load(Ordering::Acquire) {
        return Err("Recording preparation was canceled before its first request.".into());
    }
    let mut input = child
        .stdin
        .take()
        .ok_or("Recording control pipe is unavailable.")?;
    request["protocolVersion"] = json!(4);
    serde_json::to_writer(&mut input, &request).map_err(error)?;
    input
        .write_all(b"\n")
        .and_then(|_| input.flush())
        .map_err(error)?;
    let (control_send, control_receive) =
        mpsc::sync_channel::<Value>(audio_types::CONTROL_CAPACITY + 1);
    let input_stop = stop.clone();
    let control_error = Arc::new(Mutex::new(None::<String>));
    let writer_error = control_error.clone();
    let transition_writer = session.as_ref().map(|m| m.controls.clone());
    let input_writer = std::thread::spawn(move || {
        loop {
            if input_stop.load(Ordering::Acquire) {
                let _ = input.write_all(b"stop\n").and_then(|_| input.flush());
                break;
            }
            match control_receive.recv_timeout(Duration::from_millis(50)) {
                Ok(value) => {
                    let result = (|| -> Result<Vec<u8>, String> {
                        if matches!(value["operation"].as_str(), Some("pause" | "resume")) {
                            let operation = value["value"]["operationId"].as_str().unwrap_or("");
                            if transition_writer
                                .as_ref()
                                .is_none_or(|c| !c.write_permitted(operation))
                            {
                                return Err(
                                    "The recording transition expired before its private write."
                                        .into(),
                                );
                            }
                        }
                        control_line(&value)
                    })()
                    .and_then(|line| {
                        input
                            .write_all(&line)
                            .and_then(|_| input.flush())
                            .map_err(error)
                    });
                    if let Err(error) = result {
                        *writer_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
                        input_stop.store(true, Ordering::Release);
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    let output = child
        .stdout
        .take()
        .ok_or("Recording status pipe is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Recording error pipe is unavailable.")?;
    let stream_tap = tap.clone();
    let stream_stop = stop.clone();
    let preview_error = Arc::new(Mutex::new(None::<String>));
    let read_error = preview_error.clone();
    let errors = std::thread::spawn(move || {
        let mut reader = stderr;
        if let Some(tap) = stream_tap {
            loop {
                match preview::read_frame(&mut reader) {
                    Ok(Some(frame)) => tap.accept(frame),
                    Ok(None) => break,
                    Err(error) => {
                        tap.fail(&error);
                        *read_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
                        stream_stop.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        } else {
            let mut tail = [0_u8; 4096];
            while let Ok(n) = reader.read(&mut tail) {
                if n == 0 {
                    break;
                }
            }
        }
    });
    let (send, receive) = mpsc::sync_channel(16);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(output);
        loop {
            let mut line = Vec::new();
            let read = reader
                .by_ref()
                .take(PROTOCOL_LIMIT + 1)
                .read_until(b'\n', &mut line);
            match read {
                Ok(0) => break,
                Ok(_) if line.len() <= PROTOCOL_LIMIT as usize && line.ends_with(b"\n") => {
                    if send
                        .send(serde_json::from_slice::<Value>(&line).map_err(error))
                        .is_err()
                    {
                        break;
                    }
                }
                _ => {
                    let _ = send.send(Err("Invalid recording helper response.".into()));
                    break;
                }
            }
        }
    });
    let started = Instant::now();
    let mut last_progress = started;
    let mut stopping = None;
    let recording = request["operation"] == "record" || request["operation"] == "preview";
    let mut last_preview_control = Instant::now() - Duration::from_secs(1);
    let mut last_audio_control = Instant::now() - Duration::from_secs(1);
    let mut ready = false;
    let mut watchdog_interrupted = false;
    let result = loop {
        if !stop.load(Ordering::Acquire)
            && last_preview_control.elapsed() >= Duration::from_millis(400)
        {
            last_preview_control = Instant::now();
            if let Some(tap) = &tap {
                let _ = control_send.try_send(tap.control());
            }
        }
        if !stop.load(Ordering::Acquire)
            && last_audio_control.elapsed() >= Duration::from_millis(40)
        {
            last_audio_control = Instant::now();
            if let Some(audio) = &audio {
                audio.control(&control_send, &stop);
            }
            if let Some(manager) = &session {
                let context = manager.controls.context();
                manager.audio.set_context(context.clone());
                let snapshot = manager.snapshot();
                let fence_ready = context.resolved_role_mask == 0
                    || snapshot
                        .pause
                        .as_ref()
                        .and_then(|p| p.audio.as_ref())
                        .and_then(|a| a.fence.token.as_deref())
                        .is_some_and(|token| {
                            manager.audio.fence_ready(
                                context.session_id.as_deref().unwrap_or(""),
                                context.pause_revision,
                                token,
                            )
                        });
                if let Some(value) = manager
                    .controls
                    .control(fence_ready, manager.audio.accepted_sequence())
                {
                    let operation = value["value"]["operationId"]
                        .as_str()
                        .unwrap_or("")
                        .to_string();
                    if control_send.try_send(value).is_err() {
                        manager.controls.unsent(&operation);
                    }
                }
                if let Some(reason) = manager.controls.timed_out() {
                    progress(&json!({"transitionError":reason}));
                }
            }
        }
        if stop.load(Ordering::Acquire) && stopping.is_none() {
            stopping = Some(Instant::now());
        }
        if stopping.is_some_and(|at: Instant| at.elapsed() > Duration::from_secs(20)) {
            break Err("Recording did not stop in time. Its recovery files have been kept.".into());
        }
        if timeout.is_some_and(|limit| started.elapsed() > limit) {
            break Err("Windows recording components did not respond in time.".into());
        }
        if recording
            && stopping.is_none()
            && ((!ready && started.elapsed() > Duration::from_secs(15))
                || (ready && last_progress.elapsed() > Duration::from_secs(10)))
        {
            watchdog_interrupted = true;
            stop.store(true, Ordering::Release);
            // Give the host its normal stop grace; a stalled driver can then be killed.
        }
        match receive.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(message)) => {
                if message["protocolVersion"] != 4 {
                    break Err("Recording component version mismatch.".into());
                }
                match message["type"].as_str() {
                    Some("progress") => {
                        if let Some(audio) = &audio
                            && let Err(e) = audio.progress(&message["value"])
                        {
                            progress(&json!({"interruption":e}));
                            stop.store(true, Ordering::Release);
                        }
                        if let Some(tap) = &tap {
                            tap.progress(&message["value"]);
                        }
                        ready |= message["value"]["ready"].as_bool().unwrap_or(false);
                        last_progress = Instant::now();
                        progress(&message["value"]);
                    }
                    Some("result") => {
                        if let Some(audio) = &audio
                            && let Err(e) = audio.progress(&message["value"])
                        {
                            break Err(e);
                        }
                        if watchdog_interrupted {
                            progress(
                                &json!({"interruption":"Recording stopped because Windows stopped responding. The saved clip may be shorter than expected."}),
                            );
                        }
                        break Ok(message["value"].clone());
                    }
                    Some("error") => {
                        if let Some(audio) = &audio
                            && let Err(e) = audio.progress(&message)
                        {
                            break Err(e);
                        }
                        if message["noMedia"] == true {
                            progress(&json!({"noMedia":true}));
                        }
                        break Err(message["message"]
                            .as_str()
                            .unwrap_or("Windows recording failed.")
                            .to_string());
                    }
                    _ => break Err("Unknown recording helper response.".into()),
                }
            }
            Ok(Err(e)) => break Err(e),
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(
                "Windows recording components exited unexpectedly. Recovery files have been kept."
                    .into(),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    if let Some(reason) = control_error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        progress(&json!({"interruption":reason}));
    }
    if let Some(reason) = preview_error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        progress(&json!({"interruption":reason}));
    }
    drop(control_send);
    if result.is_err() {
        let _ = child.control.kill();
    }
    let exited = child.wait_timeout(Duration::from_secs(3)).map_err(error)?;
    if exited.is_none() {
        let _ = child.control.kill();
    }
    drop(child);
    // ManagedChild's job kills every inherited writer before these joins.
    drop(receive);
    let _ = reader.join();
    let _ = errors.join();
    let _ = input_writer.join();
    if result.is_ok() && !exited.is_some_and(|status| status.success()) {
        return Err("Recording helper did not exit successfully.".into());
    }
    result
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capabilities {
    available: bool,
    reason: Option<String>,
    default_output_directory: Option<String>,
    audio: Value,
}
#[tauri::command]
pub(crate) async fn recording_capabilities(
    window: Window,
    app: AppHandle,
) -> Result<Capabilities, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        let default = app
            .path()
            .video_dir()
            .or_else(|_| app.path().document_dir())
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let check = helper_call(
            &app,
            json!({"operation":"sources"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        );
        let application=helper_call(&app,json!({"operation":"audio-capabilities"}),Arc::new(AtomicBool::new(false)),Some(Duration::from_secs(10)),|_|{}).map(|v|v["application"].clone()).unwrap_or_else(|reason|json!({"available":false,"reason":reason}));
        let suppression=video::require_recording_audio().err();
        Ok(Capabilities {
            available: check.is_ok(),
            reason: check.err(),
            default_output_directory: default,
            audio:json!({"application":application,"microphoneSuppression":{"available":suppression.is_none(),"reason":suppression}}),
        })
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_sources(window: Window, app: AppHandle) -> Result<Value, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        helper_call(
            &app,
            json!({"operation":"sources"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_audio_devices(
    window: Window,
    app: AppHandle,
) -> Result<Value, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        helper_call(
            &app,
            json!({"operation":"audio-devices"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_select_region(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    display_id: String,
    request_id: String,
) -> Result<Value, String> {
    main_window(&window)?;
    if request_id.is_empty() || request_id.len() > 100 {
        return Err("Invalid selection request.".into());
    }
    let activity = activity.inner().clone();
    let manager = state.inner().clone();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        if active(&i.snapshot.phase) || i.selecting.is_some() {
            return Err("A recording operation is already active.".into());
        }
        i.selecting = Some((request_id.clone(), stop.clone()));
        i.snapshot.selection_request_id = Some(request_id.clone());
        i.snapshot.sequence += 1;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let _lease = match manager.preview.stop_wait(true)? {
                Some(lease) => lease,
                None => activity.begin("screen selection")?,
            };
            if stop.load(Ordering::Acquire) {
                return Err("Area selection was cancelled.".into());
            }
            helper_call(
                &app,
                json!({"operation":"select-region","displayId":display_id}),
                stop,
                Some(Duration::from_secs(180)),
                |_| {},
            )
        })();
        let mut i = manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.selecting
            .as_ref()
            .is_some_and(|(id, _)| id == &request_id)
        {
            i.selecting = None;
            i.snapshot.selection_request_id = None;
            i.snapshot.sequence += 1;
        }
        result
    })
    .await
    .map_err(error)?
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    version: u32,
    session_id: String,
    created_at: String,
    directory: PathBuf,
    destination: PathBuf,
    config: RecordingConfig,
    #[serde(default)]
    resolved_geometry: Option<Geometry>,
    #[serde(default)]
    published_size: Option<u64>,
    #[serde(default)]
    published_hash: Option<String>,
    #[serde(default)]
    candidate_directory: Option<PathBuf>,
    #[serde(default)]
    audio_capture: Option<audio_finalize::AudioJournal>,
}

fn registry(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(error)?
        .join("recording-recovery");
    fs::create_dir_all(&root).map_err(error)?;
    regular_ancestors(&root)?;
    Ok(root)
}
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() < 80 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn regular_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        let m = fs::symlink_metadata(ancestor).map_err(error)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return Err("Recording paths must not use redirected folders.".into());
            }
        }
        if m.file_type().is_symlink() {
            return Err("Recording paths must not use symbolic links.".into());
        }
    }
    Ok(())
}
fn write_journal(app: &AppHandle, journal: &Journal) -> Result<(), String> {
    let root = registry(app)?;
    let mut file = tempfile::NamedTempFile::new_in(&root).map_err(error)?;
    let bytes = serde_json::to_vec(journal).map_err(error)?;
    if bytes.len() > PROTOCOL_LIMIT as usize {
        return Err("Recording recovery metadata exceeds its byte limit.".into());
    }
    file.write_all(&bytes).map_err(error)?;
    file.as_file().sync_all().map_err(error)?;
    file.persist(root.join(format!("{}.json", journal.session_id)))
        .map_err(error)?;
    Ok(())
}
fn read_journal(app: &AppHandle, id: &str) -> Result<Journal, String> {
    if !safe_id(id) {
        return Err("Invalid recovery session.".into());
    }
    let path = registry(app)?.join(format!("{id}.json"));
    regular_ancestors(&path)?;
    let metadata = fs::metadata(&path).map_err(error)?;
    if !metadata.is_file() || metadata.len() > PROTOCOL_LIMIT {
        return Err("Invalid recovery journal.".into());
    }
    let j: Journal =
        serde_json::from_reader(fs::File::open(path).map_err(error)?).map_err(error)?;
    validate_journal_audio(&j)?;
    if j.session_id != id
        || j.directory.file_name().and_then(|s| s.to_str()) != Some(&format!(".vfl-recording-{id}"))
    {
        return Err("Invalid recovery journal ownership.".into());
    }
    regular_ancestors(&j.directory)?;
    let marker = j.directory.join("owner.json");
    regular_ancestors(&marker)?;
    let metadata = fs::metadata(&marker).map_err(error)?;
    if !metadata.is_file() || metadata.len() > 256 {
        return Err("Invalid recording directory ownership.".into());
    }
    let owner: Value =
        serde_json::from_reader(fs::File::open(marker).map_err(error)?).map_err(error)?;
    if owner != json!({"version":j.version,"sessionId":id}) {
        return Err("Recording directory ownership differs.".into());
    }
    let source_root = fs::canonicalize(&j.config.output_directory).map_err(error)?;
    if j.directory.parent() != Some(source_root.as_path())
        || j.destination.file_name().and_then(|s| s.to_str())
            != Some(&format!("Recording-{id}.mp4"))
    {
        return Err("Invalid recovery destination.".into());
    }
    regular_ancestors(
        j.destination
            .parent()
            .ok_or("Invalid recovery destination.")?,
    )?;
    if let Some(stage) = &j.candidate_directory {
        if stage.file_name().and_then(|s| s.to_str())
            != Some(&format!(".vfl-recording-{id}-recovery"))
        {
            return Err("Invalid recovery staging directory.".into());
        }
        validate_marker(stage, id, j.version)?;
    }
    Ok(j)
}
fn validate_journal_audio(j: &Journal) -> Result<(), String> {
    match j.version {
        1 if j.config.audio.is_none() && j.audio_capture.is_none() => Ok(()),
        2 if j.config.audio.is_some()
            && j.config.system_audio_device_id.is_none()
            && j.config.microphone_device_id.is_none() =>
        {
            let config = j.config.audio_config();
            config.validate()?;
            match (&j.audio_capture, config.enabled()) {
                (Some(audio), true) => audio.validate(&config),
                (None, false) => Ok(()),
                _ => Err("Invalid recording audio ownership.".into()),
            }
        }
        _ => Err("The recording audio recovery version is unsupported or inconsistent.".into()),
    }
}
fn validate_marker(directory: &Path, id: &str, version: u32) -> Result<(), String> {
    regular_ancestors(directory)?;
    let marker = directory.join("owner.json");
    regular_ancestors(&marker)?;
    let meta = fs::metadata(&marker).map_err(error)?;
    if !meta.is_file() || meta.len() > 256 {
        return Err("Invalid recording ownership marker.".into());
    }
    let owner: Value =
        serde_json::from_reader(fs::File::open(marker).map_err(error)?).map_err(error)?;
    if owner != json!({"version":version,"sessionId":id}) {
        return Err("Recording ownership marker differs.".into());
    }
    Ok(())
}
fn find_recoveries(app: &AppHandle) -> Result<Vec<Recovery>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(registry(app)?).map_err(error)?.take(1000) {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(j) = read_journal(app, id) else {
            continue;
        };
        let Ok(_session_lock) = lock_session(&j) else {
            continue;
        };
        if publication_matches(&j) {
            let _ = cleanup(app, &j);
            continue;
        }
        found.push(Recovery {
            session_id: j.session_id,
            created_at: j.created_at,
            message: "An interrupted recording is available to recover.".into(),
        });
    }
    Ok(found)
}
fn validate_config(config: &RecordingConfig) -> Result<(), String> {
    if config.request_id.is_empty() || config.request_id.len() > 100 {
        return Err("Invalid recording request.".into());
    }
    if ![24, 25, 30, 50, 60].contains(&config.fps)
        || ![0, 3, 5].contains(&config.countdown_seconds)
        || !matches!(config.quality.as_str(), "standard" | "high")
    {
        return Err("Unsupported recording settings.".into());
    }
    config.source.validate()?;
    config.audio_config().validate()?;
    if config.output_directory.is_empty() {
        return Err("Choose an output folder.".into());
    }
    Ok(())
}
fn create_journal(
    app: &AppHandle,
    id: &str,
    config: RecordingConfig,
    geometry: Geometry,
) -> Result<(Journal, fs::File), String> {
    validate_config(&config)?;
    let destination_root = fs::canonicalize(&config.output_directory)
        .map_err(|e| format!("Cannot open output folder: {e}"))?;
    regular_ancestors(&destination_root)?;
    if !destination_root.is_dir() {
        return Err("Choose an output folder.".into());
    }
    ensure_local_space(&destination_root, RESERVE_BYTES * 2)?;
    let directory = destination_root.join(format!(".vfl-recording-{id}"));
    fs::create_dir(&directory).map_err(error)?;
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("owner.json"))
        .map_err(error)?;
    serde_json::to_writer(
        &mut marker,
        &json!({"version":JOURNAL_VERSION,"sessionId":id}),
    )
    .map_err(error)?;
    marker.sync_all().map_err(error)?;
    let session_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.join("session.lock"))
        .map_err(error)?;
    session_lock.try_lock().map_err(error)?;
    let destination = destination_root.join(format!("Recording-{id}.mp4"));
    let audio_capture = audio_finalize::AudioJournal::new(&config.audio_config());
    let j = Journal {
        version: JOURNAL_VERSION,
        session_id: id.into(),
        created_at: format!(
            "{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(error)?
                .as_secs()
        ),
        directory,
        destination,
        config,
        resolved_geometry: Some(geometry),
        published_size: None,
        published_hash: None,
        candidate_directory: None,
        audio_capture,
    };
    if let Err(e) = write_journal(app, &j) {
        let _ = fs::remove_dir(&j.directory);
        return Err(e);
    }
    Ok((j, session_lock))
}
fn lock_session(j: &Journal) -> Result<fs::File, String> {
    let path = j.directory.join("session.lock");
    regular_ancestors(&path)?;
    if !fs::metadata(&path).map_err(error)?.is_file() {
        return Err("Invalid recording session lock.".into());
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(error)?;
    lock.try_lock()
        .map_err(|_| "This recording is still in use by another app window.".to_string())?;
    Ok(lock)
}

#[cfg(windows)]
fn ensure_local_space(path: &Path, minimum: u64) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
        fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
        fn GetDriveTypeW(path: *const u16) -> u32;
        fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            filesystem: *mut u16,
            filesystem_len: u32,
        ) -> i32;
    }
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = [0_u16; 32768];
    let mut available = 0;
    unsafe {
        if GetVolumePathNameW(text.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) == 0
            || GetDriveTypeW(volume.as_ptr()) != 3
        {
            return Err("Choose a local fixed drive for recordings.".into());
        }
        let mut filesystem = [0_u16; 32];
        if GetVolumeInformationW(
            volume.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            32,
        ) == 0
        {
            return Err("Cannot identify the recording filesystem.".into());
        }
        let filesystem = String::from_utf16_lossy(
            &filesystem[..filesystem.iter().position(|c| *c == 0).unwrap_or(32)],
        );
        if filesystem.eq_ignore_ascii_case("FAT32") || filesystem.eq_ignore_ascii_case("FAT") {
            return Err(
                "Choose an NTFS, ReFS or exFAT folder for recordings. FAT32 has a 4 GB file limit."
                    .into(),
            );
        }
        if GetDiskFreeSpaceExW(
            text.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err("Cannot check recording disk space.".into());
        }
    }
    if available < minimum {
        return Err(
            "There is not enough free space to safely save this recording. Choose another folder."
                .into(),
        );
    }
    Ok(())
}
#[cfg(not(windows))]
fn ensure_local_space(_path: &Path, _minimum: u64) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub(crate) async fn recording_snapshot(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.load(&app)?;
        Ok(manager.snapshot())
    })
    .await
    .map_err(error)?
}

fn transition_failure(
    manager: &RecordingManager,
    code: &str,
) -> audio_controls::ControlResult<Snapshot> {
    use audio_controls::*;
    let code: FailureCode =
        serde_json::from_value(json!(code)).unwrap_or(FailureCode::ProtocolFailure);
    let (retry, action, message) = code.policy();
    let snapshot = manager.snapshot();
    let mut pause = snapshot.pause.clone();
    if code == FailureCode::StaleOwner
        && let Some(audio) = pause.as_mut().and_then(|p| p.audio.as_mut())
    {
        audio.fence.token = None;
        audio.fence.replacement_review_token = None;
    }
    ControlResult::failure(ControlFailure {
        error: FailureDetail {
            code,
            retry,
            action,
            command_sequence: None,
            message: message.into(),
        },
        context: FailureContext {
            session_id: snapshot.session_id.clone(),
            source_generation: snapshot.audio.as_ref().map(|a| a.source_generation.clone()),
            control_revision: Some(snapshot.control_revision),
            pause_revision: snapshot.pause.as_ref().map(|p| p.pause_revision),
            timed_out_attempt: None,
        },
        state: FailureState {
            phase: snapshot.phase,
            audio_status: snapshot.audio,
            ledger: snapshot.audio_control_ledger,
            pause,
        },
    })
}
fn publish_transition_timeout(
    app: &AppHandle,
    manager: &RecordingManager,
    timeout: &session_controls::TransitionTimeout,
) {
    manager.update(app, &timeout.session_id, |s| {
        s.apply_transition_timeout(timeout);
    });
}
fn wait_transition(
    app: AppHandle,
    manager: RecordingManager,
    request: session_controls::TransitionRequest,
    action: &str,
) -> audio_controls::ControlResult<Snapshot> {
    let initial = request_transition(&app, &manager, request.clone(), action);
    if matches!(initial, audio_controls::ControlResult::Err { .. }) {
        return initial;
    }
    loop {
        let context = manager.controls.context();
        if context.owner_token.as_deref() != Some(&request.owner_token) {
            return transition_failure(&manager, "staleOwner");
        }
        if context.session_id.as_deref() != Some(&request.session_id) {
            return transition_failure(&manager, "staleSession");
        }
        if let Some(code) = manager.controls.timed_out() {
            publish_transition_timeout(&app, &manager, &code);
        }
        match manager.controls.outcome(&request.operation_id) {
            Ok(Some(())) => {
                let snapshot = manager.snapshot();
                if snapshot
                    .pending_transition
                    .as_ref()
                    .is_none_or(|p| p.operation_id != request.operation_id)
                {
                    return audio_controls::ControlResult::success(snapshot)
                        .unwrap_or_else(|_| transition_failure(&manager, "oversizedRequest"));
                }
            }
            Err(code) => return transition_failure(&manager, &code),
            Ok(None) => {}
        }
        if context
            .stop
            .as_ref()
            .is_none_or(|s| s.load(Ordering::Acquire))
        {
            return transition_failure(&manager, "sessionTerminal");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn admit_transition(
    manager: &RecordingManager,
    request: session_controls::TransitionRequest,
    action: &str,
) -> Result<session_controls::PendingTransition, String> {
    if let Err(reason) = audio_controls::checked_bytes(&request, 512) {
        return Err(if reason == audio_controls::FailureCode::OversizedRequest {
            "oversizedRequest"
        } else {
            "invalidRequest"
        }
        .into());
    }
    // Known exact identities join their original outcome before checking a
    // fence that successful Resume has already consumed.
    if manager.controls.known_request(&request, action)? {
        return Ok(session_controls::PendingTransition {
            operation_id: request.operation_id,
            action: action.into(),
            control_revision: request.expected_control_revision + 1,
        });
    }
    let context = manager.controls.context();
    if context.owner_token.as_deref() != Some(&request.owner_token) {
        return Err("staleOwner".into());
    }
    if action == "resume"
        && let Some(token) = &request.reconcile_fence_token
        && (context.resolved_role_mask == 0
            || !manager
                .audio
                .fence_ready(&request.session_id, context.pause_revision, token))
    {
        return Err("staleAdmission".into());
    }

    if action == "resume"
        && manager
            .snapshot()
            .pause
            .as_ref()
            .and_then(|p| p.audio.as_ref())
            .is_some_and(|a| a.fence.state == audio_controls::FencePhase::ReplacementReviewRequired)
    {
        return Err("replacementReviewRequired".into());
    }
    if action == "pause" && request.reconcile_fence_token.is_some() {
        return Err("invalidRequest".into());
    }
    manager.controls.admit(request, action)
}
pub(crate) fn request_transition(
    app: &AppHandle,
    manager: &RecordingManager,
    request: session_controls::TransitionRequest,
    action: &str,
) -> audio_controls::ControlResult<Snapshot> {
    let session = request.session_id.clone();
    match admit_transition(manager, request, action) {
        Ok(pending) => {
            if !manager.controls.is_pending(&pending.operation_id) {
                return audio_controls::ControlResult::success(manager.snapshot())
                    .unwrap_or_else(|_| transition_failure(manager, "oversizedRequest"));
            }
            let context = manager.controls.context();
            if action == "pause" {
                manager.preview.pause_recording_delivery(app, &session);
            }
            manager.audio.set_context(context.clone());
            manager.update(app, &session, |s| {
                s.control_revision = context.control_revision;
                s.phase = context.phase;
                s.pending_transition = Some(pending);
                if action == "pause" {
                    s.message = Some("Pausing and releasing recording inputs…".into());
                }
            });
            audio_controls::ControlResult::success(manager.snapshot())
                .unwrap_or_else(|_| transition_failure(manager, "oversizedRequest"))
        }
        Err(reason) => transition_failure(manager, &reason),
    }
}

#[tauri::command]
pub(crate) async fn recording_pause(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    request: session_controls::TransitionRequest,
) -> Result<audio_controls::ControlResult<Snapshot>, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || wait_transition(app, manager, request, "pause"))
        .await
        .map_err(error)
}
#[tauri::command]
pub(crate) async fn recording_resume(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    request: session_controls::TransitionRequest,
) -> Result<audio_controls::ControlResult<Snapshot>, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || wait_transition(app, manager, request, "resume"))
        .await
        .map_err(error)
}

#[tauri::command]
pub(crate) async fn recording_start(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    config: RecordingConfig,
    admission: controls_hotkeys::UiStartContext,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    start_owned(
        app,
        state.inner().clone(),
        activity.inner().clone(),
        config,
        admission,
    )
    .await
}

pub(crate) async fn start_owned(
    app: AppHandle,
    manager: RecordingManager,
    activity: ActivityCoordinator,
    config: RecordingConfig,
    admission: controls_hotkeys::UiStartContext,
) -> Result<Snapshot, String> {
    let service = app
        .state::<controls_hotkeys::ControlsManager>()
        .inner()
        .clone();
    let permit = service.ui_start_permit(admission)?;
    let token = permit.token().to_string();
    let cancel = permit.cancel();
    let result = start_internal(
        app,
        manager,
        activity,
        config,
        StartAdmission {
            permit,
            cancel,
            attempt: Some(token.clone()),
        },
    )
    .await;
    if result.is_err() {
        service.finish_attempt(&token);
    }
    result
}
struct StartAdmission {
    permit: controls_hotkeys::StartPermit,
    cancel: Arc<AtomicBool>,
    attempt: Option<String>,
}
async fn start_internal(
    app: AppHandle,
    manager: RecordingManager,
    activity: ActivityCoordinator,
    mut config: RecordingConfig,
    admission: StartAdmission,
) -> Result<Snapshot, String> {
    config.audio = Some(config.audio_config());
    config.system_audio_device_id = None;
    config.microphone_device_id = None;
    validate_config(&config)?;
    helper_path(&app)?;
    {
        let i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        if i.request_id.as_deref() == Some(&config.request_id) {
            return Ok(i.snapshot.clone());
        }
    }
    let (id, stop, snapshot) = admission.permit.admit(|| {
        let mut i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        if active(&i.snapshot.phase) || i.selecting.is_some() {
            return Err("A recording operation is already active.".into());
        }

        let id = format!(
            "{:x}-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(error)?
                .as_nanos(),
            std::process::id()
        );
        let stop = admission.cancel.clone();
        let audio = config.audio_config();
        let roles = u8::from(!matches!(audio.render, RenderAudio::Off))
            | (u8::from(audio.microphone.is_some()) << 1);
        manager.controls.set_session(&id, stop.clone(), roles)?;
        i.snapshot = Snapshot {
            session_id: Some(id.clone()),
            sequence: i.snapshot.sequence + 1,
            phase: "preparing".into(),
            recoveries: i.snapshot.recoveries.clone(),
            config: Some(config.clone()),
            ..Snapshot::default()
        };
        i.stop = Some(stop.clone());
        i.request_id = Some(config.request_id.clone());
        i.start_attempt = admission.attempt.clone();
        i.snapshot.control_revision = manager.controls.context().control_revision;
        i.snapshot.resolved_role_mask = roles;
        let snapshot = i.snapshot.clone();
        Ok((id, stop, snapshot))
    })?;
    manager.audio.set_context(manager.controls.context());
    manager.publish_control_target(&app);
    let checked = {
        let app = app.clone();
        let manager = manager.clone();
        let config = config.clone();
        let cancel = stop.clone();
        tauri::async_runtime::spawn_blocking(move || {
            validate_start_draft_cancel(&app, &manager, config, cancel)
        })
        .await
        .map_err(error)
        .and_then(|r| r)
    };
    config = match checked {
        Ok(config) => config,
        Err(reason) => {
            manager.update(&app, &id, |s| {
                s.phase = if stop.load(Ordering::Acquire) {
                    "idle"
                } else {
                    "failed"
                }
                .into();
                s.message = Some(reason.clone());
            });
            return Err(reason);
        }
    };
    manager.update(&app, &id, |s| s.config = Some(config.clone()));
    let preview = manager.preview.clone();
    let transfer = tauri::async_runtime::spawn_blocking(move || preview.stop_wait(true))
        .await
        .map_err(error)?;
    let lease = match transfer.and_then(|lease| match lease {
        Some(lease) => Ok(lease),
        None => activity.begin("recording"),
    }) {
        Ok(lease) => lease,
        Err(error) => {
            manager.update(&app, &id, |s| {
                s.phase = "failed".into();
                s.message = Some(error.clone());
            });
            return Err(error);
        }
    };
    manager.preview.recording_owner(&id);
    let failed_manager = manager.clone();
    let failed_app = app.clone();
    let failed_id = id.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("recording-session".into())
        .spawn(move || run_session(app, manager, id, config, stop, lease))
    {
        failed_manager
            .preview
            .finish_recording(&failed_app, &failed_id);
        failed_manager.update(&failed_app, &failed_id, |s| {
            s.phase = "failed".into();
            s.message = Some(format!("Cannot start recorder: {e}"));
        });
        return Err(format!("Cannot start recorder: {e}"));
    }
    Ok(snapshot)
}

fn run_session(
    app: AppHandle,
    manager: RecordingManager,
    id: String,
    config: RecordingConfig,
    stop: Arc<AtomicBool>,
    lease: ActivityLease,
) {
    let mut cancelled_before_capture = false;
    let result = (|| {
        let audio_config = config.audio_config();
        if audio_config.enabled() {
            video::require_recording_audio()?;
        }
        let native_audio = manager.audio.prepare(&id, &audio_config)?;
        manager.audio.set_context(manager.controls.context());
        let native = manager.preview.native_source(&config.source)?;
        let resolved = helper_call(
            &app,
            json!({"operation":"resolve-source","source":native}),
            stop.clone(),
            Some(Duration::from_secs(10)),
            |_| {},
        )?;
        let geometry: Geometry =
            serde_json::from_value(resolved["geometry"].clone()).map_err(error)?;
        geometry.validate()?;
        manager.update(&app, &id, |s| s.resolved_geometry = Some(geometry));
        let (mut journal, session_lock) = create_journal(&app, &id, config.clone(), geometry)?;
        let mut no_media = false;
        let result = (|| {
            let deadline = Instant::now() + Duration::from_secs(config.countdown_seconds as u64);
            while Instant::now() < deadline {
                if stop.load(Ordering::Acquire) {
                    return Err("Recording cancelled before capture started.".into());
                }
                let left = deadline.saturating_duration_since(Instant::now()).as_secs() + 1;
                manager.update(&app, &id, |s| {
                    s.phase = "countdown".into();
                    s.countdown_remaining = Some(left.min(config.countdown_seconds as u64) as u32);
                });
                std::thread::sleep(Duration::from_millis(100));
            }
            if stop.load(Ordering::Acquire) {
                return Err("Recording cancelled before capture started.".into());
            }
            manager.update(&app, &id, |s| {
                s.phase = "starting".into();
                s.countdown_remaining = None;
            });
            let spool = journal.directory.join("capture.mp4");
            let host = json!({"region":resolved["region"],"source":native,"fps":config.fps,"showCursor":config.show_cursor,"quality":config.quality,"audio":native_audio,"spoolPath":spool,"ffmpegPath":bundled_media(&app,"ffmpeg")?});
            let mut last_space = Instant::now();
            let mut space_error = None;
            let outcome = helper_call_stream(
                &app,
                json!({"operation":"record","config":host}),
                stop.clone(),
                None,
                |progress| {
                    if let Some(timeout) = progress.get("transitionError").and_then(|v| {
                        serde_json::from_value::<session_controls::TransitionTimeout>(v.clone())
                            .ok()
                    }) {
                        publish_transition_timeout(&app, &manager, &timeout);
                    }
                    no_media |= progress["noMedia"] == true;
                    if let Some(reason) = progress["interruption"].as_str() {
                        space_error = Some(reason.to_string());
                    }
                    if progress["stopHudVisible"] == true {
                        let attempt = manager
                            .inner
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .start_attempt
                            .clone();
                        if let Some(attempt) = attempt {
                            app.state::<controls_hotkeys::ControlsManager>()
                                .helper_hud_visible(&attempt, &id);
                        }
                    }
                    if let Some(action @ ("pause" | "resume")) = progress["hudIntent"].as_str() {
                        let context = manager.controls.context();
                        if let (Some(owner), Ok(operation)) =
                            (context.owner_token, session_controls::new_operation_id())
                        {
                            let _ = request_transition(
                                &app,
                                &manager,
                                session_controls::TransitionRequest {
                                    owner_token: owner,
                                    session_id: id.clone(),
                                    operation_id: operation,
                                    expected_control_revision: context.control_revision,
                                    reconcile_fence_token: None,
                                },
                                action,
                            );
                        }
                    }
                    let mut clock_error = None;
                    manager.update(&app, &id, |s| {
                        if let Some(epoch) = progress.get("clockEpoch")
                            && let Err(error) = s.retain_clock(&id, "initial", epoch.clone(), 0)
                        {
                            clock_error = Some(error);
                        }
                        if progress["ready"].as_bool().unwrap_or(false) && s.phase == "starting" {
                            s.phase = "recording".into();
                        }
                        s.audio = manager.audio.snapshot(Some(&id));
                        s.elapsed_ms = progress["elapsedMs"].as_u64().unwrap_or(s.elapsed_ms);
                        s.bytes_written =
                            progress["bytesWritten"].as_u64().unwrap_or(s.bytes_written);
                    });
                    if let Some(error) = clock_error {
                        space_error = Some(error);
                        stop.store(true, Ordering::Release);
                    }
                    let context = manager.controls.context();
                    if let Some(transition) = progress["transition"].as_object()
                        && context.session_id.as_deref() == Some(&id)
                        && transition.get("pauseRevision").and_then(Value::as_u64)
                            == Some(context.pause_revision)
                    {
                        let operation = transition
                            .get("operationId")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let phase = transition
                            .get("phase")
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        let current = manager.snapshot();
                        let exact_pending = current.matches_transition(
                            operation,
                            transition.get("controlRevision").and_then(Value::as_u64),
                        ) && manager.controls.is_pending(operation);
                        if exact_pending
                            && phase == "paused"
                            && transition.get("errorCode").and_then(Value::as_str)
                                == Some("sourceUnavailable")
                        {
                            if manager.controls.complete_result(
                                operation,
                                "paused",
                                Err("sourceUnavailable".into()),
                            ) {
                                manager.update(&app, &id, |s| {
                                    s.phase = "paused".into();
                                    s.pending_transition = None;
                                    s.message = Some("The pinned source is temporarily unavailable. Repair it before another Resume.".into());
                                });
                            }
                        } else if exact_pending
                            && phase == "paused"
                            && transition.get("inputsReleased") == Some(&json!(true))
                        {
                            // Validate the complete receipt before exposing a successful Pause.
                            // Tap.progress has already applied this FIFO event's audio status.
                            let sealed = (|| -> Result<Snapshot, String> {
                                let mut next = current.clone();
                                next.retain_clock(&id, "pause", json!({
                                    "pauseRevision": transition.get("pauseRevision"),
                                    "cutoffSample": transition.get("cutoffSample"),
                                    "physicalCutoffQpc100ns": transition.get("physicalCutoffQpc100ns"),
                                    "releasedQpc100ns": transition.get("releasedQpc100ns")
                                }), context.pause_revision)?;
                                let cutoff = next
                                    .last_pause_clock
                                    .as_ref()
                                    .ok_or("Pause clock is missing.")?
                                    .cutoff_sample
                                    .parse::<u64>()
                                    .map_err(|_| "Invalid Pause cutoff.")?;
                                let pending = serde_json::from_value(
                                    transition
                                        .get("audioPending")
                                        .cloned()
                                        .ok_or("Pause pending prefix is missing.")?,
                                )
                                .map_err(|_| "Invalid Pause pending prefix.")?;
                                manager.audio.pause_seal(
                                    &id,
                                    context.pause_revision,
                                    cutoff,
                                    pending,
                                )?;
                                Ok(next)
                            })();
                            match sealed {
                                Ok(next) if manager.controls.complete(operation, "paused") => {
                                    let audio = manager.audio.snapshot_control();
                                    manager.update(&app, &id, |s| {
                                        s.phase = "paused".into();
                                        s.control_revision = context.control_revision;
                                        s.pending_transition = None;
                                        s.last_pause_clock = next.last_pause_clock;
                                        s.pause = Some(audio_controls::PauseSeal {
                                            pause_revision: context.pause_revision,
                                            cutoff_sample: s
                                                .last_pause_clock
                                                .as_ref()
                                                .unwrap()
                                                .cutoff_sample
                                                .clone(),
                                            audio: audio.pause_audio,
                                        });
                                        s.audio_control_ledger = audio.ledger;
                                    });
                                }
                                Ok(_) => {}
                                Err(reason) => {
                                    manager.controls.complete_result(
                                        operation,
                                        "stopping",
                                        Err("protocolFailure".into()),
                                    );
                                    manager.update(&app, &id, |s| {
                                        s.phase = "stopping".into();
                                        s.pending_transition = None;
                                        s.message = Some(reason.clone());
                                    });
                                    space_error = Some(reason);
                                    stop.store(true, Ordering::Release);
                                }
                            }
                        } else if exact_pending && phase == "resuming" {
                            manager.update(&app, &id, |s| s.phase = "resuming".into());
                        } else if exact_pending && phase == "recording" {
                            let mut next = current;
                            let clock = next.retain_clock(
                                &id,
                                "resume",
                                json!({
                                    "activeSample":transition.get("activeEpochSample"),
                                    "physicalQpc100ns":transition.get("physicalEpochQpc100ns")
                                }),
                                context.pause_revision,
                            );
                            match clock {
                                Ok(_) if manager.controls.complete(operation, "recording") => {
                                    manager.update(&app, &id, |s| {
                                        s.phase = "recording".into();
                                        s.pause = None;
                                        s.pending_transition = None;
                                        s.clock_epoch = next.clock_epoch;
                                    });
                                }
                                Ok(_) => {}
                                Err(reason) => {
                                    manager.controls.complete_result(
                                        operation,
                                        "stopping",
                                        Err("protocolFailure".into()),
                                    );
                                    manager.update(&app, &id, |s| {
                                        s.phase = "stopping".into();
                                        s.pending_transition = None;
                                        s.message = Some(reason.clone());
                                    });
                                    space_error = Some(reason);
                                    stop.store(true, Ordering::Release);
                                }
                            }
                        }
                    }
                    if last_space.elapsed() > Duration::from_secs(1) {
                        last_space = Instant::now();
                        if let Err(e) = ensure_local_space(&journal.directory, RESERVE_BYTES) {
                            space_error = Some(e);
                            stop.store(true, Ordering::Release);
                        }
                    }
                },
                HostTaps {
                    preview: Some(preview_manager::Tap {
                        manager: manager.preview.clone(),
                        app: app.clone(),
                        owner: id.clone(),
                    }),
                    audio: Some(audio_manager::Tap {
                        manager: manager.audio.clone(),
                        app: app.clone(),
                        owner: id.clone(),
                    }),
                    session: Some(manager.clone()),
                },
            )?;
            if let Some(audio) = &mut journal.audio_capture {
                audio.samples = outcome["audioFrames"].as_u64();
                write_journal(&app, &journal)?;
            }
            if let Some(reason) = outcome["interruption"].as_str() {
                space_error = Some(reason.into());
            }

            manager.update(&app, &id, |s| {
                s.phase = "finalizing".into();
                s.pause = None;
                s.pending_transition = None;
                s.elapsed_ms = outcome["elapsedMs"].as_u64().unwrap_or(s.elapsed_ms);
                s.bytes_written = outcome["bytesWritten"].as_u64().unwrap_or(s.bytes_written);
                s.message = space_error;
            });
            let (path, warning) = finalize(&app, &journal, None)?;
            if let Some(warning) = warning {
                manager.update(&app, &id, |s| s.message = Some(warning));
            }
            Ok(path)
        })();
        if result.is_err() && (no_media || !journal.directory.join("capture.mp4").exists()) {
            let cleaned = cleanup(&app, &journal).is_ok();
            cancelled_before_capture =
                cleaned && stop.load(Ordering::Acquire) && manager.snapshot().phase == "stopping";
        }
        drop(session_lock);
        result
    })();
    manager.audio.finish(&app, &id);
    manager.preview.finish_recording(&app, &id);
    let attempt = manager
        .inner
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .start_attempt
        .clone();
    if let Some(attempt) = attempt {
        app.state::<controls_hotkeys::ControlsManager>()
            .finish_attempt(&attempt);
    }
    // Release exclusive admission before exposing the terminal state.
    drop(lease);
    let recoveries = find_recoveries(&app).unwrap_or_default();
    manager.update_recoveries(&app, &id, recoveries, |s| {
        s.countdown_remaining = None;
        s.audio = manager.audio.snapshot(Some(&id));
        match result {
            Ok(path) => {
                s.phase = "completed".into();
                s.bytes_written = fs::metadata(&path)
                    .map(|m| m.len())
                    .unwrap_or(s.bytes_written);
                s.output_path = Some(path.to_string_lossy().into_owned());
            }
            Err(e) => {
                s.phase = if s.recoveries.iter().any(|r| r.session_id == id) {
                    "recoverable"
                } else if cancelled_before_capture {
                    "idle"
                } else {
                    "failed"
                }
                .into();
                s.message = Some(e);
            }
        }
    });
    let mut i = manager.inner.lock().unwrap_or_else(|e| e.into_inner());
    if i.snapshot.session_id.as_deref() == Some(&id) {
        i.stop = None;
    }
}
#[cfg(test)]
fn validate_bounds(region: &Region, display: &Value) -> Result<(), String> {
    let x = display["x"]
        .as_i64()
        .ok_or("Invalid display coordinates.")?;
    let y = display["y"]
        .as_i64()
        .ok_or("Invalid display coordinates.")?;
    let w = display["width"]
        .as_i64()
        .ok_or("Invalid display dimensions.")?;
    let h = display["height"]
        .as_i64()
        .ok_or("Invalid display dimensions.")?;
    if i64::from(region.x) < x
        || i64::from(region.y) < y
        || i64::from(region.x) + i64::from(region.width) > x + w
        || i64::from(region.y) + i64::from(region.height) > y + h
    {
        return Err("The area no longer fits this display. Select it again.".into());
    }
    Ok(())
}
#[tauri::command]
pub(crate) fn recording_stop(
    window: Window,
    state: State<'_, RecordingManager>,
    session_id: Option<String>,
    selection_request_id: Option<String>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let mut i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if let Some((id, selecting)) = &i.selecting
        && selection_request_id.as_deref() == Some(id)
    {
        selecting.store(true, Ordering::Release);
    }
    if i.snapshot.session_id == session_id && active(&i.snapshot.phase) {
        if let Some(stop) = &i.stop {
            stop.store(true, Ordering::Release);
        }
        if i.snapshot.phase != "finalizing" {
            i.snapshot.phase = "stopping".into();
            i.snapshot.sequence += 1;
        }
    }
    Ok(i.snapshot.clone())
}

fn bundled_media(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let file = format!("ffmpeg-sidecar/{name}{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .map_err(error)?
        .parent()
        .ok_or("Missing app folder.")?
        .join(&file);
    let resource = app.path().resource_dir().map_err(error)?.join(file);
    [beside, resource]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("The bundled {name} is missing. Restore the complete app folder."))
}
fn run_media(mut command: Command, timeout: Duration) -> Result<(), String> {
    // File-only remux/validation: output is discarded and stderr is bounded by
    // draining into a fixed buffer. No shell or arbitrary user flags.
    command.arg("-nostdin");
    let mut child = process::ManagedChild::spawn(command).map_err(error)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let a = std::thread::spawn(move || {
        if let Some(mut r) = stdout {
            let _ = std::io::copy(&mut r, &mut std::io::sink());
        }
    });
    let b = std::thread::spawn(move || {
        if let Some(mut r) = stderr {
            let _ = std::io::copy(&mut r, &mut std::io::sink());
        }
    });
    let status = child.wait_timeout(timeout).map_err(error)?;
    if status.is_none() {
        let _ = child.control.kill();
    }
    drop(child);
    let _ = a.join();
    let _ = b.join();
    if status.is_some_and(|s| s.success()) {
        Ok(())
    } else {
        Err("Could not finish the MP4. Recovery files have been kept.".into())
    }
}
fn file_digest(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn publication_matches(j: &Journal) -> bool {
    regular_ancestors(&j.destination).is_ok()
        && j.published_size.is_some_and(|size| {
            fs::metadata(&j.destination).is_ok_and(|m| m.is_file() && m.len() == size)
        })
        && j.published_hash
            .as_ref()
            .is_some_and(|h| file_digest(&j.destination).is_ok_and(|actual| &actual == h))
}
fn finalize(
    app: &AppHandle,
    journal: &Journal,
    output_directory: Option<String>,
) -> Result<(PathBuf, Option<String>), String> {
    let mut j = journal.clone();
    if publication_matches(&j) {
        let _ = cleanup(app, &j);
        return Ok((j.destination, None));
    }
    let destination = if let Some(output) = output_directory {
        let dir = fs::canonicalize(output).map_err(error)?;
        regular_ancestors(&dir)?;
        dir.join(format!("Recording-{}.mp4", j.session_id))
    } else {
        j.destination.clone()
    };
    let candidate_root = if destination.parent() != j.directory.parent() {
        let stage = destination
            .parent()
            .ok_or("Invalid output folder.")?
            .join(format!(".vfl-recording-{}-recovery", j.session_id));
        if stage.exists() {
            validate_marker(&stage, &j.session_id, j.version)?;
        } else {
            fs::create_dir(&stage).map_err(error)?;
            let mut owner = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage.join("owner.json"))
                .map_err(error)?;
            serde_json::to_writer(
                &mut owner,
                &json!({"version":j.version,"sessionId":j.session_id}),
            )
            .map_err(error)?;
            owner.sync_all().map_err(error)?;
        }
        // Persist the owned alternative staging path before any media writes.
        j.candidate_directory = Some(stage.clone());
        write_journal(app, &j)?;
        stage
    } else {
        j.directory.clone()
    };
    let candidate = candidate_root.join("candidate.mp4");
    let spool = j.directory.join("capture.mp4");
    regular_ancestors(&spool)?;
    let bytes = fs::metadata(&spool).map_err(error)?.len();
    if bytes == 0 {
        return Err("The recording contains no recoverable video.".into());
    }
    let audio_plan = if j.version == 2 {
        audio_finalize::prepare(app, &j, &spool)?
    } else {
        None
    };
    if candidate.exists()
        && (validate_candidate(&candidate, &j.config, journal_geometry(&j)?).is_err()
            || audio_plan.as_ref().is_some_and(|plan| {
                audio_finalize::validate_tracks(app, &candidate, plan).is_err()
            }))
    {
        regular_ancestors(&candidate)?;
        fs::remove_file(&candidate).map_err(error)?;
    }
    if !candidate.exists() && j.version == 2 {
        let (command, required, timeout) = if let Some(plan) = &audio_plan {
            let (required, timeout) = plan.budget(bytes);
            (
                plan.command(&bundled_media(app, "ffmpeg")?, &spool, &candidate),
                required,
                timeout,
            )
        } else {
            let mut command = Command::new(bundled_media(app, "ffmpeg")?);
            command
                .args(["-hide_banner", "-v", "error", "-n", "-i"])
                .arg(&spool)
                .args([
                    "-map",
                    "0:v:0",
                    "-c:v",
                    "copy",
                    "-an",
                    "-map_metadata",
                    "-1",
                    "-movflags",
                    "+faststart",
                ])
                .arg(&candidate);
            let (required, timeout) = remux_budget(bytes, 0);
            (command, required, timeout)
        };
        ensure_local_space(&candidate_root, required)?;
        if let Err(e) = run_media(command, timeout) {
            let _ = fs::remove_file(&candidate);
            return Err(e);
        }
    }
    if !candidate.exists() {
        let mut pcm_bytes = 0;
        let mut command = Command::new(bundled_media(app, "ffmpeg")?);
        command
            .args(["-hide_banner", "-v", "error", "-n", "-i"])
            .arg(&spool);
        let pcm = j.directory.join("capture.pcm");
        if pcm.exists() {
            regular_ancestors(&pcm)?;
            let metadata = fs::metadata(&pcm).map_err(error)?;
            if !metadata.is_file()
                || metadata.len() < 4
                || !(j.config.system_audio_device_id.is_some()
                    || j.config.microphone_device_id.is_some())
            {
                return Err(
                    "The recording's audio recovery file is invalid. Its files have been kept."
                        .into(),
                );
            }
            pcm_bytes = metadata.len();
            // The native host writes sample zero at the exact video QPC epoch.
            // This is timestamp-normalized 48 kHz stereo PCM, not a separately
            // started audio capture clock. Encode it once during publication.
            command
                .args(["-f", "s16le", "-ar", "48000", "-ac", "2", "-t"])
                .arg(format!("{:.9}", (pcm_bytes / 4) as f64 / 48_000.0))
                .arg("-i")
                .arg(pcm)
                .args([
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-af",
                    "apad",
                    "-shortest",
                ]);
        } else {
            command.args(["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy"]);
        }
        command
            .args(["-map_metadata", "-1", "-movflags", "+faststart"])
            .arg(&candidate);
        let (required_bytes, timeout) = remux_budget(bytes, pcm_bytes);
        ensure_local_space(&candidate_root, required_bytes)?;
        if let Err(e) = run_media(command, timeout) {
            let _ = fs::remove_file(&candidate);
            return Err(e);
        }
    }
    validate_candidate(&candidate, &j.config, journal_geometry(&j)?)?;
    if let Some(plan) = &audio_plan {
        audio_finalize::validate_tracks(app, &candidate, plan)?;
    }
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidate)
        .map_err(error)?
        .sync_all()
        .map_err(error)?;
    j.published_size = Some(fs::metadata(&candidate).map_err(error)?.len());
    j.published_hash = Some(file_digest(&candidate)?);
    // Keep the destination local to the owned session until publication. A
    // recovery-to-other-volume intent is settled by this exact digest too.
    let original_destination = j.destination.clone();
    j.destination = destination.clone();
    write_journal(app, &j)?;
    match publish_candidate(&candidate, &destination) {
        Ok(_) => {
            // Publication is irreversible success. A cleanup error must never
            // hide the saved MP4 or offer it as unsaved footage.
            let _ = cleanup(app, &j);
            Ok((destination,audio_plan.as_ref().filter(|p|p.recovered_prefix).map(|_|"Saved the complete common audio/video prefix. The interrupted tail may be missing.".into())))
        }
        Err(e) => {
            let detail = e;
            j.destination = original_destination;
            j.published_size = None;
            j.published_hash = None;
            let _ = write_journal(app, &j);
            Err(format!(
                "Cannot save without replacing an existing file: {detail}. Choose another folder; the recording has been kept."
            ))
        }
    }
}

fn remux_budget(video_bytes: u64, pcm_bytes: u64) -> (u64, Duration) {
    // AAC192k plus conservative mux/VBR overhead at32kB/s; native PCM is192kB/s.
    // Incomplete terminal sample bytes cannot add a complete audio frame.
    let seconds = (pcm_bytes / 4).div_ceil(48_000);
    let reserve = video_bytes
        .saturating_add(seconds.saturating_mul(32_000))
        .saturating_add(RESERVE_BYTES);
    let timeout = 60_u64
        .saturating_add(video_bytes / 1_000_000)
        .saturating_add(seconds.div_ceil(4));
    (reserve, Duration::from_secs(timeout))
}

fn journal_geometry(j: &Journal) -> Result<Geometry, String> {
    let geometry = j
        .resolved_geometry
        .or(match &j.config.source {
            Source::Region(region) => Some(Geometry {
                width: region.width,
                height: region.height,
            }),
            Source::Window { .. } => None,
        })
        .ok_or("Recording recovery is missing its native source geometry.")?;
    geometry.validate()?;
    Ok(geometry)
}
fn validate_candidate(
    candidate: &Path,
    config: &RecordingConfig,
    geometry: Geometry,
) -> Result<(), String> {
    regular_ancestors(candidate)?;
    let probe = video::probe_video(candidate.to_string_lossy().into_owned())?;
    if probe.video_codec.as_deref() != Some("h264")
        || probe.width != (geometry.width + 1) & !1
        || probe.height != (geometry.height + 1) & !1
        || !probe.duration_s.is_finite()
        || probe.duration_s <= 0.
        || !probe
            .frame_rate
            .is_some_and(|fps| (fps - f64::from(config.fps)).abs() < 0.05)
        || probe.has_audio != config.audio_config().enabled()
        || probe.has_audio && probe.audio_codec.as_deref() != Some("aac")
    {
        return Err(
            "The recorded MP4 did not pass validation. Recovery files have been kept.".into(),
        );
    }
    Ok(())
}

fn publish_candidate(candidate: &Path, destination: &Path) -> Result<(), String> {
    // The normal path stays on the session's volume and needs no third copy.
    if candidate.parent().and_then(Path::parent) == destination.parent() {
        let path = tempfile::TempPath::from_path(candidate);
        if let Err(mut e) = path.persist_noclobber(destination) {
            // Retain the ownership-bearing error before converting it to text.
            e.path.disable_cleanup(true);
            return Err(e.error.to_string());
        }
        Ok(())
    } else {
        let mut copy =
            tempfile::NamedTempFile::new_in(destination.parent().ok_or("Invalid output folder.")?)
                .map_err(error)?;
        std::io::copy(&mut fs::File::open(candidate).map_err(error)?, &mut copy).map_err(error)?;
        copy.as_file().sync_all().map_err(error)?;
        copy.persist_noclobber(destination)
            .map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn recording_close(
    window: Window,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    audio_preview: State<'_, crate::audio_preview::AudioPreviewManager>,
    session_id: Option<String>,
) -> Result<(), String> {
    main_window(&window)?;
    let preview = state.preview.clone();
    drop(
        tauri::async_runtime::spawn_blocking(move || preview.stop_wait(false))
            .await
            .map_err(error)??,
    );
    let proxies = audio_preview.inner().clone();
    tauri::async_runtime::spawn_blocking(move || proxies.cancel_all())
        .await
        .map_err(error)??;
    let i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if i.snapshot.session_id != session_id || active(&i.snapshot.phase) || i.selecting.is_some() {
        return Err("Recording state changed. Keep the app open and try again.".into());
    }
    let close = activity.begin_close()?;
    drop(i);
    window.destroy().map_err(error)?;
    close.commit();
    Ok(())
}
fn cleanup_inventory(directory: &Path, names: &[&str]) -> Result<(), String> {
    regular_ancestors(directory)?;
    for entry in fs::read_dir(directory).map_err(error)? {
        let entry = entry.map_err(error)?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| names.contains(&name))
        {
            return Err(
                "The recording folder contains unowned files. Its files have been kept.".into(),
            );
        }
        regular_ancestors(&entry.path())?;
        if !entry.metadata().map_err(error)?.is_file() {
            return Err(
                "An owned recording file is not a regular file. Its files have been kept.".into(),
            );
        }
    }
    Ok(())
}
fn remove_cleanup_files(directory: &Path, names: &[&str]) -> Result<(), String> {
    for name in names {
        let path = directory.join(name);
        if path.exists() {
            regular_ancestors(&path)?;
            fs::remove_file(path).map_err(error)?;
        }
    }
    fs::remove_dir(directory).map_err(error)
}
fn cleanup_session_files(j: &Journal) -> Result<(), String> {
    validate_journal_audio(j)?;
    validate_marker(&j.directory, &j.session_id, j.version)?;
    let mut names = vec!["capture.mp4", "candidate.mp4", "owner.json", "session.lock"];
    if j.version == 1 {
        names.push("capture.pcm");
    } else if let Some(audio) = &j.audio_capture {
        names.extend(audio.owned_files(&j.config.audio_config())?);
    }
    // Preflight both inventories before deleting even their ownership markers.
    // Unknown siblings are preserved with the journal still usable for inspection.
    cleanup_inventory(&j.directory, &names)?;
    let stage_names = ["candidate.mp4", "owner.json"];
    let stage = j
        .candidate_directory
        .as_ref()
        .filter(|stage| stage.exists());
    if let Some(stage) = stage {
        validate_marker(stage, &j.session_id, j.version)?;
        cleanup_inventory(stage, &stage_names)?;
    }
    if let Some(stage) = stage {
        remove_cleanup_files(stage, &stage_names)?;
    }
    remove_cleanup_files(&j.directory, &names)
}
fn cleanup(app: &AppHandle, j: &Journal) -> Result<(), String> {
    cleanup_session_files(j)?;
    fs::remove_file(registry(app)?.join(format!("{}.json", j.session_id))).map_err(error)?;
    Ok(())
}

#[tauri::command]
pub(crate) fn recording_recover(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    session_id: String,
    output_directory: Option<String>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let j = read_journal(&app, &session_id)?;
    let session_lock = lock_session(&j)?;
    let lease = activity.begin("recording recovery")?;
    let manager = state.inner().clone();
    {
        let mut i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        if active(&i.snapshot.phase) {
            return Err("Wait for the active recording.".into());
        }
        i.snapshot.set_recovery_source(&j);
        i.snapshot.phase = "finalizing".into();
        i.snapshot.message = None;
        i.snapshot.output_path = None;
        i.snapshot.sequence += 1;
    }
    let snapshot = manager.snapshot();
    std::thread::spawn(move || {
        let result = finalize(&app, &j, output_directory);
        drop(session_lock);
        drop(lease);
        let recoveries = find_recoveries(&app).unwrap_or_default();
        manager.update_recoveries(&app, &session_id, recoveries, |s| match result {
            Ok((path, warning)) => {
                s.message = warning;
                s.phase = "completed".into();
                s.output_path = Some(path.to_string_lossy().into_owned());
            }
            Err(e) => {
                s.phase = "recoverable".into();
                s.message = Some(e);
            }
        });
    });
    Ok(snapshot)
}
#[tauri::command]
pub(crate) fn recording_discard_recovery(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    session_id: String,
) -> Result<(), String> {
    main_window(&window)?;
    let _lease = activity.begin("discarding recording recovery")?;
    let j = read_journal(&app, &session_id)?;
    let _session_lock = lock_session(&j)?;
    cleanup(&app, &j)?;
    let recoveries = find_recoveries(&app)?;
    let mut i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    i.replace_recoveries(recoveries);
    i.snapshot.sequence += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> RecordingConfig {
        RecordingConfig {
            request_id: "test".into(),
            source: Source::Region(Region {
                display_id: "display1".into(),
                x: -1920,
                y: 0,
                width: 640,
                height: 480,
            }),
            fps: 30,
            quality: "standard".into(),
            countdown_seconds: 3,
            show_cursor: true,
            audio: None,
            system_audio_device_id: None,
            microphone_device_id: None,
            output_directory: "out".into(),
        }
    }
    #[test]
    fn inflight_pause_ack_keeps_its_admitted_revision_after_owner_rotation() {
        let manager = RecordingManager::default();
        manager.controls.replace_owner_option(Some("a".repeat(32)));
        manager
            .controls
            .set_session("session", Arc::new(AtomicBool::new(false)), 0)
            .unwrap();
        manager.controls.phase("session", "recording");
        let pending = manager
            .controls
            .admit(
                session_controls::TransitionRequest {
                    owner_token: "a".repeat(32),
                    session_id: "session".into(),
                    operation_id: "b".repeat(32),
                    expected_control_revision: manager.controls.context().control_revision,
                    reconcile_fence_token: None,
                },
                "pause",
            )
            .unwrap();
        let snapshot = Snapshot {
            session_id: Some("session".into()),
            pending_transition: Some(pending.clone()),
            ..Default::default()
        };
        manager.controls.replace_owner_option(Some("c".repeat(32)));
        assert!(manager.controls.context().control_revision > pending.control_revision);
        assert!(snapshot.matches_transition(&pending.operation_id, Some(pending.control_revision)));
        assert!(!snapshot.matches_transition(
            &pending.operation_id,
            Some(manager.controls.context().control_revision)
        ));
        assert!(manager.controls.is_pending(&pending.operation_id));
        let wire = manager.controls.control(true, 0).unwrap();
        assert_eq!(wire["value"]["controlRevision"], pending.control_revision);
        assert_eq!(wire["value"]["pauseRevision"], 1);
        assert!(manager.controls.complete(&pending.operation_id, "paused"));
        assert_eq!(manager.controls.context().owner_token, Some("c".repeat(32)));
    }
    #[test]
    fn expired_transition_publication_cannot_clear_a_newer_pending_operation() {
        let mut snapshot = Snapshot {
            session_id: Some("session".into()),
            phase: "paused".into(),
            pending_transition: Some(session_controls::PendingTransition {
                operation_id: "b".repeat(32),
                action: "resume".into(),
                control_revision: 4,
            }),
            ..Default::default()
        };
        let old = session_controls::TransitionTimeout {
            session_id: "session".into(),
            operation_id: "a".repeat(32),
            control_revision: 3,
            code: "resumeTimeout".into(),
        };
        let before = serde_json::to_value(&snapshot).unwrap();
        assert!(!snapshot.apply_transition_timeout(&old));
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);
        let current = session_controls::TransitionTimeout {
            operation_id: "b".repeat(32),
            control_revision: 4,
            ..old
        };
        assert!(snapshot.apply_transition_timeout(&current));
        assert!(snapshot.pending_transition.is_none());
        assert_eq!(snapshot.phase, "paused");
    }
    #[test]
    fn exact_resume_retry_joins_consumed_fence_before_current_admission_checks() {
        let manager = RecordingManager::default();
        manager.controls.replace_owner_option(Some("a".repeat(32)));
        manager
            .controls
            .set_session("current", Arc::new(AtomicBool::new(false)), 1)
            .unwrap();
        manager.controls.phase("current", "paused");
        let request = session_controls::TransitionRequest {
            owner_token: "a".repeat(32),
            session_id: "current".into(),
            operation_id: "b".repeat(32),
            expected_control_revision: manager.controls.context().control_revision,
            reconcile_fence_token: Some("c".repeat(32)),
        };
        // The native seal/fence has already authorized and completed this exact
        // operation. Its fence is no longer present in the public Paused state.
        manager.controls.admit(request.clone(), "resume").unwrap();
        manager.controls.control(true, 0).unwrap();
        assert!(
            manager
                .controls
                .complete(&request.operation_id, "recording")
        );
        assert!(admit_transition(&manager, request.clone(), "resume").is_ok());
        assert!(!manager.controls.is_pending(&request.operation_id));
        assert!(manager.controls.control(true, 0).is_none());
        let mut conflict = request;
        conflict.reconcile_fence_token = Some("d".repeat(32));
        assert_eq!(
            admit_transition(&manager, conflict, "resume").unwrap_err(),
            "identityConflict"
        );
    }
    #[test]
    fn clock_receipts_bind_session_and_reset_without_retaining_a_history() {
        let mut snapshot = Snapshot {
            session_id: Some("current".into()),
            ..Default::default()
        };
        let initial = json!({"activeSample":"0","physicalQpc100ns":"1000000"});
        assert!(
            !snapshot
                .retain_clock("old", "initial", initial.clone(), 0)
                .unwrap()
        );
        assert!(snapshot.clock_epoch.is_none());
        assert!(
            snapshot
                .retain_clock("current", "initial", initial.clone(), 0)
                .unwrap()
        );
        assert!(
            !snapshot
                .retain_clock("current", "initial", initial, 0)
                .unwrap()
        );
        assert!(snapshot.retain_clock("current","pause",json!({"pauseRevision":1,"cutoffSample":"480","physicalCutoffQpc100ns":"1100000","releasedQpc100ns":"2200000"}),1).unwrap());
        assert!(
            snapshot
                .retain_clock(
                    "current",
                    "resume",
                    json!({"activeSample":"480","physicalQpc100ns":"5000000"}),
                    1
                )
                .unwrap()
        );
        assert!(snapshot.retain_clock("current","pause",json!({"pauseRevision":2,"cutoffSample":"960","physicalCutoffQpc100ns":"5100000","releasedQpc100ns":"5200000"}),2).unwrap());
        assert_eq!(
            snapshot.last_pause_clock.as_ref().unwrap().pause_revision,
            2
        );
        assert_eq!(snapshot.clock_epoch.as_ref().unwrap().active_sample, "480");
        let before = serde_json::to_value(&snapshot).unwrap();
        assert!(
            snapshot
                .retain_clock(
                    "current",
                    "resume",
                    json!({"activeSample":"960","physicalQpc100ns":"5199999"}),
                    2
                )
                .is_err()
        );
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);
        let journal = Journal {
            version: 1,
            session_id: "recovery".into(),
            created_at: "0".into(),
            directory: "owned".into(),
            destination: "output.mp4".into(),
            config: config(),
            resolved_geometry: None,
            published_size: None,
            published_hash: None,
            candidate_directory: None,
            audio_capture: None,
        };
        snapshot.set_recovery_source(&journal);
        assert!(snapshot.clock_epoch.is_none());
        assert!(snapshot.last_pause_clock.is_none());
    }
    #[test]
    fn validates_settings_before_native_work() {
        let mut c = config();
        assert!(validate_config(&c).is_ok());
        for fps in [0, 1, 120, u32::MAX] {
            c.fps = fps;
            assert!(validate_config(&c).is_err());
        }
        c = config();
        if let Source::Region(region) = &mut c.source {
            region.width = 641;
        }
        assert!(validate_config(&c).is_ok());
        if let Source::Region(region) = &mut c.source {
            region.width = 7681;
        }
        assert!(validate_config(&c).is_err());
        c = config();
        c.countdown_seconds = 100;
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn authoritative_pause_fence_read_advances_snapshot_sequence_only_when_changed() {
        use audio_controls::*;
        let mut snapshot = Snapshot {
            sequence: 7,
            phase: "paused".into(),
            pause: Some(PauseSeal {
                pause_revision: 1,
                cutoff_sample: "480".into(),
                audio: Some(PauseAudio {
                    source_generation: "audio-session".into(),
                    sealed_accepted_sequence: 0,
                    fence: FenceState {
                        state: FencePhase::Waiting,
                        token: None,
                        replacement_review_token: None,
                    },
                }),
            }),
            ..Default::default()
        };
        let ready = ControlSnapshot {
            pause_audio: Some(PauseAudio {
                source_generation: "audio-session".into(),
                sealed_accepted_sequence: 0,
                fence: FenceState {
                    state: FencePhase::Ready,
                    token: Some("a".repeat(32)),
                    replacement_review_token: None,
                },
            }),
            ledger: Some(AudioLedger::default()),
        };
        assert!(snapshot.sync_audio_control(ready.clone()));
        assert_eq!(snapshot.sequence, 8);
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["pause"]["audio"]["fence"]["state"],
            "ready"
        );
        assert!(!snapshot.sync_audio_control(ready));
        assert_eq!(snapshot.sequence, 8);
    }
    #[test]
    fn legacy_recovery_public_snapshot_normalizes_audio_without_changing_journal() {
        for system in [false, true] {
            for microphone in [false, true] {
                let mut legacy = config();
                legacy.system_audio_device_id = system.then(|| "legacy-system".into());
                legacy.microphone_device_id = microphone.then(|| "legacy-mic".into());
                let journal = Journal {
                    version: 1,
                    session_id: "legacy-snapshot".into(),
                    created_at: "0".into(),
                    directory: PathBuf::from("legacy-session"),
                    destination: PathBuf::from("legacy-output.mp4"),
                    config: legacy,
                    resolved_geometry: None,
                    published_size: None,
                    published_hash: None,
                    candidate_directory: None,
                    audio_capture: None,
                };
                let original = serde_json::to_vec(&journal).unwrap();
                assert!(journal.config.audio.is_none());
                for phase in ["finalizing", "completed", "recoverable"] {
                    let stale_meter = audio_types::AudioMeter {
                        state: "active".into(),
                        peak: 0.3,
                        rms: 0.2,
                        ..Default::default()
                    };
                    let mut snapshot = Snapshot {
                        session_id: Some("old-session".into()),
                        phase: phase.into(),
                        output_path: (phase == "completed").then(|| "legacy-output.mp4".into()),
                        audio: Some(AudioStatus {
                            session_id: "old-session".into(),
                            source_generation: "old-audio".into(),
                            sequence: 10,
                            accepted_command_sequence: 1,
                            applied_command_sequence: 1,
                            applied_at_sample: "480".into(),
                            rejected_command_sequence: None,
                            render_level: Default::default(),
                            microphone_level: Default::default(),
                            render: stale_meter.clone(),
                            microphone: stale_meter.clone(),
                            mixed: stale_meter,
                            message: None,
                        }),
                        ..Snapshot::default()
                    };
                    snapshot.set_recovery_source(&journal);
                    let public = serde_json::to_value(snapshot).unwrap();
                    assert_eq!(public["sessionId"], "legacy-snapshot");
                    assert!(public["audio"].is_null());
                    let audio = &public["config"]["audio"];
                    assert_eq!(
                        audio["render"],
                        if system {
                            json!({"kind":"system", "deviceId":"legacy-system"})
                        } else {
                            json!({"kind":"off"})
                        }
                    );
                    assert_eq!(
                        audio["microphone"],
                        if microphone {
                            json!({"deviceId":"legacy-mic", "suppression":"off"})
                        } else {
                            json!(null)
                        }
                    );
                    for level in ["renderLevel", "microphoneLevel"] {
                        assert_eq!(audio[level], json!({"gain":1.0, "muted":false}));
                    }
                    assert!(public["config"].get("systemAudioDeviceId").is_none());
                    assert!(public["config"].get("microphoneDeviceId").is_none());
                    assert_eq!(serde_json::to_vec(&journal).unwrap(), original);
                    assert_eq!(journal.version, 1);
                    assert!(journal.audio_capture.is_none());
                    println!("legacy-recovery-public-snapshot:{public}");
                }
            }
        }
    }
    #[test]
    fn physical_regions_handle_negative_origins_and_overflow() {
        let d = json!({"x":-1920,"y":0,"width":1920,"height":1080});
        let Source::Region(mut r) = config().source else {
            panic!("region fixture");
        };
        assert!(validate_bounds(&r, &d).is_ok());
        r.x = -1921;
        assert!(validate_bounds(&r, &d).is_err());
        r.x = i32::MAX;
        r.width = u32::MAX;
        assert!(validate_bounds(&r, &d).is_err());
    }
    #[test]
    fn recovery_ids_cannot_escape_registry() {
        for id in ["", "../abc", "C:\\abc", "a/b", "..", "x"] {
            assert!(!safe_id(id));
        }
        assert!(safe_id("abc123-567"));
    }
    #[test]
    fn identity_bound_updates_cannot_replace_new_session() {
        let manager = RecordingManager::default();
        let mut i = manager.inner.lock().unwrap();
        i.snapshot.session_id = Some("new".into());
        i.snapshot.phase = "recording".into();
        drop(i);
        assert!(manager.needs_close_guard());
        manager.shutdown();
        assert_eq!(manager.snapshot().session_id.as_deref(), Some("new"));
    }
    #[test]
    fn concurrent_startup_loads_keep_one_discovery_in_flight() {
        use std::sync::atomic::AtomicUsize;
        let manager = RecordingManager::default();
        let scans = Arc::new(AtomicUsize::new(0));
        let (entered, started) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let first = manager.clone();
        let first_scans = scans.clone();
        let first = std::thread::spawn(move || {
            first.load_with(|| {
                first_scans.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(vec![Recovery {
                    session_id: "kept".into(),
                    created_at: "0".into(),
                    message: "recover".into(),
                }])
            })
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (attempt, attempted) = mpsc::channel();
        let (finished, finish) = mpsc::channel();
        let second = manager.clone();
        let second_scans = scans.clone();
        let second = std::thread::spawn(move || {
            attempt.send(()).unwrap();
            let result = second.load_with(|| {
                second_scans.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            });
            finished.send(()).unwrap();
            result
        });
        attempted.recv_timeout(Duration::from_secs(5)).unwrap();
        let early_finish = finish.recv_timeout(Duration::from_secs(1));
        let state_responsive = manager.inner.try_lock().is_ok();
        let still_loading = !manager.inner.lock().unwrap().loaded;
        release.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert!(early_finish.is_ok());
        assert!(state_responsive);
        assert!(still_loading);
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.recoveries.len(), 1);
        assert_eq!(snapshot.recoveries[0].session_id, "kept");
        assert_eq!(snapshot.sequence, 1);
    }
    #[test]
    fn startup_discovery_retries_after_a_concurrent_recovery_list_change() {
        for discard in [false, true] {
            let recovery = Recovery {
                session_id: "kept".into(),
                created_at: "0".into(),
                message: "recover".into(),
            };
            let manager = RecordingManager::default();
            let stale = if discard {
                vec![recovery.clone()]
            } else {
                vec![]
            };
            let fresh = if discard { vec![] } else { vec![recovery] };
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut i = manager.inner.lock().unwrap();
                i.snapshot.recoveries = stale.clone();
                i.snapshot.phase = "recording".into();
                i.stop = Some(stop.clone());
            }
            let (entered, started) = mpsc::channel();
            let (release, resume) = mpsc::channel();
            let loading = manager.clone();
            let expected = fresh.clone();
            let worker = std::thread::spawn(move || {
                let mut scans = 0;
                loading
                    .load_with(|| {
                        scans += 1;
                        if scans == 1 {
                            entered.send(()).unwrap();
                            resume.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(stale.clone())
                        } else {
                            Ok(expected.clone())
                        }
                    })
                    .unwrap();
                scans
            });
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            manager
                .inner
                .lock()
                .unwrap()
                .replace_recoveries(fresh.clone());
            // This must remain independent of slow discovery/hashing.
            manager.shutdown();
            assert!(stop.load(Ordering::Acquire));
            assert!(manager.needs_close_guard());
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), 2);
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.recoveries.len(), fresh.len());
            if !discard {
                assert_eq!(snapshot.recoveries[0].session_id, "kept");
            }
        }
    }
    #[test]
    fn publication_requires_size_and_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mp4");
        fs::write(&path, b"ours").unwrap();
        let mut j = Journal {
            version: 1,
            session_id: "abc".into(),
            created_at: "0".into(),
            directory: dir.path().to_path_buf(),
            destination: path.clone(),
            config: config(),
            resolved_geometry: None,
            published_size: Some(4),
            published_hash: Some(file_digest(&path).unwrap()),
            candidate_directory: None,
            audio_capture: None,
        };
        assert!(publication_matches(&j));
        fs::write(&path, b"else").unwrap();
        assert!(!publication_matches(&j));
        j.published_hash = None;
        assert!(!publication_matches(&j));
    }
    #[test]
    fn no_clobber_retains_candidate_and_existing_output_on_collision() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join(".vfl-recording-abc");
        fs::create_dir(&stage).unwrap();
        let candidate = stage.join("candidate.mp4");
        let output = root.path().join("Recording-abc.mp4");
        fs::write(&candidate, b"our complete recording").unwrap();
        fs::write(&output, b"existing user file").unwrap();
        assert!(publish_candidate(&candidate, &output).is_err());
        assert_eq!(fs::read(&candidate).unwrap(), b"our complete recording");
        assert_eq!(fs::read(&output).unwrap(), b"existing user file");
        let alternative = root.path().join("Recording-def.mp4");
        publish_candidate(&candidate, &alternative).unwrap();
        assert_eq!(fs::read(alternative).unwrap(), b"our complete recording");
        assert!(!candidate.exists());
    }
    #[test]
    fn session_file_lock_prevents_second_instance_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.lock");
        let first = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        first.try_lock().unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(second.try_lock().is_err());
        drop(first);
        assert!(second.try_lock().is_ok());
    }
    #[test]
    fn ownership_marker_must_match_session_before_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({"version":1,"sessionId":"abc"})).unwrap(),
        )
        .unwrap();
        assert!(validate_marker(dir.path(), "abc", 1).is_ok());
        assert!(validate_marker(dir.path(), "def", 1).is_err());
        fs::write(path, b"invalid").unwrap();
        assert!(validate_marker(dir.path(), "abc", 1).is_err());
    }
    #[test]
    fn cleanup_preserves_unknown_siblings_and_removes_only_version_declared_files() {
        use audio_types::{Microphone, RecordingAudioConfig, RenderAudio, Suppression};
        for mode in ["legacy", "render", "microphone-off", "microphone-save"] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join(".vfl-recording-abc");
            let stage = root.path().join(".vfl-recording-abc-recovery");
            fs::create_dir(&directory).unwrap();
            fs::create_dir(&stage).unwrap();
            let mut config = config();
            if mode != "legacy" {
                config.audio = Some(RecordingAudioConfig {
                    render: if mode == "render" {
                        RenderAudio::System {
                            device_id: "default".into(),
                        }
                    } else {
                        RenderAudio::Off
                    },
                    microphone: (mode != "render").then(|| Microphone {
                        device_id: "default".into(),
                        suppression: if mode == "microphone-save" {
                            Suppression::Save
                        } else {
                            Suppression::Off
                        },
                    }),
                    ..Default::default()
                });
            }
            let journal = Journal {
                version: if mode == "legacy" { 1 } else { 2 },
                session_id: "abc".into(),
                created_at: "0".into(),
                directory: directory.clone(),
                destination: root.path().join("Recording-abc.mp4"),
                audio_capture: audio_finalize::AudioJournal::new(&config.audio_config()),
                config,
                resolved_geometry: None,
                published_size: None,
                published_hash: None,
                candidate_directory: Some(stage.clone()),
            };
            let marker =
                serde_json::to_vec(&json!({"version":journal.version,"sessionId":"abc"})).unwrap();
            let mut owned = vec!["capture.mp4", "candidate.mp4", "session.lock"];
            let mut unknown = vec!["unrelated.bin"];
            if mode == "legacy" {
                owned.push("capture.pcm");
                unknown.extend([
                    "render.f32le",
                    "microphone.f32le",
                    audio_envelope::FILE_NAME,
                ]);
            } else {
                owned.extend(
                    journal
                        .audio_capture
                        .as_ref()
                        .unwrap()
                        .owned_files(&journal.config.audio_config())
                        .unwrap(),
                );
                unknown.push("capture.pcm");
                unknown.push(if mode == "render" {
                    "microphone.f32le"
                } else {
                    "render.f32le"
                });
                if mode != "microphone-save" {
                    unknown.extend(["microphone-processed.f32le", "microphone-processed.partial"]);
                }
            }
            for name in owned.iter().chain(&unknown) {
                fs::write(directory.join(name), name.as_bytes()).unwrap();
            }
            fs::write(directory.join("owner.json"), &marker).unwrap();
            fs::write(stage.join("owner.json"), &marker).unwrap();
            fs::write(stage.join("candidate.mp4"), b"stage candidate").unwrap();
            assert!(cleanup_session_files(&journal).is_err(), "{mode}");
            for name in owned.iter().chain(&unknown) {
                assert_eq!(
                    fs::read(directory.join(name)).unwrap(),
                    name.as_bytes(),
                    "{mode}/{name}"
                );
            }
            assert_eq!(fs::read(directory.join("owner.json")).unwrap(), marker);
            assert_eq!(
                fs::read(stage.join("candidate.mp4")).unwrap(),
                b"stage candidate"
            );
            for name in &unknown {
                fs::remove_file(directory.join(name)).unwrap();
            }
            // An alternate stage unknown must also prevent session deletion.
            fs::write(stage.join("unrelated.bin"), b"stage unknown").unwrap();
            assert!(cleanup_session_files(&journal).is_err());
            assert_eq!(
                fs::read(directory.join("capture.mp4")).unwrap(),
                b"capture.mp4"
            );
            assert_eq!(
                fs::read(stage.join("unrelated.bin")).unwrap(),
                b"stage unknown"
            );
            assert_eq!(fs::read(stage.join("owner.json")).unwrap(), marker);
            fs::remove_file(stage.join("unrelated.bin")).unwrap();
            cleanup_session_files(&journal).unwrap();
            assert!(!directory.exists());
            assert!(!stage.exists());
        }
    }
    #[test]
    fn only_bundled_and_exact_dev_origins_can_control_capture() {
        for origin in ["tauri://localhost/", "http://tauri.localhost/"] {
            assert!(trusted_origin(&origin.parse().unwrap()));
        }
        for origin in [
            "https://evil.test/",
            "http://localhost:14220/",
            "http://evil@tauri.localhost/",
            "http://localhost:1422.evil.test/",
        ] {
            if let Ok(url) = origin.parse() {
                assert!(!trusted_origin(&url));
            }
        }
    }
    #[test]
    fn pcm_finalization_budget_depends_on_duration_not_compressed_video_size() {
        let hour_pcm = 48_000 * 4 * 3600;
        let (space, time) = remux_budget(1000, hour_pcm);
        assert_eq!(space, 1000 + 3600 * 32_000 + RESERVE_BYTES);
        assert_eq!(time, Duration::from_secs(960));
        assert_eq!(
            remux_budget(1000, 0),
            (1000 + RESERVE_BYTES, Duration::from_secs(60))
        );
        assert_eq!(
            remux_budget(1000, hour_pcm + 3),
            remux_budget(1000, hour_pcm)
        );
    }
    #[test]
    fn recovery_audio_versions_never_silently_reinterpret_legacy_stems() {
        let mut journal = Journal {
            version: 1,
            session_id: "abc".into(),
            created_at: "0".into(),
            directory: PathBuf::new(),
            destination: PathBuf::new(),
            config: config(),
            resolved_geometry: None,
            published_size: None,
            published_hash: None,
            candidate_directory: None,
            audio_capture: None,
        };
        journal.config.system_audio_device_id = Some("default".into());
        assert!(validate_journal_audio(&journal).is_ok());
        journal.version = 2;
        assert!(validate_journal_audio(&journal).is_err());
        journal.config.audio = Some(journal.config.audio_config()); // explicit legacy values cannot coexist with v2
        assert!(validate_journal_audio(&journal).is_err());
        journal.config.system_audio_device_id = None;
        assert!(validate_journal_audio(&journal).is_err());
        journal.audio_capture = audio_finalize::AudioJournal::new(&journal.config.audio_config());
        assert!(validate_journal_audio(&journal).is_ok());
        assert!(serde_json::to_vec(&journal).unwrap().len() < PROTOCOL_LIMIT as usize);
        journal.version = 1;
        assert!(validate_journal_audio(&journal).is_err());
    }
    #[test]
    fn control_writer_enforces_the_actual512_byte_line_including_newline() {
        assert_eq!(
            control_line(&json!({"x":"a".repeat(503)})).unwrap().len(),
            512
        );
        assert!(control_line(&json!({"x":"a".repeat(504)})).is_err());
        let escaped = control_line(&json!({"x":"a\nb"})).unwrap();
        assert_eq!(escaped.iter().filter(|b| **b == b'\n').count(), 1);
    }
}
