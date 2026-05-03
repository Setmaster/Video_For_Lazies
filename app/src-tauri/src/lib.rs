use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager, State, Window};

mod video;

const SUPPORTED_PREVIEW_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v"];

struct JobHandle {
    job_id: u64,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

struct JobManager {
    next_job_id: AtomicU64,
    current: Mutex<Option<JobHandle>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AppSmokeConfig {
    input_path: String,
    output_path: String,
    status_path: String,
    format: video::OutputFormat,
    size_limit_mb: f64,
    trim_start_s: f64,
    trim_end_s: Option<f64>,
    skip_preview_interactions: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct AppSmokeStatus {
    stage: String,
    ok: Option<bool>,
    message: Option<String>,
    output_path: Option<String>,
    output_size_bytes: Option<u64>,
    trim_start_s: Option<f64>,
    trim_end_s: Option<f64>,
    expected_duration_s: Option<f64>,
    #[serde(default)]
    stage_history: Vec<String>,
}

impl JobManager {
    fn new() -> Self {
        Self {
            next_job_id: AtomicU64::new(1),
            current: Mutex::new(None),
        }
    }

    fn alloc_job_id(&self) -> u64 {
        self.next_job_id.fetch_add(1, Ordering::Relaxed)
    }
}

fn parse_smoke_f64(
    env: &HashMap<String, String>,
    key: &str,
    default: Option<f64>,
) -> Result<Option<f64>, String> {
    let Some(raw) = env.get(key) else {
        return Ok(default);
    };

    let value = raw
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{key} must be a number."))?;
    Ok(Some(value))
}

fn parse_smoke_output_format(raw: &str) -> Result<video::OutputFormat, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "mp4" => Ok(video::OutputFormat::Mp4),
        "webm" => Ok(video::OutputFormat::Webm),
        "mp3" => Ok(video::OutputFormat::Mp3),
        _ => Err("VFL_SMOKE_FORMAT must be one of: mp4, webm, mp3.".to_string()),
    }
}

fn parse_smoke_bool(env: &HashMap<String, String>, key: &str) -> Result<bool, String> {
    let Some(raw) = env.get(key) else {
        return Ok(false);
    };

    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        _ => Err(format!("{key} must be a boolean value.")),
    }
}

fn validate_smoke_status_path(raw: &str) -> Result<String, String> {
    let path = PathBuf::from(raw);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "VFL_SMOKE_STATUS must include a folder.".to_string())?;
    if !parent.exists() {
        return Err("VFL_SMOKE_STATUS folder must already exist.".to_string());
    }

    let temp_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|e| format!("Failed to resolve system temporary directory: {e}"))?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("Failed to resolve VFL_SMOKE_STATUS folder: {e}"))?;

    if !parent.starts_with(&temp_root) {
        return Err(
            "VFL_SMOKE_STATUS must point inside the system temporary directory.".to_string(),
        );
    }

    Ok(path.to_string_lossy().to_string())
}

fn parse_smoke_config_from_env(
    env: &HashMap<String, String>,
) -> Result<Option<AppSmokeConfig>, String> {
    let Some(input_path) = env
        .get("VFL_SMOKE_INPUT")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let output_path = env
        .get("VFL_SMOKE_OUTPUT")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "VFL_SMOKE_OUTPUT is required when VFL_SMOKE_INPUT is set.".to_string())?;

    let status_path = env
        .get("VFL_SMOKE_STATUS")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "VFL_SMOKE_STATUS is required when VFL_SMOKE_INPUT is set.".to_string())?;
    let status_path = validate_smoke_status_path(status_path)?;

    let format = parse_smoke_output_format(
        env.get("VFL_SMOKE_FORMAT")
            .map(String::as_str)
            .unwrap_or("mp4"),
    )?;
    let size_limit_mb = parse_smoke_f64(env, "VFL_SMOKE_SIZE_LIMIT_MB", Some(0.0))?
        .ok_or_else(|| "VFL_SMOKE_SIZE_LIMIT_MB parsing failed.".to_string())?;
    let trim_start_s = parse_smoke_f64(env, "VFL_SMOKE_TRIM_START_S", Some(0.0))?
        .ok_or_else(|| "VFL_SMOKE_TRIM_START_S parsing failed.".to_string())?;
    let trim_end_s = parse_smoke_f64(env, "VFL_SMOKE_TRIM_END_S", None)?;
    let skip_preview_interactions = parse_smoke_bool(env, "VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS")?;

    if size_limit_mb < 0.0 {
        return Err("VFL_SMOKE_SIZE_LIMIT_MB must be >= 0.".to_string());
    }
    if trim_start_s < 0.0 {
        return Err("VFL_SMOKE_TRIM_START_S must be >= 0.".to_string());
    }
    if let Some(end_s) = trim_end_s {
        if end_s < 0.0 {
            return Err("VFL_SMOKE_TRIM_END_S must be >= 0.".to_string());
        }
        if end_s <= trim_start_s {
            return Err(
                "VFL_SMOKE_TRIM_END_S must be greater than VFL_SMOKE_TRIM_START_S.".to_string(),
            );
        }
    }

    Ok(Some(AppSmokeConfig {
        input_path: input_path.to_string(),
        output_path: output_path.to_string(),
        status_path,
        format,
        size_limit_mb,
        trim_start_s,
        trim_end_s,
        skip_preview_interactions,
    }))
}

fn write_smoke_status_file(path: &Path, status: &AppSmokeStatus) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create smoke status directory: {e}"))?;
    }

    let json = serde_json::to_vec_pretty(status)
        .map_err(|e| format!("Failed to serialize smoke status: {e}"))?;
    let tmp_path = path.with_extension("tmp");

    std::fs::write(&tmp_path, json).map_err(|e| format!("Failed to write smoke status: {e}"))?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("Failed to replace smoke status: {err}")),
    }
    std::fs::rename(&tmp_path, path).map_err(|e| format!("Failed to publish smoke status: {e}"))?;

    Ok(())
}

fn read_smoke_status_file(path: &Path) -> Option<AppSmokeStatus> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice::<AppSmokeStatus>(&raw).ok()
}

fn merge_smoke_stage_history(
    existing: Option<&AppSmokeStatus>,
    incoming_history: &[String],
    next_stage: &str,
) -> Vec<String> {
    let mut history = existing
        .map(|status| status.stage_history.clone())
        .unwrap_or_default();

    if history.is_empty()
        && let Some(previous_stage) = existing
            .map(|status| status.stage.trim())
            .filter(|stage| !stage.is_empty())
    {
        history.push(previous_stage.to_string());
    }

    for stage in incoming_history {
        if !history.iter().any(|existing_stage| existing_stage == stage) {
            history.push(stage.clone());
        }
    }

    if !history.iter().any(|stage| stage == next_stage) {
        history.push(next_stage.to_string());
    }

    history
}

fn merge_smoke_optional_fields(existing: Option<&AppSmokeStatus>, next: &mut AppSmokeStatus) {
    let Some(existing) = existing else {
        return;
    };

    if next.ok.is_none() {
        next.ok = existing.ok;
    }
    if next.message.is_none() {
        next.message = existing.message.clone();
    }
    if next.output_path.is_none() {
        next.output_path = existing.output_path.clone();
    }
    if next.output_size_bytes.is_none() {
        next.output_size_bytes = existing.output_size_bytes;
    }
    if next.trim_start_s.is_none() {
        next.trim_start_s = existing.trim_start_s;
    }
    if next.trim_end_s.is_none() {
        next.trim_end_s = existing.trim_end_s;
    }
    if next.expected_duration_s.is_none() {
        next.expected_duration_s = existing.expected_duration_s;
    }
}

#[tauri::command]
fn probe_video(path: String) -> Result<video::VideoProbe, String> {
    video::probe_video(path)
}

#[tauri::command]
fn detect_crop(path: String) -> Result<Option<video::Crop>, String> {
    video::detect_crop(path)
}

#[tauri::command]
fn suggest_output_path(input_path: String, format: video::OutputFormat) -> Result<String, String> {
    video::suggest_output_path_unique(input_path, format)
}

#[tauri::command]
#[allow(non_snake_case)]
fn extract_frame(inputPath: String, timeS: f64, outputPath: String) -> Result<(), String> {
    video::extract_frame(inputPath, timeS, outputPath)
}

#[tauri::command]
fn allow_preview_path(window: Window, path: String) -> Result<(), String> {
    let path_buf = PathBuf::from(&path);
    let meta =
        std::fs::metadata(&path_buf).map_err(|e| format!("Preview file is not accessible: {e}"))?;
    if !meta.is_file() {
        return Err("Preview path must point to a file.".to_string());
    }
    if !is_supported_preview_path(&path_buf) {
        return Err("Preview file must be one of: mp4, mov, mkv, avi, webm, m4v.".to_string());
    }

    let path_buf = path_buf
        .canonicalize()
        .map_err(|e| format!("Preview file is not accessible: {e}"))?;

    window
        .app_handle()
        .asset_protocol_scope()
        .allow_file(path_buf)
        .map_err(|e| format!("Failed to allow preview access: {e}"))
}

fn is_supported_preview_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            let extension = extension.to_ascii_lowercase();
            SUPPORTED_PREVIEW_EXTENSIONS.contains(&extension.as_str())
        })
        .unwrap_or(false)
}

#[tauri::command]
fn read_smoke_config() -> Result<Option<AppSmokeConfig>, String> {
    parse_smoke_config_from_env(&std::env::vars().collect())
}

#[tauri::command]
fn write_smoke_status(status: AppSmokeStatus) -> Result<(), String> {
    let Some(config) = parse_smoke_config_from_env(&std::env::vars().collect())? else {
        return Ok(());
    };

    let status_path = Path::new(&config.status_path);
    let existing_status = read_smoke_status_file(status_path);
    let mut next_status = status;
    merge_smoke_optional_fields(existing_status.as_ref(), &mut next_status);
    next_status.stage_history = merge_smoke_stage_history(
        existing_status.as_ref(),
        &next_status.stage_history,
        &next_status.stage,
    );

    write_smoke_status_file(status_path, &next_status)
}

#[tauri::command]
fn start_encode(
    window: Window,
    state: State<'_, JobManager>,
    request: video::EncodeRequest,
) -> Result<u64, String> {
    let mut current = state
        .current
        .lock()
        .map_err(|_| "Internal error (job lock poisoned).".to_string())?;

    if current.is_some() {
        return Err("An encode job is already running.".to_string());
    }

    let job_id = state.alloc_job_id();
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));

    *current = Some(JobHandle {
        job_id,
        cancel: cancel.clone(),
        child: child.clone(),
    });
    drop(current);

    let app_handle = window.app_handle().clone();

    std::thread::spawn(move || {
        let result = video::run_encode_job(&window, job_id, &cancel, &child, request);

        let _ = match result {
            Ok(done) => window.emit("encode-finished", done),
            Err(err) => window.emit(
                "encode-finished",
                video::EncodeFinishedPayload {
                    job_id,
                    ok: false,
                    output_path: None,
                    output_size_bytes: None,
                    message: Some(err),
                },
            ),
        };

        let manager: State<'_, JobManager> = app_handle.state();
        if let Ok(mut guard) = manager.current.lock()
            && guard.as_ref().is_some_and(|handle| handle.job_id == job_id)
        {
            *guard = None;
        }
    });

    Ok(job_id)
}

#[tauri::command]
#[allow(non_snake_case)]
fn cancel_encode(state: State<'_, JobManager>, jobId: u64) -> Result<(), String> {
    let current = state
        .current
        .lock()
        .map_err(|_| "Internal error (job lock poisoned).".to_string())?;

    let Some(handle) = current.as_ref() else {
        return Ok(());
    };

    if handle.job_id != jobId {
        return Ok(());
    }

    handle.cancel.store(true, Ordering::Relaxed);

    if let Ok(mut guard) = handle.child.lock()
        && let Some(child) = guard.as_mut()
    {
        let _ = child.kill();
    }

    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(JobManager::new())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            probe_video,
            detect_crop,
            suggest_output_path,
            extract_frame,
            allow_preview_path,
            read_smoke_config,
            write_smoke_status,
            start_encode,
            cancel_encode
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        AppSmokeConfig, AppSmokeStatus, is_supported_preview_path, merge_smoke_optional_fields,
        merge_smoke_stage_history, parse_smoke_config_from_env,
    };
    use crate::video::OutputFormat;
    use std::collections::HashMap;
    use std::fs;

    fn smoke_env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn temp_smoke_status_path(label: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "vfl_smoke_status_test_{}_{}",
            std::process::id(),
            label
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("status.json").to_string_lossy().to_string()
    }

    #[test]
    fn parse_smoke_config_returns_none_without_input() {
        let env = smoke_env(&[]);
        assert_eq!(parse_smoke_config_from_env(&env).unwrap(), None);
    }

    #[test]
    fn preview_path_support_is_extension_limited() {
        assert!(is_supported_preview_path(std::path::Path::new("clip.MP4")));
        assert!(is_supported_preview_path(std::path::Path::new("clip.webm")));
        assert!(!is_supported_preview_path(std::path::Path::new(
            "notes.txt"
        )));
        assert!(!is_supported_preview_path(std::path::Path::new("clip")));
    }

    #[test]
    fn parse_smoke_config_reads_defaults() {
        let status_path = temp_smoke_status_path("defaults");
        let env = smoke_env(&[
            ("VFL_SMOKE_INPUT", r"C:\tmp\input.mp4"),
            ("VFL_SMOKE_OUTPUT", r"C:\tmp\output.mp4"),
            ("VFL_SMOKE_STATUS", &status_path),
        ]);

        assert_eq!(
            parse_smoke_config_from_env(&env).unwrap(),
            Some(AppSmokeConfig {
                input_path: r"C:\tmp\input.mp4".to_string(),
                output_path: r"C:\tmp\output.mp4".to_string(),
                status_path,
                format: OutputFormat::Mp4,
                size_limit_mb: 0.0,
                trim_start_s: 0.0,
                trim_end_s: None,
                skip_preview_interactions: false,
            })
        );
    }

    #[test]
    fn parse_smoke_config_reads_optional_values() {
        let status_path = temp_smoke_status_path("optional");
        let env = smoke_env(&[
            ("VFL_SMOKE_INPUT", r"C:\tmp\input.mp4"),
            ("VFL_SMOKE_OUTPUT", r"C:\tmp\output.webm"),
            ("VFL_SMOKE_STATUS", &status_path),
            ("VFL_SMOKE_FORMAT", "webm"),
            ("VFL_SMOKE_SIZE_LIMIT_MB", "12.5"),
            ("VFL_SMOKE_TRIM_START_S", "0.25"),
            ("VFL_SMOKE_TRIM_END_S", "1.5"),
            ("VFL_SMOKE_SKIP_PREVIEW_INTERACTIONS", "true"),
        ]);

        assert_eq!(
            parse_smoke_config_from_env(&env).unwrap(),
            Some(AppSmokeConfig {
                input_path: r"C:\tmp\input.mp4".to_string(),
                output_path: r"C:\tmp\output.webm".to_string(),
                status_path,
                format: OutputFormat::Webm,
                size_limit_mb: 12.5,
                trim_start_s: 0.25,
                trim_end_s: Some(1.5),
                skip_preview_interactions: true,
            })
        );
    }

    #[test]
    fn parse_smoke_config_requires_output_and_status() {
        let env = smoke_env(&[("VFL_SMOKE_INPUT", r"C:\tmp\input.mp4")]);
        let err = parse_smoke_config_from_env(&env).unwrap_err();
        assert!(err.contains("VFL_SMOKE_OUTPUT"));
    }

    #[test]
    fn parse_smoke_config_rejects_status_path_outside_temp_dir() {
        let non_temp_status = std::env::current_dir().unwrap().join("smoke-status.json");
        let status_path = non_temp_status.to_string_lossy().to_string();
        let env = smoke_env(&[
            ("VFL_SMOKE_INPUT", r"C:\tmp\input.mp4"),
            ("VFL_SMOKE_OUTPUT", r"C:\tmp\output.mp4"),
            ("VFL_SMOKE_STATUS", &status_path),
        ]);
        let err = parse_smoke_config_from_env(&env).unwrap_err();
        assert!(err.contains("VFL_SMOKE_STATUS"));
    }

    #[test]
    fn parse_smoke_config_rejects_invalid_ranges() {
        let status_path = temp_smoke_status_path("ranges");
        let env = smoke_env(&[
            ("VFL_SMOKE_INPUT", r"C:\tmp\input.mp4"),
            ("VFL_SMOKE_OUTPUT", r"C:\tmp\output.mp4"),
            ("VFL_SMOKE_STATUS", &status_path),
            ("VFL_SMOKE_TRIM_START_S", "2"),
            ("VFL_SMOKE_TRIM_END_S", "1"),
        ]);
        let err = parse_smoke_config_from_env(&env).unwrap_err();
        assert!(err.contains("VFL_SMOKE_TRIM_END_S"));
    }

    #[test]
    fn merge_smoke_stage_history_starts_with_current_stage() {
        assert_eq!(
            merge_smoke_stage_history(None, &[], "detected"),
            vec!["detected".to_string()]
        );
    }

    #[test]
    fn merge_smoke_stage_history_appends_missing_stage() {
        let existing = AppSmokeStatus {
            stage: "preview-ready".to_string(),
            ok: None,
            message: None,
            output_path: None,
            output_size_bytes: None,
            trim_start_s: None,
            trim_end_s: None,
            expected_duration_s: None,
            stage_history: vec![
                "detected".to_string(),
                "input-applied".to_string(),
                "probe-ready".to_string(),
                "preview-ready".to_string(),
            ],
        };

        assert_eq!(
            merge_smoke_stage_history(Some(&existing), &[], "interaction-ready"),
            vec![
                "detected".to_string(),
                "input-applied".to_string(),
                "probe-ready".to_string(),
                "preview-ready".to_string(),
                "interaction-ready".to_string(),
            ]
        );
    }

    #[test]
    fn merge_smoke_stage_history_backfills_previous_stage_for_old_status_files() {
        let existing = AppSmokeStatus {
            stage: "preview-ready".to_string(),
            ok: None,
            message: None,
            output_path: None,
            output_size_bytes: None,
            trim_start_s: None,
            trim_end_s: None,
            expected_duration_s: None,
            stage_history: Vec::new(),
        };

        assert_eq!(
            merge_smoke_stage_history(Some(&existing), &[], "encoding"),
            vec!["preview-ready".to_string(), "encoding".to_string()]
        );
    }

    #[test]
    fn merge_smoke_stage_history_preserves_incoming_frontend_history() {
        let existing = AppSmokeStatus {
            stage: "input-applied".to_string(),
            ok: None,
            message: None,
            output_path: None,
            output_size_bytes: None,
            trim_start_s: None,
            trim_end_s: None,
            expected_duration_s: None,
            stage_history: vec!["detected".to_string(), "input-applied".to_string()],
        };

        assert_eq!(
            merge_smoke_stage_history(
                Some(&existing),
                &[
                    "detected".to_string(),
                    "input-applied".to_string(),
                    "probe-ready".to_string(),
                    "preview-ready".to_string(),
                ],
                "preview-ready",
            ),
            vec![
                "detected".to_string(),
                "input-applied".to_string(),
                "probe-ready".to_string(),
                "preview-ready".to_string(),
            ]
        );
    }

    #[test]
    fn merge_smoke_optional_fields_preserves_trim_metrics() {
        let existing = AppSmokeStatus {
            stage: "interaction-ready".to_string(),
            ok: Some(true),
            message: Some("Trim metrics verified.".to_string()),
            output_path: None,
            output_size_bytes: None,
            trim_start_s: Some(0.6),
            trim_end_s: Some(1.35),
            expected_duration_s: Some(0.75),
            stage_history: vec!["interaction-ready".to_string()],
        };
        let mut next = AppSmokeStatus {
            stage: "success".to_string(),
            ok: Some(true),
            message: Some("Packaged app smoke export succeeded.".to_string()),
            output_path: Some("C:\\tmp\\output.mp4".to_string()),
            output_size_bytes: Some(1234),
            trim_start_s: None,
            trim_end_s: None,
            expected_duration_s: None,
            stage_history: vec!["success".to_string()],
        };

        merge_smoke_optional_fields(Some(&existing), &mut next);

        assert_eq!(next.trim_start_s, Some(0.6));
        assert_eq!(next.trim_end_s, Some(1.35));
        assert_eq!(next.expected_duration_s, Some(0.75));
    }
}
