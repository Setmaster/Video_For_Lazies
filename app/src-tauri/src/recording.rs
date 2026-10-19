//! Recording owns its native process, activity lease and recovery files through
//! publication. The webview receives status, never video frames or PCM.
use crate::{
    activity::{ActivityCoordinator, ActivityLease},
    process, video,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow as Window};

const PROTOCOL_LIMIT: u64 = 65_536;
const JOURNAL_VERSION: u32 = 1;
const RESERVE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Region {
    pub display_id: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordingConfig {
    request_id: String,
    source: Region,
    fps: u32,
    quality: String,
    countdown_seconds: u32,
    show_cursor: bool,
    system_audio_device_id: Option<String>,
    microphone_device_id: Option<String>,
    output_directory: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Recovery {
    session_id: String,
    created_at: String,
    message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    session_id: Option<String>,
    sequence: u64,
    phase: String,
    elapsed_ms: u64,
    bytes_written: u64,
    countdown_remaining: Option<u32>,
    output_path: Option<String>,
    message: Option<String>,
    recoveries: Vec<Recovery>,
    config: Option<RecordingConfig>,
    selection_request_id: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            session_id: None,
            sequence: 0,
            phase: "idle".into(),
            elapsed_ms: 0,
            bytes_written: 0,
            countdown_remaining: None,
            output_path: None,
            message: None,
            recoveries: vec![],
            config: None,
            selection_request_id: None,
        }
    }
}
fn active(phase: &str) -> bool {
    matches!(
        phase,
        "preparing" | "countdown" | "starting" | "recording" | "stopping" | "finalizing"
    )
}

#[derive(Default)]
struct Inner {
    snapshot: Snapshot,
    request_id: Option<String>,
    stop: Option<Arc<AtomicBool>>,
    selecting: Option<(String, Arc<AtomicBool>)>,
    loaded: bool,
    recovery_revision: u64,
}
impl Inner {
    fn replace_recoveries(&mut self, recoveries: Vec<Recovery>) {
        self.snapshot.recoveries = recoveries;
        self.recovery_revision = self.recovery_revision.saturating_add(1);
    }
}
#[derive(Clone, Default)]
pub(crate) struct RecordingManager {
    inner: Arc<Mutex<Inner>>,
    startup_load: Arc<Mutex<()>>,
}

impl RecordingManager {
    pub(crate) fn needs_close_guard(&self) -> bool {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        active(&i.snapshot.phase) || i.selecting.is_some()
    }
    pub(crate) fn shutdown(&self) {
        let i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stop) = &i.stop {
            stop.store(true, Ordering::Release);
        }
        if let Some((_, stop)) = &i.selecting {
            stop.store(true, Ordering::Release);
        }
    }
    fn snapshot(&self) -> Snapshot {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
    }
    fn update(&self, app: &AppHandle, id: &str, change: impl FnOnce(&mut Snapshot)) {
        self.update_inner(app, id, None, change);
    }
    fn update_recoveries(
        &self,
        app: &AppHandle,
        id: &str,
        recoveries: Vec<Recovery>,
        change: impl FnOnce(&mut Snapshot),
    ) {
        self.update_inner(app, id, Some(recoveries), change);
    }
    fn update_inner(
        &self,
        app: &AppHandle,
        id: &str,
        recoveries: Option<Vec<Recovery>>,
        change: impl FnOnce(&mut Snapshot),
    ) {
        let snapshot = {
            let mut i = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if i.snapshot.session_id.as_deref() != Some(id) {
                return;
            }
            if let Some(recoveries) = recoveries {
                i.replace_recoveries(recoveries);
            }
            change(&mut i.snapshot);
            i.snapshot.sequence = i.snapshot.sequence.saturating_add(1);
            i.snapshot.clone()
        };
        let _ = app.emit("recording-status", snapshot);
    }
    fn load(&self, app: &AppHandle) -> Result<(), String> {
        self.load_with(|| find_recoveries(app))
    }
    fn load_with(
        &self,
        mut discover: impl FnMut() -> Result<Vec<Recovery>, String>,
    ) -> Result<(), String> {
        // One startup worker owns discovery. Concurrent polls return current
        // state without caching a partial scan or accumulating blocked workers.
        let _load = match self.startup_load.try_lock() {
            Ok(load) => load,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(()),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("Recording recovery discovery is unavailable.".into());
            }
        };
        // Directory discovery and whole-file publication checks never own the
        // session mutex or the native UI thread. Stop/Close remain responsive.
        loop {
            let revision = {
                let i = self
                    .inner
                    .lock()
                    .map_err(|_| "Recording state is unavailable.")?;
                if i.loaded {
                    return Ok(());
                }
                i.recovery_revision
            };
            let recovered = discover()?;
            let mut i = self
                .inner
                .lock()
                .map_err(|_| "Recording state is unavailable.")?;
            if i.recovery_revision != revision {
                // A terminal save/recovery/discard refreshed the list while
                // this scan ran. Never overwrite it with an older discovery.
                continue;
            }
            i.replace_recoveries(recovered);
            i.loaded = true;
            i.snapshot.sequence += 1;
            return Ok(());
        }
    }
}

fn main_window(window: &Window) -> Result<(), String> {
    let url = window.url().map_err(error)?;
    if window.label() == "main" && trusted_origin(&url) {
        Ok(())
    } else {
        Err("Recording is available from the main window only.".into())
    }
}
fn trusted_origin(url: &tauri::Url) -> bool {
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    matches!(
        (url.scheme(), url.host_str(), url.port()),
        ("tauri", Some("localhost"), None) | ("http", Some("tauri.localhost"), None)
    ) || cfg!(debug_assertions)
        && url.scheme() == "http"
        && url.host_str() == Some("localhost")
        && url.port() == Some(1422)
}
fn error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn helper_path(app: &AppHandle) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err("Recording is coming to Linux after Windows.".into());
    }
    let name = "recording-sidecar/vfl_recording_host.exe";
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join(name));
    }
    if let Ok(resources) = app.path().resource_dir() {
        paths.push(resources.join(name));
    }
    paths.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "The bundled recording helper is missing. Restore the complete app folder.".into()
    })
}

/// A bounded protocol and a watchdog sit outside the native media call stack.
fn helper_call(
    app: &AppHandle,
    mut request: Value,
    stop: Arc<AtomicBool>,
    timeout: Option<Duration>,
    mut progress: impl FnMut(&Value),
) -> Result<Value, String> {
    let path = helper_path(app)?;
    regular_ancestors(&path)?;
    let command = Command::new(&path);
    let mut child = process::ManagedChild::spawn_with_stdin(command).map_err(|e| {
        format!(
            "Cannot start Windows recording components: {e}. Windows N may need Media Feature Pack."
        )
    })?;
    let mut input = child
        .stdin
        .take()
        .ok_or("Recording control pipe is unavailable.")?;
    request["protocolVersion"] = json!(1);
    serde_json::to_writer(&mut input, &request).map_err(error)?;
    input
        .write_all(b"\n")
        .and_then(|_| input.flush())
        .map_err(error)?;
    let output = child
        .stdout
        .take()
        .ok_or("Recording status pipe is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Recording error pipe is unavailable.")?;
    let errors = std::thread::spawn(move || {
        let mut reader = stderr;
        let mut tail = [0_u8; 4096];
        while let Ok(n) = reader.read(&mut tail) {
            if n == 0 {
                break;
            }
        }
    });
    let (send, receive) = mpsc::sync_channel(16);
    let reader = std::thread::spawn(move || {
        let mut reader = BufReader::new(output);
        loop {
            let mut line = Vec::new();
            let read = reader
                .by_ref()
                .take(PROTOCOL_LIMIT + 1)
                .read_until(b'\n', &mut line);
            match read {
                Ok(0) => break,
                Ok(_) if line.len() <= PROTOCOL_LIMIT as usize && line.ends_with(b"\n") => {
                    if send
                        .send(serde_json::from_slice::<Value>(&line).map_err(error))
                        .is_err()
                    {
                        break;
                    }
                }
                _ => {
                    let _ = send.send(Err("Invalid recording helper response.".into()));
                    break;
                }
            }
        }
    });
    let started = Instant::now();
    let mut last_progress = started;
    let mut stopping = None;
    let recording = request["operation"] == "record";
    let mut ready = false;
    let mut watchdog_interrupted = false;
    let result = loop {
        if stop.load(Ordering::Acquire) && stopping.is_none() {
            let _ = input.write_all(b"stop\n").and_then(|_| input.flush());
            stopping = Some(Instant::now());
        }
        if stopping.is_some_and(|at: Instant| at.elapsed() > Duration::from_secs(20)) {
            break Err("Recording did not stop in time. Its recovery files have been kept.".into());
        }
        if timeout.is_some_and(|limit| started.elapsed() > limit) {
            break Err("Windows recording components did not respond in time.".into());
        }
        if recording
            && stopping.is_none()
            && ((!ready && started.elapsed() > Duration::from_secs(15))
                || (ready && last_progress.elapsed() > Duration::from_secs(10)))
        {
            watchdog_interrupted = true;
            stop.store(true, Ordering::Release);
            // Give the host its normal stop grace; a stalled driver can then be killed.
        }
        match receive.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(message)) => {
                if message["protocolVersion"] != 1 {
                    break Err("Recording component version mismatch.".into());
                }
                match message["type"].as_str() {
                    Some("progress") => {
                        ready |= message["value"]["ready"].as_bool().unwrap_or(false);
                        last_progress = Instant::now();
                        progress(&message["value"]);
                    }
                    Some("result") => {
                        if watchdog_interrupted {
                            progress(
                                &json!({"interruption":"Recording stopped because Windows stopped responding. The saved clip may be shorter than expected."}),
                            );
                        }
                        break Ok(message["value"].clone());
                    }
                    Some("error") => {
                        if message["noMedia"] == true {
                            progress(&json!({"noMedia":true}));
                        }
                        break Err(message["message"]
                            .as_str()
                            .unwrap_or("Windows recording failed.")
                            .to_string());
                    }
                    _ => break Err("Unknown recording helper response.".into()),
                }
            }
            Ok(Err(e)) => break Err(e),
            Err(mpsc::RecvTimeoutError::Disconnected) => break Err(
                "Windows recording components exited unexpectedly. Recovery files have been kept."
                    .into(),
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    };
    drop(input);
    if result.is_err() {
        let _ = child.control.kill();
    }
    let exited = child.wait_timeout(Duration::from_secs(3)).map_err(error)?;
    if exited.is_none() {
        let _ = child.control.kill();
    }
    drop(child);
    // ManagedChild's job kills every inherited writer before these joins.
    drop(receive);
    let _ = reader.join();
    let _ = errors.join();
    if result.is_ok() && !exited.is_some_and(|status| status.success()) {
        return Err("Recording helper did not exit successfully.".into());
    }
    result
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Capabilities {
    available: bool,
    reason: Option<String>,
    default_output_directory: Option<String>,
}
#[tauri::command]
pub(crate) async fn recording_capabilities(
    window: Window,
    app: AppHandle,
) -> Result<Capabilities, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        let default = app
            .path()
            .video_dir()
            .or_else(|_| app.path().document_dir())
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        let check = helper_call(
            &app,
            json!({"operation":"sources"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        );
        Ok(Capabilities {
            available: check.is_ok(),
            reason: check.err(),
            default_output_directory: default,
        })
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_sources(window: Window, app: AppHandle) -> Result<Value, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        helper_call(
            &app,
            json!({"operation":"sources"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_audio_devices(
    window: Window,
    app: AppHandle,
) -> Result<Value, String> {
    main_window(&window)?;
    crate::command_dispatch::files(move || {
        helper_call(
            &app,
            json!({"operation":"audio-devices"}),
            Arc::new(AtomicBool::new(false)),
            Some(Duration::from_secs(10)),
            |_| {},
        )
    })
    .await
}
#[tauri::command]
pub(crate) async fn recording_select_region(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    display_id: String,
    request_id: String,
) -> Result<Value, String> {
    main_window(&window)?;
    if request_id.is_empty() || request_id.len() > 100 {
        return Err("Invalid selection request.".into());
    }
    let lease = activity.begin("screen selection")?;
    let manager = state.inner().clone();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        i.selecting = Some((request_id.clone(), stop.clone()));
        i.snapshot.selection_request_id = Some(request_id.clone());
        i.snapshot.sequence += 1;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        let result = helper_call(
            &app,
            json!({"operation":"select-region","displayId":display_id}),
            stop,
            Some(Duration::from_secs(180)),
            |_| {},
        );
        let mut i = manager.inner.lock().unwrap_or_else(|e| e.into_inner());
        if i.selecting
            .as_ref()
            .is_some_and(|(id, _)| id == &request_id)
        {
            i.selecting = None;
            i.snapshot.selection_request_id = None;
            i.snapshot.sequence += 1;
        }
        result
    })
    .await
    .map_err(error)?
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    version: u32,
    session_id: String,
    created_at: String,
    directory: PathBuf,
    destination: PathBuf,
    config: RecordingConfig,
    #[serde(default)]
    published_size: Option<u64>,
    #[serde(default)]
    published_hash: Option<String>,
    #[serde(default)]
    candidate_directory: Option<PathBuf>,
}

fn registry(app: &AppHandle) -> Result<PathBuf, String> {
    let root = app
        .path()
        .app_local_data_dir()
        .map_err(error)?
        .join("recording-recovery");
    fs::create_dir_all(&root).map_err(error)?;
    regular_ancestors(&root)?;
    Ok(root)
}
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() < 80 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}
fn regular_ancestors(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        let m = fs::symlink_metadata(ancestor).map_err(error)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if m.file_attributes() & 0x400 != 0 {
                return Err("Recording paths must not use redirected folders.".into());
            }
        }
        if m.file_type().is_symlink() {
            return Err("Recording paths must not use symbolic links.".into());
        }
    }
    Ok(())
}
fn write_journal(app: &AppHandle, journal: &Journal) -> Result<(), String> {
    let root = registry(app)?;
    let mut file = tempfile::NamedTempFile::new_in(&root).map_err(error)?;
    serde_json::to_writer(&mut file, journal).map_err(error)?;
    file.as_file().sync_all().map_err(error)?;
    file.persist(root.join(format!("{}.json", journal.session_id)))
        .map_err(error)?;
    Ok(())
}
fn read_journal(app: &AppHandle, id: &str) -> Result<Journal, String> {
    if !safe_id(id) {
        return Err("Invalid recovery session.".into());
    }
    let path = registry(app)?.join(format!("{id}.json"));
    regular_ancestors(&path)?;
    let metadata = fs::metadata(&path).map_err(error)?;
    if !metadata.is_file() || metadata.len() > PROTOCOL_LIMIT {
        return Err("Invalid recovery journal.".into());
    }
    let j: Journal =
        serde_json::from_reader(fs::File::open(path).map_err(error)?).map_err(error)?;
    if j.version != JOURNAL_VERSION
        || j.session_id != id
        || j.directory.file_name().and_then(|s| s.to_str()) != Some(&format!(".vfl-recording-{id}"))
    {
        return Err("Invalid recovery journal ownership.".into());
    }
    regular_ancestors(&j.directory)?;
    let marker = j.directory.join("owner.json");
    regular_ancestors(&marker)?;
    let metadata = fs::metadata(&marker).map_err(error)?;
    if !metadata.is_file() || metadata.len() > 256 {
        return Err("Invalid recording directory ownership.".into());
    }
    let owner: Value =
        serde_json::from_reader(fs::File::open(marker).map_err(error)?).map_err(error)?;
    if owner != json!({"version":JOURNAL_VERSION,"sessionId":id}) {
        return Err("Recording directory ownership differs.".into());
    }
    let source_root = fs::canonicalize(&j.config.output_directory).map_err(error)?;
    if j.directory.parent() != Some(source_root.as_path())
        || j.destination.file_name().and_then(|s| s.to_str())
            != Some(&format!("Recording-{id}.mp4"))
    {
        return Err("Invalid recovery destination.".into());
    }
    regular_ancestors(
        j.destination
            .parent()
            .ok_or("Invalid recovery destination.")?,
    )?;
    if let Some(stage) = &j.candidate_directory {
        if stage.file_name().and_then(|s| s.to_str())
            != Some(&format!(".vfl-recording-{id}-recovery"))
        {
            return Err("Invalid recovery staging directory.".into());
        }
        validate_marker(stage, id)?;
    }
    Ok(j)
}
fn validate_marker(directory: &Path, id: &str) -> Result<(), String> {
    regular_ancestors(directory)?;
    let marker = directory.join("owner.json");
    regular_ancestors(&marker)?;
    let meta = fs::metadata(&marker).map_err(error)?;
    if !meta.is_file() || meta.len() > 256 {
        return Err("Invalid recording ownership marker.".into());
    }
    let owner: Value =
        serde_json::from_reader(fs::File::open(marker).map_err(error)?).map_err(error)?;
    if owner != json!({"version":JOURNAL_VERSION,"sessionId":id}) {
        return Err("Recording ownership marker differs.".into());
    }
    Ok(())
}
fn find_recoveries(app: &AppHandle) -> Result<Vec<Recovery>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(registry(app)?).map_err(error)?.take(1000) {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(j) = read_journal(app, id) else {
            continue;
        };
        let Ok(_session_lock) = lock_session(&j) else {
            continue;
        };
        if publication_matches(&j) {
            let _ = cleanup(app, &j);
            continue;
        }
        found.push(Recovery {
            session_id: j.session_id,
            created_at: j.created_at,
            message: "An interrupted recording is available to recover.".into(),
        });
    }
    Ok(found)
}
fn validate_config(config: &RecordingConfig) -> Result<(), String> {
    if config.request_id.is_empty() || config.request_id.len() > 100 {
        return Err("Invalid recording request.".into());
    }
    if ![24, 25, 30, 50, 60].contains(&config.fps)
        || ![0, 3, 5].contains(&config.countdown_seconds)
        || !matches!(config.quality.as_str(), "standard" | "high")
    {
        return Err("Unsupported recording settings.".into());
    }
    let r = &config.source;
    if r.width < 16 || r.height < 16 || r.width > 7680 || r.height > 4320 || r.display_id.is_empty()
    {
        return Err("Choose an area between 16 pixels and 7680 × 4320.".into());
    }
    if config.output_directory.is_empty() {
        return Err("Choose an output folder.".into());
    }
    Ok(())
}
fn create_journal(
    app: &AppHandle,
    id: &str,
    config: RecordingConfig,
) -> Result<(Journal, fs::File), String> {
    validate_config(&config)?;
    let destination_root = fs::canonicalize(&config.output_directory)
        .map_err(|e| format!("Cannot open output folder: {e}"))?;
    regular_ancestors(&destination_root)?;
    if !destination_root.is_dir() {
        return Err("Choose an output folder.".into());
    }
    ensure_local_space(&destination_root, RESERVE_BYTES * 2)?;
    let directory = destination_root.join(format!(".vfl-recording-{id}"));
    fs::create_dir(&directory).map_err(error)?;
    let mut marker = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("owner.json"))
        .map_err(error)?;
    serde_json::to_writer(
        &mut marker,
        &json!({"version":JOURNAL_VERSION,"sessionId":id}),
    )
    .map_err(error)?;
    marker.sync_all().map_err(error)?;
    let session_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(directory.join("session.lock"))
        .map_err(error)?;
    session_lock.try_lock().map_err(error)?;
    let destination = destination_root.join(format!("Recording-{id}.mp4"));
    let j = Journal {
        version: JOURNAL_VERSION,
        session_id: id.into(),
        created_at: format!(
            "{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(error)?
                .as_secs()
        ),
        directory,
        destination,
        config,
        published_size: None,
        published_hash: None,
        candidate_directory: None,
    };
    if let Err(e) = write_journal(app, &j) {
        let _ = fs::remove_dir(&j.directory);
        return Err(e);
    }
    Ok((j, session_lock))
}
fn lock_session(j: &Journal) -> Result<fs::File, String> {
    let path = j.directory.join("session.lock");
    regular_ancestors(&path)?;
    if !fs::metadata(&path).map_err(error)?.is_file() {
        return Err("Invalid recording session lock.".into());
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(error)?;
    lock.try_lock()
        .map_err(|_| "This recording is still in use by another app window.".to_string())?;
    Ok(lock)
}

#[cfg(windows)]
fn ensure_local_space(path: &Path, minimum: u64) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
        fn GetVolumePathNameW(path: *const u16, volume: *mut u16, len: u32) -> i32;
        fn GetDriveTypeW(path: *const u16) -> u32;
        fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            name_len: u32,
            serial: *mut u32,
            max_component: *mut u32,
            flags: *mut u32,
            filesystem: *mut u16,
            filesystem_len: u32,
        ) -> i32;
    }
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut volume = [0_u16; 32768];
    let mut available = 0;
    unsafe {
        if GetVolumePathNameW(text.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) == 0
            || GetDriveTypeW(volume.as_ptr()) != 3
        {
            return Err("Choose a local fixed drive for recordings.".into());
        }
        let mut filesystem = [0_u16; 32];
        if GetVolumeInformationW(
            volume.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            32,
        ) == 0
        {
            return Err("Cannot identify the recording filesystem.".into());
        }
        let filesystem = String::from_utf16_lossy(
            &filesystem[..filesystem.iter().position(|c| *c == 0).unwrap_or(32)],
        );
        if filesystem.eq_ignore_ascii_case("FAT32") || filesystem.eq_ignore_ascii_case("FAT") {
            return Err(
                "Choose an NTFS, ReFS or exFAT folder for recordings. FAT32 has a 4 GB file limit."
                    .into(),
            );
        }
        if GetDiskFreeSpaceExW(
            text.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
        {
            return Err("Cannot check recording disk space.".into());
        }
    }
    if available < minimum {
        return Err(
            "There is not enough free space to safely save this recording. Choose another folder."
                .into(),
        );
    }
    Ok(())
}
#[cfg(not(windows))]
fn ensure_local_space(_path: &Path, _minimum: u64) -> Result<(), String> {
    Ok(())
}

#[tauri::command]
pub(crate) async fn recording_snapshot(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let manager = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        manager.load(&app)?;
        Ok(manager.snapshot())
    })
    .await
    .map_err(error)?
}

#[tauri::command]
pub(crate) fn recording_start(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    config: RecordingConfig,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    validate_config(&config)?;
    helper_path(&app)?;
    let mut i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if i.request_id.as_deref() == Some(&config.request_id) {
        return Ok(i.snapshot.clone());
    }
    if active(&i.snapshot.phase) || i.selecting.is_some() {
        return Err("A recording operation is already active.".into());
    }
    let lease = activity.begin("recording")?;
    let id = format!(
        "{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(error)?
            .as_nanos(),
        std::process::id()
    );
    let stop = Arc::new(AtomicBool::new(false));
    i.snapshot = Snapshot {
        session_id: Some(id.clone()),
        sequence: i.snapshot.sequence + 1,
        phase: "preparing".into(),
        recoveries: i.snapshot.recoveries.clone(),
        config: Some(config.clone()),
        ..Snapshot::default()
    };
    i.stop = Some(stop.clone());
    i.request_id = Some(config.request_id.clone());
    let snapshot = i.snapshot.clone();
    drop(i);
    let manager = state.inner().clone();
    let failed_manager = manager.clone();
    let failed_app = app.clone();
    let failed_id = id.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("recording-session".into())
        .spawn(move || run_session(app, manager, id, config, stop, lease))
    {
        failed_manager.update(&failed_app, &failed_id, |s| {
            s.phase = "failed".into();
            s.message = Some(format!("Cannot start recorder: {e}"));
        });
        return Err(format!("Cannot start recorder: {e}"));
    }
    Ok(snapshot)
}

fn run_session(
    app: AppHandle,
    manager: RecordingManager,
    id: String,
    config: RecordingConfig,
    stop: Arc<AtomicBool>,
    lease: ActivityLease,
) {
    let mut cancelled_before_capture = false;
    let result = (|| {
        // Re-resolve the display and audio endpoints immediately before arming.
        let sources = helper_call(
            &app,
            json!({"operation":"sources"}),
            stop.clone(),
            Some(Duration::from_secs(10)),
            |_| {},
        )?;
        let display = sources
            .as_array()
            .and_then(|list| list.iter().find(|d| d["id"] == config.source.display_id))
            .ok_or("The selected display is no longer connected. Select it again.")?;
        validate_bounds(&config.source, display)?;
        let (journal, session_lock) = create_journal(&app, &id, config.clone())?;
        let mut no_media = false;
        let result = (|| {
            let deadline = Instant::now() + Duration::from_secs(config.countdown_seconds as u64);
            while Instant::now() < deadline {
                if stop.load(Ordering::Acquire) {
                    return Err("Recording cancelled before capture started.".into());
                }
                let left = deadline.saturating_duration_since(Instant::now()).as_secs() + 1;
                manager.update(&app, &id, |s| {
                    s.phase = "countdown".into();
                    s.countdown_remaining = Some(left.min(config.countdown_seconds as u64) as u32);
                });
                std::thread::sleep(Duration::from_millis(100));
            }
            if stop.load(Ordering::Acquire) {
                return Err("Recording cancelled before capture started.".into());
            }
            manager.update(&app, &id, |s| {
                s.phase = "starting".into();
                s.countdown_remaining = None;
            });
            let spool = journal.directory.join("capture.mp4");
            let host = json!({"region":config.source,"fps":config.fps,"showCursor":config.show_cursor,"quality":config.quality,"systemAudioDeviceId":config.system_audio_device_id,"microphoneDeviceId":config.microphone_device_id,"spoolPath":spool,"ffmpegPath":bundled_media(&app,"ffmpeg")?});
            let mut last_space = Instant::now();
            let mut space_error = None;
            let outcome = helper_call(
                &app,
                json!({"operation":"record","config":host}),
                stop.clone(),
                None,
                |progress| {
                    no_media |= progress["noMedia"] == true;
                    if let Some(reason) = progress["interruption"].as_str() {
                        space_error = Some(reason.to_string());
                    }
                    manager.update(&app, &id, |s| {
                        if progress["ready"].as_bool().unwrap_or(false) && s.phase == "starting" {
                            s.phase = "recording".into();
                        }
                        s.elapsed_ms = progress["elapsedMs"].as_u64().unwrap_or(s.elapsed_ms);
                        s.bytes_written =
                            progress["bytesWritten"].as_u64().unwrap_or(s.bytes_written);
                    });
                    if last_space.elapsed() > Duration::from_secs(1) {
                        last_space = Instant::now();
                        if let Err(e) = ensure_local_space(&journal.directory, RESERVE_BYTES) {
                            space_error = Some(e);
                            stop.store(true, Ordering::Release);
                        }
                    }
                },
            )?;
            manager.update(&app, &id, |s| {
                s.phase = "finalizing".into();
                s.elapsed_ms = outcome["elapsedMs"].as_u64().unwrap_or(s.elapsed_ms);
                s.bytes_written = outcome["bytesWritten"].as_u64().unwrap_or(s.bytes_written);
                s.message = space_error;
            });
            let path = finalize(&app, &journal, None)?;
            Ok(path)
        })();
        if result.is_err() && (no_media || !journal.directory.join("capture.mp4").exists()) {
            let cleaned = cleanup(&app, &journal).is_ok();
            cancelled_before_capture =
                cleaned && stop.load(Ordering::Acquire) && manager.snapshot().phase == "stopping";
        }
        drop(session_lock);
        result
    })();
    // Release exclusive admission before exposing the terminal state.
    drop(lease);
    let recoveries = find_recoveries(&app).unwrap_or_default();
    manager.update_recoveries(&app, &id, recoveries, |s| {
        s.countdown_remaining = None;
        match result {
            Ok(path) => {
                s.phase = "completed".into();
                s.bytes_written = fs::metadata(&path)
                    .map(|m| m.len())
                    .unwrap_or(s.bytes_written);
                s.output_path = Some(path.to_string_lossy().into_owned());
            }
            Err(e) => {
                s.phase = if s.recoveries.iter().any(|r| r.session_id == id) {
                    "recoverable"
                } else if cancelled_before_capture {
                    "idle"
                } else {
                    "failed"
                }
                .into();
                s.message = Some(e);
            }
        }
    });
    let mut i = manager.inner.lock().unwrap_or_else(|e| e.into_inner());
    if i.snapshot.session_id.as_deref() == Some(&id) {
        i.stop = None;
    }
}
fn validate_bounds(region: &Region, display: &Value) -> Result<(), String> {
    let x = display["x"]
        .as_i64()
        .ok_or("Invalid display coordinates.")?;
    let y = display["y"]
        .as_i64()
        .ok_or("Invalid display coordinates.")?;
    let w = display["width"]
        .as_i64()
        .ok_or("Invalid display dimensions.")?;
    let h = display["height"]
        .as_i64()
        .ok_or("Invalid display dimensions.")?;
    if i64::from(region.x) < x
        || i64::from(region.y) < y
        || i64::from(region.x) + i64::from(region.width) > x + w
        || i64::from(region.y) + i64::from(region.height) > y + h
    {
        return Err("The area no longer fits this display. Select it again.".into());
    }
    Ok(())
}
#[tauri::command]
pub(crate) fn recording_stop(
    window: Window,
    state: State<'_, RecordingManager>,
    session_id: Option<String>,
    selection_request_id: Option<String>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let mut i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if let Some((id, selecting)) = &i.selecting
        && selection_request_id.as_deref() == Some(id)
    {
        selecting.store(true, Ordering::Release);
    }
    if i.snapshot.session_id == session_id && active(&i.snapshot.phase) {
        if let Some(stop) = &i.stop {
            stop.store(true, Ordering::Release);
        }
        if i.snapshot.phase != "finalizing" {
            i.snapshot.phase = "stopping".into();
            i.snapshot.sequence += 1;
        }
    }
    Ok(i.snapshot.clone())
}

fn bundled_media(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let file = format!("ffmpeg-sidecar/{name}{}", std::env::consts::EXE_SUFFIX);
    let beside = std::env::current_exe()
        .map_err(error)?
        .parent()
        .ok_or("Missing app folder.")?
        .join(&file);
    let resource = app.path().resource_dir().map_err(error)?.join(file);
    [beside, resource]
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| format!("The bundled {name} is missing. Restore the complete app folder."))
}
fn run_media(mut command: Command, timeout: Duration) -> Result<(), String> {
    // File-only remux/validation: output is discarded and stderr is bounded by
    // draining into a fixed buffer. No shell or arbitrary user flags.
    command.arg("-nostdin");
    let mut child = process::ManagedChild::spawn(command).map_err(error)?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let a = std::thread::spawn(move || {
        if let Some(mut r) = stdout {
            let _ = std::io::copy(&mut r, &mut std::io::sink());
        }
    });
    let b = std::thread::spawn(move || {
        if let Some(mut r) = stderr {
            let _ = std::io::copy(&mut r, &mut std::io::sink());
        }
    });
    let status = child.wait_timeout(timeout).map_err(error)?;
    if status.is_none() {
        let _ = child.control.kill();
    }
    drop(child);
    let _ = a.join();
    let _ = b.join();
    if status.is_some_and(|s| s.success()) {
        Ok(())
    } else {
        Err("Could not finish the MP4. Recovery files have been kept.".into())
    }
}
fn file_digest(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = fs::File::open(path).map_err(error)?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf).map_err(error)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn publication_matches(j: &Journal) -> bool {
    regular_ancestors(&j.destination).is_ok()
        && j.published_size.is_some_and(|size| {
            fs::metadata(&j.destination).is_ok_and(|m| m.is_file() && m.len() == size)
        })
        && j.published_hash
            .as_ref()
            .is_some_and(|h| file_digest(&j.destination).is_ok_and(|actual| &actual == h))
}
fn finalize(
    app: &AppHandle,
    journal: &Journal,
    output_directory: Option<String>,
) -> Result<PathBuf, String> {
    let mut j = journal.clone();
    if publication_matches(&j) {
        let _ = cleanup(app, &j);
        return Ok(j.destination);
    }
    let destination = if let Some(output) = output_directory {
        let dir = fs::canonicalize(output).map_err(error)?;
        regular_ancestors(&dir)?;
        dir.join(format!("Recording-{}.mp4", j.session_id))
    } else {
        j.destination.clone()
    };
    let candidate_root = if destination.parent() != j.directory.parent() {
        let stage = destination
            .parent()
            .ok_or("Invalid output folder.")?
            .join(format!(".vfl-recording-{}-recovery", j.session_id));
        if stage.exists() {
            validate_marker(&stage, &j.session_id)?;
        } else {
            fs::create_dir(&stage).map_err(error)?;
            let mut owner = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage.join("owner.json"))
                .map_err(error)?;
            serde_json::to_writer(
                &mut owner,
                &json!({"version":JOURNAL_VERSION,"sessionId":j.session_id}),
            )
            .map_err(error)?;
            owner.sync_all().map_err(error)?;
        }
        // Persist the owned alternative staging path before any media writes.
        j.candidate_directory = Some(stage.clone());
        write_journal(app, &j)?;
        stage
    } else {
        j.directory.clone()
    };
    let candidate = candidate_root.join("candidate.mp4");
    let spool = j.directory.join("capture.mp4");
    regular_ancestors(&spool)?;
    let bytes = fs::metadata(&spool).map_err(error)?.len();
    if bytes == 0 {
        return Err("The recording contains no recoverable video.".into());
    }
    if candidate.exists() && validate_candidate(&candidate, &j.config).is_err() {
        regular_ancestors(&candidate)?;
        fs::remove_file(&candidate).map_err(error)?;
    }
    if !candidate.exists() {
        let mut pcm_bytes = 0;
        let mut command = Command::new(bundled_media(app, "ffmpeg")?);
        command
            .args(["-hide_banner", "-v", "error", "-n", "-i"])
            .arg(&spool);
        let pcm = j.directory.join("capture.pcm");
        if pcm.exists() {
            regular_ancestors(&pcm)?;
            let metadata = fs::metadata(&pcm).map_err(error)?;
            if !metadata.is_file()
                || metadata.len() < 4
                || !(j.config.system_audio_device_id.is_some()
                    || j.config.microphone_device_id.is_some())
            {
                return Err(
                    "The recording's audio recovery file is invalid. Its files have been kept."
                        .into(),
                );
            }
            pcm_bytes = metadata.len();
            // The native host writes sample zero at the exact video QPC epoch.
            // This is timestamp-normalized 48 kHz stereo PCM, not a separately
            // started audio capture clock. Encode it once during publication.
            command
                .args(["-f", "s16le", "-ar", "48000", "-ac", "2", "-t"])
                .arg(format!("{:.9}", (pcm_bytes / 4) as f64 / 48_000.0))
                .arg("-i")
                .arg(pcm)
                .args([
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "192k",
                    "-af",
                    "apad",
                    "-shortest",
                ]);
        } else {
            command.args(["-map", "0:v:0", "-map", "0:a:0?", "-c", "copy"]);
        }
        command
            .args(["-map_metadata", "-1", "-movflags", "+faststart"])
            .arg(&candidate);
        let (required_bytes, timeout) = remux_budget(bytes, pcm_bytes);
        ensure_local_space(&candidate_root, required_bytes)?;
        if let Err(e) = run_media(command, timeout) {
            let _ = fs::remove_file(&candidate);
            return Err(e);
        }
    }
    validate_candidate(&candidate, &j.config)?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&candidate)
        .map_err(error)?
        .sync_all()
        .map_err(error)?;
    j.published_size = Some(fs::metadata(&candidate).map_err(error)?.len());
    j.published_hash = Some(file_digest(&candidate)?);
    // Keep the destination local to the owned session until publication. A
    // recovery-to-other-volume intent is settled by this exact digest too.
    let original_destination = j.destination.clone();
    j.destination = destination.clone();
    write_journal(app, &j)?;
    match publish_candidate(&candidate, &destination) {
        Ok(_) => {
            // Publication is irreversible success. A cleanup error must never
            // hide the saved MP4 or offer it as unsaved footage.
            let _ = cleanup(app, &j);
            Ok(destination)
        }
        Err(e) => {
            let detail = e;
            j.destination = original_destination;
            j.published_size = None;
            j.published_hash = None;
            let _ = write_journal(app, &j);
            Err(format!(
                "Cannot save without replacing an existing file: {detail}. Choose another folder; the recording has been kept."
            ))
        }
    }
}

fn remux_budget(video_bytes: u64, pcm_bytes: u64) -> (u64, Duration) {
    // AAC192k plus conservative mux/VBR overhead at32kB/s; native PCM is192kB/s.
    // Incomplete terminal sample bytes cannot add a complete audio frame.
    let seconds = (pcm_bytes / 4).div_ceil(48_000);
    let reserve = video_bytes
        .saturating_add(seconds.saturating_mul(32_000))
        .saturating_add(RESERVE_BYTES);
    let timeout = 60_u64
        .saturating_add(video_bytes / 1_000_000)
        .saturating_add(seconds.div_ceil(4));
    (reserve, Duration::from_secs(timeout))
}

fn validate_candidate(candidate: &Path, config: &RecordingConfig) -> Result<(), String> {
    regular_ancestors(candidate)?;
    let probe = video::probe_video(candidate.to_string_lossy().into_owned())?;
    if probe.video_codec.as_deref() != Some("h264")
        || probe.width != (config.source.width + 1) & !1
        || probe.height != (config.source.height + 1) & !1
        || !probe.duration_s.is_finite()
        || probe.duration_s <= 0.
        || !probe
            .frame_rate
            .is_some_and(|fps| (fps - f64::from(config.fps)).abs() < 0.05)
        || probe.has_audio
            != (config.system_audio_device_id.is_some() || config.microphone_device_id.is_some())
        || probe.has_audio && probe.audio_codec.as_deref() != Some("aac")
    {
        return Err(
            "The recorded MP4 did not pass validation. Recovery files have been kept.".into(),
        );
    }
    Ok(())
}

fn publish_candidate(candidate: &Path, destination: &Path) -> Result<(), String> {
    // The normal path stays on the session's volume and needs no third copy.
    if candidate.parent().and_then(Path::parent) == destination.parent() {
        let path = tempfile::TempPath::from_path(candidate);
        if let Err(mut e) = path.persist_noclobber(destination) {
            // Retain the ownership-bearing error before converting it to text.
            e.path.disable_cleanup(true);
            return Err(e.error.to_string());
        }
        Ok(())
    } else {
        let mut copy =
            tempfile::NamedTempFile::new_in(destination.parent().ok_or("Invalid output folder.")?)
                .map_err(error)?;
        std::io::copy(&mut fs::File::open(candidate).map_err(error)?, &mut copy).map_err(error)?;
        copy.as_file().sync_all().map_err(error)?;
        copy.persist_noclobber(destination)
            .map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[tauri::command]
pub(crate) fn recording_close(
    window: Window,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    session_id: Option<String>,
) -> Result<(), String> {
    main_window(&window)?;
    let i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    if i.snapshot.session_id != session_id || active(&i.snapshot.phase) || i.selecting.is_some() {
        return Err("Recording state changed. Keep the app open and try again.".into());
    }
    let close = activity.begin_close()?;
    drop(i);
    window.destroy().map_err(error)?;
    close.commit();
    Ok(())
}
fn cleanup(app: &AppHandle, j: &Journal) -> Result<(), String> {
    if let Some(stage) = &j.candidate_directory
        && stage.exists()
    {
        validate_marker(stage, &j.session_id)?;
        for name in ["candidate.mp4", "owner.json"] {
            let path = stage.join(name);
            if path.exists() {
                regular_ancestors(&path)?;
                fs::remove_file(path).map_err(error)?;
            }
        }
        fs::remove_dir(stage).map_err(error)?;
    }
    regular_ancestors(&j.directory)?;
    for name in [
        "capture.mp4",
        "capture.pcm",
        "candidate.mp4",
        "owner.json",
        "session.lock",
    ] {
        let file = j.directory.join(name);
        if file.exists() {
            regular_ancestors(&file)?;
            fs::remove_file(file).map_err(error)?;
        }
    }
    // Never recursively delete or remove unrelated files.
    fs::remove_dir(&j.directory).map_err(error)?;
    fs::remove_file(registry(app)?.join(format!("{}.json", j.session_id))).map_err(error)?;
    Ok(())
}
#[tauri::command]
pub(crate) fn recording_recover(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    session_id: String,
    output_directory: Option<String>,
) -> Result<Snapshot, String> {
    main_window(&window)?;
    let j = read_journal(&app, &session_id)?;
    let session_lock = lock_session(&j)?;
    let lease = activity.begin("recording recovery")?;
    let manager = state.inner().clone();
    {
        let mut i = manager
            .inner
            .lock()
            .map_err(|_| "Recording is unavailable.")?;
        if active(&i.snapshot.phase) {
            return Err("Wait for the active recording.".into());
        }
        i.snapshot.session_id = Some(session_id.clone());
        i.snapshot.config = Some(j.config.clone());
        i.snapshot.phase = "finalizing".into();
        i.snapshot.message = None;
        i.snapshot.output_path = None;
        i.snapshot.sequence += 1;
    }
    let snapshot = manager.snapshot();
    std::thread::spawn(move || {
        let result = finalize(&app, &j, output_directory);
        drop(session_lock);
        drop(lease);
        let recoveries = find_recoveries(&app).unwrap_or_default();
        manager.update_recoveries(&app, &session_id, recoveries, |s| match result {
            Ok(path) => {
                s.phase = "completed".into();
                s.output_path = Some(path.to_string_lossy().into_owned());
            }
            Err(e) => {
                s.phase = "recoverable".into();
                s.message = Some(e);
            }
        });
    });
    Ok(snapshot)
}
#[tauri::command]
pub(crate) fn recording_discard_recovery(
    window: Window,
    app: AppHandle,
    state: State<'_, RecordingManager>,
    activity: State<'_, ActivityCoordinator>,
    session_id: String,
) -> Result<(), String> {
    main_window(&window)?;
    let _lease = activity.begin("discarding recording recovery")?;
    let j = read_journal(&app, &session_id)?;
    let _session_lock = lock_session(&j)?;
    cleanup(&app, &j)?;
    let recoveries = find_recoveries(&app)?;
    let mut i = state
        .inner
        .lock()
        .map_err(|_| "Recording is unavailable.")?;
    i.replace_recoveries(recoveries);
    i.snapshot.sequence += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> RecordingConfig {
        RecordingConfig {
            request_id: "test".into(),
            source: Region {
                display_id: "display1".into(),
                x: -1920,
                y: 0,
                width: 640,
                height: 480,
            },
            fps: 30,
            quality: "standard".into(),
            countdown_seconds: 3,
            show_cursor: true,
            system_audio_device_id: None,
            microphone_device_id: None,
            output_directory: "out".into(),
        }
    }
    #[test]
    fn validates_settings_before_native_work() {
        let mut c = config();
        assert!(validate_config(&c).is_ok());
        for fps in [0, 1, 120, u32::MAX] {
            c.fps = fps;
            assert!(validate_config(&c).is_err());
        }
        c = config();
        c.source.width = 641;
        assert!(validate_config(&c).is_ok());
        c.source.width = 7681;
        assert!(validate_config(&c).is_err());
        c = config();
        c.countdown_seconds = 100;
        assert!(validate_config(&c).is_err());
    }
    #[test]
    fn physical_regions_handle_negative_origins_and_overflow() {
        let d = json!({"x":-1920,"y":0,"width":1920,"height":1080});
        let mut r = config().source;
        assert!(validate_bounds(&r, &d).is_ok());
        r.x = -1921;
        assert!(validate_bounds(&r, &d).is_err());
        r.x = i32::MAX;
        r.width = u32::MAX;
        assert!(validate_bounds(&r, &d).is_err());
    }
    #[test]
    fn recovery_ids_cannot_escape_registry() {
        for id in ["", "../abc", "C:\\abc", "a/b", "..", "x"] {
            assert!(!safe_id(id));
        }
        assert!(safe_id("abc123-567"));
    }
    #[test]
    fn identity_bound_updates_cannot_replace_new_session() {
        let manager = RecordingManager::default();
        let mut i = manager.inner.lock().unwrap();
        i.snapshot.session_id = Some("new".into());
        i.snapshot.phase = "recording".into();
        drop(i);
        assert!(manager.needs_close_guard());
        manager.shutdown();
        assert_eq!(manager.snapshot().session_id.as_deref(), Some("new"));
    }
    #[test]
    fn concurrent_startup_loads_keep_one_discovery_in_flight() {
        use std::sync::atomic::AtomicUsize;
        let manager = RecordingManager::default();
        let scans = Arc::new(AtomicUsize::new(0));
        let (entered, started) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let first = manager.clone();
        let first_scans = scans.clone();
        let first = std::thread::spawn(move || {
            first.load_with(|| {
                first_scans.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                resume.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(vec![Recovery {
                    session_id: "kept".into(),
                    created_at: "0".into(),
                    message: "recover".into(),
                }])
            })
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (attempt, attempted) = mpsc::channel();
        let (finished, finish) = mpsc::channel();
        let second = manager.clone();
        let second_scans = scans.clone();
        let second = std::thread::spawn(move || {
            attempt.send(()).unwrap();
            let result = second.load_with(|| {
                second_scans.fetch_add(1, Ordering::SeqCst);
                Ok(vec![])
            });
            finished.send(()).unwrap();
            result
        });
        attempted.recv_timeout(Duration::from_secs(5)).unwrap();
        let early_finish = finish.recv_timeout(Duration::from_secs(1));
        let state_responsive = manager.inner.try_lock().is_ok();
        let still_loading = !manager.inner.lock().unwrap().loaded;
        release.send(()).unwrap();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert!(early_finish.is_ok());
        assert!(state_responsive);
        assert!(still_loading);
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        let snapshot = manager.snapshot();
        assert_eq!(snapshot.recoveries.len(), 1);
        assert_eq!(snapshot.recoveries[0].session_id, "kept");
        assert_eq!(snapshot.sequence, 1);
    }
    #[test]
    fn startup_discovery_retries_after_a_concurrent_recovery_list_change() {
        for discard in [false, true] {
            let recovery = Recovery {
                session_id: "kept".into(),
                created_at: "0".into(),
                message: "recover".into(),
            };
            let manager = RecordingManager::default();
            let stale = if discard {
                vec![recovery.clone()]
            } else {
                vec![]
            };
            let fresh = if discard { vec![] } else { vec![recovery] };
            let stop = Arc::new(AtomicBool::new(false));
            {
                let mut i = manager.inner.lock().unwrap();
                i.snapshot.recoveries = stale.clone();
                i.snapshot.phase = "recording".into();
                i.stop = Some(stop.clone());
            }
            let (entered, started) = mpsc::channel();
            let (release, resume) = mpsc::channel();
            let loading = manager.clone();
            let expected = fresh.clone();
            let worker = std::thread::spawn(move || {
                let mut scans = 0;
                loading
                    .load_with(|| {
                        scans += 1;
                        if scans == 1 {
                            entered.send(()).unwrap();
                            resume.recv_timeout(Duration::from_secs(5)).unwrap();
                            Ok(stale.clone())
                        } else {
                            Ok(expected.clone())
                        }
                    })
                    .unwrap();
                scans
            });
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            manager
                .inner
                .lock()
                .unwrap()
                .replace_recoveries(fresh.clone());
            // This must remain independent of slow discovery/hashing.
            manager.shutdown();
            assert!(stop.load(Ordering::Acquire));
            assert!(manager.needs_close_guard());
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), 2);
            let snapshot = manager.snapshot();
            assert_eq!(snapshot.recoveries.len(), fresh.len());
            if !discard {
                assert_eq!(snapshot.recoveries[0].session_id, "kept");
            }
        }
    }
    #[test]
    fn publication_requires_size_and_exact_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.mp4");
        fs::write(&path, b"ours").unwrap();
        let mut j = Journal {
            version: 1,
            session_id: "abc".into(),
            created_at: "0".into(),
            directory: dir.path().to_path_buf(),
            destination: path.clone(),
            config: config(),
            published_size: Some(4),
            published_hash: Some(file_digest(&path).unwrap()),
            candidate_directory: None,
        };
        assert!(publication_matches(&j));
        fs::write(&path, b"else").unwrap();
        assert!(!publication_matches(&j));
        j.published_hash = None;
        assert!(!publication_matches(&j));
    }
    #[test]
    fn no_clobber_retains_candidate_and_existing_output_on_collision() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join(".vfl-recording-abc");
        fs::create_dir(&stage).unwrap();
        let candidate = stage.join("candidate.mp4");
        let output = root.path().join("Recording-abc.mp4");
        fs::write(&candidate, b"our complete recording").unwrap();
        fs::write(&output, b"existing user file").unwrap();
        assert!(publish_candidate(&candidate, &output).is_err());
        assert_eq!(fs::read(&candidate).unwrap(), b"our complete recording");
        assert_eq!(fs::read(&output).unwrap(), b"existing user file");
        let alternative = root.path().join("Recording-def.mp4");
        publish_candidate(&candidate, &alternative).unwrap();
        assert_eq!(fs::read(alternative).unwrap(), b"our complete recording");
        assert!(!candidate.exists());
    }
    #[test]
    fn session_file_lock_prevents_second_instance_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.lock");
        let first = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        first.try_lock().unwrap();
        let second = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert!(second.try_lock().is_err());
        drop(first);
        assert!(second.try_lock().is_ok());
    }
    #[test]
    fn ownership_marker_must_match_session_before_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.json");
        fs::write(
            &path,
            serde_json::to_vec(&json!({"version":1,"sessionId":"abc"})).unwrap(),
        )
        .unwrap();
        assert!(validate_marker(dir.path(), "abc").is_ok());
        assert!(validate_marker(dir.path(), "def").is_err());
        fs::write(path, b"invalid").unwrap();
        assert!(validate_marker(dir.path(), "abc").is_err());
    }
    #[test]
    fn only_bundled_and_exact_dev_origins_can_control_capture() {
        for origin in ["tauri://localhost/", "http://tauri.localhost/"] {
            assert!(trusted_origin(&origin.parse().unwrap()));
        }
        for origin in [
            "https://evil.test/",
            "http://localhost:14220/",
            "http://evil@tauri.localhost/",
            "http://localhost:1422.evil.test/",
        ] {
            if let Ok(url) = origin.parse() {
                assert!(!trusted_origin(&url));
            }
        }
    }
    #[test]
    fn pcm_finalization_budget_depends_on_duration_not_compressed_video_size() {
        let hour_pcm = 48_000 * 4 * 3600;
        let (space, time) = remux_budget(1000, hour_pcm);
        assert_eq!(space, 1000 + 3600 * 32_000 + RESERVE_BYTES);
        assert_eq!(time, Duration::from_secs(960));
        assert_eq!(
            remux_budget(1000, 0),
            (1000 + RESERVE_BYTES, Duration::from_secs(60))
        );
        assert_eq!(
            remux_budget(1000, hour_pcm + 3),
            remux_budget(1000, hour_pcm)
        );
    }
}
