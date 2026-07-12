use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::ipc::{Channel, JavaScriptChannelId};
use tauri::{AppHandle, Manager, State, Webview};

const APP_ID: &str = "com.setmaster.video-for-lazies";
const UPDATE_CHANNEL: &str = "stable";
const UPDATE_PROTOCOL_VERSION: u64 = 1;
const UPDATE_MANIFEST_SCHEMA: &str = "com.setmaster.video-for-lazies.update-manifest.v1";
const PAYLOAD_MANIFEST_SCHEMA: &str = "com.setmaster.video-for-lazies.payload-manifest.v1";
const APPLY_PLAN_SCHEMA: &str = "com.setmaster.video-for-lazies.apply-plan.v1";
const UPDATE_JOURNAL_SCHEMA: &str = "com.setmaster.video-for-lazies.update-journal.v1";
const UPDATE_PREFS_SCHEMA: &str = "com.setmaster.video-for-lazies.update-prefs.v1";
const UPDATE_STATE_DIR: &str = ".vfl-updates";
const UPDATE_JOURNAL_FILE_NAME: &str = "update-state.json";
const UPDATE_PENDING_FILE_NAME: &str = "pending-success.json";
const UPDATE_STAGING_LOCK_FILE_NAME: &str = "staging.lock";
const UPDATE_RECOVERY_LOCK_FILE_NAME: &str = "recovery.lock";
const UPDATE_APPLY_LOCK_FILE_NAME: &str = "apply.lock";
// Filename prefix for the helper copy we actually launch from the temp dir.
// The helper now embeds an explicit asInvoker manifest, and this keyword-free
// name remains a second defense against Windows UAC installer detection if a
// future packaging error strips that resource. The shipped binary keeps its
// `vfl-update-helper` name because cross-version staging looks it up by that
// name; only this launched copy is renamed.
const TEMP_HELPER_PREFIX: &str = "vfl-apply-";
// Legacy launched-copy prefix from <= v1.8.0 (contained "update"); still swept
// by cleanup so stale copies from older versions do not accumulate.
const LEGACY_TEMP_HELPER_PREFIX: &str = "vfl-update-helper-";
const PAYLOAD_MANIFEST_FILE_NAME: &str = "VFL_PAYLOAD_MANIFEST.json";
const PORTABLE_ROOT_DIR_NAME: &str = "Video_For_Lazies";
const UPDATE_MANIFEST_URL: &str = "https://github.com/Setmaster/Video_For_Lazies/releases/latest/download/vfl-update-manifest-v1.json";
const CHECK_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
const SKIP_INTERVAL_MS: u64 = 7 * 24 * 60 * 60 * 1000;
const MAX_UPDATE_BYTES: u64 = 750 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 20_000;
const COPY_RETRIES: usize = 80;
const COPY_RETRY_DELAY_MS: u64 = 250;
const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const PARENT_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEFAULT_SIGNATURE_SUFFIX: &str = ".sig";
const UPDATE_PUBLIC_KEYS: &[&str] = &[
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQ3ODU2M0VEQUIwRkNEQTYKUldTbXpRK3I3V09GMTdIUnBHaDlkMzBEN0FRYnJyTUVEa2FRN0Q0Ylh4RDMxT09hYy9vR3hEd2IK",
];

/// CLI flag passed to the elevated relaunch so the new instance knows it
/// should immediately resume the update instead of waiting for the user.
pub const ELEVATED_UPDATE_ARG: &str = "--apply-update-elevated";
pub const ELEVATED_RECOVERY_ARG: &str = "--recover-update-elevated";

static ELEVATED_UPDATE_RUN: AtomicBool = AtomicBool::new(false);
static ELEVATED_RECOVERY_RUN: AtomicBool = AtomicBool::new(false);
static ATOMIC_WRITE_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Default)]
pub struct UpdaterCoordinator {
    active_operation: Arc<Mutex<Option<String>>>,
}

impl UpdaterCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    fn begin(&self, operation: &str) -> Result<UpdaterOperationGuard, UpdatePublicError> {
        let operation_id = format!("{}-{}-{}", operation, std::process::id(), now_ms());
        let mut active = self.active_operation.lock().map_err(|_| {
            UpdatePublicError::new(
                "update-coordinator-unavailable",
                UpdatePhase::Failed,
                true,
                "restartApp",
                "The updater is temporarily unavailable. Restart the app and try again.",
            )
        })?;
        if active.is_some() {
            return Err(UpdatePublicError::new(
                "update-busy",
                UpdatePhase::Failed,
                true,
                "waitAndRetry",
                "Another update operation is already running. Wait a moment and try again.",
            ));
        }
        *active = Some(operation_id.clone());
        Ok(UpdaterOperationGuard {
            coordinator: self.clone(),
            operation_id,
        })
    }
}

struct UpdaterOperationGuard {
    coordinator: UpdaterCoordinator,
    operation_id: String,
}

impl Drop for UpdaterOperationGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.coordinator.active_operation.lock()
            && active.as_deref() == Some(self.operation_id.as_str())
        {
            *active = None;
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum UpdatePhase {
    Checking,
    Downloading,
    VerifyingArchive,
    Extracting,
    VerifyingPayload,
    Staging,
    LaunchingHelper,
    WaitingForExit,
    BackingUp,
    Replacing,
    RollingBack,
    Restarting,
    Recovering,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProgressEvent {
    pub operation_id: String,
    pub phase: UpdatePhase,
    pub completed_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePublicError {
    pub code: String,
    pub phase: UpdatePhase,
    pub retryable: bool,
    pub action: String,
    pub message: String,
}

impl UpdatePublicError {
    fn new(code: &str, phase: UpdatePhase, retryable: bool, action: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            phase,
            retryable,
            action: action.to_string(),
            message: message.to_string(),
        }
    }

    fn check_failed(_internal: String) -> Self {
        Self::new(
            "update-check-failed",
            UpdatePhase::Checking,
            true,
            "checkConnectionAndRetry",
            "Video For Lazies could not check for updates. Check your connection and try again.",
        )
    }

    fn prepare_failed(_internal: String) -> Self {
        Self::new(
            "update-prepare-failed",
            UpdatePhase::Failed,
            true,
            "retryOrDownloadPortable",
            "The update could not be prepared safely. Your current app was not changed. Try again, or download the portable release manually.",
        )
    }

    fn preferences_failed(_internal: String) -> Self {
        Self::new(
            "update-preferences-failed",
            UpdatePhase::Failed,
            true,
            "retry",
            "The update preference could not be saved. Try again.",
        )
    }

    fn finalize_failed(_internal: String) -> Self {
        Self::new(
            "update-finalize-failed",
            UpdatePhase::Recovering,
            true,
            "restartApp",
            "The updater could not verify its saved state. Restart the app. If this continues, use the existing portable folder or download a fresh copy.",
        )
    }
}

impl std::fmt::Display for UpdatePublicError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

struct ProgressReporter {
    operation_id: String,
    channel: Option<Channel<UpdateProgressEvent>>,
}

impl ProgressReporter {
    fn new(operation_id: String, channel: Option<Channel<UpdateProgressEvent>>) -> Self {
        Self {
            operation_id,
            channel,
        }
    }

    fn emit(&self, phase: UpdatePhase, message: &str) {
        self.emit_bytes(phase, None, None, message);
    }

    fn emit_bytes(
        &self,
        phase: UpdatePhase,
        completed_bytes: Option<u64>,
        total_bytes: Option<u64>,
        message: &str,
    ) {
        if let Some(channel) = &self.channel {
            let _ = channel.send(UpdateProgressEvent {
                operation_id: self.operation_id.clone(),
                phase,
                completed_bytes,
                total_bytes,
                message: message.to_string(),
            });
        }
    }
}

pub fn set_elevated_update_run(value: bool) {
    ELEVATED_UPDATE_RUN.store(value, AtomicOrdering::Relaxed);
}

pub fn set_elevated_recovery_run(value: bool) {
    ELEVATED_RECOVERY_RUN.store(value, AtomicOrdering::Relaxed);
}

fn elevated_update_run() -> bool {
    ELEVATED_UPDATE_RUN.load(AtomicOrdering::Relaxed)
}

#[cfg_attr(not(windows), allow(dead_code))]
fn elevated_recovery_run() -> bool {
    ELEVATED_RECOVERY_RUN.load(AtomicOrdering::Relaxed)
}

#[tauri::command]
pub fn elevated_update_pending() -> bool {
    elevated_update_run()
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct UpdatePrefs {
    schema: Option<String>,
    last_checked_at_ms: Option<u64>,
    suppress_prompts_until_ms: Option<u64>,
    highest_trusted_version: Option<String>,
    remind_later_version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateManifest {
    schema: String,
    app_id: String,
    channel: String,
    version: String,
    release_tag: String,
    release_url: String,
    published_at: String,
    min_updater_protocol: u64,
    notes: UpdateNotes,
    artifacts: HashMap<String, UpdateArtifact>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateNotes {
    pub title: String,
    pub summary: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateArtifact {
    file_name: String,
    url: String,
    sha256: String,
    size_bytes: u64,
    root_dir: String,
    payload_manifest: UpdatePayloadReference,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdatePayloadReference {
    path: String,
    sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PayloadManifest {
    schema: String,
    app_id: String,
    version: String,
    target: String,
    root_dir: String,
    files: Vec<PayloadFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PayloadFile {
    path: String,
    sha256: String,
    size_bytes: u64,
    mode: Option<u32>,
    kind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateApplyPlan {
    schema: String,
    update_id: String,
    from_version: String,
    to_version: String,
    target: String,
    install_dir: PathBuf,
    stage_dir: PathBuf,
    backup_dir: PathBuf,
    parent_pid: u32,
    executable_name: String,
    helper_name: String,
    // The payload-manifest digest is covered by the signed release manifest.
    // Legacy v1 plans deserialize, but a new helper refuses to apply one that
    // does not carry this trust anchor.
    #[serde(default)]
    expected_payload_manifest_sha256: Option<String>,
    // True when the staging instance ran elevated: the helper inherits that
    // token, so it must relaunch the updated app through the shell to drop
    // back to the user's normal privileges. Old plans omit the field.
    #[serde(default)]
    relaunch_via_shell: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum UpdateJournalPhase {
    Staged,
    WaitingForParent,
    BackingUp,
    BackupComplete,
    Replacing,
    AwaitingStartup,
    RollingBack,
    RolledBack,
    RecoveryRequired,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateJournal {
    schema: String,
    update_id: String,
    phase: UpdateJournalPhase,
    plan: UpdateApplyPlan,
    #[serde(default)]
    backup_payload_manifest_sha256: Option<String>,
    updated_at_ms: u64,
    error_code: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct UpdateLockRecord {
    schema: String,
    phase: String,
    update_id: Option<String>,
    pid: u32,
    started_at_ms: u64,
}

impl UpdateLockRecord {
    fn staging() -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "staging".to_string(),
            update_id: None,
            pid: std::process::id(),
            started_at_ms: now_ms(),
        }
    }

    fn helper_handoff(plan: &UpdateApplyPlan) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "helperHandoff".to_string(),
            update_id: Some(plan.update_id.clone()),
            pid: std::process::id(),
            started_at_ms: now_ms(),
        }
    }

    fn recovery_claim(plan: &UpdateApplyPlan) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "recoveryClaim".to_string(),
            update_id: Some(plan.update_id.clone()),
            pid: std::process::id(),
            started_at_ms: now_ms(),
        }
    }

    fn recovery_handoff(plan: &UpdateApplyPlan) -> Self {
        Self::recovery_handoff_for(plan, std::process::id())
    }

    fn recovery_handoff_for(plan: &UpdateApplyPlan, pid: u32) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "recoveryHandoff".to_string(),
            update_id: Some(plan.update_id.clone()),
            pid,
            started_at_ms: now_ms(),
        }
    }

    fn applying(plan: &UpdateApplyPlan) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "applying".to_string(),
            update_id: Some(plan.update_id.clone()),
            pid: std::process::id(),
            started_at_ms: now_ms(),
        }
    }

    fn orphan_cleanup_claim() -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            phase: "orphanCleanup".to_string(),
            update_id: None,
            pid: std::process::id(),
            started_at_ms: now_ms(),
        }
    }
}

impl UpdateJournal {
    fn new(plan: &UpdateApplyPlan, phase: UpdateJournalPhase) -> Self {
        Self {
            schema: UPDATE_JOURNAL_SCHEMA.to_string(),
            update_id: plan.update_id.clone(),
            phase,
            plan: plan.clone(),
            backup_payload_manifest_sha256: None,
            updated_at_ms: now_ms(),
            error_code: None,
        }
    }

    fn transition(&mut self, phase: UpdateJournalPhase, error_code: Option<&str>) {
        self.phase = phase;
        self.updated_at_ms = now_ms();
        self.error_code = error_code.map(str::to_string);
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateArtifactInfo {
    pub target: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckResponse {
    pub status: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub notes: Option<UpdateNotes>,
    pub artifact: Option<UpdateArtifactInfo>,
    pub checked_at_ms: u64,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateApplyResponse {
    pub status: String,
    pub version: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStartupResponse {
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptChoice {
    RemindLater,
    Skip7Days,
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateHelperLaunchOutcome {
    Launched,
    #[cfg(windows)]
    ElevationRequired,
}

impl PromptChoice {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "remindLater" => Ok(Self::RemindLater),
            "skip7days" => Ok(Self::Skip7Days),
            "dismiss" => Ok(Self::Dismiss),
            _ => Err("Unknown update prompt choice.".to_string()),
        }
    }
}

#[tauri::command]
pub async fn check_for_update(
    app: AppHandle,
    webview: Webview,
    coordinator: State<'_, UpdaterCoordinator>,
    force: Option<bool>,
    on_event: Option<JavaScriptChannelId>,
) -> Result<UpdateCheckResponse, UpdatePublicError> {
    let force = force.unwrap_or(false);
    let coordinator = coordinator.inner().clone();
    let channel = on_event.map(|id| id.channel_on(webview));
    tauri::async_runtime::spawn_blocking(move || {
        let operation = coordinator.begin("check")?;
        let reporter = ProgressReporter::new(operation.operation_id.clone(), channel);
        reporter.emit(UpdatePhase::Checking, "Checking for updates...");
        let response =
            check_for_update_inner(&app, force).map_err(UpdatePublicError::check_failed)?;
        reporter.emit(UpdatePhase::Completed, "Update check complete.");
        Ok(response)
    })
    .await
    .map_err(|_| UpdatePublicError::check_failed("update worker stopped".to_string()))?
}

#[tauri::command]
pub async fn record_update_prompt_choice(
    app: AppHandle,
    coordinator: State<'_, UpdaterCoordinator>,
    choice: String,
    version: String,
) -> Result<(), UpdatePublicError> {
    let coordinator = coordinator.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _operation = coordinator.begin("preferences")?;
        let choice = PromptChoice::parse(&choice).map_err(UpdatePublicError::preferences_failed)?;
        let version = parse_semver(&version).map_err(UpdatePublicError::preferences_failed)?;
        let now = now_ms();
        let mut prefs = load_prefs(&app).map_err(UpdatePublicError::preferences_failed)?;

        apply_prompt_choice(&mut prefs, choice, &version, now);
        save_prefs(&app, &prefs).map_err(UpdatePublicError::preferences_failed)
    })
    .await
    .map_err(|_| UpdatePublicError::preferences_failed("update worker stopped".to_string()))?
}

#[tauri::command]
pub async fn prepare_and_apply_update(
    app: AppHandle,
    webview: Webview,
    coordinator: State<'_, UpdaterCoordinator>,
    on_event: Option<JavaScriptChannelId>,
) -> Result<UpdateApplyResponse, UpdatePublicError> {
    let coordinator = coordinator.inner().clone();
    let channel = on_event.map(|id| id.channel_on(webview));
    tauri::async_runtime::spawn_blocking(move || {
        let operation = coordinator.begin("apply")?;
        let reporter = ProgressReporter::new(operation.operation_id.clone(), channel);
        prepare_and_apply_update_inner(app, &reporter).map_err(UpdatePublicError::prepare_failed)
    })
    .await
    .map_err(|_| UpdatePublicError::prepare_failed("update worker stopped".to_string()))?
}

fn prepare_and_apply_update_inner(
    app: AppHandle,
    reporter: &ProgressReporter,
) -> Result<UpdateApplyResponse, String> {
    reporter.emit(
        UpdatePhase::Checking,
        "Checking the signed update manifest...",
    );
    let check = check_for_update_inner(&app, true)?;
    if check.status != "available" {
        return Err(check
            .reason
            .unwrap_or_else(|| "No update is available.".to_string()));
    }

    let install_dir = install_dir()?;
    if !install_dir_writable(&install_dir) {
        return elevate_for_update(&app, &check);
    }
    let mut staging_claim = acquire_staging_lock(&install_dir)?;

    let manifest_bundle = fetch_verified_update_manifest()?;
    let manifest = manifest_bundle.manifest;
    let target = current_target()?;
    let artifact = manifest
        .artifacts
        .get(target)
        .cloned()
        .ok_or_else(|| "The update manifest does not include this platform.".to_string())?;
    validate_update_manifest(&manifest, Some(&load_prefs(&app)?))?;
    validate_artifact(target, &artifact)?;

    let plan = stage_update(&app, &manifest, &artifact, target, reporter)?;
    reporter.emit(
        UpdatePhase::LaunchingHelper,
        "Starting the protected update helper...",
    );
    let helper_path = match copy_staged_helper_to_temp(&plan) {
        Ok(path) => path,
        Err(error) => {
            return match cleanup_staged_update(&plan) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; staged update cleanup also failed: {cleanup_error}"
                )),
            };
        }
    };
    if let Err(error) = staging_claim.prepare_handoff(&plan) {
        return match cleanup_staged_update(&plan) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; staged update cleanup also failed: {cleanup_error}"
            )),
        };
    }
    let _launch_outcome = match launch_update_helper(&helper_path, &plan) {
        Ok(outcome) => outcome,
        Err(error) => {
            return match cleanup_staged_update(&plan) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; staged update cleanup also failed: {cleanup_error}"
                )),
            };
        }
    };

    #[cfg(windows)]
    if _launch_outcome == UpdateHelperLaunchOutcome::ElevationRequired {
        // Reuse the established elevated-app path. The elevated instance
        // restages the update with relaunch_via_shell=true, which prevents the
        // updated app from inheriting an administrator token from the helper.
        cleanup_staged_update(&plan)?;
        return elevate_for_update(&app, &check);
    }

    staging_claim.handoff();

    let version = plan.to_version.clone();
    reporter.emit(
        UpdatePhase::Restarting,
        "Restarting to finish the verified update...",
    );
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        app.exit(0);
    });

    Ok(UpdateApplyResponse {
        status: "restarting".to_string(),
        version,
        message: "The update is ready. Video For Lazies will restart to finish installing it."
            .to_string(),
    })
}

/// True when the app can create and delete files in its own install folder.
/// Probes the real update-state directory so the answer matches what staging
/// is about to do.
fn install_dir_writable(install_dir: &Path) -> bool {
    let probe_dir = install_dir.join(UPDATE_STATE_DIR);
    if fs::create_dir_all(&probe_dir).is_err() {
        return false;
    }
    let probe_path = probe_dir.join(format!(".write-probe-{}", std::process::id()));
    match fs::write(&probe_path, b"vfl-write-probe") {
        Ok(()) => {
            let _ = fs::remove_file(&probe_path);
            true
        }
        Err(_) => false,
    }
}

#[cfg(windows)]
fn elevate_for_update(
    app: &AppHandle,
    check: &UpdateCheckResponse,
) -> Result<UpdateApplyResponse, String> {
    if elevated_update_run() {
        return Err(
            "The app folder is still not writable even with administrator permission. \
             Move the app to a writable folder and update again."
                .to_string(),
        );
    }

    relaunch_self_elevated_for_update()?;

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        handle.exit(0);
    });

    Ok(UpdateApplyResponse {
        status: "elevating".to_string(),
        version: check.latest_version.clone().unwrap_or_default(),
        message:
            "Video For Lazies will restart with administrator permission to install the update."
                .to_string(),
    })
}

#[cfg(not(windows))]
fn elevate_for_update(
    _app: &AppHandle,
    _check: &UpdateCheckResponse,
) -> Result<UpdateApplyResponse, String> {
    Err(
        "The app folder is not writable, so the update cannot be installed. \
         Move the app to a writable folder and update again."
            .to_string(),
    )
}

/// Relaunches this executable with the elevated-update flag through
/// `Start-Process -Verb RunAs`, which triggers the Windows UAC prompt.
/// Returns an error when Windows declines (the user cancelled the prompt).
#[cfg(windows)]
fn relaunch_self_elevated_for_update() -> Result<(), String> {
    relaunch_self_elevated(ELEVATED_UPDATE_ARG)
}

#[cfg(windows)]
fn relaunch_self_elevated_for_recovery() -> Result<(), String> {
    relaunch_self_elevated(ELEVATED_RECOVERY_ARG)
}

#[cfg(windows)]
fn relaunch_self_elevated(argument: &str) -> Result<(), String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to locate the app executable: {e}"))?;
    let install_dir = install_dir()?;
    let command_text = format!(
        "Start-Process -FilePath {} -ArgumentList '{}' -WorkingDirectory {} -Verb RunAs",
        powershell_quote(&exe.to_string_lossy()),
        argument,
        powershell_quote(&install_dir.to_string_lossy()),
    );

    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(&command_text);
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }

    let status = command
        .status()
        .map_err(|e| format!("Failed to request administrator permission: {e}"))?;
    if !status.success() {
        return Err(
            "Windows did not grant administrator permission, so the update was not installed."
                .to_string(),
        );
    }
    Ok(())
}

/// PowerShell single-quoted literal: only embedded single quotes need
/// escaping (doubled), everything else is taken verbatim.
#[cfg_attr(not(windows), allow(dead_code))]
fn powershell_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "''"))
}

#[tauri::command]
pub async fn finalize_update_startup(
    app: AppHandle,
    webview: Webview,
    coordinator: State<'_, UpdaterCoordinator>,
    on_event: Option<JavaScriptChannelId>,
) -> Result<UpdateStartupResponse, UpdatePublicError> {
    let coordinator = coordinator.inner().clone();
    let channel = on_event.map(|id| id.channel_on(webview));
    tauri::async_runtime::spawn_blocking(move || {
        let operation = coordinator.begin("finalize")?;
        let reporter = ProgressReporter::new(operation.operation_id.clone(), channel);
        reporter.emit(UpdatePhase::Recovering, "Checking saved update state...");
        let response =
            finalize_update_startup_inner(&app).map_err(UpdatePublicError::finalize_failed)?;
        reporter.emit(UpdatePhase::Completed, "Saved update state checked.");
        Ok(response)
    })
    .await
    .map_err(|_| UpdatePublicError::finalize_failed("update worker stopped".to_string()))?
}

fn finalize_update_startup_inner(app: &AppHandle) -> Result<UpdateStartupResponse, String> {
    let install_dir = install_dir()?;
    let lock_path = apply_lock_path(&install_dir);
    let staging_lock_path = staging_lock_path(&install_dir);
    let recovery_lock_path = recovery_lock_path(&install_dir);

    let journal_path = update_journal_path(&install_dir);
    if !atomic_path_exists(&journal_path) {
        if staging_lock_path.exists()
            && !lock_path.exists()
            && !atomic_path_exists(&update_pending_path(&install_dir))
        {
            let _ = reclaim_orphan_staging_without_journal(&install_dir);
        }
        if lock_path.exists() || staging_lock_path.exists() || recovery_lock_path.exists() {
            cleanup_old_temp_helpers();
            return Ok(recovery_required_response(
                "An update claim exists without a readable journal. No files were changed or cleaned. Close the app and reopen it once. If the claim remains, keep the portable folder and download a fresh copy.",
            ));
        }
        let response = finalize_legacy_pending_update(app, &install_dir)?;
        cleanup_old_temp_helpers();
        return Ok(response);
    }

    let journal: UpdateJournal = match read_atomic_json(&journal_path, "update journal") {
        Ok(journal) => journal,
        Err(_) => {
            cleanup_old_temp_helpers();
            return Ok(recovery_required_response(
                "The saved update journal is damaged. No cleanup or replacement was attempted. Keep the portable folder and its .vfl-updates directory, then download a fresh copy or recover from the saved backup.",
            ));
        }
    };
    if validate_update_journal(&journal, &install_dir).is_err() {
        cleanup_old_temp_helpers();
        return Ok(recovery_required_response(
            "The saved update journal could not be trusted. No cleanup or replacement was attempted. Keep the portable folder and its .vfl-updates directory, then download a fresh copy or recover from the saved backup.",
        ));
    }

    let destructive_phase = matches!(
        journal.phase,
        UpdateJournalPhase::Replacing
            | UpdateJournalPhase::RollingBack
            | UpdateJournalPhase::RecoveryRequired
    );
    if !destructive_phase
        && (lock_path.exists() || staging_lock_path.exists() || recovery_lock_path.exists())
    {
        match reclaim_stale_locks_for_safe_phase(&journal.plan) {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                cleanup_old_temp_helpers();
                return Ok(recovery_required_response(
                    "An update helper is still active, or its handoff did not finish. The updater left all files and backups in place. Close the app, wait a moment, and reopen it.",
                ));
            }
        }
    }

    let response = match journal.phase {
        UpdateJournalPhase::AwaitingStartup if journal.plan.to_version == current_version() => {
            complete_successful_update(app, &journal.plan)?;
            UpdateStartupResponse {
                status: "completed".to_string(),
                message: format!(
                    "Video For Lazies {} was installed successfully.",
                    journal.plan.to_version
                ),
            }
        }
        UpdateJournalPhase::RolledBack if journal.plan.from_version == current_version() => {
            cleanup_update_artifacts(&journal.plan, true)?;
            UpdateStartupResponse {
                status: "recovered".to_string(),
                message:
                    "The interrupted update was rolled back. Your previous version is ready to use."
                        .to_string(),
            }
        }
        UpdateJournalPhase::Staged
        | UpdateJournalPhase::WaitingForParent
        | UpdateJournalPhase::BackingUp
        | UpdateJournalPhase::BackupComplete => {
            cleanup_update_artifacts(&journal.plan, true)?;
            UpdateStartupResponse {
                status: "recovered".to_string(),
                message: "An update stopped before file replacement began. Its temporary files were cleared and the installed app was not changed."
                    .to_string(),
            }
        }
        UpdateJournalPhase::Replacing
        | UpdateJournalPhase::RollingBack
        | UpdateJournalPhase::RecoveryRequired => match start_automatic_recovery(app, &journal) {
            Ok(response) => response,
            Err(_internal) => recovery_required_response(
                "Automatic recovery could not start safely. The updater preserved the staged files and verified backup. Close the app and try reopening it once. If recovery still cannot start, keep the portable folder and download a fresh copy.",
            ),
        },
        UpdateJournalPhase::AwaitingStartup | UpdateJournalPhase::RolledBack => {
            recovery_required_response(
                "The running app version does not match the saved update state. The updater preserved the staged files and backup. Restart from the same portable folder, or download a fresh copy before removing .vfl-updates.",
            )
        }
    };
    cleanup_old_temp_helpers();
    Ok(response)
}

fn finalize_legacy_pending_update(
    app: &AppHandle,
    install_dir: &Path,
) -> Result<UpdateStartupResponse, String> {
    let pending_path = update_pending_path(install_dir);
    if !atomic_path_exists(&pending_path) {
        return Ok(UpdateStartupResponse {
            status: "none".to_string(),
            message: "No pending update state was found.".to_string(),
        });
    }

    let plan: UpdateApplyPlan = match read_atomic_json(&pending_path, "legacy pending update") {
        Ok(plan) => plan,
        Err(_) => {
            return Ok(recovery_required_response(
                "Legacy update state is damaged. No cleanup was attempted. Keep the portable folder and its .vfl-updates directory, then download a fresh copy.",
            ));
        }
    };
    if validate_apply_plan(&plan).is_err() || plan.install_dir != install_dir {
        return Ok(recovery_required_response(
            "Legacy update state could not be trusted. No cleanup was attempted. Keep the portable folder and its .vfl-updates directory, then download a fresh copy.",
        ));
    }
    if plan.to_version != current_version() {
        return Ok(recovery_required_response(
            "The running app version does not match the pending legacy update. The saved backup was preserved. Restart from the same portable folder or download a fresh copy.",
        ));
    }

    complete_successful_update(app, &plan)?;
    Ok(UpdateStartupResponse {
        status: "completed".to_string(),
        message: format!(
            "Video For Lazies {} was installed successfully.",
            plan.to_version
        ),
    })
}

fn recovery_required_response(message: &str) -> UpdateStartupResponse {
    UpdateStartupResponse {
        status: "recoveryRequired".to_string(),
        message: message.to_string(),
    }
}

fn start_automatic_recovery(
    app: &AppHandle,
    journal: &UpdateJournal,
) -> Result<UpdateStartupResponse, String> {
    if let Err(error) = verify_recovery_materials(journal) {
        let mut failed = journal.clone();
        failed.transition(
            UpdateJournalPhase::RecoveryRequired,
            Some("recovery-material-invalid"),
        );
        let journal_error = write_update_journal(&failed).err();
        return Err(match journal_error {
            Some(journal_error) => format!(
                "Recovery material validation failed: {error}; recording the failure also failed: {journal_error}"
            ),
            None => format!("Recovery material validation failed: {error}"),
        });
    }

    if !install_dir_writable(&journal.plan.install_dir) {
        #[cfg(windows)]
        {
            if elevated_recovery_run() {
                return Err(
                    "The portable folder is still not writable with administrator permission."
                        .to_string(),
                );
            }
            relaunch_self_elevated_for_recovery()?;
            let handle = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                handle.exit(0);
            });
            return Ok(UpdateStartupResponse {
                status: "elevatingRecovery".to_string(),
                message: "Video For Lazies will restart with administrator permission to recover the previous version."
                    .to_string(),
            });
        }
        #[cfg(not(windows))]
        {
            return Err("The portable folder is not writable for automatic recovery.".to_string());
        }
    }

    let recovery_claim = acquire_recovery_staging_lock(&journal.plan)?;
    let helper_path = copy_staged_helper_to_temp(&journal.plan)?;
    let mut recovery_journal = journal.clone();
    recovery_journal.transition(
        UpdateJournalPhase::RollingBack,
        Some("automatic-recovery-requested"),
    );
    write_update_journal(&recovery_journal)?;
    let helper_pid = launch_recovery_helper(&helper_path, &journal.plan, std::process::id())?;
    recovery_claim.handoff_to_helper(&journal.plan, helper_pid)?;

    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        handle.exit(0);
    });
    Ok(UpdateStartupResponse {
        status: "recovering".to_string(),
        message: "The previous version is being restored from the verified backup. Video For Lazies will restart when recovery finishes."
            .to_string(),
    })
}

fn verify_recovery_materials(
    journal: &UpdateJournal,
) -> Result<(PayloadManifest, PayloadManifest), String> {
    let plan = &journal.plan;
    let expected_backup_digest = journal
        .backup_payload_manifest_sha256
        .as_deref()
        .ok_or_else(|| "Update journal is missing its verified backup digest.".to_string())?;
    let new_manifest = verify_staged_payload(plan)?;
    let (old_manifest, old_manifest_digest) = read_payload_manifest_with_digest(&plan.backup_dir)?;
    if old_manifest_digest != expected_backup_digest {
        return Err("Backup payload manifest does not match the recorded digest.".to_string());
    }
    validate_payload_manifest(
        &plan.backup_dir,
        &old_manifest,
        &plan.from_version,
        &plan.target,
        true,
    )?;
    if sha256_file(&plan.backup_dir.join(PAYLOAD_MANIFEST_FILE_NAME))? != expected_backup_digest {
        return Err("Backup payload manifest changed during recovery validation.".to_string());
    }
    Ok((old_manifest, new_manifest))
}

pub fn run_update_helper_cli() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") | Some("version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            std::process::ExitCode::SUCCESS
        }
        Some("--self-test") | Some("self-test") => {
            println!("vfl-update-helper ok");
            std::process::ExitCode::SUCCESS
        }
        Some("apply") => {
            let Some(flag) = args.next() else {
                eprintln!("usage: vfl-update-helper apply --plan <path>");
                return std::process::ExitCode::from(2);
            };
            let Some(plan_path) = args.next() else {
                eprintln!("usage: vfl-update-helper apply --plan <path>");
                return std::process::ExitCode::from(2);
            };
            if flag != "--plan" {
                eprintln!("usage: vfl-update-helper apply --plan <path>");
                return std::process::ExitCode::from(2);
            }
            match apply_update_plan(Path::new(&plan_path)) {
                Ok(()) => {
                    #[cfg(debug_assertions)]
                    write_debug_cli_outcome(0);
                    std::process::ExitCode::SUCCESS
                }
                Err(_internal) => {
                    #[cfg(debug_assertions)]
                    write_debug_cli_outcome(1);
                    eprintln!(
                        "The update could not be installed safely. Restart Video For Lazies and try again. If the problem continues, keep the .vfl-updates backup and download a fresh portable copy."
                    );
                    std::process::ExitCode::from(1)
                }
            }
        }
        Some("recover") => {
            let (Some(plan_flag), Some(plan_path), Some(parent_flag), Some(parent_pid)) =
                (args.next(), args.next(), args.next(), args.next())
            else {
                eprintln!("usage: vfl-update-helper recover --plan <path> --parent-pid <pid>");
                return std::process::ExitCode::from(2);
            };
            let Ok(parent_pid) = parent_pid.parse::<u32>() else {
                eprintln!("usage: vfl-update-helper recover --plan <path> --parent-pid <pid>");
                return std::process::ExitCode::from(2);
            };
            if plan_flag != "--plan" || parent_flag != "--parent-pid" || args.next().is_some() {
                eprintln!("usage: vfl-update-helper recover --plan <path> --parent-pid <pid>");
                return std::process::ExitCode::from(2);
            }
            match recover_update_plan(Path::new(&plan_path), parent_pid, true) {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(_internal) => {
                    eprintln!(
                        "Automatic update recovery could not finish safely. Keep the .vfl-updates backup and download a fresh portable copy if retrying does not help."
                    );
                    std::process::ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!(
                "usage: vfl-update-helper [--version|--self-test|apply --plan <path>|recover --plan <path> --parent-pid <pid>]"
            );
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(debug_assertions)]
fn write_debug_cli_outcome(code: u8) {
    if std::env::var("VFL_UPDATER_TEST_MODE").ok().as_deref() != Some("1") {
        return;
    }
    let Some(path) = std::env::var_os("VFL_UPDATER_TEST_EXIT_MARKER").map(PathBuf::from) else {
        return;
    };
    if !path.is_absolute() {
        return;
    }
    if let Ok(mut file) = File::create(path) {
        let _ = file.write_all(code.to_string().as_bytes());
        let _ = file.sync_all();
    }
}

struct VerifiedManifest {
    manifest: UpdateManifest,
}

fn check_for_update_inner(app: &AppHandle, force: bool) -> Result<UpdateCheckResponse, String> {
    let now = now_ms();
    let mut prefs = load_prefs(app)?;
    if let Some(until) = prefs.suppress_prompts_until_ms
        && !force
        && now < until
    {
        return Ok(skipped_response(
            now,
            "Update prompts are paused for now.".to_string(),
        ));
    }

    if !force
        && prefs.remind_later_version.is_none()
        && let Some(last_checked) = prefs.last_checked_at_ms
        && now.saturating_sub(last_checked) < CHECK_INTERVAL_MS
    {
        return Ok(skipped_response(
            now,
            "The daily update check already ran.".to_string(),
        ));
    }

    let bundle = fetch_verified_update_manifest()?;
    let manifest = bundle.manifest;
    validate_update_manifest(&manifest, Some(&prefs))?;
    let latest = parse_semver(&manifest.version)?;
    let current = parse_semver(current_version())?;

    if latest <= current {
        record_update_check_result(&mut prefs, &latest, &current, now);
        save_prefs(app, &prefs)?;
        return Ok(UpdateCheckResponse {
            status: "current".to_string(),
            current_version: current_version().to_string(),
            latest_version: Some(latest.to_string()),
            release_url: Some(manifest.release_url),
            notes: Some(manifest.notes),
            artifact: None,
            checked_at_ms: now,
            reason: Some("Video For Lazies is up to date.".to_string()),
        });
    }

    let target = current_target()?;
    let artifact = manifest
        .artifacts
        .get(target)
        .ok_or_else(|| "The update manifest does not include this platform.".to_string())?;
    validate_artifact(target, artifact)?;

    record_update_check_result(&mut prefs, &latest, &current, now);
    save_prefs(app, &prefs)?;

    Ok(UpdateCheckResponse {
        status: "available".to_string(),
        current_version: current_version().to_string(),
        latest_version: Some(latest.to_string()),
        release_url: Some(manifest.release_url),
        notes: Some(manifest.notes),
        artifact: Some(UpdateArtifactInfo {
            target: target.to_string(),
            file_name: artifact.file_name.clone(),
            size_bytes: artifact.size_bytes,
            url: artifact.url.clone(),
            sha256: artifact.sha256.clone(),
        }),
        checked_at_ms: now,
        reason: None,
    })
}

fn skipped_response(now: u64, reason: String) -> UpdateCheckResponse {
    UpdateCheckResponse {
        status: "skipped".to_string(),
        current_version: current_version().to_string(),
        latest_version: None,
        release_url: None,
        notes: None,
        artifact: None,
        checked_at_ms: now,
        reason: Some(reason),
    }
}

fn apply_prompt_choice(prefs: &mut UpdatePrefs, choice: PromptChoice, version: &Version, now: u64) {
    match choice {
        PromptChoice::RemindLater | PromptChoice::Dismiss => {
            prefs.remind_later_version = Some(version.to_string());
        }
        PromptChoice::Skip7Days => {
            prefs.suppress_prompts_until_ms = Some(now.saturating_add(SKIP_INTERVAL_MS));
            prefs.remind_later_version = None;
        }
    }
}

fn record_update_check_result(
    prefs: &mut UpdatePrefs,
    latest: &Version,
    current: &Version,
    now: u64,
) {
    prefs.last_checked_at_ms = Some(now);
    if latest > current {
        prefs.remind_later_version = Some(latest.to_string());
    } else {
        prefs.remind_later_version = None;
    }
    set_highest_trusted_version(prefs, latest);
}

fn fetch_verified_update_manifest() -> Result<VerifiedManifest, String> {
    #[cfg(debug_assertions)]
    if let Some(manifest) = load_digest_bound_debug_manifest()? {
        return Ok(VerifiedManifest { manifest });
    }

    let manifest_url = std::env::var("VFL_UPDATE_MANIFEST_URL")
        .unwrap_or_else(|_| UPDATE_MANIFEST_URL.to_string());
    let signature_url = std::env::var("VFL_UPDATE_SIGNATURE_URL")
        .unwrap_or_else(|_| format!("{manifest_url}{DEFAULT_SIGNATURE_SUFFIX}"));
    let allow_local_http = update_local_http_allowed();

    validate_manifest_url(&manifest_url, allow_local_http)?;
    validate_manifest_url(&signature_url, allow_local_http)?;

    let manifest_bytes = download_bytes(&manifest_url, 5 * 1024 * 1024)?;
    let signature_bytes = download_bytes(&signature_url, 1024 * 1024)?;
    verify_manifest_signature(&manifest_bytes, &signature_bytes)?;

    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("Update manifest is not valid JSON: {e}"))?;
    Ok(VerifiedManifest { manifest })
}

#[cfg(debug_assertions)]
fn load_digest_bound_debug_manifest() -> Result<Option<UpdateManifest>, String> {
    let test_mode = std::env::var("VFL_UPDATER_TEST_MODE").unwrap_or_default();
    let path = std::env::var_os("VFL_UPDATER_TEST_MANIFEST_PATH");
    let digest = std::env::var("VFL_UPDATER_TEST_MANIFEST_SHA256").ok();
    if test_mode.is_empty() && path.is_none() && digest.is_none() {
        return Ok(None);
    }
    if test_mode != "1" {
        return Err("The debug updater trust seam requires explicit test mode.".to_string());
    }
    let path = path
        .map(PathBuf::from)
        .ok_or_else(|| "The debug updater manifest path is missing.".to_string())?;
    if !path.is_absolute() {
        return Err("The debug updater manifest path must be absolute.".to_string());
    }
    let digest =
        digest.ok_or_else(|| "The debug updater manifest digest is missing.".to_string())?;
    validate_sha256(&digest, "debug updater manifest SHA256")?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|e| format!("Failed to inspect the debug updater manifest: {e}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 5 * 1024 * 1024
    {
        return Err("The debug updater manifest is not a bounded regular file.".to_string());
    }
    let raw =
        fs::read(&path).map_err(|e| format!("Failed to read the debug updater manifest: {e}"))?;
    if sha256_bytes(&raw) != digest {
        return Err("The debug updater manifest digest does not match.".to_string());
    }
    let manifest: UpdateManifest = serde_json::from_slice(&raw)
        .map_err(|e| format!("The debug updater manifest is invalid: {e}"))?;
    validate_update_manifest(&manifest, None)?;
    Ok(Some(manifest))
}

fn verify_manifest_signature(manifest_bytes: &[u8], signature_bytes: &[u8]) -> Result<(), String> {
    let signature_text = decode_tauri_signer_text(signature_bytes)
        .map_err(|e| format!("Update manifest signature is invalid: {e}"))?;
    let signature = Signature::decode(&signature_text)
        .map_err(|e| format!("Update manifest signature is invalid: {e:?}"))?;

    for public_key_text in UPDATE_PUBLIC_KEYS
        .iter()
        .filter_map(|key| decode_tauri_signer_text(key.as_bytes()).ok())
    {
        let Ok(public_key) = PublicKey::decode(&public_key_text) else {
            continue;
        };
        if public_key.verify(manifest_bytes, &signature, false).is_ok() {
            return Ok(());
        }
    }

    Err("Update manifest signature is not trusted.".to_string())
}

fn decode_tauri_signer_text(raw: &[u8]) -> Result<String, String> {
    let trimmed = std::str::from_utf8(raw)
        .map_err(|e| format!("signature material is not UTF-8: {e}"))?
        .trim();
    if trimmed.starts_with("untrusted comment:") {
        return Ok(trimmed.to_string());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| format!("base64 decode failed: {e}"))?;
    String::from_utf8(decoded).map_err(|e| format!("decoded signature material is not UTF-8: {e}"))
}

fn validate_update_manifest(
    manifest: &UpdateManifest,
    prefs: Option<&UpdatePrefs>,
) -> Result<(), String> {
    if manifest.schema != UPDATE_MANIFEST_SCHEMA {
        return Err("Unsupported update manifest schema.".to_string());
    }
    if manifest.app_id != APP_ID {
        return Err("Update manifest is for a different app.".to_string());
    }
    if manifest.channel != UPDATE_CHANNEL {
        return Err("Update manifest is for a different channel.".to_string());
    }
    if manifest.min_updater_protocol > UPDATE_PROTOCOL_VERSION {
        return Err("This app is too old to install the latest update.".to_string());
    }

    let latest = parse_semver(&manifest.version)?;
    if manifest.release_tag != format!("v{latest}") {
        return Err("Update manifest release tag does not match its version.".to_string());
    }
    validate_release_url(&manifest.release_url)?;
    validate_release_url(&manifest.notes.url)?;
    if let Some(prefs) = prefs
        && let Some(highest) = prefs.highest_trusted_version.as_deref()
    {
        let highest = parse_semver(highest)?;
        if latest < highest {
            return Err("Update manifest is older than a previously trusted update.".to_string());
        }
    }

    for (target, artifact) in &manifest.artifacts {
        validate_artifact(target, artifact)?;
    }

    Ok(())
}

fn validate_artifact(target: &str, artifact: &UpdateArtifact) -> Result<(), String> {
    match target {
        "windows-x64" | "linux-x64" => {}
        _ => return Err(format!("Unsupported update target in manifest: {target}")),
    }
    if artifact.root_dir != PORTABLE_ROOT_DIR_NAME {
        return Err("Update artifact has an unexpected portable root.".to_string());
    }
    let artifact_name = Path::new(&artifact.file_name);
    if artifact.file_name.is_empty()
        || artifact.file_name.contains(['/', '\\', '\0', ':'])
        || artifact_name.file_name().and_then(|name| name.to_str())
            != Some(artifact.file_name.as_str())
        || !artifact.file_name.ends_with(".zip")
    {
        return Err("Update artifact filename is unsafe.".to_string());
    }
    if artifact.size_bytes == 0 || artifact.size_bytes > MAX_UPDATE_BYTES {
        return Err("Update artifact size is outside the supported range.".to_string());
    }
    validate_sha256(&artifact.sha256, "artifact SHA256")?;
    validate_download_url(&artifact.url, update_local_http_allowed())?;
    let download_url =
        reqwest::Url::parse(&artifact.url).map_err(|e| format!("Invalid artifact URL: {e}"))?;
    if download_url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        != Some(artifact.file_name.as_str())
    {
        return Err("Update artifact filename does not match its download URL.".to_string());
    }
    if artifact.payload_manifest.path
        != format!("{PORTABLE_ROOT_DIR_NAME}/{PAYLOAD_MANIFEST_FILE_NAME}")
    {
        return Err("Update artifact payload manifest path is unexpected.".to_string());
    }
    validate_sha256(&artifact.payload_manifest.sha256, "payload manifest SHA256")?;
    Ok(())
}

fn validate_manifest_url(raw: &str, allow_local_http: bool) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("Invalid update URL: {e}"))?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_local_http && is_local_host(url.host_str()) => Ok(()),
        _ => Err("Update metadata must use HTTPS.".to_string()),
    }
}

fn validate_release_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("Invalid release URL: {e}"))?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err("Release links must point to GitHub.".to_string());
    }
    if !url
        .path()
        .starts_with("/Setmaster/Video_For_Lazies/releases/")
    {
        return Err("Release links must point to the Video For Lazies repository.".to_string());
    }
    Ok(())
}

fn validate_download_url(raw: &str, allow_local_http: bool) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("Invalid artifact URL: {e}"))?;
    match url.scheme() {
        "https" => {
            if url.host_str() != Some("github.com")
                || !url
                    .path()
                    .starts_with("/Setmaster/Video_For_Lazies/releases/download/")
            {
                return Err("Update artifacts must come from GitHub Releases.".to_string());
            }
            Ok(())
        }
        "http" if allow_local_http && is_local_host(url.host_str()) => Ok(()),
        _ => Err("Update artifacts must use HTTPS.".to_string()),
    }
}

fn is_local_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost") | Some("127.0.0.1") | Some("::1"))
}

fn update_local_http_allowed() -> bool {
    matches!(
        std::env::var("VFL_UPDATE_ALLOW_LOCAL_HTTP")
            .unwrap_or_default()
            .as_str(),
        "1" | "true" | "TRUE" | "yes" | "on"
    )
}

fn stage_update(
    app: &AppHandle,
    manifest: &UpdateManifest,
    artifact: &UpdateArtifact,
    target: &str,
    reporter: &ProgressReporter,
) -> Result<UpdateApplyPlan, String> {
    let install_dir = install_dir()?;
    let update_id = format!("{}-{}", manifest.version, now_ms());
    let updates_dir = install_dir.join(UPDATE_STATE_DIR);
    let stage_root = updates_dir.join("staged").join(&update_id);
    let extract_dir = stage_root.join("extract");
    let zip_path = stage_root.join(&artifact.file_name);
    let backup_dir = updates_dir.join("backups").join(&update_id);
    let orphan_plan_path = updates_dir.join("plans").join(format!("{update_id}.json"));

    let staged_result = (|| -> Result<UpdateApplyPlan, String> {
        fs::create_dir_all(&stage_root)
            .map_err(|e| format!("Failed to create update stage: {e}"))?;
        fs::create_dir_all(&extract_dir)
            .map_err(|e| format!("Failed to create update extraction directory: {e}"))?;
        fs::create_dir_all(&backup_dir)
            .map_err(|e| format!("Failed to create update backup: {e}"))?;

        reporter.emit_bytes(
            UpdatePhase::Downloading,
            Some(0),
            Some(artifact.size_bytes),
            "Downloading the verified update archive...",
        );
        let mut last_reported_bytes = 0u64;
        download_file(
            &artifact.url,
            &zip_path,
            artifact.size_bytes,
            |written, total| {
                if written == total || written.saturating_sub(last_reported_bytes) >= 1024 * 1024 {
                    last_reported_bytes = written;
                    reporter.emit_bytes(
                        UpdatePhase::Downloading,
                        Some(written),
                        Some(total),
                        "Downloading the verified update archive...",
                    );
                }
            },
        )?;
        reporter.emit(
            UpdatePhase::VerifyingArchive,
            "Verifying the downloaded archive...",
        );
        let actual_hash = sha256_file(&zip_path)?;
        if actual_hash != artifact.sha256 {
            return Err(
                "Downloaded update archive hash does not match the signed manifest.".to_string(),
            );
        }

        reporter.emit(UpdatePhase::Extracting, "Extracting the verified update...");
        extract_update_zip(&zip_path, &extract_dir)?;
        let portable_dir = extract_dir.join(PORTABLE_ROOT_DIR_NAME);
        reporter.emit(
            UpdatePhase::VerifyingPayload,
            "Verifying every staged update file...",
        );
        let payload = read_payload_manifest(&portable_dir)?;
        validate_payload_manifest(&portable_dir, &payload, &manifest.version, target, true)?;
        let payload_hash = sha256_file(&portable_dir.join(PAYLOAD_MANIFEST_FILE_NAME))?;
        if payload_hash != artifact.payload_manifest.sha256 {
            return Err(
                "Payload manifest hash does not match the signed update manifest.".to_string(),
            );
        }

        let plan = UpdateApplyPlan {
            schema: APPLY_PLAN_SCHEMA.to_string(),
            update_id,
            from_version: current_version().to_string(),
            to_version: manifest.version.clone(),
            target: target.to_string(),
            install_dir,
            stage_dir: portable_dir,
            backup_dir: backup_dir.clone(),
            parent_pid: std::process::id(),
            executable_name: main_executable_name()?.to_string(),
            helper_name: helper_executable_name()?.to_string(),
            expected_payload_manifest_sha256: Some(artifact.payload_manifest.sha256.clone()),
            relaunch_via_shell: elevated_update_run(),
        };
        validate_apply_plan(&plan)?;

        let plan_path = apply_plan_path(&plan);
        if let Some(parent) = plan_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update plan dir: {e}"))?;
        }
        atomic_write_json(&plan_path, &plan, "update plan")?;
        let staging_state_result = (|| {
            let mut prefs = load_prefs(app)?;
            set_highest_trusted_version(&mut prefs, &parse_semver(&manifest.version)?);
            save_prefs(app, &prefs)?;
            let journal = UpdateJournal::new(&plan, UpdateJournalPhase::Staged);
            write_update_journal(&journal)
        })();
        if let Err(error) = staging_state_result {
            return match cleanup_staged_update(&plan) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; staged update cleanup also failed: {cleanup_error}"
                )),
            };
        }

        reporter.emit(
            UpdatePhase::Staging,
            "The verified update is staged safely.",
        );

        Ok(plan)
    })();

    finish_staged_result(staged_result, &stage_root, &backup_dir, &orphan_plan_path)
}

fn finish_staged_result(
    staged_result: Result<UpdateApplyPlan, String>,
    stage_root: &Path,
    backup_dir: &Path,
    orphan_plan_path: &Path,
) -> Result<UpdateApplyPlan, String> {
    match staged_result {
        Ok(plan) => Ok(plan),
        Err(error) => {
            let mut cleanup_errors = Vec::new();
            if let Err(cleanup_error) = remove_dir_if_exists(stage_root, "failed update stage") {
                cleanup_errors.push(cleanup_error);
            }
            if let Err(cleanup_error) = remove_dir_if_exists(backup_dir, "failed update backup") {
                cleanup_errors.push(cleanup_error);
            }
            if let Err(cleanup_error) = remove_atomic_file(orphan_plan_path) {
                cleanup_errors.push(cleanup_error);
            }
            if cleanup_errors.is_empty() {
                Err(error)
            } else {
                Err(format!(
                    "{error}; staging cleanup also failed: {}",
                    cleanup_errors.join("; ")
                ))
            }
        }
    }
}

fn copy_staged_helper_to_temp(plan: &UpdateApplyPlan) -> Result<PathBuf, String> {
    let payload = verify_staged_payload(plan)?;
    let helper_entry = payload
        .files
        .iter()
        .find(|entry| entry.path == plan.helper_name)
        .ok_or_else(|| "The staged payload does not list its update helper.".to_string())?;
    let helper_source = plan.stage_dir.join(&plan.helper_name);
    let helper_target = std::env::temp_dir().join(format!(
        "{}{}{}",
        TEMP_HELPER_PREFIX,
        plan.update_id,
        executable_suffix()
    ));
    remove_file_with_retries(&helper_target)?;
    retry_copy_file(&helper_source, &helper_target)?;
    let copied_metadata = fs::symlink_metadata(&helper_target)
        .map_err(|e| format!("Failed to verify copied update helper metadata: {e}"))?;
    if copied_metadata.file_type().is_symlink()
        || !copied_metadata.is_file()
        || copied_metadata.len() != helper_entry.size_bytes
        || sha256_file(&helper_target)? != helper_entry.sha256.as_str()
    {
        return Err("The copied update helper failed verification.".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&helper_target, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to make update helper executable: {e}"))?;
    }
    Ok(helper_target)
}

fn launch_update_helper(
    helper_path: &Path,
    plan: &UpdateApplyPlan,
) -> Result<UpdateHelperLaunchOutcome, String> {
    let mut command = Command::new(helper_path);
    command
        .arg("apply")
        .arg("--plan")
        .arg(apply_plan_path(plan))
        .current_dir(&plan.install_dir);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    match command.spawn() {
        Ok(_) => Ok(UpdateHelperLaunchOutcome::Launched),
        Err(error) => {
            #[cfg(windows)]
            if is_elevation_required_error(&error) {
                return Ok(UpdateHelperLaunchOutcome::ElevationRequired);
            }

            Err(format!("Failed to launch update helper: {error}"))
        }
    }
}

fn launch_recovery_helper(
    helper_path: &Path,
    plan: &UpdateApplyPlan,
    parent_pid: u32,
) -> Result<u32, String> {
    let mut command = Command::new(helper_path);
    command
        .arg("recover")
        .arg("--plan")
        .arg(apply_plan_path(plan))
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .current_dir(&plan.install_dir);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    command
        .spawn()
        .map(|child| child.id())
        .map_err(|e| format!("Failed to launch the update recovery helper: {e}"))
}

#[cfg(any(windows, test))]
fn is_elevation_required_error(error: &io::Error) -> bool {
    error.raw_os_error() == Some(740)
}

fn apply_update_plan(plan_path: &Path) -> Result<(), String> {
    apply_update_plan_inner(plan_path, true)
}

fn apply_update_plan_inner(plan_path: &Path, relaunch: bool) -> Result<(), String> {
    apply_update_plan_inner_with_fault(plan_path, relaunch, ApplyFault::default())
}

#[derive(Debug, Clone, Copy, Default)]
struct ApplyFault {
    fail_after_replacements: Option<usize>,
    remove_backup_before_rollback: bool,
    #[cfg(test)]
    pause_before_main_publication: bool,
    #[cfg(test)]
    new_target_collision: Option<TestNewTargetCollision>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
enum TestNewTargetCollision {
    RegularFile,
    #[cfg(unix)]
    DanglingSymlink,
}

fn apply_update_plan_inner_with_fault(
    plan_path: &Path,
    relaunch: bool,
    fault: ApplyFault,
) -> Result<(), String> {
    apply_update_plan_inner_with_options(plan_path, relaunch, fault, None)
}

fn apply_update_plan_inner_with_options(
    plan_path: &Path,
    relaunch: bool,
    fault: ApplyFault,
    verified_legacy_manifest: Option<&UpdateManifest>,
) -> Result<(), String> {
    let mut plan: UpdateApplyPlan = read_atomic_json(plan_path, "update plan")?;
    validate_apply_plan(&plan)?;
    if plan_path != apply_plan_path(&plan) {
        return Err("Update apply plan was loaded from an unexpected location.".to_string());
    }
    if preflight_prior_update_before_apply(&plan, relaunch)? {
        return Ok(());
    }
    let mut apply_lock = acquire_apply_lock(&plan)?;
    let mut journal =
        prepare_apply_journal_with_manifest(plan_path, &mut plan, verified_legacy_manifest)?;
    journal.transition(UpdateJournalPhase::WaitingForParent, None);
    write_update_journal(&journal)?;
    wait_for_parent_to_exit(plan.parent_pid)?;

    let new_manifest = verify_staged_payload(&plan)?;
    let old_manifest = read_payload_manifest(&plan.install_dir)?;
    validate_payload_manifest(
        &plan.install_dir,
        &old_manifest,
        &plan.from_version,
        &plan.target,
        false,
    )?;

    let mut old_paths = manifest_owned_paths(&old_manifest);
    old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));

    let mut new_paths = manifest_owned_paths(&new_manifest);
    new_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));
    validate_no_unknown_collisions(&plan, &old_paths, &new_paths)?;

    journal.transition(UpdateJournalPhase::BackingUp, None);
    write_update_journal(&journal)?;
    let backup_payload_manifest_sha256 = backup_existing_owned_files(&plan, &old_paths)?;
    journal.backup_payload_manifest_sha256 = Some(backup_payload_manifest_sha256);
    journal.transition(UpdateJournalPhase::BackupComplete, None);
    write_update_journal(&journal)?;

    // Re-read the signed manifest digest and every staged file immediately
    // before replacement. This closes the download-to-apply mutation window.
    let new_manifest = verify_staged_payload(&plan)?;
    journal.transition(UpdateJournalPhase::Replacing, None);
    write_update_journal(&journal)?;
    match replace_owned_files(&plan, &old_paths, &new_paths, &new_manifest, fault) {
        Ok(()) => {
            journal.transition(UpdateJournalPhase::AwaitingStartup, None);
            if let Err(error) = write_update_journal(&journal) {
                return rollback_after_apply_failure(
                    &plan,
                    &mut journal,
                    &old_paths,
                    &new_paths,
                    &new_manifest,
                    fault,
                    error,
                );
            }
            if let Err(error) = atomic_write_json(
                &update_pending_path(&plan.install_dir),
                &plan,
                "pending update state",
            ) {
                return rollback_after_apply_failure(
                    &plan,
                    &mut journal,
                    &old_paths,
                    &new_paths,
                    &new_manifest,
                    fault,
                    error,
                );
            }
            apply_lock.release()?;
            if relaunch {
                launch_updated_app(&plan)?;
            }
            Ok(())
        }
        Err(error) => rollback_after_apply_failure(
            &plan,
            &mut journal,
            &old_paths,
            &new_paths,
            &new_manifest,
            fault,
            error,
        ),
    }
}

/// A v1.9.1 app does not understand journals or apply locks. If a migrated
/// helper is killed during replacement, that old app can only launch another
/// new helper with a fresh legacy plan. Recover the prior trusted transaction
/// before trying to claim the retry plan, so the old frontend never needs to
/// interpret the new state format.
fn preflight_prior_update_before_apply(
    retry_plan: &UpdateApplyPlan,
    relaunch: bool,
) -> Result<bool, String> {
    let journal_path = update_journal_path(&retry_plan.install_dir);
    if !atomic_path_exists(&journal_path) {
        return Ok(false);
    }
    let mut prior: UpdateJournal = read_atomic_json(&journal_path, "prior update journal")?;
    validate_update_journal(&prior, &retry_plan.install_dir)?;

    if prior.plan == *retry_plan && prior.phase == UpdateJournalPhase::Staged {
        return Ok(false);
    }

    match prior.phase {
        UpdateJournalPhase::Replacing
        | UpdateJournalPhase::RollingBack
        | UpdateJournalPhase::RecoveryRequired => {
            verify_recovery_materials(&prior)?;
            let recovery_claim = acquire_recovery_staging_lock(&prior.plan)?;
            prior.transition(
                UpdateJournalPhase::RollingBack,
                Some("retry-helper-recovery"),
            );
            write_update_journal(&prior)?;
            recovery_claim.handoff_to_helper(&prior.plan, std::process::id())?;
            recover_update_plan(
                &apply_plan_path(&prior.plan),
                retry_plan.parent_pid,
                relaunch,
            )?;
            Ok(true)
        }
        UpdateJournalPhase::Staged
        | UpdateJournalPhase::WaitingForParent
        | UpdateJournalPhase::BackingUp
        | UpdateJournalPhase::BackupComplete
        | UpdateJournalPhase::RolledBack => {
            let has_prior_claim = apply_lock_path(&retry_plan.install_dir).exists()
                || staging_lock_path(&retry_plan.install_dir).exists()
                || recovery_lock_path(&retry_plan.install_dir).exists();
            if has_prior_claim && !reclaim_stale_locks_for_safe_phase(&prior.plan)? {
                return Err("A prior update helper is still active.".to_string());
            }
            cleanup_update_artifacts(&prior.plan, true)?;
            Ok(false)
        }
        UpdateJournalPhase::AwaitingStartup => Err(
            "A previously installed update must finish startup before another update can apply."
                .to_string(),
        ),
    }
}

#[cfg(test)]
fn prepare_apply_journal(
    plan_path: &Path,
    plan: &mut UpdateApplyPlan,
) -> Result<UpdateJournal, String> {
    prepare_apply_journal_with_manifest(plan_path, plan, None)
}

fn prepare_apply_journal_with_manifest(
    plan_path: &Path,
    plan: &mut UpdateApplyPlan,
    verified_legacy_manifest: Option<&UpdateManifest>,
) -> Result<UpdateJournal, String> {
    if let Some(expected_digest) = plan.expected_payload_manifest_sha256.as_deref() {
        validate_sha256(expected_digest, "expected payload manifest SHA256")?;
        let journal_path = update_journal_path(&plan.install_dir);
        if !atomic_path_exists(&journal_path)
            && !atomic_path_exists(&update_pending_path(&plan.install_dir))
        {
            verify_staged_payload(plan)?;
            write_update_journal(&UpdateJournal::new(plan, UpdateJournalPhase::Staged))?;
        }
        return load_matching_update_journal(plan);
    }

    // Already-shipped v1 updaters write plans without the signed payload
    // digest or journal, then launch the helper from the new payload. Upgrade
    // that one-hop handoff by fetching and verifying the signed manifest again.
    // If `latest` moved between staging and apply, fail closed and let the next
    // attempt stage the new latest release.
    if atomic_path_exists(&update_journal_path(&plan.install_dir))
        || atomic_path_exists(&update_pending_path(&plan.install_dir))
    {
        return Err("Legacy update state conflicts with an existing journal.".to_string());
    }
    if let Some(signed_manifest) = verified_legacy_manifest {
        return migrate_legacy_apply_plan(plan_path, plan, signed_manifest);
    }
    let signed_bundle = fetch_verified_update_manifest()?;
    migrate_legacy_apply_plan(plan_path, plan, &signed_bundle.manifest)
}

fn migrate_legacy_apply_plan(
    plan_path: &Path,
    plan: &mut UpdateApplyPlan,
    signed_manifest: &UpdateManifest,
) -> Result<UpdateJournal, String> {
    plan.expected_payload_manifest_sha256 =
        Some(signed_payload_digest_for_plan(plan, signed_manifest)?);
    validate_apply_plan(plan)?;
    verify_staged_payload(plan)?;

    atomic_write_json(plan_path, plan, "migrated legacy update plan")?;
    let journal = UpdateJournal::new(plan, UpdateJournalPhase::Staged);
    write_update_journal(&journal)?;
    Ok(journal)
}

fn signed_payload_digest_for_plan(
    plan: &UpdateApplyPlan,
    manifest: &UpdateManifest,
) -> Result<String, String> {
    validate_update_manifest(manifest, None)?;
    if manifest.version != plan.to_version {
        return Err(
            "The signed latest release changed before the legacy update could be applied."
                .to_string(),
        );
    }
    let artifact = manifest
        .artifacts
        .get(&plan.target)
        .ok_or_else(|| "The signed manifest does not include this update target.".to_string())?;
    validate_artifact(&plan.target, artifact)?;
    Ok(artifact.payload_manifest.sha256.clone())
}

fn verify_staged_payload(plan: &UpdateApplyPlan) -> Result<PayloadManifest, String> {
    let expected_digest = plan
        .expected_payload_manifest_sha256
        .as_deref()
        .ok_or_else(|| "Update plan is missing the signed payload digest.".to_string())?;
    let manifest_path = plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME);
    if sha256_file(&manifest_path)? != expected_digest {
        return Err(
            "Staged payload manifest no longer matches the signed update manifest.".to_string(),
        );
    }
    let manifest = read_payload_manifest(&plan.stage_dir)?;
    validate_payload_manifest(
        &plan.stage_dir,
        &manifest,
        &plan.to_version,
        &plan.target,
        true,
    )?;
    // Hash again after validating the listed files so a concurrent manifest
    // rewrite cannot silently change the file set used by replacement.
    if sha256_file(&manifest_path)? != expected_digest {
        return Err("Staged payload manifest changed during final verification.".to_string());
    }
    Ok(manifest)
}

fn validate_no_unknown_collisions(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    for relative_path in new_paths {
        validate_target_parent_chain(&plan.install_dir, relative_path)?;
        if !old_paths.contains(relative_path)
            && lexical_path_exists(&plan.install_dir.join(relative_path))?
        {
            return Err(
                "The update would overwrite an unknown file in the portable folder.".to_string(),
            );
        }
    }
    Ok(())
}

fn validate_target_parent_chain(install_dir: &Path, relative_path: &Path) -> Result<(), String> {
    let mut current = install_dir.to_path_buf();
    let Some(parent) = relative_path.parent() else {
        return Ok(());
    };
    for component in parent.components() {
        let Component::Normal(part) = component else {
            return Err("An update target path escaped the portable folder.".to_string());
        };
        current.push(part);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(
                    "The update would traverse an unknown non-directory entry in the portable folder."
                        .to_string(),
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "Failed to inspect an update target directory safely: {error}"
                ));
            }
        }
    }
    Ok(())
}

fn lexical_path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Failed to inspect an update target safely: {error}"
        )),
    }
}

fn rollback_after_apply_failure(
    plan: &UpdateApplyPlan,
    journal: &mut UpdateJournal,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
    new_manifest: &PayloadManifest,
    fault: ApplyFault,
    original_error: String,
) -> Result<(), String> {
    let expected_backup_digest = journal
        .backup_payload_manifest_sha256
        .clone()
        .ok_or_else(|| "Update journal is missing its verified backup digest.".to_string())?;
    journal.transition(UpdateJournalPhase::RollingBack, Some("apply-failed"));
    let journal_transition_error = write_update_journal(journal).err();
    if fault.remove_backup_before_rollback {
        fs::remove_dir_all(&plan.backup_dir)
            .map_err(|error| format!("Injected rollback setup could not remove backup: {error}"))?;
    }

    match restore_backup(
        plan,
        old_paths,
        new_paths,
        new_manifest,
        &expected_backup_digest,
    ) {
        Ok((_restored_manifest, _restored_manifest_digest)) => {
            journal.transition(UpdateJournalPhase::RolledBack, Some("apply-failed"));
            write_update_journal(journal)?;
            if let Some(journal_error) = journal_transition_error {
                return Err(format!(
                    "{original_error}; rollback succeeded, but its initial journal transition failed: {journal_error}"
                ));
            }
            Err(format!(
                "{original_error}; the previous version was restored."
            ))
        }
        Err(rollback_error) => {
            journal.transition(
                UpdateJournalPhase::RecoveryRequired,
                Some("rollback-failed"),
            );
            let recovery_journal_error = write_update_journal(journal).err();
            let mut combined = format!(
                "{original_error}; rollback also failed: {rollback_error}. Manual recovery is required."
            );
            if let Some(error) = journal_transition_error {
                combined.push_str(&format!(" Initial rollback journal write failed: {error}."));
            }
            if let Some(error) = recovery_journal_error {
                combined.push_str(&format!(" Recovery journal write failed: {error}."));
            }
            Err(combined)
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct RecoveryFault {
    mutate_manifest_before_final_check: bool,
    #[cfg(test)]
    pause_before_main_publication: bool,
}

fn recover_update_plan(plan_path: &Path, parent_pid: u32, relaunch: bool) -> Result<(), String> {
    recover_update_plan_with_fault(plan_path, parent_pid, relaunch, RecoveryFault::default())
}

fn recover_update_plan_with_fault(
    plan_path: &Path,
    parent_pid: u32,
    relaunch: bool,
    fault: RecoveryFault,
) -> Result<(), String> {
    let plan: UpdateApplyPlan = read_atomic_json(plan_path, "recovery update plan")?;
    validate_apply_plan(&plan)?;
    if plan_path != apply_plan_path(&plan) {
        return Err("Recovery plan was loaded from an unexpected location.".to_string());
    }
    let mut journal: UpdateJournal =
        read_atomic_json(&update_journal_path(&plan.install_dir), "recovery journal")?;
    validate_update_journal(&journal, &plan.install_dir)?;
    if journal.plan != plan
        || !matches!(
            journal.phase,
            UpdateJournalPhase::RollingBack | UpdateJournalPhase::RecoveryRequired
        )
    {
        return Err("Recovery journal is not in a recoverable destructive phase.".to_string());
    }

    let mut apply_lock = acquire_recovery_apply_lock(&plan)?;
    wait_for_parent_to_exit(parent_pid)?;

    // Re-read every trust input after the app exits and immediately before
    // restoration. Neither the startup app nor an earlier helper validation is
    // treated as sufficient across the process handoff.
    journal = read_atomic_json(&update_journal_path(&plan.install_dir), "recovery journal")?;
    validate_update_journal(&journal, &plan.install_dir)?;
    if journal.plan != plan || journal.phase != UpdateJournalPhase::RollingBack {
        return Err("Recovery journal changed during helper handoff.".to_string());
    }
    let (old_manifest, new_manifest) = match verify_recovery_materials(&journal) {
        Ok(materials) => materials,
        Err(error) => {
            journal.transition(
                UpdateJournalPhase::RecoveryRequired,
                Some("recovery-material-invalid"),
            );
            let journal_error = write_update_journal(&journal).err();
            return Err(match journal_error {
                Some(journal_error) => format!(
                    "Recovery material validation failed: {error}; recording the failure also failed: {journal_error}"
                ),
                None => format!("Recovery material validation failed: {error}"),
            });
        }
    };
    let mut old_paths = manifest_owned_paths(&old_manifest);
    old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));
    let mut new_paths = manifest_owned_paths(&new_manifest);
    new_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));

    let expected_backup_digest = journal
        .backup_payload_manifest_sha256
        .clone()
        .ok_or_else(|| "Update journal is missing its verified backup digest.".to_string())?;
    let (restored_manifest, restored_manifest_digest) = match restore_backup_with_fault(
        &plan,
        &old_paths,
        &new_paths,
        &new_manifest,
        &expected_backup_digest,
        fault,
    ) {
        Ok(restored) => restored,
        Err(error) => {
            journal.transition(
                UpdateJournalPhase::RecoveryRequired,
                Some("automatic-rollback-failed"),
            );
            let journal_error = write_update_journal(&journal).err();
            return Err(match journal_error {
                Some(journal_error) => format!(
                    "Automatic rollback failed: {error}; recording recovery failure also failed: {journal_error}"
                ),
                None => format!("Automatic rollback failed: {error}"),
            });
        }
    };
    if fault.mutate_manifest_before_final_check {
        fs::write(
            plan.install_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            b"injected post-restore mutation",
        )
        .map_err(|e| format!("Failed to inject recovery verification fault: {e}"))?;
    }
    if let Err(error) =
        verify_restored_payload(&plan, &restored_manifest, &restored_manifest_digest)
    {
        journal.transition(
            UpdateJournalPhase::RecoveryRequired,
            Some("restored-payload-verification-failed"),
        );
        let journal_error = write_update_journal(&journal).err();
        return Err(match journal_error {
            Some(journal_error) => format!(
                "Restored payload verification failed: {error}; recording recovery failure also failed: {journal_error}"
            ),
            None => format!("Restored payload verification failed: {error}"),
        });
    }

    journal.transition(
        UpdateJournalPhase::RolledBack,
        Some("automatic-recovery-complete"),
    );
    write_update_journal(&journal)?;
    let release_error = apply_lock.release().err();
    let relaunch_error = if relaunch {
        launch_updated_app(&plan).err()
    } else {
        None
    };
    let errors = [release_error, relaunch_error]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "The previous version was restored, but recovery relaunch had errors: {}",
            errors.join("; ")
        ))
    }
}

fn validate_apply_plan(plan: &UpdateApplyPlan) -> Result<(), String> {
    if plan.schema != APPLY_PLAN_SCHEMA {
        return Err("Unsupported update apply plan schema.".to_string());
    }
    if !plan.install_dir.is_absolute()
        || !plan.stage_dir.is_absolute()
        || !plan.backup_dir.is_absolute()
    {
        return Err("Update apply plan paths must be absolute.".to_string());
    }
    if has_parent_component(&plan.install_dir)
        || has_parent_component(&plan.stage_dir)
        || has_parent_component(&plan.backup_dir)
    {
        return Err(
            "Update apply plan paths must not contain parent directory segments.".to_string(),
        );
    }
    if plan.stage_dir == plan.install_dir || plan.backup_dir == plan.install_dir {
        return Err("Update apply plan paths are not scoped safely.".to_string());
    }
    if plan.update_id.is_empty()
        || !plan
            .update_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("Update apply plan identifier is invalid.".to_string());
    }
    let from_version = parse_semver(&plan.from_version)?;
    let to_version = parse_semver(&plan.to_version)?;
    if to_version <= from_version {
        return Err("Update apply plan version transition is invalid.".to_string());
    }
    let expected_stage_root = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("staged")
        .join(&plan.update_id);
    if !plan.stage_dir.starts_with(&expected_stage_root) {
        return Err("Update stage directory is outside the update state folder.".to_string());
    }
    let expected_backup_dir = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("backups")
        .join(&plan.update_id);
    if plan.backup_dir != expected_backup_dir {
        return Err("Update backup directory is outside the update state folder.".to_string());
    }
    if plan.target != current_target()? {
        return Err("Update apply plan target does not match this platform.".to_string());
    }
    if plan.executable_name != main_executable_name()? {
        return Err("Update apply plan executable name is unexpected.".to_string());
    }
    if plan.helper_name != helper_executable_name()? {
        return Err("Update apply plan helper name is unexpected.".to_string());
    }
    if let Some(digest) = plan.expected_payload_manifest_sha256.as_deref() {
        validate_sha256(digest, "expected payload manifest SHA256")?;
    }
    Ok(())
}

fn backup_existing_owned_files(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
) -> Result<String, String> {
    let (expected_manifest, expected_manifest_digest) =
        read_payload_manifest_with_digest(&plan.install_dir)?;
    validate_payload_manifest(
        &plan.install_dir,
        &expected_manifest,
        &plan.from_version,
        &plan.target,
        false,
    )?;
    let mut expected_paths = manifest_owned_paths(&expected_manifest);
    expected_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));
    if &expected_paths != old_paths {
        return Err("Installed payload paths changed before backup.".to_string());
    }

    remove_dir_if_exists(&plan.backup_dir, "stale update backup")?;
    fs::create_dir_all(&plan.backup_dir)
        .map_err(|e| format!("Failed to create update backup: {e}"))?;
    for relative_path in old_paths {
        let source = plan.install_dir.join(relative_path);
        let source_metadata = fs::symlink_metadata(&source)
            .map_err(|e| format!("An installed owned file disappeared before backup: {e}"))?;
        if source_metadata.file_type().is_symlink() || !source_metadata.is_file() {
            return Err("An installed owned path is no longer a regular file.".to_string());
        }
        let backup_path = plan.backup_dir.join(relative_path);
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update backup directory: {e}"))?;
        }
        retry_copy_file(&source, &backup_path)?;
        if let Some(parent) = backup_path.parent() {
            sync_directory_chain(parent, &plan.install_dir.join(UPDATE_STATE_DIR))?;
        }
    }
    let (backed_up_manifest, backed_up_manifest_digest) =
        read_payload_manifest_with_digest(&plan.backup_dir)?;
    if backed_up_manifest != expected_manifest
        || backed_up_manifest_digest != expected_manifest_digest
    {
        return Err(
            "The completed update backup does not match the installed payload.".to_string(),
        );
    }
    validate_payload_manifest(
        &plan.backup_dir,
        &backed_up_manifest,
        &plan.from_version,
        &plan.target,
        true,
    )?;
    let (installed_after, installed_digest_after) =
        read_payload_manifest_with_digest(&plan.install_dir)?;
    if installed_after != expected_manifest || installed_digest_after != expected_manifest_digest {
        return Err(
            "The installed payload changed while its backup was being created.".to_string(),
        );
    }
    validate_payload_manifest(
        &plan.install_dir,
        &installed_after,
        &plan.from_version,
        &plan.target,
        false,
    )?;
    Ok(expected_manifest_digest)
}

type ExpectedPayloadFile = (Option<u32>, String, u64);
type ExpectedNewPayloadFiles = HashMap<PathBuf, ExpectedPayloadFile>;

fn expected_new_payload_files(
    plan: &UpdateApplyPlan,
    new_manifest: &PayloadManifest,
) -> Result<ExpectedNewPayloadFiles, String> {
    let mut entries = new_manifest
        .files
        .iter()
        .map(|entry| {
            let path = payload_path_buf(&entry.path)?;
            Ok((path, (entry.mode, entry.sha256.clone(), entry.size_bytes)))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let manifest_path = plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME);
    let manifest_size = fs::metadata(&manifest_path)
        .map_err(|e| format!("Failed to read staged payload manifest metadata: {e}"))?
        .len();
    entries.insert(
        PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME),
        (
            if plan.target == "linux-x64" {
                Some(0o644)
            } else {
                None
            },
            plan.expected_payload_manifest_sha256
                .clone()
                .ok_or_else(|| "Update plan is missing the signed payload digest.".to_string())?,
            manifest_size,
        ),
    );
    Ok(entries)
}

fn verified_payload_file_matches(
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
    expected_mode: Option<u32>,
    target: &str,
) -> Result<bool, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Failed to inspect a possible updater-owned file safely: {error}"
            ));
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != expected_size
        || sha256_file(path)? != expected_hash
    {
        return Ok(false);
    }
    Ok(validate_mode(path, expected_mode, target).is_ok())
}

fn replace_owned_files(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
    new_manifest: &PayloadManifest,
    fault: ApplyFault,
) -> Result<(), String> {
    let mut replacements = 0usize;
    let entries = expected_new_payload_files(plan, new_manifest)?;

    let main_path = PathBuf::from(&plan.executable_name);
    let helper_path = PathBuf::from(&plan.helper_name);
    let mut publication_order = new_paths
        .iter()
        .filter(|path| **path != helper_path && **path != main_path)
        .cloned()
        .collect::<Vec<_>>();
    if new_paths.contains(&helper_path) {
        publication_order.push(helper_path);
    }
    // Publish the main executable last. A crash before this final atomic step
    // leaves the old app available to start and drive automatic recovery.
    if new_paths.contains(&main_path) {
        publication_order.push(main_path);
    }

    let publish_root = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("publish")
        .join(&plan.update_id);
    remove_dir_if_exists(&publish_root, "stale update publication state")?;

    #[cfg(test)]
    if let Some(collision) = fault.new_target_collision {
        let relative_path = new_paths
            .difference(old_paths)
            .next()
            .ok_or_else(|| "The collision test requires a new-only update path.".to_string())?;
        let target = plan.install_dir.join(relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create collision test directory: {e}"))?;
        }
        match collision {
            TestNewTargetCollision::RegularFile => {
                fs::write(&target, b"preserve-unknown-file")
                    .map_err(|e| format!("Failed to create collision test file: {e}"))?;
            }
            #[cfg(unix)]
            TestNewTargetCollision::DanglingSymlink => {
                std::os::unix::fs::symlink("missing-user-target", &target)
                    .map_err(|e| format!("Failed to create collision test symlink: {e}"))?;
            }
        }
    }

    for relative_path in &publication_order {
        let source = plan.stage_dir.join(relative_path);
        let target = plan.install_dir.join(relative_path);
        let (mode, expected_hash, expected_size) = entries
            .get(relative_path)
            .ok_or_else(|| "Staged update path is missing signed metadata.".to_string())?;
        let source_metadata = fs::symlink_metadata(&source)
            .map_err(|e| format!("Failed to read staged update file metadata: {e}"))?;
        if source_metadata.file_type().is_symlink()
            || !source_metadata.is_file()
            || source_metadata.len() != *expected_size
            || sha256_file(&source)? != expected_hash.as_str()
        {
            return Err("A staged update file changed before replacement.".to_string());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update target directory: {e}"))?;
        }
        validate_target_parent_chain(&plan.install_dir, relative_path)?;
        publish_verified_update_file(
            plan,
            &publish_root,
            relative_path,
            &source,
            &target,
            expected_hash,
            *expected_size,
            *mode,
            old_paths.contains(relative_path),
            fault,
        )?;
        replacements += 1;
        if fault.fail_after_replacements == Some(replacements) {
            return Err("Injected replacement interruption.".to_string());
        }
    }

    // Deletions happen only after every replacement is durably published. If
    // the helper dies earlier, obsolete files are merely left in place and the
    // old executable remains a valid recovery entry point.
    for relative_path in old_paths.difference(new_paths) {
        remove_file_with_retries(&plan.install_dir.join(relative_path))?;
        replacements += 1;
        if fault.fail_after_replacements == Some(replacements) {
            return Err("Injected replacement interruption.".to_string());
        }
    }

    remove_dir_if_exists(&publish_root, "completed update publication state")?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_verified_update_file(
    plan: &UpdateApplyPlan,
    publish_root: &Path,
    relative_path: &Path,
    source: &Path,
    target: &Path,
    expected_hash: &str,
    expected_size: u64,
    mode: Option<u32>,
    replace_existing: bool,
    _fault: ApplyFault,
) -> Result<(), String> {
    let incoming = publish_root.join(relative_path);
    if let Some(parent) = incoming.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create update publication directory: {e}"))?;
    }
    remove_file_with_retries(&incoming)?;
    retry_copy_file(source, &incoming)?;
    set_mode_if_needed(&incoming, mode)?;
    #[cfg(unix)]
    File::open(&incoming)
        .and_then(|file| file.sync_all())
        .map_err(|e| format!("Failed to persist verified update publication file: {e}"))?;
    if let Some(parent) = incoming.parent() {
        sync_parent_directory(parent)
            .map_err(|e| format!("Failed to sync update publication directory: {e}"))?;
    }

    let incoming_metadata = fs::symlink_metadata(&incoming)
        .map_err(|e| format!("Failed to inspect update publication file: {e}"))?;
    if incoming_metadata.file_type().is_symlink()
        || !incoming_metadata.is_file()
        || incoming_metadata.len() != expected_size
        || sha256_file(&incoming)? != expected_hash
    {
        return Err("An update publication file failed pre-publication verification.".to_string());
    }
    validate_mode(&incoming, mode, &plan.target)?;

    #[cfg(test)]
    if _fault.pause_before_main_publication
        && relative_path == Path::new(plan.executable_name.as_str())
    {
        pause_before_main_publication_for_test()?;
    }
    #[cfg(debug_assertions)]
    if relative_path == Path::new(plan.executable_name.as_str()) {
        pause_at_digest_bound_debug_gate(plan, "applyMain")?;
    }

    if replace_existing {
        retry_atomic_replace_file(&incoming, target)?;
    } else {
        retry_atomic_publish_new_file(&incoming, target)?;
    }
    let target_metadata = fs::symlink_metadata(target)
        .map_err(|e| format!("Failed to inspect published update file: {e}"))?;
    if target_metadata.file_type().is_symlink()
        || !target_metadata.is_file()
        || target_metadata.len() != expected_size
        || sha256_file(target)? != expected_hash
    {
        return Err("A published update file failed final verification.".to_string());
    }
    validate_mode(target, mode, &plan.target)?;
    if let Some(parent) = target.parent() {
        sync_directory_chain(parent, &plan.install_dir)?;
    }
    #[cfg(debug_assertions)]
    if relative_path == Path::new(plan.executable_name.as_str()) {
        pause_at_digest_bound_debug_gate(plan, "applyAfterMain")?;
    }
    Ok(())
}

#[cfg(test)]
fn pause_before_main_publication_for_test() -> Result<(), String> {
    let ready_path = std::env::var_os("VFL_TEST_PREPUBLISH_READY")
        .map(PathBuf::from)
        .ok_or_else(|| "Pre-publication test marker was not configured.".to_string())?;
    let mut ready = File::create(ready_path)
        .map_err(|e| format!("Failed to create pre-publication test marker: {e}"))?;
    ready
        .write_all(b"ready")
        .and_then(|()| ready.sync_all())
        .map_err(|e| format!("Failed to persist pre-publication test marker: {e}"))?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(debug_assertions)]
fn pause_at_digest_bound_debug_gate(plan: &UpdateApplyPlan, phase: &str) -> Result<(), String> {
    let requested_phase = std::env::var("VFL_UPDATER_TEST_PAUSE_PHASE").unwrap_or_default();
    if requested_phase.is_empty() {
        return Ok(());
    }
    if std::env::var("VFL_UPDATER_TEST_MODE").ok().as_deref() != Some("1") {
        return Err("The debug updater pause gate requires explicit test mode.".to_string());
    }
    if requested_phase != phase {
        return Ok(());
    }
    let expected_id = std::env::var("VFL_UPDATER_TEST_UPDATE_ID")
        .map_err(|_| "The debug updater pause gate is missing its update id.".to_string())?;
    let expected_digest = std::env::var("VFL_UPDATER_TEST_PAYLOAD_DIGEST")
        .map_err(|_| "The debug updater pause gate is missing its payload digest.".to_string())?;
    validate_sha256(&expected_digest, "debug updater pause payload SHA256")?;
    if expected_id != plan.update_id
        || plan.expected_payload_manifest_sha256.as_deref() != Some(expected_digest.as_str())
    {
        return Err("The debug updater pause gate does not match this signed plan.".to_string());
    }
    let marker = std::env::var_os("VFL_UPDATER_TEST_PAUSE_MARKER")
        .map(PathBuf::from)
        .ok_or_else(|| "The debug updater pause marker is missing.".to_string())?;
    if !marker.is_absolute() {
        return Err("The debug updater pause marker must be absolute.".to_string());
    }
    let record = serde_json::json!({
        "phase": phase,
        "updateId": plan.update_id,
        "payloadManifestSha256": expected_digest,
        "pid": std::process::id(),
    });
    let mut file = File::create(marker)
        .map_err(|e| format!("Failed to create the debug updater pause marker: {e}"))?;
    file.write_all(&serde_json::to_vec(&record).map_err(|e| e.to_string())?)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("Failed to persist the debug updater pause marker: {e}"))?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn retry_atomic_replace_file(source: &Path, target: &Path) -> Result<(), String> {
    retry_io(
        || atomic_replace_file(source, target),
        || format!("Failed to atomically publish {}", target.display()),
    )
}

fn retry_atomic_publish_new_file(source: &Path, target: &Path) -> Result<(), String> {
    let mut last_error = None;
    for attempt in 0..COPY_RETRIES {
        match atomic_publish_new_file(source, target) {
            Ok(()) => return Ok(()),
            Err(error) if publish_target_already_exists(&error) => {
                return Err(
                    "The update would overwrite an unknown file in the portable folder."
                        .to_string(),
                );
            }
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < COPY_RETRIES {
                    std::thread::sleep(Duration::from_millis(COPY_RETRY_DELAY_MS));
                }
            }
        }
    }
    Err(format!(
        "Failed to atomically publish a new update file: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown filesystem error".to_string())
    ))
}

fn publish_target_already_exists(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(80 | 183)) {
        return true;
    }
    false
}

#[cfg(windows)]
fn atomic_publish_new_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_publish_new_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::hard_link(source, target)
}

#[cfg(windows)]
fn atomic_replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    let result = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

fn restore_backup(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
    new_manifest: &PayloadManifest,
    expected_backup_digest: &str,
) -> Result<(PayloadManifest, String), String> {
    restore_backup_with_fault(
        plan,
        old_paths,
        new_paths,
        new_manifest,
        expected_backup_digest,
        RecoveryFault::default(),
    )
}

fn restore_backup_with_fault(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
    new_manifest: &PayloadManifest,
    expected_backup_digest: &str,
    fault: RecoveryFault,
) -> Result<(PayloadManifest, String), String> {
    let (old_manifest, backup_manifest_digest) =
        read_payload_manifest_with_digest(&plan.backup_dir)?;
    if backup_manifest_digest != expected_backup_digest {
        return Err("Rollback backup manifest does not match the recorded digest.".to_string());
    }
    validate_payload_manifest(
        &plan.backup_dir,
        &old_manifest,
        &plan.from_version,
        &plan.target,
        true,
    )?;
    let mut verified_old_paths = manifest_owned_paths(&old_manifest);
    verified_old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));
    if &verified_old_paths != old_paths {
        return Err("Rollback backup paths do not match the recorded old payload.".to_string());
    }
    let mut expected_files = old_manifest
        .files
        .iter()
        .map(|entry| {
            Ok((
                payload_path_buf(&entry.path)?,
                (entry.sha256.clone(), entry.size_bytes),
            ))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;
    let backup_manifest_size = fs::metadata(plan.backup_dir.join(PAYLOAD_MANIFEST_FILE_NAME))
        .map_err(|e| format!("Failed to read rollback manifest metadata: {e}"))?
        .len();
    expected_files.insert(
        PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME),
        (backup_manifest_digest.clone(), backup_manifest_size),
    );

    let main_path = PathBuf::from(&plan.executable_name);
    let restore_one = |relative_path: &Path| -> Result<(), String> {
        let backup_path = plan.backup_dir.join(relative_path);
        let target = plan.install_dir.join(relative_path);
        let Some((expected_hash, expected_size)) = expected_files.get(relative_path) else {
            return Err("Rollback backup is missing verified file metadata.".to_string());
        };
        if !backup_path.exists() {
            return Err(format!(
                "Rollback backup is missing for {}",
                relative_path.display()
            ));
        }
        if let Some(parent) = target.parent()
            && let Err(error) = fs::create_dir_all(parent)
        {
            return Err(format!("Failed to create rollback directory: {error}"));
        }
        atomic_restore_file(
            plan,
            relative_path,
            &backup_path,
            &target,
            expected_hash,
            *expected_size,
            fault,
        )?;
        if let Some(parent) = target.parent() {
            sync_directory_chain(parent, &plan.install_dir)
                .map_err(|error| format!("Failed to sync rollback directory: {error}"))?;
        }
        Ok(())
    };

    let mut failures = Vec::new();
    for relative_path in old_paths.iter().filter(|path| **path != main_path) {
        if let Err(error) = restore_one(relative_path) {
            failures.push(error);
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    // Switch executables only after every non-main old file is durably in
    // place. A non-main restore failure must leave the current main and all
    // of its new-only runtime dependencies untouched.
    if old_paths.contains(&main_path) {
        restore_one(&main_path)?;
    }
    // Retire files introduced only by the interrupted update after the old
    // payload, including its main executable, is fully restored. Until then
    // the currently installed app keeps every dependency it may need to start
    // and drive another recovery attempt.
    let expected_new_files = expected_new_payload_files(plan, new_manifest)?;
    for relative_path in new_paths.difference(old_paths) {
        let Some((mode, expected_hash, expected_size)) = expected_new_files.get(relative_path)
        else {
            failures.push("Signed update metadata is missing a new-only path.".to_string());
            continue;
        };
        let target = plan.install_dir.join(relative_path);
        match verified_payload_file_matches(
            &target,
            expected_hash,
            *expected_size,
            *mode,
            &plan.target,
        ) {
            Ok(true) => {
                if let Err(error) = remove_file_with_retries(&target) {
                    failures.push(error);
                }
            }
            Ok(false) => {
                // A missing path needs no cleanup. A mismatched regular file,
                // symlink, or other entry is unknown user state and must be
                // preserved rather than inferred to belong to the updater.
            }
            Err(error) => failures.push(error),
        }
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }

    let installed_manifest_path = plan.install_dir.join(PAYLOAD_MANIFEST_FILE_NAME);
    if sha256_file(&installed_manifest_path)? != backup_manifest_digest {
        return Err(
            "Restored payload manifest bytes do not match the verified backup.".to_string(),
        );
    }
    let installed_manifest = read_payload_manifest(&plan.install_dir)?;
    if installed_manifest != old_manifest {
        return Err("Restored payload manifest content changed during recovery.".to_string());
    }
    validate_payload_manifest(
        &plan.install_dir,
        &installed_manifest,
        &plan.from_version,
        &plan.target,
        false,
    )?;
    Ok((old_manifest, backup_manifest_digest))
}

fn verify_restored_payload(
    plan: &UpdateApplyPlan,
    expected_manifest: &PayloadManifest,
    expected_manifest_digest: &str,
) -> Result<(), String> {
    let (installed_manifest, installed_digest) =
        read_payload_manifest_with_digest(&plan.install_dir)?;
    if installed_digest != expected_manifest_digest || &installed_manifest != expected_manifest {
        return Err("Installed payload manifest changed after recovery publication.".to_string());
    }
    validate_payload_manifest(
        &plan.install_dir,
        &installed_manifest,
        &plan.from_version,
        &plan.target,
        false,
    )?;
    if sha256_file(&plan.install_dir.join(PAYLOAD_MANIFEST_FILE_NAME))? != expected_manifest_digest
    {
        return Err("Installed payload manifest changed during final recovery check.".to_string());
    }
    Ok(())
}

fn atomic_restore_file(
    plan: &UpdateApplyPlan,
    relative_path: &Path,
    source: &Path,
    target: &Path,
    expected_hash: &str,
    expected_size: u64,
    _fault: RecoveryFault,
) -> Result<(), String> {
    let parent = target
        .parent()
        .ok_or_else(|| "Recovery target has no parent directory.".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create recovery target directory: {e}"))?;
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|e| format!("Failed to read verified rollback source metadata: {e}"))?;
    if source_metadata.file_type().is_symlink()
        || !source_metadata.is_file()
        || source_metadata.len() != expected_size
        || sha256_file(source)? != expected_hash
    {
        return Err("A verified rollback source changed before restoration.".to_string());
    }

    let swap_root = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("recovery-swap")
        .join(&plan.update_id);
    let incoming_path = swap_root.join("incoming").join(relative_path);
    if let Some(parent) = incoming_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create recovery incoming directory: {e}"))?;
    }
    remove_file_with_retries(&incoming_path)?;
    retry_copy_file(source, &incoming_path)?;
    let incoming_metadata = fs::symlink_metadata(&incoming_path)
        .map_err(|e| format!("Failed to inspect copied rollback source: {e}"))?;
    if incoming_metadata.file_type().is_symlink()
        || !incoming_metadata.is_file()
        || incoming_metadata.len() != expected_size
        || sha256_file(&incoming_path)? != expected_hash
    {
        return Err("Copied rollback source failed verification before publication.".to_string());
    }

    #[cfg(test)]
    if _fault.pause_before_main_publication
        && relative_path == Path::new(plan.executable_name.as_str())
    {
        pause_before_recovery_main_publication_for_test()?;
    }
    #[cfg(debug_assertions)]
    if relative_path == Path::new(plan.executable_name.as_str()) {
        pause_at_digest_bound_debug_gate(plan, "recoveryMain")?;
    }

    // The verified backup remains intact. Publishing the fully persisted
    // incoming copy over the current target is one same-volume atomic step,
    // so a hard kill leaves either the pre-recovery or restored executable at
    // the canonical path, never a gap between two renames.
    retry_atomic_replace_file(&incoming_path, target)?;
    let target_metadata = fs::symlink_metadata(target)
        .map_err(|e| format!("Failed to inspect published rollback file: {e}"))?;
    if target_metadata.file_type().is_symlink()
        || !target_metadata.is_file()
        || target_metadata.len() != expected_size
        || sha256_file(target)? != expected_hash
    {
        return Err("Published rollback file failed final verification.".to_string());
    }
    sync_parent_directory(parent)
        .map_err(|e| format!("Failed to sync restored file directory: {e}"))?;
    Ok(())
}

#[cfg(test)]
fn pause_before_recovery_main_publication_for_test() -> Result<(), String> {
    let ready_path = std::env::var_os("VFL_TEST_RECOVERY_PREPUBLISH_READY")
        .map(PathBuf::from)
        .ok_or_else(|| "Recovery pre-publication test marker was not configured.".to_string())?;
    let mut ready = File::create(ready_path)
        .map_err(|e| format!("Failed to create recovery publication test marker: {e}"))?;
    ready
        .write_all(b"ready")
        .and_then(|()| ready.sync_all())
        .map_err(|e| format!("Failed to persist recovery publication test marker: {e}"))?;
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn launch_updated_app(plan: &UpdateApplyPlan) -> Result<(), String> {
    #[cfg(all(test, target_os = "linux"))]
    if let Some(marker_path) = std::env::var_os("VFL_TEST_RELAUNCH_MARKER") {
        Command::new(
            std::env::current_exe()
                .map_err(|e| format!("Failed to locate recovery relaunch test binary: {e}"))?,
        )
        .args([
            "--exact",
            "updater::tests::linux_relaunch_marker_child",
            "--nocapture",
        ])
        .env("VFL_RELAUNCH_MARKER_CHILD", "1")
        .env("VFL_TEST_RELAUNCH_MARKER", marker_path)
        .spawn()
        .map_err(|e| format!("Failed to launch recovery marker process: {e}"))?;
        return Ok(());
    }

    let executable = plan.install_dir.join(&plan.executable_name);

    #[cfg(windows)]
    if plan.relaunch_via_shell {
        // This helper inherited the elevated token from the staging instance.
        // explorer.exe always runs with the user's normal token, so launching
        // through it drops the elevation for the updated app (otherwise the
        // relaunched app could not receive drag and drop from Explorer).
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("explorer.exe");
        command.arg(&executable).creation_flags(0x08000000);
        command
            .spawn()
            .map_err(|e| format!("Failed to relaunch Video For Lazies: {e}"))?;
        return Ok(());
    }

    let mut command = Command::new(executable);
    command.current_dir(&plan.install_dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .spawn()
        .map_err(|e| format!("Failed to relaunch Video For Lazies: {e}"))?;
    Ok(())
}

fn wait_for_parent_to_exit(parent_pid: u32) -> Result<(), String> {
    wait_for_parent_to_exit_with_timeout(parent_pid, PARENT_EXIT_TIMEOUT, PARENT_EXIT_POLL_INTERVAL)
}

#[cfg(not(target_os = "windows"))]
fn wait_for_parent_to_exit_with_timeout(
    parent_pid: u32,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<(), String> {
    if parent_pid == 0 {
        return Ok(());
    }
    let started = std::time::Instant::now();
    loop {
        if !process_is_running(parent_pid)? {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err("The running app did not exit before the update timeout.".to_string());
        }
        std::thread::sleep(poll_interval);
    }
}

#[cfg(target_os = "windows")]
fn wait_for_parent_to_exit_with_timeout(
    parent_pid: u32,
    timeout: Duration,
    _poll_interval: Duration,
) -> Result<(), String> {
    if parent_pid == 0 {
        return Ok(());
    }
    if wait_for_windows_process_exit(parent_pid, timeout)? {
        Ok(())
    } else {
        Err("The running app did not exit before the update timeout.".to_string())
    }
}

#[cfg(target_os = "linux")]
fn process_is_running(pid: u32) -> Result<bool, String> {
    let stat_path = Path::new("/proc").join(pid.to_string()).join("stat");
    let stat = match fs::read_to_string(stat_path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "Failed to inspect the parent process state: {error}"
            ));
        }
    };
    // `/proc/<pid>/stat` begins with `pid (comm) state`. The command name may
    // contain spaces and parentheses, so split only after its final `)`.
    let command_end = stat
        .rfind(')')
        .ok_or_else(|| "The parent process state is malformed.".to_string())?;
    let state = stat[command_end + 1..]
        .split_whitespace()
        .next()
        .ok_or_else(|| "The parent process state is missing.".to_string())?;
    Ok(!matches!(state, "Z" | "X"))
}

#[cfg(target_os = "windows")]
fn process_is_running(pid: u32) -> Result<bool, String> {
    if pid == 0 {
        return Ok(false);
    }
    wait_for_windows_process_exit(pid, Duration::ZERO).map(|exited| !exited)
}

#[cfg(target_os = "windows")]
fn wait_for_windows_process_exit(pid: u32, timeout: Duration) -> Result<bool, String> {
    use std::ffi::c_void;

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_OBJECT_0: u32 = 0x0000_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;
    const WAIT_FAILED: u32 = 0xffff_ffff;
    const ERROR_INVALID_PARAMETER: i32 = 87;

    // SAFETY: OpenProcess is called with a PID value and no inherited handle.
    let handle = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER) {
            Ok(true)
        } else {
            Err(format!(
                "Failed to open the parent process for bounded waiting: {error}"
            ))
        };
    }
    let timeout_ms = timeout.as_millis().min((u32::MAX - 1) as u128) as u32;
    // SAFETY: handle was returned by OpenProcess and remains open here.
    let wait_result = unsafe { WaitForSingleObject(handle, timeout_ms) };
    let wait_error = (wait_result == WAIT_FAILED).then(io::Error::last_os_error);
    // SAFETY: the same owned process handle is closed exactly once.
    let close_result = unsafe { CloseHandle(handle) };
    if close_result == 0 {
        return Err(format!(
            "Failed to close the parent process wait handle: {}",
            io::Error::last_os_error()
        ));
    }
    match wait_result {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        WAIT_FAILED => Err(format!(
            "Failed while waiting for the parent process: {}",
            wait_error.expect("WAIT_FAILED captures its OS error")
        )),
        _ => Err("The parent process wait returned an unexpected result.".to_string()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn process_is_running(_pid: u32) -> Result<bool, String> {
    Err("This platform cannot verify the parent process state.".to_string())
}

fn retry_copy_file(source: &Path, target: &Path) -> Result<(), String> {
    retry_io(
        || {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            match fs::remove_file(target) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                #[cfg(windows)]
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    let mut permissions = fs::metadata(target)?.permissions();
                    clear_windows_readonly(&mut permissions);
                    fs::set_permissions(target, permissions)?;
                    fs::remove_file(target)?;
                }
                Err(error) => return Err(error),
            }
            let mut source_file = File::open(source)?;
            let source_permissions = source_file.metadata()?.permissions();
            let mut target_file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(target)?;
            io::copy(&mut source_file, &mut target_file)?;
            target_file.flush()?;
            target_file.sync_all()?;
            drop(target_file);
            fs::set_permissions(target, source_permissions)?;
            if let Some(parent) = target.parent() {
                sync_parent_directory(parent)?;
            }
            Ok(())
        },
        || {
            format!(
                "Failed to copy {} to {}",
                source.display(),
                target.display()
            )
        },
    )
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn clear_windows_readonly(permissions: &mut fs::Permissions) {
    // This branch is Windows-only, where the generic API clears the DOS
    // read-only attribute rather than widening Unix write permissions.
    permissions.set_readonly(false);
}

fn remove_file_with_retries(path: &Path) -> Result<(), String> {
    retry_io(
        || match fs::remove_file(path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    sync_parent_directory(parent)?;
                }
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        },
        || format!("Failed to remove {}", path.display()),
    )
}

fn retry_io<F, L>(mut operation: F, label: L) -> Result<(), String>
where
    F: FnMut() -> io::Result<()>,
    L: Fn() -> String,
{
    let mut last_error = None;
    for _ in 0..COPY_RETRIES {
        match operation() {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                std::thread::sleep(Duration::from_millis(COPY_RETRY_DELAY_MS));
            }
        }
    }
    Err(format!(
        "{}: {}",
        label(),
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn read_payload_manifest(portable_dir: &Path) -> Result<PayloadManifest, String> {
    let raw = fs::read(portable_dir.join(PAYLOAD_MANIFEST_FILE_NAME))
        .map_err(|e| format!("Failed to read payload manifest: {e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("Failed to parse payload manifest: {e}"))
}

fn read_payload_manifest_with_digest(
    portable_dir: &Path,
) -> Result<(PayloadManifest, String), String> {
    let raw = fs::read(portable_dir.join(PAYLOAD_MANIFEST_FILE_NAME))
        .map_err(|e| format!("Failed to read payload manifest: {e}"))?;
    let digest = sha256_bytes(&raw);
    let manifest = serde_json::from_slice(&raw)
        .map_err(|e| format!("Failed to parse payload manifest: {e}"))?;
    Ok((manifest, digest))
}

fn validate_payload_manifest(
    portable_dir: &Path,
    manifest: &PayloadManifest,
    version: &str,
    target: &str,
    exact_file_list: bool,
) -> Result<(), String> {
    if manifest.schema != PAYLOAD_MANIFEST_SCHEMA {
        return Err("Unsupported payload manifest schema.".to_string());
    }
    if manifest.app_id != APP_ID {
        return Err("Payload manifest is for a different app.".to_string());
    }
    if manifest.version != version {
        return Err("Payload manifest version does not match the update manifest.".to_string());
    }
    if manifest.target != target {
        return Err("Payload manifest target does not match this platform.".to_string());
    }
    if manifest.root_dir != PORTABLE_ROOT_DIR_NAME {
        return Err("Payload manifest portable root is unexpected.".to_string());
    }
    if manifest.files.is_empty() {
        return Err("Payload manifest does not list any files.".to_string());
    }

    let mut seen = HashSet::new();
    for entry in &manifest.files {
        if entry.kind != "file" {
            return Err("Payload manifest contains a non-file entry.".to_string());
        }
        validate_sha256(&entry.sha256, "payload file SHA256")?;
        let relative_path = payload_path_buf(&entry.path)?;
        if !seen.insert(relative_path.clone()) {
            return Err(format!(
                "Payload manifest has a duplicate path: {}",
                entry.path
            ));
        }

        let absolute_path = portable_dir.join(&relative_path);
        let meta = fs::symlink_metadata(&absolute_path)
            .map_err(|e| format!("Payload file is missing: {} ({e})", entry.path))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            return Err(format!(
                "Payload path is not a regular file: {}",
                entry.path
            ));
        }
        if meta.len() != entry.size_bytes {
            return Err(format!("Payload file size mismatch: {}", entry.path));
        }
        if sha256_file(&absolute_path)? != entry.sha256 {
            return Err(format!("Payload file hash mismatch: {}", entry.path));
        }
        validate_mode(&absolute_path, entry.mode, target)?;
    }

    for required_file in required_payload_files(target)? {
        if !seen.contains(&PathBuf::from(required_file)) {
            return Err(format!(
                "Payload manifest is missing required file: {required_file}"
            ));
        }
    }

    if exact_file_list {
        let actual_files = walk_regular_files(portable_dir)?;
        let expected_files = seen
            .iter()
            .cloned()
            .chain(std::iter::once(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME)))
            .collect::<BTreeSet<_>>();
        if actual_files != expected_files {
            return Err(
                "Payload manifest does not exactly match the extracted update.".to_string(),
            );
        }
    }

    Ok(())
}

fn manifest_owned_paths(manifest: &PayloadManifest) -> BTreeSet<PathBuf> {
    manifest
        .files
        .iter()
        .filter_map(|entry| payload_path_buf(&entry.path).ok())
        .collect()
}

fn required_payload_files(target: &str) -> Result<Vec<&'static str>, String> {
    let mut required = vec![
        main_executable_name()?,
        helper_executable_name()?,
        "README.md",
        "LICENSE.txt",
        "THIRD_PARTY_NOTICES.md",
        "SOURCE.md",
        "FFMPEG_BUNDLING.md",
        "ffmpeg-sidecar/LICENSE.txt",
        "ffmpeg-sidecar/FFMPEG_BUNDLE_NOTICES.txt",
    ];
    if target == "linux-x64" {
        required.push("Video_For_Lazies.png");
        required.push("Video_For_Lazies.desktop");
    }
    Ok(required)
}

fn payload_path_buf(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("Payload path must not be empty.".to_string());
    }
    if raw.contains('\0')
        || raw.contains('\\')
        || raw.starts_with('/')
        || raw.starts_with('~')
        || raw.contains(':')
    {
        return Err(format!("Unsafe payload path: {raw}"));
    }
    let path = Path::new(raw);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            _ => return Err(format!("Unsafe payload path: {raw}")),
        }
    }
    if clean.as_os_str().is_empty() {
        return Err("Payload path must not be empty.".to_string());
    }
    if clean
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .is_some_and(|part| part.eq_ignore_ascii_case(UPDATE_STATE_DIR))
    {
        return Err("Payload paths must not enter the updater state directory.".to_string());
    }
    Ok(clean)
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn walk_regular_files(root: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    walk_regular_files_inner(root, root, &mut files)?;
    Ok(files)
}

fn walk_regular_files_inner(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(current).map_err(|e| format!("Failed to read directory: {e}"))? {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {e}"))?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("Failed to read file metadata: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "Update payload must not contain symlinks: {}",
                path.display()
            ));
        }
        if meta.is_dir() {
            walk_regular_files_inner(root, &path, files)?;
        } else if meta.is_file() {
            files.insert(
                path.strip_prefix(root)
                    .map_err(|e| format!("Failed to relativize payload path: {e}"))?
                    .to_path_buf(),
            );
        } else {
            return Err(format!(
                "Update payload must contain regular files only: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn validate_mode(path: &Path, expected_mode: Option<u32>, target: &str) -> Result<(), String> {
    if target != "linux-x64" {
        if expected_mode.is_some() {
            return Err("Non-Linux payload entries must not include Unix modes.".to_string());
        }
        return Ok(());
    }
    let expected_mode = expected_mode
        .ok_or_else(|| "Linux payload entries must include Unix modes.".to_string())?;
    if expected_mode > 0o777 {
        return Err("Linux payload mode is outside the supported range.".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let actual = fs::metadata(path)
            .map_err(|e| format!("Failed to read file permissions: {e}"))?
            .permissions()
            .mode()
            & 0o777;
        if actual != expected_mode {
            return Err(format!("Payload file mode mismatch: {}", path.display()));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn set_mode_if_needed(path: &Path, mode: Option<u32>) -> Result<(), String> {
    if mode.is_none() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = mode.expect("mode is checked above");
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
            .map_err(|e| format!("Failed to set file permissions: {e}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn extract_update_zip(zip_path: &Path, output_dir: &Path) -> Result<(), String> {
    let file = File::open(zip_path).map_err(|e| format!("Failed to open update archive: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Failed to read update archive: {e}"))?;
    if archive.is_empty() || archive.len() > MAX_ZIP_ENTRIES {
        return Err("Update archive file count is outside the supported range.".to_string());
    }

    let mut extracted_bytes = 0u64;
    for index in 0..archive.len() {
        let mut file = archive
            .by_index(index)
            .map_err(|e| format!("Failed to read update archive entry: {e}"))?;
        let enclosed_name = file
            .enclosed_name()
            .ok_or_else(|| "Update archive contains an unsafe path.".to_string())?;
        if !enclosed_name.starts_with(PORTABLE_ROOT_DIR_NAME) {
            return Err("Update archive does not use the expected portable root.".to_string());
        }
        if file.is_symlink() {
            return Err("Update archive must not contain symlinks.".to_string());
        }
        extracted_bytes = extracted_bytes
            .checked_add(file.size())
            .ok_or_else(|| "Update archive is too large.".to_string())?;
        if extracted_bytes > MAX_EXTRACTED_BYTES {
            return Err("Update archive expands beyond the supported size.".to_string());
        }

        let output_path = output_dir.join(&enclosed_name);
        if file.is_dir() {
            fs::create_dir_all(&output_path)
                .map_err(|e| format!("Failed to create update directory: {e}"))?;
            continue;
        }
        if !file.is_file() {
            return Err("Update archive contains a non-file entry.".to_string());
        }
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update directory: {e}"))?;
        }
        let mut output = File::create(&output_path)
            .map_err(|e| format!("Failed to extract update file: {e}"))?;
        io::copy(&mut file, &mut output)
            .map_err(|e| format!("Failed to write update file: {e}"))?;
        if let Some(mode) = file.unix_mode() {
            set_mode_if_needed(&output_path, Some(mode & 0o777))?;
        }
    }
    Ok(())
}

fn download_bytes(url: &str, max_bytes: u64) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(45))
        .user_agent("Video For Lazies updater")
        .build()
        .map_err(|e| format!("Failed to create update client: {e}"))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to download update metadata: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Update metadata download failed with HTTP {}.",
            response.status()
        ));
    }
    if response.content_length().is_some_and(|len| len > max_bytes) {
        return Err("Update metadata is larger than expected.".to_string());
    }
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read update metadata: {e}"))?;
    if bytes.len() as u64 > max_bytes {
        return Err("Update metadata is larger than expected.".to_string());
    }
    Ok(bytes)
}

fn download_file<F>(
    url: &str,
    output_path: &Path,
    expected_size: u64,
    mut on_progress: F,
) -> Result<(), String>
where
    F: FnMut(u64, u64),
{
    validate_download_url(url, update_local_http_allowed())?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .user_agent("Video For Lazies updater")
        .build()
        .map_err(|e| format!("Failed to create update client: {e}"))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|e| format!("Failed to download update archive: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Update archive download failed with HTTP {}.",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|len| len != expected_size || len > MAX_UPDATE_BYTES)
    {
        return Err("Update archive size does not match the signed manifest.".to_string());
    }
    let mut output =
        File::create(output_path).map_err(|e| format!("Failed to create update archive: {e}"))?;
    let mut written = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let remaining = expected_size.saturating_add(1).saturating_sub(written);
        if remaining == 0 {
            break;
        }
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = response
            .read(&mut buffer[..limit])
            .map_err(|e| format!("Failed to read update archive: {e}"))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|e| format!("Failed to write update archive: {e}"))?;
        written = written.saturating_add(read as u64);
        on_progress(written.min(expected_size), expected_size);
    }
    if written != expected_size {
        return Err("Update archive size does not match the signed manifest.".to_string());
    }
    output
        .flush()
        .map_err(|e| format!("Failed to flush update archive: {e}"))?;
    output
        .sync_all()
        .map_err(|e| format!("Failed to persist update archive: {e}"))?;
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("Failed to hash file: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| format!("Failed to read file while hashing: {e}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!("{label} must be a lowercase SHA256 hex digest."));
    }
    Ok(())
}

fn load_prefs(app: &AppHandle) -> Result<UpdatePrefs, String> {
    let path = prefs_path(app)?;
    if !atomic_path_exists(&path) {
        return Ok(UpdatePrefs {
            schema: Some(UPDATE_PREFS_SCHEMA.to_string()),
            ..UpdatePrefs::default()
        });
    }
    let mut prefs: UpdatePrefs = read_atomic_json(&path, "update preferences")?;
    prefs.schema = Some(UPDATE_PREFS_SCHEMA.to_string());
    Ok(prefs)
}

fn save_prefs(app: &AppHandle, prefs: &UpdatePrefs) -> Result<(), String> {
    let path = prefs_path(app)?;
    let mut prefs = prefs.clone();
    prefs.schema = Some(UPDATE_PREFS_SCHEMA.to_string());
    atomic_write_json(&path, &prefs, "update preferences")
}

fn prefs_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_local_data_dir()
        .map_err(|e| format!("Failed to locate app data directory: {e}"))?
        .join("update-prefs.json"))
}

fn set_highest_trusted_version(prefs: &mut UpdatePrefs, version: &Version) {
    let should_update = prefs
        .highest_trusted_version
        .as_deref()
        .and_then(|stored| Version::parse(stored).ok())
        .map(|stored| version > &stored)
        .unwrap_or(true);
    if should_update {
        prefs.highest_trusted_version = Some(version.to_string());
    }
}

fn parse_semver(raw: &str) -> Result<Version, String> {
    Version::parse(raw.trim_start_matches('v')).map_err(|e| format!("Invalid SemVer version: {e}"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn current_target() -> Result<&'static str, String> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        return Ok("windows-x64");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        return Ok("linux-x64");
    }
    #[allow(unreachable_code)]
    Err("This platform is not supported by portable updates.".to_string())
}

fn main_executable_name() -> Result<&'static str, String> {
    #[cfg(target_os = "windows")]
    {
        return Ok("Video_For_Lazies.exe");
    }
    #[cfg(target_os = "linux")]
    {
        return Ok("video_for_lazies");
    }
    #[allow(unreachable_code)]
    Err("This platform is not supported by portable updates.".to_string())
}

fn helper_executable_name() -> Result<&'static str, String> {
    #[cfg(target_os = "windows")]
    {
        return Ok("vfl-update-helper.exe");
    }
    #[cfg(target_os = "linux")]
    {
        return Ok("vfl-update-helper");
    }
    #[allow(unreachable_code)]
    Err("This platform is not supported by portable updates.".to_string())
}

fn executable_suffix() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        ".exe"
    }
    #[cfg(not(target_os = "windows"))]
    {
        ""
    }
}

fn install_dir() -> Result<PathBuf, String> {
    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to locate app executable: {e}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Failed to locate app install directory.".to_string())
}

fn apply_plan_path(plan: &UpdateApplyPlan) -> PathBuf {
    plan.install_dir
        .join(UPDATE_STATE_DIR)
        .join("plans")
        .join(format!("{}.json", plan.update_id))
}

fn update_journal_path(install_dir: &Path) -> PathBuf {
    install_dir
        .join(UPDATE_STATE_DIR)
        .join(UPDATE_JOURNAL_FILE_NAME)
}

fn update_pending_path(install_dir: &Path) -> PathBuf {
    install_dir
        .join(UPDATE_STATE_DIR)
        .join(UPDATE_PENDING_FILE_NAME)
}

fn staging_lock_path(install_dir: &Path) -> PathBuf {
    install_dir
        .join(UPDATE_STATE_DIR)
        .join(UPDATE_STAGING_LOCK_FILE_NAME)
}

fn recovery_lock_path(install_dir: &Path) -> PathBuf {
    install_dir
        .join(UPDATE_STATE_DIR)
        .join(UPDATE_RECOVERY_LOCK_FILE_NAME)
}

fn apply_lock_path(install_dir: &Path) -> PathBuf {
    install_dir
        .join(UPDATE_STATE_DIR)
        .join(UPDATE_APPLY_LOCK_FILE_NAME)
}

fn atomic_previous_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("update-state");
    path.with_file_name(format!("{file_name}.previous"))
}

fn atomic_temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("update-state");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.{}.tmp",
        std::process::id(),
        now_ms(),
        ATOMIC_WRITE_NONCE.fetch_add(1, AtomicOrdering::Relaxed)
    ))
}

fn atomic_path_exists(path: &Path) -> bool {
    path.exists() || atomic_previous_path(path).exists()
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T, label: &str) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("Failed to serialize {label}: {e}"))?;
    atomic_write(path, &bytes, label)
}

fn atomic_write(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Failed to locate the {label} directory."))?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("Failed to create the {label} directory: {e}"))?;

    let temp_path = atomic_temp_path(path);
    let previous_path = atomic_previous_path(path);
    let mut temp = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|e| format!("Failed to create temporary {label}: {e}"))?;
    if let Err(error) = temp
        .write_all(bytes)
        .and_then(|()| temp.flush())
        .and_then(|()| temp.sync_all())
    {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("Failed to persist temporary {label}: {error}"));
    }
    drop(temp);

    if previous_path.exists() {
        if path.exists() {
            fs::remove_file(&previous_path)
                .map_err(|e| format!("Failed to clear previous {label}: {e}"))?;
        } else {
            // Complete an interrupted prior rotation before starting a new
            // one, so the last known-good file is never discarded.
            fs::rename(&previous_path, path)
                .map_err(|e| format!("Failed to restore previous {label}: {e}"))?;
        }
    }
    let had_current = path.exists();
    if had_current {
        fs::rename(path, &previous_path)
            .map_err(|e| format!("Failed to rotate current {label}: {e}"))?;
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let restore_result = if had_current {
            fs::rename(&previous_path, path)
        } else {
            Ok(())
        };
        let _ = fs::remove_file(&temp_path);
        return match restore_result {
            Ok(()) => Err(format!("Failed to publish new {label}: {error}")),
            Err(restore_error) => Err(format!(
                "Failed to publish new {label}: {error}; restoring the prior file also failed: {restore_error}"
            )),
        };
    }
    sync_parent_directory(parent)
        .map_err(|e| format!("Failed to sync the {label} directory: {e}"))?;
    if had_current {
        fs::remove_file(&previous_path)
            .map_err(|e| format!("Failed to retire previous {label}: {e}"))?;
        sync_parent_directory(parent)
            .map_err(|e| format!("Failed to sync the {label} directory: {e}"))?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> io::Result<()> {
    Ok(())
}

fn sync_directory_chain(start: &Path, root: &Path) -> Result<(), String> {
    if !start.starts_with(root) {
        return Err("Directory sync escaped its update root.".to_string());
    }
    let mut current = Some(start);
    while let Some(directory) = current {
        sync_parent_directory(directory)
            .map_err(|e| format!("Failed to sync update directory: {e}"))?;
        if directory == root {
            return Ok(());
        }
        current = directory.parent();
    }
    Err("Directory sync did not reach its update root.".to_string())
}

fn read_atomic_json<T: DeserializeOwned>(path: &Path, label: &str) -> Result<T, String> {
    let previous_path = atomic_previous_path(path);
    match fs::read(path) {
        Ok(bytes) => {
            // An existing but malformed primary is not an interrupted rename;
            // falling back could regress a destructive journal phase. Treat it
            // as corruption and fail closed instead.
            return serde_json::from_slice(&bytes)
                .map_err(|error| format!("Failed to parse {label}: {error}"));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Failed to read {label}: {error}")),
    }

    match fs::read(&previous_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("Failed to parse previous {label}: {e}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(format!("The {label} does not exist."))
        }
        Err(error) => Err(format!("Failed to read previous {label}: {error}")),
    }
}

fn remove_atomic_file(path: &Path) -> Result<(), String> {
    for candidate in [path.to_path_buf(), atomic_previous_path(path)] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Failed to remove saved update state: {error}")),
        }
    }
    Ok(())
}

struct StagingLockGuard {
    path: PathBuf,
    file: Option<File>,
    handed_off: bool,
}

impl StagingLockGuard {
    fn prepare_handoff(&mut self, plan: &UpdateApplyPlan) -> Result<(), String> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| "The update staging claim is no longer open.".to_string())?;
        write_lock_record(file, &UpdateLockRecord::helper_handoff(plan))
    }

    fn handoff(mut self) {
        self.handed_off = true;
        self.file.take();
    }
}

impl Drop for StagingLockGuard {
    fn drop(&mut self) {
        self.file.take();
        if !self.handed_off {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn acquire_staging_lock(install_dir: &Path) -> Result<StagingLockGuard, String> {
    let path = staging_lock_path(install_dir);
    let state_dir = install_dir.join(UPDATE_STATE_DIR);
    fs::create_dir_all(&state_dir)
        .map_err(|e| format!("Failed to create the update lock directory: {e}"))?;
    if apply_lock_path(install_dir).exists() || recovery_lock_path(install_dir).exists() {
        return Err("Another update operation is already applying files.".to_string());
    }
    let file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "Another app process is already staging an update, or its staging claim needs recovery."
                    .to_string()
            } else {
                format!("Failed to acquire the update staging claim: {error}")
            }
        })?;
    let mut guard = StagingLockGuard {
        path,
        file: Some(file),
        handed_off: false,
    };
    write_lock_record(
        guard.file.as_mut().expect("staging lock file is present"),
        &UpdateLockRecord::staging(),
    )?;

    if apply_lock_path(install_dir).exists()
        || recovery_lock_path(install_dir).exists()
        || atomic_path_exists(&update_journal_path(install_dir))
        || atomic_path_exists(&update_pending_path(install_dir))
    {
        return Err(
            "A saved update operation must be finalized or recovered before starting another."
                .to_string(),
        );
    }
    Ok(guard)
}

struct RecoveryHandoffGuard {
    global: StagingLockGuard,
    staging: StagingLockGuard,
}

impl RecoveryHandoffGuard {
    fn handoff_to_helper(mut self, plan: &UpdateApplyPlan, helper_pid: u32) -> Result<(), String> {
        let record = UpdateLockRecord::recovery_handoff_for(plan, helper_pid);
        write_lock_record(
            self.global
                .file
                .as_mut()
                .expect("global recovery claim is present"),
            &record,
        )?;
        write_lock_record(
            self.staging
                .file
                .as_mut()
                .expect("staging recovery claim is present"),
            &record,
        )?;
        self.staging.handoff();
        self.global.handoff();
        Ok(())
    }
}

fn acquire_recovery_global_lock(plan: &UpdateApplyPlan) -> Result<StagingLockGuard, String> {
    let path = recovery_lock_path(&plan.install_dir);
    let state_dir = plan.install_dir.join(UPDATE_STATE_DIR);
    fs::create_dir_all(&state_dir)
        .map_err(|e| format!("Failed to create the recovery lock directory: {e}"))?;
    match create_lock_guard(&path, &UpdateLockRecord::recovery_claim(plan)) {
        Ok(guard) => Ok(guard),
        Err(_) if path.exists() => {
            let existing = read_lock_record(&path)?;
            if existing.schema != UPDATE_JOURNAL_SCHEMA
                || existing.update_id.as_deref() != Some(plan.update_id.as_str())
                || !matches!(existing.phase.as_str(), "recoveryClaim" | "recoveryHandoff")
            {
                return Err("The global recovery claim does not match this journal.".to_string());
            }
            let apply_path = apply_lock_path(&plan.install_dir);
            if apply_path.exists() {
                let apply = read_lock_record(&apply_path)?;
                if apply.schema == UPDATE_JOURNAL_SCHEMA
                    && apply.update_id.as_deref() == Some(plan.update_id.as_str())
                    && process_is_running(apply.pid)?
                {
                    return Err("The recorded recovery helper is still active.".to_string());
                }
            }
            if process_is_running(existing.pid)? {
                return Err("Another recovery coordinator is still active.".to_string());
            }
            remove_file_with_retries(&path)?;
            create_lock_guard(&path, &UpdateLockRecord::recovery_claim(plan))
        }
        Err(error) => Err(error),
    }
}

fn create_lock_guard(path: &Path, record: &UpdateLockRecord) -> Result<StagingLockGuard, String> {
    let file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "Another recovery coordinator already owns the global claim.".to_string()
            } else {
                format!("Failed to acquire the update recovery claim: {error}")
            }
        })?;
    let mut guard = StagingLockGuard {
        path: path.to_path_buf(),
        file: Some(file),
        handed_off: false,
    };
    write_lock_record(
        guard.file.as_mut().expect("created lock file is present"),
        record,
    )?;
    Ok(guard)
}

fn acquire_recovery_staging_lock(plan: &UpdateApplyPlan) -> Result<RecoveryHandoffGuard, String> {
    let mut global = acquire_recovery_global_lock(plan)?;

    let apply_path = apply_lock_path(&plan.install_dir);
    if apply_path.exists() {
        let apply_record = read_lock_record(&apply_path)?;
        if apply_record.schema != UPDATE_JOURNAL_SCHEMA
            || apply_record.phase != "applying"
            || apply_record.update_id.as_deref() != Some(plan.update_id.as_str())
        {
            return Err("The saved apply lock does not match this recovery journal.".to_string());
        }
        if process_is_running(apply_record.pid)? {
            return Err(
                "The update helper recorded by the apply lock is still active.".to_string(),
            );
        }
        remove_file_with_retries(&apply_path)?;
    }

    let staging_path = staging_lock_path(&plan.install_dir);
    if staging_path.exists() {
        let staging_record = read_lock_record(&staging_path)?;
        if staging_record.schema != UPDATE_JOURNAL_SCHEMA
            || staging_record.update_id.as_deref() != Some(plan.update_id.as_str())
            || !matches!(
                staging_record.phase.as_str(),
                "helperHandoff" | "recoveryClaim" | "recoveryHandoff"
            )
        {
            return Err("The saved staging handoff does not match this journal.".to_string());
        }
        if process_is_running(staging_record.pid)? {
            return Err("The recorded staging or recovery owner is still active.".to_string());
        }
        remove_file_with_retries(&staging_path)?;
    }
    let staging = create_lock_guard(&staging_path, &UpdateLockRecord::recovery_handoff(plan))?;
    write_lock_record(
        global
            .file
            .as_mut()
            .expect("global recovery claim is present"),
        &UpdateLockRecord::recovery_handoff(plan),
    )?;
    Ok(RecoveryHandoffGuard { global, staging })
}

fn reclaim_stale_locks_for_safe_phase(plan: &UpdateApplyPlan) -> Result<bool, String> {
    let _global = acquire_recovery_global_lock(plan)?;
    let apply_path = apply_lock_path(&plan.install_dir);
    if apply_path.exists() {
        let apply = read_lock_record(&apply_path)?;
        if apply.schema != UPDATE_JOURNAL_SCHEMA
            || apply.phase != "applying"
            || apply.update_id.as_deref() != Some(plan.update_id.as_str())
        {
            return Err("The apply lock does not match the saved update plan.".to_string());
        }
        if process_is_running(apply.pid)? {
            return Ok(false);
        }
        remove_file_with_retries(&apply_path)?;
    }

    let staging_path = staging_lock_path(&plan.install_dir);
    if staging_path.exists() {
        let staging = read_lock_record(&staging_path)?;
        if staging.schema != UPDATE_JOURNAL_SCHEMA
            || staging.update_id.as_deref() != Some(plan.update_id.as_str())
            || !matches!(
                staging.phase.as_str(),
                "helperHandoff" | "recoveryClaim" | "recoveryHandoff"
            )
        {
            return Err("The staging lock does not match the saved update plan.".to_string());
        }
        if process_is_running(staging.pid)? {
            return Ok(false);
        }
        remove_file_with_retries(&staging_path)?;
    }
    Ok(true)
}

fn reclaim_orphan_staging_without_journal(install_dir: &Path) -> Result<bool, String> {
    if apply_lock_path(install_dir).exists()
        || atomic_path_exists(&update_journal_path(install_dir))
        || atomic_path_exists(&update_pending_path(install_dir))
    {
        return Ok(false);
    }
    let recovery_path = recovery_lock_path(install_dir);
    let cleanup_record = UpdateLockRecord::orphan_cleanup_claim();
    let _global = match create_lock_guard(&recovery_path, &cleanup_record) {
        Ok(guard) => guard,
        Err(_) if recovery_path.exists() => {
            let existing = read_lock_record(&recovery_path)?;
            if existing.schema != UPDATE_JOURNAL_SCHEMA
                || existing.phase != "orphanCleanup"
                || existing.update_id.is_some()
                || process_is_running(existing.pid)?
            {
                return Ok(false);
            }
            remove_file_with_retries(&recovery_path)?;
            create_lock_guard(&recovery_path, &cleanup_record)?
        }
        Err(error) => return Err(error),
    };

    let staging_path = staging_lock_path(install_dir);
    let staging = read_lock_record(&staging_path)?;
    if staging.schema != UPDATE_JOURNAL_SCHEMA
        || staging.phase != "staging"
        || staging.update_id.is_some()
        || process_is_running(staging.pid)?
    {
        return Ok(false);
    }
    remove_file_with_retries(&staging_path)?;
    let state_dir = install_dir.join(UPDATE_STATE_DIR);
    remove_dir_if_exists(&state_dir.join("staged"), "orphaned update stages")?;
    remove_dir_if_exists(&state_dir.join("backups"), "orphaned update backups")?;
    remove_dir_if_exists(&state_dir.join("plans"), "orphaned update plans")?;
    remove_dir_if_exists(&state_dir.join("publish"), "orphaned update publications")?;
    Ok(true)
}

fn write_lock_record(file: &mut File, record: &UpdateLockRecord) -> Result<(), String> {
    let bytes = serde_json::to_vec(record)
        .map_err(|e| format!("Failed to serialize the update lock: {e}"))?;
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.set_len(0))
        .and_then(|_| file.write_all(&bytes))
        .and_then(|_| file.flush())
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("Failed to persist the update lock: {e}"))
}

fn read_lock_record(path: &Path) -> Result<UpdateLockRecord, String> {
    let raw = fs::read(path).map_err(|e| format!("Failed to read the update handoff: {e}"))?;
    serde_json::from_slice(&raw).map_err(|e| format!("Failed to parse the update handoff: {e}"))
}

fn write_update_journal(journal: &UpdateJournal) -> Result<(), String> {
    atomic_write_json(
        &update_journal_path(&journal.plan.install_dir),
        journal,
        "update journal",
    )
}

fn validate_update_journal(journal: &UpdateJournal, install_dir: &Path) -> Result<(), String> {
    if journal.schema != UPDATE_JOURNAL_SCHEMA
        || journal.update_id != journal.plan.update_id
        || journal.plan.install_dir != install_dir
    {
        return Err("Update journal identity is invalid.".to_string());
    }
    validate_apply_plan(&journal.plan)?;
    let backup_digest_required = matches!(
        journal.phase,
        UpdateJournalPhase::BackupComplete
            | UpdateJournalPhase::Replacing
            | UpdateJournalPhase::AwaitingStartup
            | UpdateJournalPhase::RollingBack
            | UpdateJournalPhase::RolledBack
            | UpdateJournalPhase::RecoveryRequired
    );
    match journal.backup_payload_manifest_sha256.as_deref() {
        Some(digest) => validate_sha256(digest, "backup payload manifest SHA256")?,
        None if backup_digest_required => {
            return Err("Update journal is missing its verified backup digest.".to_string());
        }
        None => {}
    }
    Ok(())
}

fn load_matching_update_journal(plan: &UpdateApplyPlan) -> Result<UpdateJournal, String> {
    let journal: UpdateJournal =
        read_atomic_json(&update_journal_path(&plan.install_dir), "update journal")?;
    validate_update_journal(&journal, &plan.install_dir)?;
    if journal.plan != *plan || journal.phase != UpdateJournalPhase::Staged {
        return Err("Update journal does not match the staged apply plan.".to_string());
    }
    Ok(journal)
}

struct ApplyLockGuard {
    path: PathBuf,
    released: bool,
}

impl ApplyLockGuard {
    fn release(&mut self) -> Result<(), String> {
        if self.released {
            return Ok(());
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.released = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.released = true;
                Ok(())
            }
            Err(error) => Err(format!("Failed to release the update apply lock: {error}")),
        }
    }
}

impl Drop for ApplyLockGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplyLockMode {
    Apply,
    Recovery,
}

fn acquire_apply_lock(plan: &UpdateApplyPlan) -> Result<ApplyLockGuard, String> {
    acquire_apply_lock_for(plan, ApplyLockMode::Apply)
}

fn acquire_recovery_apply_lock(plan: &UpdateApplyPlan) -> Result<ApplyLockGuard, String> {
    acquire_apply_lock_for(plan, ApplyLockMode::Recovery)
}

fn acquire_apply_lock_for(
    plan: &UpdateApplyPlan,
    mode: ApplyLockMode,
) -> Result<ApplyLockGuard, String> {
    if mode == ApplyLockMode::Recovery {
        wait_for_recovery_handoff(plan, Duration::from_secs(5))?;
    }
    let path = apply_lock_path(&plan.install_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create the update lock directory: {e}"))?;
    }
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                "Another update helper is active, or a prior apply lock needs recovery.".to_string()
            } else {
                format!("Failed to acquire the update apply lock: {error}")
            }
        })?;
    if let Err(error) = write_lock_record(&mut file, &UpdateLockRecord::applying(plan)) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    drop(file);
    let guard = ApplyLockGuard {
        path,
        released: false,
    };

    let recovery_path = recovery_lock_path(&plan.install_dir);
    if mode == ApplyLockMode::Recovery {
        let recovery = read_lock_record(&recovery_path)?;
        if recovery.schema != UPDATE_JOURNAL_SCHEMA
            || recovery.phase != "recoveryHandoff"
            || recovery.update_id.as_deref() != Some(plan.update_id.as_str())
            || recovery.pid != std::process::id()
        {
            return Err("The global recovery handoff does not match this plan.".to_string());
        }
    }

    let staging_path = staging_lock_path(&plan.install_dir);
    if staging_path.exists() {
        let handoff = read_lock_record(&staging_path)?;
        let expected_phase = match mode {
            ApplyLockMode::Apply => "helperHandoff",
            ApplyLockMode::Recovery => "recoveryHandoff",
        };
        if handoff.schema != UPDATE_JOURNAL_SCHEMA
            || handoff.phase != expected_phase
            || handoff.update_id.as_deref() != Some(plan.update_id.as_str())
            || (mode == ApplyLockMode::Recovery && handoff.pid != std::process::id())
        {
            return Err(
                "The update staging handoff is stale or does not match this plan.".to_string(),
            );
        }
        remove_file_with_retries(&staging_path)?;
        if mode == ApplyLockMode::Recovery {
            remove_file_with_retries(&recovery_path)?;
        }
    } else if mode == ApplyLockMode::Recovery
        || plan.expected_payload_manifest_sha256.is_some()
        || atomic_path_exists(&update_journal_path(&plan.install_dir))
        || atomic_path_exists(&update_pending_path(&plan.install_dir))
    {
        return Err("The update helper did not receive a valid staging handoff.".to_string());
    }

    Ok(guard)
}

fn wait_for_recovery_handoff(plan: &UpdateApplyPlan, timeout: Duration) -> Result<(), String> {
    let recovery_path = recovery_lock_path(&plan.install_dir);
    let staging_path = staging_lock_path(&plan.install_dir);
    let started = std::time::Instant::now();
    loop {
        let matches_current_helper = read_lock_record(&recovery_path)
            .ok()
            .zip(read_lock_record(&staging_path).ok())
            .is_some_and(|(recovery, staging)| {
                [recovery, staging].into_iter().all(|record| {
                    record.schema == UPDATE_JOURNAL_SCHEMA
                        && record.phase == "recoveryHandoff"
                        && record.update_id.as_deref() == Some(plan.update_id.as_str())
                        && record.pid == std::process::id()
                })
            });
        if matches_current_helper {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(
                "The recovery helper did not receive its PID-bound handoff before timeout."
                    .to_string(),
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn complete_successful_update(app: &AppHandle, plan: &UpdateApplyPlan) -> Result<(), String> {
    let mut prefs = load_prefs(app)?;
    prefs.remind_later_version = None;
    set_highest_trusted_version(&mut prefs, &parse_semver(&plan.to_version)?);
    save_prefs(app, &prefs)?;
    cleanup_update_artifacts(plan, true)
}

fn cleanup_update_artifacts(plan: &UpdateApplyPlan, remove_journal: bool) -> Result<(), String> {
    let recovery_swap = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("recovery-swap")
        .join(&plan.update_id);
    remove_dir_if_exists(&recovery_swap, "recovery swap state")?;
    let publication_state = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("publish")
        .join(&plan.update_id);
    remove_dir_if_exists(&publication_state, "update publication state")?;
    remove_dir_if_exists(&plan.backup_dir, "update backup")?;
    let stage_root = plan
        .install_dir
        .join(UPDATE_STATE_DIR)
        .join("staged")
        .join(&plan.update_id);
    remove_dir_if_exists(&stage_root, "staged update")?;
    remove_atomic_file(&update_pending_path(&plan.install_dir))?;
    remove_atomic_file(&apply_plan_path(plan))?;
    if remove_journal {
        remove_atomic_file(&update_journal_path(&plan.install_dir))?;
    }
    Ok(())
}

fn remove_dir_if_exists(path: &Path, label: &str) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Failed to remove {label}: {error}")),
    }
}

fn cleanup_staged_update(plan: &UpdateApplyPlan) -> Result<(), String> {
    cleanup_update_artifacts(plan, true)
}

fn cleanup_old_temp_helpers() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with(TEMP_HELPER_PREFIX)
            && !file_name.starts_with(LEGACY_TEMP_HELPER_PREFIX)
        {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > Duration::from_secs(24 * 60 * 60))
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn embedded_public_key_decodes() {
        let key_text = decode_tauri_signer_text(UPDATE_PUBLIC_KEYS[0].as_bytes()).unwrap();
        PublicKey::decode(&key_text).unwrap();
    }

    #[test]
    fn verifies_tauri_signer_signature_fixture() {
        let manifest = br#"{"schema":"test"}
"#;
        let signature = b"dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVTbXpRK3I3V09GMXgxcjdwRTNaZ01iaXVXUzF2VVZoU2N2ajR0a3ZyNjlvNWFCRGJXdmNpYW9WV3BiMW1JSGkrZ01GNXhuWXZEYkV1YXdOQTJkNUczZlVxSHVxYnVEeVFnPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzc3ODQyMjQyCWZpbGU6dmZsLXVwZGF0ZS1tYW5pZmVzdC12MS5qc29uCnFMVTBldERQS1JyaWtDUG5DYmp0NDl0QlBxTFpVN0xrVHZqanE2ZklMWldRNWhqZ3pDUzFOamNGaTRGZlA5TG5Wcmd2SnNJMWlqUnBlN1NibFZheUF3PT0K";
        verify_manifest_signature(manifest, signature).unwrap();
    }

    #[test]
    fn payload_path_rejects_unsafe_paths() {
        for path in [
            "",
            "../outside",
            "/absolute",
            r"nested\windows",
            "C:/drive",
            "nested/../outside",
            "has\0null",
            ".vfl-updates/owned-state",
            ".VFL-UPDATES/owned-state",
        ] {
            assert!(payload_path_buf(path).is_err(), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unknown_dangling_symlinks_and_symlinked_parents_block_publication() {
        use std::os::unix::fs::symlink;

        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("symlink-collision", "1.1.0", "1.1.1");
        let old_manifest = read_payload_manifest(&install_dir).unwrap();
        let new_manifest = read_payload_manifest(&plan.stage_dir).unwrap();
        let old_paths = manifest_owned_paths(&old_manifest);
        let new_paths = manifest_owned_paths(&new_manifest);

        symlink("missing-target", install_dir.join("new-owned.txt")).unwrap();
        let error = validate_no_unknown_collisions(&plan, &old_paths, &new_paths).unwrap_err();
        assert!(error.contains("unknown file"));
        fs::remove_file(install_dir.join("new-owned.txt")).unwrap();

        let nested = PathBuf::from("nested").join("new-file.txt");
        symlink("missing-directory", install_dir.join("nested")).unwrap();
        let error = validate_no_unknown_collisions(&plan, &old_paths, &BTreeSet::from([nested]))
            .unwrap_err();
        assert!(error.contains("non-directory"));
    }

    #[test]
    fn payload_path_accepts_normalized_relative_paths() {
        assert_eq!(
            payload_path_buf("ffmpeg-sidecar/ffmpeg").unwrap(),
            PathBuf::from("ffmpeg-sidecar").join("ffmpeg")
        );
    }

    #[test]
    fn sha256_validation_requires_lower_hex() {
        assert!(validate_sha256(&"a".repeat(64), "hash").is_ok());
        assert!(validate_sha256(&"A".repeat(64), "hash").is_err());
        assert!(validate_sha256("abc", "hash").is_err());
    }

    #[test]
    fn artifact_filename_must_be_a_matching_safe_zip_basename() {
        let target = current_target().unwrap();
        let mut artifact = UpdateArtifact {
            file_name: "safe.zip".to_string(),
            url: "https://github.com/Setmaster/Video_For_Lazies/releases/download/v1.2.3/safe.zip"
                .to_string(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            root_dir: PORTABLE_ROOT_DIR_NAME.to_string(),
            payload_manifest: UpdatePayloadReference {
                path: format!("{PORTABLE_ROOT_DIR_NAME}/{PAYLOAD_MANIFEST_FILE_NAME}"),
                sha256: "b".repeat(64),
            },
        };
        assert!(validate_artifact(target, &artifact).is_ok());

        artifact.file_name = "../unsafe.zip".to_string();
        assert!(validate_artifact(target, &artifact).is_err());

        artifact.file_name = "different.zip".to_string();
        assert!(validate_artifact(target, &artifact).is_err());
    }

    #[test]
    fn apply_plans_without_relaunch_flag_default_to_direct_relaunch() {
        // Plans written by pre-1.7 versions omit relaunchViaShell entirely.
        let raw = r#"{
            "schema": "com.setmaster.video-for-lazies.apply-plan.v1",
            "updateId": "legacy",
            "fromVersion": "1.6.0",
            "toVersion": "1.7.0",
            "target": "windows-x64",
            "installDir": "C:/apps/vfl",
            "stageDir": "C:/apps/vfl/.vfl-updates/staged/legacy",
            "backupDir": "C:/apps/vfl/.vfl-updates/backups/legacy",
            "parentPid": 0,
            "executableName": "Video_For_Lazies.exe",
            "helperName": "vfl-update-helper.exe"
        }"#;
        let plan: UpdateApplyPlan = serde_json::from_str(raw).unwrap();
        assert!(!plan.relaunch_via_shell);
    }

    #[test]
    fn legacy_plan_digest_is_adopted_only_from_matching_verified_manifest_data() {
        let (_temp, _install_dir, mut plan, _plan_path) =
            create_apply_fixture("legacy-digest", "1.1.0", "1.1.1");
        plan.expected_payload_manifest_sha256 = None;
        let target = current_target().unwrap();
        let artifact = UpdateArtifact {
            file_name: "safe.zip".to_string(),
            url: "https://github.com/Setmaster/Video_For_Lazies/releases/download/v1.1.1/safe.zip"
                .to_string(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            root_dir: PORTABLE_ROOT_DIR_NAME.to_string(),
            payload_manifest: UpdatePayloadReference {
                path: format!("{PORTABLE_ROOT_DIR_NAME}/{PAYLOAD_MANIFEST_FILE_NAME}"),
                sha256: "b".repeat(64),
            },
        };
        let mut manifest = UpdateManifest {
            schema: UPDATE_MANIFEST_SCHEMA.to_string(),
            app_id: APP_ID.to_string(),
            channel: UPDATE_CHANNEL.to_string(),
            version: "1.1.1".to_string(),
            release_tag: "v1.1.1".to_string(),
            release_url: "https://github.com/Setmaster/Video_For_Lazies/releases/tag/v1.1.1"
                .to_string(),
            published_at: "2026-07-11T00:00:00Z".to_string(),
            min_updater_protocol: UPDATE_PROTOCOL_VERSION,
            notes: UpdateNotes {
                title: "Release".to_string(),
                summary: "Release".to_string(),
                url: "https://github.com/Setmaster/Video_For_Lazies/releases/tag/v1.1.1"
                    .to_string(),
            },
            artifacts: HashMap::from([(target.to_string(), artifact)]),
        };

        assert_eq!(
            signed_payload_digest_for_plan(&plan, &manifest).unwrap(),
            "b".repeat(64)
        );
        manifest.version = "1.1.2".to_string();
        manifest.release_tag = "v1.1.2".to_string();
        assert!(signed_payload_digest_for_plan(&plan, &manifest).is_err());
    }

    #[test]
    fn powershell_quote_escapes_embedded_single_quotes() {
        assert_eq!(powershell_quote(r"C:\Apps\VFL"), r"'C:\Apps\VFL'");
        assert_eq!(
            powershell_quote(r"C:\User's Files\VFL"),
            r"'C:\User''s Files\VFL'"
        );
    }

    #[test]
    fn install_dir_writable_reflects_filesystem_access() {
        let dir = std::env::temp_dir().join(format!(
            "vfl_updater_write_probe_test_{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(install_dir_writable(&dir));
        // The probe must not leave artifacts behind besides the state dir.
        let state_dir = dir.join(UPDATE_STATE_DIR);
        assert!(state_dir.exists());
        assert_eq!(fs::read_dir(&state_dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn launched_helper_temp_name_avoids_uac_installer_keywords() {
        // Keep the launched copy keyword-free as a second defense if packaging
        // ever strips the helper's explicit asInvoker manifest.
        let sample = format!("{TEMP_HELPER_PREFIX}1.8.1-1700000000000.exe");
        let lower = sample.to_ascii_lowercase();
        for keyword in ["update", "setup", "install", "patch"] {
            assert!(
                !lower.contains(keyword),
                "launched helper name '{sample}' must not contain installer keyword '{keyword}'"
            );
        }
        // Cleanup still sweeps the legacy ("...update...") copies older versions
        // wrote, so they do not pile up after the rename.
        assert_ne!(TEMP_HELPER_PREFIX, LEGACY_TEMP_HELPER_PREFIX);
        assert!(LEGACY_TEMP_HELPER_PREFIX.contains("update"));
    }

    #[test]
    fn helper_elevation_fallback_is_limited_to_windows_error_740() {
        assert!(is_elevation_required_error(&io::Error::from_raw_os_error(
            740
        )));
        for code in [2, 5, 1223] {
            assert!(!is_elevation_required_error(&io::Error::from_raw_os_error(
                code
            )));
        }
    }

    #[test]
    fn dismissing_update_prompt_behaves_like_remind_later() {
        let mut prefs = UpdatePrefs::default();
        let version = Version::parse("1.2.3").unwrap();

        apply_prompt_choice(&mut prefs, PromptChoice::Dismiss, &version, 1234);

        assert_eq!(prefs.remind_later_version.as_deref(), Some("1.2.3"));
        assert_eq!(prefs.suppress_prompts_until_ms, None);
    }

    #[test]
    fn skipping_update_prompt_clears_remind_later() {
        let mut prefs = UpdatePrefs {
            remind_later_version: Some("1.2.3".to_string()),
            ..UpdatePrefs::default()
        };
        let version = Version::parse("1.2.3").unwrap();

        apply_prompt_choice(&mut prefs, PromptChoice::Skip7Days, &version, 1234);

        assert_eq!(prefs.remind_later_version, None);
        assert_eq!(
            prefs.suppress_prompts_until_ms,
            Some(1234_u64.saturating_add(SKIP_INTERVAL_MS))
        );
    }

    #[test]
    fn available_update_check_records_next_launch_reminder() {
        let mut prefs = UpdatePrefs::default();
        let latest = Version::parse("1.2.3").unwrap();
        let current = Version::parse("1.2.2").unwrap();

        record_update_check_result(&mut prefs, &latest, &current, 5678);

        assert_eq!(prefs.last_checked_at_ms, Some(5678));
        assert_eq!(prefs.remind_later_version.as_deref(), Some("1.2.3"));
        assert_eq!(prefs.highest_trusted_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn current_update_check_clears_next_launch_reminder() {
        let mut prefs = UpdatePrefs {
            remind_later_version: Some("1.2.3".to_string()),
            highest_trusted_version: Some("1.2.3".to_string()),
            ..UpdatePrefs::default()
        };
        let latest = Version::parse("1.2.3").unwrap();
        let current = Version::parse("1.2.3").unwrap();

        record_update_check_result(&mut prefs, &latest, &current, 9012);

        assert_eq!(prefs.last_checked_at_ms, Some(9012));
        assert_eq!(prefs.remind_later_version, None);
        assert_eq!(prefs.highest_trusted_version.as_deref(), Some("1.2.3"));
    }

    #[test]
    fn payload_manifest_validation_rejects_extra_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let target = current_target().unwrap();
        write_minimal_payload(root, "app", None);
        let manifest = manifest_from_dir(root, "1.2.3", target);
        fs::write(
            root.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        write_payload_file(root, "extra.txt", b"extra", Some(0o644));

        let err = validate_payload_manifest(root, &manifest, "1.2.3", target, true).unwrap_err();
        assert!(err.contains("exactly match"));
    }

    #[test]
    fn apply_update_preserves_unknown_files_and_removes_old_owned_files() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Video_For_Lazies");
        let stage_dir = install_dir
            .join(UPDATE_STATE_DIR)
            .join("staged")
            .join("test-update")
            .join(PORTABLE_ROOT_DIR_NAME);
        let backup_dir = install_dir
            .join(UPDATE_STATE_DIR)
            .join("backups")
            .join("test-update");
        fs::create_dir_all(&install_dir).unwrap();
        fs::create_dir_all(&stage_dir).unwrap();

        write_minimal_payload(&install_dir, "old", Some(("obsolete-owned.txt", b"remove")));
        let old_manifest = manifest_from_dir(&install_dir, "1.1.0", current_target().unwrap());
        fs::write(
            install_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();
        write_payload_file(&install_dir, "user-note.txt", b"keep", platform_mode(0o644));
        let mut staging_claim = acquire_staging_lock(&install_dir).unwrap();

        write_minimal_payload(&stage_dir, "new", Some(("new-owned.txt", b"add")));
        let new_manifest = manifest_from_dir(&stage_dir, "1.1.1", current_target().unwrap());
        fs::write(
            stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&new_manifest).unwrap(),
        )
        .unwrap();
        let expected_payload_manifest_sha256 =
            sha256_file(&stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME)).unwrap();

        let plan = UpdateApplyPlan {
            schema: APPLY_PLAN_SCHEMA.to_string(),
            update_id: "test-update".to_string(),
            from_version: "1.1.0".to_string(),
            to_version: "1.1.1".to_string(),
            target: current_target().unwrap().to_string(),
            install_dir: install_dir.clone(),
            stage_dir,
            backup_dir,
            parent_pid: 0,
            executable_name: main_executable_name().unwrap().to_string(),
            helper_name: helper_executable_name().unwrap().to_string(),
            expected_payload_manifest_sha256: Some(expected_payload_manifest_sha256),
            relaunch_via_shell: false,
        };
        let plan_path = apply_plan_path(&plan);
        fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        atomic_write_json(&plan_path, &plan, "test update plan").unwrap();
        write_update_journal(&UpdateJournal::new(&plan, UpdateJournalPhase::Staged)).unwrap();
        staging_claim.prepare_handoff(&plan).unwrap();
        staging_claim.handoff();

        apply_update_plan_inner(&plan_path, false).unwrap();

        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"new"
        );
        assert_eq!(
            fs::read(install_dir.join("user-note.txt")).unwrap(),
            b"keep"
        );
        assert!(!install_dir.join("obsolete-owned.txt").exists());
        assert_eq!(fs::read(install_dir.join("new-owned.txt")).unwrap(), b"add");
        assert!(
            install_dir
                .join(UPDATE_STATE_DIR)
                .join("pending-success.json")
                .exists()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn atomic_publication_supports_read_only_payload_modes() {
        use std::os::unix::fs::PermissionsExt;

        let (_temp, install_dir, mut plan, plan_path) =
            create_apply_fixture("read-only", "1.1.0", "1.1.1");
        let staged_readme = plan.stage_dir.join("README.md");
        fs::set_permissions(&staged_readme, fs::Permissions::from_mode(0o444)).unwrap();
        let mut manifest = read_payload_manifest(&plan.stage_dir).unwrap();
        manifest
            .files
            .iter_mut()
            .find(|entry| entry.path == "README.md")
            .unwrap()
            .mode = Some(0o444);
        fs::write(
            plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        plan.expected_payload_manifest_sha256 =
            Some(sha256_file(&plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME)).unwrap());
        atomic_write_json(&plan_path, &plan, "read-only update plan").unwrap();
        write_update_journal(&UpdateJournal::new(&plan, UpdateJournalPhase::Staged)).unwrap();

        apply_update_plan_inner(&plan_path, false).unwrap();

        assert_eq!(
            fs::metadata(install_dir.join("README.md"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444
        );
        assert_eq!(fs::read(install_dir.join("README.md")).unwrap(), b"new");
    }

    #[test]
    fn duplicate_update_operations_are_rejected_until_guard_drops() {
        let coordinator = UpdaterCoordinator::new();
        let first = coordinator.begin("apply").unwrap();

        let error = coordinator.begin("apply").err().unwrap();
        assert_eq!(error.code, "update-busy");
        assert!(error.retryable);

        drop(first);
        assert!(coordinator.begin("apply").is_ok());
    }

    #[test]
    fn public_update_errors_never_serialize_internal_paths() {
        let private_path = "/home/private-user/portable/.vfl-updates/backups/secret";
        let error = UpdatePublicError::prepare_failed(format!(
            "Failed to copy {private_path} to C:\\Users\\private-user\\Desktop"
        ));
        let serialized = serde_json::to_string(&error).unwrap();

        assert!(!serialized.contains(private_path));
        assert!(!serialized.contains("private-user"));
        assert_eq!(error.code, "update-prepare-failed");
    }

    #[test]
    fn parent_exit_wait_is_bounded_and_observes_process_state() {
        wait_for_parent_to_exit_with_timeout(0, Duration::ZERO, Duration::ZERO).unwrap();

        let error = wait_for_parent_to_exit_with_timeout(
            std::process::id(),
            Duration::from_millis(5),
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(error.contains("timeout"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn unreaped_linux_zombie_is_treated_as_exited() {
        let mut child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if !process_is_running(child.id()).unwrap() {
                child.wait().unwrap();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.wait();
        panic!("unreaped exited child was still classified as running");
    }

    #[test]
    fn windows_parent_wait_contract_uses_bounded_kernel_wait_without_tasklist() {
        let source = include_str!("updater.rs");
        assert!(source.contains("OpenProcess(SYNCHRONIZE"));
        assert!(source.contains("WaitForSingleObject(handle, timeout_ms)"));
        assert!(source.contains("CloseHandle(handle)"));
        assert!(!source.contains("Command::new(\"tasklist.exe\")"));
    }

    #[test]
    fn apply_lock_rejects_duplicate_helpers_and_releases_cleanly() {
        let (_temp, _install_dir, plan, _plan_path) =
            create_apply_fixture("lock", "1.1.0", "1.1.1");
        let mut first = acquire_apply_lock(&plan).unwrap();

        let error = acquire_apply_lock(&plan).err().unwrap();
        assert!(error.contains("Another update helper"));

        first.release().unwrap();
        assert!(!apply_lock_path(&plan.install_dir).exists());
        let stale_retry = acquire_apply_lock(&plan).err().unwrap();
        assert!(stale_retry.contains("valid staging handoff"));
    }

    #[test]
    fn staging_lock_serializes_app_processes_before_download() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Video_For_Lazies");
        fs::create_dir_all(&install_dir).unwrap();
        let first = acquire_staging_lock(&install_dir).unwrap();

        let duplicate = acquire_staging_lock(&install_dir).err().unwrap();
        assert!(duplicate.contains("already staging"));

        drop(first);
        assert!(acquire_staging_lock(&install_dir).is_ok());
    }

    #[test]
    fn failed_staging_removes_partial_archive_extract_and_backup_trees() {
        let temp = tempfile::tempdir().unwrap();
        let stage_root = temp.path().join("staged").join("failed");
        let backup_dir = temp.path().join("backups").join("failed");
        let orphan_plan_path = temp.path().join("plans").join("failed.json");
        fs::create_dir_all(stage_root.join("extract")).unwrap();
        fs::create_dir_all(&backup_dir).unwrap();
        fs::create_dir_all(orphan_plan_path.parent().unwrap()).unwrap();
        fs::write(stage_root.join("partial.zip"), b"partial").unwrap();
        fs::write(backup_dir.join("partial.bin"), b"partial").unwrap();
        fs::write(&orphan_plan_path, b"partial").unwrap();

        let error = finish_staged_result(
            Err("injected staging failure".to_string()),
            &stage_root,
            &backup_dir,
            &orphan_plan_path,
        )
        .unwrap_err();
        assert!(error.contains("injected staging failure"));
        assert!(!stage_root.exists());
        assert!(!backup_dir.exists());
        assert!(!orphan_plan_path.exists());
    }

    #[test]
    fn missing_primary_journal_recovers_from_atomic_previous_file() {
        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("journal-fallback", "1.1.0", "1.1.1");
        let journal_path = update_journal_path(&install_dir);
        let journal: UpdateJournal = read_atomic_json(&journal_path, "update journal").unwrap();
        fs::write(
            atomic_previous_path(&journal_path),
            serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        fs::remove_file(&journal_path).unwrap();

        let recovered: UpdateJournal = read_atomic_json(&journal_path, "update journal").unwrap();
        assert_eq!(recovered.update_id, plan.update_id);
        assert_eq!(recovered.phase, UpdateJournalPhase::Staged);
    }

    #[test]
    fn corrupt_primary_journal_never_regresses_to_previous_phase() {
        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("journal-no-regress", "1.1.0", "1.1.1");
        let journal_path = update_journal_path(&install_dir);
        let previous = UpdateJournal::new(&plan, UpdateJournalPhase::BackupComplete);
        fs::write(
            atomic_previous_path(&journal_path),
            serde_json::to_vec(&previous).unwrap(),
        )
        .unwrap();
        fs::write(&journal_path, b"{ corrupt destructive phase").unwrap();

        assert!(read_atomic_json::<UpdateJournal>(&journal_path, "update journal").is_err());
    }

    #[test]
    fn corrupt_journal_without_atomic_fallback_fails_closed() {
        let (_temp, install_dir, _plan, _plan_path) =
            create_apply_fixture("journal-corrupt", "1.1.0", "1.1.1");
        let journal_path = update_journal_path(&install_dir);
        remove_atomic_file(&journal_path).unwrap();
        fs::write(&journal_path, b"{ corrupt").unwrap();

        assert!(read_atomic_json::<UpdateJournal>(&journal_path, "update journal").is_err());
    }

    #[test]
    fn destructive_journal_requires_a_valid_recorded_backup_digest() {
        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("journal-backup-digest", "1.1.0", "1.1.1");
        let mut journal = UpdateJournal::new(&plan, UpdateJournalPhase::Replacing);
        let error = validate_update_journal(&journal, &install_dir).unwrap_err();
        assert!(error.contains("verified backup digest"));

        journal.backup_payload_manifest_sha256 = Some("not-a-digest".to_string());
        let error = validate_update_journal(&journal, &install_dir).unwrap_err();
        assert!(error.contains("SHA256"));
    }

    #[test]
    fn stale_destructive_journal_is_not_reused_as_a_staged_operation() {
        let (_temp, _install_dir, plan, _plan_path) =
            create_apply_fixture("journal-stale", "1.1.0", "1.1.1");
        let mut journal = UpdateJournal::new(&plan, UpdateJournalPhase::Replacing);
        journal.backup_payload_manifest_sha256 = Some("a".repeat(64));
        journal.error_code = Some("interrupted".to_string());
        write_update_journal(&journal).unwrap();

        let error = load_matching_update_journal(&plan).unwrap_err();
        assert!(error.contains("does not match"));
    }

    #[test]
    fn signed_plan_reconstructs_only_a_missing_pre_apply_journal() {
        let (_temp, install_dir, mut plan, plan_path) =
            create_apply_fixture("journal-reconstruct", "1.1.0", "1.1.1");
        remove_atomic_file(&update_journal_path(&install_dir)).unwrap();

        let journal = prepare_apply_journal(&plan_path, &mut plan).unwrap();
        assert_eq!(journal.phase, UpdateJournalPhase::Staged);
        assert_eq!(journal.plan, plan);

        fs::write(update_journal_path(&install_dir), b"{ corrupt").unwrap();
        assert!(prepare_apply_journal(&plan_path, &mut plan).is_err());
    }

    #[test]
    fn signed_payload_manifest_digest_is_rechecked_before_apply() {
        let (_temp, install_dir, plan, plan_path) =
            create_apply_fixture("digest", "1.1.0", "1.1.1");
        let manifest_path = plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME);
        let mut bytes = fs::read(&manifest_path).unwrap();
        bytes.push(b'\n');
        fs::write(&manifest_path, bytes).unwrap();

        let error = apply_update_plan_inner(&plan_path, false).unwrap_err();
        assert!(error.contains("signed update manifest"));
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
    }

    #[test]
    fn corrupt_installed_manifest_fails_closed_before_backup_or_replace() {
        let (_temp, install_dir, _plan, plan_path) =
            create_apply_fixture("old-manifest", "1.1.0", "1.1.1");
        fs::write(
            install_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            b"not valid json",
        )
        .unwrap();

        let error = apply_update_plan_inner(&plan_path, false).unwrap_err();
        assert!(error.contains("parse payload manifest"));
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
    }

    #[test]
    fn backup_requires_every_installed_owned_file_and_validates_exact_copy() {
        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("backup-complete", "1.1.0", "1.1.1");
        let old_manifest = read_payload_manifest(&install_dir).unwrap();
        let mut old_paths = manifest_owned_paths(&old_manifest);
        old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));

        fs::remove_file(install_dir.join("README.md")).unwrap();
        let error = backup_existing_owned_files(&plan, &old_paths).unwrap_err();
        assert!(error.contains("missing") || error.contains("disappeared"));
        assert!(!plan.backup_dir.join("README.md").exists());
    }

    #[test]
    fn post_preflight_new_file_collision_is_preserved_through_rollback() {
        let (_temp, install_dir, _plan, plan_path) =
            create_apply_fixture("new-file-collision", "1.1.0", "1.1.1");
        let error = apply_update_plan_inner_with_fault(
            &plan_path,
            false,
            ApplyFault {
                new_target_collision: Some(TestNewTargetCollision::RegularFile),
                ..ApplyFault::default()
            },
        )
        .unwrap_err();

        assert!(error.contains("unknown file"));
        assert!(error.contains("previous version was restored"));
        assert_eq!(
            fs::read(install_dir.join("new-owned.txt")).unwrap(),
            b"preserve-unknown-file"
        );
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
        let journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(journal.phase, UpdateJournalPhase::RolledBack);
    }

    #[cfg(unix)]
    #[test]
    fn post_preflight_dangling_symlink_collision_is_preserved_through_rollback() {
        let (_temp, install_dir, _plan, plan_path) =
            create_apply_fixture("new-symlink-collision", "1.1.0", "1.1.1");
        let error = apply_update_plan_inner_with_fault(
            &plan_path,
            false,
            ApplyFault {
                new_target_collision: Some(TestNewTargetCollision::DanglingSymlink),
                ..ApplyFault::default()
            },
        )
        .unwrap_err();

        assert!(error.contains("unknown file"));
        let collision = install_dir.join("new-owned.txt");
        assert!(
            fs::symlink_metadata(&collision)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_link(&collision).unwrap(),
            PathBuf::from("missing-user-target")
        );
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
    }

    #[test]
    fn interrupted_replacement_restores_previous_version_and_records_rollback() {
        let (_temp, install_dir, _plan, plan_path) =
            create_apply_fixture("rollback", "1.1.0", "1.1.1");
        let fault = ApplyFault {
            fail_after_replacements: Some(1),
            remove_backup_before_rollback: false,
            pause_before_main_publication: false,
            ..ApplyFault::default()
        };

        let error = apply_update_plan_inner_with_fault(&plan_path, false, fault).unwrap_err();
        assert!(error.contains("previous version was restored"));
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
        let journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(journal.phase, UpdateJournalPhase::RolledBack);
        assert!(!atomic_path_exists(&update_pending_path(&install_dir)));
    }

    #[test]
    fn rollback_failure_is_reported_and_records_recovery_required() {
        let (_temp, install_dir, _plan, plan_path) =
            create_apply_fixture("rollback-failure", "1.1.0", "1.1.1");
        let fault = ApplyFault {
            fail_after_replacements: Some(1),
            remove_backup_before_rollback: true,
            pause_before_main_publication: false,
            ..ApplyFault::default()
        };

        let error = apply_update_plan_inner_with_fault(&plan_path, false, fault).unwrap_err();
        assert!(error.contains("rollback also failed"));
        assert!(error.contains("Manual recovery is required"));
        let journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(journal.phase, UpdateJournalPhase::RecoveryRequired);
        assert_eq!(journal.error_code.as_deref(), Some("rollback-failed"));
    }

    #[test]
    fn live_apply_owner_is_never_reclaimed() {
        let (_temp, _install_dir, plan, _plan_path) =
            create_apply_fixture("live-owner", "1.1.0", "1.1.1");
        let mut apply = acquire_apply_lock(&plan).unwrap();

        let error = acquire_recovery_staging_lock(&plan).err().unwrap();
        assert!(error.contains("still active"));
        assert!(apply_lock_path(&plan.install_dir).exists());
        assert!(!recovery_lock_path(&plan.install_dir).exists());

        apply.release().unwrap();
    }

    #[test]
    fn dead_recovery_handoff_and_both_lock_window_are_reclaimed() {
        let (_temp, _install_dir, plan, _plan_path) =
            create_apply_fixture("dead-handoff", "1.1.0", "1.1.1");
        remove_file_with_retries(&staging_lock_path(&plan.install_dir)).unwrap();
        let mut dead_record = UpdateLockRecord::recovery_handoff(&plan);
        dead_record.pid = u32::MAX;
        let recovery =
            create_lock_guard(&recovery_lock_path(&plan.install_dir), &dead_record).unwrap();
        recovery.handoff();
        let staging =
            create_lock_guard(&staging_lock_path(&plan.install_dir), &dead_record).unwrap();
        staging.handoff();
        let apply = create_lock_guard(
            &apply_lock_path(&plan.install_dir),
            &UpdateLockRecord {
                phase: "applying".to_string(),
                ..dead_record.clone()
            },
        )
        .unwrap();
        apply.handoff();

        let replacement_claim = acquire_recovery_staging_lock(&plan).unwrap();
        assert!(!apply_lock_path(&plan.install_dir).exists());
        assert!(recovery_lock_path(&plan.install_dir).exists());
        assert!(staging_lock_path(&plan.install_dir).exists());
        drop(replacement_claim);
        assert!(!recovery_lock_path(&plan.install_dir).exists());
        assert!(!staging_lock_path(&plan.install_dir).exists());
    }

    #[test]
    fn rolled_back_state_reclaims_dead_apply_lock_before_startup_cleanup() {
        let (_temp, install_dir, plan, _plan_path) =
            create_apply_fixture("rolled-back-dead-lock", "1.1.0", "1.1.1");
        remove_file_with_retries(&staging_lock_path(&install_dir)).unwrap();
        let mut journal = UpdateJournal::new(&plan, UpdateJournalPhase::RolledBack);
        journal.error_code = Some("automatic-recovery-complete".to_string());
        write_update_journal(&journal).unwrap();
        let mut dead_apply = UpdateLockRecord::applying(&plan);
        dead_apply.pid = u32::MAX;
        let apply = create_lock_guard(&apply_lock_path(&install_dir), &dead_apply).unwrap();
        apply.handoff();

        assert!(reclaim_stale_locks_for_safe_phase(&plan).unwrap());
        assert!(!apply_lock_path(&install_dir).exists());
        cleanup_update_artifacts(&plan, true).unwrap();
        assert!(!atomic_path_exists(&update_journal_path(&install_dir)));
    }

    #[test]
    fn dead_pre_download_staging_owner_is_reclaimed_without_a_journal() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Video_For_Lazies");
        let state_dir = install_dir.join(UPDATE_STATE_DIR);
        fs::create_dir_all(state_dir.join("staged/orphan")).unwrap();
        fs::create_dir_all(state_dir.join("backups/orphan")).unwrap();
        fs::create_dir_all(state_dir.join("plans")).unwrap();
        fs::write(state_dir.join("plans/orphan.json"), b"orphan").unwrap();
        let mut dead_staging = UpdateLockRecord::staging();
        dead_staging.pid = u32::MAX;
        let staging = create_lock_guard(&staging_lock_path(&install_dir), &dead_staging).unwrap();
        staging.handoff();

        assert!(reclaim_orphan_staging_without_journal(&install_dir).unwrap());
        assert!(!staging_lock_path(&install_dir).exists());
        assert!(!state_dir.join("staged").exists());
        assert!(!state_dir.join("backups").exists());
        assert!(!state_dir.join("plans").exists());
    }

    #[test]
    fn automatic_recovery_restores_verified_backup_and_preserves_unknown_siblings() {
        let (_temp, install_dir, plan, plan_path) =
            create_apply_fixture("recover-success", "1.1.0", "1.1.1");
        let mut apply = prepare_replacing_state_for_test(&plan_path);
        apply.release().unwrap();
        let unknown_sibling = install_dir.join(format!(
            ".{}.recovery-previous",
            main_executable_name().unwrap()
        ));
        fs::write(&unknown_sibling, b"user-owned").unwrap();

        let claim = acquire_recovery_staging_lock(&plan).unwrap();
        let mut journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        journal.transition(
            UpdateJournalPhase::RollingBack,
            Some("automatic-recovery-requested"),
        );
        write_update_journal(&journal).unwrap();
        claim.handoff_to_helper(&plan, std::process::id()).unwrap();

        recover_update_plan(&plan_path, 0, false).unwrap();
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
        assert_eq!(fs::read(&unknown_sibling).unwrap(), b"user-owned");
        assert_eq!(
            fs::read(install_dir.join("user-note.txt")).unwrap(),
            b"keep"
        );
        assert!(!install_dir.join("new-owned.txt").exists());
        let recovered: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(recovered.phase, UpdateJournalPhase::RolledBack);
        assert!(plan.backup_dir.exists());
        assert!(plan.stage_dir.exists());

        cleanup_update_artifacts(&plan, true).unwrap();
        assert!(!atomic_path_exists(&update_journal_path(&install_dir)));
    }

    #[test]
    fn corrupt_backup_remains_recovery_required_without_cleanup() {
        let (_temp, install_dir, plan, plan_path) =
            create_apply_fixture("recover-corrupt", "1.1.0", "1.1.1");
        let mut apply = prepare_replacing_state_for_test(&plan_path);
        apply.release().unwrap();
        let claim = acquire_recovery_staging_lock(&plan).unwrap();
        let mut journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        journal.transition(
            UpdateJournalPhase::RollingBack,
            Some("automatic-recovery-requested"),
        );
        write_update_journal(&journal).unwrap();
        fs::write(plan.backup_dir.join(PAYLOAD_MANIFEST_FILE_NAME), b"corrupt").unwrap();
        claim.handoff_to_helper(&plan, std::process::id()).unwrap();

        assert!(recover_update_plan(&plan_path, 0, false).is_err());
        let failed: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(failed.phase, UpdateJournalPhase::RecoveryRequired);
        assert!(plan.backup_dir.exists());
        assert!(plan.stage_dir.exists());
        assert!(apply_plan_path(&plan).exists());
    }

    #[test]
    fn self_consistent_backup_substitution_is_rejected_by_recorded_digest() {
        let (_temp, install_dir, plan, plan_path) =
            create_apply_fixture("recover-substituted", "1.1.0", "1.1.1");
        let mut apply = prepare_replacing_state_for_test(&plan_path);
        apply.release().unwrap();
        let claim = acquire_recovery_staging_lock(&plan).unwrap();
        let mut journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        journal.transition(
            UpdateJournalPhase::RollingBack,
            Some("automatic-recovery-requested"),
        );
        write_update_journal(&journal).unwrap();

        write_payload_file(
            &plan.backup_dir,
            "README.md",
            b"internally-valid-substitute",
            platform_mode(0o644),
        );
        let mut substituted_manifest = read_payload_manifest(&plan.backup_dir).unwrap();
        let substituted_entry = substituted_manifest
            .files
            .iter_mut()
            .find(|entry| entry.path == "README.md")
            .unwrap();
        substituted_entry.sha256 = sha256_file(&plan.backup_dir.join("README.md")).unwrap();
        substituted_entry.size_bytes = b"internally-valid-substitute".len() as u64;
        fs::write(
            plan.backup_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&substituted_manifest).unwrap(),
        )
        .unwrap();
        validate_payload_manifest(
            &plan.backup_dir,
            &substituted_manifest,
            &plan.from_version,
            &plan.target,
            true,
        )
        .unwrap();
        let main_before = fs::read(install_dir.join(main_executable_name().unwrap())).unwrap();
        claim.handoff_to_helper(&plan, std::process::id()).unwrap();

        let error = recover_update_plan(&plan_path, 0, false).unwrap_err();
        assert!(error.contains("recorded digest"));
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            main_before
        );
        let failed: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(failed.phase, UpdateJournalPhase::RecoveryRequired);
        assert!(plan.backup_dir.exists());
        assert!(plan.stage_dir.exists());
        assert!(apply_plan_path(&plan).exists());
    }

    #[test]
    fn post_restore_manifest_mutation_never_reaches_rolled_back() {
        let (_temp, install_dir, plan, plan_path) =
            create_apply_fixture("recover-post-mutation", "1.1.0", "1.1.1");
        let mut apply = prepare_replacing_state_for_test(&plan_path);
        apply.release().unwrap();
        let claim = acquire_recovery_staging_lock(&plan).unwrap();
        let mut journal: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        journal.transition(
            UpdateJournalPhase::RollingBack,
            Some("automatic-recovery-requested"),
        );
        write_update_journal(&journal).unwrap();
        claim.handoff_to_helper(&plan, std::process::id()).unwrap();

        let error = recover_update_plan_with_fault(
            &plan_path,
            0,
            false,
            RecoveryFault {
                mutate_manifest_before_final_check: true,
                ..RecoveryFault::default()
            },
        )
        .unwrap_err();
        assert!(error.contains("verification failed"));
        let failed: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(failed.phase, UpdateJournalPhase::RecoveryRequired);
        assert!(plan.backup_dir.exists());
        assert!(plan.stage_dir.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn legacy_v191_retry_recovers_a_kill_before_atomic_main_publication() {
        let (temp, install_dir, mut first_plan, first_plan_path) =
            create_apply_fixture("legacy-first", "1.9.1", "1.10.0");
        configure_runnable_main_scripts(&first_plan);
        first_plan.expected_payload_manifest_sha256 = None;
        first_plan.relaunch_via_shell = false;
        remove_atomic_file(&update_journal_path(&install_dir)).unwrap();
        remove_file_with_retries(&staging_lock_path(&install_dir)).unwrap();
        write_legacy_v191_plan(&first_plan_path, &first_plan);

        let raw_first_plan = fs::read_to_string(&first_plan_path).unwrap();
        assert!(!raw_first_plan.contains("expectedPayloadManifestSha256"));
        assert!(raw_first_plan.contains("\"relaunchViaShell\": false"));
        let exact_helper_args = [
            "apply".to_string(),
            "--plan".to_string(),
            first_plan_path.to_string_lossy().into_owned(),
        ];
        assert_eq!(exact_helper_args[0], "apply");
        assert_eq!(exact_helper_args[1], "--plan");

        let ready_path = temp.path().join("legacy-prepublish-ready");
        let test_binary = std::env::current_exe().unwrap();
        let mut first_helper = Command::new(&test_binary)
            .args([
                "--exact",
                "updater::tests::linux_legacy_prepublish_child",
                "--nocapture",
            ])
            .env("VFL_LEGACY_PREPUBLISH_CHILD", "1")
            .env("VFL_TEST_PLAN", &first_plan_path)
            .env("VFL_TEST_PREPUBLISH_READY", &ready_path)
            .spawn()
            .unwrap();
        wait_for_test_marker(&ready_path, &mut first_helper);

        let migrated: UpdateApplyPlan =
            read_atomic_json(&first_plan_path, "migrated test plan").unwrap();
        assert!(migrated.expected_payload_manifest_sha256.is_some());
        let interrupted: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "interrupted journal").unwrap();
        assert_eq!(interrupted.phase, UpdateJournalPhase::Replacing);
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            OLD_RUNNABLE_APP
        );
        assert_runnable_app_reports(&install_dir, temp.path(), "before-kill", b"old");

        let killed_pid = first_helper.id();
        first_helper.kill().unwrap();
        first_helper.wait().unwrap();
        assert!(!process_is_running(killed_pid).unwrap());
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            OLD_RUNNABLE_APP
        );
        assert_runnable_app_reports(&install_dir, temp.path(), "after-kill", b"old");

        // This is what the restored v1.9.1 app can do: stage a fresh legacy
        // plan and launch the new helper with `apply --plan <path>`. It does
        // not understand the prior journal or stale apply lock.
        let retry_plan = create_legacy_retry_fixture(&first_plan, "legacy-retry");
        let retry_plan_path = apply_plan_path(&retry_plan);
        let retry_manifest = test_verified_update_manifest_for_plan(&retry_plan);
        apply_update_plan_inner_with_options(
            &retry_plan_path,
            false,
            ApplyFault::default(),
            Some(&retry_manifest),
        )
        .unwrap();

        let recovered: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "recovered journal").unwrap();
        assert_eq!(recovered.update_id, first_plan.update_id);
        assert_eq!(recovered.phase, UpdateJournalPhase::RolledBack);
        assert!(!apply_lock_path(&install_dir).exists());
        assert_runnable_app_reports(&install_dir, temp.path(), "after-recovery", b"old");
        let still_legacy_retry = fs::read_to_string(&retry_plan_path).unwrap();
        assert!(!still_legacy_retry.contains("expectedPayloadManifestSha256"));

        // The next invocation cleans the safely rolled-back transaction,
        // migrates the exact legacy retry plan, and installs it normally.
        apply_update_plan_inner_with_options(
            &retry_plan_path,
            false,
            ApplyFault::default(),
            Some(&retry_manifest),
        )
        .unwrap();
        assert_runnable_app_reports(&install_dir, temp.path(), "after-retry", b"new");
        let finished: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "finished retry journal").unwrap();
        assert_eq!(finished.update_id, retry_plan.update_id);
        assert_eq!(finished.phase, UpdateJournalPhase::AwaitingStartup);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_legacy_prepublish_child() {
        if std::env::var("VFL_LEGACY_PREPUBLISH_CHILD").ok().as_deref() != Some("1") {
            return;
        }
        let plan_path = PathBuf::from(std::env::var_os("VFL_TEST_PLAN").unwrap());
        let legacy_plan: UpdateApplyPlan =
            read_atomic_json(&plan_path, "legacy child plan").unwrap();
        assert_eq!(legacy_plan.from_version, "1.9.1");
        assert!(legacy_plan.expected_payload_manifest_sha256.is_none());
        let verified_manifest = test_verified_update_manifest_for_plan(&legacy_plan);
        apply_update_plan_inner_with_options(
            &plan_path,
            false,
            ApplyFault {
                pause_before_main_publication: true,
                ..ApplyFault::default()
            },
            Some(&verified_manifest),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_recovery_process_restores_after_main_publication() {
        let (temp, install_dir, plan, plan_path) =
            create_apply_fixture("sigkill", "1.1.0", "1.1.1");
        let ready_path = temp.path().join("apply-ready");
        let test_binary = std::env::current_exe().unwrap();
        let mut apply_child = Command::new(&test_binary)
            .args([
                "--exact",
                "updater::tests::linux_apply_hard_kill_child",
                "--nocapture",
            ])
            .env("VFL_APPLY_KILL_CHILD", "1")
            .env("VFL_TEST_PLAN", &plan_path)
            .env("VFL_TEST_READY", &ready_path)
            .spawn()
            .unwrap();
        wait_for_test_marker(&ready_path, &mut apply_child);
        let killed_pid = apply_child.id();
        apply_child.kill().unwrap();
        apply_child.wait().unwrap();
        assert!(!process_is_running(killed_pid).unwrap());
        assert!(apply_lock_path(&install_dir).exists());
        let interrupted: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(interrupted.phase, UpdateJournalPhase::Replacing);
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"new"
        );

        let claim = acquire_recovery_staging_lock(&plan).unwrap();
        let mut recovery_journal = interrupted;
        recovery_journal.transition(
            UpdateJournalPhase::RollingBack,
            Some("automatic-recovery-requested"),
        );
        write_update_journal(&recovery_journal).unwrap();
        let relaunch_marker = temp.path().join("restored-app-relaunched");
        let mut dummy_parent = Command::new(&test_binary)
            .args([
                "--exact",
                "updater::tests::linux_recovery_dummy_parent_child",
                "--nocapture",
            ])
            .env("VFL_RECOVERY_DUMMY_PARENT", "1")
            .spawn()
            .unwrap();
        let mut recovery_child = Command::new(&test_binary)
            .args([
                "--exact",
                "updater::tests::linux_recovery_process_child",
                "--nocapture",
            ])
            .env("VFL_RECOVERY_CHILD", "1")
            .env("VFL_TEST_PLAN", &plan_path)
            .env("VFL_TEST_PARENT_PID", dummy_parent.id().to_string())
            .env("VFL_TEST_RELAUNCH_MARKER", &relaunch_marker)
            .spawn()
            .unwrap();
        claim.handoff_to_helper(&plan, recovery_child.id()).unwrap();
        std::thread::sleep(Duration::from_millis(150));
        assert!(dummy_parent.try_wait().unwrap().is_none());
        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"new"
        );
        dummy_parent.kill().unwrap();
        dummy_parent.wait().unwrap();
        let status = recovery_child.wait().unwrap();
        assert!(status.success());
        wait_for_marker_without_child(&relaunch_marker);

        assert_eq!(
            fs::read(install_dir.join(main_executable_name().unwrap())).unwrap(),
            b"old"
        );
        assert_eq!(
            fs::read(install_dir.join("user-note.txt")).unwrap(),
            b"keep"
        );
        let recovered: UpdateJournal =
            read_atomic_json(&update_journal_path(&install_dir), "update journal").unwrap();
        assert_eq!(recovered.phase, UpdateJournalPhase::RolledBack);
        assert!(plan.backup_dir.exists());
        cleanup_update_artifacts(&plan, true).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_apply_hard_kill_child() {
        if std::env::var("VFL_APPLY_KILL_CHILD").ok().as_deref() != Some("1") {
            return;
        }
        let plan_path = PathBuf::from(std::env::var_os("VFL_TEST_PLAN").unwrap());
        let ready_path = PathBuf::from(std::env::var_os("VFL_TEST_READY").unwrap());
        let _apply_lock = prepare_replacing_state_for_test(&plan_path);
        let mut ready = File::create(ready_path).unwrap();
        ready.write_all(b"ready").unwrap();
        ready.sync_all().unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_recovery_process_child() {
        if std::env::var("VFL_RECOVERY_CHILD").ok().as_deref() != Some("1") {
            return;
        }
        let plan_path = PathBuf::from(std::env::var_os("VFL_TEST_PLAN").unwrap());
        let parent_pid = std::env::var("VFL_TEST_PARENT_PID")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        recover_update_plan(&plan_path, parent_pid, true).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_recovery_dummy_parent_child() {
        if std::env::var("VFL_RECOVERY_DUMMY_PARENT").ok().as_deref() != Some("1") {
            return;
        }
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_relaunch_marker_child() {
        if std::env::var("VFL_RELAUNCH_MARKER_CHILD").ok().as_deref() != Some("1") {
            return;
        }
        let marker = PathBuf::from(std::env::var_os("VFL_TEST_RELAUNCH_MARKER").unwrap());
        let mut file = File::create(marker).unwrap();
        file.write_all(b"relaunched").unwrap();
        file.sync_all().unwrap();
    }

    fn prepare_replacing_state_for_test(plan_path: &Path) -> ApplyLockGuard {
        let plan: UpdateApplyPlan = read_atomic_json(plan_path, "test update plan").unwrap();
        let apply_lock = acquire_apply_lock(&plan).unwrap();
        let mut journal = load_matching_update_journal(&plan).unwrap();
        verify_staged_payload(&plan).unwrap();
        let old_manifest = read_payload_manifest(&plan.install_dir).unwrap();
        validate_payload_manifest(
            &plan.install_dir,
            &old_manifest,
            &plan.from_version,
            &plan.target,
            false,
        )
        .unwrap();
        let mut old_paths = manifest_owned_paths(&old_manifest);
        old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));
        journal.transition(UpdateJournalPhase::BackingUp, None);
        write_update_journal(&journal).unwrap();
        let backup_payload_manifest_sha256 =
            backup_existing_owned_files(&plan, &old_paths).unwrap();
        journal.backup_payload_manifest_sha256 = Some(backup_payload_manifest_sha256);
        journal.transition(UpdateJournalPhase::BackupComplete, None);
        write_update_journal(&journal).unwrap();
        journal.transition(UpdateJournalPhase::Replacing, None);
        write_update_journal(&journal).unwrap();
        let main = PathBuf::from(main_executable_name().unwrap());
        remove_file_with_retries(&plan.install_dir.join(&main)).unwrap();
        retry_copy_file(&plan.stage_dir.join(&main), &plan.install_dir.join(&main)).unwrap();
        apply_lock
    }

    #[cfg(target_os = "linux")]
    fn wait_for_test_marker(path: &Path, child: &mut std::process::Child) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            if let Some(status) = child.try_wait().unwrap() {
                panic!("apply crash child exited before ready marker: {status}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        panic!("apply crash child did not reach Replacing before timeout");
    }

    #[cfg(target_os = "linux")]
    fn wait_for_marker_without_child(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("restored app relaunch marker did not appear before timeout");
    }

    #[cfg(target_os = "linux")]
    const OLD_RUNNABLE_APP: &[u8] = b"#!/bin/sh\nprintf old > \"$1\"\n";
    #[cfg(target_os = "linux")]
    const NEW_RUNNABLE_APP: &[u8] = b"#!/bin/sh\nprintf new > \"$1\"\n";

    #[cfg(target_os = "linux")]
    fn configure_runnable_main_scripts(plan: &UpdateApplyPlan) {
        rewrite_payload_entry(
            &plan.install_dir,
            main_executable_name().unwrap(),
            OLD_RUNNABLE_APP,
            Some(0o755),
        );
        rewrite_payload_entry(
            &plan.stage_dir,
            main_executable_name().unwrap(),
            NEW_RUNNABLE_APP,
            Some(0o755),
        );
    }

    #[cfg(target_os = "linux")]
    fn rewrite_payload_entry(root: &Path, relative_path: &str, bytes: &[u8], mode: Option<u32>) {
        write_payload_file(root, relative_path, bytes, mode);
        let mut manifest = read_payload_manifest(root).unwrap();
        let entry = manifest
            .files
            .iter_mut()
            .find(|entry| entry.path == relative_path)
            .unwrap();
        entry.sha256 = sha256_file(&root.join(relative_path)).unwrap();
        entry.size_bytes = bytes.len() as u64;
        entry.mode = mode;
        fs::write(
            root.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    fn assert_runnable_app_reports(
        install_dir: &Path,
        marker_root: &Path,
        label: &str,
        expected: &[u8],
    ) {
        let marker = marker_root.join(format!("runnable-{label}"));
        let status = Command::new(install_dir.join(main_executable_name().unwrap()))
            .arg(&marker)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read(marker).unwrap(), expected);
    }

    #[cfg(target_os = "linux")]
    fn write_legacy_v191_plan(path: &Path, plan: &UpdateApplyPlan) {
        let exact_v191 = serde_json::json!({
            "schema": plan.schema,
            "updateId": plan.update_id,
            "fromVersion": plan.from_version,
            "toVersion": plan.to_version,
            "target": plan.target,
            "installDir": plan.install_dir,
            "stageDir": plan.stage_dir,
            "backupDir": plan.backup_dir,
            "parentPid": plan.parent_pid,
            "executableName": plan.executable_name,
            "helperName": plan.helper_name,
            "relaunchViaShell": plan.relaunch_via_shell,
        });
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, serde_json::to_vec_pretty(&exact_v191).unwrap()).unwrap();
        let parsed: UpdateApplyPlan = serde_json::from_value(exact_v191).unwrap();
        assert!(parsed.expected_payload_manifest_sha256.is_none());
        assert!(!parsed.relaunch_via_shell);
    }

    #[cfg(target_os = "linux")]
    fn create_legacy_retry_fixture(first_plan: &UpdateApplyPlan, label: &str) -> UpdateApplyPlan {
        let update_id = format!("test-{label}");
        let stage_dir = first_plan
            .install_dir
            .join(UPDATE_STATE_DIR)
            .join("staged")
            .join(&update_id)
            .join(PORTABLE_ROOT_DIR_NAME);
        let backup_dir = first_plan
            .install_dir
            .join(UPDATE_STATE_DIR)
            .join("backups")
            .join(&update_id);
        fs::create_dir_all(&stage_dir).unwrap();
        write_minimal_payload(&stage_dir, "new", Some(("new-owned.txt", b"add")));
        let manifest = manifest_from_dir(&stage_dir, "1.10.0", current_target().unwrap());
        fs::write(
            stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let plan = UpdateApplyPlan {
            schema: APPLY_PLAN_SCHEMA.to_string(),
            update_id,
            from_version: "1.9.1".to_string(),
            to_version: "1.10.0".to_string(),
            target: current_target().unwrap().to_string(),
            install_dir: first_plan.install_dir.clone(),
            stage_dir,
            backup_dir,
            parent_pid: 0,
            executable_name: main_executable_name().unwrap().to_string(),
            helper_name: helper_executable_name().unwrap().to_string(),
            expected_payload_manifest_sha256: None,
            relaunch_via_shell: false,
        };
        configure_retry_runnable_main(&plan);
        write_legacy_v191_plan(&apply_plan_path(&plan), &plan);
        plan
    }

    #[cfg(target_os = "linux")]
    fn configure_retry_runnable_main(plan: &UpdateApplyPlan) {
        rewrite_payload_entry(
            &plan.stage_dir,
            main_executable_name().unwrap(),
            NEW_RUNNABLE_APP,
            Some(0o755),
        );
    }

    #[cfg(target_os = "linux")]
    fn test_verified_update_manifest_for_plan(plan: &UpdateApplyPlan) -> UpdateManifest {
        let payload_digest = sha256_file(&plan.stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME)).unwrap();
        let file_name = format!("Video_For_Lazies-v{}-test.zip", plan.to_version);
        let release_url = format!(
            "https://github.com/Setmaster/Video_For_Lazies/releases/tag/v{}",
            plan.to_version
        );
        UpdateManifest {
            schema: UPDATE_MANIFEST_SCHEMA.to_string(),
            app_id: APP_ID.to_string(),
            channel: UPDATE_CHANNEL.to_string(),
            version: plan.to_version.clone(),
            release_tag: format!("v{}", plan.to_version),
            release_url: release_url.clone(),
            published_at: "2026-07-11T00:00:00Z".to_string(),
            min_updater_protocol: UPDATE_PROTOCOL_VERSION,
            notes: UpdateNotes {
                title: "Test update".to_string(),
                summary: "Test update".to_string(),
                url: release_url,
            },
            artifacts: HashMap::from([(
                plan.target.clone(),
                UpdateArtifact {
                    file_name: file_name.clone(),
                    url: format!(
                        "https://github.com/Setmaster/Video_For_Lazies/releases/download/v{}/{}",
                        plan.to_version, file_name
                    ),
                    sha256: "a".repeat(64),
                    size_bytes: 1,
                    root_dir: PORTABLE_ROOT_DIR_NAME.to_string(),
                    payload_manifest: UpdatePayloadReference {
                        path: format!("{PORTABLE_ROOT_DIR_NAME}/{PAYLOAD_MANIFEST_FILE_NAME}"),
                        sha256: payload_digest,
                    },
                },
            )]),
        }
    }

    fn create_apply_fixture(
        label: &str,
        from_version: &str,
        to_version: &str,
    ) -> (tempfile::TempDir, PathBuf, UpdateApplyPlan, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("Video_For_Lazies");
        let update_id = format!("test-{label}");
        let stage_dir = install_dir
            .join(UPDATE_STATE_DIR)
            .join("staged")
            .join(&update_id)
            .join(PORTABLE_ROOT_DIR_NAME);
        let backup_dir = install_dir
            .join(UPDATE_STATE_DIR)
            .join("backups")
            .join(&update_id);
        fs::create_dir_all(&install_dir).unwrap();
        fs::create_dir_all(&stage_dir).unwrap();

        write_minimal_payload(&install_dir, "old", Some(("obsolete-owned.txt", b"remove")));
        let old_manifest = manifest_from_dir(&install_dir, from_version, current_target().unwrap());
        fs::write(
            install_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&old_manifest).unwrap(),
        )
        .unwrap();
        write_payload_file(&install_dir, "user-note.txt", b"keep", platform_mode(0o644));
        let mut staging_claim = acquire_staging_lock(&install_dir).unwrap();

        write_minimal_payload(&stage_dir, "new", Some(("new-owned.txt", b"add")));
        let new_manifest = manifest_from_dir(&stage_dir, to_version, current_target().unwrap());
        fs::write(
            stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&new_manifest).unwrap(),
        )
        .unwrap();
        let expected_payload_manifest_sha256 =
            sha256_file(&stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME)).unwrap();

        let plan = UpdateApplyPlan {
            schema: APPLY_PLAN_SCHEMA.to_string(),
            update_id,
            from_version: from_version.to_string(),
            to_version: to_version.to_string(),
            target: current_target().unwrap().to_string(),
            install_dir: install_dir.clone(),
            stage_dir,
            backup_dir,
            parent_pid: 0,
            executable_name: main_executable_name().unwrap().to_string(),
            helper_name: helper_executable_name().unwrap().to_string(),
            expected_payload_manifest_sha256: Some(expected_payload_manifest_sha256),
            relaunch_via_shell: false,
        };
        let plan_path = apply_plan_path(&plan);
        atomic_write_json(&plan_path, &plan, "test update plan").unwrap();
        write_update_journal(&UpdateJournal::new(&plan, UpdateJournalPhase::Staged)).unwrap();
        staging_claim.prepare_handoff(&plan).unwrap();
        staging_claim.handoff();

        (temp, install_dir, plan, plan_path)
    }

    fn write_payload_file(root: &Path, relative_path: &str, bytes: &[u8], mode: Option<u32>) {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut file = File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let _ = mode;
        }
    }

    fn write_minimal_payload(root: &Path, marker: &str, extra: Option<(&str, &[u8])>) {
        let target = current_target().unwrap();
        write_payload_file(
            root,
            main_executable_name().unwrap(),
            marker.as_bytes(),
            platform_mode(0o755),
        );
        write_payload_file(
            root,
            helper_executable_name().unwrap(),
            marker.as_bytes(),
            platform_mode(0o755),
        );
        write_payload_file(root, "README.md", marker.as_bytes(), platform_mode(0o644));
        write_payload_file(root, "LICENSE.txt", marker.as_bytes(), platform_mode(0o644));
        write_payload_file(
            root,
            "THIRD_PARTY_NOTICES.md",
            marker.as_bytes(),
            platform_mode(0o644),
        );
        write_payload_file(root, "SOURCE.md", marker.as_bytes(), platform_mode(0o644));
        write_payload_file(
            root,
            "FFMPEG_BUNDLING.md",
            marker.as_bytes(),
            platform_mode(0o644),
        );
        write_payload_file(
            root,
            "ffmpeg-sidecar/LICENSE.txt",
            marker.as_bytes(),
            platform_mode(0o644),
        );
        write_payload_file(
            root,
            "ffmpeg-sidecar/FFMPEG_BUNDLE_NOTICES.txt",
            marker.as_bytes(),
            platform_mode(0o644),
        );
        if target == "linux-x64" {
            write_payload_file(
                root,
                "Video_For_Lazies.png",
                marker.as_bytes(),
                platform_mode(0o644),
            );
            write_payload_file(
                root,
                "Video_For_Lazies.desktop",
                marker.as_bytes(),
                platform_mode(0o755),
            );
        }
        if let Some((relative_path, bytes)) = extra {
            write_payload_file(root, relative_path, bytes, platform_mode(0o644));
        }
    }

    fn platform_mode(mode: u32) -> Option<u32> {
        if current_target().unwrap() == "linux-x64" {
            Some(mode)
        } else {
            None
        }
    }

    fn manifest_from_dir(root: &Path, version: &str, target: &str) -> PayloadManifest {
        let files = walk_regular_files(root)
            .unwrap()
            .into_iter()
            .filter(|path| path != Path::new(PAYLOAD_MANIFEST_FILE_NAME))
            .map(|path| {
                let absolute_path = root.join(&path);
                let meta = fs::metadata(&absolute_path).unwrap();
                PayloadFile {
                    path: path.to_string_lossy().replace('\\', "/"),
                    sha256: sha256_file(&absolute_path).unwrap(),
                    size_bytes: meta.len(),
                    mode: if target == "linux-x64" {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            Some(meta.permissions().mode() & 0o777)
                        }
                        #[cfg(not(unix))]
                        {
                            Some(0o644)
                        }
                    } else {
                        None
                    },
                    kind: "file".to_string(),
                }
            })
            .collect();
        PayloadManifest {
            schema: PAYLOAD_MANIFEST_SCHEMA.to_string(),
            app_id: APP_ID.to_string(),
            version: version.to_string(),
            target: target.to_string(),
            root_dir: PORTABLE_ROOT_DIR_NAME.to_string(),
            files,
        }
    }
}
