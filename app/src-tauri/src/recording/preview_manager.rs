//! Preview delivery has its own identity and lease, separate from media lifetime.
use super::*;
use super::{
    preview::{self, Frame},
    source::{Source, WindowIdentity, WindowListing},
};
use std::{collections::HashSet, sync::Condvar};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreviewSnapshot {
    pub preview_id: Option<String>,
    pub sequence: u64,
    pub config_epoch: u64,
    pub phase: String,
    pub source: Option<Source>,
    pub frame_sequence: u64,
    pub source_width: u32,
    pub source_height: u32,
    pub last_frame_age_ms: Option<u64>,
    pub message: Option<String>,
}
impl Default for PreviewSnapshot {
    fn default() -> Self {
        Self {
            preview_id: None,
            sequence: 0,
            config_epoch: 0,
            phase: "idle".into(),
            source: None,
            frame_sequence: 0,
            source_width: 0,
            source_height: 0,
            last_frame_age_ms: None,
            message: None,
        }
    }
}
#[derive(Default)]
struct PreviewState {
    snapshot: PreviewSnapshot,
    request_id: Option<String>,
    seen: HashSet<String>,
    show_cursor: bool,
    owner: Option<String>,
    recording: bool,
    running: bool,
    handoff: bool,
    stop: Option<Arc<AtomicBool>>,
    lease: Option<ActivityLease>,
    consumer: Option<Instant>,
    latest: Option<Frame>,
    arrived: Option<Instant>,
    #[cfg(test)]
    waiters: usize,
}
#[derive(Default)]
pub(super) struct PreviewManager {
    inner: Mutex<PreviewState>,
    done: Condvar,
    windows: Mutex<Vec<(String, WindowIdentity)>>,
    next: std::sync::atomic::AtomicU64,
}
fn next(counter: &std::sync::atomic::AtomicU64) -> Result<u64, String> {
    crate::activity::allocate_operation_id(counter, 9_007_199_254_740_991)
        .ok_or_else(|| "Restart the app before another preview.".into())
}
impl PreviewManager {
    fn publish(&self, app: &AppHandle) {
        let _ = app.emit("recording-preview-status", self.snapshot());
    }
    pub fn snapshot(&self) -> PreviewSnapshot {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = i.snapshot.clone();
        result.last_frame_age_ms = i
            .arrived
            .map(|at| at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        result
    }
    fn clear(i: &mut PreviewState, phase: &str, message: Option<String>) {
        i.snapshot.config_epoch = i.snapshot.config_epoch.saturating_add(1);
        i.snapshot.sequence = i.snapshot.sequence.saturating_add(1);
        i.snapshot.phase = phase.into();
        i.snapshot.message = message;
        i.latest = None;
        i.arrived = None;
        i.consumer = None;
    }
    pub fn shutdown(&self) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !i.recording
            && let Some(stop) = &i.stop
        {
            stop.store(true, Ordering::Release);
        }
        Self::clear(&mut i, "idle", None);
    }
    pub fn register_windows(&self, list: WindowListing) -> Result<Value, String> {
        let mut registry = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let mut fresh = Vec::new();
        let mut public = Vec::new();
        for info in list.windows.into_iter().take(128) {
            let id = match registry
                .iter()
                .find(|(_, identity)| *identity == info.identity)
            {
                Some((id, _)) => id.clone(),
                None => format!("window-{}", next(&self.next)?),
            };
            fresh.push((id.clone(), info.identity));
            public.push(json!({"id":id,"name":info.name,"processName":info.process_name,"width":info.width,"height":info.height,"displayId":info.display_id,"available":info.available,"reason":info.reason,"protection":info.protection,"method":"wgc-window"}));
        }
        *registry = fresh;
        Ok(json!({"windows":public,"truncated":list.truncated}))
    }
    pub fn native_source(&self, source: &Source) -> Result<source::NativeSource, String> {
        source.validate()?;
        match source {
            Source::Region(region) => Ok(source::NativeSource::Region(region.clone())),
            Source::Window { window_id } => {
                let registry = self
                    .windows
                    .lock()
                    .map_err(|_| "Window selection is unavailable.")?;
                let identity = registry
                    .iter()
                    .find(|(id, _)| id == window_id)
                    .map(|(_, identity)| identity.clone())
                    .ok_or(
                        "The selected window is no longer available. Refresh and select it again.",
                    )?;
                Ok(source::NativeSource::Window { identity })
            }
        }
    }
    pub fn recording_owner(&self, owner: &str) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::clear(&mut i, "idle", None);
        i.snapshot.preview_id = None;
        i.request_id = None;
        i.owner = Some(owner.into());
        i.recording = true;
        i.running = false;
        i.stop = None;
    }
    pub fn finish_recording(&self, app: &AppHandle, owner: &str) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(owner) || !i.recording {
            return;
        }
        Self::clear(&mut i, "idle", None);
        i.owner = None;
        i.recording = false;
        drop(i);
        self.publish(app);
    }
    pub fn stop_wait(&self, handoff: bool) -> Result<Option<ActivityLease>, String> {
        self.stop_wait_owner(handoff, None)
    }
    pub fn stop_wait_owner(
        &self,
        handoff: bool,
        owner: Option<&str>,
    ) -> Result<Option<ActivityLease>, String> {
        let mut i = self.inner.lock().map_err(|_| "Preview is unavailable.")?;
        if owner.is_some_and(|owner| i.owner.as_deref() != Some(owner)) {
            return Ok(None);
        }
        if i.recording {
            return Ok(None);
        }
        let expected_owner = i.owner.clone();
        // A stale ordinary Stop must never revoke recording's retained lease.
        i.handoff |= handoff;
        if let Some(stop) = &i.stop {
            stop.store(true, Ordering::Release);
        }
        Self::clear(&mut i, "stopping", None);
        let deadline = Instant::now() + Duration::from_secs(24);
        while i.running {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                if handoff {
                    i.handoff = false;
                }
                return Err("Preview is still stopping. Wait and try again.".into());
            }
            #[cfg(test)]
            {
                i.waiters += 1;
            }
            let (state, _) = self
                .done
                .wait_timeout(i, remaining)
                .map_err(|_| "Preview is unavailable.")?;
            i = state;
            #[cfg(test)]
            {
                i.waiters -= 1;
            }
            if i.owner != expected_owner {
                return Ok(None);
            }
        }
        if i.owner != expected_owner || (!handoff && i.handoff) {
            return Ok(None);
        }
        i.owner = None;
        i.stop = None;
        i.snapshot.phase = "idle".into();
        let lease = i.lease.take();
        i.handoff = false;
        Ok(lease)
    }
    fn finish(&self, app: &AppHandle, owner: &str, result: Result<Value, String>) {
        let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(owner) || i.recording {
            return;
        }
        let stopped = i
            .stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Acquire));
        Self::clear(
            &mut i,
            if result.is_ok() || stopped {
                "idle"
            } else {
                "failed"
            },
            result.err().filter(|_| !stopped),
        );
        i.running = false;
        i.stop = None;
        if !i.handoff {
            i.lease = None;
            i.owner = None;
        }
        self.done.notify_all();
        drop(i);
        self.publish(app);
    }
    pub fn stop_request(
        &self,
        app: &AppHandle,
        preview_id: Option<&str>,
        request_id: Option<&str>,
    ) -> Result<PreviewSnapshot, String> {
        if preview_id.is_some() == request_id.is_some() {
            return Err("Identify exactly one preview or preview request.".into());
        }
        let mut i = self.inner.lock().map_err(|_| "Preview is unavailable.")?;
        if let Some(request) = request_id {
            if request.is_empty() || request.len() > 100 {
                return Err("Invalid preview request.".into());
            }
            if i.seen.len() >= 4096 && !i.seen.contains(request) {
                return Err("Restart the app before another preview request.".into());
            }
            i.seen.insert(request.into());
        }
        let matches = preview_id.is_some_and(|id| i.snapshot.preview_id.as_deref() == Some(id))
            || request_id.is_some_and(|id| i.request_id.as_deref() == Some(id));
        if matches {
            if !i.recording
                && let Some(stop) = &i.stop
            {
                stop.store(true, Ordering::Release);
            }
            let phase = if i.running && !i.recording {
                "stopping"
            } else {
                "idle"
            };
            Self::clear(&mut i, phase, None);
        }
        drop(i);
        self.publish(app);
        Ok(self.snapshot())
    }
    pub fn renew(
        &self,
        app: &AppHandle,
        id: &str,
        visible: bool,
    ) -> Result<PreviewSnapshot, String> {
        let mut i = self.inner.lock().map_err(|_| "Preview is unavailable.")?;
        if i.snapshot.preview_id.as_deref() != Some(id) {
            return Err("This preview is no longer active.".into());
        }
        if !visible {
            if !i.recording
                && let Some(stop) = &i.stop
            {
                stop.store(true, Ordering::Release);
            }
            Self::clear(&mut i, "idle", None);
        } else if matches!(
            i.snapshot.phase.as_str(),
            "starting" | "previewing" | "recording"
        ) {
            i.consumer = Some(Instant::now());
        }
        drop(i);
        if !visible {
            self.publish(app);
        }
        Ok(self.snapshot())
    }
    pub fn frame(&self, id: &str, after: u64) -> Result<Vec<u8>, String> {
        let i = self.inner.lock().map_err(|_| "Preview is unavailable.")?;
        if i.snapshot.preview_id.as_deref() != Some(id) {
            return Err("This preview is no longer active.".into());
        }
        Ok(i.latest
            .as_ref()
            .filter(|frame| {
                frame.header.config_epoch == i.snapshot.config_epoch
                    && frame.header.sequence > after
            })
            .map(|frame| frame.bytes.clone())
            .unwrap_or_default())
    }
}
#[derive(Clone)]
pub(super) struct Tap {
    pub manager: Arc<PreviewManager>,
    pub app: AppHandle,
    pub owner: String,
}
impl Tap {
    pub fn control(&self) -> Value {
        let i = self.manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(&self.owner) {
            return json!({"operation":"preview-off"});
        }
        let active = i
            .consumer
            .is_some_and(|at| at.elapsed() < Duration::from_millis(preview::LEASE_MS))
            && matches!(
                i.snapshot.phase.as_str(),
                "starting" | "previewing" | "recording"
            );
        if active {
            json!({"operation":"preview-renew","previewId":i.snapshot.preview_id,"configEpoch":i.snapshot.config_epoch,"leaseMs":i.consumer.map(|at|preview::LEASE_MS.saturating_sub(at.elapsed().as_millis() as u64)).unwrap_or(0)})
        } else {
            json!({"operation":"preview-off"})
        }
    }
    pub fn accept(&self, frame: Frame) {
        let mut i = self.manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(&self.owner)
            || i.snapshot.preview_id.as_deref() != Some(frame.header.preview_id.as_str())
            || i.snapshot.config_epoch != frame.header.config_epoch
            || frame.header.sequence <= i.snapshot.frame_sequence
            || i.consumer
                .is_none_or(|at| at.elapsed() >= Duration::from_millis(preview::LEASE_MS))
        {
            return;
        }
        let was_starting = i.snapshot.phase == "starting";
        i.snapshot.frame_sequence = frame.header.sequence;
        i.snapshot.sequence += 1;
        i.snapshot.phase = if i.recording {
            "recording"
        } else {
            "previewing"
        }
        .into();
        i.latest = Some(frame);
        i.arrived = Some(Instant::now());
        drop(i);
        if was_starting {
            self.manager.publish(&self.app);
        }
    }
    pub fn fail(&self, reason: &str) {
        let mut i = self.manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(&self.owner) {
            return;
        }
        PreviewManager::clear(&mut i, "failed", Some(reason.into()));
        drop(i);
        self.manager.publish(&self.app);
    }
    pub fn progress(&self, value: &Value) {
        let Some(notice) = value.get("preview") else {
            return;
        };
        let mut i = self.manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.owner.as_deref() != Some(&self.owner)
            || notice["previewId"].as_str() != i.snapshot.preview_id.as_deref()
        {
            return;
        }
        let epoch = notice["configEpoch"].as_u64().unwrap_or(0);
        if epoch < i.snapshot.config_epoch {
            return;
        }
        if epoch == 0 || epoch > 9_007_199_254_740_991 {
            return;
        }
        let geometry = serde_json::from_value::<Geometry>(
            json!({"width":notice["sourceWidth"],"height":notice["sourceHeight"]}),
        );
        let Ok(geometry) = geometry else {
            return;
        };
        if geometry.validate().is_err() {
            return;
        }
        if epoch > i.snapshot.config_epoch {
            i.latest = None;
            i.arrived = None;
            i.snapshot.config_epoch = epoch;
            i.snapshot.phase = "starting".into();
            i.snapshot.message = None;
        }
        i.snapshot.source_width = geometry.width;
        i.snapshot.source_height = geometry.height;
        if notice["expired"] == true {
            PreviewManager::clear(
                &mut i,
                "idle",
                Some("Preview stopped because its consumer disconnected.".into()),
            );
        }
        i.snapshot.sequence += 1;
        drop(i);
        self.manager.publish(&self.app);
    }
}

#[tauri::command]
pub(crate) async fn recording_windows(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
) -> Result<Value, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    crate::command_dispatch::files(move || {
        let value = helper_call(
            &app,
            json!({"operation":"windows","excludedPid":std::process::id()}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )?;
        let list: WindowListing = serde_json::from_value(value).map_err(error)?;
        manager.preview.register_windows(list)
    })
    .await
}
#[tauri::command]
pub(crate) fn recording_preview_start(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    request_id: String,
    source: Source,
    show_cursor: bool,
) -> Result<PreviewSnapshot, String> {
    main_window(&window)?;
    if !window.is_visible().map_err(error)? || window.is_minimized().map_err(error)? {
        return Err("Open the Record workspace before starting preview.".into());
    }
    if request_id.is_empty() || request_id.len() > 100 {
        return Err("Invalid preview request.".into());
    }
    source.validate()?;
    let native = state.preview.native_source(&source)?;
    let record = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if record.selecting.is_some() {
        return Err("Finish selecting the recording area first.".into());
    }
    let recording = active(&record.snapshot.phase);
    if recording
        && record
            .snapshot
            .config
            .as_ref()
            .is_none_or(|config| config.source != source || config.show_cursor != show_cursor)
    {
        return Err("Preview must match the active recording source and cursor.".into());
    }
    let mut i = state
        .preview
        .inner
        .lock()
        .map_err(|_| "Preview is unavailable.")?;
    if i.request_id.as_deref() == Some(&request_id) {
        if i.snapshot.source.as_ref() != Some(&source) || i.show_cursor != show_cursor {
            return Err("This preview request belongs to different settings.".into());
        }
        return Ok(i.snapshot.clone());
    }
    if i.seen.contains(&request_id) {
        return Err("This preview request was cancelled or retired.".into());
    }
    if i.seen.len() >= 4096 {
        return Err("Restart the app before another preview.".into());
    }
    if i.running
        || i.handoff
        || matches!(
            i.snapshot.phase.as_str(),
            "starting" | "previewing" | "recording"
        )
    {
        return Err("Stop the current preview before starting another.".into());
    }
    let new_lease = if recording {
        if i.owner.as_deref() != record.snapshot.session_id.as_deref() || !i.recording {
            return Err("The recorder is still preparing. Retry preview when ready.".into());
        }
        None
    } else {
        Some(activity.begin("recording preview")?)
    };
    let id = format!(
        "preview-{}-{}",
        std::process::id(),
        next(&state.preview.next)?
    );
    i.seen.insert(request_id.clone());
    i.request_id = Some(request_id);
    i.show_cursor = show_cursor;
    i.snapshot = PreviewSnapshot {
        preview_id: Some(id.clone()),
        sequence: i.snapshot.sequence + 1,
        config_epoch: 1,
        phase: "starting".into(),
        source: Some(source),
        ..PreviewSnapshot::default()
    };
    i.consumer = Some(Instant::now());
    i.latest = None;
    i.arrived = None;
    if recording {
        return Ok(i.snapshot.clone());
    }
    let lease = new_lease.expect("standalone preview owns admission");
    let stop = Arc::new(AtomicBool::new(false));
    let owner = id.clone();
    i.owner = Some(owner.clone());
    i.recording = false;
    i.running = true;
    i.stop = Some(stop.clone());
    i.lease = Some(lease);
    let snapshot = i.snapshot.clone();
    drop(i);
    drop(record);
    let preview = state.preview.clone();
    let tap = Tap {
        manager: preview.clone(),
        app: app.clone(),
        owner: owner.clone(),
    };
    let failed_preview = preview.clone();
    let failed_app = app.clone();
    let failed_owner = owner.clone();
    if std::thread::Builder::new().name("recording-preview".into()).spawn(move || {
        let result = helper_call_stream(
            &app,
            json!({"operation":"preview","source":native,"showCursor":show_cursor,"previewId":id}),
            stop,
            None,
            |_| {},
            Some(tap),
            None,
        );
        preview.finish(&app, &owner, result);
    }).is_err() {
        failed_preview.finish(&failed_app, &failed_owner, Err("Cannot start preview worker.".into()));
        return Err("Cannot start preview worker.".into());
    }
    Ok(snapshot)
}
#[tauri::command]
pub(crate) fn recording_preview_snapshot(
    window: Window,
    state: State<'_, RecordingManager>,
    preview_id: String,
    app: AppHandle,
) -> Result<PreviewSnapshot, String> {
    main_window(&window)?;
    state.preview.renew(
        &app,
        &preview_id,
        window.is_visible().map_err(error)? && !window.is_minimized().map_err(error)?,
    )
}
#[tauri::command]
pub(crate) fn recording_preview_frame(
    window: Window,
    state: State<'_, RecordingManager>,
    preview_id: String,
    after_sequence: u64,
    app: AppHandle,
) -> Result<tauri::ipc::Response, String> {
    main_window(&window)?;
    state.preview.renew(
        &app,
        &preview_id,
        window.is_visible().map_err(error)? && !window.is_minimized().map_err(error)?,
    )?;
    Ok(tauri::ipc::Response::new(
        state.preview.frame(&preview_id, after_sequence)?,
    ))
}
#[tauri::command]
pub(crate) async fn recording_preview_stop(
    window: Window,
    state: State<'_, RecordingManager>,
    preview_id: Option<String>,
    request_id: Option<String>,
    app: AppHandle,
) -> Result<PreviewSnapshot, String> {
    main_window(&window)?;
    let owner = {
        let i = state
            .preview
            .inner
            .lock()
            .map_err(|_| "Preview is unavailable.")?;
        let matches = preview_id
            .as_deref()
            .is_some_and(|id| i.snapshot.preview_id.as_deref() == Some(id))
            || request_id
                .as_deref()
                .is_some_and(|id| i.request_id.as_deref() == Some(id));
        if matches && i.running && !i.recording {
            i.owner.clone()
        } else {
            None
        }
    };
    let result = state
        .preview
        .stop_request(&app, preview_id.as_deref(), request_id.as_deref())?;
    if let Some(owner) = owner {
        let preview = state.preview.clone();
        drop(
            tauri::async_runtime::spawn_blocking(move || {
                preview.stop_wait_owner(false, Some(&owner))
            })
            .await
            .map_err(error)??,
        );
        state.preview.publish(&app);
        return Ok(state.preview.snapshot());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn running(manager: &PreviewManager, owner: &str, lease: ActivityLease) -> Arc<AtomicBool> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut i = manager.inner.lock().unwrap();
        i.owner = Some(owner.into());
        i.running = true;
        i.stop = Some(stop.clone());
        i.lease = Some(lease);
        stop
    }
    fn wait_until_stopping(stop: &AtomicBool) {
        let until = Instant::now() + Duration::from_secs(3);
        while !stop.load(Ordering::Acquire) {
            assert!(Instant::now() < until, "waiter did not enter teardown");
            std::thread::yield_now();
        }
    }
    #[test]
    fn old_waiter_cannot_wait_on_or_clear_replacement_owner() {
        let manager = Arc::new(PreviewManager::default());
        let activity = ActivityCoordinator::default();
        let stop = running(&manager, "old", activity.begin("preview").unwrap());
        let other = manager.clone();
        let waiter = std::thread::spawn(move || other.stop_wait_owner(false, Some("old")));
        wait_until_stopping(&stop);
        // Hold the same mutex through old completion and replacement admission.
        // The waiter must observe the replacement before it can reacquire it.
        let replacement_stop = Arc::new(AtomicBool::new(false));
        {
            let mut i = manager.inner.lock().unwrap();
            i.running = false;
            i.lease = None;
            i.owner = Some("new".into());
            i.running = true;
            i.stop = Some(replacement_stop.clone());
            i.lease = Some(activity.begin("preview").unwrap());
            manager.done.notify_all();
        }
        assert!(waiter.join().unwrap().unwrap().is_none());
        let i = manager.inner.lock().unwrap();
        assert_eq!(i.owner.as_deref(), Some("new"));
        assert!(i.running && i.lease.is_some());
        assert!(!replacement_stop.load(Ordering::Acquire));
    }
    #[test]
    fn ordinary_stop_cannot_revoke_recording_handoff_lease() {
        let manager = Arc::new(PreviewManager::default());
        let activity = ActivityCoordinator::default();
        let stop = running(&manager, "old", activity.begin("preview").unwrap());
        let transfer_manager = manager.clone();
        let transfer = std::thread::spawn(move || transfer_manager.stop_wait(true));
        wait_until_stopping(&stop);
        assert!(manager.inner.lock().unwrap().handoff);
        let ordinary_manager = manager.clone();
        let ordinary =
            std::thread::spawn(move || ordinary_manager.stop_wait_owner(false, Some("old")));
        let until = Instant::now() + Duration::from_secs(3);
        while manager.inner.lock().unwrap().waiters != 2 {
            assert!(Instant::now() < until, "both teardown waiters must enter");
            std::thread::yield_now();
        }
        // Completing under lock models the worker's retained-lease finish.
        {
            let mut i = manager.inner.lock().unwrap();
            assert!(i.handoff);
            i.running = false;
            manager.done.notify_all();
        }
        let lease = transfer
            .join()
            .unwrap()
            .unwrap()
            .expect("retained admission");
        assert!(ordinary.join().unwrap().unwrap().is_none());
        assert!(activity.begin("export").is_err());
        drop(lease);
        assert!(activity.begin("export").is_ok());
    }
    #[test]
    fn retired_owner_stop_does_not_touch_recording_or_new_preview() {
        let manager = PreviewManager::default();
        manager.recording_owner("recording");
        assert!(
            manager
                .stop_wait_owner(false, Some("retired"))
                .unwrap()
                .is_none()
        );
        let i = manager.inner.lock().unwrap();
        assert_eq!(i.owner.as_deref(), Some("recording"));
        assert!(i.recording);
    }
}
