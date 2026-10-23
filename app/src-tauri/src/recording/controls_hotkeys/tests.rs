use super::*;
use prefs::Store;
use registry::*;
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};
fn chord(key: &str) -> Chord {
    Chord {
        modifiers: Modifiers::CtrlShift,
        key: key.into(),
    }
}
#[derive(Default)]
struct Fake {
    registered: BTreeMap<i32, Chord>,
    calls: usize,
    conflict: Option<Chord>,
    unregister_failure: bool,
    released: bool,
}
impl Registrar for Fake {
    fn register(&mut self, id: i32, chord: &Chord) -> Result<(), RegisterError> {
        self.calls += 1;
        if self.conflict.as_ref() == Some(chord) {
            return Err(RegisterError {
                conflict: true,
                message: "owned test conflict".into(),
            });
        }
        if self.registered.values().any(|c| c == chord) {
            return Err(RegisterError {
                conflict: true,
                message: "duplicate".into(),
            });
        }
        self.registered.insert(id, chord.clone());
        Ok(())
    }
    fn unregister(&mut self, id: i32) -> Result<(), String> {
        if self.unregister_failure {
            return Err("owned test cleanup failure".into());
        }
        self.registered.remove(&id);
        Ok(())
    }
    fn primary_released(&self, _: &Chord) -> Result<bool, String> {
        Ok(self.released)
    }
    fn all_released(&self) -> Result<bool, String> {
        Ok(self.released)
    }
}
#[derive(Default)]
struct Memory {
    saved: Vec<Bindings>,
    fail: bool,
}
impl Store for Memory {
    fn save(&mut self, b: &Bindings) -> Result<(), String> {
        if self.fail {
            Err("owned test persistence failure".into())
        } else {
            self.saved.push(b.clone());
            Ok(())
        }
    }
}
fn registrar() -> Fake {
    Fake {
        released: true,
        ..Default::default()
    }
}
fn owner(m: &ControlsManager) -> String {
    let mut g = m.shared.gate.lock().unwrap();
    let token = g.token().unwrap();
    g.owner = Some(token.clone());
    token
}
fn config() -> RecordingConfig {
    serde_json::from_value(serde_json::json!({"requestId":"test","source":{"kind":"window","windowId":"opaque-test"},"fps":30,"quality":"balanced","countdownSeconds":0,"showCursor":true,"outputDirectory":"owned-fixture"})).unwrap()
}
fn arm(g: &mut gate::ArmGate, now: Instant) {
    let token = g.token().unwrap();
    g.arm = Some(gate::DraftArm {
        token,
        config_revision: 1,
        config: config(),
        mode: ArmMode::Leased,
        expires: Some(now + Duration::from_millis(LEASE_MS)),
    });
}
fn context(m: &ControlsManager) -> UiStartContext {
    let g = m.shared.gate.lock().unwrap();
    UiStartContext {
        owner_token: g.owner.clone().unwrap(),
        invalidation_epoch: g.invalidation_epoch,
        minimize_cycle: g.minimize_cycle,
    }
}
#[test]
fn missing_legacy_corrupt_oversized_nonregular_preferences_never_register() {
    let temp = tempfile::tempdir().unwrap();
    let store = prefs::FileStore::new(temp.path());
    let path = temp.path().join("recording-controls.json");
    for bytes in [
        None,
        Some(b"{\"start\":\"Ctrl+Shift+F10\"}".to_vec()),
        Some(b"{bad".to_vec()),
        Some(vec![b' '; 4097]),
    ] {
        if let Some(bytes) = bytes {
            std::fs::write(&path, &bytes).unwrap();
            let original = std::fs::read(&path).unwrap();
            let (b, message) = store.load();
            assert!(message.is_some());
            assert!(b.empty());
            assert_eq!(std::fs::read(&path).unwrap(), original);
        }
        let (b, _) = store.load();
        let mut r = Registry::new(registrar(), b);
        r.refresh(&BTreeMap::new(), false);
        assert_eq!(r.registrar.calls, 0);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.load().0.empty());
    assert!(store.load().1.is_some());
}
#[cfg(target_os = "linux")]
#[test]
fn fifo_preferences_are_rejected_before_open() {
    use std::os::unix::ffi::OsStrExt;
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("recording-controls.json");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    unsafe extern "C" {
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> i32;
    }
    assert_eq!(unsafe { mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(prefs::FileStore::new(temp.path()).load().1.is_some());
}
#[test]
fn explicit_preferences_roundtrip_and_reset_registers_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = prefs::FileStore::new(temp.path());
    let b = Bindings {
        start: Some(chord("F9")),
        ..Default::default()
    };
    store.save(&b).unwrap();
    assert_eq!(store.load().0, b);
    let mut r = Registry::new(registrar(), b);
    r.refresh(&BTreeMap::new(), false);
    let calls = r.registrar.calls;
    r.reset(&mut store).unwrap();
    r.refresh(&BTreeMap::new(), false);
    assert_eq!(calls, r.registrar.calls);
    assert!(r.entries.is_empty());
    assert!(store.load().0.empty());
}
#[test]
fn failed_reset_never_restores_previous_keys() {
    let mut r = Registry::new(
        registrar(),
        Bindings {
            stop: Some(chord("F8")),
            ..Default::default()
        },
    );
    r.refresh(&BTreeMap::new(), false);
    let calls = r.registrar.calls;
    r.registrar.unregister_failure = true;
    assert!(
        r.reset(&mut Memory {
            fail: true,
            ..Default::default()
        })
        .is_err()
    );
    r.refresh(&BTreeMap::new(), false);
    assert_eq!(calls, r.registrar.calls);
    assert!(r.configured.empty());
    let row = r.rows(false).stop;
    assert!(!row.dispatch_enabled);
    assert_eq!(row.status, RowStatus::CleanupFailed);
    assert!(row.registered.is_some());
}
#[test]
fn duplicates_and_reserved_keys_are_rejected_before_any_os_call() {
    let mut r = Registry::new(registrar(), Bindings::default());
    assert!(
        r.apply(
            Bindings {
                start: Some(chord("A")),
                stop: Some(chord("A")),
                ..Default::default()
            },
            &mut Memory::default(),
            &BTreeMap::new()
        )
        .is_err()
    );
    for key in ["F12", "F0", "F25", "f1", "F01", "Escape", "a"] {
        assert!(chord(key).validate().is_err());
    }
    assert_eq!(r.registrar.calls, 0);
}
#[test]
fn swap_unregisters_old_keys_before_registration_and_persists_only_after_success() {
    let original = Bindings {
        start: Some(chord("A")),
        stop: Some(chord("B")),
        ..Default::default()
    };
    let mut r = Registry::new(registrar(), original);
    r.refresh(&BTreeMap::new(), false);
    let old: BTreeSet<_> = r.entries.values().map(|e| e.id).collect();
    let next = Bindings {
        start: Some(chord("B")),
        stop: Some(chord("A")),
        ..Default::default()
    };
    let mut store = Memory::default();
    r.apply(next.clone(), &mut store, &BTreeMap::new()).unwrap();
    assert_eq!(store.saved, vec![next]);
    assert!(r.entries.values().all(|e| !old.contains(&e.id)));
}
#[test]
fn conflict_and_persistence_failures_rollback_explicit_bindings_with_fresh_ids() {
    for persist in [false, true] {
        let original = Bindings {
            start: Some(chord("A")),
            ..Default::default()
        };
        let mut r = Registry::new(registrar(), original.clone());
        r.refresh(&BTreeMap::new(), false);
        let old = r.entries[&Action::Start].id;
        if !persist {
            r.registrar.conflict = Some(chord("B"));
        }
        let mut store = Memory {
            fail: persist,
            ..Default::default()
        };
        assert!(
            r.apply(
                Bindings {
                    start: Some(chord("B")),
                    ..Default::default()
                },
                &mut store,
                &BTreeMap::new()
            )
            .is_err()
        );
        assert_eq!(r.configured, original);
        assert_ne!(r.entries[&Action::Start].id, old);
        assert_eq!(r.registrar.registered.len(), 1);
    }
}
#[test]
fn conflicts_are_truthful_and_not_retried_by_timer() {
    let mut fake = registrar();
    fake.conflict = Some(chord("F8"));
    let mut r = Registry::new(
        fake,
        Bindings {
            pause: Some(chord("F8")),
            ..Default::default()
        },
    );
    for _ in 0..20 {
        r.refresh(&BTreeMap::new(), false);
    }
    assert_eq!(r.registrar.calls, 1);
    assert_eq!(r.rows(false).pause.status, RowStatus::Conflict);
    assert!(r.rows(false).pause.registered.is_none());
}
#[test]
fn held_primary_key_cannot_dispatch_until_release() {
    let mut fake = registrar();
    fake.released = false;
    let mut r = Registry::new(
        fake,
        Bindings {
            stop: Some(chord("F8")),
            ..Default::default()
        },
    );
    let mut targets = BTreeMap::new();
    targets.insert(
        Action::Stop,
        Target {
            key: TargetKey::Stop {
                session: "one".into(),
            },
            stop: Some(Arc::new(AtomicBool::new(false))),
        },
    );
    r.refresh(&targets, false);
    assert!(!r.rows(false).stop.dispatch_enabled);
    r.registrar.released = true;
    r.refresh(&targets, false);
    assert!(r.rows(false).stop.dispatch_enabled);
}
#[test]
fn stop_id_survives_owner_start_and_phase_fences_and_stale_stop_never_hits_new_session() {
    let m = ControlsManager::default();
    let owner = owner(&m);
    let first = Arc::new(AtomicBool::new(false));
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.session = Some(SessionTarget {
            session_id: "first".into(),
            control_revision: 1,
            can_pause: true,
            can_resume: false,
            stop: first.clone(),
        });
    }
    let mut r = Registry::new(
        registrar(),
        Bindings {
            pause: Some(chord("F7")),
            stop: Some(chord("F8")),
            ..Default::default()
        },
    );
    r.refresh(&Shared::targets(&m.shared.gate.lock().unwrap()), false);
    let id = r.entries[&Action::Stop].id;
    m.shared.publish(&r);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.invalidate("restore");
        g.owner = None;
        g.assignment_suppressed = true;
        g.session.as_mut().unwrap().can_pause = false;
        g.session.as_mut().unwrap().can_resume = true;
        g.session.as_mut().unwrap().control_revision = 2;
    }
    r.refresh(&Shared::targets(&m.shared.gate.lock().unwrap()), false);
    assert_eq!(r.entries[&Action::Stop].id, id);
    m.shared.publish(&r);
    m.shared.key(
        id,
        chord("F8").modifier_bits(),
        chord("F8").virtual_key().unwrap(),
    );
    assert!(first.load(Ordering::Acquire));
    let second = Arc::new(AtomicBool::new(false));
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.owner = Some(owner);
        g.session = Some(SessionTarget {
            session_id: "second".into(),
            control_revision: 1,
            can_pause: true,
            can_resume: false,
            stop: second.clone(),
        });
    }
    m.shared.key(id, 6, chord("F8").virtual_key().unwrap());
    assert!(!second.load(Ordering::Acquire));
}
#[test]
fn minimized_only_promotes_live_acknowledged_draft_and_restore_retires_first() {
    let now = Instant::now();
    let mut g = gate::ArmGate::default();
    g.observer_live = true;
    g.window = NativeWindow::Visible;
    arm(&mut g, now);
    let old = g.arm.as_ref().unwrap().token.clone();
    g.observe(NativeWindow::Minimized, now + Duration::from_millis(2999));
    assert_eq!(g.arm.as_ref().unwrap().mode, ArmMode::Retained);
    assert_ne!(g.arm.as_ref().unwrap().token, old);
    g.expire(now + Duration::from_secs(100));
    assert!(g.arm.is_some());
    g.observe(NativeWindow::Visible, now + Duration::from_secs(101));
    assert!(g.arm.is_none());
    arm(&mut g, now);
    g.observe(NativeWindow::Minimized, now + Duration::from_millis(3000));
    assert!(g.arm.is_none());
}
#[test]
fn background_and_occlusion_hints_do_not_retire_ordinary_lease() {
    let now = Instant::now();
    let mut g = gate::ArmGate::default();
    g.window = NativeWindow::Visible;
    arm(&mut g, now);
    let token = g.arm.as_ref().unwrap().token.clone();
    g.observe(NativeWindow::Visible, now + Duration::from_secs(1));
    assert_eq!(g.arm.as_ref().unwrap().token, token);
    g.observe(NativeWindow::Hidden, now + Duration::from_secs(2));
    assert!(g.arm.is_none());
}
#[test]
fn old_ui_arrival_refuses_without_attempt_and_later_invalidation_fences_captured_permit() {
    let m = ControlsManager::default();
    owner(&m);
    let old = context(&m);
    m.invalidate_start("About");
    assert!(m.ui_start_permit(old).is_err());
    assert!(m.shared.gate.lock().unwrap().attempt.is_none());
    let current = context(&m);
    let permit = m.ui_start_permit(current).unwrap();
    m.invalidate_start("config");
    let called = AtomicBool::new(false);
    assert!(
        permit
            .admit(|| {
                called.store(true, Ordering::Release);
                Ok(())
            })
            .is_err()
    );
    assert!(!called.load(Ordering::Acquire));
}
#[test]
fn successful_admission_consumes_prior_ui_context_without_erasing_retained_config() {
    let m = ControlsManager::default();
    owner(&m);
    let old = context(&m);
    let permit = m.ui_start_permit(old.clone()).unwrap();
    assert!(permit.admit(|| Ok(())).is_ok());
    m.finish_attempt(permit.token());
    assert!(m.ui_start_permit(old).is_err());
    assert!(m.ui_start_permit(context(&m)).is_ok());
}
#[test]
fn attachment_duplicate_is_idempotent_before_revision_cas() {
    let m = ControlsManager::default();
    let req = ControlsRequest::AttachOwner {
        expected_revision: 0,
        expected_owner_token: None,
        renderer_instance_id: "a".repeat(32),
    };
    let first = m.common(req.clone()).unwrap();
    assert!(matches!(first, ControlsReply::Ok { .. }));
    let first_owner = m.snapshot().owner_token;
    assert!(matches!(m.common(req).unwrap(), ControlsReply::Ok { .. }));
    assert_eq!(m.snapshot().owner_token, first_owner);
}
struct IdleBackend;
impl ControlsBackend for IdleBackend {
    fn is_idle(&self) -> bool {
        true
    }
    fn validate_draft(&self, c: RecordingConfig) -> Result<RecordingConfig, String> {
        Ok(c)
    }
    fn dispatch(&self, _: ControlIntent) -> Result<(), String> {
        Err("No execution in pure fixture.".into())
    }
    fn replace_owner(&self, _: Option<String>) {}
}
#[test]
fn assignment_ack_suppresses_and_fences_then_requires_native_key_release() {
    let m = ControlsManager::default();
    let token = owner(&m);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.window = NativeWindow::Visible;
    }
    *m.shared.backend.lock().unwrap() = Some(Arc::new(IdleBackend));
    let mut r = Registry::new(
        registrar(),
        Bindings {
            start: Some(chord("F9")),
            ..Default::default()
        },
    );
    r.refresh(&BTreeMap::new(), false);
    m.shared.publish(&r);
    let old = context(&m);
    let mut lane = service::Mutations::new();
    let mut store = Memory::default();
    let req = ControlsRequest::BeginAssignment {
        expected_revision: m.snapshot().revision,
        owner_token: token.clone(),
        operation_id: "b".repeat(32),
    };
    assert!(matches!(
        lane.execute(&m, &mut r, &mut store, req.clone()),
        ControlsReply::Ok { .. }
    ));
    assert!(r.entries.is_empty());
    assert!(m.ui_start_permit(old).is_err());
    let assignment = m.snapshot().assignment.unwrap().token;
    // Lost-reply duplicate returns the same lease, without extending or rotating it.
    assert!(matches!(
        lane.execute(&m, &mut r, &mut store, req),
        ControlsReply::Ok { .. }
    ));
    assert_eq!(m.snapshot().assignment.unwrap().token, assignment);
    r.registrar.released = false;
    let end = ControlsRequest::EndAssignment {
        expected_revision: m.snapshot().revision,
        owner_token: token.clone(),
        assignment_token: assignment.clone(),
        capture_closed: true,
    };
    assert!(matches!(
        lane.execute(&m, &mut r, &mut store, end),
        ControlsReply::Err { .. }
    ));
    assert!(m.shared.gate.lock().unwrap().assignment_suppressed);
    assert!(r.entries.is_empty());
    r.registrar.released = true;
    let end = ControlsRequest::EndAssignment {
        expected_revision: m.snapshot().revision,
        owner_token: token,
        assignment_token: assignment,
        capture_closed: true,
    };
    assert!(matches!(
        lane.execute(&m, &mut r, &mut store, end),
        ControlsReply::Ok { .. }
    ));
    assert!(m.snapshot().arm.is_none());
}
#[test]
fn expired_assignment_never_reenables_until_keys_are_released() {
    let m = ControlsManager::default();
    let token = owner(&m);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.assignment_suppressed = true;
        g.assignment = Some(gate::Assignment {
            token: "c".repeat(32),
            owner: token,
            expires: Instant::now() - Duration::from_millis(1),
            capture_closed: false,
        });
    }
    let mut r = Registry::new(registrar(), Bindings::default());
    r.registrar.released = false;
    let lane = service::Mutations::new();
    lane.settle_assignment(&m, &r);
    assert!(m.shared.gate.lock().unwrap().assignment_suppressed);
    r.registrar.released = true;
    lane.settle_assignment(&m, &r);
    assert!(!m.shared.gate.lock().unwrap().assignment_suppressed);
    assert!(m.snapshot().arm.is_none());
}

#[test]
fn exact_stop_does_not_wait_for_start_gate() {
    let m = ControlsManager::default();
    let stop = Arc::new(AtomicBool::new(false));
    let entry = Entry {
        id: 1,
        action: Action::Stop,
        chord: chord("F8"),
        target: Target {
            key: TargetKey::Stop {
                session: "own".into(),
            },
            stop: Some(stop.clone()),
        },
        ready: true,
    };
    m.shared.hotkeys.lock().unwrap().insert(1, entry);
    let hold = m.shared.gate.lock().unwrap();
    let cloned = m.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::spawn(move || {
        cloned.shared.key(1, 6, chord("F8").virtual_key().unwrap());
        let _ = tx.send(());
    });
    let result = rx.recv_timeout(Duration::from_secs(1));
    drop(hold);
    thread.join().unwrap();
    assert!(result.is_ok());
    assert!(stop.load(Ordering::Acquire));
}
#[test]
fn registration_id_exhaustion_never_wraps_or_reuses() {
    let mut r = Registry::new(
        registrar(),
        Bindings {
            start: Some(chord("F9")),
            ..Default::default()
        },
    );
    for sequence in 0..=0xbfff {
        let mut targets = BTreeMap::new();
        targets.insert(
            Action::Start,
            Target {
                key: TargetKey::Start {
                    generation: sequence,
                    token: sequence.to_string(),
                    epoch: sequence,
                    cycle: 0,
                    owner: "a".repeat(32),
                },
                stop: None,
            },
        );
        r.refresh(&targets, false);
    }
    assert_eq!(r.registrar.calls, 0xbfff);
    assert!(r.entries.is_empty());
    assert_eq!(r.rows(false).start.status, RowStatus::Unavailable);
}
#[test]
fn restore_cycle_rejects_late_ui_request_before_permit_creation() {
    let m = ControlsManager::default();
    owner(&m);
    let old = context(&m);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.observe(NativeWindow::Minimized, Instant::now());
        g.observe(NativeWindow::Unavailable, Instant::now());
    }
    assert!(m.ui_start_permit(old).is_err());
    assert!(m.shared.gate.lock().unwrap().attempt.is_none());
    assert!(m.ui_start_permit(context(&m)).is_ok());
}
#[test]
fn stale_global_validation_error_cannot_overwrite_new_attempt() {
    let m = ControlsManager::default();
    owner(&m);
    let first = m.ui_start_permit(context(&m)).unwrap();
    m.finish_attempt(first.token());
    let second = m.ui_start_permit(context(&m)).unwrap();
    m.fail_attempt(first.token(), "old failure");
    assert_eq!(
        m.shared
            .gate
            .lock()
            .unwrap()
            .attempt
            .as_ref()
            .unwrap()
            .token,
        second.token()
    );
    assert_ne!(m.snapshot().message.as_deref(), Some("old failure"));
    m.fail_attempt(second.token(), "current failure");
    assert!(m.shared.gate.lock().unwrap().attempt.is_none());
    assert_eq!(m.snapshot().message.as_deref(), Some("current failure"));
}
#[test]
fn full_intent_queue_reports_busy_without_admission_and_stop_bypasses_it() {
    let m = ControlsManager::default();
    owner(&m);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.window = NativeWindow::Visible;
        arm(&mut g, Instant::now());
    }
    let target = Shared::targets(&m.shared.gate.lock().unwrap())
        .remove(&Action::Start)
        .unwrap();
    let entry = Entry {
        id: 1,
        action: Action::Start,
        chord: chord("F9"),
        target,
        ready: true,
    };
    m.shared.hotkeys.lock().unwrap().insert(1, entry.clone());
    for _ in 0..16 {
        m.shared.pending.lock().unwrap().push_back(entry.clone());
    }
    m.shared.key(1, 6, chord("F9").virtual_key().unwrap());
    assert_eq!(m.shared.pending.lock().unwrap().len(), 16);
    assert!(m.snapshot().message.unwrap().contains("busy"));
    assert!(m.shared.gate.lock().unwrap().attempt.is_none());
    let stop = Arc::new(AtomicBool::new(false));
    m.shared.hotkeys.lock().unwrap().insert(
        2,
        Entry {
            id: 2,
            action: Action::Stop,
            chord: chord("F8"),
            target: Target {
                key: TargetKey::Stop {
                    session: "owned".into(),
                },
                stop: Some(stop.clone()),
            },
            ready: true,
        },
    );
    m.shared.key(2, 6, chord("F8").virtual_key().unwrap());
    assert!(stop.load(Ordering::Acquire));
    assert_eq!(m.shared.pending.lock().unwrap().len(), 16);
    m.invalidate_start("new context");
    m.shared.key(1, 6, chord("F9").virtual_key().unwrap());
    assert_eq!(m.snapshot().message.as_deref(), Some("new context"));
}
#[test]
fn failed_preflight_retires_old_start_entries_and_requires_new_id_and_key_release() {
    let m = ControlsManager::default();
    owner(&m);
    {
        let mut g = m.shared.gate.lock().unwrap();
        g.window = NativeWindow::Visible;
        arm(&mut g, Instant::now());
    }
    let original_arm = m
        .shared
        .gate
        .lock()
        .unwrap()
        .arm
        .as_ref()
        .unwrap()
        .token
        .clone();
    let mut r = Registry::new(
        registrar(),
        Bindings {
            start: Some(chord("F9")),
            stop: Some(chord("F8")),
            ..Default::default()
        },
    );
    r.refresh(&Shared::targets(&m.shared.gate.lock().unwrap()), false);
    m.shared.publish(&r);
    let old = r.entries[&Action::Start].clone();
    let stop_id = r.entries[&Action::Stop].id;
    // Minting and failing a real service permit must not revive an older OS ID
    // or an entry already dequeued before failure. The Win32 dispatch uses this
    // same current_entry guard after resolving its actual native window state.
    let attempt = m.ui_start_permit(context(&m)).unwrap();
    assert!(!Shared::current_entry(&m.shared.gate.lock().unwrap(), &old));
    m.fail_attempt(attempt.token(), "owned preflight refusal");
    assert_eq!(
        m.shared.gate.lock().unwrap().arm.as_ref().unwrap().token,
        original_arm
    );
    assert!(!Shared::current_entry(&m.shared.gate.lock().unwrap(), &old));
    m.shared.key(old.id, 6, chord("F9").virtual_key().unwrap());
    assert!(m.shared.pending.lock().unwrap().is_empty());
    assert!(m.shared.gate.lock().unwrap().attempt.is_none());
    r.registrar.released = false;
    r.refresh(&Shared::targets(&m.shared.gate.lock().unwrap()), false);
    m.shared.publish(&r);
    let fresh = r.entries[&Action::Start].id;
    assert_ne!(fresh, old.id);
    assert_eq!(r.entries[&Action::Stop].id, stop_id);
    m.shared.key(fresh, 6, chord("F9").virtual_key().unwrap());
    assert!(m.shared.pending.lock().unwrap().is_empty());
    r.registrar.released = true;
    r.refresh(&Shared::targets(&m.shared.gate.lock().unwrap()), false);
    m.shared.publish(&r);
    assert!(m.shared.pending.lock().unwrap().is_empty());
    m.shared.key(fresh, 6, chord("F9").virtual_key().unwrap());
    assert_eq!(m.shared.pending.lock().unwrap().len(), 1);
}
#[test]
fn start_generation_exhaustion_clears_arm_and_does_not_wrap() {
    let mut g = gate::ArmGate::default();
    arm(&mut g, Instant::now());
    g.start_generation = MAX_REVISION;
    assert!(g.advance_start().is_err());
    assert_eq!(g.start_generation, MAX_REVISION);
    assert!(g.arm.is_none());
}
fn key_event(key: &str, shift: bool, alt: bool) -> KeyCapture {
    KeyCapture {
        key: key.into(),
        ctrl: true,
        shift,
        alt,
        meta: false,
    }
}
#[test]
fn single_f_uses_layout_but_function_keys_do_not() {
    let mut mapped = Vec::new();
    let result = keymap::normalize(key_event("F", true, false), |c| {
        mapped.push(c);
        Ok(0x46)
    })
    .unwrap();
    assert_eq!(mapped, vec![u16::from(b'f')]);
    assert_eq!(result.key, "F");
    assert!(
        keymap::normalize(key_event("F1", true, false), |_| panic!(
            "Function keys do not require character mapping"
        ))
        .is_ok()
    );
    assert!(
        keymap::normalize(key_event("F12", true, false), |_| panic!(
            "Reserved function key must not map as text"
        ))
        .is_err()
    );
}
#[test]
fn caps_style_letter_case_does_not_invent_shift() {
    let result = keymap::normalize(key_event("A", false, true), |c| {
        assert_eq!(c, u16::from(b'a'));
        Ok(0x41)
    })
    .unwrap();
    assert_eq!(
        result,
        Chord {
            key: "A".into(),
            modifiers: Modifiers::CtrlAlt
        }
    );
    let shifted = keymap::normalize(key_event("A", true, false), |c| {
        assert_eq!(c, u16::from(b'a'));
        Ok(0x41)
    })
    .unwrap();
    assert_eq!(shifted.modifiers, Modifiers::CtrlShift);
}
#[test]
fn shifted_symbol_maps_to_native_digit_with_exact_modifiers() {
    let result = keymap::normalize(key_event("!", true, false), |c| {
        assert_eq!(c, u16::from(b'!'));
        Ok(0x131)
    })
    .unwrap();
    assert_eq!(result.key, "1");
    assert_eq!(result.modifiers, Modifiers::CtrlShift);
    assert!(keymap::normalize(key_event("!", false, true), |_| Ok(0x131)).is_err());
}
#[test]
fn unsupported_layout_results_and_bare_keys_are_rejected() {
    for result in [-1i16, 0x831, 0x70] {
        assert!(keymap::normalize(key_event("a", true, false), |_| Ok(result)).is_err());
    }
    let mut event = key_event("a", true, false);
    event.ctrl = false;
    assert!(
        keymap::normalize(event, |_| panic!("Bare modifiers rejected before mapping")).is_err()
    );
}
