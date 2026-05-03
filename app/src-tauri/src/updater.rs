use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};

const APP_ID: &str = "com.setmaster.video-for-lazies";
const UPDATE_CHANNEL: &str = "stable";
const UPDATE_PROTOCOL_VERSION: u64 = 1;
const UPDATE_MANIFEST_SCHEMA: &str = "com.setmaster.video-for-lazies.update-manifest.v1";
const PAYLOAD_MANIFEST_SCHEMA: &str = "com.setmaster.video-for-lazies.payload-manifest.v1";
const APPLY_PLAN_SCHEMA: &str = "com.setmaster.video-for-lazies.apply-plan.v1";
const UPDATE_PREFS_SCHEMA: &str = "com.setmaster.video-for-lazies.update-prefs.v1";
const UPDATE_STATE_DIR: &str = ".vfl-updates";
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
const DEFAULT_SIGNATURE_SUFFIX: &str = ".sig";
const UPDATE_PUBLIC_KEYS: &[&str] = &[
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEQ3ODU2M0VEQUIwRkNEQTYKUldTbXpRK3I3V09GMTdIUnBHaDlkMzBEN0FRYnJyTUVEa2FRN0Q0Ylh4RDMxT09hYy9vR3hEd2IK",
];

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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PayloadManifest {
    schema: String,
    app_id: String,
    version: String,
    target: String,
    root_dir: String,
    files: Vec<PayloadFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PayloadFile {
    path: String,
    sha256: String,
    size_bytes: u64,
    mode: Option<u32>,
    kind: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptChoice {
    RemindLater,
    Skip7Days,
    Dismiss,
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
pub fn check_for_update(
    app: AppHandle,
    force: Option<bool>,
) -> Result<UpdateCheckResponse, String> {
    let force = force.unwrap_or(false);
    check_for_update_inner(&app, force)
}

#[tauri::command]
pub fn record_update_prompt_choice(
    app: AppHandle,
    choice: String,
    version: String,
) -> Result<(), String> {
    let choice = PromptChoice::parse(&choice)?;
    let version = parse_semver(&version)?;
    let now = now_ms();
    let mut prefs = load_prefs(&app)?;

    match choice {
        PromptChoice::RemindLater => {
            prefs.remind_later_version = Some(version.to_string());
        }
        PromptChoice::Skip7Days => {
            prefs.suppress_prompts_until_ms = Some(now.saturating_add(SKIP_INTERVAL_MS));
            prefs.remind_later_version = None;
        }
        PromptChoice::Dismiss => {
            prefs.remind_later_version = None;
        }
    }

    save_prefs(&app, &prefs)
}

#[tauri::command]
pub fn prepare_and_apply_update(app: AppHandle) -> Result<UpdateApplyResponse, String> {
    let check = check_for_update_inner(&app, true)?;
    if check.status != "available" {
        return Err(check
            .reason
            .unwrap_or_else(|| "No update is available.".to_string()));
    }

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

    let plan = stage_update(&app, &manifest, &artifact, target)?;
    let helper_path = copy_staged_helper_to_temp(&plan)?;
    launch_update_helper(&helper_path, &plan)?;

    let version = plan.to_version.clone();
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

#[tauri::command]
pub fn finalize_update_startup(app: AppHandle) -> Result<(), String> {
    let install_dir = install_dir()?;
    let pending_path = install_dir
        .join(UPDATE_STATE_DIR)
        .join("pending-success.json");
    if !pending_path.exists() {
        cleanup_old_temp_helpers();
        return Ok(());
    }

    let raw =
        fs::read(&pending_path).map_err(|e| format!("Failed to read pending update state: {e}"))?;
    let plan: UpdateApplyPlan = serde_json::from_slice(&raw)
        .map_err(|e| format!("Failed to parse pending update state: {e}"))?;
    if plan.to_version == current_version() {
        let _ = fs::remove_dir_all(&plan.backup_dir);
        let _ = fs::remove_dir_all(plan.install_dir.join(UPDATE_STATE_DIR).join("staged"));
        let _ = fs::remove_file(&pending_path);
        let mut prefs = load_prefs(&app)?;
        prefs.remind_later_version = None;
        set_highest_trusted_version(
            &mut prefs,
            &Version::parse(&plan.to_version)
                .map_err(|e| format!("Pending update version is invalid: {e}"))?,
        );
        save_prefs(&app, &prefs)?;
    }
    cleanup_old_temp_helpers();
    Ok(())
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
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("{err}");
                    std::process::ExitCode::from(1)
                }
            }
        }
        _ => {
            eprintln!("usage: vfl-update-helper [--version|--self-test|apply --plan <path>]");
            std::process::ExitCode::from(2)
        }
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

    prefs.last_checked_at_ms = Some(now);
    set_highest_trusted_version(&mut prefs, &latest);
    save_prefs(app, &prefs)?;

    if latest <= current {
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

fn fetch_verified_update_manifest() -> Result<VerifiedManifest, String> {
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
    if artifact.size_bytes == 0 || artifact.size_bytes > MAX_UPDATE_BYTES {
        return Err("Update artifact size is outside the supported range.".to_string());
    }
    validate_sha256(&artifact.sha256, "artifact SHA256")?;
    validate_download_url(&artifact.url, update_local_http_allowed())?;
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
) -> Result<UpdateApplyPlan, String> {
    let install_dir = install_dir()?;
    let update_id = format!("{}-{}", manifest.version, now_ms());
    let updates_dir = install_dir.join(UPDATE_STATE_DIR);
    let stage_root = updates_dir.join("staged").join(&update_id);
    let extract_dir = stage_root.join("extract");
    let zip_path = stage_root.join(&artifact.file_name);
    let backup_dir = updates_dir.join("backups").join(&update_id);

    fs::create_dir_all(&stage_root).map_err(|e| format!("Failed to create update stage: {e}"))?;
    fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Failed to create update extraction directory: {e}"))?;
    fs::create_dir_all(&backup_dir).map_err(|e| format!("Failed to create update backup: {e}"))?;

    download_file(&artifact.url, &zip_path, artifact.size_bytes)?;
    let actual_hash = sha256_file(&zip_path)?;
    if actual_hash != artifact.sha256 {
        return Err(
            "Downloaded update archive hash does not match the signed manifest.".to_string(),
        );
    }

    extract_update_zip(&zip_path, &extract_dir)?;
    let portable_dir = extract_dir.join(PORTABLE_ROOT_DIR_NAME);
    let payload = read_payload_manifest(&portable_dir)?;
    validate_payload_manifest(&portable_dir, &payload, &manifest.version, target, true)?;
    let payload_hash = sha256_file(&portable_dir.join(PAYLOAD_MANIFEST_FILE_NAME))?;
    if payload_hash != artifact.payload_manifest.sha256 {
        return Err("Payload manifest hash does not match the signed update manifest.".to_string());
    }

    let plan = UpdateApplyPlan {
        schema: APPLY_PLAN_SCHEMA.to_string(),
        update_id,
        from_version: current_version().to_string(),
        to_version: manifest.version.clone(),
        target: target.to_string(),
        install_dir,
        stage_dir: portable_dir,
        backup_dir,
        parent_pid: std::process::id(),
        executable_name: main_executable_name()?.to_string(),
        helper_name: helper_executable_name()?.to_string(),
    };
    validate_apply_plan(&plan)?;

    let plan_path = apply_plan_path(&plan);
    if let Some(parent) = plan_path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create update plan dir: {e}"))?;
    }
    fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&plan)
            .map_err(|e| format!("Failed to serialize update plan: {e}"))?,
    )
    .map_err(|e| format!("Failed to write update plan: {e}"))?;

    let mut prefs = load_prefs(app)?;
    set_highest_trusted_version(&mut prefs, &parse_semver(&manifest.version)?);
    save_prefs(app, &prefs)?;

    Ok(plan)
}

fn copy_staged_helper_to_temp(plan: &UpdateApplyPlan) -> Result<PathBuf, String> {
    let helper_source = plan.stage_dir.join(&plan.helper_name);
    let helper_target = std::env::temp_dir().join(format!(
        "vfl-update-helper-{}{}",
        plan.update_id,
        executable_suffix()
    ));
    retry_copy_file(&helper_source, &helper_target)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&helper_target, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to make update helper executable: {e}"))?;
    }
    Ok(helper_target)
}

fn launch_update_helper(helper_path: &Path, plan: &UpdateApplyPlan) -> Result<(), String> {
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

    command
        .spawn()
        .map_err(|e| format!("Failed to launch update helper: {e}"))?;
    Ok(())
}

fn apply_update_plan(plan_path: &Path) -> Result<(), String> {
    apply_update_plan_inner(plan_path, true)
}

fn apply_update_plan_inner(plan_path: &Path, relaunch: bool) -> Result<(), String> {
    let raw = fs::read(plan_path).map_err(|e| format!("Failed to read update plan: {e}"))?;
    let plan: UpdateApplyPlan =
        serde_json::from_slice(&raw).map_err(|e| format!("Failed to parse update plan: {e}"))?;
    validate_apply_plan(&plan)?;
    wait_for_parent_to_exit(plan.parent_pid);

    let new_manifest = read_payload_manifest(&plan.stage_dir)?;
    validate_payload_manifest(
        &plan.stage_dir,
        &new_manifest,
        &plan.to_version,
        &plan.target,
        true,
    )?;
    let old_manifest = read_payload_manifest(&plan.install_dir).ok();

    let mut old_paths = old_manifest
        .as_ref()
        .map(manifest_owned_paths)
        .unwrap_or_default();
    old_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));

    let mut new_paths = manifest_owned_paths(&new_manifest);
    new_paths.insert(PathBuf::from(PAYLOAD_MANIFEST_FILE_NAME));

    backup_existing_owned_files(&plan, &old_paths, &new_paths)?;
    match replace_owned_files(&plan, &old_paths, &new_paths, &new_manifest) {
        Ok(()) => {
            let pending_path = plan
                .install_dir
                .join(UPDATE_STATE_DIR)
                .join("pending-success.json");
            fs::write(
                &pending_path,
                serde_json::to_vec_pretty(&plan)
                    .map_err(|e| format!("Failed to serialize update success state: {e}"))?,
            )
            .map_err(|e| format!("Failed to write update success state: {e}"))?;
            if relaunch {
                launch_updated_app(&plan)?;
            }
            Ok(())
        }
        Err(err) => {
            let _ = restore_backup(&plan, &old_paths, &new_paths);
            Err(err)
        }
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
    if !plan
        .stage_dir
        .starts_with(plan.install_dir.join(UPDATE_STATE_DIR).join("staged"))
    {
        return Err("Update stage directory is outside the update state folder.".to_string());
    }
    if !plan
        .backup_dir
        .starts_with(plan.install_dir.join(UPDATE_STATE_DIR).join("backups"))
    {
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
    Ok(())
}

fn backup_existing_owned_files(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    let all_paths = old_paths.union(new_paths).cloned().collect::<BTreeSet<_>>();
    fs::create_dir_all(&plan.backup_dir)
        .map_err(|e| format!("Failed to create update backup: {e}"))?;
    for relative_path in all_paths {
        let source = plan.install_dir.join(&relative_path);
        if !source.exists() {
            continue;
        }
        let backup_path = plan.backup_dir.join(&relative_path);
        if let Some(parent) = backup_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update backup directory: {e}"))?;
        }
        retry_copy_file(&source, &backup_path)?;
    }
    Ok(())
}

fn replace_owned_files(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
    new_manifest: &PayloadManifest,
) -> Result<(), String> {
    for relative_path in old_paths.difference(new_paths) {
        remove_file_with_retries(&plan.install_dir.join(relative_path))?;
    }

    let entries = new_manifest
        .files
        .iter()
        .map(|entry| {
            let path = payload_path_buf(&entry.path)?;
            Ok((path, entry.mode))
        })
        .collect::<Result<HashMap<_, _>, String>>()?;

    for relative_path in new_paths {
        let source = plan.stage_dir.join(relative_path);
        let target = plan.install_dir.join(relative_path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create update target directory: {e}"))?;
        }
        remove_file_with_retries(&target)?;
        retry_copy_file(&source, &target)?;
        set_mode_if_needed(&target, entries.get(relative_path).copied().flatten())?;
    }

    Ok(())
}

fn restore_backup(
    plan: &UpdateApplyPlan,
    old_paths: &BTreeSet<PathBuf>,
    new_paths: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    for relative_path in new_paths.difference(old_paths) {
        let _ = remove_file_with_retries(&plan.install_dir.join(relative_path));
    }
    for relative_path in old_paths {
        let backup_path = plan.backup_dir.join(relative_path);
        let target = plan.install_dir.join(relative_path);
        if !backup_path.exists() {
            let _ = remove_file_with_retries(&target);
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create rollback directory: {e}"))?;
        }
        let _ = remove_file_with_retries(&target);
        retry_copy_file(&backup_path, &target)?;
    }
    Ok(())
}

fn launch_updated_app(plan: &UpdateApplyPlan) -> Result<(), String> {
    let executable = plan.install_dir.join(&plan.executable_name);
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

fn wait_for_parent_to_exit(parent_pid: u32) {
    if parent_pid == 0 {
        return;
    }
    std::thread::sleep(Duration::from_millis(1200));
}

fn retry_copy_file(source: &Path, target: &Path) -> Result<(), String> {
    retry_io(
        || {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, target)?;
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

fn remove_file_with_retries(path: &Path) -> Result<(), String> {
    retry_io(
        || match fs::remove_file(path) {
            Ok(()) => Ok(()),
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

fn download_file(url: &str, output_path: &Path, expected_size: u64) -> Result<(), String> {
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
    let written = io::copy(
        &mut response.by_ref().take(expected_size.saturating_add(1)),
        &mut output,
    )
    .map_err(|e| format!("Failed to write update archive: {e}"))?;
    if written != expected_size {
        return Err("Update archive size does not match the signed manifest.".to_string());
    }
    output
        .flush()
        .map_err(|e| format!("Failed to flush update archive: {e}"))?;
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
    if !path.exists() {
        return Ok(UpdatePrefs {
            schema: Some(UPDATE_PREFS_SCHEMA.to_string()),
            ..UpdatePrefs::default()
        });
    }
    let raw = fs::read(&path).map_err(|e| format!("Failed to read update preferences: {e}"))?;
    let mut prefs: UpdatePrefs = serde_json::from_slice(&raw)
        .map_err(|e| format!("Failed to parse update preferences: {e}"))?;
    prefs.schema = Some(UPDATE_PREFS_SCHEMA.to_string());
    Ok(prefs)
}

fn save_prefs(app: &AppHandle, prefs: &UpdatePrefs) -> Result<(), String> {
    let path = prefs_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create update preference directory: {e}"))?;
    }
    let mut prefs = prefs.clone();
    prefs.schema = Some(UPDATE_PREFS_SCHEMA.to_string());
    fs::write(
        path,
        serde_json::to_vec_pretty(&prefs)
            .map_err(|e| format!("Failed to serialize update preferences: {e}"))?,
    )
    .map_err(|e| format!("Failed to write update preferences: {e}"))
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

fn cleanup_old_temp_helpers() {
    let Ok(entries) = fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !file_name.starts_with("vfl-update-helper-") {
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
        ] {
            assert!(payload_path_buf(path).is_err(), "{path}");
        }
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

        write_minimal_payload(&stage_dir, "new", Some(("new-owned.txt", b"add")));
        let new_manifest = manifest_from_dir(&stage_dir, "1.1.1", current_target().unwrap());
        fs::write(
            stage_dir.join(PAYLOAD_MANIFEST_FILE_NAME),
            serde_json::to_vec(&new_manifest).unwrap(),
        )
        .unwrap();

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
        };
        let plan_path = apply_plan_path(&plan);
        fs::create_dir_all(plan_path.parent().unwrap()).unwrap();
        fs::write(&plan_path, serde_json::to_vec(&plan).unwrap()).unwrap();

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
