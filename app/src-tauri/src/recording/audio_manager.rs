//! Audio identities and reconciliation use an independent bounded transport lane.
use super::*;
use super::{
    audio_control_state::Engine, audio_controls::*, audio_types::*,
    session_controls::ControlContext,
};
use std::sync::{Condvar, atomic::AtomicU64};
#[derive(Default)]
struct Inner {
    owner: Option<String>,
    config: Option<NativeAudioConfig>,
    engine: Option<Engine>,
    context: ControlContext,
    native_sequence: Option<u64>,
    sequence: u64,
    ready: bool,
    closed: bool,
}
pub(super) struct AudioManager {
    inner: Mutex<Inner>,
    changed: Condvar,
    applications: Mutex<Vec<(String, ProcessIdentity)>>,
    next: AtomicU64,
    epoch: std::time::Instant,
}
impl Default for AudioManager {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
            applications: Mutex::new(vec![]),
            next: AtomicU64::new(0),
            epoch: std::time::Instant::now(),
        }
    }
}
impl AudioManager {
    fn now(&self) -> u64 {
        self.epoch.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    fn publish(&self, app: &AppHandle) {
        if let Some(status) = self.snapshot(None) {
            let _ = app.emit("recording-audio-status", status);
        }
    }
    pub fn register(&self, list: AudioApplicationListing) -> Result<Value, String> {
        let mut registry = self
            .applications
            .lock()
            .map_err(|_| "Audio applications are unavailable.")?;
        let mut fresh = Vec::new();
        let mut applications = Vec::new();
        let mut truncated = list.truncated;
        for item in list.applications.into_iter().take(128) {
            let id = match registry
                .iter()
                .find(|(_, identity)| *identity == item.identity)
            {
                Some((id, _)) => id.clone(),
                None => format!(
                    "audio-app-{}",
                    crate::activity::allocate_operation_id(&self.next, 9_007_199_254_740_991)
                        .ok_or("Restart the app before another application selection.")?
                ),
            };
            let public = json!({"id":id,"name":item.name,"processName":item.process_name,"scope":"processTree","available":item.available,"reason":item.reason});
            applications.push(public);
            if serde_json::to_vec(&json!({"applications":applications,"truncated":true}))
                .map_err(error)?
                .len()
                >= 60 * 1024
            {
                applications.pop();
                truncated = true;
                break;
            }
            fresh.push((id, item.identity));
        }
        *registry = fresh;
        Ok(json!({"applications":applications,"truncated":truncated}))
    }
    pub fn resolve_audio_config(
        &self,
        id: &str,
        config: &RecordingAudioConfig,
    ) -> Result<NativeAudioConfig, String> {
        config.validate()?;
        let render = match &config.render {
            RenderAudio::Off => NativeRender::Off,
            RenderAudio::System { device_id } => NativeRender::System {
                device_id: device_id.clone(),
            },
            RenderAudio::Application { application_id } => NativeRender::Application {
                identity: self
                    .applications
                    .lock()
                    .map_err(|_| "Audio applications are unavailable.")?
                    .iter()
                    .find(|(token, _)| token == application_id)
                    .map(|(_, identity)| identity.clone())
                    .ok_or("The audio application is stale. Refresh and select it again.")?,
            },
        };
        let native = NativeAudioConfig {
            session_id: id.into(),
            source_generation: format!("audio-{id}"),
            render,
            microphone: config.microphone.clone(),
            render_level: config.render_level,
            microphone_level: config.microphone_level,
        };
        native.validate()?;
        Ok(native)
    }
    pub fn prepare(
        &self,
        id: &str,
        config: &RecordingAudioConfig,
    ) -> Result<NativeAudioConfig, String> {
        let native = self.resolve_audio_config(id, config)?;
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        let context = state.context.clone();
        *state = Inner {
            owner: Some(id.into()),
            config: Some(native.clone()),
            engine: Some(Engine::new(native.clone(), context.clone())),
            context,
            ..Default::default()
        };
        Ok(native)
    }
    pub fn set_context(&self, context: ControlContext) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if context.control_revision < s.context.control_revision
            || context.control_revision == s.context.control_revision
                && s.context.session_id != context.session_id
        {
            return;
        }
        if s.owner.is_some() && s.owner != context.session_id {
            // A new session retires the previous registry before its first
            // public preparing snapshot; stale cloned contexts cannot restore it.
            s.engine = None;
            s.config = None;
            s.owner = None;
            s.ready = false;
            s.closed = false;
            s.native_sequence = None;
            s.sequence = 0;
        }
        s.context = context.clone();
        if let Some(e) = &mut s.engine {
            e.set_context(context, self.now());
            s.context = e.context.clone();
        }
        self.changed.notify_all();
    }
    pub fn accepted_sequence(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .engine
            .as_ref()
            .map_or(0, Engine::accepted_sequence)
    }
    pub fn snapshot_control(&self) -> ControlSnapshot {
        let s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.engine
            .as_ref()
            .map_or_else(ControlSnapshot::default, |e| ControlSnapshot {
                pause_audio: e.pause_audio(),
                ledger: e.ledger(),
            })
    }
    pub fn pause_seal(
        &self,
        id: &str,
        revision: u64,
        cutoff: u64,
        pending: Vec<AcceptedIntent>,
    ) -> Result<(), String> {
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        if s.owner.as_deref() != Some(id) {
            return Err("Audio session changed.".into());
        }
        s.engine
            .as_mut()
            .ok_or("Audio session is unavailable.")?
            .pause_seal(revision, cutoff, pending)
            .map_err(|c| c.policy().2.to_string())?;
        self.changed.notify_all();
        Ok(())
    }
    pub fn fence_ready(&self, id: &str, revision: u64, token: &str) -> bool {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.owner.as_deref() != Some(id) {
            return false;
        }
        s.engine.as_mut().is_some_and(|e| {
            e.tick(self.now());
            e.fence_ready(revision, token)
        })
    }
    pub fn snapshot(&self, id: Option<&str>) -> Option<AudioStatus> {
        let s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if id.is_some_and(|id| s.owner.as_deref() != Some(id)) {
            return None;
        }
        s.engine.as_ref().map(|e| e.status.clone())
    }
    pub fn update(
        &self,
        app: &AppHandle,
        id: &str,
        mut status: AudioStatus,
        ready: bool,
    ) -> Result<(), String> {
        validate_status(&status)?;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        let config = s.config.as_ref().ok_or("Audio session is unavailable.")?;
        if s.owner.as_deref() != Some(id)
            || status.session_id != id
            || status.source_generation != config.source_generation
        {
            return Err("Audio status belongs to another session.".into());
        }
        if s.native_sequence.is_some_and(|n| status.sequence <= n) {
            return Ok(());
        }
        s.native_sequence = Some(status.sequence);
        s.ready |= ready;
        s.sequence = s
            .sequence
            .max(s.engine.as_ref().map_or(0, |e| e.status.sequence));
        s.sequence = s
            .sequence
            .checked_add(1)
            .filter(|n| *n <= MAX_SAFE)
            .ok_or("Audio counter exhausted.")?;
        status.sequence = s.sequence;
        if let Some(e) = &mut s.engine {
            e.update(status, self.now());
        }
        self.changed.notify_all();
        drop(s);
        self.publish(app);
        Ok(())
    }
    pub fn finish(&self, app: &AppHandle, id: &str) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.owner.as_deref() != Some(id) {
            return;
        }
        s.closed = true;
        s.ready = false;
        if let Some(e) = &mut s.engine {
            e.stop();
        }
        self.changed.notify_all();
        drop(s);
        self.publish(app);
    }
    fn failure(s: &Inner, code: FailureCode, sequence: Option<u64>) -> ControlFailure {
        if let Some(e) = &s.engine {
            return e.failure(code, sequence);
        }
        let (retry, action, message) = code.policy();
        ControlFailure {
            error: FailureDetail {
                code,
                retry,
                action,
                message: message.into(),
                command_sequence: sequence,
            },
            context: FailureContext::default(),
            state: FailureState {
                phase: s.context.phase.clone(),
                audio_status: None,
                ledger: None,
                pause: None,
            },
        }
    }
    pub fn submit(&self, app: &AppHandle, request: LevelRequest) -> ControlResult<LevelReply> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let seq = request.command_sequence;
        let result = s
            .engine
            .as_mut()
            .ok_or(FailureCode::StaleSession)
            .and_then(|e| e.submit(request, self.now()))
            .and_then(ControlResult::success);
        let result = match result {
            Ok(v) => v,
            Err(c) => {
                if matches!(
                    c,
                    FailureCode::Capacity
                        | FailureCode::EnvelopeCapacity
                        | FailureCode::CounterExhausted
                ) && let Some(stop) = &s.context.stop
                {
                    stop.store(true, Ordering::Release);
                }
                ControlResult::failure(Self::failure(&s, c, Some(seq)))
            }
        };
        drop(s);
        self.publish(app);
        result
    }
    fn reconcile(
        &self,
        request: ReconcileRequest,
        retry: Option<ReconcileRetryRequest>,
    ) -> ControlResult<ReconcileReply> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let batch_id = request.batch_id.clone();
        let expected_owner = request.owner_token.clone();
        let expected_session = request.session_id.clone();
        let expected_generation = request.source_generation.clone();
        let admitted =
            s.engine
                .as_mut()
                .ok_or(FailureCode::StaleSession)
                .and_then(|e| match retry {
                    Some(r) => e.retry(r, self.now()),
                    None => e.reconcile(request, self.now()),
                });
        let attempt = match admitted {
            Ok(a) => a,
            Err(c) => {
                if matches!(
                    c,
                    FailureCode::Capacity
                        | FailureCode::EnvelopeCapacity
                        | FailureCode::CounterExhausted
                ) && let Some(stop) = &s.context.stop
                {
                    stop.store(true, Ordering::Release);
                }
                return ControlResult::failure(Self::failure(&s, c, None));
            }
        };
        let wait_deadline = Instant::now() + Duration::from_millis(RECONCILE_MS);
        loop {
            // A wait can outlive owner/session replacement. Never resolve its
            // batchId against a later registry or disclose a fresh owner's fence.
            let stale = if s.context.owner_token.as_deref() != Some(&expected_owner) {
                Some(FailureCode::StaleOwner)
            } else if s.owner.as_deref() != Some(&expected_session) {
                Some(FailureCode::StaleSession)
            } else if s
                .config
                .as_ref()
                .is_none_or(|c| c.source_generation != expected_generation)
            {
                Some(FailureCode::SourceChanged)
            } else {
                None
            };
            if let Some(code) = stale {
                return ControlResult::failure(Self::failure(&s, code, None));
            }
            let outcome = s
                .engine
                .as_mut()
                .and_then(|e| e.outcome(&batch_id, attempt, self.now()));
            if let Some(result) = outcome {
                return match result.and_then(ControlResult::success) {
                    Ok(v) => v,
                    Err(c) => ControlResult::failure(Self::failure(&s, c, None)),
                };
            }
            if Instant::now() >= wait_deadline {
                return ControlResult::failure(Self::failure(
                    &s,
                    FailureCode::ReconciliationTimeout,
                    None,
                ));
            }
            // No media worker lane, registry lock, or transition mutex is held while waiting.
            let (next, _) = self
                .changed
                .wait_timeout(s, Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner());
            s = next;
            if s.engine.is_none() {
                return ControlResult::failure(Self::failure(&s, FailureCode::StaleSession, None));
            }
        }
    }
    fn control(&self, id: &str, send: &mpsc::SyncSender<Value>, stop: &AtomicBool) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.owner.as_deref() != Some(id) || s.closed || !s.ready {
            return;
        }
        let Some(e) = &mut s.engine else {
            return;
        };
        for command in e.ordinary() {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let seq = command.command_sequence;
            if send
                .try_send(json!({"operation":"audio-levels","value":command}))
                .is_err()
            {
                return;
            }
            e.sent(seq);
        }
        while !stop.load(Ordering::Acquire) {
            let Some(message) = e.wire_front(self.now()) else {
                break;
            };
            let Ok(value) = serde_json::to_value(message) else {
                stop.store(true, Ordering::Release);
                return;
            };
            if send.try_send(value).is_err() {
                break;
            }
            e.wire_sent();
        }
    }
    fn progress(&self, id: &str, value: &Value) -> Result<(), String> {
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        if s.owner.as_deref() != Some(id) {
            return Err("Audio session changed.".into());
        }
        let Some(e) = &mut s.engine else {
            return Ok(());
        };
        if let Some(receipts) = value.get("audioReceipts") {
            let receipts: Vec<AudioReceipt> =
                serde_json::from_value(receipts.clone()).map_err(error)?;
            if receipts.len() > 64 {
                return Err("Audio receipt bound exceeded.".into());
            }
            for r in receipts {
                e.receipt(r).map_err(|c| c.policy().2.to_string())?;
            }
        }
        if let Some(ack) = value.get("reconcileAck").filter(|v| !v.is_null()) {
            e.ack(
                serde_json::from_value(ack.clone()).map_err(error)?,
                self.now(),
            )
            .map_err(|c| c.policy().2.to_string())?;
        }
        if let Some(failure) = value.get("reconcileError").filter(|v| !v.is_null()) {
            e.stage_error(
                failure["batchToken"].as_str().unwrap_or(""),
                failure["attempt"].as_u64().unwrap_or(0),
                failure["code"].as_str().unwrap_or(""),
                self.now(),
            )
            .map_err(|c| c.policy().2.to_string())?;
        }
        self.changed.notify_all();
        Ok(())
    }
}
fn validate_status(status: &AudioStatus) -> Result<(), String> {
    if !bounded_id(&status.session_id)
        || !bounded_id(&status.source_generation)
        || status.sequence > 9_007_199_254_740_991
        || status.accepted_command_sequence > 9_007_199_254_740_991
        || status.applied_command_sequence > status.accepted_command_sequence
        || status
            .applied_at_sample
            .parse::<u64>()
            .ok()
            .is_none_or(|n| n.to_string() != status.applied_at_sample)
        || status
            .rejected_command_sequence
            .is_some_and(|n| n > 9_007_199_254_740_991)
    {
        return Err("Invalid audio status identity.".into());
    }
    status.render_level.validate()?;
    status.microphone_level.validate()?;
    for meter in [&status.render, &status.microphone, &status.mixed] {
        if !matches!(
            meter.state.as_str(),
            "off" | "pending" | "active" | "silent" | "muted" | "failed"
        ) || !meter.peak.is_finite()
            || !meter.rms.is_finite()
            || meter.peak < 0.
            || meter.rms < 0.
        {
            return Err("Invalid audio meter status.".into());
        }
    }
    if status.message.as_ref().is_some_and(|m| m.len() > 4096)
        || serde_json::to_vec(status).map_err(error)?.len() >= STATUS_BYTES
    {
        return Err("Audio status exceeded its byte bound.".into());
    }
    Ok(())
}
#[derive(Clone)]
pub(super) struct Tap {
    pub manager: Arc<AudioManager>,
    pub app: AppHandle,
    pub owner: String,
}
impl Tap {
    pub fn control(&self, send: &mpsc::SyncSender<Value>, stop: &AtomicBool) {
        self.manager.control(&self.owner, send, stop);
    }
    pub fn progress(&self, value: &Value) -> Result<(), String> {
        self.manager.progress(&self.owner, value)?;
        if let Some(audio) = value.get("audio").filter(|v| !v.is_null()) {
            self.manager.update(
                &self.app,
                &self.owner,
                serde_json::from_value(audio.clone()).map_err(error)?,
                value["ready"] == true,
            )?;
        }
        Ok(())
    }
}
#[tauri::command]
pub(crate) async fn recording_audio_applications(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
) -> Result<Value, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    crate::command_dispatch::files(move || {
        let value = helper_call(
            &app,
            json!({"operation":"audio-applications","excludedPid":std::process::id()}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )?;
        manager
            .audio
            .register(serde_json::from_value(value).map_err(error)?)
    })
    .await
}
#[tauri::command]
pub(crate) fn recording_audio_snapshot(
    window: Window,
    state: State<'_, RecordingManager>,
    session_id: String,
) -> Result<Option<AudioStatus>, String> {
    main_window(&window)?;
    Ok(state.audio.snapshot(Some(&session_id)))
}
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) fn recording_audio_set_levels(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    owner_token: String,
    expected_control_revision: u64,
    session_id: String,
    source_generation: String,
    command_sequence: u64,
    render_level: MixLevel,
    microphone_level: MixLevel,
) -> Result<ControlResult<LevelReply>, String> {
    main_window(&window)?;
    state.audio.set_context(state.controls.context());
    Ok(state.audio.submit(
        &app,
        LevelRequest {
            owner_token,
            expected_control_revision,
            session_id,
            source_generation,
            command_sequence,
            render_level,
            microphone_level,
        },
    ))
}
#[tauri::command]
pub(crate) async fn recording_audio_reconcile(
    window: Window,
    state: State<'_, RecordingManager>,
    request: ReconcileRequest,
) -> Result<ControlResult<ReconcileReply>, String> {
    main_window(&window)?;
    let manager = state.audio.clone();
    manager.set_context(state.controls.context());
    tauri::async_runtime::spawn_blocking(move || manager.reconcile(request, None))
        .await
        .map_err(error)
}
#[tauri::command]
pub(crate) async fn recording_audio_reconcile_retry(
    window: Window,
    state: State<'_, RecordingManager>,
    request: ReconcileRetryRequest,
) -> Result<ControlResult<ReconcileReply>, String> {
    main_window(&window)?;
    let manager = state.audio.clone();
    manager.set_context(state.controls.context());
    tauri::async_runtime::spawn_blocking(move || {
        manager.reconcile(request.request.clone(), Some(request))
    })
    .await
    .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manager() -> AudioManager {
        let m = AudioManager::default();
        let context = ControlContext {
            owner_token: Some("1".repeat(32)),
            session_id: Some("session".into()),
            phase: "recording".into(),
            control_revision: 1,
            pause_revision: 0,
            resolved_role_mask: 1,
            stop: Some(Arc::new(AtomicBool::new(false))),
        };
        m.set_context(context);
        m.prepare(
            "session",
            &RecordingAudioConfig {
                render: RenderAudio::System {
                    device_id: "default".into(),
                },
                ..Default::default()
            },
        )
        .unwrap();
        m.inner.lock().unwrap().ready = true;
        m
    }
    fn add(manager: &AudioManager, seq: u64) {
        manager
            .inner
            .lock()
            .unwrap()
            .engine
            .as_mut()
            .unwrap()
            .submit(
                LevelRequest {
                    owner_token: "1".repeat(32),
                    expected_control_revision: 1,
                    session_id: "session".into(),
                    source_generation: "audio-session".into(),
                    command_sequence: seq,
                    render_level: MixLevel {
                        gain: 1.,
                        muted: seq % 2 == 1,
                    },
                    microphone_level: MixLevel::default(),
                },
                0,
            )
            .unwrap();
    }
    #[test]
    fn bounded_transport_retains_an_unsent_edge_when_pipe_full() {
        let m = manager();
        add(&m, 1);
        add(&m, 2);
        let (send, receive) = mpsc::sync_channel(1);
        send.try_send(json!({"occupied":true})).unwrap();
        let stop = AtomicBool::new(false);
        m.control("session", &send, &stop);
        receive.recv().unwrap();
        m.control("session", &send, &stop);
        assert_eq!(receive.recv().unwrap()["value"]["commandSequence"], 1);
        m.control("session", &send, &stop);
        assert_eq!(receive.recv().unwrap()["value"]["commandSequence"], 2);
        assert_eq!(m.snapshot_control().ledger.unwrap().pending.len(), 2);
        stop.store(true, Ordering::Release);
        m.control("session", &send, &stop);
        assert!(receive.try_recv().is_err());
    }
    #[test]
    fn one_pump_delivers_all_64_edges_and_stale_session_cannot_write_new_pipe() {
        let m = manager();
        for seq in 1..=64 {
            add(&m, seq);
        }
        let (send, receive) = mpsc::sync_channel(65);
        let stop = AtomicBool::new(false);
        m.control("session", &send, &stop);
        for seq in 1..=64 {
            assert_eq!(receive.recv().unwrap()["value"]["commandSequence"], seq);
        }
        assert!(receive.try_recv().is_err());
        m.control("stale", &send, &stop);
        assert!(receive.try_recv().is_err());
    }
    #[test]
    fn new_session_retires_old_ledger_and_stale_context_cannot_restore_it() {
        let m = manager();
        add(&m, 1);
        let old = m.inner.lock().unwrap().context.clone();
        let mut next = old.clone();
        next.session_id = Some("new-session".into());
        next.control_revision += 1;
        next.phase = "preparing".into();
        m.set_context(next.clone());
        assert!(m.snapshot_control().ledger.is_none());
        assert!(m.snapshot(Some("session")).is_none());
        m.set_context(old);
        assert_eq!(m.inner.lock().unwrap().context.session_id, next.session_id);
    }
    #[test]
    fn owner_replacement_settles_waiter_without_exposing_fresh_review_or_fence_tokens() {
        let m = Arc::new(manager());
        let mut context = m.inner.lock().unwrap().context.clone();
        context.control_revision += 1;
        context.pause_revision = 1;
        context.phase = "pausing".into();
        m.set_context(context.clone());
        context.phase = "paused".into();
        m.set_context(context.clone());
        m.pause_seal("session", 1, 480, vec![]).unwrap();
        let request = ReconcileRequest {
            owner_token: "1".repeat(32),
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            pause_revision: 1,
            batch_id: "2".repeat(32),
            commands: vec![],
            mode: ReconcileMode::RetainedQueue,
            replacement_review_token: None,
            acknowledge_lost_local_intent: None,
        };
        let worker = m.clone();
        let join = std::thread::spawn(move || worker.reconcile(request, None));
        let deadline = Instant::now() + Duration::from_secs(1);
        let admitted = loop {
            if m.inner
                .lock()
                .unwrap()
                .engine
                .as_mut()
                .unwrap()
                .wire_front(m.now())
                .is_some()
            {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        context.owner_token = Some("4".repeat(32));
        context.control_revision += 1;
        m.set_context(context);
        let result = join.join().unwrap();
        assert!(admitted, "waiter must reach a real pending attempt");
        match result {
            ControlResult::Err { failure, .. } => {
                assert_eq!(failure.error.code, FailureCode::StaleOwner);
                let audio = failure.state.pause.unwrap().audio.unwrap();
                assert!(audio.fence.token.is_none());
                assert!(audio.fence.replacement_review_token.is_none());
            }
            _ => panic!("old owner cannot obtain success from replacement context"),
        }
    }
}
