//! Selected sources share one native QPC/sample epoch and post-mute float stems.
use super::{
    audio,
    audio_envelope::{self, Envelope},
    audio_mixer::{Command, ControlQueue, Mixer},
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
        let before = s.queue.as_ref().unwrap().clone();
        let result = s.queue.as_mut().unwrap().push(levels.clone(), receipt);
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
    fn claim(&self, zero: i64, sample: u64, final_tail: bool) -> Option<Command> {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.closed && !final_tail {
            return None;
        }
        let cutoff = s.final_sample;
        s.queue.as_mut()?.claim_due(zero, sample, cutoff)
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
