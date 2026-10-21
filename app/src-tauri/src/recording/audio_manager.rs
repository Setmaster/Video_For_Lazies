//! Audio controls are ordered independently of recording Stop/save and preview.
use super::*;
use super::{audio_mixer::ControlQueue, audio_types::*};
use std::sync::atomic::AtomicU64;

#[derive(Default)]
struct Inner {
    owner: Option<String>,
    config: Option<NativeAudioConfig>,
    queue: Option<ControlQueue>,
    status: Option<AudioStatus>,
    native_sequence: Option<u64>,
    sequence: u64,
    ready: bool,
    closed: bool,
}
#[derive(Default)]
pub(super) struct AudioManager {
    inner: Mutex<Inner>,
    applications: Mutex<Vec<(String, ProcessIdentity)>>,
    next: AtomicU64,
}
impl AudioManager {
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
    pub fn prepare(
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
        let mut state = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        *state = Inner {
            owner: Some(id.into()),
            config: Some(native.clone()),
            queue: Some(ControlQueue::new(
                native.render_level,
                native.microphone_level,
            )),
            ..Inner::default()
        };
        Ok(native)
    }
    pub fn snapshot(&self, id: Option<&str>) -> Option<AudioStatus> {
        let s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if id.is_some_and(|id| s.owner.as_deref() != Some(id)) {
            return None;
        }
        s.status.clone()
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
        if s.native_sequence.is_some_and(|old| status.sequence <= old) {
            return Ok(());
        }
        s.native_sequence = Some(status.sequence);
        s.ready |= ready;
        if let Some(queue) = &mut s.queue {
            status.accepted_command_sequence =
                status.accepted_command_sequence.max(queue.last_sequence());
            if let Some(command) = queue.front() {
                let sequence = command.levels.command_sequence;
                if sequence <= status.applied_command_sequence {
                    queue.settle(status.applied_command_sequence);
                }
            }
        }
        s.sequence += 1;
        status.sequence = s.sequence;
        s.status = Some(status);
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
        let last = s.queue.as_ref().map_or(0, ControlQueue::last_sequence);
        s.sequence += 1;
        let sequence = s.sequence;
        if let Some(status) = &mut s.status {
            status.sequence = sequence;
            if last > status.applied_command_sequence {
                status.rejected_command_sequence = Some(last);
                status.message.get_or_insert(
                    "Audio changes after the recording cutoff were not applied.".into(),
                );
            }
        }
        drop(s);
        self.publish(app);
    }
    pub fn submit(
        &self,
        app: &AppHandle,
        levels: AudioLevels,
    ) -> Result<AudioControlSubmission, String> {
        levels.validate()?;
        let mut s = self
            .inner
            .lock()
            .map_err(|_| "Audio controls are unavailable.")?;
        let config = s
            .config
            .as_ref()
            .ok_or("Wait until recording starts before changing audio.")?;
        if config.session_id != levels.session_id
            || config.source_generation != levels.source_generation
        {
            return Err("This audio change belongs to another session.".into());
        }
        if !s.ready || s.closed || config.role_mask() == 0 {
            return Err("Audio controls are not active.".into());
        }
        if let Err(reason) = s.queue.as_mut().unwrap().push(levels.clone(), 0) {
            s.sequence += 1;
            let sequence = s.sequence;
            if let Some(status) = &mut s.status {
                status.sequence = sequence;
                status.rejected_command_sequence = Some(levels.command_sequence);
                status.message = Some(reason.clone());
            }
            drop(s);
            self.publish(app);
            return Err(reason);
        }
        s.sequence += 1;
        let sequence = s.sequence;
        if let Some(status) = &mut s.status {
            status.accepted_command_sequence = status
                .accepted_command_sequence
                .max(levels.command_sequence);
            status.sequence = sequence;
        }
        let result = AudioControlSubmission {
            session_id: levels.session_id,
            source_generation: levels.source_generation,
            accepted_command_sequence: levels.command_sequence,
        };
        drop(s);
        self.publish(app);
        Ok(result)
    }
    fn control(&self, id: &str, send: &mpsc::SyncSender<Value>, stop: &AtomicBool) {
        let mut s = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if s.owner.as_deref() != Some(id) || s.closed || !s.ready {
            return;
        }
        let Some(queue) = s.queue.as_mut() else {
            return;
        };
        let mut sent_new = false;
        while !stop.load(Ordering::Acquire) {
            let new = queue.pending_front().is_some();
            if !new && sent_new {
                break;
            }
            let Some(command) = queue.pending_front().or_else(|| queue.front()) else {
                break;
            };
            let value = json!({"operation":"audio-levels","value":command.levels});
            // Reserve in-flight identity only after the bounded pipe lane accepts it.
            if send.try_send(value).is_err() {
                break;
            }
            if new {
                queue.claim_next();
                sent_new = true;
            } else {
                break;
            }
        }
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
    if serde_json::to_vec(status).map_err(error)?.len() >= STATUS_BYTES {
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
        let ready = value["ready"] == true;
        let Some(audio) = value.get("audio").filter(|v| !v.is_null()) else {
            return Ok(());
        };
        self.manager.update(
            &self.app,
            &self.owner,
            serde_json::from_value(audio.clone()).map_err(error)?,
            ready,
        )
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
// Tauri keeps this command's identity and level fields as explicit IPC arguments.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub(crate) fn recording_audio_set_levels(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    session_id: String,
    source_generation: String,
    command_sequence: u64,
    render_level: MixLevel,
    microphone_level: MixLevel,
) -> Result<AudioControlSubmission, String> {
    main_window(&window)?;
    let i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if i.snapshot.session_id.as_deref() != Some(&session_id) || i.snapshot.phase != "recording" {
        return Err("Audio controls are not active for this recording.".into());
    }
    let result = state.audio.submit(
        &app,
        AudioLevels {
            session_id,
            source_generation,
            command_sequence,
            render_level,
            microphone_level,
        },
    );
    if let Err(reason) = &result
        && reason.contains("safe queue")
        && let Some(stop) = &i.stop
    {
        stop.store(true, Ordering::Release);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_transport_does_not_claim_or_skip_an_edge_when_full() {
        let manager = AudioManager::default();
        let mut queue = ControlQueue::new(MixLevel::default(), MixLevel::default());
        for (sequence, muted) in [(1, true), (2, false)] {
            queue
                .push(
                    AudioLevels {
                        session_id: "session".into(),
                        source_generation: "generation".into(),
                        command_sequence: sequence,
                        render_level: MixLevel { gain: 1., muted },
                        microphone_level: MixLevel::default(),
                    },
                    0,
                )
                .unwrap();
        }
        *manager.inner.lock().unwrap() = Inner {
            owner: Some("session".into()),
            queue: Some(queue),
            ready: true,
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(1);
        send.try_send(json!({"operation":"preview-renew"})).unwrap();
        manager.control("session", &send, &AtomicBool::new(false));
        assert_eq!(
            manager
                .inner
                .lock()
                .unwrap()
                .queue
                .as_ref()
                .unwrap()
                .pending_front()
                .unwrap()
                .levels
                .command_sequence,
            1
        );
        receive.recv().unwrap();
        manager.control("session", &send, &AtomicBool::new(false));
        manager.control("session", &send, &AtomicBool::new(false));
        assert_eq!(receive.recv().unwrap()["value"]["commandSequence"], 1);
        assert_eq!(
            manager
                .inner
                .lock()
                .unwrap()
                .queue
                .as_ref()
                .unwrap()
                .pending_front()
                .unwrap()
                .levels
                .command_sequence,
            2
        );
        manager.control("session", &send, &AtomicBool::new(false));
        assert_eq!(receive.recv().unwrap()["value"]["commandSequence"], 2);
        assert_eq!(
            manager.inner.lock().unwrap().queue.as_ref().unwrap().len(),
            2
        );
        // A stale owner cannot write to a later session's pipe lane.
        manager.inner.lock().unwrap().owner = Some("later".into());
        manager.control("session", &send, &AtomicBool::new(false));
        assert!(receive.try_recv().is_err());
    }
    #[test]
    fn one_pump_delivers_the_entire_bounded_edge_burst_without_waiting_for_acks() {
        let manager = AudioManager::default();
        let mut queue = ControlQueue::new(MixLevel::default(), MixLevel::default());
        for sequence in 1..=64 {
            queue
                .push(
                    AudioLevels {
                        session_id: "session".into(),
                        source_generation: "generation".into(),
                        command_sequence: sequence,
                        render_level: MixLevel {
                            gain: 1.,
                            muted: sequence % 2 == 1,
                        },
                        microphone_level: MixLevel::default(),
                    },
                    0,
                )
                .unwrap();
        }
        *manager.inner.lock().unwrap() = Inner {
            owner: Some("session".into()),
            queue: Some(queue),
            ready: true,
            ..Default::default()
        };
        let (send, receive) = mpsc::sync_channel(65);
        let stop = AtomicBool::new(false);
        manager.control("session", &send, &stop);
        for sequence in 1..=64 {
            assert_eq!(
                receive.try_recv().unwrap()["value"]["commandSequence"],
                sequence
            );
        }
        assert!(receive.try_recv().is_err());
        assert!(
            manager
                .inner
                .lock()
                .unwrap()
                .queue
                .as_ref()
                .unwrap()
                .pending_front()
                .is_none()
        );
        stop.store(true, Ordering::Release);
        manager.control("session", &send, &stop);
        assert!(receive.try_recv().is_err());
    }
}
