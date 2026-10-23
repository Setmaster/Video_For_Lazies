use super::{
    ArmMode, ArmSnapshot, AssignmentSnapshot, LEASE_MS, MAX_REVISION, NativeWindow,
    RecordingConfig, SessionTarget, Shared, bounded_message, valid_token,
};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) struct DraftArm {
    pub token: String,
    pub config_revision: u64,
    pub config: RecordingConfig,
    pub mode: ArmMode,
    pub expires: Option<Instant>,
}
pub(super) struct Assignment {
    pub token: String,
    pub owner: String,
    pub expires: Instant,
    pub capture_closed: bool,
}
pub(super) struct AttemptState {
    pub token: String,
    pub session_id: Option<String>,
    pub cancel: Arc<AtomicBool>,
    pub admitted: bool,
    pub hud_visible: bool,
}
pub(super) struct ArmGate {
    pub revision: u64,
    pub owner: Option<String>,
    pub renderer_instance: Option<String>,
    pub attach_expected: Option<String>,
    pub invalidation_epoch: u64,
    pub minimize_cycle: u64,
    pub client_revision: u64,
    pub arm: Option<DraftArm>,
    pub assignment: Option<Assignment>,
    pub assignment_suppressed: bool,
    pub window: NativeWindow,
    pub hwnd: usize,
    pub ui_thread: u32,
    pub observer_live: bool,
    pub session: Option<SessionTarget>,
    pub attempt: Option<AttemptState>,
    pub message: Option<String>,
    pub initialized: bool,
    pub stopped: bool,
    pub start_generation: u64,
    seed: u64,
    next: u64,
}
impl Default for ArmGate {
    fn default() -> Self {
        let mut h = RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        Self {
            revision: 0,
            owner: None,
            renderer_instance: None,
            attach_expected: None,
            invalidation_epoch: 0,
            minimize_cycle: 0,
            client_revision: 0,
            arm: None,
            assignment: None,
            assignment_suppressed: false,
            window: NativeWindow::Unavailable,
            hwnd: 0,
            ui_thread: 0,
            observer_live: false,
            session: None,
            attempt: None,
            message: None,
            initialized: false,
            stopped: false,
            start_generation: 0,
            seed: h.finish(),
            next: 0,
        }
    }
}
impl ArmGate {
    pub fn token(&mut self) -> Result<String, String> {
        self.next = self
            .next
            .checked_add(1)
            .filter(|v| *v <= MAX_REVISION)
            .ok_or("Shortcut token capacity exhausted.")?;
        Ok(format!("{:016x}{:016x}", self.seed, self.next))
    }
    pub fn advance_start(&mut self) -> Result<(), String> {
        let Some(next) = self
            .start_generation
            .checked_add(1)
            .filter(|n| *n <= MAX_REVISION)
        else {
            self.arm = None;
            let error =
                "Start context capacity exhausted; restart before using Start again.".to_string();
            self.message = Some(error.clone());
            self.bump();
            return Err(error);
        };
        self.start_generation = next;
        Ok(())
    }
    pub fn bump(&mut self) {
        if let Some(n) = self.revision.checked_add(1).filter(|v| *v <= MAX_REVISION) {
            self.revision = n;
        } else {
            self.arm = None;
            self.message = Some("Shortcut revision capacity exhausted; Start disabled.".into());
        }
    }
    pub fn owner(&self, owner: &str) -> Result<(), String> {
        if !valid_token(owner) || self.owner.as_deref() != Some(owner) {
            Err("Shortcut owner changed; refresh controls.".into())
        } else {
            Ok(())
        }
    }
    pub fn invalidate(&mut self, reason: &str) {
        if let Some(p) = self.attempt.as_ref().filter(|p| !p.admitted) {
            p.cancel.store(true, Ordering::Release);
        }
        self.arm = None;
        self.invalidation_epoch = self
            .invalidation_epoch
            .checked_add(1)
            .filter(|n| *n <= MAX_REVISION)
            .unwrap_or(MAX_REVISION);
        let _ = self.advance_start();
        self.message = Some(bounded_message(reason));
        self.bump();
    }
    pub fn expire(&mut self, now: Instant) {
        if self
            .arm
            .as_ref()
            .is_some_and(|a| a.mode == ArmMode::Leased && a.expires.is_some_and(|e| e <= now))
        {
            self.invalidate("Start draft expired; reconcile the current Record settings.");
        }
        if self.assignment.as_ref().is_some_and(|a| a.expires <= now) {
            self.assignment = None;
            self.assignment_suppressed = true;
            self.invalidate("Shortcut assignment expired; waiting for key-release cleanup.");
        }
    }
    pub fn observe(&mut self, window: NativeWindow, now: Instant) {
        self.expire(now);
        let previous = self.window;
        self.window = window;
        if previous == window {
            return;
        }
        if window == NativeWindow::Minimized {
            self.minimize_cycle = self
                .minimize_cycle
                .checked_add(1)
                .filter(|n| *n <= MAX_REVISION)
                .unwrap_or(MAX_REVISION);
            if self.assignment.is_some() {
                self.assignment = None;
                self.assignment_suppressed = true;
                self.invalidate("Shortcut assignment ended when the window minimized.");
                return;
            }
            if !self.observer_live || self.stopped {
                return;
            }
            let eligible = self
                .arm
                .as_ref()
                .is_some_and(|a| a.mode == ArmMode::Leased && a.expires.is_some_and(|e| e > now));
            if eligible {
                match self.token() {
                    Ok(token) => {
                        if let Some(a) = self.arm.as_mut() {
                            a.token = token;
                            a.mode = ArmMode::Retained;
                            a.expires = None;
                        }
                        if self.advance_start().is_err() {
                            return;
                        }
                        self.bump();
                    }
                    Err(e) => self.invalidate(&e),
                }
            }
        } else if previous == NativeWindow::Minimized
            || matches!(window, NativeWindow::Hidden | NativeWindow::Unavailable)
        {
            self.minimize_cycle = self
                .minimize_cycle
                .checked_add(1)
                .filter(|n| *n <= MAX_REVISION)
                .unwrap_or(MAX_REVISION);
            self.invalidate("Window changed; reconcile a fresh Record draft.");
        }
    }
    pub fn arm_snapshot(&self, now: Instant) -> Option<ArmSnapshot> {
        self.arm.as_ref().map(|a| ArmSnapshot {
            token: a.token.clone(),
            config_revision: a.config_revision,
            mode: a.mode,
            expires_in_ms: a
                .expires
                .map(|e| e.saturating_duration_since(now).as_millis() as u64),
        })
    }
    pub fn assignment_snapshot(&self, now: Instant) -> Option<AssignmentSnapshot> {
        self.assignment.as_ref().map(|a| AssignmentSnapshot {
            token: a.token.clone(),
            expires_in_ms: a.expires.saturating_duration_since(now).as_millis() as u64,
        })
    }
    pub fn renew_arm(
        &mut self,
        owner: &str,
        token: &str,
        revision: u64,
        now: Instant,
    ) -> Result<(), String> {
        self.expire(now);
        self.owner(owner)?;
        if self.window != NativeWindow::Visible {
            return Err("Only an ordinary visible-window draft can renew.".into());
        }
        let arm = self
            .arm
            .as_mut()
            .filter(|a| {
                a.token == token && a.config_revision == revision && a.mode == ArmMode::Leased
            })
            .ok_or("Start draft changed or expired.")?;
        arm.expires = Some(now + Duration::from_millis(LEASE_MS));
        Ok(())
    }
}
#[derive(Clone)]
pub(crate) struct StartPermit {
    pub(super) shared: Weak<Shared>,
    pub(super) token: String,
    pub(super) arm_token: String,
    pub(super) epoch: u64,
    pub(super) cycle: u64,
    pub(super) owner: Option<String>,
    pub(super) ui: bool,
    pub(super) consumed: Arc<AtomicBool>,
    pub(super) cancel: Arc<AtomicBool>,
}
impl StartPermit {
    pub(crate) fn admit<T>(&self, commit: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let shared = self.shared.upgrade().ok_or("Shortcut service closed.")?;
        let mut g = shared
            .gate
            .lock()
            .map_err(|_| "Shortcut state unavailable.")?;
        #[cfg(windows)]
        {
            let actual = super::windows::classify(g.hwnd);
            g.observe(actual, Instant::now());
        }
        g.expire(Instant::now());
        if shared.shutting_down.load(Ordering::Acquire)
            || self.cancel.load(Ordering::Acquire)
            || g.stopped
            || g.assignment_suppressed
            || g.assignment.is_some()
            || g.owner != self.owner
            || g.invalidation_epoch != self.epoch
            || g.minimize_cycle != self.cycle
        {
            return Err("Start context was retired.".into());
        }
        if !self.ui {
            let a = g
                .arm
                .as_ref()
                .filter(|a| a.token == self.arm_token)
                .ok_or("Start draft retired.")?;
            if (a.mode == ArmMode::Retained && g.window != NativeWindow::Minimized)
                || (a.mode == ArmMode::Leased && g.window != NativeWindow::Visible)
            {
                return Err("Start window state changed.".into());
            }
        }
        if g.session.is_some()
            || !g
                .attempt
                .as_ref()
                .is_some_and(|p| p.token == self.token && p.hud_visible && !p.admitted)
        {
            return Err("Start admission is no longer available.".into());
        }
        let next_epoch = g
            .invalidation_epoch
            .checked_add(1)
            .filter(|n| *n <= MAX_REVISION)
            .ok_or("Start context capacity exhausted.")?;
        self.consumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Start permit already consumed.")?;
        if let Some(attempt) = g.attempt.as_mut() {
            attempt.admitted = true;
        }
        let result = commit();
        if result.is_ok() {
            g.invalidation_epoch = next_epoch;
            g.bump();
        }
        if result.is_err()
            && let Some(p) = g.attempt.as_mut().filter(|p| p.token == self.token)
        {
            p.admitted = false;
        }
        result
    }
}
