//! Selected sources share one native QPC/sample epoch and post-mute float stems.
use super::{
    audio,
    audio_controls::{AcceptedIntent, AudioReceipt, LevelIntent, Placement},
    audio_envelope::{self, Envelope},
    audio_mixer::{Command, ControlQueue, Mixer},
    audio_reconcile_wire::{self, Message, Stager},
    audio_types::*,
};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
};

#[derive(Default)]
struct State {
    config: Option<NativeAudioConfig>,
    queue: Option<ControlQueue>,
    status: Option<AudioStatus>,
    ready: bool,
    closed: bool,
    persisted: u64,
    fatal: Option<String>,
    cutoff: Option<i64>,
    final_sample: Option<u64>,
    active_zero: Option<i64>,
    pause_sample: Option<u64>,
    pause_revision: u64,
    sealed_sequence: u64,
    receipts: Vec<AudioReceipt>,
    stager: Stager,
    stage_receipts: std::collections::BTreeMap<u64, i64>,
    ack: Option<audio_reconcile_wire::Ack>,
    reconcile_error: Option<serde_json::Value>,
    reconciled_revision: Option<u64>,
}
#[derive(Default)]
pub struct Controls {
    inner: Mutex<State>,
}
impl Controls {
    pub fn configure(&self, config: &NativeAudioConfig) -> Result<(), String> {
        config.validate()?;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        if s.config.is_some() {
            return Err("Audio session already initialized.".into());
        }
        let meter = |enabled: bool| AudioMeter {
            state: if enabled { "pending" } else { "off" }.into(),
            ..AudioMeter::default()
        };
        let roles = config.role_mask();
        s.status = Some(AudioStatus {
            session_id: config.session_id.clone(),
            source_generation: config.source_generation.clone(),
            sequence: 0,
            accepted_command_sequence: 0,
            applied_command_sequence: 0,
            applied_at_sample: "0".into(),
            rejected_command_sequence: None,
            render_level: config.render_level,
            microphone_level: config.microphone_level,
            render: meter(roles & 1 != 0),
            microphone: meter(roles & 2 != 0),
            mixed: meter(roles != 0),
            message: None,
        });
        s.queue = Some(ControlQueue::new(
            config.render_level,
            config.microphone_level,
        ));
        s.config = Some(config.clone());
        Ok(())
    }
    pub fn ready(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).ready = true;
    }
    pub fn set_epoch(&self, zero: i64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_zero = Some(zero);
    }
    pub fn submit(&self, levels: AudioLevels) -> Result<(), String> {
        let receipt = super::qpc100ns().map_err(|e| e.to_string())?;
        levels.validate()?;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        let config = s.config.as_ref().ok_or("Audio is not initialized.")?;
        if config.session_id != levels.session_id
            || config.source_generation != levels.source_generation
        {
            return Err("This audio control belongs to another session.".into());
        }
        if !s.ready || s.closed || config.role_mask() == 0 {
            return Err("Audio controls are not active.".into());
        }
        let roles = config.role_mask();
        let zero = s
            .active_zero
            .ok_or("Audio media clock is not initialized.")?;
        let old = s
            .queue
            .as_ref()
            .unwrap()
            .lookup_command(levels.command_sequence);
        if old.is_none() && s.receipts.len() >= CONTROL_CAPACITY {
            let reason = "Audio receipt delivery exceeded its safe bound; recording stopped.";
            Self::reject(&mut s, levels.command_sequence, reason, Some(receipt));
            return Err(reason.into());
        }
        let target = old
            .as_ref()
            .and_then(|c| c.target_sample)
            .unwrap_or_else(|| super::audio_mixer::control_sample(zero, receipt));
        let before = s.queue.as_ref().unwrap().clone();
        let result = s
            .queue
            .as_mut()
            .unwrap()
            .push_preserving(levels.clone(), receipt);
        let capacity =
            s.queue.as_ref().unwrap().edge_count() + s.persisted > audio_envelope::MAX_RECORDS;
        if capacity {
            s.queue = Some(before);
        }
        if let Err(reason) = result {
            let fatal = reason.contains("safe queue");
            Self::reject(
                &mut s,
                levels.command_sequence,
                &reason,
                if fatal { Some(receipt) } else { None },
            );
            return Err(reason);
        }
        if capacity && roles != 0 {
            let reason = "Mute history reached its safe capacity; the change was not applied.";
            Self::reject(&mut s, levels.command_sequence, reason, Some(receipt));
            return Err(reason.into());
        }
        s.queue
            .as_mut()
            .unwrap()
            .bind_target(levels.command_sequence, target)?;
        if old.is_none() {
            s.receipts.push(AudioReceipt {
                command_sequence: levels.command_sequence,
                target_sample: target.to_string(),
                placement: Placement::Active,
            });
        }
        if let Some(status) = &mut s.status {
            status.accepted_command_sequence = status
                .accepted_command_sequence
                .max(levels.command_sequence);
            status.sequence += 1;
        }
        Ok(())
    }
    fn reject(s: &mut State, sequence: u64, reason: &str, cutoff: Option<i64>) {
        if let Some(status) = &mut s.status {
            status.rejected_command_sequence = Some(sequence);
            status.message = Some(reason.into());
            status.sequence += 1;
        }
        if let Some(cutoff) = cutoff {
            s.fatal = Some(reason.into());
            s.cutoff = Some(cutoff);
            s.closed = true;
        }
    }
    fn claim(&self, _zero: i64, sample: u64, final_tail: bool) -> Option<Command> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.closed && !final_tail {
            return None;
        }
        let cutoff = s.final_sample;
        s.queue.as_mut()?.claim_due_at(sample, cutoff)
    }
    pub fn pause_at(&self, cutoff: u64, revision: u64, sealed: u64) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.ready = false;
        s.final_sample = Some(cutoff);
        s.pause_sample = Some(cutoff);
        s.pause_revision = revision;
        s.sealed_sequence = sealed;
        s.reconciled_revision = None;
    }
    pub fn seal_admission(&self, sealed: u64) -> Result<(), String> {
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls unavailable.")?;
        if s.closed || !s.ready || s.queue.as_ref().is_none_or(|q| q.last_sequence() != sealed) {
            return Err("Pause did not receive the complete accepted audio watermark.".into());
        }
        s.ready = false;
        Ok(())
    }
    pub fn resume_at(&self, zero: i64) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.active_zero = Some(zero);
        s.final_sample = None;
        s.pause_sample = None;
        s.ready = true;
        s.stager.clear();
        s.stage_receipts.clear();
        s.reconciled_revision = None;
        let roles = s.config.as_ref().map_or(0, NativeAudioConfig::role_mask);
        if let Some(status) = &mut s.status {
            status.render.state = if roles & 1 != 0 { "pending" } else { "off" }.into();
            status.microphone.state = if roles & 2 != 0 { "pending" } else { "off" }.into();
            status.mixed.state = if roles != 0 { "pending" } else { "off" }.into();
            status.sequence += 1;
        }
    }
    pub fn admit_resume(&self, revision: u64) -> Result<(), String> {
        let s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls unavailable.")?;
        if s.closed || s.pause_sample.is_none() || s.pause_revision != revision {
            return Err("Resume belongs to a different Pause boundary.".into());
        }
        if s.config
            .as_ref()
            .is_none_or(|c| c.role_mask() != 0 && s.reconciled_revision != Some(revision))
        {
            return Err("Resume requires a complete native audio-prefix acknowledgment.".into());
        }
        Ok(())
    }
    pub fn paused_released(&self) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(status) = &mut s.status {
            for meter in [
                &mut status.render,
                &mut status.microphone,
                &mut status.mixed,
            ] {
                meter.state = "off".into();
                meter.peak = 0.;
                meter.rms = 0.;
            }
            status.sequence += 1;
        }
    }
    pub fn pending(&self) -> Vec<AcceptedIntent> {
        let s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.queue
            .as_ref()
            .map(|q| {
                q.pending_commands()
                    .into_iter()
                    .map(|c| AcceptedIntent {
                        intent: LevelIntent::from(&c.levels),
                        target_sample: c.target_sample.map(|n| n.to_string()),
                        placement: Placement::Active,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn notices(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut value = serde_json::Map::new();
        if !s.receipts.is_empty() {
            value.insert(
                "audioReceipts".into(),
                serde_json::json!(std::mem::take(&mut s.receipts)),
            );
        }
        if let Some(ack) = s.ack.take() {
            value.insert("reconcileAck".into(), serde_json::json!(ack));
        }
        if let Some(error) = s.reconcile_error.take() {
            value.insert("reconcileError".into(), error);
        }
        value
    }
    pub fn reconcile(&self, message: Message) -> Result<(), String> {
        let receipt = super::qpc100ns().map_err(|e| e.to_string())?;
        let now = (receipt.max(0) as u64) / 10_000;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls unavailable.")?;
        let Some(cutoff) = s.pause_sample else {
            return Err("Reconciliation requires a paused capture.".into());
        };
        let config = s
            .config
            .as_ref()
            .ok_or("Audio is not initialized.")?
            .clone();
        if config.role_mask() == 0 {
            return Err("This recording has no audio reconciliation fence.".into());
        }
        let accepted = s.queue.as_ref().unwrap().last_sequence();
        let (token, attempt) = match &message {
            Message::Begin(v) => (v.batch_token.clone(), v.attempt),
            Message::Entry(v) => (v.batch_token.clone(), v.attempt),
            Message::Seal(v) => (v.batch_token.clone(), v.attempt),
        };
        let result = (|| -> Result<(), String> {
            match message {
                Message::Begin(begin) => {
                    let context = audio_reconcile_wire::HostContext {
                        session_id: config.session_id,
                        source_generation: config.source_generation,
                        pause_revision: s.pause_revision,
                        sealed_sequence: s.sealed_sequence,
                        accepted_sequence: accepted,
                    };
                    s.stager.begin(begin, &context, now)?;
                    s.reconciled_revision = None;
                }
                Message::Entry(entry) => {
                    let target = entry
                        .target_sample
                        .parse::<u64>()
                        .map_err(|_| "Invalid target sample.")?;
                    if target < cutoff
                        || entry.placement == Placement::ResumeBound && target != cutoff
                    {
                        return Err("Reconciliation target precedes the Pause boundary.".into());
                    }
                    if s.stage_receipts.len() >= CONTROL_CAPACITY
                        && !s.stage_receipts.contains_key(&entry.command_sequence)
                    {
                        return Err("Reconciliation receipt bound exceeded.".into());
                    }
                    s.stage_receipts
                        .entry(entry.command_sequence)
                        .or_insert(receipt);
                    s.stager.entry(entry, now)?;
                }
                Message::Seal(seal) => {
                    let prefix = s.stager.seal(seal, now)?;
                    let mut previous = s
                        .status
                        .as_ref()
                        .map(|a| [a.render_level.muted, a.microphone_level.muted])
                        .unwrap_or([false, false]);
                    let commands: Vec<Command> = prefix
                        .entries
                        .iter()
                        .map(|entry| {
                            let levels = entry
                                .intent()
                                .levels(&config.session_id, &config.source_generation);
                            let mutes = [levels.render_level.muted, levels.microphone_level.muted];
                            let old = s
                                .queue
                                .as_ref()
                                .unwrap()
                                .lookup_command(entry.command_sequence);
                            let edge = previous != mutes;
                            previous = mutes;
                            if let Some(old) = &old
                                && (old.levels != levels
                                    || old.target_sample != entry.target_sample.parse().ok())
                            {
                                return Err(
                                    "Conflicting retained reconciliation identity or target."
                                        .to_string(),
                                );
                            }
                            Ok(old.unwrap_or_else(|| Command {
                                levels,
                                received_qpc: *s
                                    .stage_receipts
                                    .get(&entry.command_sequence)
                                    .unwrap_or(&receipt),
                                edge,
                                target_sample: entry.target_sample.parse().ok(),
                            }))
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    let mut queue = s.queue.as_ref().unwrap().clone();
                    queue.restore_prefix(&commands)?;
                    if queue.edge_count() + s.persisted > audio_envelope::MAX_RECORDS {
                        return Err("Mute envelope capacity exceeded.".into());
                    }
                    s.queue = Some(queue);
                    s.reconciled_revision = Some(s.pause_revision);
                    s.ack = Some(prefix.ack());
                    if let Some(status) = s.status.as_mut() {
                        status.accepted_command_sequence = prefix.begin.accepted_sequence;
                        status.sequence += 1;
                    }
                }
            }
            Ok(())
        })();
        if let Err(reason) = &result {
            let expired = reason.contains("expired") || reason.contains("retired");
            s.reconcile_error = Some(
                serde_json::json!({"batchToken":token,"attempt":attempt,"code":if expired{"stageExpired"}else{"protocolFailure"}}),
            );
            if expired {
                return Ok(());
            }
            s.fatal = Some(reason.clone());
            s.cutoff = Some(receipt);
            s.closed = true;
        }
        result
    }
    fn commit(&self, mixer: &Mixer, meters: [AudioMeter; 3], persisted: u64) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.persisted = persisted;
        if let Some(q) = &mut s.queue {
            q.settle(mixer.applied_sequence);
        }
        if let Some(status) = &mut s.status {
            status.applied_command_sequence = mixer.applied_sequence;
            status.applied_at_sample = mixer.applied_sample.to_string();
            status.render_level = mixer.effective[0];
            status.microphone_level = mixer.effective[1];
            [status.render, status.microphone, status.mixed] = meters;
            status.sequence += 1;
        }
    }
    pub fn freeze(&self, cutoff: u64) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        s.final_sample = Some(cutoff);
    }
    pub fn failure(&self) -> Option<String> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .fatal
            .clone()
    }
    pub fn cutoff(&self) -> Option<i64> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).cutoff
    }
    pub fn snapshot(&self) -> Option<AudioStatus> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .status
            .clone()
    }
    pub fn finish(&self, message: Option<String>) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        s.closed = true;
        let last = s.queue.as_ref().map_or(0, ControlQueue::last_sequence);
        if let Some(status) = &mut s.status {
            if last > status.applied_command_sequence {
                status.rejected_command_sequence = Some(last);
                status.message =
                    Some("Audio changes after the recording cutoff were not applied.".into());
            }
            if let Some(message) = message {
                status.message = Some(message);
            }
            status.sequence += 1;
        }
    }
}

pub struct Pipeline {
    capture: audio::AudioCapture,
    controls: Arc<Controls>,
    mixer: Mixer,
    envelope: Option<Envelope>,
    stems: [Option<File>; 2],
    pub frames: u64,
    config: NativeAudioConfig,
}
impl Pipeline {
    pub fn start(
        config: &NativeAudioConfig,
        directory: &Path,
        controls: Arc<Controls>,
        stop: &AtomicBool,
    ) -> Result<Self, String> {
        controls.configure(config)?;
        let roles = config.role_mask();
        let envelope = if roles != 0 {
            Some(Envelope::create(
                directory,
                &config.session_id,
                roles,
                config.mute_mask(),
            )?)
        } else {
            None
        };
        let mut stems: [Option<File>; 2] = [None, None];
        for (role, name) in ["render.f32le", "microphone.f32le"].into_iter().enumerate() {
            if roles & (1 << role) != 0 {
                stems[role] = Some(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(directory.join(name))
                        .map_err(|e| format!("Prepare {name}: {e}"))?,
                );
            }
        }
        let capture = audio::AudioCapture::start_native(
            &config.render,
            config.microphone.as_ref().map(|m| m.device_id.as_str()),
            stop,
        )?;
        Ok(Self {
            capture,
            controls,
            mixer: Mixer::new(config),
            envelope,
            stems,
            frames: 0,
            config: config.clone(),
        })
    }
    pub fn poll(&mut self) -> Result<(), String> {
        if let Some(error) = self.controls.failure() {
            return Err(error);
        }
        self.capture.poll()
    }
    pub fn stop(&mut self) -> Result<(), String> {
        // A control-capacity stop must still drain the healthy endpoint tail.
        let poll = self.capture.poll();
        let stop = self.capture.stop();
        poll.and(stop)
    }
    pub fn render(
        &mut self,
        zero: i64,
        count: u32,
        stop: &AtomicBool,
        final_tail: bool,
    ) -> Result<(), String> {
        if count == 0 || count > 480 {
            return Err("Invalid audio sample block.".into());
        }
        while final_tail || !stop.load(std::sync::atomic::Ordering::Acquire) {
            let Some(command) = self.controls.claim(zero, self.frames, final_tail) else {
                break;
            };
            let mask = (u8::from(command.levels.render_level.muted)
                | (u8::from(command.levels.microphone_level.muted) << 1))
                & self.mixer.roles;
            if let Some(envelope) = &mut self.envelope {
                envelope.append(self.frames, command.levels.command_sequence, mask)?;
            }
            self.mixer.apply(&command.levels, self.frames);
        }
        // A Stop observed during edge persistence leaves this block unwritten.
        // The frozen-cutoff tail completes the same boundary, including its edges.
        if !final_tail && stop.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let time = zero + (i128::from(self.frames) * 10_000_000 / 48_000) as i64;
        let sources = self.capture.render_sources(time, count)?;
        let (blocks, meters) = self.mixer.process(sources)?;
        for (file, block) in self.stems.iter_mut().zip(blocks) {
            if let Some(file) = file {
                let mut bytes = Vec::with_capacity(block.len() * 8);
                for frame in block {
                    for value in frame {
                        bytes.extend_from_slice(&value.to_le_bytes());
                    }
                }
                file.write_all(&bytes)
                    .map_err(|e| format!("Write common-clock audio stem: {e}"))?;
            }
        }
        self.frames += u64::from(count);
        let persisted = self
            .envelope
            .as_ref()
            .map_or(0, |e| audio_envelope::MAX_RECORDS - e.remaining());
        // Both selected stems are written before acknowledging this boundary.
        self.controls.commit(&self.mixer, meters, persisted);
        Ok(())
    }
    pub fn bytes(&self) -> u64 {
        self.frames * 8 * u64::from(self.mixer.roles.count_ones())
            + self.envelope.as_ref().map_or(0, Envelope::bytes)
    }
    pub fn release_capture(&mut self) {
        self.capture.release();
    }
    pub fn reopen(&mut self, stop: &AtomicBool) -> Result<(), String> {
        self.capture = audio::AudioCapture::start_native(
            &self.config.render,
            self.config
                .microphone
                .as_ref()
                .map(|m| m.device_id.as_str()),
            stop,
        )?;
        Ok(())
    }
    pub fn finish(&self) -> Result<(), String> {
        for file in self.stems.iter().flatten() {
            file.sync_all()
                .map_err(|e| format!("Flush audio stem: {e}"))?;
        }
        if let Some(envelope) = &self.envelope {
            envelope.finish()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod controls_tests {
    use super::*;
    fn config() -> NativeAudioConfig {
        NativeAudioConfig {
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            render: NativeRender::System {
                device_id: "fixed-endpoint".into(),
            },
            microphone: None,
            render_level: MixLevel::default(),
            microphone_level: MixLevel::default(),
        }
    }
    fn levels(n: u64) -> AudioLevels {
        AudioLevels {
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            command_sequence: n,
            render_level: MixLevel {
                gain: 0.5,
                muted: false,
            },
            microphone_level: MixLevel::default(),
        }
    }
    #[test]
    fn receipt_capacity_rejects_before_new_identity_or_pcm_admission() {
        let c = Controls::default();
        c.configure(&config()).unwrap();
        c.set_epoch(0);
        c.ready();
        for n in 1..=64 {
            c.submit(levels(n)).unwrap();
        }
        assert!(c.submit(levels(65)).is_err());
        let s = c.inner.lock().unwrap();
        assert_eq!(s.queue.as_ref().unwrap().last_sequence(), 64);
        assert_eq!(s.receipts.len(), 64);
        assert!(s.fatal.is_some());
        assert_eq!(s.status.as_ref().unwrap().applied_command_sequence, 0);
    }
    #[test]
    fn complete_prefix_preserves_all_received_gains_and_first_targets_before_resume() {
        use audio_reconcile_wire::*;
        let c = Controls::default();
        c.configure(&config()).unwrap();
        {
            let mut s = c.inner.lock().unwrap();
            for n in 1..=2 {
                let q = s.queue.as_mut().unwrap();
                q.push_preserving(levels(n), n as i64).unwrap();
                q.bind_target(n, 480).unwrap();
            }
        }
        c.pause_at(480, 1, 2);
        assert!(c.admit_resume(1).is_err());
        let entries: Vec<Entry> = (1..=2)
            .map(|n| Entry {
                batch_token: "a".repeat(32),
                attempt: 1,
                ordinal: (n - 1) as usize,
                command_sequence: n,
                render_level: levels(n).render_level,
                microphone_level: levels(n).microphone_level,
                target_sample: "480".into(),
                placement: Placement::Active,
            })
            .collect();
        let digest = prefix_digest(&entries).unwrap();
        c.reconcile(Message::Begin(Begin {
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            pause_revision: 1,
            batch_token: "a".repeat(32),
            attempt: 1,
            count: 2,
            prefix_digest: digest.clone(),
            sealed_sequence: 2,
            accepted_sequence: 2,
        }))
        .unwrap();
        for entry in entries {
            c.reconcile(Message::Entry(entry)).unwrap();
        }
        c.reconcile(Message::Seal(Seal {
            batch_token: "a".repeat(32),
            attempt: 1,
            count: 2,
            prefix_digest: digest,
        }))
        .unwrap();
        assert!(c.notices().contains_key("reconcileAck"));
        c.admit_resume(1).unwrap();
        assert!(c.admit_resume(2).is_err());
        assert_eq!(c.pending().len(), 2);
        assert_eq!(c.snapshot().unwrap().applied_command_sequence, 0);
        c.resume_at(100_000_000);
        for n in 1..=2 {
            let command = c.claim(100_000_000, 480, false).unwrap();
            assert_eq!(command.levels.command_sequence, n);
            assert_eq!(command.received_qpc, n as i64);
            assert_eq!(command.target_sample, Some(480));
        }
        assert!(c.claim(100_000_000, 480, false).is_none());
        assert_eq!(c.snapshot().unwrap().applied_command_sequence, 0);
    }
    #[test]
    fn configured_off_status_does_not_require_an_audio_reconcile_fence() {
        let c = Controls::default();
        let mut config = config();
        config.render = NativeRender::Off;
        c.configure(&config).unwrap();
        assert!(c.snapshot().is_some());
        c.pause_at(480, 1, 0);
        c.admit_resume(1).unwrap();
        assert!(c.admit_resume(2).is_err());
    }
    #[test]
    fn pause_barrier_requires_complete_accepted_watermark() {
        let c = Controls::default();
        c.configure(&config()).unwrap();
        c.set_epoch(0);
        c.ready();
        c.submit(levels(1)).unwrap();
        assert!(c.seal_admission(2).is_err());
        c.seal_admission(1).unwrap();
        assert!(c.submit(levels(2)).is_err());
    }
}
