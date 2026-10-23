use super::{Action, BindingRow, Bindings, Chord, RowStatus, Rows, bounded_message, prefs::Store};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool},
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TargetKey {
    None,
    Start {
        generation: u64,
        token: String,
        epoch: u64,
        cycle: u64,
        owner: String,
    },
    Transition {
        session: String,
        revision: u64,
        owner: String,
    },
    Stop {
        session: String,
    },
}
#[derive(Clone)]
pub(super) struct Target {
    pub key: TargetKey,
    pub stop: Option<Arc<AtomicBool>>,
}
impl Target {
    pub fn none() -> Self {
        Self {
            key: TargetKey::None,
            stop: None,
        }
    }
}
#[derive(Clone)]
pub(super) struct Entry {
    pub id: i32,
    pub action: Action,
    pub chord: Chord,
    pub target: Target,
    pub ready: bool,
}
pub(super) struct RegisterError {
    pub conflict: bool,
    pub message: String,
}
pub(super) trait Registrar {
    fn register(&mut self, id: i32, chord: &Chord) -> Result<(), RegisterError>;
    fn unregister(&mut self, id: i32) -> Result<(), String>;
    fn primary_released(&self, chord: &Chord) -> Result<bool, String>;
    fn all_released(&self) -> Result<bool, String>;
}
pub(super) struct Registry<R: Registrar> {
    pub registrar: R,
    pub configured: Bindings,
    pub entries: BTreeMap<Action, Entry>,
    pub blocked: BTreeMap<Action, (Chord, RowStatus, String)>,
    next_id: i32,
}
impl<R: Registrar> Registry<R> {
    pub fn new(registrar: R, configured: Bindings) -> Self {
        Self {
            registrar,
            configured,
            entries: BTreeMap::new(),
            blocked: BTreeMap::new(),
            next_id: 0,
        }
    }
    fn id(&mut self) -> Result<i32, String> {
        if self.next_id >= 0xbfff {
            return Err(
                "Shortcut ID capacity exhausted; restart the app before assigning again.".into(),
            );
        }
        self.next_id += 1;
        Ok(self.next_id)
    }
    pub fn retire(&mut self, action: Action) -> Result<(), String> {
        if let Some(mut e) = self.entries.remove(&action) {
            e.ready = false;
            e.target = Target::none();
            if let Err(error) = self.registrar.unregister(e.id) {
                self.blocked.insert(
                    action,
                    (
                        e.chord.clone(),
                        RowStatus::CleanupFailed,
                        bounded_message(&error),
                    ),
                );
                self.entries.insert(action, e);
                return Err(error);
            }
        }
        Ok(())
    }
    pub fn disable_all(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for a in Action::ALL {
            if let Err(e) = self.retire(a) {
                errors.push(e)
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(bounded_message(errors.join("; ")))
        }
    }
    fn refresh_one(&mut self, a: Action, target: Target, suppressed: bool) {
        if self
            .blocked
            .get(&a)
            .is_some_and(|(_, s, _)| *s == RowStatus::CleanupFailed)
            && self.entries.contains_key(&a)
        {
            return;
        }
        let chord = self.configured.get(a).cloned();
        if self.entries.get(&a).is_some_and(|e| {
            Some(&e.chord) == chord.as_ref() && e.target.key == target.key && !suppressed
        }) {
            if let Some(e) = self.entries.get_mut(&a)
                && !e.ready
            {
                e.ready = self.registrar.primary_released(&e.chord).unwrap_or(false);
            }
            return;
        }
        if self.retire(a).is_err() {
            return;
        }
        if suppressed {
            return;
        }
        let Some(chord) = chord else { return };
        if self.blocked.get(&a).is_some_and(|(c, _, _)| c == &chord) {
            return;
        }
        let id = match self.id() {
            Ok(i) => i,
            Err(e) => {
                self.blocked.insert(a, (chord, RowStatus::Unavailable, e));
                return;
            }
        };
        match self.registrar.register(id, &chord) {
            Ok(()) => {
                let ready = self.registrar.primary_released(&chord).unwrap_or(false);
                self.entries.insert(
                    a,
                    Entry {
                        id,
                        action: a,
                        chord,
                        target,
                        ready,
                    },
                );
            }
            Err(e) => {
                self.blocked.insert(
                    a,
                    (
                        chord,
                        if e.conflict {
                            RowStatus::Conflict
                        } else {
                            RowStatus::Unavailable
                        },
                        bounded_message(e.message),
                    ),
                );
            }
        }
    }
    pub fn refresh(&mut self, targets: &BTreeMap<Action, Target>, suppressed: bool) {
        for a in Action::ALL {
            self.refresh_one(
                a,
                targets.get(&a).cloned().unwrap_or_else(Target::none),
                suppressed,
            )
        }
    }
    pub fn rows(&self, suppressed: bool) -> Rows {
        let row = |a: Action| {
            let configured = self.configured.get(a).cloned();
            let e = self.entries.get(&a);
            let registered = e.map(|e| e.chord.clone());
            let dispatch_enabled =
                !suppressed && e.is_some_and(|e| e.ready && e.target.key != TargetKey::None);
            let (status, reason) = if let Some((c, s, r)) =
                self.blocked.get(&a).filter(|(c, _, _)| {
                    Some(c) == configured.as_ref() || e.is_some_and(|e| &e.chord == c)
                }) {
                let _ = c;
                (*s, Some(r.clone()))
            } else if suppressed && (configured.is_some() || registered.is_some()) {
                (
                    RowStatus::Suppressed,
                    Some("Shortcut dispatch is suppressed during assignment.".into()),
                )
            } else if registered.is_some() {
                (
                    RowStatus::Registered,
                    if e.is_some_and(|e| !e.ready) {
                        Some("Release the shortcut key before using it.".into())
                    } else {
                        None
                    },
                )
            } else if configured.is_some() {
                (
                    RowStatus::Unavailable,
                    Some("Shortcut registration is unavailable.".into()),
                )
            } else {
                (RowStatus::Unset, None)
            };
            BindingRow {
                configured,
                registered,
                dispatch_enabled,
                status,
                reason,
            }
        };
        Rows {
            start: row(Action::Start),
            pause: row(Action::Pause),
            resume: row(Action::Resume),
            stop: row(Action::Stop),
        }
    }
    pub fn apply(
        &mut self,
        candidate: Bindings,
        store: &mut dyn Store,
        targets: &BTreeMap<Action, Target>,
    ) -> Result<(), String> {
        candidate.validate()?;
        let previous = self.configured.clone();
        self.disable_all()?;
        self.blocked.clear();
        self.configured = candidate;
        self.refresh(targets, false);
        let registered_all = Action::ALL
            .iter()
            .all(|a| self.configured.get(*a).is_none() || self.entries.contains_key(a));
        let result = if registered_all {
            store.save(&self.configured)
        } else {
            Err("A shortcut could not be registered. Previous explicit bindings were restored where possible.".into())
        };
        if let Err(error) = result {
            let cleanup = self.disable_all();
            self.configured = previous;
            if cleanup.is_ok() {
                self.blocked.clear();
                self.refresh(targets, false);
            } else {
                for e in self.entries.values_mut() {
                    e.ready = false;
                    e.target = Target::none();
                }
            }
            return Err(match cleanup {
                Ok(()) => error,
                Err(e) => format!("{error} Cleanup failed: {e}"),
            });
        }
        Ok(())
    }
    pub fn reset(&mut self, store: &mut dyn Store) -> Result<(), String> {
        // Reset never registers, even when persistence or unregistration fails.
        let cleanup = self.disable_all();
        self.configured = Bindings::default();
        let saved = store.save(&self.configured);
        match (cleanup, saved) {
            (Ok(()), Ok(())) => {
                self.blocked.clear();
                Ok(())
            }
            (Err(a), Err(b)) => Err(format!("{a}; {b}")),
            (Err(a), _) | (_, Err(a)) => Err(a),
        }
    }
}
