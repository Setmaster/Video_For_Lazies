//! Compact private prefix transfer. Assembly never applies PCM or starts inputs.
use super::{
    audio_controls::{self, LevelIntent, MAX_SAFE, Placement, STAGE_MS},
    audio_types::{CONTROL_BYTES, CONTROL_CAPACITY, MixLevel, bounded_id},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Begin {
    pub session_id: String,
    pub source_generation: String,
    pub pause_revision: u64,
    pub batch_token: String,
    pub attempt: u64,
    pub count: usize,
    pub prefix_digest: String,
    pub sealed_sequence: u64,
    pub accepted_sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    pub batch_token: String,
    pub attempt: u64,
    pub ordinal: usize,
    pub command_sequence: u64,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
    pub target_sample: String,
    pub placement: Placement,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Seal {
    pub batch_token: String,
    pub attempt: u64,
    pub count: usize,
    pub prefix_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "operation", content = "value", deny_unknown_fields)]
pub enum Message {
    #[serde(rename = "reconcile-begin")]
    Begin(Begin),
    #[serde(rename = "reconcile-entry")]
    Entry(Entry),
    #[serde(rename = "reconcile-seal")]
    Seal(Seal),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Ack {
    pub batch_token: String,
    pub attempt: u64,
    pub count: usize,
    pub prefix_digest: String,
}
#[derive(Clone, Debug, PartialEq)]
pub struct SealedPrefix {
    pub begin: Begin,
    pub entries: Vec<Entry>,
}
impl SealedPrefix {
    pub fn ack(&self) -> Ack {
        Ack {
            batch_token: self.begin.batch_token.clone(),
            attempt: self.begin.attempt,
            count: self.entries.len(),
            prefix_digest: self.begin.prefix_digest.clone(),
        }
    }
}
impl Entry {
    pub fn intent(&self) -> LevelIntent {
        LevelIntent {
            command_sequence: self.command_sequence,
            render_level: self.render_level,
            microphone_level: self.microphone_level,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        let mut normalized = self.intent();
        normalized
            .normalize()
            .map_err(|_| "Invalid reconciliation levels.")?;
        if !audio_controls::token(&self.batch_token)
            || self.attempt == 0
            || self.attempt > MAX_SAFE
            || self.ordinal >= CONTROL_CAPACITY
            || audio_controls::sample(&self.target_sample).is_none()
            || self.placement == Placement::AwaitingReceipt
            || normalized.render_level.gain.to_bits() != self.render_level.gain.to_bits()
            || normalized.microphone_level.gain.to_bits() != self.microphone_level.gain.to_bits()
        {
            return Err("Invalid reconciliation entry.".into());
        }
        Ok(())
    }
}
impl Begin {
    pub fn validate(&self) -> Result<(), String> {
        if !bounded_id(&self.session_id)
            || !bounded_id(&self.source_generation)
            || !audio_controls::token(&self.batch_token)
            || !audio_controls::digest(&self.prefix_digest)
            || self.pause_revision == 0
            || self.pause_revision > MAX_SAFE
            || self.attempt == 0
            || self.attempt > MAX_SAFE
            || self.count > CONTROL_CAPACITY
            || self.accepted_sequence > MAX_SAFE
            || self.sealed_sequence > self.accepted_sequence
        {
            return Err("Invalid reconciliation begin.".into());
        }
        Ok(())
    }
}
impl Seal {
    pub fn validate(&self) -> Result<(), String> {
        if !audio_controls::token(&self.batch_token)
            || !audio_controls::digest(&self.prefix_digest)
            || self.attempt == 0
            || self.attempt > MAX_SAFE
            || self.count > CONTROL_CAPACITY
        {
            return Err("Invalid reconciliation seal.".into());
        }
        Ok(())
    }
}
pub fn encode(message: &Message) -> Result<Vec<u8>, String> {
    match message {
        Message::Begin(v) => v.validate()?,
        Message::Entry(v) => v.validate()?,
        Message::Seal(v) => v.validate()?,
    }
    bounded_line(message)
}
fn bounded_line<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > CONTROL_BYTES {
        return Err("Reconciliation line exceeds512bytes including LF.".into());
    }
    Ok(bytes)
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrefixItem<'a> {
    ordinal: usize,
    command_sequence: u64,
    render_level: MixLevel,
    microphone_level: MixLevel,
    target_sample: &'a str,
    placement: Placement,
}
pub fn prefix_digest(entries: &[Entry]) -> Result<String, String> {
    if entries.len() > CONTROL_CAPACITY {
        return Err("Reconciliation prefix exceeds64entries.".into());
    }
    let mut items = Vec::with_capacity(entries.len());
    for (ordinal, entry) in entries.iter().enumerate() {
        entry.validate()?;
        if entry.ordinal != ordinal
            || ordinal > 0 && entries[ordinal - 1].command_sequence >= entry.command_sequence
        {
            return Err("Reconciliation prefix order differs.".into());
        }
        items.push(PrefixItem {
            ordinal,
            command_sequence: entry.command_sequence,
            render_level: entry.render_level,
            microphone_level: entry.microphone_level,
            target_sample: &entry.target_sample,
            placement: entry.placement,
        });
    }
    audio_controls::hash(&items).map_err(|_| "Cannot hash reconciliation prefix.".into())
}
#[derive(Clone, Debug)]
pub struct HostContext {
    pub session_id: String,
    pub source_generation: String,
    pub pause_revision: u64,
    pub sealed_sequence: u64,
    pub accepted_sequence: u64,
}
#[derive(Clone)]
struct Transfer {
    begin: Begin,
    entries: Vec<Entry>,
    deadline_ms: u64,
}
#[derive(Default)]
pub struct Stager {
    active: Option<Transfer>,
    ready: Option<SealedPrefix>,
    retired: std::collections::VecDeque<(String, u64)>,
}
impl Stager {
    pub fn clear(&mut self) {
        self.active = None;
        self.ready = None;
        self.retired.clear();
    }
    fn retire(&mut self, token: String, attempt: u64) {
        if let Some(old) = self.retired.iter_mut().find(|v| v.0 == token) {
            old.1 = old.1.max(attempt);
        } else {
            self.retired.push_back((token, attempt));
        }
        while self.retired.len() > 16 {
            self.retired.pop_front();
        }
    }
    fn current(&mut self, now: u64) -> Result<&mut Transfer, String> {
        if self.active.as_ref().is_some_and(|v| now >= v.deadline_ms) {
            let old = self.active.take().unwrap();
            self.retire(old.begin.batch_token, old.begin.attempt);
            return Err("Reconciliation stage expired.".into());
        }
        self.active
            .as_mut()
            .ok_or_else(|| "Reconciliation stage is unavailable.".into())
    }
    pub fn begin(&mut self, begin: Begin, context: &HostContext, now: u64) -> Result<(), String> {
        encode(&Message::Begin(begin.clone()))?;
        if begin.session_id != context.session_id
            || begin.source_generation != context.source_generation
            || begin.pause_revision != context.pause_revision
            || begin.sealed_sequence != context.sealed_sequence
            || begin.accepted_sequence < context.accepted_sequence
        {
            return Err("Reconciliation pause context changed.".into());
        }
        if self
            .active
            .as_ref()
            .is_some_and(|stage| now >= stage.deadline_ms)
        {
            let old = self.active.take().unwrap();
            self.retire(old.begin.batch_token, old.begin.attempt);
        }
        if self
            .retired
            .iter()
            .any(|old| old.0 == begin.batch_token && begin.attempt <= old.1)
        {
            return Err("Reconciliation attempt is retired.".into());
        }
        if let Some(ready) = &self.ready {
            if ready.begin.batch_token == begin.batch_token && ready.begin.attempt == begin.attempt
            {
                return if ready.begin == begin {
                    Ok(())
                } else {
                    Err("Conflicting duplicate reconciliation begin.".into())
                };
            }
            if ready.begin.batch_token == begin.batch_token && begin.attempt < ready.begin.attempt {
                return Err("Reconciliation attempt is stale.".into());
            }
        }
        if self.active.is_some() {
            self.current(now)?;
            let old = &self.active.as_ref().unwrap().begin;
            if old.batch_token == begin.batch_token && old.attempt == begin.attempt {
                return if *old == begin {
                    Ok(())
                } else {
                    Err("Conflicting duplicate reconciliation begin.".into())
                };
            }
            if old.batch_token == begin.batch_token && begin.attempt < old.attempt {
                return Err("Reconciliation attempt is stale.".into());
            }
            let old = self.active.take().unwrap();
            self.retire(old.begin.batch_token, old.begin.attempt);
        }
        if let Some(old) = self.ready.take() {
            self.retire(old.begin.batch_token, old.begin.attempt);
        }
        self.active = Some(Transfer {
            begin,
            entries: Vec::new(),
            deadline_ms: now.saturating_add(STAGE_MS),
        });
        Ok(())
    }
    pub fn entry(&mut self, entry: Entry, now: u64) -> Result<(), String> {
        encode(&Message::Entry(entry.clone()))?;
        if let Some(ready) = &self.ready
            && ready.begin.batch_token == entry.batch_token
            && ready.begin.attempt == entry.attempt
        {
            return if ready.entries.get(entry.ordinal) == Some(&entry) {
                Ok(())
            } else {
                Err("Conflicting duplicate sealed entry.".into())
            };
        }
        let transfer = self.current(now)?;
        if transfer.begin.batch_token != entry.batch_token
            || transfer.begin.attempt != entry.attempt
            || entry.ordinal >= transfer.begin.count
        {
            return Err("Reconciliation entry belongs to another stage.".into());
        }
        if let Some(old) = transfer.entries.get(entry.ordinal) {
            return if *old == entry {
                Ok(())
            } else {
                Err("Conflicting duplicate reconciliation entry.".into())
            };
        }
        if entry.ordinal != transfer.entries.len()
            || transfer
                .entries
                .last()
                .is_some_and(|v| v.command_sequence >= entry.command_sequence)
        {
            return Err("Reconciliation entry order differs.".into());
        }
        transfer.entries.push(entry);
        Ok(())
    }
    pub fn seal(&mut self, seal: Seal, now: u64) -> Result<SealedPrefix, String> {
        encode(&Message::Seal(seal.clone()))?;
        if let Some(ready) = &self.ready
            && ready.ack()
                == (Ack {
                    batch_token: seal.batch_token.clone(),
                    attempt: seal.attempt,
                    count: seal.count,
                    prefix_digest: seal.prefix_digest.clone(),
                })
        {
            return Ok(ready.clone());
        }
        let transfer = self.current(now)?;
        if transfer.begin.batch_token != seal.batch_token
            || transfer.begin.attempt != seal.attempt
            || transfer.begin.count != seal.count
            || transfer.entries.len() != seal.count
            || transfer.begin.prefix_digest != seal.prefix_digest
            || prefix_digest(&transfer.entries)? != seal.prefix_digest
        {
            return Err("Incomplete or conflicting reconciliation prefix.".into());
        }
        let transfer = self.active.take().unwrap();
        let ready = SealedPrefix {
            begin: transfer.begin,
            entries: transfer.entries,
        };
        self.ready = Some(ready.clone());
        Ok(ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parts() -> (Begin, Entry, Seal, HostContext) {
        let entry = Entry {
            batch_token: "b".repeat(32),
            attempt: 1,
            ordinal: 0,
            command_sequence: 1,
            render_level: MixLevel::default(),
            microphone_level: MixLevel::default(),
            target_sample: "480".into(),
            placement: Placement::ResumeBound,
        };
        let digest = prefix_digest(std::slice::from_ref(&entry)).unwrap();
        let begin = Begin {
            session_id: "s".into(),
            source_generation: "g".into(),
            pause_revision: 1,
            batch_token: entry.batch_token.clone(),
            attempt: 1,
            count: 1,
            prefix_digest: digest.clone(),
            sealed_sequence: 0,
            accepted_sequence: 1,
        };
        let seal = Seal {
            batch_token: entry.batch_token.clone(),
            attempt: 1,
            count: 1,
            prefix_digest: digest,
        };
        let ctx = HostContext {
            session_id: "s".into(),
            source_generation: "g".into(),
            pause_revision: 1,
            sealed_sequence: 0,
            accepted_sequence: 0,
        };
        (begin, entry, seal, ctx)
    }
    #[test]
    fn actual_private_line_bound_counts_newline_and_maximum_safe_counters() {
        let (mut b, mut e, mut s, _) = parts();
        b.session_id = "s".repeat(80);
        b.source_generation = "g".repeat(80);
        b.pause_revision = MAX_SAFE;
        b.attempt = MAX_SAFE;
        b.count = 64;
        b.sealed_sequence = MAX_SAFE;
        b.accepted_sequence = MAX_SAFE;
        assert_eq!(encode(&Message::Begin(b.clone())).unwrap().len(), 510);
        e.attempt = MAX_SAFE;
        e.ordinal = 63;
        e.command_sequence = MAX_SAFE;
        e.target_sample = u64::MAX.to_string();
        e.render_level.gain = 1.2345678;
        e.microphone_level.gain = 0.12345678;
        assert!(encode(&Message::Entry(e)).unwrap().len() <= 373);
        s.attempt = MAX_SAFE;
        s.count = 64;
        assert_eq!(encode(&Message::Seal(s)).unwrap().len(), 209);
        assert_eq!(bounded_line(&"x".repeat(509)).unwrap().len(), 512);
        assert!(bounded_line(&"x".repeat(510)).is_err()); // actual compact 513 including LF
        b.session_id.push('s');
        assert!(encode(&Message::Begin(b)).is_err());
    }
    #[test]
    fn complete_prefix_only_acknowledges_once_and_duplicate_begin_does_not_extend_ttl() {
        let (b, e, s, ctx) = parts();
        let mut stage = Stager::default();
        stage.begin(b.clone(), &ctx, 0).unwrap();
        assert!(stage.seal(s.clone(), 1).is_err());
        stage.begin(b.clone(), &ctx, 1999).unwrap();
        assert!(stage.entry(e.clone(), 2000).is_err());
        assert!(stage.begin(b, &ctx, 2001).is_err());
        let (mut b, mut e, mut s, ctx) = parts();
        b.attempt = 2;
        e.attempt = 2;
        s.attempt = 2;
        stage.begin(b.clone(), &ctx, 2002).unwrap();
        stage.entry(e.clone(), 2003).unwrap();
        stage.entry(e, 2004).unwrap();
        let complete = stage.seal(s.clone(), 2005).unwrap();
        assert_eq!(complete.ack().attempt, 2);
        assert_eq!(stage.seal(s, 2006).unwrap(), complete);
        stage.clear();
        assert!(stage.entry(parts().1, 2007).is_err());
    }
    #[test]
    fn digest_binds_order_targets_f32_and_complete_remaining_prefix_with_gaps() {
        let (_, a, _, _) = parts();
        let mut b = a.clone();
        b.command_sequence = 3;
        b.ordinal = 1;
        b.target_sample = "960".into();
        let digest = prefix_digest(&[a.clone(), b.clone()]).unwrap();
        b.target_sample = "961".into();
        assert_ne!(digest, prefix_digest(&[a.clone(), b.clone()]).unwrap());
        b.ordinal = 0;
        assert!(prefix_digest(&[a, b]).is_err());
        let mut a = parts().1;
        a.render_level.gain = -0.;
        assert!(a.validate().is_err());
    }
}
