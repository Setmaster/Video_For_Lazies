//! Single-owner selected-audio preview preparation. Cancellation never waits on
//! the ordinary media lane; borrowed sources are never preview-owned files.
use crate::activity::ActivityCoordinator;
use crate::media_source::SourceIdentity;
use crate::process::{self, JobContext, JobScope, ManagedChild, ProcessGroup};
use crate::video::{self, VideoProbe};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{Manager, State, Window};
use tempfile::TempDir;

const MIB: u64 = 1024 * 1024;
const HISTORY: usize = 128;
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum AudioPreviewAsset {
    #[serde(rename_all = "camelCase")]
    Original {
        request_id: String,
        proxy_id: Option<String>,
        path: String,
        source_identity: String,
        audio_stream_index: u32,
        timeline_offset_s: f64,
    },
    #[serde(rename_all = "camelCase")]
    Proxy {
        request_id: String,
        proxy_id: String,
        path: String,
        source_identity: String,
        audio_stream_index: u32,
        timeline_offset_s: f64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Input {
    path: String,
    index: u32,
    identity: String,
}
struct Entry {
    input: Option<Input>,
    cancel: Arc<AtomicBool>,
    group: ProcessGroup,
    result: Mutex<Option<Result<AudioPreviewAsset, String>>>,
    settled: Condvar,
    clock: Instant,
    deadline_ms: AtomicU64,
    timed_out: AtomicBool,
}
impl Entry {
    fn new(input: Option<Input>) -> Self {
        Self {
            input,
            cancel: Arc::new(AtomicBool::new(false)),
            group: Arc::new(Mutex::new(Vec::new())),
            result: Mutex::new(None),
            settled: Condvar::new(),
            clock: Instant::now(),
            deadline_ms: AtomicU64::new(120_000),
            timed_out: AtomicBool::new(false),
        }
    }
    fn wait(&self) -> Result<AudioPreviewAsset, String> {
        let result = {
            let mut result = self.result.lock().unwrap_or_else(|e| e.into_inner());
            while result.is_none() {
                result = self.settled.wait(result).unwrap_or_else(|e| e.into_inner());
            }
            result.as_ref().unwrap().clone()
        };
        if result.is_ok() {
            check_cancel(self)?;
            // Identical request retries may outlive the original operation's
            // held source handle. Never return stale ready bytes by identity.
            if let Some(input) = &self.input {
                let source = SourceIdentity::capture(Path::new(&input.path))?;
                source.verify_expected(Some(&input.identity))?;
            }
            check_cancel(self)?;
        }
        result
    }

    fn finish(&self, result: Result<AudioPreviewAsset, String>) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        self.settled.notify_all();
    }
}
#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    order: VecDeque<String>,
    active: Option<String>,
    directory: Option<TempDir>,
    scope: Option<tauri::fs::Scope>,
    assets: HashMap<String, OwnedAsset>,
    closed: bool,
    next_asset: u64,
}
struct OwnedAsset {
    request_id: String,
    path: PathBuf,
}
#[derive(Clone)]
pub(crate) struct AudioPreviewManager {
    inner: Arc<Mutex<Registry>>,
    activity: ActivityCoordinator,
}
impl AudioPreviewManager {
    pub(crate) fn new(activity: ActivityCoordinator) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Registry::default())),
            activity,
        }
    }
    fn reserve_history(reg: &mut Registry) -> Result<(), String> {
        if reg.entries.len() < HISTORY {
            return Ok(());
        }
        let candidate = reg.order.iter().position(|id| {
            reg.entries.get(id).is_some_and(|e| {
                e.cancel.load(Ordering::Acquire)
                    && e.result.lock().unwrap_or_else(|e| e.into_inner()).is_some()
            }) && reg.active.as_ref() != Some(id)
                && !reg.assets.values().any(|a| &a.request_id == id)
        });
        if let Some(i) = candidate {
            let id = reg.order.remove(i).unwrap();
            reg.entries.remove(&id);
            Ok(())
        } else {
            Err(
                "Audio preview request history is full. Close obsolete previews before retrying."
                    .into(),
            )
        }
    }
    fn start(&self, window: Window, id: String, input: Input) -> Result<Arc<Entry>, String> {
        valid_request_id(&id)?;
        if input.path.len() > 32768 || input.path.contains('\0') || input.identity.len() != 64 {
            return Err("Invalid audio preview input binding.".into());
        }
        let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if reg.closed {
            return Err("The preview manager is shutting down.".into());
        }
        if let Some(entry) = reg.entries.get(&id) {
            if entry.input.as_ref() != Some(&input) && entry.input.is_some() {
                return Err("Conflicting audio preview request ID.".into());
            }
            return Ok(entry.clone());
        }
        Self::reserve_history(&mut reg)?;
        if reg.active.is_some() {
            return Err(
                "Cancel the previous audio preview preparation before starting another.".into(),
            );
        }
        let lease = self.activity.begin("audio preview preparation")?;
        let entry = Arc::new(Entry::new(Some(input.clone())));
        reg.entries.insert(id.clone(), entry.clone());
        reg.order.push_back(id.clone());
        reg.active = Some(id.clone());
        drop(reg);
        let manager = self.clone();
        let worker = entry.clone();
        let launch = std::thread::Builder::new()
            .name("audio-preview".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _watchdog = DeadlineGuard::new(worker.clone());
                    let _scope = JobScope::enter(JobContext {
                        cancel: worker.cancel.clone(),
                        child: worker.group.clone(),
                    });
                    manager.prepare(&window, &id, &input, &worker)
                }))
                .unwrap_or_else(|_| Err("Audio preview worker stopped unexpectedly.".into()));
                let result = if worker.timed_out.load(Ordering::Acquire) {
                    Err(
                        "Selected audio preview exceeded its input-scaled preparation deadline."
                            .into(),
                    )
                } else {
                    result
                };
                // All process/reader/file guards have settled before releasing admission.
                let mut reg = manager.inner.lock().unwrap_or_else(|e| e.into_inner());
                let result = if worker.cancel.load(Ordering::Acquire) || result.is_err() {
                    let cleanup = remove_request_assets(&mut reg, &id);
                    if let Err(error) = cleanup {
                        Err(error)
                    } else {
                        result.and_then(|_| Err("Audio preview cancelled.".into()))
                    }
                } else {
                    result
                };
                drop(lease);
                reg.active = None;
                worker.finish(result);
            });
        if let Err(error) = launch {
            let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            reg.active = None;
            entry.finish(Err(format!("Cannot start audio preview worker: {error}")));
        }
        Ok(entry)
    }
    fn prepare(
        &self,
        window: &Window,
        id: &str,
        input: &Input,
        entry: &Entry,
    ) -> Result<AudioPreviewAsset, String> {
        check_cancel(entry)?;
        let identity = SourceIdentity::capture(Path::new(&input.path))?;
        identity.verify_expected(Some(&input.identity))?;
        let probe = video::probe_video_selected(
            input.path.clone(),
            Some(input.index),
            Some(input.identity.clone()),
        )?;
        check_cancel(entry)?;
        identity.verify(Path::new(&input.path))?;
        if probe.preview_original_safe {
            return Ok(AudioPreviewAsset::Original {
                request_id: id.into(),
                proxy_id: None,
                path: identity.path().to_string_lossy().into(),
                source_identity: input.identity.clone(),
                audio_stream_index: input.index,
                timeline_offset_s: 0.0,
            });
        }
        if probe.video_codec.as_deref() != Some("h264") || probe.unsupported_rotation_deg.is_some()
        {
            return Err("Selected-track preview requires compatible H.264 video. The selected audio remains available for export.".into());
        }
        let transcode = probe.audio_codec.as_deref() != Some("aac");
        let budget = Budget::for_input(identity.len, probe.duration_s, transcode)?;
        entry.deadline_ms.store(
            budget
                .deadline
                .as_millis()
                .try_into()
                .map_err(|_| "Audio preview deadline overflow.")?,
            Ordering::Release,
        );
        check_cancel(entry)?;
        let (proxy_id, path, root) = {
            let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if reg.assets.len() >= 2 {
                return Err(
                    "Unload and release the previous audio preview before preparing another."
                        .into(),
                );
            }
            if reg.directory.is_none() {
                let dir = tempfile::Builder::new()
                    .prefix("vfl-audio-preview-")
                    .tempdir()
                    .map_err(|e| format!("Cannot create audio preview storage: {e}"))?;
                // One session-only nonrecursive grant; never allow the source or general temp root here.
                window
                    .app_handle()
                    .asset_protocol_scope()
                    .allow_directory(dir.path(), false)
                    .map_err(|e| e.to_string())?;
                reg.scope = Some(window.app_handle().asset_protocol_scope().clone());
                reg.directory = Some(dir);
            }
            let root = reg
                .directory
                .as_ref()
                .unwrap()
                .path()
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let free = available_space(&root)?;
            budget.check_start(free)?;
            reg.next_asset = reg
                .next_asset
                .checked_add(1)
                .ok_or("Restart before creating another audio preview.")?;
            let proxy_id = format!("preview-{}", reg.next_asset);
            let path = root.join(format!("{proxy_id}.mp4"));
            reg.assets.insert(
                proxy_id.clone(),
                OwnedAsset {
                    request_id: id.into(),
                    path: path.clone(),
                },
            );
            (proxy_id, path, root)
        };
        let args = proxy_args(
            &identity.path().to_string_lossy(),
            &path,
            &probe,
            transcode,
            budget.reservation,
        );
        run_proxy(&args, &path, &root, &budget, entry)?;
        check_cancel(entry)?;
        identity.verify(Path::new(&input.path))?;
        let actual = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
        if !actual.is_file()
            || actual.len() == 0
            || actual.len() > budget.reservation
            || path.canonicalize().map_err(|e| e.to_string())?.parent() != Some(root.as_path())
        {
            return Err("Audio preview storage validation failed.".into());
        }
        let ready = video::probe_video(path.to_string_lossy().into())?;
        let offset = validate_proxy(&probe, &ready)?;
        check_cancel(entry)?;
        identity.verify(Path::new(&input.path))?;
        Ok(AudioPreviewAsset::Proxy {
            request_id: id.into(),
            proxy_id,
            path: path.to_string_lossy().into(),
            source_identity: input.identity.clone(),
            audio_stream_index: input.index,
            timeline_offset_s: offset,
        })
    }
    pub(crate) fn cancel(&self, id: &str) -> Result<(), String> {
        valid_request_id(id)?;
        let entry = {
            let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = reg.entries.get(id) {
                entry.clone()
            } else {
                Self::reserve_history(&mut reg)?;
                let entry = Arc::new(Entry::new(None));
                entry.cancel.store(true, Ordering::Release);
                entry.finish(Err("Audio preview cancelled before preparation.".into()));
                reg.entries.insert(id.into(), entry.clone());
                reg.order.push_back(id.into());
                entry
            }
        };
        entry.cancel.store(true, Ordering::Release);
        process::kill_process_group(&entry.group);
        let _ = entry.wait();
        let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        remove_request_assets(&mut reg, id)
    }
    pub(crate) fn release(&self, id: &str, proxy_id: &str) -> Result<(), String> {
        valid_request_id(id)?;
        if proxy_id.is_empty() || proxy_id.len() > 100 {
            return Err("Invalid owned audio preview ID.".into());
        }
        let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if reg.active.as_deref() == Some(id) {
            return Err("Cancel active preview preparation before releasing it.".into());
        }
        if let Some(entry) = reg.entries.get(id) {
            match entry
                .result
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                Some(Ok(AudioPreviewAsset::Original { .. })) => {
                    return Err(
                        "The original media source is borrowed and cannot be released as a proxy."
                            .into(),
                    );
                }
                Some(Ok(AudioPreviewAsset::Proxy {
                    proxy_id: owned, ..
                })) if owned != proxy_id => {
                    return Err("Audio preview ownership does not match.".into());
                }
                _ => {}
            }
        }
        if let Some(asset) = reg.assets.get(proxy_id) {
            if asset.request_id != id {
                return Err("Audio preview ownership does not match.".into());
            }
            if reg.active.as_deref() == Some(id) {
                return Err("Cancel active preview preparation before releasing it.".into());
            }
            match fs::remove_file(&asset.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(format!(
                        "Could not release audio preview; unload it and retry: {e}"
                    ));
                }
            }
            reg.assets.remove(proxy_id);
        }
        if let Some(entry) = reg.entries.get(id) {
            entry.cancel.store(true, Ordering::Release);
        }
        Ok(())
    }
    /// Reusable normal-close/cancel cleanup. It never latches admission closed.
    pub(crate) fn cancel_all(&self) -> Result<(), String> {
        let entries = {
            let reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            reg.entries.keys().cloned().collect::<Vec<_>>()
        };
        let mut error = None;
        for id in entries {
            if let Err(e) = self.cancel(&id) {
                error = Some(e);
            }
        }
        error.map_or(Ok(()), Err)
    }
    pub(crate) fn needs_close_guard(&self) -> bool {
        let reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        !reg.closed && (reg.active.is_some() || !reg.assets.is_empty())
    }
    /// Destruction backstop only; no waiting or normal-close admission latch.
    pub(crate) fn signal_shutdown(&self) {
        let entries = {
            let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            reg.closed = true;
            reg.entries.values().cloned().collect::<Vec<_>>()
        };
        for entry in entries {
            entry.cancel.store(true, Ordering::Release);
            process::kill_process_group(&entry.group);
        }
    }
    pub(crate) fn shutdown(&self) -> Result<(), String> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.cancel_all()?;
        let mut reg = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(directory) = reg.directory.as_ref()
            && let Some(scope) = &reg.scope
        {
            scope
                .forbid_directory(directory.path(), false)
                .map_err(|e| e.to_string())?;
        }
        if let Some(directory) = reg.directory.take() {
            directory
                .close()
                .map_err(|e| format!("Could not remove owned audio preview directory: {e}"))?;
        }
        Ok(())
    }
}
fn remove_request_assets(reg: &mut Registry, id: &str) -> Result<(), String> {
    let keys = reg
        .assets
        .iter()
        .filter(|(_, a)| a.request_id == id)
        .map(|(k, _)| k.clone())
        .collect::<Vec<_>>();
    for key in keys {
        let asset = &reg.assets[&key];
        match fs::remove_file(&asset.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "Audio preview cleanup is pending; unload it and retry: {e}"
                ));
            }
        }
        reg.assets.remove(&key);
    }
    Ok(())
}
fn valid_request_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 100
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Err("Invalid audio preview request ID.".into())
    } else {
        Ok(())
    }
}
fn check_cancel(entry: &Entry) -> Result<(), String> {
    if entry.cancel.load(Ordering::Acquire) {
        Err("Audio preview cancelled.".into())
    } else {
        Ok(())
    }
}
struct DeadlineGuard {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl DeadlineGuard {
    fn new(entry: Arc<Entry>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                if entry.clock.elapsed().as_millis()
                    > u128::from(entry.deadline_ms.load(Ordering::Acquire))
                {
                    entry.timed_out.store(true, Ordering::Release);
                    entry.cancel.store(true, Ordering::Release);
                    process::kill_process_group(&entry.group);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[derive(Debug)]
struct Budget {
    reservation: u64,
    headroom: u64,
    deadline: Duration,
}
impl Budget {
    fn for_input(bytes: u64, duration: f64, transcode: bool) -> Result<Self, String> {
        if bytes == 0 || !duration.is_finite() || duration <= 0.0 {
            return Err("Cannot estimate selected-track preview storage.".into());
        }
        let audio = if transcode {
            let value = duration * 192000.0 / 8.0 * 1.1;
            if value > u64::MAX as f64 {
                return Err("Audio preview storage estimate overflow.".into());
            }
            value.ceil() as u64
        } else {
            0
        };
        let reservation = bytes
            .checked_add(bytes.div_ceil(20).max(64 * MIB))
            .and_then(|v| v.checked_add(audio))
            .ok_or("Audio preview storage estimate overflow.")?;
        let headroom = reservation.div_ceil(20).max(256 * MIB);
        let seconds =
            120.0 + reservation as f64 / MIB as f64 + if transcode { duration * 2.0 } else { 0.0 };
        if !seconds.is_finite() || seconds > u64::MAX as f64 / 2.0 {
            return Err("Audio preview deadline estimate overflow.".into());
        }
        Ok(Self {
            reservation,
            headroom,
            deadline: Duration::from_secs_f64(seconds),
        })
    }
    fn check_start(&self, free: u64) -> Result<(), String> {
        if free
            < self
                .reservation
                .checked_add(self.headroom)
                .ok_or("Audio preview storage estimate overflow.")?
        {
            Err(format!(
                "Selected audio preview needs {} bytes plus {} bytes headroom; {} bytes are available. Free space or export the selected track instead.",
                self.reservation, self.headroom, free
            ))
        } else {
            Ok(())
        }
    }
}
fn proxy_args(
    input: &str,
    path: &Path,
    probe: &VideoProbe,
    transcode: bool,
    reservation: u64,
) -> Vec<String> {
    let mut args = vec![
        "-nostdin".into(),
        "-n".into(),
        "-v".into(),
        "error".into(),
        "-copyts".into(),
        "-start_at_zero".into(),
        "-protocol_whitelist".into(),
        "file,pipe".into(),
        "-i".into(),
        input.into(),
        "-map".into(),
        format!("0:{}", probe.video_stream_index),
        "-map".into(),
        format!("0:{}", probe.audio_stream_index.unwrap()),
        "-map_metadata".into(),
        "0".into(),
        "-map_chapters".into(),
        "-1".into(),
        "-c:v".into(),
        "copy".into(),
        "-c:a".into(),
        if transcode {
            "aac".into()
        } else {
            "copy".into()
        },
    ];
    if transcode {
        args.extend([
            "-b:a".into(),
            "192k".into(),
            "-ar".into(),
            "48000".into(),
            "-ac".into(),
            "2".into(),
        ]);
    }
    args.extend([
        "-disposition:a:0".into(),
        "default".into(),
        "-avoid_negative_ts".into(),
        "disabled".into(),
        "-fs".into(),
        reservation.to_string(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-f".into(),
        "mp4".into(),
        path.to_string_lossy().into(),
    ]);
    args
}
fn validate_proxy(source: &VideoProbe, ready: &VideoProbe) -> Result<f64, String> {
    let invalid = || {
        "Selected audio preview did not preserve the complete video/audio interval and layout."
            .to_string()
    };
    if ready.audio_tracks.len() != 1
        || ready.audio_tracks_truncated
        || ready.video_stream_index != 0
        || ready.audio_stream_index != Some(1)
        || ready.video_codec.as_deref() != Some("h264")
        || ready.audio_codec.as_deref() != Some("aac")
        || ready.width != source.width
        || ready.height != source.height
        || ready.rotation_deg != source.rotation_deg
        || ready.sample_aspect_ratio != source.sample_aspect_ratio
        || ready.color_primaries != source.color_primaries
        || ready.color_transfer != source.color_transfer
        || ready.color_space != source.color_space
    {
        return Err(invalid());
    }
    let offset = ready.video_start_s - source.video_start_s;
    if !offset.is_finite() || (ready.duration_s - source.duration_s - offset).abs() > 0.2 {
        return Err(invalid());
    }
    for (a, b) in [
        (source.video_end_s, ready.video_end_s),
        (source.audio_end_s, ready.audio_end_s),
    ] {
        match (a, b) {
            (Some(a), Some(b)) if (b - a - offset).abs() <= 0.2 => {}
            _ => return Err(invalid()),
        }
    }
    match (source.audio_start_s, ready.audio_start_s) {
        (Some(a), Some(b)) if (b - a - offset).abs() <= 0.1 => {}
        _ => return Err(invalid()),
    }
    Ok(offset)
}
fn run_proxy(
    args: &[String],
    output: &Path,
    root: &Path,
    budget: &Budget,
    entry: &Entry,
) -> Result<(), String> {
    let bin = video::default_ffmpeg();
    let mut command = std::process::Command::new(bin);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    run_proxy_command(command, output, root, budget, entry)
}
type ReaderResult<T> = Option<std::thread::JoinHandle<std::io::Result<T>>>;
struct ProxyReaders {
    control: process::ProcessControl,
    progress: ReaderResult<()>,
    diagnostics: ReaderResult<Vec<u8>>,
}
impl Drop for ProxyReaders {
    fn drop(&mut self) {
        // Includes a pipe-reader launch failure or panic: no detached reader
        // can outlive the request's cancellation/admission ACK.
        let _ = self.control.kill();
        if let Some(reader) = self.progress.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.diagnostics.take() {
            let _ = reader.join();
        }
    }
}
fn run_proxy_command(
    command: std::process::Command,
    output: &Path,
    root: &Path,
    budget: &Budget,
    entry: &Entry,
) -> Result<(), String> {
    let mut child = ManagedChild::spawn(command)
        .map_err(|e| format!("Cannot start selected audio preview: {e}"))?;
    let progress = Arc::new(AtomicU64::new(0));
    let clock = Instant::now();
    let stdout = child
        .stdout
        .take()
        .ok_or("Missing preview progress pipe.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Missing preview diagnostic pipe.")?;
    let mut readers = ProxyReaders {
        control: child.control.clone(),
        progress: None,
        diagnostics: None,
    };
    readers.progress = Some({
        let progress = progress.clone();
        std::thread::Builder::new()
            .name("audio-preview-progress".into())
            .spawn(move || {
                let mut input = stdout;
                let mut buffer = [0u8; 4096];
                let mut line = Vec::new();
                let mut last = 0_u64;
                loop {
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    for &b in &buffer[..n] {
                        if b == b'\n' {
                            if let Ok(s) = std::str::from_utf8(&line)
                                && let Some(v) = s
                                    .strip_prefix("out_time_us=")
                                    .and_then(|v| v.parse::<u64>().ok())
                                && v > last
                            {
                                last = v;
                                progress
                                    .store(clock.elapsed().as_millis() as u64, Ordering::Relaxed);
                            }
                            line.clear();
                        } else if line.len() < 1024 {
                            line.push(b);
                        }
                    }
                }
                Ok::<_, std::io::Error>(())
            })
            .map_err(|e| format!("Cannot start preview progress reader: {e}"))?
    });
    readers.diagnostics = Some(
        std::thread::Builder::new()
            .name("audio-preview-diagnostics".into())
            .spawn(move || {
                let mut input = stderr;
                let mut tail = VecDeque::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = input.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    for &b in &buf[..n] {
                        if tail.len() == 64 * 1024 {
                            tail.pop_front();
                        }
                        tail.push_back(b);
                    }
                }
                Ok::<_, std::io::Error>(tail.into_iter().collect::<Vec<_>>())
            })
            .map_err(|e| format!("Cannot start preview diagnostic reader: {e}"))?,
    );
    let mut last_size = 0;
    let mut last_change = Instant::now();
    let mut failure = None;
    let status = loop {
        match child.wait_timeout(Duration::from_millis(250)) {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(e) => {
                failure = Some(e.to_string());
                break None;
            }
        }
        let size = fs::metadata(output).map_or(0, |m| m.len());
        if size > last_size {
            last_size = size;
            last_change = Instant::now();
        }
        let last_progress = progress.load(Ordering::Relaxed);
        let progress_idle = (clock.elapsed().as_millis() as u64).saturating_sub(last_progress);
        if entry.cancel.load(Ordering::Acquire) {
            failure = Some("Audio preview cancelled.".into());
        } else if clock.elapsed() > budget.deadline
            || (last_change.elapsed() > Duration::from_secs(60) && progress_idle > 60_000)
        {
            failure=Some("Selected audio preview stopped making progress or exceeded its input-scaled deadline.".into());
        } else if size > budget.reservation
            || available_space(root).map_or(true, |free| free < budget.headroom)
        {
            failure = Some(
                "Selected audio preview exceeded its storage reservation or free-space headroom."
                    .into(),
            );
        }
        if failure.is_some() {
            break None;
        }
    };
    if failure.is_some() {
        let _ = child.control.kill();
        let _ = child.wait();
    }
    let progress_result = readers.progress.take().unwrap().join();
    let diagnostic_result = readers.diagnostics.take().unwrap().join();
    let tail = diagnostic_result
        .map_err(|_| "Preview diagnostic reader stopped.")?
        .map_err(|e| e.to_string())?;
    progress_result
        .map_err(|_| "Preview progress reader stopped.")?
        .map_err(|e| e.to_string())?;
    if let Some(error) = failure {
        return Err(error);
    }
    if !status.is_some_and(|s| s.success()) {
        return Err(format!(
            "Selected audio preview failed: {}",
            String::from_utf8_lossy(&tail)
        ));
    }
    Ok(())
}
#[cfg(windows)]
fn available_space(path: &Path) -> Result<u64, String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0;
    if unsafe {
        GetDiskFreeSpaceExW(
            path.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        Err("Cannot inspect preview storage capacity.".into())
    } else {
        Ok(free)
    }
}
#[cfg(target_os = "linux")]
fn available_space(path: &Path) -> Result<u64, String> {
    use std::os::unix::ffi::OsStrExt;
    #[repr(C)]
    struct Stat {
        bsize: u64,
        frsize: u64,
        blocks: u64,
        bfree: u64,
        bavail: u64,
        files: u64,
        ffree: u64,
        favail: u64,
        fsid: u64,
        flags: u64,
        namemax: u64,
        spare: [i32; 6],
    }
    unsafe extern "C" {
        fn statvfs(path: *const std::ffi::c_char, out: *mut Stat) -> i32;
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let mut stat = std::mem::MaybeUninit::<Stat>::uninit();
    if unsafe { statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err("Cannot inspect preview storage capacity.".into());
    }
    let stat = unsafe { stat.assume_init() };
    stat.bavail
        .checked_mul(stat.frsize)
        .ok_or_else(|| "Preview storage capacity overflow.".into())
}
#[cfg(not(any(windows, target_os = "linux")))]
fn available_space(_: &Path) -> Result<u64, String> {
    Err("Selected audio preview storage checks are unsupported on this platform.".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Cancelled {
    request_id: String,
    quiescent: bool,
}
#[tauri::command]
pub(crate) async fn prepare_audio_preview(
    window: Window,
    manager: State<'_, AudioPreviewManager>,
    request_id: String,
    path: String,
    audio_stream_index: u32,
    audio_source_identity: String,
) -> Result<AudioPreviewAsset, String> {
    // Registration/admission happen synchronously, before the first await.
    let entry = manager.start(
        window,
        request_id,
        Input {
            path,
            index: audio_stream_index,
            identity: audio_source_identity,
        },
    )?;
    tauri::async_runtime::spawn_blocking(move || entry.wait())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
pub(crate) async fn cancel_audio_preview(
    manager: State<'_, AudioPreviewManager>,
    request_id: String,
) -> Result<Cancelled, String> {
    let manager = manager.inner().clone();
    let id = request_id.clone();
    tauri::async_runtime::spawn_blocking(move || manager.cancel(&id))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Cancelled {
        request_id,
        quiescent: true,
    })
}
#[tauri::command]
pub(crate) async fn release_audio_preview(
    manager: State<'_, AudioPreviewManager>,
    request_id: String,
    proxy_id: String,
) -> Result<(), String> {
    let manager = manager.inner().clone();
    tauri::async_runtime::spawn_blocking(move || manager.release(&request_id, &proxy_id))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_scales_past_two_gib_and_preserves_free_space_headroom() {
        let small = Budget::for_input(MIB, 60.0, false).unwrap();
        let long = Budget::for_input(5 * 1024 * MIB, 3600.0, false).unwrap();
        assert!(long.reservation > 5 * 1024 * MIB);
        assert!(long.deadline > Duration::from_secs(120));
        assert!(
            long.check_start(long.reservation + long.headroom - 1)
                .is_err()
        );
        assert!(long.check_start(long.reservation + long.headroom).is_ok());
        assert!(Budget::for_input(MIB, 60.0, true).unwrap().reservation > small.reservation);
        assert!(Budget::for_input(u64::MAX, 1.0, false).is_err());
    }
    #[test]
    fn early_cancel_cannot_launch_late_work_and_never_owns_a_source() {
        let manager = AudioPreviewManager::new(ActivityCoordinator::default());
        manager.cancel("early-1").unwrap();
        let reg = manager.inner.lock().unwrap();
        assert!(reg.entries["early-1"].cancel.load(Ordering::Acquire));
        assert!(reg.directory.is_none());
        assert!(reg.assets.is_empty());
    }
    #[test]
    fn cleanup_failures_remain_accounted_and_release_checks_both_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.mp4");
        fs::write(&path, b"derived").unwrap();
        let manager = AudioPreviewManager::new(ActivityCoordinator::default());
        manager.inner.lock().unwrap().assets.insert(
            "proxy-1".into(),
            OwnedAsset {
                request_id: "request-1".into(),
                path: path.clone(),
            },
        );
        assert!(manager.release("other", "proxy-1").is_err());
        assert!(path.exists());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(manager.release("request-1", "proxy-1").is_err());
        assert_eq!(manager.inner.lock().unwrap().assets.len(), 1);
        fs::remove_dir(&path).unwrap();
        manager.release("request-1", "proxy-1").unwrap();
        assert!(!path.exists());
        manager.release("request-1", "proxy-1").unwrap();
    }
    #[test]
    fn ordinary_cancellation_does_not_latch_close() {
        let activity = ActivityCoordinator::default();
        let manager = AudioPreviewManager::new(activity.clone());
        manager.cancel_all().unwrap();
        assert!(!activity.is_closing());
        assert!(activity.begin("export").is_ok());
    }
    #[test]
    fn borrowed_original_is_not_an_owned_release_target() {
        let manager = AudioPreviewManager::new(ActivityCoordinator::default());
        let source = tempfile::NamedTempFile::new().unwrap();
        let entry = Arc::new(Entry::new(None));
        entry.finish(Ok(AudioPreviewAsset::Original {
            request_id: "original-1".into(),
            proxy_id: None,
            path: source.path().to_string_lossy().into(),
            source_identity: "a".repeat(64),
            audio_stream_index: 1,
            timeline_offset_s: 0.0,
        }));
        manager
            .inner
            .lock()
            .unwrap()
            .entries
            .insert("original-1".into(), entry.clone());
        assert!(manager.release("original-1", "invented-proxy").is_err());
        let json = serde_json::to_value(entry.wait().unwrap()).unwrap();
        assert_eq!(json["kind"], "original");
        assert_eq!(json["proxyId"], serde_json::Value::Null);
        assert_eq!(json["audioStreamIndex"], 1);
        manager.cancel("original-1").unwrap();
        assert!(source.path().exists());
        assert!(manager.inner.lock().unwrap().directory.is_none());
    }
    #[test]
    fn request_history_never_evicts_unsettled_owned_work() {
        let manager = AudioPreviewManager::new(ActivityCoordinator::default());
        let mut reg = manager.inner.lock().unwrap();
        for i in 0..HISTORY {
            let id = format!("entry-{i}");
            reg.order.push_back(id.clone());
            reg.entries.insert(id, Arc::new(Entry::new(None)));
        }
        assert!(AudioPreviewManager::reserve_history(&mut reg).is_err());
        reg.entries["entry-0"].cancel.store(true, Ordering::Release);
        reg.active = Some("entry-0".into());
        assert!(AudioPreviewManager::reserve_history(&mut reg).is_err());
        reg.active = None;
        assert!(AudioPreviewManager::reserve_history(&mut reg).is_err());
        reg.entries["entry-0"].finish(Err("cancelled".into()));
        assert!(AudioPreviewManager::reserve_history(&mut reg).is_ok());
        assert_eq!(reg.entries.len(), HISTORY - 1);
    }
    #[cfg(unix)]
    #[test]
    fn cancel_ack_waits_for_real_child_readers_and_activity_release() {
        let directory = tempfile::tempdir().unwrap();
        let activity = ActivityCoordinator::default();
        let manager = AudioPreviewManager::new(activity.clone());
        let entry = Arc::new(Entry::new(None));
        {
            let mut reg = manager.inner.lock().unwrap();
            reg.entries.insert("worker-1".into(), entry.clone());
            reg.order.push_back("worker-1".into());
            reg.active = Some("worker-1".into());
        }
        let lease = activity.begin("audio preview preparation").unwrap();
        let job = entry.clone();
        let owner = manager.clone();
        let root = directory.path().to_path_buf();
        let worker = std::thread::spawn(move || {
            let _scope = JobScope::enter(JobContext {
                cancel: job.cancel.clone(),
                child: job.group.clone(),
            });
            let mut command = std::process::Command::new("/usr/bin/python3");
            command.env_clear().env("HOME",&root).env("TMPDIR",&root).args(["-I","-c","import sys,time;sys.stderr.write('x'*150000);sys.stderr.flush();print('out_time_us=1',flush=True);time.sleep(60)"]);
            let result = run_proxy_command(
                command,
                &root.join("never-ready.mp4"),
                &root,
                &Budget::for_input(MIB, 60.0, false).unwrap(),
                &job,
            );
            drop(lease);
            owner.inner.lock().unwrap().active = None;
            job.finish(result.and_then(|_| Err("Unexpected successful fixture.".into())));
        });
        let wait = Instant::now();
        while entry.group.lock().unwrap().is_empty() && wait.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(activity.begin("export").is_err());
        manager.cancel("worker-1").unwrap();
        worker.join().unwrap();
        assert!(entry.group.lock().unwrap().is_empty());
        assert!(activity.begin("export").is_ok());
        assert!(!manager.needs_close_guard());
    }
    #[test]
    fn completed_video_cannot_hide_a_truncated_selected_audio_interval() {
        let json = serde_json::json!({"format":{"format_name":"mov,mp4","duration":"10","start_time":"0"},"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":160,"height":90,"pix_fmt":"yuv420p","start_time":"0","duration":"10"},
            {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"start_time":"0","duration":"10"}
        ]});
        let source = video::parse_probe_output(&json.to_string()).unwrap();
        let mut ready = source.clone();
        assert_eq!(validate_proxy(&source, &ready).unwrap(), 0.0);
        ready.audio_end_s = Some(1.0);
        assert!(validate_proxy(&source, &ready).is_err());
        ready = source.clone();
        ready.video_end_s = Some(1.0);
        assert!(validate_proxy(&source, &ready).is_err());
        ready = source.clone();
        ready.video_start_s += 0.5;
        ready.audio_start_s = Some(0.5);
        ready.video_end_s = Some(10.5);
        ready.audio_end_s = Some(10.5);
        ready.duration_s += 0.5;
        assert_eq!(validate_proxy(&source, &ready).unwrap(), 0.5);
    }
    #[test]
    fn destroyed_manager_cannot_veto_the_final_event_loop_exit() {
        let manager = AudioPreviewManager::new(ActivityCoordinator::default());
        {
            let mut reg = manager.inner.lock().unwrap();
            reg.assets.insert(
                "preview-1".into(),
                OwnedAsset {
                    request_id: "ready-1".into(),
                    path: PathBuf::from("inert-never-deleted-source"),
                },
            );
        }
        assert!(manager.needs_close_guard());
        manager.signal_shutdown();
        assert!(!manager.needs_close_guard());
        assert_eq!(manager.inner.lock().unwrap().assets.len(), 1);
    }
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn capacity_is_queried_on_the_actual_owned_output_volume() {
        let directory = tempfile::tempdir().unwrap();
        assert!(available_space(directory.path()).unwrap() > 0);
    }
    #[test]
    fn cached_ready_retry_revalidates_source_without_changing_idempotence() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.mp4");
        fs::write(&path, b"old source").unwrap();
        let identity = {
            let source = SourceIdentity::capture(&path).unwrap();
            source.fingerprint().to_owned()
        };
        let input = Input {
            path: path.to_string_lossy().into(),
            index: 1,
            identity: identity.clone(),
        };
        let entry = Entry::new(Some(input));
        entry.finish(Ok(AudioPreviewAsset::Original {
            request_id: "cached-1".into(),
            proxy_id: None,
            path: path.to_string_lossy().into(),
            source_identity: identity,
            audio_stream_index: 1,
            timeline_offset_s: 0.0,
        }));
        assert!(entry.wait().is_ok());
        assert!(entry.wait().is_ok());
        fs::write(&path, b"a different source length").unwrap();
        assert!(entry.wait().is_err());
    }
}
