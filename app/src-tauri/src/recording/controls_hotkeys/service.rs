use super::*;
use registry::{Registrar, Registry, Target, TargetKey};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

impl Shared {
    pub(super) fn wake(&self) {
        if let Ok(c) = self.commands.lock()
            && let Some(tx) = c.as_ref()
        {
            let _ = tx.try_send(WorkerCommand::Refresh);
        }
    }
    pub(super) fn targets(g: &gate::ArmGate) -> BTreeMap<Action, Target> {
        let mut result = BTreeMap::new();
        let inactive = g.stopped || g.assignment_suppressed || g.assignment.is_some();
        // Start/owner fences never retire the active exact-session Stop target.
        if !g.stopped
            && let Some(s) = &g.session
        {
            result.insert(
                Action::Stop,
                Target {
                    key: TargetKey::Stop {
                        session: s.session_id.clone(),
                    },
                    stop: Some(s.stop.clone()),
                },
            );
        }
        if !inactive {
            if g.session.is_none()
                && g.attempt.is_none()
                && let (Some(a), Some(owner)) = (&g.arm, &g.owner)
                && ((a.mode == ArmMode::Leased && g.window == NativeWindow::Visible)
                    || (a.mode == ArmMode::Retained
                        && g.window == NativeWindow::Minimized
                        && g.observer_live))
            {
                result.insert(
                    Action::Start,
                    Target {
                        key: TargetKey::Start {
                            generation: g.start_generation,
                            token: a.token.clone(),
                            epoch: g.invalidation_epoch,
                            cycle: g.minimize_cycle,
                            owner: owner.clone(),
                        },
                        stop: None,
                    },
                );
            }
            if let Some(s) = &g.session
                && let Some(owner) = g.owner.as_ref()
            {
                let target = Target {
                    key: TargetKey::Transition {
                        session: s.session_id.clone(),
                        revision: s.control_revision,
                        owner: owner.clone(),
                    },
                    stop: None,
                };
                if s.can_pause {
                    result.insert(Action::Pause, target.clone());
                }
                if s.can_resume {
                    result.insert(Action::Resume, target);
                }
            }
        }
        result
    }
    pub(super) fn emit(&self) {
        let app = self.app.lock().ok().and_then(|a| a.clone());
        if let Some(app) = app
            && let Some(manager) = app.try_state::<ControlsManager>()
        {
            let _ = app.emit("recording-controls-state", manager.snapshot());
        }
    }
    pub(super) fn publish<R: Registrar>(&self, r: &Registry<R>) {
        if let Ok(mut g) = self.gate.lock()
            && let Ok(mut e) = self.effects.lock()
        {
            let rows = r.rows(g.stopped || (g.session.is_none() && g.assignment_suppressed));
            let changed = serde_json::to_vec(&rows).ok() != serde_json::to_vec(&e.rows).ok()
                || r.configured != e.bindings;
            e.rows = rows;
            e.bindings = r.configured.clone();
            e.entries = r.entries.clone();
            if changed {
                g.bump();
            }
        }
        if let Ok(mut entries) = self.hotkeys.lock() {
            *entries = r.entries.values().map(|e| (e.id, e.clone())).collect();
        }
    }
    pub(super) fn current_entry(g: &gate::ArmGate, entry: &registry::Entry) -> bool {
        Self::targets(g)
            .get(&entry.action)
            .is_some_and(|t| t.key == entry.target.key)
    }
    pub(super) fn key(&self, id: i32, mods: u32, key: u32) {
        let Some(entry) = self.hotkeys.lock().ok().and_then(|e| e.get(&id).cloned()) else {
            return;
        };
        if !entry.ready
            || entry.chord.modifier_bits() != mods
            || entry.chord.virtual_key().ok() != Some(key)
        {
            return;
        }
        // A registration captures a permanent exact-session latch. Even an old
        // queued Stop can only stop that old session, never a newer one. It
        // must not wait for the Start fence, manager, or transition worker.
        if entry.action == Action::Stop {
            if let Some(stop) = entry.target.stop {
                stop.store(true, Ordering::Release);
            }
            return;
        }
        let Ok(mut g) = self.gate.lock() else { return };
        g.expire(Instant::now());
        if !Self::current_entry(&g, &entry) {
            return;
        }
        drop(g);
        let action = entry.action;
        let target = entry.target.key.clone();
        let overflow = if let Ok(mut queue) = self.pending.lock() {
            if queue.len() < 16 {
                queue.push_back(entry);
                false
            } else {
                true
            }
        } else {
            true
        };
        // The queue lock is released before the fence lock. A stale overflow
        // cannot replace feedback belonging to a new arm/session.
        if overflow
            && let Ok(mut g) = self.gate.lock()
            && Self::targets(&g)
                .get(&action)
                .is_some_and(|t| t.key == target)
        {
            g.message =
                Some("Shortcut dispatch is busy. Press the shortcut again when ready.".into());
            g.bump();
        }
    }
}
impl ControlsManager {
    pub(crate) fn snapshot(&self) -> ControlsSnapshot {
        let mut g = self.shared.gate.lock().unwrap_or_else(|p| p.into_inner());
        g.expire(Instant::now());
        let e = self
            .shared
            .effects
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let targets = Shared::targets(&g);
        let mut rows = e.rows.clone();
        for (a, row) in [
            (Action::Start, &mut rows.start),
            (Action::Pause, &mut rows.pause),
            (Action::Resume, &mut rows.resume),
            (Action::Stop, &mut rows.stop),
        ] {
            row.dispatch_enabled &= e
                .entries
                .get(&a)
                .is_some_and(|entry| targets.get(&a).is_some_and(|t| t.key == entry.target.key));
        }
        ControlsSnapshot {
            revision: g.revision,
            owner_token: g.owner.clone(),
            invalidation_epoch: g.invalidation_epoch,
            minimize_cycle: g.minimize_cycle,
            bindings: e.bindings.clone(),
            rows,
            assignment: g.assignment_snapshot(Instant::now()),
            arm: g.arm_snapshot(Instant::now()),
            native_window: g.window,
            message: g.message.clone(),
        }
    }
    pub(super) fn success(&self) -> ControlsReply {
        ControlsReply::Ok {
            ok: true,
            value: self.snapshot(),
        }
    }
    pub(super) fn failure(&self, code: &str, message: impl AsRef<str>) -> ControlsReply {
        ControlsReply::Err {
            ok: false,
            error: ControlsFailure {
                code: code.into(),
                message: bounded_message(message),
            },
            state: self.snapshot(),
        }
    }
    pub(crate) fn validate_owner(&self, owner: &str) -> Result<(), String> {
        self.shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?
            .owner(owner)
    }
    pub(crate) fn invalidate_start(&self, reason: &str) {
        if let Ok(mut g) = self.shared.gate.lock() {
            g.invalidate(reason);
        }
        self.shared.wake();
    }
    pub(crate) fn page_load_started(&self) {
        if let Ok(mut g) = self.shared.gate.lock() {
            g.invalidate("Renderer navigation requires a new control owner.");
            g.owner = None;
            g.renderer_instance = None;
            g.assignment = None;
            g.assignment_suppressed = true;
        }
        let backend = self.shared.backend.lock().ok().and_then(|b| b.clone());
        if let Some(b) = backend {
            b.replace_owner(None);
        }
        self.shared.wake();
    }
    pub(crate) fn set_session(&self, target: Option<SessionTarget>) {
        if let Ok(mut g) = self.shared.gate.lock() {
            let before = g
                .session
                .as_ref()
                .map(|s| (&s.session_id, s.control_revision, s.can_pause, s.can_resume));
            let after = target
                .as_ref()
                .map(|s| (&s.session_id, s.control_revision, s.can_pause, s.can_resume));
            let changed = before != after;
            if let Some(t) = target.as_ref()
                && let Some(a) = g.attempt.as_mut().filter(|a| a.admitted)
                && a.session_id.is_none()
            {
                a.session_id = Some(t.session_id.clone());
            }
            g.session = target;
            if changed {
                g.bump();
            }
        }
        self.shared.wake();
    }
    pub(crate) fn helper_hud_visible(&self, attempt_token: &str, session_id: &str) {
        if let Ok(tx) = self.shared.commands.lock()
            && let Some(tx) = tx.as_ref()
        {
            let _ = tx.try_send(WorkerCommand::HudVisible {
                attempt: attempt_token.into(),
                session: session_id.into(),
            });
        }
    }
    pub(crate) fn fail_attempt(&self, attempt_token: &str, reason: &str) {
        let exact = if let Ok(mut g) = self.shared.gate.lock() {
            if g.attempt.as_ref().is_some_and(|a| a.token == attempt_token) {
                g.attempt = None;
                let _ = g.advance_start();
                g.message = Some(bounded_message(reason));
                g.bump();
                true
            } else {
                false
            }
        } else {
            false
        };
        if exact {
            if let Ok(tx) = self.shared.commands.lock()
                && let Some(tx) = tx.as_ref()
            {
                let _ = tx.try_send(WorkerCommand::Finish {
                    attempt: attempt_token.into(),
                });
            }
            self.shared.wake();
        }
    }
    pub(crate) fn finish_attempt(&self, attempt_token: &str) {
        if let Ok(mut g) = self.shared.gate.lock()
            && g.attempt.as_ref().is_some_and(|a| a.token == attempt_token)
        {
            g.attempt = None;
            let _ = g.advance_start();
            g.bump();
        }
        if let Ok(tx) = self.shared.commands.lock()
            && let Some(tx) = tx.as_ref()
        {
            let _ = tx.try_send(WorkerCommand::Finish {
                attempt: attempt_token.into(),
            });
        }
        self.shared.wake();
    }
    pub(crate) fn signal_shutdown(&self) {
        self.shared.shutting_down.store(true, Ordering::Release);
        if let Ok(mut g) = self.shared.gate.lock() {
            g.stopped = true;
            g.invalidate("Application is closing.");
            if let Some(a) = &g.attempt {
                a.cancel.store(true, Ordering::Release);
            }
            if let Some(s) = &g.session {
                s.stop.store(true, Ordering::Release);
            }
        }
        self.shared.wake();
    }
    pub(crate) fn shutdown(&self) -> Result<(), String> {
        self.signal_shutdown();
        let worker = self
            .shared
            .worker
            .lock()
            .map_err(|_| "Shortcut worker lock unavailable.")?
            .take();
        let result = if let Some(w) = worker {
            w.join()
                .map_err(|_| "Shortcut owner thread panicked.".to_string())
                .and_then(|r| r)
        } else {
            Ok(())
        };
        // Break AppState -> service -> backend/AppHandle ownership after owner settlement.
        if let Ok(mut b) = self.shared.backend.lock() {
            b.take();
        }
        if let Ok(mut a) = self.shared.app.lock() {
            a.take();
        }
        if let Ok(mut c) = self.shared.commands.lock() {
            c.take();
        }
        #[cfg(windows)]
        {
            if self
                .shared
                .gate
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?
                .observer_live
            {
                return Err("Main-window observer removal has not been confirmed.".into());
            }
        }
        if let Some(error) = self
            .shared
            .cleanup_error
            .lock()
            .map_err(|_| "Shortcut cleanup state unavailable.")?
            .take()
        {
            return Err(error);
        }
        result
    }
    pub(crate) fn initialize(
        &self,
        app: AppHandle,
        main: &WebviewWindow,
        backend: Arc<dyn ControlsBackend>,
    ) -> Result<(), String> {
        *self
            .shared
            .app
            .lock()
            .map_err(|_| "Shortcut state unavailable.")? = Some(app.clone());
        *self
            .shared
            .backend
            .lock()
            .map_err(|_| "Shortcut state unavailable.")? = Some(backend);
        let directory = match app.path().app_config_dir() {
            Ok(path) => path,
            Err(e) => {
                let error = format!("Shortcut settings directory unavailable: {e}");
                if let Ok(mut g) = self.shared.gate.lock() {
                    g.invalidate(&error);
                    g.window = NativeWindow::Unavailable;
                }
                return Err(error);
            }
        };
        let store = prefs::FileStore::new(&directory);
        let (bindings, message) = store.load();
        {
            let mut g = self
                .shared
                .gate
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?;
            if g.initialized {
                return Err("Shortcut service already initialized.".into());
            }
            g.initialized = true;
            g.message = message;
        }
        {
            let mut effects = self
                .shared
                .effects
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?;
            let row = |action| {
                let configured = bindings.get(action).cloned();
                BindingRow{status:if configured.is_some(){RowStatus::Unavailable}else{RowStatus::Unset},reason:configured.as_ref().map(|_|"Shortcut registration is unavailable until native initialization completes.".into()),configured,registered:None,dispatch_enabled:false}
            };
            effects.rows = Rows {
                start: row(Action::Start),
                pause: row(Action::Pause),
                resume: row(Action::Resume),
                stop: row(Action::Stop),
            };
            effects.bindings = bindings.clone();
        }
        #[cfg(windows)]
        {
            let result = windows::initialize(self.clone(), main, bindings, store);
            if let Err(e) = &result
                && let Ok(mut g) = self.shared.gate.lock()
            {
                g.invalidate(e);
                g.window = NativeWindow::Unavailable;
            }
            result
        }
        #[cfg(not(windows))]
        {
            let _ = (main, store);
            let mut e = self
                .shared
                .effects
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?;
            let rows = &mut e.rows;
            for (action, row) in [
                (Action::Start, &mut rows.start),
                (Action::Pause, &mut rows.pause),
                (Action::Resume, &mut rows.resume),
                (Action::Stop, &mut rows.stop),
            ] {
                row.configured = bindings.get(action).cloned();
                if row.configured.is_some() {
                    row.status = RowStatus::Unavailable;
                    row.reason = Some("Global recording shortcuts require Windows.".into());
                }
            }
            e.bindings = bindings;
            drop(e);
            if let Ok(mut g) = self.shared.gate.lock() {
                g.message = Some("Global recording shortcuts are available on Windows.".into());
            }
            Ok(())
        }
    }
    pub(super) async fn handle(&self, request: ControlsRequest) -> Result<ControlsReply, String> {
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            > 24_576
        {
            return Ok(self.failure("tooLarge", "Shortcut request is too large."));
        }
        if let ControlsRequest::PublishDraft { .. } = request {
            return self.publish(request).await;
        }
        let sender = self
            .shared
            .commands
            .lock()
            .map_err(|_| "Shortcut command channel unavailable.")?
            .clone();
        if let Some(sender) = sender {
            let (tx, rx) = mpsc::channel();
            if sender
                .try_send(WorkerCommand::Request(Box::new(request), tx))
                .is_err()
            {
                return Ok(self.failure("busy", "Shortcut command queue is busy."));
            }
            let this = self.clone();
            return tauri::async_runtime::spawn_blocking(move || {
                rx.recv_timeout(Duration::from_secs(5)).unwrap_or_else(|_| {
                    this.failure(
                        "unavailable",
                        "Shortcut command did not settle; refresh the current state.",
                    )
                })
            })
            .await
            .map_err(|e| e.to_string());
        }
        Ok(self.common(request).unwrap_or_else(|_| {
            self.failure(
                "unavailable",
                "Global shortcuts are unavailable; all dispatch remains disabled.",
            )
        }))
    }
    pub(super) fn common(
        &self,
        request: ControlsRequest,
    ) -> Result<ControlsReply, Box<ControlsRequest>> {
        match request {
            ControlsRequest::Snapshot => Ok(self.success()),
            ControlsRequest::AttachOwner {
                expected_revision,
                expected_owner_token,
                renderer_instance_id,
            } => {
                if !valid_token(&renderer_instance_id) {
                    return Ok(self.failure("invalid", "Renderer identity is invalid."));
                }
                let result = (|| {
                    let mut g = self
                        .shared
                        .gate
                        .lock()
                        .map_err(|_| "Shortcut state unavailable.")?;
                    if g.renderer_instance.as_deref() == Some(&renderer_instance_id)
                        && (g.attach_expected == expected_owner_token
                            || g.owner == expected_owner_token)
                    {
                        return Ok(None);
                    }
                    if expected_revision != g.revision || g.owner != expected_owner_token {
                        return Err("Control owner changed; refresh before attaching.");
                    }
                    let token = g.token().map_err(|_| "Control token capacity exhausted.")?;
                    g.invalidate("New renderer owner requires a fresh Record draft.");
                    g.owner = Some(token.clone());
                    g.renderer_instance = Some(renderer_instance_id);
                    g.attach_expected = expected_owner_token;
                    g.client_revision = 0;
                    if self
                        .shared
                        .commands
                        .lock()
                        .map_err(|_| "Shortcut channel unavailable.")?
                        .is_none()
                    {
                        g.assignment_suppressed = false;
                    }
                    Ok(Some(token))
                })();
                match result {
                    Ok(owner) => {
                        if let Some(owner) = owner {
                            let b = self.shared.backend.lock().ok().and_then(|b| b.clone());
                            if let Some(b) = b {
                                b.replace_owner(Some(owner));
                            }
                        }
                        self.shared.wake();
                        Ok(self.success())
                    }
                    Err(e) => Ok(self.failure("staleOwner", e)),
                }
            }
            ControlsRequest::InvalidateDraft {
                owner_token,
                client_revision,
            } => {
                let result = (|| {
                    let mut g = self
                        .shared
                        .gate
                        .lock()
                        .map_err(|_| "Shortcut state unavailable.")?;
                    g.owner(&owner_token)
                        .map_err(|_| "Control owner changed.")?;
                    if client_revision == 0
                        || client_revision > MAX_REVISION
                        || client_revision <= g.client_revision
                    {
                        return Err("Draft revision is stale.");
                    }
                    g.client_revision = client_revision;
                    g.invalidate("Record configuration or view changed.");
                    Ok(())
                })();
                self.shared.wake();
                Ok(match result {
                    Ok(()) => self.success(),
                    Err(e) => self.failure("staleRevision", e),
                })
            }
            ControlsRequest::RenewDraft {
                owner_token,
                arm_token,
                config_revision,
            } => {
                let r = self
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.".to_string())
                    .and_then(|mut g| {
                        g.renew_arm(&owner_token, &arm_token, config_revision, Instant::now())
                    });
                Ok(match r {
                    Ok(()) => self.success(),
                    Err(e) => self.failure("staleArm", e),
                })
            }
            ControlsRequest::ClassifyVisibility { owner_token } => {
                if let Err(e) = self.validate_owner(&owner_token) {
                    return Ok(self.failure("staleOwner", e));
                }
                #[cfg(windows)]
                {
                    if let Ok(mut g) = self.shared.gate.lock() {
                        let actual = windows::classify(g.hwnd);
                        g.observe(actual, Instant::now());
                    }
                }
                self.shared.wake();
                Ok(self.success())
            }
            other => Err(Box::new(other)),
        }
    }
    pub(super) fn normalize(
        &self,
        owner: &str,
        assignment: &str,
        event: KeyCapture,
    ) -> Result<Chord, String> {
        let mut g = self
            .shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        g.expire(Instant::now());
        g.owner(owner)?;
        if !g
            .assignment
            .as_ref()
            .is_some_and(|a| a.token == assignment && a.owner == owner && !a.capture_closed)
            || g.window != NativeWindow::Visible
        {
            return Err("Shortcut capture lease is unavailable.".into());
        }
        let ui_thread = g.ui_thread;
        drop(g);
        if event.key.len() > 64 {
            return Err("Unsupported shortcut key.".into());
        }
        #[cfg(windows)]
        {
            windows::normalize(ui_thread, event)
        }
        #[cfg(not(windows))]
        {
            let _ = (ui_thread, event);
            Err("Global shortcuts require Windows.".into())
        }
    }
    async fn publish(&self, request: ControlsRequest) -> Result<ControlsReply, String> {
        let ControlsRequest::PublishDraft {
            owner_token,
            invalidation_epoch,
            minimize_cycle,
            expected_arm_token,
            client_revision,
            config_revision,
            draft,
        } = request
        else {
            unreachable!()
        };
        let start = (|| -> Result<(u64, RecordingConfig), String> {
            let mut g = self
                .shared
                .gate
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?;
            g.expire(Instant::now());
            g.owner(&owner_token)?;
            if g.window != NativeWindow::Visible
                || !g.initialized
                || g.stopped
                || g.assignment_suppressed
                || g.assignment.is_some()
                || g.session.is_some()
            {
                return Err("A fresh native-visible Record draft is unavailable now.".into());
            }
            if g.invalidation_epoch != invalidation_epoch
                || g.minimize_cycle != minimize_cycle
                || g.arm.as_ref().map(|a| a.token.clone()) != expected_arm_token
                || client_revision <= g.client_revision
                || client_revision > MAX_REVISION
                || config_revision == 0
                || config_revision > MAX_REVISION
            {
                return Err("Draft context changed; reconcile before publishing.".into());
            }
            g.client_revision = client_revision;
            g.invalidate("Validating the selected Record draft.");
            let epoch = g.invalidation_epoch;
            Ok((epoch, draft.config("draft-validation".into())))
        })();
        let (epoch, config) = match start {
            Ok(x) => x,
            Err(e) => return Ok(self.failure("staleDraft", e)),
        };
        self.shared.wake();
        let backend = self
            .shared
            .backend
            .lock()
            .map_err(|_| "Shortcut backend unavailable.")?
            .clone();
        let Some(backend) = backend else {
            return Ok(self.failure("unavailable", "Recording controls are unavailable."));
        };
        let resolved = tauri::async_runtime::spawn_blocking(move || backend.validate_draft(config))
            .await
            .map_err(|e| e.to_string())?;
        let resolved = match resolved {
            Ok(c) => c,
            Err(e) => return Ok(self.failure("invalidDraft", e)),
        };
        let commit = (|| -> Result<(), String> {
            let mut g = self
                .shared
                .gate
                .lock()
                .map_err(|_| "Shortcut state unavailable.")?;
            g.owner(&owner_token)?;
            if g.invalidation_epoch != epoch
                || g.minimize_cycle != minimize_cycle
                || g.client_revision != client_revision
                || g.window != NativeWindow::Visible
                || g.assignment_suppressed
                || g.stopped
                || g.session.is_some()
                || g.attempt.is_some()
            {
                return Err("Draft validation completed after its context was retired.".into());
            }
            let token = g.token()?;
            g.arm = Some(gate::DraftArm {
                token,
                config_revision,
                config: resolved,
                mode: ArmMode::Leased,
                expires: Some(Instant::now() + Duration::from_millis(LEASE_MS)),
            });
            g.bump();
            Ok(())
        })();
        self.shared.wake();
        Ok(match commit {
            Ok(()) => self.success(),
            Err(e) => self.failure("staleDraft", e),
        })
    }
}

/// Owner-thread mutation lane. Exact operation receipts precede revision CAS;
/// evicted operations cannot replay because their original revision is stale.
pub(super) struct Mutations {
    receipts: VecDeque<Receipt>,
}
struct Receipt {
    owner: String,
    id: String,
    payload: Vec<u8>,
    error: Option<ControlsFailure>,
    at: Instant,
}
impl Mutations {
    pub fn new() -> Self {
        Self {
            receipts: VecDeque::new(),
        }
    }
    pub fn execute<R: Registrar>(
        &mut self,
        m: &ControlsManager,
        r: &mut Registry<R>,
        store: &mut dyn prefs::Store,
        request: ControlsRequest,
    ) -> ControlsReply {
        let request = match m.common(request) {
            Ok(reply) => return reply,
            Err(request) => *request,
        };
        let operation = match &request {
            ControlsRequest::BeginAssignment {
                owner_token,
                operation_id,
                ..
            }
            | ControlsRequest::ApplyBindings {
                owner_token,
                operation_id,
                ..
            }
            | ControlsRequest::Reset {
                owner_token,
                operation_id,
                ..
            } => Some((owner_token.clone(), operation_id.clone())),
            _ => None,
        };
        let bytes = serde_json::to_vec(&request).unwrap_or_default();
        self.receipts
            .retain(|e| e.at.elapsed() < Duration::from_secs(120));
        if let Some((owner, id)) = &operation {
            if let Err(e) = m.validate_owner(owner) {
                return m.failure("staleOwner", e);
            }
            if !valid_token(id) {
                return m.failure("invalid", "Shortcut operation identity is invalid.");
            }
            if let Some(receipt) = self
                .receipts
                .iter()
                .find(|e| e.owner == *owner && e.id == *id)
            {
                if receipt.payload != bytes {
                    return m.failure(
                        "operationConflict",
                        "Operation identity was reused with different arguments.",
                    );
                }
                return match &receipt.error {
                    Some(e) => m.failure(&e.code, &e.message),
                    None => m.success(),
                };
            }
        }
        let result = self.apply(m, r, store, request);
        m.shared.publish(r);
        let reply = match result {
            Ok(()) => m.success(),
            Err(e) => m.failure("assignment", e),
        };
        if let Some((owner, id)) = operation {
            let error = match &reply {
                ControlsReply::Err { error, .. } => Some(error.clone()),
                _ => None,
            };
            if self.receipts.len() >= 16 {
                self.receipts.pop_front();
            }
            self.receipts.push_back(Receipt {
                owner,
                id,
                payload: bytes,
                error,
                at: Instant::now(),
            });
        }
        reply
    }
    fn apply<R: Registrar>(
        &mut self,
        m: &ControlsManager,
        r: &mut Registry<R>,
        store: &mut dyn prefs::Store,
        request: ControlsRequest,
    ) -> Result<(), String> {
        match request {
            ControlsRequest::BeginAssignment {
                expected_revision,
                owner_token,
                operation_id: _,
            } => {
                let backend = m
                    .shared
                    .backend
                    .lock()
                    .map_err(|_| "Recording backend unavailable.")?
                    .clone()
                    .ok_or("Recording backend unavailable.")?;
                {
                    let mut g = m
                        .shared
                        .gate
                        .lock()
                        .map_err(|_| "Shortcut state unavailable.")?;
                    g.expire(Instant::now());
                    g.owner(&owner_token)?;
                    if g.revision != expected_revision {
                        return Err("Shortcut state changed; refresh before assigning.".into());
                    }
                    g.invalidate("Start retired before shortcut assignment.");
                }
                // No ArmGate held while inspecting manager state; recheck admission below.
                if !backend.is_idle() {
                    return Err("Stop recording before assigning shortcuts.".into());
                }
                {
                    let mut g = m
                        .shared
                        .gate
                        .lock()
                        .map_err(|_| "Shortcut state unavailable.")?;
                    g.owner(&owner_token)?;
                    if g.stopped
                        || g.window != NativeWindow::Visible
                        || g.session.is_some()
                        || g.attempt.as_ref().is_some_and(|a| a.admitted)
                    {
                        return Err("Shortcut assignment is unavailable now.".into());
                    }
                    g.assignment = None;
                    g.assignment_suppressed = true;
                }
                r.disable_all()?;
                let mut g = m
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.")?;
                g.owner(&owner_token)?;
                if g.window != NativeWindow::Visible || g.stopped {
                    return Err("Window changed during shortcut suppression.".into());
                }
                let token = g.token()?;
                g.assignment = Some(gate::Assignment {
                    token,
                    owner: owner_token,
                    expires: Instant::now() + Duration::from_millis(LEASE_MS),
                    capture_closed: false,
                });
                g.bump();
                Ok(())
            }
            ControlsRequest::RenewAssignment {
                owner_token,
                assignment_token,
            } => {
                let mut g = m
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.")?;
                g.expire(Instant::now());
                g.owner(&owner_token)?;
                if g.window != NativeWindow::Visible || g.stopped {
                    return Err("Shortcut assignment is no longer visible.".into());
                }
                let a = g
                    .assignment
                    .as_mut()
                    .filter(|a| a.token == assignment_token && a.owner == owner_token)
                    .ok_or("Shortcut assignment expired.")?;
                a.expires = Instant::now() + Duration::from_millis(LEASE_MS);
                Ok(())
            }
            ControlsRequest::ApplyBindings {
                expected_revision,
                owner_token,
                assignment_token,
                capture_closed,
                bindings,
                ..
            } => {
                bindings.validate()?;
                self.close_capture(
                    m,
                    expected_revision,
                    &owner_token,
                    &assignment_token,
                    capture_closed,
                )?;
                if !r.registrar.all_released()? {
                    return Err("Release all shortcut keys before saving.".into());
                }
                // Assignment has proven input quiescent. No Start target is restored.
                let targets = BTreeMap::new();
                let result = r.apply(bindings, store, &targets);
                let mut g = m
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.")?;
                g.assignment = None;
                g.assignment_suppressed = false;
                g.bump();
                result
            }
            ControlsRequest::EndAssignment {
                expected_revision,
                owner_token,
                assignment_token,
                capture_closed,
            } => {
                self.close_capture(
                    m,
                    expected_revision,
                    &owner_token,
                    &assignment_token,
                    capture_closed,
                )?;
                if !r.registrar.all_released()? {
                    return Err("Release all shortcut keys before ending assignment.".into());
                }
                let mut g = m
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.")?;
                g.assignment = None;
                g.assignment_suppressed = false;
                g.bump();
                Ok(())
            }
            ControlsRequest::Reset {
                expected_revision,
                owner_token,
                ..
            } => {
                {
                    let mut g = m
                        .shared
                        .gate
                        .lock()
                        .map_err(|_| "Shortcut state unavailable.")?;
                    g.owner(&owner_token)?;
                    if g.revision != expected_revision {
                        return Err("Shortcut state changed; refresh before reset.".into());
                    }
                    if g.session.is_some() || g.attempt.as_ref().is_some_and(|a| a.admitted) {
                        return Err("Stop recording before resetting shortcuts.".into());
                    }
                    g.invalidate("Shortcut bindings reset.");
                    g.assignment = None;
                    g.assignment_suppressed = true;
                }
                let result = r.reset(store);
                let mut g = m
                    .shared
                    .gate
                    .lock()
                    .map_err(|_| "Shortcut state unavailable.")?;
                g.assignment_suppressed = false;
                g.bump();
                result
            }
            _ => Err("Unsupported shortcut mutation.".into()),
        }
    }
    fn close_capture(
        &self,
        m: &ControlsManager,
        revision: u64,
        owner: &str,
        assignment: &str,
        closed: bool,
    ) -> Result<(), String> {
        let mut g = m
            .shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        g.expire(Instant::now());
        g.owner(owner)?;
        if g.revision != revision || g.window != NativeWindow::Visible || g.stopped || !closed {
            return Err("Close shortcut capture and refresh the current assignment first.".into());
        }
        let a = g
            .assignment
            .as_mut()
            .filter(|a| a.token == assignment && a.owner == owner)
            .ok_or("Shortcut assignment expired.")?;
        a.capture_closed = true;
        Ok(())
    }
    pub fn settle_assignment<R: Registrar>(&self, m: &ControlsManager, r: &Registry<R>) {
        let waiting = m
            .shared
            .gate
            .lock()
            .map(|mut g| {
                g.expire(Instant::now());
                g.assignment_suppressed && g.assignment.is_none()
            })
            .unwrap_or(false);
        if waiting
            && r.registrar.all_released().unwrap_or(false)
            && let Ok(mut g) = m.shared.gate.lock()
            && g.assignment.is_none()
        {
            g.assignment_suppressed = false;
            g.bump();
        }
    }
}
