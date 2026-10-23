//! Bounded helper transition mailbox; permanent Stop remains a separate atomic.
use serde::{Deserialize, Serialize};
use std::sync::{
    Mutex,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transition {
    pub operation_id: String,
    pub control_revision: u64,
    pub pause_revision: u64,
    #[serde(default)]
    pub sealed_sequence: u64,
}
#[derive(Default)]
struct State {
    pending: Option<(String, Transition)>,
    last: Option<(String, Transition)>,
    paused: bool,
    control_revision: u64,
    pause_revision: u64,
}
#[derive(Default)]
pub struct Controls {
    state: Mutex<State>,
    elapsed: AtomicU64,
    phase: AtomicU8,
    intent: AtomicU8,
}
impl Controls {
    pub fn is_duplicate(&self, action: &str, value: &Transition) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        [s.pending.as_ref(), s.last.as_ref()]
            .into_iter()
            .flatten()
            .any(|old| old.0 == action && old.1 == *value)
    }
    pub fn hud_state(&self) -> (u64, u8) {
        (
            self.elapsed.load(Ordering::Acquire),
            self.phase.load(Ordering::Acquire),
        )
    }
    pub fn update_hud(&self, elapsed_ms: u64, phase: u8) {
        self.elapsed.store(elapsed_ms, Ordering::Release);
        self.phase.store(phase, Ordering::Release);
    }
    pub fn hud_intent(&self) {
        let phase = self.phase.load(Ordering::Acquire);
        if phase == 0 || phase == 2 {
            let _ = self.intent.compare_exchange(
                0,
                if phase == 0 { 1 } else { 2 },
                Ordering::AcqRel,
                Ordering::Acquire,
            );
        }
    }
    pub fn take_intent(&self) -> Option<&'static str> {
        match self.intent.swap(0, Ordering::AcqRel) {
            1 => Some("pause"),
            2 => Some("resume"),
            _ => None,
        }
    }
    pub fn submit(&self, action: &str, value: Transition) -> Result<(), String> {
        if !matches!(action, "pause" | "resume")
            || value.operation_id.len() != 32
            || !value
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || value.control_revision > 9_007_199_254_740_991
            || value.pause_revision == 0
            || value.pause_revision > 9_007_199_254_740_991
        {
            return Err("Invalid transition control.".into());
        }
        let mut s = self
            .state
            .lock()
            .map_err(|_| "Transition controls unavailable.")?;
        for old in [s.pending.as_ref(), s.last.as_ref()].into_iter().flatten() {
            if old.1.operation_id == value.operation_id {
                return if old.0 == action && old.1 == value {
                    Ok(())
                } else {
                    Err("Conflicting transition identity.".into())
                };
            }
        }
        if s.pending.is_some() || s.paused != (action == "resume") {
            return Err("Transition is not admitted in this phase.".into());
        }
        if value.control_revision <= s.control_revision
            || value.pause_revision
                != if action == "pause" {
                    s.pause_revision
                        .checked_add(1)
                        .ok_or("Pause revision exhausted.")?
                } else {
                    s.pause_revision
                }
        {
            return Err("Stale transition revision.".into());
        }
        s.control_revision = value.control_revision;
        s.pause_revision = value.pause_revision;
        s.pending = Some((action.into(), value));
        Ok(())
    }
    pub fn take(&self, action: &str) -> Option<Transition> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.pending.as_ref()?.0 != action {
            return None;
        }
        let (action, value) = s.pending.take()?;
        s.last = Some((action, value.clone()));
        Some(value)
    }
    pub fn paused(&self, value: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).paused = value;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn transition(id: char, revision: u64, pause: u64) -> Transition {
        Transition {
            operation_id: id.to_string().repeat(32),
            control_revision: revision,
            pause_revision: pause,
            sealed_sequence: 0,
        }
    }
    #[test]
    fn forgotten_old_pause_cannot_rearm_after_multiple_cycles() {
        let c = Controls::default();
        let first = transition('a', 2, 1);
        for (pause, resume, revision, n) in [('a', 'b', 2, 1), ('c', 'd', 4, 2)] {
            let request = transition(pause, revision, n);
            c.submit("pause", request.clone()).unwrap();
            c.submit("pause", request).unwrap();
            c.take("pause").unwrap();
            c.paused(true);
            c.submit("resume", transition(resume, revision + 1, n))
                .unwrap();
            c.take("resume").unwrap();
            c.paused(false);
        }
        assert!(c.submit("pause", first).is_err());
        assert!(c.take("pause").is_none());
        c.submit("pause", transition('e', 6, 3)).unwrap();
    }
}
