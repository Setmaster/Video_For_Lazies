//! Exact-session transition admission. Stop never waits for a transition slot.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const TRANSITION_WAIT: Duration = Duration::from_secs(5);
#[derive(Clone, Default)]
pub(crate) struct ControlContext {
    pub owner_token: Option<String>,
    pub session_id: Option<String>,
    pub phase: String,
    pub control_revision: u64,
    pub pause_revision: u64,
    pub resolved_role_mask: u8,
    pub stop: Option<Arc<AtomicBool>>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransitionRequest {
    pub owner_token: String,
    pub session_id: String,
    pub operation_id: String,
    pub expected_control_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconcile_fence_token: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct TransitionTimeout {
    pub session_id: String,
    pub operation_id: String,
    pub control_revision: u64,
    pub code: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PendingTransition {
    pub operation_id: String,
    pub action: String,
    pub control_revision: u64,
}
#[derive(Clone)]
struct Operation {
    pause_revision: u64,
    request: TransitionRequest,
    action: String,
    admitted: Instant,
    sent: bool,
    sent_at: Option<Instant>,
    outcome: Option<Result<(), String>>,
}
impl Operation {
    fn expired(&self) -> bool {
        if self.action == "resume"
            && let Some(sent) = self.sent_at
        {
            return sent.elapsed() >= Duration::from_secs(8);
        }
        self.admitted.elapsed() >= TRANSITION_WAIT
    }
    fn timeout_code(&self) -> &'static str {
        if self.action == "resume" && !self.sent {
            "resumeTimeout"
        } else {
            "transitionTimeout"
        }
    }
}
#[derive(Default)]
struct Inner {
    context: ControlContext,
    current: Option<Operation>,
    retired: VecDeque<(Operation, Instant)>,
}
#[derive(Default)]
pub(crate) struct SessionControls {
    inner: Mutex<Inner>,
}
pub(crate) fn token(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn new_operation_id() -> Result<String, String> {
    use std::{
        collections::hash_map::RandomState,
        hash::{BuildHasher, Hasher},
        sync::{OnceLock, atomic::AtomicU64},
    };
    static SEED: OnceLock<u64> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seed = *SEED.get_or_init(|| {
        let mut h = RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        h.finish()
    });
    let next = NEXT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(1).filter(|n| *n <= SAFE_INTEGER)
        })
        .map_err(|_| "counterExhausted")?
        + 1;
    Ok(format!("{seed:016x}{next:016x}"))
}
impl SessionControls {
    pub(crate) fn replace_owner_option(&self, owner: Option<String>) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.context.owner_token != owner {
            if i.context.control_revision == SAFE_INTEGER {
                if let Some(stop) = &i.context.stop {
                    stop.store(true, Ordering::Release);
                }
                i.context.owner_token = None;
                return;
            }
            i.context.control_revision += 1;
            if i.current.as_ref().is_some_and(|op| op.action == "resume") {
                if i.current.as_ref().is_some_and(|op| op.sent)
                    && let Some(stop) = &i.context.stop
                {
                    stop.store(true, Ordering::Release);
                }
                i.current = None;
            }
            i.context.owner_token = owner;
        }
    }
    pub(crate) fn context(&self) -> ControlContext {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .context
            .clone()
    }
    #[cfg(test)]
    pub(crate) fn replace_owner(&self, owner: String) -> Result<(), String> {
        if !token(&owner) {
            return Err("invalidRequest".into());
        }
        self.replace_owner_option(Some(owner));
        Ok(())
    }
    pub(crate) fn set_session(
        &self,
        session: &str,
        stop: Arc<AtomicBool>,
        roles: u8,
    ) -> Result<(), String> {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.context.control_revision >= SAFE_INTEGER {
            return Err("counterExhausted".into());
        }
        i.current = None;
        i.context.session_id = Some(session.into());
        i.context.phase = "preparing".into();
        i.context.control_revision += 1;
        i.context.pause_revision = 0;
        i.context.resolved_role_mask = roles;
        i.context.stop = Some(stop);
        Ok(())
    }
    pub(crate) fn phase(&self, session: &str, phase: &str) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.context.session_id.as_deref() == Some(session) {
            i.context.phase = phase.into();
            if matches!(
                phase,
                "stopping" | "finalizing" | "completed" | "failed" | "recoverable" | "idle"
            ) {
                if let Some(op) = i.current.take() {
                    i.retired.push_back((op, Instant::now()));
                }
                while i.retired.len() > 16 {
                    i.retired.pop_front();
                }
            }
        }
    }
    pub(crate) fn known_request(
        &self,
        request: &TransitionRequest,
        action: &str,
    ) -> Result<bool, String> {
        if !token(&request.owner_token)
            || !token(&request.operation_id)
            || request.session_id.is_empty()
            || request.session_id.len() > 80
            || !request
                .session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || request
                .reconcile_fence_token
                .as_ref()
                .is_some_and(|f| !token(f))
            || request.expected_control_revision > SAFE_INTEGER
            || !matches!(action, "pause" | "resume")
        {
            return Err("invalidRequest".into());
        }
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        i.retired
            .retain(|(_, at)| at.elapsed() < Duration::from_secs(120));
        if i.context.owner_token.as_deref() != Some(&request.owner_token) {
            return Err("staleOwner".into());
        }
        if i.context.session_id.as_deref() != Some(&request.session_id) {
            return Err("staleSession".into());
        }
        let old = i
            .current
            .as_ref()
            .filter(|op| op.request.operation_id == request.operation_id)
            .or_else(|| {
                i.retired
                    .iter()
                    .find(|(o, _)| o.request.operation_id == request.operation_id)
                    .map(|(o, _)| o)
            });
        match old {
            Some(old) if old.request != *request || old.action != action => {
                Err("identityConflict".into())
            }
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }
    pub(crate) fn admit(
        &self,
        request: TransitionRequest,
        action: &str,
    ) -> Result<PendingTransition, String> {
        if !token(&request.owner_token)
            || !token(&request.operation_id)
            || request.session_id.is_empty()
            || request.session_id.len() > 80
            || !request
                .session_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || request
                .reconcile_fence_token
                .as_ref()
                .is_some_and(|f| !token(f))
            || request.expected_control_revision > SAFE_INTEGER
            || !matches!(action, "pause" | "resume")
        {
            return Err("invalidRequest".into());
        }
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        i.retired
            .retain(|(_, at)| at.elapsed() < Duration::from_secs(120));
        if i.context.owner_token.as_deref() != Some(&request.owner_token) {
            return Err("staleOwner".into());
        }
        if i.context.session_id.as_deref() != Some(&request.session_id) {
            return Err("staleSession".into());
        }
        if i.context
            .stop
            .as_ref()
            .is_none_or(|s| s.load(Ordering::Acquire))
        {
            return Err("sessionTerminal".into());
        }
        if let Some(old) = i
            .current
            .as_ref()
            .filter(|op| op.request.operation_id == request.operation_id)
            .or_else(|| {
                i.retired
                    .iter()
                    .find(|(o, _)| o.request.operation_id == request.operation_id)
                    .map(|(o, _)| o)
            })
        {
            if old.request != request || old.action != action {
                return Err("identityConflict".into());
            }
            return Ok(PendingTransition {
                operation_id: request.operation_id,
                action: action.into(),
                control_revision: old.request.expected_control_revision + 1,
            });
        }
        if i.current.is_some() {
            return Err("transitionBusy".into());
        }
        if i.context.control_revision != request.expected_control_revision {
            return Err("staleAdmission".into());
        }
        if i.context.phase
            != if action == "pause" {
                "recording"
            } else {
                "paused"
            }
        {
            return Err("staleAdmission".into());
        }
        if i.context.control_revision == SAFE_INTEGER || i.context.pause_revision == SAFE_INTEGER {
            return Err("counterExhausted".into());
        }
        i.context.control_revision += 1;
        if action == "pause" {
            i.context.pause_revision += 1;
            i.context.phase = "pausing".into();
        }
        let pending = PendingTransition {
            operation_id: request.operation_id.clone(),
            action: action.into(),
            control_revision: i.context.control_revision,
        };
        i.current = Some(Operation {
            pause_revision: i.context.pause_revision,
            request,
            action: action.into(),
            admitted: Instant::now(),
            sent: false,
            sent_at: None,
            outcome: None,
        });
        Ok(pending)
    }
    pub(crate) fn control(&self, fence_ready: bool, sealed_sequence: u64) -> Option<Value> {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.context
            .stop
            .as_ref()
            .is_none_or(|s| s.load(Ordering::Acquire))
        {
            return None;
        }
        let operation = i.current.as_mut()?;
        let revision = operation.request.expected_control_revision + 1;
        let pause_revision = operation.pause_revision;
        if operation.expired() || operation.sent || (operation.action == "resume" && !fence_ready) {
            return None;
        }
        operation.sent = true;
        operation.sent_at = Some(Instant::now());
        Some(
            serde_json::json!({"operation":operation.action,"value":{"operationId":operation.request.operation_id,"controlRevision":revision,"pauseRevision":pause_revision,"sealedSequence":sealed_sequence}}),
        )
    }
    pub(crate) fn unsent(&self, operation_id: &str) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(op) = i
            .current
            .as_mut()
            .filter(|o| o.request.operation_id == operation_id)
        {
            op.sent = false;
            op.sent_at = None;
        }
    }
    pub(crate) fn is_pending(&self, operation_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .as_ref()
            .is_some_and(|op| op.request.operation_id == operation_id)
    }
    pub(crate) fn write_permitted(&self, operation_id: &str) -> bool {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        !i.context
            .stop
            .as_ref()
            .is_none_or(|s| s.load(Ordering::Acquire))
            && i.current.as_ref().is_some_and(|op| {
                op.request.operation_id == operation_id && op.sent && !op.expired()
            })
    }
    pub(crate) fn complete(&self, operation_id: &str, phase: &str) -> bool {
        self.complete_result(operation_id, phase, Ok(()))
    }
    pub(crate) fn complete_result(
        &self,
        operation_id: &str,
        phase: &str,
        result: Result<(), String>,
    ) -> bool {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.current
            .as_ref()
            .is_none_or(|o| o.request.operation_id != operation_id)
        {
            return false;
        }
        if i.current.as_ref().is_some_and(Operation::expired) {
            let mut op = i.current.take().unwrap();
            op.outcome = Some(Err(op.timeout_code().into()));
            if op.timeout_code() == "transitionTimeout"
                && let Some(stop) = &i.context.stop
            {
                stop.store(true, Ordering::Release);
            }
            i.retired.push_back((op, Instant::now()));
            while i.retired.len() > 16 {
                i.retired.pop_front();
            }
            return false;
        }
        if i.context
            .stop
            .as_ref()
            .is_some_and(|s| s.load(Ordering::Acquire))
        {
            return false;
        }
        let mut op = i.current.take().unwrap();
        op.outcome = Some(result);
        i.context.phase = phase.into();
        i.retired.push_back((op, Instant::now()));
        while i.retired.len() > 16 {
            i.retired.pop_front();
        }
        true
    }
    pub(crate) fn timed_out(&self) -> Option<TransitionTimeout> {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let op = i.current.as_ref()?;
        if !op.expired() {
            return None;
        }
        let mut op = i.current.take().unwrap();
        let code = op.timeout_code();
        op.outcome = Some(Err(code.into()));
        if code == "transitionTimeout"
            && let Some(stop) = &i.context.stop
        {
            stop.store(true, Ordering::Release);
        }
        let timeout = TransitionTimeout {
            session_id: op.request.session_id.clone(),
            operation_id: op.request.operation_id.clone(),
            control_revision: op.request.expected_control_revision + 1,
            code: code.into(),
        };
        i.retired.push_back((op, Instant::now()));
        while i.retired.len() > 16 {
            i.retired.pop_front();
        }
        Some(timeout)
    }
    pub(crate) fn outcome(&self, operation_id: &str) -> Result<Option<()>, String> {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.current
            .as_ref()
            .is_some_and(|op| op.request.operation_id == operation_id)
        {
            return Ok(None);
        }
        i.retired
            .iter()
            .find(|(op, _)| op.request.operation_id == operation_id)
            .ok_or_else(|| "staleUnknown".to_string())?
            .0
            .outcome
            .clone()
            .unwrap_or(Err("sessionTerminal".into()))
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> (SessionControls, Arc<AtomicBool>) {
        let controls = SessionControls::default();
        controls.replace_owner("a".repeat(32)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        controls.set_session("session", stop.clone(), 1).unwrap();
        controls.phase("session", "recording");
        (controls, stop)
    }
    fn request(controls: &SessionControls, id: char) -> TransitionRequest {
        TransitionRequest {
            owner_token: "a".repeat(32),
            session_id: "session".into(),
            operation_id: id.to_string().repeat(32),
            expected_control_revision: controls.context().control_revision,
            reconcile_fence_token: None,
        }
    }
    #[test]
    fn duplicates_never_rearm_completed_operation_or_replace_current() {
        let (c, _) = ready();
        let pause = request(&c, 'b');
        c.admit(pause.clone(), "pause").unwrap();
        assert!(c.control(true, 64).is_some());
        assert!(c.control(true, 64).is_none());
        assert!(c.complete(&pause.operation_id, "paused"));
        let resume = request(&c, 'c');
        c.admit(resume.clone(), "resume").unwrap();
        c.admit(pause.clone(), "pause").unwrap();
        assert!(!c.is_pending(&pause.operation_id));
        assert!(c.is_pending(&resume.operation_id));
        let mut changed = pause;
        changed.expected_control_revision += 1;
        assert_eq!(c.admit(changed, "pause").unwrap_err(), "identityConflict");
    }
    #[test]
    fn expired_resume_cannot_be_revived_by_late_fence_or_reply() {
        let (c, stop) = ready();
        let pause = request(&c, 'b');
        c.admit(pause.clone(), "pause").unwrap();
        c.complete(&pause.operation_id, "paused");
        let resume = request(&c, 'c');
        c.admit(resume.clone(), "resume").unwrap();
        assert!(c.control(false, 0).is_none());
        c.inner.lock().unwrap().current.as_mut().unwrap().admitted =
            Instant::now() - Duration::from_secs(6);
        // The transport pumps before its timeout sweep. A newly ready fence
        // must not move an already expired operation onto an acquisition budget.
        assert!(c.control(true, 0).is_none());
        assert_eq!(c.timed_out().unwrap().code, "resumeTimeout");
        assert!(!stop.load(Ordering::Acquire));
        assert!(c.control(true, 0).is_none());
        assert!(!c.complete(&resume.operation_id, "recording"));
        c.admit(resume, "resume").unwrap();
        assert!(c.control(true, 0).is_none());
    }
    #[test]
    fn stop_and_owner_replacement_fence_acquisition_without_losing_pause() {
        let (c, stop) = ready();
        let pause = request(&c, 'b');
        c.admit(pause.clone(), "pause").unwrap();
        c.replace_owner_option(Some("d".repeat(32)));
        assert!(c.complete(&pause.operation_id, "paused"));
        assert!(!stop.load(Ordering::Acquire));
        let mut resume = request(&c, 'c');
        resume.owner_token = "d".repeat(32);
        c.admit(resume, "resume").unwrap();
        c.control(true, 0).unwrap();
        c.replace_owner_option(Some("e".repeat(32)));
        assert!(stop.load(Ordering::Acquire));
        assert!(c.control(true, 0).is_none());
    }
    #[test]
    fn late_transition_completion_cannot_publish_or_extend_deadline() {
        let (c, stop) = ready();
        let pause = request(&c, 'b');
        c.admit(pause.clone(), "pause").unwrap();
        c.control(true, 0).unwrap();
        c.inner.lock().unwrap().current.as_mut().unwrap().admitted =
            Instant::now() - Duration::from_secs(6);
        assert!(!c.complete(&pause.operation_id, "paused"));
        assert!(stop.load(Ordering::Acquire));
        assert!(!c.is_pending(&pause.operation_id));
    }
}
