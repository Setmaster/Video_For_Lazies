use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Window};

const MB_BYTES: u64 = 1_000_000;
const PROGRESS_EMIT_EVERY: Duration = Duration::from_millis(200);
const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const CROP_DETECT_TIMEOUT: Duration = Duration::from_secs(60);
const FRAME_EXTRACT_TIMEOUT: Duration = Duration::from_secs(45);
const ENCODE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const FFMPEG_SIDECAR_DIR: &str = "ffmpeg-sidecar";

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static ENCODER_CACHE: OnceLock<Mutex<HashMap<String, HashSet<String>>>> = OnceLock::new();
const AUDIO_BITRATE_PRESETS_KBPS: &[u32] = &[96, 128, 192, 256, 320];

fn command_no_window(bin: &str) -> Command {
    #[cfg(windows)]
    let mut cmd = Command::new(bin);
    #[cfg(not(windows))]
    let cmd = Command::new(bin);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProbe {
    pub duration_s: f64,
    pub width: u32,
    pub height: u32,
    pub frame_rate: Option<f64>,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Trim {
    pub start_s: f64,
    pub end_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorAdjust {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Mp4,
    Webm,
    Mp3,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodecPreference {
    Auto,
    H264,
    Mpeg4,
    Vp9,
    Vp8,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum VideoQualityPreference {
    Auto,
    Smaller,
    Balanced,
    Higher,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncodeSpeedPreference {
    Auto,
    Faster,
    Balanced,
    Smaller,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AudioChannelPreference {
    Auto,
    Stereo,
    Mono,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AdvancedEncodeSettings {
    #[serde(default)]
    pub video_codec: Option<VideoCodecPreference>,
    #[serde(default)]
    pub audio_bitrate_kbps: Option<u32>,
    #[serde(default)]
    pub video_quality: Option<VideoQualityPreference>,
    #[serde(default)]
    pub encode_speed: Option<EncodeSpeedPreference>,
    #[serde(default)]
    pub frame_rate_cap_fps: Option<u32>,
    #[serde(default)]
    pub audio_channels: Option<AudioChannelPreference>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ResizeMode {
    #[default]
    Source,
    MaxEdge,
    Custom,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResizeSettings {
    #[serde(default)]
    pub mode: ResizeMode,
    #[serde(default)]
    pub max_edge_px: Option<u32>,
    #[serde(default)]
    pub width_px: Option<u32>,
    #[serde(default)]
    pub height_px: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeRequest {
    pub input_path: String,
    pub output_path: String,
    pub format: OutputFormat,
    pub title: Option<String>,
    pub size_limit_mb: f64,
    pub audio_enabled: bool,
    #[serde(default)]
    pub normalize_audio: bool,
    #[serde(default)]
    pub strip_metadata: bool,
    #[serde(default)]
    pub advanced: AdvancedEncodeSettings,

    pub trim: Option<Trim>,
    pub crop: Option<Crop>,
    pub reverse: bool,
    pub speed: f64,
    pub rotate_deg: u16,
    #[serde(default)]
    pub resize: Option<ResizeSettings>,
    #[serde(default)]
    pub max_edge_px: Option<u32>,
    pub color: Option<ColorAdjust>,
    /// When true, imperceptibly perturb only the first output frame so its hash
    /// differs from the source / a vanilla export (defeats exact-hash dedupe on
    /// forums). Off by default; the Forum 4 MB recipe turns it on.
    #[serde(default)]
    pub perturb_first_frame: bool,
    /// Optional fixed seed for the first-frame perturbation. None (the normal
    /// case) makes the backend pick a fresh random seed per export so repeated
    /// exports stay unique; an explicit seed is used for deterministic tests.
    #[serde(default)]
    pub perturb_seed: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeProgressPayload {
    pub job_id: u64,
    pub pass: u8,
    pub total_passes: u8,
    pub pass_pct: f64,
    pub overall_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeFinishedPayload {
    pub job_id: u64,
    pub ok: bool,
    pub output_path: Option<String>,
    pub output_size_bytes: Option<u64>,
    pub message: Option<String>,
    pub diagnostics: Option<ExportDiagnostics>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportDiagnostics {
    pub mode: String,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_bitrate_kbps: Option<u32>,
    pub audio_bitrate_kbps: Option<u32>,
    pub requested_size_bytes: Option<u64>,
    pub actual_size_bytes: Option<u64>,
    pub passes: u8,
    pub attempts: u32,
    pub audio_removed_for_size_target: bool,
    pub command_preview: String,
}

#[derive(Debug, Clone)]
struct EncodePlan {
    video_bitrate_kbps: u32,
    audio_bitrate_kbps: u32,
    include_audio: bool,
}

#[derive(Debug, Clone)]
struct SizeLimitedEncodeContract {
    planned_width: u32,
    planned_height: u32,
    min_video_kbps: u32,
    plan: EncodePlan,
    audio_removed_for_size_target: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    LibX264,
    Mpeg4,
    LibVpxVp9,
    LibVpx,
}

impl VideoCodec {
    fn as_ffmpeg_name(self) -> &'static str {
        match self {
            VideoCodec::LibX264 => "libx264",
            VideoCodec::Mpeg4 => "mpeg4",
            VideoCodec::LibVpxVp9 => "libvpx-vp9",
            VideoCodec::LibVpx => "libvpx",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioCodec {
    Aac,
    LibOpus,
    LibVorbis,
    LibMp3Lame,
    Mp3,
}

impl AudioCodec {
    fn as_ffmpeg_name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::LibOpus => "libopus",
            AudioCodec::LibVorbis => "libvorbis",
            AudioCodec::LibMp3Lame => "libmp3lame",
            AudioCodec::Mp3 => "mp3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualityMode {
    Crf {
        crf: &'static str,
        preset: Option<&'static str>,
    },
    Cq {
        crf: &'static str,
        bitrate: &'static str,
    },
    QScale {
        qscale: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodecSelection {
    video_codec: Option<VideoCodec>,
    audio_codec: Option<AudioCodec>,
    quality_mode: Option<QualityMode>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoCodecCapability {
    pub format: OutputFormat,
    pub value: VideoCodecPreference,
    pub label: &'static str,
    pub ffmpeg_name: &'static str,
    pub available: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeCapabilities {
    pub video_codecs: Vec<VideoCodecCapability>,
    pub audio_bitrate_kbps: Vec<u32>,
}

fn trimmed_env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_executable_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn bundled_sidecar_candidate(
    executable_dir: Option<&Path>,
    stem: &str,
    binary_suffix: &str,
) -> Option<PathBuf> {
    let executable_dir = executable_dir?;
    Some(
        executable_dir
            .join(FFMPEG_SIDECAR_DIR)
            .join(format!("{stem}{binary_suffix}")),
    )
}

fn resolve_binary_path(
    env_override: Option<String>,
    bundled_candidate: Option<PathBuf>,
    fallback: &str,
) -> String {
    if let Some(override_path) = env_override {
        return override_path;
    }
    if let Some(candidate) = bundled_candidate.filter(|path| path.exists()) {
        return candidate.to_string_lossy().to_string();
    }
    fallback.to_string()
}

fn default_ffmpeg() -> String {
    resolve_binary_path(
        trimmed_env_var("VFL_FFMPEG_PATH"),
        bundled_sidecar_candidate(
            current_executable_dir().as_deref(),
            "ffmpeg",
            std::env::consts::EXE_SUFFIX,
        ),
        "ffmpeg",
    )
}

fn default_ffprobe() -> String {
    resolve_binary_path(
        trimmed_env_var("VFL_FFPROBE_PATH"),
        bundled_sidecar_candidate(
            current_executable_dir().as_deref(),
            "ffprobe",
            std::env::consts::EXE_SUFFIX,
        ),
        "ffprobe",
    )
}

fn binary_not_found_message(binary_name: &str, env_var: &str, tried: &str) -> String {
    format!(
        "{binary_name} was not found.\n\nRelease builds first look for a bundled runtime in `{FFMPEG_SIDECAR_DIR}` next to the app executable. Otherwise install FFmpeg and ensure `{binary_name}` is on PATH, or set `{env_var}` to the full path.\n\nTried: {tried}"
    )
}

fn run_command_output_with_timeout(
    mut cmd: Command,
    binary_name: &str,
    env_var: &str,
    tried: &str,
    action: &str,
    timeout: Duration,
) -> Result<Output, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            binary_not_found_message(binary_name, env_var, tried)
        } else {
            format!("Failed to start {action} ({tried}): {e}")
        }
    })?;

    // Drain both pipes while polling; a child that fills an unread pipe buffer
    // blocks forever and would be misreported as a timeout.
    fn spawn_pipe_reader<R: std::io::Read + Send + 'static>(
        reader: Option<R>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        reader.map(|mut r| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = r.read_to_end(&mut buf);
                buf
            })
        })
    }
    let mut stdout_reader = spawn_pipe_reader(child.stdout.take());
    let mut stderr_reader = spawn_pipe_reader(child.stderr.take());
    let join_pipe = |handle: &mut Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default()
    };

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = join_pipe(&mut stdout_reader);
                let stderr = join_pipe(&mut stderr_reader);
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if started_at.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = join_pipe(&mut stdout_reader);
                    let _ = join_pipe(&mut stderr_reader);
                    return Err(format!(
                        "{action} timed out after {} seconds.",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Failed while waiting for {action}: {e}")),
        }
    }
}

fn canonical_destination_path(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Output path must include a folder.".to_string())?;
    if !parent.exists() {
        return Err("Output folder does not exist.".to_string());
    }
    if !parent.is_dir() {
        return Err("Output folder is not a directory.".to_string());
    }

    let file_name = path
        .file_name()
        .ok_or_else(|| "Output path must include a filename.".to_string())?;
    let parent = parent
        .canonicalize()
        .map_err(|e| format!("Failed to resolve output folder: {e}"))?;
    Ok(parent.join(file_name))
}

fn validate_output_path(
    input_path: &Path,
    output_path: &Path,
    expected_extension: &str,
) -> Result<PathBuf, String> {
    if output_path.as_os_str().is_empty() {
        return Err("Output path is required.".to_string());
    }

    let actual_extension = output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .ok_or_else(|| format!("Output filename must end with .{expected_extension}."))?;
    if actual_extension != expected_extension {
        return Err(format!(
            "Output filename must end with .{expected_extension}."
        ));
    }

    let input_path = input_path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve input file: {e}"))?;
    let destination_path = canonical_destination_path(output_path)?;
    if destination_path == input_path {
        return Err("Output path must be different from the input file.".to_string());
    }
    if destination_path.exists() {
        return Err("Output file already exists. Choose a new filename.".to_string());
    }

    Ok(destination_path)
}

fn push_metadata_args(args: &mut Vec<String>, request: &EncodeRequest) {
    if request.strip_metadata {
        // Drop source global metadata (GPS location, capture info) before the
        // explicit title, so the title survives the strip.
        args.push("-map_metadata".to_string());
        args.push("-1".to_string());
    }
    if let Some(title) = request
        .title
        .as_deref()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        args.push("-metadata".to_string());
        args.push(format!("title={title}"));
    }
}

fn temp_output_path(output_path: &Path, job_id: u64, label: &str) -> Result<PathBuf, String> {
    let parent = output_path
        .parent()
        .ok_or_else(|| "Output path must include a folder.".to_string())?;
    let stem = output_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("output");
    let extension = output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| "Output path must include an extension.".to_string())?;
    Ok(parent.join(format!(".{stem}.vfl-{job_id}-{label}.tmp.{extension}")))
}

fn publish_output_file(temp_path: &Path, output_path: &Path) -> Result<(), String> {
    if output_path.exists() {
        let _ = fs::remove_file(temp_path);
        return Err("Output file already exists. Choose a new filename.".to_string());
    }
    fs::rename(temp_path, output_path).map_err(|e| format!("Failed to publish output file: {e}"))
}

fn parse_encoder_names(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            if flags.len() != 6
                || !flags
                    .chars()
                    .all(|ch| ch.is_ascii_alphabetic() || ch == '.')
            {
                return None;
            }
            let name = parts.next()?;
            if name == "=" {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

fn cached_encoder_names(ffmpeg_bin: &str) -> Result<HashSet<String>, String> {
    let cache = ENCODER_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(existing) = guard.get(ffmpeg_bin)
    {
        return Ok(existing.clone());
    }

    let mut cmd = command_no_window(ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-encoders");
    let output = run_command_output_with_timeout(
        cmd,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        ffmpeg_bin,
        "ffmpeg encoder probe",
        ENCODER_PROBE_TIMEOUT,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg encoder probe failed.\n\n{stderr}"));
    }

    let parsed = parse_encoder_names(&String::from_utf8_lossy(&output.stdout));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(ffmpeg_bin.to_string(), parsed.clone());
    }
    Ok(parsed)
}

fn video_quality_preference(advanced: &AdvancedEncodeSettings) -> VideoQualityPreference {
    advanced
        .video_quality
        .unwrap_or(VideoQualityPreference::Auto)
}

fn encode_speed_preference(advanced: &AdvancedEncodeSettings) -> EncodeSpeedPreference {
    advanced.encode_speed.unwrap_or(EncodeSpeedPreference::Auto)
}

fn audio_channel_preference(advanced: &AdvancedEncodeSettings) -> AudioChannelPreference {
    advanced
        .audio_channels
        .unwrap_or(AudioChannelPreference::Auto)
}

fn quality_mode_for_codec(
    video_codec: VideoCodec,
    preference: VideoQualityPreference,
) -> QualityMode {
    let preference = match preference {
        VideoQualityPreference::Auto => VideoQualityPreference::Balanced,
        other => other,
    };

    match (video_codec, preference) {
        (VideoCodec::LibX264, VideoQualityPreference::Smaller) => QualityMode::Crf {
            crf: "28",
            preset: Some("medium"),
        },
        (VideoCodec::LibX264, VideoQualityPreference::Balanced) => QualityMode::Crf {
            crf: "23",
            preset: Some("medium"),
        },
        (VideoCodec::LibX264, VideoQualityPreference::Higher) => QualityMode::Crf {
            crf: "20",
            preset: Some("medium"),
        },
        (VideoCodec::Mpeg4, VideoQualityPreference::Smaller) => QualityMode::QScale { qscale: "8" },
        (VideoCodec::Mpeg4, VideoQualityPreference::Balanced) => {
            QualityMode::QScale { qscale: "5" }
        }
        (VideoCodec::Mpeg4, VideoQualityPreference::Higher) => QualityMode::QScale { qscale: "3" },
        (VideoCodec::LibVpxVp9, VideoQualityPreference::Smaller) => QualityMode::Cq {
            crf: "38",
            bitrate: "0",
        },
        (VideoCodec::LibVpxVp9, VideoQualityPreference::Balanced) => QualityMode::Cq {
            crf: "32",
            bitrate: "0",
        },
        (VideoCodec::LibVpxVp9, VideoQualityPreference::Higher) => QualityMode::Cq {
            crf: "28",
            bitrate: "0",
        },
        (VideoCodec::LibVpx, VideoQualityPreference::Smaller) => QualityMode::Cq {
            crf: "16",
            bitrate: "800k",
        },
        (VideoCodec::LibVpx, VideoQualityPreference::Balanced) => QualityMode::Cq {
            crf: "10",
            bitrate: "1M",
        },
        (VideoCodec::LibVpx, VideoQualityPreference::Higher) => QualityMode::Cq {
            crf: "8",
            bitrate: "1500k",
        },
        (_, VideoQualityPreference::Auto) => unreachable!(),
    }
}

fn codec_preference_for_codec(video_codec: VideoCodec) -> VideoCodecPreference {
    match video_codec {
        VideoCodec::LibX264 => VideoCodecPreference::H264,
        VideoCodec::Mpeg4 => VideoCodecPreference::Mpeg4,
        VideoCodec::LibVpxVp9 => VideoCodecPreference::Vp9,
        VideoCodec::LibVpx => VideoCodecPreference::Vp8,
    }
}

fn codec_label(preference: VideoCodecPreference) -> &'static str {
    match preference {
        VideoCodecPreference::Auto => "Auto",
        VideoCodecPreference::H264 => "H.264",
        VideoCodecPreference::Mpeg4 => "MPEG-4",
        VideoCodecPreference::Vp9 => "VP9",
        VideoCodecPreference::Vp8 => "VP8",
    }
}

fn codec_ffmpeg_name(preference: VideoCodecPreference) -> Option<&'static str> {
    match preference {
        VideoCodecPreference::Auto => None,
        VideoCodecPreference::H264 => Some("libx264"),
        VideoCodecPreference::Mpeg4 => Some("mpeg4"),
        VideoCodecPreference::Vp9 => Some("libvpx-vp9"),
        VideoCodecPreference::Vp8 => Some("libvpx"),
    }
}

fn auto_video_codec(format: OutputFormat, encoder_names: &HashSet<String>) -> Option<VideoCodec> {
    match format {
        OutputFormat::Mp4 => Some(if encoder_names.contains("libx264") {
            VideoCodec::LibX264
        } else {
            VideoCodec::Mpeg4
        }),
        OutputFormat::Webm => Some(if encoder_names.contains("libvpx-vp9") {
            VideoCodec::LibVpxVp9
        } else {
            VideoCodec::LibVpx
        }),
        OutputFormat::Mp3 => None,
    }
}

fn audio_codec_for_format(
    format: OutputFormat,
    encoder_names: &HashSet<String>,
) -> Option<AudioCodec> {
    match format {
        OutputFormat::Mp4 => Some(AudioCodec::Aac),
        OutputFormat::Webm => Some(if encoder_names.contains("libopus") {
            AudioCodec::LibOpus
        } else {
            AudioCodec::LibVorbis
        }),
        OutputFormat::Mp3 => Some(if encoder_names.contains("libmp3lame") {
            AudioCodec::LibMp3Lame
        } else {
            AudioCodec::Mp3
        }),
    }
}

fn requested_video_codec(
    format: OutputFormat,
    preference: VideoCodecPreference,
    encoder_names: &HashSet<String>,
) -> Result<Option<VideoCodec>, String> {
    let codec = match (format, preference) {
        (_, VideoCodecPreference::Auto) | (OutputFormat::Mp3, _) => return Ok(None),
        (OutputFormat::Mp4, VideoCodecPreference::H264) => VideoCodec::LibX264,
        (OutputFormat::Mp4, VideoCodecPreference::Mpeg4) => VideoCodec::Mpeg4,
        (OutputFormat::Webm, VideoCodecPreference::Vp9) => VideoCodec::LibVpxVp9,
        (OutputFormat::Webm, VideoCodecPreference::Vp8) => VideoCodec::LibVpx,
        (OutputFormat::Mp4, VideoCodecPreference::Vp9 | VideoCodecPreference::Vp8) => {
            return Err("VP8/VP9 codecs are only valid for WebM output.".to_string());
        }
        (OutputFormat::Webm, VideoCodecPreference::H264 | VideoCodecPreference::Mpeg4) => {
            return Err("H.264 and MPEG-4 codecs are only valid for MP4 output.".to_string());
        }
    };

    let ffmpeg_name = codec.as_ffmpeg_name();
    if !encoder_names.contains(ffmpeg_name) {
        return Err(format!(
            "The selected {} encoder ({ffmpeg_name}) is not available in this FFmpeg build.",
            codec_label(preference)
        ));
    }

    Ok(Some(codec))
}

fn select_codec_plan(
    format: OutputFormat,
    encoder_names: &HashSet<String>,
    advanced: &AdvancedEncodeSettings,
) -> Result<CodecSelection, String> {
    match format {
        OutputFormat::Mp4 | OutputFormat::Webm => {
            let preference = advanced.video_codec.unwrap_or(VideoCodecPreference::Auto);
            let video_codec = requested_video_codec(format, preference, encoder_names)?
                .or_else(|| auto_video_codec(format, encoder_names))
                .ok_or_else(|| "Missing video codec selection.".to_string())?;
            Ok(CodecSelection {
                video_codec: Some(video_codec),
                audio_codec: audio_codec_for_format(format, encoder_names),
                quality_mode: Some(quality_mode_for_codec(
                    video_codec,
                    video_quality_preference(advanced),
                )),
            })
        }
        OutputFormat::Mp3 => Ok(CodecSelection {
            video_codec: None,
            audio_codec: audio_codec_for_format(format, encoder_names),
            quality_mode: None,
        }),
    }
}

fn video_codec_capabilities_for_format(
    format: OutputFormat,
    encoder_names: &HashSet<String>,
) -> Vec<VideoCodecCapability> {
    let default_preference =
        auto_video_codec(format, encoder_names).map(codec_preference_for_codec);
    let preferences: &[VideoCodecPreference] = match format {
        OutputFormat::Mp4 => &[VideoCodecPreference::H264, VideoCodecPreference::Mpeg4],
        OutputFormat::Webm => &[VideoCodecPreference::Vp9, VideoCodecPreference::Vp8],
        OutputFormat::Mp3 => &[],
    };

    preferences
        .iter()
        .filter_map(|preference| {
            let ffmpeg_name = codec_ffmpeg_name(*preference)?;
            Some(VideoCodecCapability {
                format,
                value: *preference,
                label: codec_label(*preference),
                ffmpeg_name,
                available: encoder_names.contains(ffmpeg_name),
                is_default: default_preference == Some(*preference),
            })
        })
        .collect()
}

pub fn encode_capabilities() -> Result<EncodeCapabilities, String> {
    let ffmpeg_bin = default_ffmpeg();
    let encoder_names = cached_encoder_names(&ffmpeg_bin)?;
    let mut video_codecs = Vec::new();
    video_codecs.extend(video_codec_capabilities_for_format(
        OutputFormat::Mp4,
        &encoder_names,
    ));
    video_codecs.extend(video_codec_capabilities_for_format(
        OutputFormat::Webm,
        &encoder_names,
    ));

    Ok(EncodeCapabilities {
        video_codecs,
        audio_bitrate_kbps: AUDIO_BITRATE_PRESETS_KBPS.to_vec(),
    })
}

fn advanced_audio_bitrate_kbps(request: &EncodeRequest) -> Result<Option<u32>, String> {
    let Some(kbps) = request.advanced.audio_bitrate_kbps else {
        return Ok(None);
    };

    if !AUDIO_BITRATE_PRESETS_KBPS.contains(&kbps) {
        return Err(format!(
            "Audio bitrate must be one of: {} kbps.",
            AUDIO_BITRATE_PRESETS_KBPS
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    Ok(Some(kbps))
}

fn audio_channel_count(request: &EncodeRequest) -> Option<u8> {
    match audio_channel_preference(&request.advanced) {
        AudioChannelPreference::Auto => None,
        AudioChannelPreference::Stereo => Some(2),
        AudioChannelPreference::Mono => Some(1),
    }
}

fn validate_frame_rate_cap_fps(cap_fps: u32) -> Result<u32, String> {
    if (1..=240).contains(&cap_fps) {
        Ok(cap_fps)
    } else {
        Err("Frame rate cap must be between 1 and 240 fps.".to_string())
    }
}

fn requested_frame_rate_cap_fps(request: &EncodeRequest) -> Result<Option<u32>, String> {
    request
        .advanced
        .frame_rate_cap_fps
        .map(validate_frame_rate_cap_fps)
        .transpose()
}

fn effective_speed(request: &EncodeRequest) -> f64 {
    if request.speed.is_finite() && request.speed > 0.0 {
        request.speed
    } else {
        1.0
    }
}

fn frame_rate_cap_filter_fps(
    request: &EncodeRequest,
    probe: &VideoProbe,
) -> Result<Option<u32>, String> {
    let Some(cap_fps) = requested_frame_rate_cap_fps(request)? else {
        return Ok(None);
    };
    let Some(source_fps) = probe.frame_rate.filter(|fps| fps.is_finite() && *fps > 0.0) else {
        return Ok(Some(cap_fps));
    };

    // setpts speed-up multiplies the effective frame rate before the cap applies.
    if source_fps * effective_speed(request) > cap_fps as f64 + 0.01 {
        Ok(Some(cap_fps))
    } else {
        Ok(None)
    }
}

fn output_frame_rate_for_planning(
    request: &EncodeRequest,
    probe: &VideoProbe,
) -> Result<Option<f64>, String> {
    if let Some(cap_fps) = frame_rate_cap_filter_fps(request, probe)? {
        Ok(Some(cap_fps as f64))
    } else {
        Ok(probe.frame_rate.map(|fps| fps * effective_speed(request)))
    }
}

fn advanced_forces_reencode(
    request: &EncodeRequest,
    probe: &VideoProbe,
    size_limit_enabled: bool,
) -> Result<bool, String> {
    if matches!(request.format, OutputFormat::Mp3) {
        return Ok(false);
    }

    let video_codec = request
        .advanced
        .video_codec
        .unwrap_or(VideoCodecPreference::Auto);
    if video_codec != VideoCodecPreference::Auto {
        return Ok(true);
    }

    if !size_limit_enabled
        && video_quality_preference(&request.advanced) != VideoQualityPreference::Auto
    {
        return Ok(true);
    }

    if encode_speed_preference(&request.advanced) != EncodeSpeedPreference::Auto {
        return Ok(true);
    }

    if frame_rate_cap_filter_fps(request, probe)?.is_some() {
        return Ok(true);
    }

    if request.audio_enabled && probe.has_audio {
        if request.normalize_audio {
            return Ok(true);
        }
        if !size_limit_enabled && request.advanced.audio_bitrate_kbps.is_some() {
            return Ok(true);
        }
        if audio_channel_count(request).is_some() {
            return Ok(true);
        }
    }

    Ok(false)
}

fn encode_speed_args_for_codec(
    video_codec: VideoCodec,
    preference: EncodeSpeedPreference,
) -> Vec<String> {
    match (video_codec, preference) {
        (_, EncodeSpeedPreference::Auto) => Vec::new(),
        (VideoCodec::LibX264, EncodeSpeedPreference::Faster) => ["-preset", "faster"]
            .into_iter()
            .map(String::from)
            .collect(),
        (VideoCodec::LibX264, EncodeSpeedPreference::Balanced) => ["-preset", "medium"]
            .into_iter()
            .map(String::from)
            .collect(),
        (VideoCodec::LibX264, EncodeSpeedPreference::Smaller) => {
            ["-preset", "slow"].into_iter().map(String::from).collect()
        }
        (VideoCodec::LibVpxVp9, EncodeSpeedPreference::Faster) => {
            ["-deadline", "good", "-cpu-used", "5", "-row-mt", "1"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::LibVpxVp9, EncodeSpeedPreference::Balanced) => {
            ["-deadline", "good", "-cpu-used", "3", "-row-mt", "1"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::LibVpxVp9, EncodeSpeedPreference::Smaller) => {
            ["-deadline", "good", "-cpu-used", "1", "-row-mt", "1"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::LibVpx, EncodeSpeedPreference::Faster) => {
            ["-deadline", "good", "-cpu-used", "4"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::LibVpx, EncodeSpeedPreference::Balanced) => {
            ["-deadline", "good", "-cpu-used", "2"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::LibVpx, EncodeSpeedPreference::Smaller) => {
            ["-deadline", "good", "-cpu-used", "1"]
                .into_iter()
                .map(String::from)
                .collect()
        }
        (VideoCodec::Mpeg4, _) => Vec::new(),
    }
}

fn output_extension(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Mp4 => "mp4",
        OutputFormat::Webm => "webm",
        OutputFormat::Mp3 => "mp3",
    }
}

fn even_at_least_two(value: u32) -> u32 {
    let even = if value & 1 == 0 {
        value
    } else {
        value.saturating_sub(1)
    };
    even.max(2)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizePlan {
    Source,
    MaxEdge(u32),
    Custom { width_px: u32, height_px: u32 },
}

fn validate_output_dimension_px(label: &str, value: u32) -> Result<u32, String> {
    if value < 16 {
        return Err(format!("{label} must be >= 16 px."));
    }
    if value > 32768 {
        return Err(format!("{label} must be <= 32768 px."));
    }
    Ok(value)
}

fn requested_resize(req: &EncodeRequest) -> Result<ResizePlan, String> {
    let Some(resize) = &req.resize else {
        return match req.max_edge_px {
            Some(max_edge_px) => Ok(ResizePlan::MaxEdge(validate_output_dimension_px(
                "Max edge",
                max_edge_px,
            )?)),
            None => Ok(ResizePlan::Source),
        };
    };

    match resize.mode {
        ResizeMode::Source => Ok(ResizePlan::Source),
        ResizeMode::MaxEdge => {
            let max_edge_px = resize.max_edge_px.or(req.max_edge_px).ok_or_else(|| {
                "Max edge must be set when output dimensions use Max edge mode.".to_string()
            })?;
            Ok(ResizePlan::MaxEdge(validate_output_dimension_px(
                "Max edge",
                max_edge_px,
            )?))
        }
        ResizeMode::Custom => {
            let width_px = resize.width_px.ok_or_else(|| {
                "Output width must be set when output dimensions use Custom mode.".to_string()
            })?;
            let height_px = resize.height_px.ok_or_else(|| {
                "Output height must be set when output dimensions use Custom mode.".to_string()
            })?;
            Ok(ResizePlan::Custom {
                width_px: validate_output_dimension_px("Output width", width_px)?,
                height_px: validate_output_dimension_px("Output height", height_px)?,
            })
        }
    }
}

fn estimated_output_dimensions(
    req: &EncodeRequest,
    probe: &VideoProbe,
) -> Result<(u32, u32), String> {
    let mut width = probe.width.max(2);
    let mut height = probe.height.max(2);

    if let Some(crop) = &req.crop {
        width = even_at_least_two(crop.width);
        height = even_at_least_two(crop.height);
    }

    match req.rotate_deg {
        90 | 270 => std::mem::swap(&mut width, &mut height),
        _ => {}
    }

    match requested_resize(req)? {
        ResizePlan::Source => {}
        ResizePlan::MaxEdge(max_edge_px) => {
            let long_edge = width.max(height);
            if long_edge > max_edge_px {
                if width >= height {
                    let scaled_height =
                        (((height as f64) * (max_edge_px as f64)) / (width as f64)).round() as u32;
                    width = even_at_least_two(max_edge_px);
                    height = even_at_least_two(scaled_height);
                } else {
                    let scaled_width =
                        (((width as f64) * (max_edge_px as f64)) / (height as f64)).round() as u32;
                    width = even_at_least_two(scaled_width);
                    height = even_at_least_two(max_edge_px);
                }
            }
        }
        ResizePlan::Custom {
            width_px,
            height_px,
        } => {
            width = even_at_least_two(width_px);
            height = even_at_least_two(height_px);
        }
    }

    // The filter chain forces even output for every path, so the plan should
    // match even for untouched odd-dimension sources.
    Ok((even_at_least_two(width), even_at_least_two(height)))
}

fn minimum_video_bitrate_kbps(
    codec: VideoCodec,
    width: u32,
    height: u32,
    frame_rate: Option<f64>,
) -> u32 {
    match codec {
        VideoCodec::Mpeg4 => {
            let fps = frame_rate
                .filter(|fps| fps.is_finite() && *fps > 0.0)
                .unwrap_or(30.0)
                .clamp(1.0, 120.0);
            let macroblocks_per_frame = width.div_ceil(16) as f64 * height.div_ceil(16) as f64;
            let minimum = ((macroblocks_per_frame * fps * 3.6) / 1000.0).ceil() as u32;
            minimum.max(96)
        }
        VideoCodec::LibX264 | VideoCodec::LibVpxVp9 | VideoCodec::LibVpx => 50,
    }
}

fn split_sequence_stem(stem: &str) -> (String, u32) {
    // Only treat "-N" as an incrementable suffix when N is short (1..=3 digits),
    // otherwise prefer appending "-2" (ex: "movie-2024" -> "movie-2024-2").
    let Some((base, n_str)) = stem.rsplit_once('-') else {
        return (stem.to_string(), 2);
    };

    if n_str.is_empty() || n_str.len() > 3 || !n_str.chars().all(|c| c.is_ascii_digit()) {
        return (stem.to_string(), 2);
    }

    let Some(n) = n_str.parse::<u32>().ok() else {
        return (stem.to_string(), 2);
    };

    (base.to_string(), n.saturating_add(1).max(2))
}

pub fn suggest_output_path_unique(
    input_path: String,
    format: OutputFormat,
    taken_paths: &[String],
) -> Result<String, String> {
    let input_path = PathBuf::from(input_path.trim());
    let stem = input_path
        .file_stem()
        .ok_or_else(|| "Invalid input path.".to_string())?
        .to_string_lossy()
        .to_string();

    let (base, start_n) = split_sequence_stem(&stem);
    let ext = output_extension(format);
    let parent = input_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new(""));
    // Paths claimed by not-yet-written outputs (queued export snapshots), which
    // an on-disk existence check alone cannot see.
    let taken: HashSet<&str> = taken_paths.iter().map(|p| p.as_str()).collect();

    let max_tries = 10_000u32;
    for i in start_n..start_n.saturating_add(max_tries) {
        let file = format!("{base}-{i}.{ext}");
        let candidate = parent.join(file);
        let candidate_str = candidate.to_string_lossy().to_string();
        if !candidate.exists() && !taken.contains(candidate_str.as_str()) {
            return Ok(candidate_str);
        }
    }

    Err("Could not find a free output filename.".to_string())
}

pub fn extract_frame(input_path: String, time_s: f64, output_path: String) -> Result<(), String> {
    let input_path = PathBuf::from(input_path.trim());
    if !input_path.exists() {
        return Err(format!("File not found: {}", input_path.display()));
    }
    if !input_path.is_file() {
        return Err("Input path must point to a file.".to_string());
    }
    if !time_s.is_finite() || time_s < 0.0 {
        return Err("Time must be >= 0 seconds.".to_string());
    }

    let output_path = validate_output_path(&input_path, &PathBuf::from(output_path.trim()), "png")?;
    let temp_id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_millis(0))
        .as_millis() as u64;
    let temp_path = temp_output_path(&output_path, temp_id, "frame")?;

    let ffmpeg_bin = default_ffmpeg();

    let mut cmd = command_no_window(&ffmpeg_bin);
    // Input-side seeking: output-side -ss decodes the whole stream up to the
    // timestamp and times out on long sources.
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-ss")
        .arg(format!("{time_s:.3}"))
        .arg("-i")
        .arg(input_path.as_os_str())
        .arg("-frames:v")
        .arg("1")
        .arg("-an")
        .arg(temp_path.as_os_str());
    let output = run_command_output_with_timeout(
        cmd,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &ffmpeg_bin,
        "frame export",
        FRAME_EXTRACT_TIMEOUT,
    )
    .inspect_err(|_| {
        let _ = fs::remove_file(&temp_path);
    })?;

    if !output.status.success() {
        let _ = fs::remove_file(&temp_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let start = lines.len().saturating_sub(20);
        let tail = lines[start..].join("\n");
        return Err(format!("Frame export failed.\n\n{tail}"));
    }

    publish_output_file(&temp_path, &output_path)?;

    Ok(())
}

#[derive(Debug, Deserialize)]
struct FFProbeOutput {
    #[serde(default)]
    streams: Vec<FFProbeStream>,
    #[serde(default)]
    format: FFProbeFormat,
}

#[derive(Debug, Deserialize, Default)]
struct FFProbeFormat {
    duration: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FFProbeSideData {
    side_data_type: Option<String>,
    rotation: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
struct FFProbeStreamTags {
    rotate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FFProbeStream {
    codec_type: Option<String>,
    duration: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    #[serde(default)]
    side_data_list: Vec<FFProbeSideData>,
    tags: Option<FFProbeStreamTags>,
}

fn json_number_to_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Display rotation in degrees (0/90/180/270), from the display matrix side
/// data or the legacy rotate tag. Rotations off the 90-degree grid return 0
/// because ffmpeg's autorotation ignores them too.
fn stream_rotation_deg(stream: &FFProbeStream) -> u32 {
    let raw = stream
        .side_data_list
        .iter()
        .filter(|sd| {
            sd.side_data_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("Display Matrix"))
        })
        .find_map(|sd| sd.rotation.as_ref().and_then(json_number_to_f64))
        .or_else(|| {
            stream
                .tags
                .as_ref()
                .and_then(|t| t.rotate.as_deref())
                .and_then(|r| r.trim().parse::<f64>().ok())
        });
    let Some(raw) = raw else {
        return 0;
    };
    if !raw.is_finite() {
        return 0;
    }
    let nearest = (raw / 90.0).round() * 90.0;
    if (raw - nearest).abs() > 2.0 {
        return 0;
    }
    (((nearest as i64 % 360) + 360) % 360) as u32
}

fn parse_ffprobe_rate(rate: Option<&str>) -> Option<f64> {
    let rate = rate?.trim();
    if rate.is_empty() {
        return None;
    }

    if let Some((numerator, denominator)) = rate.split_once('/') {
        let numerator = numerator.trim().parse::<f64>().ok()?;
        let denominator = denominator.trim().parse::<f64>().ok()?;
        if numerator <= 0.0 || denominator <= 0.0 {
            return None;
        }
        let parsed = numerator / denominator;
        return parsed.is_finite().then_some(parsed);
    }

    let parsed = rate.parse::<f64>().ok()?;
    (parsed > 0.0 && parsed.is_finite()).then_some(parsed)
}

pub fn probe_video(path: String) -> Result<VideoProbe, String> {
    let video_path = PathBuf::from(path);
    if !video_path.exists() {
        return Err(format!("File not found: {}", video_path.display()));
    }
    if !video_path.is_file() {
        return Err("Input path must point to a file.".to_string());
    }

    let ffprobe_bin = default_ffprobe();
    let mut cmd = command_no_window(&ffprobe_bin);
    cmd.arg("-v")
        .arg("error")
        .arg("-print_format")
        .arg("json")
        .arg("-show_streams")
        .arg("-show_format")
        .arg(video_path.as_os_str());
    let output = run_command_output_with_timeout(
        cmd,
        "ffprobe",
        "VFL_FFPROBE_PATH",
        &ffprobe_bin,
        "ffprobe",
        FFPROBE_TIMEOUT,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed.\n\n{stderr}"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_probe_output(&stdout)
}

fn parse_probe_output(stdout: &str) -> Result<VideoProbe, String> {
    let parsed: FFProbeOutput =
        serde_json::from_str(stdout).map_err(|e| format!("ffprobe returned invalid JSON: {e}"))?;

    let has_audio = parsed
        .streams
        .iter()
        .any(|s| s.codec_type.as_deref() == Some("audio"));

    let mut duration_s = parsed
        .format
        .duration
        .as_deref()
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);

    if duration_s <= 0.0 {
        for stream in &parsed.streams {
            if stream.codec_type.as_deref() == Some("video")
                && let Some(d) = stream
                    .duration
                    .as_deref()
                    .and_then(|d| d.parse::<f64>().ok())
            {
                duration_s = d;
                break;
            }
        }
    }

    if duration_s <= 0.0 {
        return Err("Could not determine video duration (ffprobe).".to_string());
    }

    let mut width = 0u32;
    let mut height = 0u32;
    let mut frame_rate = None;
    for stream in &parsed.streams {
        if stream.codec_type.as_deref() == Some("video")
            && let (Some(w), Some(h)) = (stream.width, stream.height)
        {
            width = w;
            height = h;
            // The webview preview, ffmpeg autorotation, and cropdetect all work
            // in display space, so report display-oriented dimensions.
            if matches!(stream_rotation_deg(stream), 90 | 270) {
                std::mem::swap(&mut width, &mut height);
            }
            frame_rate = parse_ffprobe_rate(stream.avg_frame_rate.as_deref())
                .or_else(|| parse_ffprobe_rate(stream.r_frame_rate.as_deref()));
            break;
        }
    }
    if width == 0 || height == 0 {
        return Err("Could not determine video dimensions (ffprobe).".to_string());
    }

    Ok(VideoProbe {
        duration_s,
        width,
        height,
        frame_rate,
        has_audio,
    })
}

fn parse_cropdetect_crop_token(token: &str) -> Option<Crop> {
    let token = token.trim();
    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() != 4 {
        return None;
    }
    let width = parts[0].parse::<u32>().ok()?;
    let height = parts[1].parse::<u32>().ok()?;
    let x = parts[2].parse::<u32>().ok()?;
    let y = parts[3].parse::<u32>().ok()?;
    Some(Crop {
        x,
        y,
        width,
        height,
    })
}

fn parse_cropdetect_from_stderr(stderr: &str, probe: &VideoProbe) -> Option<Crop> {
    let mut counts: HashMap<(u32, u32, u32, u32), u32> = HashMap::new();

    for line in stderr.lines() {
        let Some(idx) = line.rfind("crop=") else {
            continue;
        };
        let rest = &line[idx + 5..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '\r' || c == '\n')
            .unwrap_or(rest.len());
        let token = &rest[..end];

        let Some(crop) = parse_cropdetect_crop_token(token) else {
            continue;
        };

        if crop.width == 0 || crop.height == 0 {
            continue;
        }
        if crop.x.saturating_add(crop.width) > probe.width
            || crop.y.saturating_add(crop.height) > probe.height
        {
            continue;
        }

        // Ignore no-op suggestions.
        if crop.x == 0 && crop.y == 0 && crop.width == probe.width && crop.height == probe.height {
            continue;
        }

        // Snap to even values to match the encoder's crop rounding behavior.
        let w = (crop.width / 2) * 2;
        let h = (crop.height / 2) * 2;
        let x = (crop.x / 2) * 2;
        let y = (crop.y / 2) * 2;

        if w == 0 || h == 0 {
            continue;
        }
        if x.saturating_add(w) > probe.width || y.saturating_add(h) > probe.height {
            continue;
        }

        *counts.entry((x, y, w, h)).or_insert(0) += 1;
    }

    let mut best: Option<(u32, u32, u32, u32, u32)> = None;
    for ((x, y, w, h), count) in counts {
        let area = w.saturating_mul(h);
        match best {
            None => best = Some((x, y, w, h, count)),
            Some((bx, by, bw, bh, bc)) => {
                let best_area = bw.saturating_mul(bh);
                if count > bc || (count == bc && area > best_area) {
                    best = Some((x, y, w, h, count));
                } else {
                    best = Some((bx, by, bw, bh, bc));
                }
            }
        }
    }

    best.map(|(x, y, w, h, _)| Crop {
        x,
        y,
        width: w,
        height: h,
    })
}

pub fn detect_crop(path: String) -> Result<Option<Crop>, String> {
    let input_path = PathBuf::from(path.trim());
    if !input_path.exists() {
        return Err(format!("File not found: {}", input_path.display()));
    }
    if !input_path.is_file() {
        return Err("Input path must point to a file.".to_string());
    }

    let probe = probe_video(input_path.to_string_lossy().to_string())?;
    let ffmpeg_bin = default_ffmpeg();

    let mut cmd = command_no_window(&ffmpeg_bin);
    cmd.arg("-hide_banner")
        .arg("-loglevel")
        .arg("info")
        .arg("-nostdin")
        .arg("-i")
        .arg(input_path.as_os_str())
        .arg("-an")
        .arg("-sn")
        .arg("-vf")
        .arg("cropdetect=24:2:0")
        .arg("-frames:v")
        .arg("200")
        .arg("-f")
        .arg("null")
        .arg(null_sink());
    let output = run_command_output_with_timeout(
        cmd,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &ffmpeg_bin,
        "crop detection",
        CROP_DETECT_TIMEOUT,
    )?;

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        let lines: Vec<&str> = stderr.lines().collect();
        let start = lines.len().saturating_sub(20);
        let tail = lines[start..].join("\n");
        return Err(format!("Crop detection failed.\n\n{tail}"));
    }

    Ok(parse_cropdetect_from_stderr(&stderr, &probe))
}

fn estimate_output_duration_s(
    probe_duration_s: f64,
    trim: &Option<Trim>,
    speed: f64,
) -> Result<f64, String> {
    if probe_duration_s <= 0.0 {
        return Err("Invalid duration from probe.".to_string());
    }
    if speed <= 0.0 || !speed.is_finite() {
        return Err("Speed must be > 0.".to_string());
    }

    let mut duration = probe_duration_s;
    if let Some(t) = trim {
        let start = t.start_s.max(0.0);
        let end = t.end_s.unwrap_or(probe_duration_s).min(probe_duration_s);
        if end <= start {
            return Err("Trim end must be greater than start.".to_string());
        }
        duration = end - start;
    }

    Ok(duration / speed)
}

fn plan_bitrates(
    target_size_mb: f64,
    duration_s: f64,
    include_audio: bool,
    preferred_audio_kbps: u32,
    min_audio_kbps: u32,
    min_video_kbps: u32,
    margin: f64,
) -> Result<EncodePlan, String> {
    if target_size_mb <= 0.0 || !target_size_mb.is_finite() {
        return Err("Size limit must be > 0 MB.".to_string());
    }
    if duration_s <= 0.0 || !duration_s.is_finite() {
        return Err("Duration must be > 0.".to_string());
    }

    let target_size_bytes = (target_size_mb * MB_BYTES as f64) as u64;
    let target_bits = (target_size_bytes as f64) * 8.0 * margin;
    let total_kbps_budget = (target_bits / duration_s / 1000.0).floor().max(10.0) as u32;

    if !include_audio {
        return Ok(EncodePlan {
            video_bitrate_kbps: total_kbps_budget.max(min_video_kbps),
            audio_bitrate_kbps: 0,
            include_audio: false,
        });
    }

    let mut audio_kbps = preferred_audio_kbps.max(min_audio_kbps);
    audio_kbps = audio_kbps.min(total_kbps_budget);
    let audio_bits = (audio_kbps as f64) * 1000.0 * duration_s;
    let mut video_bits = target_bits - audio_bits;
    let mut video_kbps = (video_bits / duration_s / 1000.0).floor() as i64;

    if video_kbps < min_video_kbps as i64 {
        let audio_kbps_max = total_kbps_budget.saturating_sub(min_video_kbps);
        if audio_kbps_max < min_audio_kbps {
            return Ok(EncodePlan {
                video_bitrate_kbps: total_kbps_budget.max(min_video_kbps),
                audio_bitrate_kbps: 0,
                include_audio: false,
            });
        }
        audio_kbps = audio_kbps.clamp(min_audio_kbps, audio_kbps_max);
        let audio_bits = (audio_kbps as f64) * 1000.0 * duration_s;
        video_bits = target_bits - audio_bits;
        video_kbps = (video_bits / duration_s / 1000.0).floor() as i64;
    }

    Ok(EncodePlan {
        video_bitrate_kbps: (video_kbps.max(min_video_kbps as i64) as u32),
        audio_bitrate_kbps: audio_kbps,
        include_audio: true,
    })
}

fn plan_audio_only_kbps(
    target_size_mb: f64,
    duration_s: f64,
    margin: f64,
    min_audio_kbps: u32,
    max_audio_kbps: u32,
) -> Result<u32, String> {
    if target_size_mb <= 0.0 || !target_size_mb.is_finite() {
        return Err("Size limit must be > 0 MB.".to_string());
    }
    if duration_s <= 0.0 || !duration_s.is_finite() {
        return Err("Duration must be > 0.".to_string());
    }

    let target_size_bytes = (target_size_mb * MB_BYTES as f64) as u64;
    let target_bits = (target_size_bytes as f64) * 8.0 * margin;
    let kbps_budget = (target_bits / duration_s / 1000.0).floor().max(8.0) as u32;
    Ok(kbps_budget.clamp(min_audio_kbps, max_audio_kbps))
}

fn atempo_chain(speed: f64) -> Result<String, String> {
    if speed <= 0.0 || !speed.is_finite() {
        return Err("Speed must be > 0.".to_string());
    }

    // atempo supports 0.5..2.0. Chain filters for values outside that range.
    let mut parts: Vec<String> = Vec::new();
    let mut remaining = speed;

    while remaining > 2.0 + 1e-9 {
        parts.push("atempo=2.0".to_string());
        remaining /= 2.0;
    }
    while remaining < 0.5 - 1e-9 {
        parts.push("atempo=0.5".to_string());
        remaining /= 0.5;
    }
    parts.push(format!("atempo={remaining:.6}"));

    Ok(parts.join(","))
}

fn color_is_noop(color: &Option<ColorAdjust>) -> bool {
    let Some(c) = color else {
        return true;
    };
    c.brightness.abs() <= 1e-9
        && (c.contrast - 1.0).abs() <= 1e-9
        && (c.saturation - 1.0).abs() <= 1e-9
}

fn max_edge_scale_filter(max_edge_px: u32) -> String {
    // Keep aspect ratio, never upscale, and keep dimensions divisible by 2: the
    // explicit edge needs trunc()/2*2 because min(iw, m) is odd for odd sources
    // or odd caps, and yuv420p encoders reject odd dimensions.
    // Use quotes inside the filter string so commas inside expressions are not treated as filter separators.
    format!(
        "scale=w='if(gte(iw,ih),trunc(min(iw,{m})/2)*2,-2)':h='if(gte(iw,ih),-2,trunc(min(ih,{m})/2)*2)'",
        m = even_at_least_two(max_edge_px)
    )
}

fn custom_scale_filter(width_px: u32, height_px: u32) -> String {
    format!(
        "scale=w={}:h={}",
        even_at_least_two(width_px),
        even_at_least_two(height_px)
    )
}

fn build_video_filters(req: &EncodeRequest, probe: &VideoProbe) -> Result<Option<String>, String> {
    let mut filters: Vec<String> = Vec::new();

    if let Some(t) = &req.trim {
        let start = t.start_s.max(0.0);
        let end = t.end_s.unwrap_or(probe.duration_s).min(probe.duration_s);
        if end <= start {
            return Err("Trim end must be greater than start.".to_string());
        }
        filters.push(format!("trim=start={start}:end={end}"));
        filters.push("setpts=PTS-STARTPTS".to_string());
    }

    if let Some(c) = &req.crop {
        if c.width == 0 || c.height == 0 {
            return Err("Crop width/height must be > 0.".to_string());
        }
        if c.x.saturating_add(c.width) > probe.width || c.y.saturating_add(c.height) > probe.height
        {
            return Err("Crop area is outside the video bounds.".to_string());
        }

        // Help H.264/YUV420 compatibility by snapping to even dimensions.
        let w = (c.width / 2) * 2;
        let h = (c.height / 2) * 2;
        let x = (c.x / 2) * 2;
        let y = (c.y / 2) * 2;

        filters.push(format!("crop=w={w}:h={h}:x={x}:y={y}"));
    }

    match req.rotate_deg {
        0 => {}
        90 => filters.push("transpose=1".to_string()),
        180 => filters.push("transpose=1,transpose=1".to_string()),
        270 => filters.push("transpose=2".to_string()),
        other => {
            return Err(format!(
                "Unsupported rotate value: {other} (use 0/90/180/270)."
            ));
        }
    }

    let resize_plan = requested_resize(req)?;
    match resize_plan {
        ResizePlan::Source => {}
        ResizePlan::MaxEdge(max_edge_px) => filters.push(max_edge_scale_filter(max_edge_px)),
        ResizePlan::Custom {
            width_px,
            height_px,
        } => filters.push(custom_scale_filter(width_px, height_px)),
    }

    // Crop and the scale filters already guarantee even output; an untouched
    // odd-dimension source would otherwise fail yuv420p encoders.
    if req.crop.is_none()
        && resize_plan == ResizePlan::Source
        && (probe.width % 2 == 1 || probe.height % 2 == 1)
    {
        filters.push("crop=w=trunc(iw/2)*2:h=trunc(ih/2)*2".to_string());
    }

    if let Some(c) = &req.color {
        if !c.brightness.is_finite() || !c.contrast.is_finite() || !c.saturation.is_finite() {
            return Err("Color values must be finite numbers.".to_string());
        }
        if !(-1.0..=1.0).contains(&c.brightness) {
            return Err("Brightness must be between -1.0 and 1.0.".to_string());
        }
        if !(0.0..=2.0).contains(&c.contrast) {
            return Err("Contrast must be between 0.0 and 2.0.".to_string());
        }
        if !(0.0..=3.0).contains(&c.saturation) {
            return Err("Saturation must be between 0.0 and 3.0.".to_string());
        }
        if !color_is_noop(&req.color) {
            filters.push(format!(
                "eq=brightness={:.6}:contrast={:.6}:saturation={:.6}",
                c.brightness, c.contrast, c.saturation
            ));
        }
    }

    if req.reverse {
        filters.push("reverse".to_string());
    }

    if (req.speed - 1.0).abs() > 1e-9 {
        filters.push(format!("setpts=PTS/{:.9}", req.speed));
    }

    if let Some(cap_fps) = frame_rate_cap_filter_fps(req, probe)? {
        filters.push(format!("fps={cap_fps}"));
    }

    // Must be LAST: reverse/trim/fps reorder frames, and enable='eq(n,0)' targets
    // the first frame entering this filter, so appending it here lands on the
    // true first OUTPUT frame.
    if let Some(perturb) = first_frame_perturb_filter(req) {
        filters.push(perturb);
    }

    Ok(if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    })
}

/// Noise strength for the first-frame perturbation. Must stay >= 2: a lighter
/// touch (e.g. a single LSB flip, or `alls=1`) is quantized away by the export
/// re-encode and leaves the first frame unchanged. Measured 2026-06-13: alls>=2
/// survives forum-grade compression (CRF 40) while staying imperceptible
/// (PSNR ~35-40 dB). See research/2026-06-13-first-frame-uniqueness.md.
const FIRST_FRAME_PERTURB_STRENGTH: u32 = 3;

/// Builds the timeline-gated uniform-noise filter that imperceptibly perturbs
/// only the first frame. Returns None when the feature is off. The comma inside
/// `eq(n,0)` is backslash-escaped so the filter survives the `,`-join with the
/// rest of the filterchain.
fn first_frame_perturb_filter(req: &EncodeRequest) -> Option<String> {
    if !req.perturb_first_frame {
        return None;
    }
    // all_seed is an int in -1..=INT_MAX; mask into the non-negative i32 range.
    let seed = req.perturb_seed.unwrap_or_else(random_perturb_seed) & (i32::MAX as u32);
    Some(format!(
        "noise=alls={FIRST_FRAME_PERTURB_STRENGTH}:allf=u:all_seed={seed}:enable='eq(n\\,0)'"
    ))
}

/// A fresh per-export seed derived from the wall clock (no extra dependency).
/// Uniqueness, not unpredictability, is the goal, so this need not be a CSPRNG.
fn random_perturb_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0)
}

fn build_audio_filters(req: &EncodeRequest, probe: &VideoProbe) -> Result<Option<String>, String> {
    if !req.audio_enabled || !probe.has_audio {
        return Ok(None);
    }

    let mut filters: Vec<String> = Vec::new();

    if let Some(t) = &req.trim {
        let start = t.start_s.max(0.0);
        let end = t.end_s.unwrap_or(probe.duration_s).min(probe.duration_s);
        if end <= start {
            return Err("Trim end must be greater than start.".to_string());
        }
        filters.push(format!("atrim=start={start}:end={end}"));
        filters.push("asetpts=PTS-STARTPTS".to_string());
    }

    if req.normalize_audio {
        // loudnorm internally upsamples to 192 kHz; bring the rate back down.
        filters.push("loudnorm=I=-16:TP=-1.5:LRA=11".to_string());
        filters.push("aresample=48000".to_string());
    }

    if req.reverse {
        filters.push("areverse".to_string());
    }

    if (req.speed - 1.0).abs() > 1e-9 {
        filters.push(atempo_chain(req.speed)?);
    }

    Ok(if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    })
}

fn null_sink() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn temp_dir_for_job(job_id: u64, attempt: u32) -> Result<PathBuf, String> {
    let base = std::env::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("System time error: {e}"))?
        .as_millis();

    let dir = base.join(format!("video_for_lazies_{job_id}_{attempt}_{now}"));
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;
    Ok(dir)
}

fn parse_out_time_us(line: &str) -> Option<u64> {
    let (key, value) = line.split_once('=')?;
    match key {
        "out_time_us" => value.trim().parse::<u64>().ok(),
        "out_time_ms" => value.trim().parse::<u64>().ok(), // despite the name, ffmpeg reports microseconds here
        _ => None,
    }
}

fn emit_progress(
    window: &Window,
    job_id: u64,
    pass: u8,
    total_passes: u8,
    out_time_us: u64,
    duration_us: u64,
) {
    if duration_us == 0 {
        return;
    }
    let pass_pct = (out_time_us as f64 / duration_us as f64).clamp(0.0, 1.0);
    let overall_pct = if total_passes <= 1 {
        pass_pct
    } else {
        (((pass.saturating_sub(1)) as f64) + pass_pct) / (total_passes as f64)
    };

    let _ = window.emit(
        "encode-progress",
        EncodeProgressPayload {
            job_id,
            pass,
            total_passes,
            pass_pct,
            overall_pct,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn run_ffmpeg_with_progress(
    window: &Window,
    job_id: u64,
    ffmpeg_bin: &str,
    args: &[String],
    pass: u8,
    total_passes: u8,
    duration_us: u64,
    cancel: &AtomicBool,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Canceled.".to_string());
    }

    let mut cmd = command_no_window(ffmpeg_bin);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            binary_not_found_message("ffmpeg", "VFL_FFMPEG_PATH", ffmpeg_bin)
        } else {
            format!("Failed to start ffmpeg ({ffmpeg_bin}): {e}")
        }
    })?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture ffmpeg output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture ffmpeg error output.".to_string())?;

    {
        let mut guard = child_slot
            .lock()
            .map_err(|_| "Internal error (child lock poisoned).".to_string())?;
        *guard = Some(child);
    }

    let stderr_tail: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let stderr_tail_thread = stderr_tail.clone();
    let stderr_thread = std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line_res in reader.lines() {
            let Ok(line) = line_res else {
                break;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(mut tail) = stderr_tail_thread.lock() {
                tail.push(line.to_string());
                if tail.len() > 80 {
                    tail.remove(0);
                }
            }
        }
    });

    let process_done = Arc::new(AtomicBool::new(false));
    let idle_timed_out = Arc::new(AtomicBool::new(false));
    let last_output_at = Arc::new(Mutex::new(Instant::now()));
    let watchdog_child_slot = child_slot.clone();
    let watchdog_done = process_done.clone();
    let watchdog_timed_out = idle_timed_out.clone();
    let watchdog_last_output = last_output_at.clone();
    let watchdog_thread = std::thread::spawn(move || {
        while !watchdog_done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
            let idle_for = watchdog_last_output
                .lock()
                .ok()
                .map(|last_output| last_output.elapsed())
                .unwrap_or_default();
            if idle_for >= ENCODE_IDLE_TIMEOUT {
                watchdog_timed_out.store(true, Ordering::Relaxed);
                if let Ok(mut guard) = watchdog_child_slot.lock()
                    && let Some(child) = guard.as_mut()
                {
                    let _ = child.kill();
                }
                break;
            }
        }
    });

    let mut last_emit = Instant::now() - PROGRESS_EMIT_EVERY;
    let reader = BufReader::new(stdout);
    let mut read_error: Option<String> = None;

    for line_res in reader.lines() {
        if cancel.load(Ordering::Relaxed) {
            if let Ok(mut guard) = child_slot.lock()
                && let Some(child) = guard.as_mut()
            {
                let _ = child.kill();
            }
            break;
        }

        let line = match line_res {
            Ok(line) => line,
            Err(e) => {
                read_error = Some(format!("Failed reading ffmpeg output: {e}"));
                if let Ok(mut guard) = child_slot.lock()
                    && let Some(child) = guard.as_mut()
                {
                    let _ = child.kill();
                }
                break;
            }
        };
        if let Ok(mut last_output) = last_output_at.lock() {
            *last_output = Instant::now();
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(out_time_us) = parse_out_time_us(line) {
            let now = Instant::now();
            if now.duration_since(last_emit) >= PROGRESS_EMIT_EVERY {
                last_emit = now;
                emit_progress(window, job_id, pass, total_passes, out_time_us, duration_us);
            }
            continue;
        }
    }

    let status = {
        let mut guard = child_slot
            .lock()
            .map_err(|_| "Internal error (child lock poisoned).".to_string())?;
        let mut child = guard
            .take()
            .ok_or_else(|| "ffmpeg child missing.".to_string())?;
        child
            .wait()
            .map_err(|e| format!("Failed waiting for ffmpeg: {e}"))?
    };

    process_done.store(true, Ordering::Relaxed);
    let _ = watchdog_thread.join();
    let _ = stderr_thread.join();
    let tail_snapshot = stderr_tail
        .lock()
        .ok()
        .map(|t| t.clone())
        .unwrap_or_default();

    if cancel.load(Ordering::Relaxed) {
        return Err("Canceled.".to_string());
    }

    if idle_timed_out.load(Ordering::Relaxed) {
        return Err(format!(
            "ffmpeg made no progress for {} seconds and was stopped.",
            ENCODE_IDLE_TIMEOUT.as_secs()
        ));
    }

    if let Some(error) = read_error {
        return Err(error);
    }

    if !status.success() {
        let start = tail_snapshot.len().saturating_sub(15);
        let tail_str = tail_snapshot[start..].join("\n");
        return Err(format!(
            "ffmpeg failed (exit {}).\n\n{}",
            status.code().unwrap_or(-1),
            tail_str
        ));
    }

    emit_progress(window, job_id, pass, total_passes, duration_us, duration_us);
    Ok(())
}

fn shell_preview_arg(arg: &str) -> String {
    if arg.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '-' | '_' | '.' | ':' | '/' | '=' | '+' | ',' | '?')
    }) {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn ffmpeg_command_preview(args: &[String], replacements: &[(&str, &str)]) -> String {
    let preview_args = args
        .iter()
        .map(|arg| {
            let mut next = arg.clone();
            if next.starts_with("title=") {
                next = "title=<title>".to_string();
            } else {
                for (needle, replacement) in replacements {
                    if !needle.is_empty() {
                        next = next.replace(needle, replacement);
                    }
                }
            }
            shell_preview_arg(&next)
        })
        .collect::<Vec<_>>()
        .join(" ");

    format!("ffmpeg {preview_args}")
}

fn map_mpeg4_size_limit_error(
    format: OutputFormat,
    codec: VideoCodec,
    target_size_mb: f64,
    width: u32,
    height: u32,
    error: String,
) -> String {
    if format == OutputFormat::Mp4 && codec == VideoCodec::Mpeg4 {
        let lowered = error.to_ascii_lowercase();
        if lowered.contains("requested bitrate is too low")
            || lowered.contains("bitrate too low for this video with these parameters")
        {
            return format!(
                "The bundled MP4 fallback codec cannot encode {width}x{height} video within a {target_size_mb:.2} MB size target. Try a larger size limit, smaller output dimensions, reduce crop, or export as WebM."
            );
        }
    }

    error
}

fn build_size_limited_encode_contract(
    request: &EncodeRequest,
    probe: &VideoProbe,
    selected_video_codec: VideoCodec,
    output_duration_s: f64,
    include_audio: bool,
) -> Result<SizeLimitedEncodeContract, String> {
    let (planned_width, planned_height) = estimated_output_dimensions(request, probe)?;
    let planned_frame_rate = output_frame_rate_for_planning(request, probe)?;
    let min_video_kbps = minimum_video_bitrate_kbps(
        selected_video_codec,
        planned_width,
        planned_height,
        planned_frame_rate,
    );
    let plan = plan_bitrates(
        request.size_limit_mb,
        output_duration_s,
        include_audio,
        96,
        32,
        min_video_kbps,
        0.95,
    )?;
    let audio_removed_for_size_target = include_audio && !plan.include_audio;

    Ok(SizeLimitedEncodeContract {
        planned_width,
        planned_height,
        min_video_kbps,
        plan,
        audio_removed_for_size_target,
    })
}

fn size_limited_completion_message(
    met_target: bool,
    selected_video_codec: VideoCodec,
    video_bitrate_kbps: u32,
    min_video_kbps: u32,
    audio_removed_for_size_target: bool,
) -> Option<String> {
    let mut messages = Vec::new();

    if !met_target {
        if selected_video_codec == VideoCodec::Mpeg4 && video_bitrate_kbps <= min_video_kbps {
            messages.push("Still above size limit. The bundled MP4 fallback codec cannot go lower at this resolution. Try a larger size limit, WebM, or smaller output dimensions.");
        } else {
            messages.push("Still above size limit.");
        }
    }

    if audio_removed_for_size_target {
        messages.push("Audio was removed to fit the size target.");
    }

    (!messages.is_empty()).then(|| messages.join(" "))
}

pub fn run_encode_job(
    window: &Window,
    job_id: u64,
    cancel: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    request: EncodeRequest,
) -> Result<EncodeFinishedPayload, String> {
    let input_path = PathBuf::from(request.input_path.trim());
    if !input_path.exists() {
        return Err(format!("File not found: {}", input_path.display()));
    }
    if !input_path.is_file() {
        return Err("Input path must point to a file.".to_string());
    }

    if request.speed <= 0.0 || !request.speed.is_finite() {
        return Err("Speed must be > 0.".to_string());
    }

    if request.size_limit_mb < 0.0 || !request.size_limit_mb.is_finite() {
        return Err("Size limit must be >= 0 MB.".to_string());
    }

    let size_limit_enabled = request.size_limit_mb > 0.0;
    if size_limit_enabled && request.size_limit_mb < 0.1 {
        return Err("Size limit must be >= 0.1 MB (or 0 to disable).".to_string());
    }
    let target_bytes = (request.size_limit_mb * MB_BYTES as f64) as u64;

    let output_path = validate_output_path(
        &input_path,
        &PathBuf::from(request.output_path.trim()),
        output_extension(request.format),
    )?;

    let input_str = input_path.to_string_lossy().to_string();

    let ffmpeg_bin = default_ffmpeg();
    let probe = probe_video(input_path.to_string_lossy().to_string())?;
    let encoder_names = cached_encoder_names(&ffmpeg_bin)?;
    let codec_selection = select_codec_plan(request.format, &encoder_names, &request.advanced)?;
    let advanced_audio_bitrate_kbps = advanced_audio_bitrate_kbps(&request)?;
    let advanced_audio_channel_count = audio_channel_count(&request);
    let advanced_force_reencode = advanced_forces_reencode(&request, &probe, size_limit_enabled)?;

    let output_duration_s =
        estimate_output_duration_s(probe.duration_s, &request.trim, request.speed)?;
    let duration_us = (output_duration_s * 1_000_000.0).max(1.0) as u64;

    let include_audio =
        request.audio_enabled && probe.has_audio && !matches!(request.format, OutputFormat::Mp3);
    let audio_filters = build_audio_filters(&request, &probe)?;
    let video_filters = if matches!(request.format, OutputFormat::Mp3) {
        None
    } else {
        build_video_filters(&request, &probe)?
    };

    if matches!(request.format, OutputFormat::Mp3) {
        let audio_codec = codec_selection
            .audio_codec
            .unwrap_or(AudioCodec::LibMp3Lame)
            .as_ffmpeg_name();
        if !request.audio_enabled {
            return Err("Audio must be enabled for MP3 output.".to_string());
        }
        if !probe.has_audio {
            return Err("Input file has no audio stream.".to_string());
        }

        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input_str.clone(),
            "-progress".to_string(),
            "pipe:1".to_string(),
            "-nostats".to_string(),
            "-map".to_string(),
            "0:a:0".to_string(),
            "-vn".to_string(),
        ];

        if let Some(af) = audio_filters {
            args.push("-af".to_string());
            args.push(af);
        }

        args.push("-c:a".to_string());
        args.push(audio_codec.to_string());
        if let Some(channels) = advanced_audio_channel_count {
            args.push("-ac".to_string());
            args.push(channels.to_string());
        }
        let selected_audio_bitrate_kbps = if size_limit_enabled {
            let audio_kbps =
                plan_audio_only_kbps(request.size_limit_mb, output_duration_s, 0.98, 32, 320)?;
            args.push("-b:a".to_string());
            args.push(format!("{audio_kbps}k"));
            Some(audio_kbps)
        } else if let Some(audio_kbps) = advanced_audio_bitrate_kbps {
            args.push("-b:a".to_string());
            args.push(format!("{audio_kbps}k"));
            Some(audio_kbps)
        } else {
            args.push("-q:a".to_string());
            args.push("2".to_string());
            None
        };

        push_metadata_args(&mut args, &request);

        let temp_path = temp_output_path(&output_path, job_id, "mp3")?;
        args.push(temp_path.to_string_lossy().to_string());
        let temp_str = temp_path.to_string_lossy().to_string();
        let command_preview =
            ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

        if let Err(error) = run_ffmpeg_with_progress(
            window,
            job_id,
            &ffmpeg_bin,
            &args,
            1,
            1,
            duration_us,
            cancel.as_ref(),
            child_slot,
        ) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        let out_size = fs::metadata(&temp_path)
            .map_err(|e| format!("Failed to stat output file: {e}"))?
            .len();
        publish_output_file(&temp_path, &output_path)?;

        return Ok(EncodeFinishedPayload {
            job_id,
            ok: true,
            output_path: Some(output_path.to_string_lossy().to_string()),
            output_size_bytes: Some(out_size),
            message: None,
            diagnostics: Some(ExportDiagnostics {
                mode: "Audio-only encode".to_string(),
                video_codec: None,
                audio_codec: Some(audio_codec.to_string()),
                video_bitrate_kbps: None,
                audio_bitrate_kbps: selected_audio_bitrate_kbps,
                requested_size_bytes: size_limit_enabled.then_some(target_bytes),
                actual_size_bytes: Some(out_size),
                passes: 1,
                attempts: 1,
                audio_removed_for_size_target: false,
                command_preview,
            }),
        });
    }

    // Fast path: no-op transforms (avoid inflating already-small files).
    let no_op_transforms = request.trim.is_none()
        && request.crop.is_none()
        && !request.reverse
        && request.rotate_deg == 0
        && (request.speed - 1.0).abs() <= 1e-9
        && matches!(requested_resize(&request)?, ResizePlan::Source)
        && color_is_noop(&request.color)
        // First-frame perturbation requires a re-encode; never stream-copy.
        && !request.perturb_first_frame;

    if no_op_transforms {
        let input_size = fs::metadata(&input_path)
            .map_err(|e| format!("Failed to stat input file: {e}"))?
            .len();

        let should_try_stream_copy = if size_limit_enabled {
            input_size <= target_bytes || !request.audio_enabled
        } else {
            true
        } && !advanced_force_reencode;

        if should_try_stream_copy {
            let mut args = vec![
                "-y".to_string(),
                "-hide_banner".to_string(),
                "-loglevel".to_string(),
                "error".to_string(),
                "-i".to_string(),
                input_str.clone(),
                "-progress".to_string(),
                "pipe:1".to_string(),
                "-nostats".to_string(),
                "-map".to_string(),
                "0:v:0".to_string(),
                "-c:v".to_string(),
                "copy".to_string(),
            ];

            if request.audio_enabled && probe.has_audio {
                args.push("-map".to_string());
                args.push("0:a:0?".to_string());
                args.push("-c:a".to_string());
                args.push("copy".to_string());
            } else {
                args.push("-an".to_string());
            }

            if let Some(title) = request
                .title
                .as_deref()
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
            {
                args.push("-metadata".to_string());
                args.push(format!("title={title}"));
            }

            if matches!(request.format, OutputFormat::Mp4) {
                args.push("-movflags".to_string());
                args.push("+faststart".to_string());
            }

            let temp_path = temp_output_path(&output_path, job_id, "copy")?;
            args.push(temp_path.to_string_lossy().to_string());
            let temp_str = temp_path.to_string_lossy().to_string();
            let command_preview =
                ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

            if run_ffmpeg_with_progress(
                window,
                job_id,
                &ffmpeg_bin,
                &args,
                1,
                1,
                duration_us,
                cancel.as_ref(),
                child_slot,
            )
            .is_ok()
            {
                let out_size = fs::metadata(&temp_path)
                    .map_err(|e| format!("Failed to stat output file: {e}"))?
                    .len();
                if !size_limit_enabled || out_size <= target_bytes {
                    publish_output_file(&temp_path, &output_path)?;
                    return Ok(EncodeFinishedPayload {
                        job_id,
                        ok: true,
                        output_path: Some(output_path.to_string_lossy().to_string()),
                        output_size_bytes: Some(out_size),
                        message: None,
                        diagnostics: Some(ExportDiagnostics {
                            mode: "Stream copy".to_string(),
                            video_codec: Some("copy".to_string()),
                            audio_codec: Some(
                                if request.audio_enabled && probe.has_audio {
                                    "copy"
                                } else {
                                    "none"
                                }
                                .to_string(),
                            ),
                            video_bitrate_kbps: None,
                            audio_bitrate_kbps: None,
                            requested_size_bytes: size_limit_enabled.then_some(target_bytes),
                            actual_size_bytes: Some(out_size),
                            passes: 1,
                            attempts: 1,
                            audio_removed_for_size_target: false,
                            command_preview,
                        }),
                    });
                }
            }
            let _ = fs::remove_file(&temp_path);
        }
    }

    if !size_limit_enabled {
        let video_codec = codec_selection
            .video_codec
            .ok_or_else(|| "Missing video codec selection.".to_string())?
            .as_ffmpeg_name();
        let audio_codec = codec_selection
            .audio_codec
            .unwrap_or(AudioCodec::Aac)
            .as_ffmpeg_name();
        let quality_mode = codec_selection
            .quality_mode
            .ok_or_else(|| "Missing quality settings for export format.".to_string())?;
        let selected_video_codec = codec_selection
            .video_codec
            .ok_or_else(|| "Missing video codec selection.".to_string())?;
        let encode_speed_preference = encode_speed_preference(&request.advanced);
        let mut selected_audio_bitrate_kbps: Option<u32> = None;

        let mut args = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input_str.clone(),
            "-progress".to_string(),
            "pipe:1".to_string(),
            "-nostats".to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-c:v".to_string(),
            video_codec.to_string(),
        ];

        match quality_mode {
            QualityMode::Crf { crf, preset } => {
                args.extend(["-crf", crf].into_iter().map(|s| s.to_string()));
                if let Some(preset) = preset
                    && encode_speed_preference == EncodeSpeedPreference::Auto
                {
                    args.extend(["-preset", preset].into_iter().map(|s| s.to_string()));
                }
            }
            QualityMode::Cq { crf, bitrate } => {
                args.extend(
                    ["-crf", crf, "-b:v", bitrate]
                        .into_iter()
                        .map(|s| s.to_string()),
                );
            }
            QualityMode::QScale { qscale } => {
                args.extend(["-q:v", qscale].into_iter().map(|s| s.to_string()));
            }
        }
        args.extend(encode_speed_args_for_codec(
            selected_video_codec,
            encode_speed_preference,
        ));

        if let Some(vf) = &video_filters {
            args.push("-vf".to_string());
            args.push(vf.clone());
        }

        if include_audio && probe.has_audio && request.audio_enabled {
            args.extend(["-map", "0:a:0?"].into_iter().map(|s| s.to_string()));
            args.extend(["-c:a", audio_codec].into_iter().map(|s| s.to_string()));
            if let Some(channels) = advanced_audio_channel_count {
                args.push("-ac".to_string());
                args.push(channels.to_string());
            }
            args.push("-b:a".to_string());
            let audio_kbps = advanced_audio_bitrate_kbps.unwrap_or(match request.format {
                OutputFormat::Mp4 => 192,
                OutputFormat::Webm => 128,
                OutputFormat::Mp3 => unreachable!(),
            });
            selected_audio_bitrate_kbps = Some(audio_kbps);
            args.push(format!("{audio_kbps}k"));
            if let Some(af) = &audio_filters {
                args.push("-af".to_string());
                args.push(af.clone());
            }
        } else {
            args.push("-an".to_string());
        }

        push_metadata_args(&mut args, &request);

        if matches!(request.format, OutputFormat::Mp4) {
            args.extend(
                ["-movflags", "+faststart", "-pix_fmt", "yuv420p"]
                    .into_iter()
                    .map(|s| s.to_string()),
            );
        }

        let temp_path = temp_output_path(&output_path, job_id, "encode")?;
        args.push(temp_path.to_string_lossy().to_string());
        let temp_str = temp_path.to_string_lossy().to_string();
        let command_preview =
            ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

        if let Err(error) = run_ffmpeg_with_progress(
            window,
            job_id,
            &ffmpeg_bin,
            &args,
            1,
            1,
            duration_us,
            cancel.as_ref(),
            child_slot,
        ) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        let out_size = fs::metadata(&temp_path)
            .map_err(|e| format!("Failed to stat output file: {e}"))?
            .len();
        publish_output_file(&temp_path, &output_path)?;

        return Ok(EncodeFinishedPayload {
            job_id,
            ok: true,
            output_path: Some(output_path.to_string_lossy().to_string()),
            output_size_bytes: Some(out_size),
            message: None,
            diagnostics: Some(ExportDiagnostics {
                mode: "Video re-encode".to_string(),
                video_codec: Some(video_codec.to_string()),
                audio_codec: Some(
                    if include_audio && probe.has_audio && request.audio_enabled {
                        audio_codec
                    } else {
                        "none"
                    }
                    .to_string(),
                ),
                video_bitrate_kbps: None,
                audio_bitrate_kbps: selected_audio_bitrate_kbps,
                requested_size_bytes: None,
                actual_size_bytes: Some(out_size),
                passes: 1,
                attempts: 1,
                audio_removed_for_size_target: false,
                command_preview,
            }),
        });
    }

    let selected_video_codec = codec_selection
        .video_codec
        .ok_or_else(|| "Missing video codec selection.".to_string())?;
    let contract = build_size_limited_encode_contract(
        &request,
        &probe,
        selected_video_codec,
        output_duration_s,
        include_audio,
    )?;
    let planned_width = contract.planned_width;
    let planned_height = contract.planned_height;
    let min_video_kbps = contract.min_video_kbps;
    let audio_removed_for_size_target = contract.audio_removed_for_size_target;
    let mut plan = contract.plan;

    let video_codec = selected_video_codec.as_ffmpeg_name();
    let audio_codec = codec_selection
        .audio_codec
        .unwrap_or(AudioCodec::Aac)
        .as_ffmpeg_name();
    let encode_speed_preference = encode_speed_preference(&request.advanced);

    let mut attempt: u32 = 0;
    let max_attempts: u32 = 3;
    loop {
        attempt += 1;

        if cancel.load(Ordering::Relaxed) {
            return Err("Canceled.".to_string());
        }

        let temp_dir = temp_dir_for_job(job_id, attempt)?;
        let passlog_prefix = temp_dir.join("ffmpeg2pass");
        let passlog_str = passlog_prefix.to_string_lossy().to_string();

        // Pass 1
        let mut pass1 = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input_str.clone(),
            "-progress".to_string(),
            "pipe:1".to_string(),
            "-nostats".to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-c:v".to_string(),
            video_codec.to_string(),
            "-b:v".to_string(),
            format!("{}k", plan.video_bitrate_kbps.max(min_video_kbps)),
            "-pass".to_string(),
            "1".to_string(),
            "-passlogfile".to_string(),
            passlog_str.clone(),
            "-an".to_string(),
        ];
        pass1.extend(encode_speed_args_for_codec(
            selected_video_codec,
            encode_speed_preference,
        ));
        if let Some(vf) = &video_filters {
            pass1.push("-vf".to_string());
            pass1.push(vf.clone());
        }

        pass1.push("-f".to_string());
        pass1.push("null".to_string());
        pass1.push(null_sink().to_string());

        if let Err(error) = run_ffmpeg_with_progress(
            window,
            job_id,
            &ffmpeg_bin,
            &pass1,
            1,
            2,
            duration_us,
            cancel.as_ref(),
            child_slot,
        ) {
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(map_mpeg4_size_limit_error(
                request.format,
                selected_video_codec,
                request.size_limit_mb,
                planned_width,
                planned_height,
                error,
            ));
        }

        // Pass 2
        let mut pass2 = vec![
            "-y".to_string(),
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-i".to_string(),
            input_str.clone(),
            "-progress".to_string(),
            "pipe:1".to_string(),
            "-nostats".to_string(),
            "-map".to_string(),
            "0:v:0".to_string(),
            "-c:v".to_string(),
            video_codec.to_string(),
            "-b:v".to_string(),
            format!("{}k", plan.video_bitrate_kbps.max(min_video_kbps)),
            "-pass".to_string(),
            "2".to_string(),
            "-passlogfile".to_string(),
            passlog_str.clone(),
        ];
        pass2.extend(encode_speed_args_for_codec(
            selected_video_codec,
            encode_speed_preference,
        ));

        if let Some(vf) = &video_filters {
            pass2.push("-vf".to_string());
            pass2.push(vf.clone());
        }

        let pass2_audio_bitrate_kbps =
            if plan.include_audio && probe.has_audio && request.audio_enabled {
                Some(plan.audio_bitrate_kbps.max(32))
            } else {
                None
            };

        if pass2_audio_bitrate_kbps.is_some() {
            pass2.extend(["-map", "0:a:0?"].into_iter().map(|s| s.to_string()));
            pass2.extend(["-c:a", audio_codec].into_iter().map(|s| s.to_string()));
            if let Some(channels) = advanced_audio_channel_count {
                pass2.push("-ac".to_string());
                pass2.push(channels.to_string());
            }
            pass2.push("-b:a".to_string());
            pass2.push(format!("{}k", pass2_audio_bitrate_kbps.unwrap_or(32)));
            if let Some(af) = &audio_filters {
                pass2.push("-af".to_string());
                pass2.push(af.clone());
            }
        } else {
            pass2.push("-an".to_string());
        }

        push_metadata_args(&mut pass2, &request);

        if matches!(request.format, OutputFormat::Mp4) {
            pass2.extend(
                ["-movflags", "+faststart", "-pix_fmt", "yuv420p"]
                    .into_iter()
                    .map(|s| s.to_string()),
            );
        }

        let temp_output = temp_output_path(&output_path, job_id, &format!("pass2-{attempt}"))?;
        pass2.push(temp_output.to_string_lossy().to_string());
        let temp_output_str = temp_output.to_string_lossy().to_string();
        let command_preview = ffmpeg_command_preview(
            &pass2,
            &[
                (&input_str, "<input>"),
                (&temp_output_str, "<output>"),
                (&passlog_str, "<passlog>"),
            ],
        );

        if let Err(error) = run_ffmpeg_with_progress(
            window,
            job_id,
            &ffmpeg_bin,
            &pass2,
            2,
            2,
            duration_us,
            cancel.as_ref(),
            child_slot,
        ) {
            let _ = fs::remove_file(&temp_output);
            let _ = fs::remove_dir_all(&temp_dir);
            return Err(map_mpeg4_size_limit_error(
                request.format,
                selected_video_codec,
                request.size_limit_mb,
                planned_width,
                planned_height,
                error,
            ));
        }

        let out_size = fs::metadata(&temp_output)
            .map_err(|e| format!("Failed to stat output file: {e}"))?
            .len();

        let met_target = out_size <= target_bytes;
        let _ = fs::remove_dir_all(&temp_dir);

        if met_target || attempt >= max_attempts {
            publish_output_file(&temp_output, &output_path)?;
            return Ok(EncodeFinishedPayload {
                job_id,
                ok: true,
                output_path: Some(output_path.to_string_lossy().to_string()),
                output_size_bytes: Some(out_size),
                message: size_limited_completion_message(
                    met_target,
                    selected_video_codec,
                    plan.video_bitrate_kbps,
                    min_video_kbps,
                    audio_removed_for_size_target,
                ),
                diagnostics: Some(ExportDiagnostics {
                    mode: "Size-targeted two-pass encode".to_string(),
                    video_codec: Some(video_codec.to_string()),
                    audio_codec: Some(
                        pass2_audio_bitrate_kbps
                            .map(|_| audio_codec)
                            .unwrap_or("none")
                            .to_string(),
                    ),
                    video_bitrate_kbps: Some(plan.video_bitrate_kbps.max(min_video_kbps)),
                    audio_bitrate_kbps: pass2_audio_bitrate_kbps,
                    requested_size_bytes: Some(target_bytes),
                    actual_size_bytes: Some(out_size),
                    passes: 2,
                    attempts: attempt,
                    audio_removed_for_size_target,
                    command_preview,
                }),
            });
        }

        // Adjust bitrate and retry.
        let reduction_factor = (target_bytes as f64 / out_size as f64) * 0.95;
        let new_video_kbps = ((plan.video_bitrate_kbps as f64) * reduction_factor).floor() as u32;
        let next_video_kbps = new_video_kbps.max(min_video_kbps);
        if next_video_kbps >= plan.video_bitrate_kbps {
            publish_output_file(&temp_output, &output_path)?;
            return Ok(EncodeFinishedPayload {
                job_id,
                ok: true,
                output_path: Some(output_path.to_string_lossy().to_string()),
                output_size_bytes: Some(out_size),
                message: size_limited_completion_message(
                    false,
                    selected_video_codec,
                    plan.video_bitrate_kbps,
                    min_video_kbps,
                    audio_removed_for_size_target,
                ),
                diagnostics: Some(ExportDiagnostics {
                    mode: "Size-targeted two-pass encode".to_string(),
                    video_codec: Some(video_codec.to_string()),
                    audio_codec: Some(
                        pass2_audio_bitrate_kbps
                            .map(|_| audio_codec)
                            .unwrap_or("none")
                            .to_string(),
                    ),
                    video_bitrate_kbps: Some(plan.video_bitrate_kbps.max(min_video_kbps)),
                    audio_bitrate_kbps: pass2_audio_bitrate_kbps,
                    requested_size_bytes: Some(target_bytes),
                    actual_size_bytes: Some(out_size),
                    passes: 2,
                    attempts: attempt,
                    audio_removed_for_size_target,
                    command_preview,
                }),
            });
        }
        let _ = fs::remove_file(&temp_output);
        plan.video_bitrate_kbps = next_video_kbps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request() -> EncodeRequest {
        EncodeRequest {
            input_path: "in.mp4".to_string(),
            output_path: "out.mp4".to_string(),
            format: OutputFormat::Mp4,
            title: None,
            size_limit_mb: 8.0,
            audio_enabled: true,
            normalize_audio: false,
            strip_metadata: false,
            advanced: AdvancedEncodeSettings::default(),
            trim: None,
            crop: None,
            reverse: false,
            speed: 1.0,
            rotate_deg: 0,
            resize: None,
            max_edge_px: None,
            color: None,
            perturb_first_frame: false,
            perturb_seed: None,
        }
    }

    fn probe_10s_1920x1080_audio() -> VideoProbe {
        VideoProbe {
            duration_s: 10.0,
            width: 1920,
            height: 1080,
            frame_rate: Some(30.0),
            has_audio: true,
        }
    }

    #[test]
    fn split_sequence_stem_avoids_year_like_suffix() {
        let (base, n) = split_sequence_stem("coolvideo");
        assert_eq!(base, "coolvideo");
        assert_eq!(n, 2);

        let (base, n) = split_sequence_stem("coolvideo-2");
        assert_eq!(base, "coolvideo");
        assert_eq!(n, 3);

        let (base, n) = split_sequence_stem("movie-2024");
        assert_eq!(base, "movie-2024");
        assert_eq!(n, 2);
    }

    #[test]
    fn suggest_output_path_unique_skips_existing_files() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_millis(0))
            .as_millis();
        let dir = std::env::temp_dir().join(format!("vfl_test_out_{now}"));
        fs::create_dir_all(&dir).unwrap();

        let input = dir.join("myvideo.mp4");
        fs::write(&input, b"test").unwrap();

        fs::write(dir.join("myvideo-2.mp4"), b"out").unwrap();
        fs::write(dir.join("myvideo-3.mp4"), b"out").unwrap();

        let suggested =
            suggest_output_path_unique(input.to_string_lossy().to_string(), OutputFormat::Mp4, &[])
                .unwrap();
        assert!(suggested.ends_with("myvideo-4.mp4"), "{suggested}");

        // Queue snapshots claim paths that do not exist on disk yet.
        let taken = vec![suggested];
        let next = suggest_output_path_unique(
            input.to_string_lossy().to_string(),
            OutputFormat::Mp4,
            &taken,
        )
        .unwrap();
        assert!(next.ends_with("myvideo-5.mp4"), "{next}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffmpeg_command_preview_redacts_local_paths_and_metadata_title() {
        let args = vec![
            "-i".to_string(),
            "/tmp/source secret/input.mp4".to_string(),
            "-metadata".to_string(),
            "title=Private clip".to_string(),
            "-passlogfile".to_string(),
            "/tmp/source secret/passlog".to_string(),
            "/tmp/source secret/output.mp4".to_string(),
        ];

        let preview = ffmpeg_command_preview(
            &args,
            &[
                ("/tmp/source secret/input.mp4", "<input>"),
                ("/tmp/source secret/output.mp4", "<output>"),
                ("/tmp/source secret/passlog", "<passlog>"),
            ],
        );

        assert!(preview.contains("<input>"));
        assert!(preview.contains("<output>"));
        assert!(preview.contains("<passlog>"));
        assert!(preview.contains("title=<title>"));
        assert!(!preview.contains("/tmp/source secret"));
        assert!(!preview.contains("Private clip"));
    }

    #[test]
    fn validate_output_path_rejects_unsafe_destinations() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_millis(0))
            .as_millis();
        let dir = std::env::temp_dir().join(format!("vfl_test_validate_out_{now}"));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.mp4");
        let existing = dir.join("existing.mp4");
        fs::write(&input, b"input").unwrap();
        fs::write(&existing, b"existing").unwrap();

        let same_file_err = validate_output_path(&input, &input, "mp4").unwrap_err();
        assert!(same_file_err.contains("different"));

        let existing_err = validate_output_path(&input, &existing, "mp4").unwrap_err();
        assert!(existing_err.contains("already exists"));

        let extension_err =
            validate_output_path(&input, &dir.join("output.txt"), "mp4").unwrap_err();
        assert!(extension_err.contains(".mp4"));

        let valid = validate_output_path(&input, &dir.join("output.mp4"), "mp4").unwrap();
        assert_eq!(
            valid.file_name().and_then(|name| name.to_str()),
            Some("output.mp4")
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn publish_output_file_does_not_overwrite_existing_destination() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_millis(0))
            .as_millis();
        let dir = std::env::temp_dir().join(format!("vfl_test_publish_out_{now}"));
        fs::create_dir_all(&dir).unwrap();
        let temp = dir.join(".output.tmp.mp4");
        let dest = dir.join("output.mp4");
        fs::write(&temp, b"new").unwrap();
        fs::write(&dest, b"old").unwrap();

        let err = publish_output_file(&temp, &dest).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        assert!(!temp.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_binary_path_prefers_env_override_then_sidecar_then_fallback() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::from_millis(0))
            .as_millis();
        let dir = std::env::temp_dir().join(format!("vfl_test_sidecar_{now}"));
        fs::create_dir_all(&dir).unwrap();
        let sidecar = dir.join("ffmpeg.exe");
        fs::write(&sidecar, b"binary").unwrap();

        assert_eq!(
            resolve_binary_path(
                Some(r"C:\Custom Tools\ffmpeg.exe".to_string()),
                Some(sidecar.clone()),
                "ffmpeg"
            ),
            r"C:\Custom Tools\ffmpeg.exe"
        );

        assert_eq!(
            resolve_binary_path(None, Some(sidecar.clone()), "ffmpeg"),
            sidecar.to_string_lossy().to_string()
        );

        assert_eq!(resolve_binary_path(None, None, "ffmpeg"), "ffmpeg");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_sidecar_candidate_uses_release_sidecar_layout() {
        let executable_dir = PathBuf::from(r"C:\Example\Documents");
        let candidate =
            bundled_sidecar_candidate(Some(&executable_dir), "ffprobe", ".exe").unwrap();
        assert_eq!(
            candidate,
            executable_dir.join(FFMPEG_SIDECAR_DIR).join("ffprobe.exe")
        );

        let linux_candidate =
            bundled_sidecar_candidate(Some(&executable_dir), "ffprobe", "").unwrap();
        assert_eq!(
            linux_candidate,
            executable_dir.join(FFMPEG_SIDECAR_DIR).join("ffprobe")
        );
    }

    #[test]
    fn parse_encoder_names_ignores_legend_lines() {
        let parsed = parse_encoder_names(
            r#"
Encoders:
 V..... = Video
 A..... = Audio
 V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 V....D mpeg4                MPEG-4 part 2
 A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3)
"#,
        );

        assert!(parsed.contains("libx264"));
        assert!(parsed.contains("mpeg4"));
        assert!(parsed.contains("libmp3lame"));
        assert!(!parsed.contains("="));
    }

    #[test]
    fn select_codec_plan_falls_back_without_libx264() {
        let encoder_names = HashSet::from([
            "mpeg4".to_string(),
            "aac".to_string(),
            "libvpx-vp9".to_string(),
            "libopus".to_string(),
        ]);

        let mp4_plan = select_codec_plan(
            OutputFormat::Mp4,
            &encoder_names,
            &AdvancedEncodeSettings::default(),
        )
        .unwrap();
        assert_eq!(mp4_plan.video_codec, Some(VideoCodec::Mpeg4));
        assert_eq!(mp4_plan.audio_codec, Some(AudioCodec::Aac));
        assert_eq!(
            mp4_plan.quality_mode,
            Some(QualityMode::QScale { qscale: "5" })
        );

        let webm_plan = select_codec_plan(
            OutputFormat::Webm,
            &encoder_names,
            &AdvancedEncodeSettings::default(),
        )
        .unwrap();
        assert_eq!(webm_plan.video_codec, Some(VideoCodec::LibVpxVp9));
        assert_eq!(webm_plan.audio_codec, Some(AudioCodec::LibOpus));
    }

    #[test]
    fn select_codec_plan_honors_explicit_available_codec() {
        let encoder_names = HashSet::from([
            "libx264".to_string(),
            "mpeg4".to_string(),
            "aac".to_string(),
            "libvpx-vp9".to_string(),
            "libvpx".to_string(),
            "libopus".to_string(),
        ]);

        let mp4_plan = select_codec_plan(
            OutputFormat::Mp4,
            &encoder_names,
            &AdvancedEncodeSettings {
                video_codec: Some(VideoCodecPreference::Mpeg4),
                audio_bitrate_kbps: None,
                ..AdvancedEncodeSettings::default()
            },
        )
        .unwrap();
        assert_eq!(mp4_plan.video_codec, Some(VideoCodec::Mpeg4));

        let webm_plan = select_codec_plan(
            OutputFormat::Webm,
            &encoder_names,
            &AdvancedEncodeSettings {
                video_codec: Some(VideoCodecPreference::Vp8),
                audio_bitrate_kbps: None,
                ..AdvancedEncodeSettings::default()
            },
        )
        .unwrap();
        assert_eq!(webm_plan.video_codec, Some(VideoCodec::LibVpx));
    }

    #[test]
    fn select_codec_plan_rejects_unavailable_explicit_codec() {
        let encoder_names = HashSet::from(["mpeg4".to_string(), "aac".to_string()]);
        let err = select_codec_plan(
            OutputFormat::Mp4,
            &encoder_names,
            &AdvancedEncodeSettings {
                video_codec: Some(VideoCodecPreference::H264),
                audio_bitrate_kbps: None,
                ..AdvancedEncodeSettings::default()
            },
        )
        .unwrap_err();

        assert!(err.contains("libx264"));
        assert!(err.contains("not available"));
    }

    #[test]
    fn advanced_audio_bitrate_accepts_only_presets() {
        let mut req = base_request();
        req.advanced.audio_bitrate_kbps = Some(192);
        assert_eq!(advanced_audio_bitrate_kbps(&req).unwrap(), Some(192));

        req.advanced.audio_bitrate_kbps = Some(999);
        let err = advanced_audio_bitrate_kbps(&req).unwrap_err();
        assert!(err.contains("96, 128, 192, 256, 320"));
    }

    #[test]
    fn advanced_overrides_force_reencode_when_they_affect_video_output() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.size_limit_mb = 0.0;

        assert!(!advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.video_codec = Some(VideoCodecPreference::H264);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.video_codec = None;
        req.advanced.audio_bitrate_kbps = Some(192);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.audio_bitrate_kbps = None;
        req.advanced.video_quality = Some(VideoQualityPreference::Higher);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.video_quality = None;
        req.advanced.encode_speed = Some(EncodeSpeedPreference::Faster);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.encode_speed = None;
        req.advanced.frame_rate_cap_fps = Some(24);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.frame_rate_cap_fps = Some(60);
        assert!(!advanced_forces_reencode(&req, &probe, false).unwrap());

        req.advanced.frame_rate_cap_fps = None;
        req.advanced.audio_channels = Some(AudioChannelPreference::Mono);
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());
    }

    #[test]
    fn size_target_holds_no_size_only_quality_and_audio_bitrate_overrides() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.advanced.audio_bitrate_kbps = Some(192);
        req.advanced.video_quality = Some(VideoQualityPreference::Higher);

        assert!(!advanced_forces_reencode(&req, &probe, true).unwrap());

        req.advanced.encode_speed = Some(EncodeSpeedPreference::Smaller);
        assert!(advanced_forces_reencode(&req, &probe, true).unwrap());
    }

    #[test]
    fn quality_preferences_map_per_codec() {
        assert_eq!(
            quality_mode_for_codec(VideoCodec::LibX264, VideoQualityPreference::Higher),
            QualityMode::Crf {
                crf: "20",
                preset: Some("medium")
            }
        );
        assert_eq!(
            quality_mode_for_codec(VideoCodec::Mpeg4, VideoQualityPreference::Smaller),
            QualityMode::QScale { qscale: "8" }
        );
        assert_eq!(
            quality_mode_for_codec(VideoCodec::LibVpxVp9, VideoQualityPreference::Higher),
            QualityMode::Cq {
                crf: "28",
                bitrate: "0"
            }
        );
        assert_eq!(
            quality_mode_for_codec(VideoCodec::LibVpx, VideoQualityPreference::Smaller),
            QualityMode::Cq {
                crf: "16",
                bitrate: "800k"
            }
        );
    }

    #[test]
    fn encode_speed_preferences_map_per_codec() {
        assert_eq!(
            encode_speed_args_for_codec(VideoCodec::LibX264, EncodeSpeedPreference::Smaller),
            vec!["-preset", "slow"]
        );
        assert_eq!(
            encode_speed_args_for_codec(VideoCodec::LibVpxVp9, EncodeSpeedPreference::Faster),
            vec!["-deadline", "good", "-cpu-used", "5", "-row-mt", "1"]
        );
        assert_eq!(
            encode_speed_args_for_codec(VideoCodec::LibVpx, EncodeSpeedPreference::Balanced),
            vec!["-deadline", "good", "-cpu-used", "2"]
        );
        assert!(
            encode_speed_args_for_codec(VideoCodec::Mpeg4, EncodeSpeedPreference::Faster)
                .is_empty()
        );
    }

    #[test]
    fn parse_ffprobe_rate_handles_rationals_and_invalid_values() {
        assert_eq!(
            parse_ffprobe_rate(Some("30000/1001")).unwrap().round() as u32,
            30
        );
        assert_eq!(parse_ffprobe_rate(Some("60")).unwrap(), 60.0);
        assert_eq!(parse_ffprobe_rate(Some("0/0")), None);
        assert_eq!(parse_ffprobe_rate(Some("")), None);
    }

    #[test]
    fn atempo_chain_handles_out_of_range() {
        assert_eq!(atempo_chain(1.0).unwrap(), "atempo=1.000000");
        assert_eq!(atempo_chain(4.0).unwrap(), "atempo=2.0,atempo=2.000000");
        assert_eq!(atempo_chain(0.25).unwrap(), "atempo=0.5,atempo=0.500000");
    }

    #[test]
    fn estimate_duration_applies_trim_and_speed() {
        let trim = Some(Trim {
            start_s: 2.0,
            end_s: Some(8.0),
        });
        let out = estimate_output_duration_s(10.0, &trim, 2.0).unwrap();
        assert!((out - 3.0).abs() < 1e-9);
    }

    #[test]
    fn build_video_filters_composes_in_expected_order() {
        let mut req = base_request();
        req.trim = Some(Trim {
            start_s: 1.0,
            end_s: Some(5.0),
        });
        req.crop = Some(Crop {
            x: 1,
            y: 3,
            width: 101,
            height: 99,
        });
        req.rotate_deg = 90;
        req.reverse = true;
        req.speed = 2.0;

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            filters,
            "trim=start=1:end=5,setpts=PTS-STARTPTS,crop=w=100:h=98:x=0:y=2,transpose=1,reverse,setpts=PTS/2.000000000"
        );
    }

    #[test]
    fn build_video_filters_includes_scale_and_eq() {
        let mut req = base_request();
        req.max_edge_px = Some(720);
        req.color = Some(ColorAdjust {
            brightness: 0.1,
            contrast: 1.2,
            saturation: 0.9,
        });

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            filters,
            "scale=w='if(gte(iw,ih),trunc(min(iw,720)/2)*2,-2)':h='if(gte(iw,ih),-2,trunc(min(ih,720)/2)*2)',eq=brightness=0.100000:contrast=1.200000:saturation=0.900000"
        );
    }

    #[test]
    fn max_edge_scale_filter_evens_odd_caps() {
        assert!(max_edge_scale_filter(719).contains("min(iw,718)"));
        assert!(max_edge_scale_filter(720).contains("min(iw,720)"));
    }

    #[test]
    fn build_video_filters_evens_untouched_odd_sources() {
        let req = {
            let mut req = base_request();
            req.advanced.video_quality = Some(VideoQualityPreference::Higher);
            req
        };
        let probe = VideoProbe {
            duration_s: 10.0,
            width: 853,
            height: 480,
            frame_rate: Some(30.0),
            has_audio: true,
        };
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(filters, "crop=w=trunc(iw/2)*2:h=trunc(ih/2)*2");

        // Crop and scale paths already guarantee even output, so no extra crop.
        let mut scaled = base_request();
        scaled.max_edge_px = Some(720);
        let filters = build_video_filters(&scaled, &probe).unwrap().unwrap();
        assert!(!filters.contains("crop="));
    }

    #[test]
    fn frame_rate_cap_accounts_for_speed_multiplier() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.advanced.frame_rate_cap_fps = Some(30);

        assert_eq!(frame_rate_cap_filter_fps(&req, &probe).unwrap(), None);

        req.speed = 2.0;
        assert_eq!(frame_rate_cap_filter_fps(&req, &probe).unwrap(), Some(30));
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(filters, "setpts=PTS/2.000000000,fps=30");

        req.advanced.frame_rate_cap_fps = None;
        assert_eq!(
            output_frame_rate_for_planning(&req, &probe).unwrap(),
            Some(60.0)
        );
    }

    #[test]
    fn build_video_filters_includes_custom_resize() {
        let mut req = base_request();
        req.resize = Some(ResizeSettings {
            mode: ResizeMode::Custom,
            max_edge_px: None,
            width_px: Some(801),
            height_px: Some(451),
        });

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(filters, "scale=w=800:h=450");
    }

    #[test]
    fn build_video_filters_applies_frame_rate_cap_only_when_source_is_higher() {
        let mut req = base_request();
        req.advanced.frame_rate_cap_fps = Some(24);

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(filters, "fps=24");

        req.advanced.frame_rate_cap_fps = Some(60);
        assert_eq!(build_video_filters(&req, &probe).unwrap(), None);
    }

    #[test]
    fn first_frame_perturb_absent_by_default() {
        let req = base_request();
        assert!(first_frame_perturb_filter(&req).is_none());
        let probe = probe_10s_1920x1080_audio();
        // A default request stays a no-op filtergraph.
        assert_eq!(build_video_filters(&req, &probe).unwrap(), None);
    }

    #[test]
    fn first_frame_perturb_appends_noise_filter_last() {
        let mut req = base_request();
        req.perturb_first_frame = true;
        req.perturb_seed = Some(123_456);
        // Add an earlier filter so we can prove the perturbation lands last.
        req.color = Some(ColorAdjust {
            brightness: 0.1,
            contrast: 1.0,
            saturation: 1.0,
        });

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        // ends_with (not split-on-comma): the noise filter carries an escaped
        // comma of its own, so it being the suffix proves it is appended last.
        assert!(
            filters.ends_with("noise=alls=3:allf=u:all_seed=123456:enable='eq(n\\,0)'"),
            "perturb filter must be appended last: {filters}"
        );
        assert!(filters.starts_with("eq=brightness"));
    }

    #[test]
    fn first_frame_perturb_stays_last_after_reverse_and_fps() {
        let mut req = base_request();
        req.perturb_first_frame = true;
        req.perturb_seed = Some(7);
        req.reverse = true;
        req.advanced.frame_rate_cap_fps = Some(24);

        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert!(filters.ends_with(":enable='eq(n\\,0)'"));
        assert!(filters.contains("noise=alls=3:allf=u:all_seed=7:"));
        assert!(filters.contains("reverse"));
    }

    #[test]
    fn first_frame_perturb_disables_stream_copy_noop() {
        // The stream-copy fast path keys off no_op_transforms; perturbation must
        // force a re-encode. Mirror the no-op predicate used in run_encode_job.
        let mut req = base_request();
        req.perturb_first_frame = true;
        let no_op = req.trim.is_none()
            && req.crop.is_none()
            && !req.reverse
            && req.rotate_deg == 0
            && (req.speed - 1.0).abs() <= 1e-9
            && matches!(requested_resize(&req).unwrap(), ResizePlan::Source)
            && color_is_noop(&req.color)
            && !req.perturb_first_frame;
        assert!(!no_op);
    }

    #[test]
    fn first_frame_perturb_uses_random_seed_when_unset() {
        let mut req = base_request();
        req.perturb_first_frame = true;
        req.perturb_seed = None;
        let filter = first_frame_perturb_filter(&req).unwrap();
        assert!(filter.starts_with("noise=alls=3:allf=u:all_seed="));
        assert!(filter.ends_with(":enable='eq(n\\,0)'"));
    }

    #[test]
    fn build_audio_filters_composes_in_expected_order() {
        let mut req = base_request();
        req.trim = Some(Trim {
            start_s: 1.0,
            end_s: Some(5.0),
        });
        req.reverse = true;
        req.speed = 4.0;

        let probe = probe_10s_1920x1080_audio();
        let filters = build_audio_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            filters,
            "atrim=start=1:end=5,asetpts=PTS-STARTPTS,areverse,atempo=2.0,atempo=2.000000"
        );
    }

    #[test]
    fn build_audio_filters_inserts_loudnorm_after_trim() {
        let mut req = base_request();
        req.normalize_audio = true;
        req.trim = Some(Trim {
            start_s: 1.0,
            end_s: Some(5.0),
        });

        let probe = probe_10s_1920x1080_audio();
        let filters = build_audio_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            filters,
            "atrim=start=1:end=5,asetpts=PTS-STARTPTS,loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000"
        );

        let silent_probe = VideoProbe {
            has_audio: false,
            ..probe_10s_1920x1080_audio()
        };
        assert_eq!(build_audio_filters(&req, &silent_probe).unwrap(), None);
    }

    #[test]
    fn metadata_args_strip_before_title() {
        let mut req = base_request();
        req.title = Some("  Beach day  ".to_string());
        req.strip_metadata = true;
        let mut args = Vec::new();
        push_metadata_args(&mut args, &req);
        assert_eq!(
            args,
            vec!["-map_metadata", "-1", "-metadata", "title=Beach day"]
        );

        req.strip_metadata = false;
        req.title = None;
        let mut args = Vec::new();
        push_metadata_args(&mut args, &req);
        assert!(args.is_empty());
    }

    #[test]
    fn normalize_audio_forces_reencode() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.normalize_audio = true;
        assert!(advanced_forces_reencode(&req, &probe, false).unwrap());
        assert!(advanced_forces_reencode(&req, &probe, true).unwrap());

        req.audio_enabled = false;
        assert!(!advanced_forces_reencode(&req, &probe, false).unwrap());
    }

    #[test]
    fn parse_probe_output_swaps_dimensions_for_display_rotation() {
        let json = r#"{
            "format": {"duration": "12.5"},
            "streams": [
                {
                    "codec_type": "video",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "30/1",
                    "side_data_list": [
                        {"side_data_type": "Display Matrix", "rotation": -90}
                    ]
                },
                {"codec_type": "audio"}
            ]
        }"#;
        let probe = parse_probe_output(json).unwrap();
        assert_eq!((probe.width, probe.height), (1080, 1920));
        assert!(probe.has_audio);
        assert!((probe.duration_s - 12.5).abs() < 1e-9);
    }

    #[test]
    fn parse_probe_output_handles_legacy_rotate_tag_and_180() {
        let rotated_tag = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1", "tags": {"rotate": "90"}}
            ]
        }"#;
        let probe = parse_probe_output(rotated_tag).unwrap();
        assert_eq!((probe.width, probe.height), (480, 640));

        let upside_down = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1",
                 "side_data_list": [
                    {"side_data_type": "Display Matrix", "rotation": 180}
                 ]}
            ]
        }"#;
        let probe = parse_probe_output(upside_down).unwrap();
        assert_eq!((probe.width, probe.height), (640, 480));

        let no_rotation = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1"}
            ]
        }"#;
        let probe = parse_probe_output(no_rotation).unwrap();
        assert_eq!((probe.width, probe.height), (640, 480));
        assert!(!probe.has_audio);
    }

    #[test]
    fn plan_bitrates_drops_audio_when_budget_too_low() {
        let plan = plan_bitrates(1.0, 600.0, true, 96, 32, 50, 0.95).unwrap();
        assert!(!plan.include_audio);
        assert_eq!(plan.audio_bitrate_kbps, 0);
        assert!(plan.video_bitrate_kbps > 0);
    }

    #[test]
    fn plan_bitrates_includes_audio_when_budget_allows() {
        let plan = plan_bitrates(8.0, 10.0, true, 96, 32, 50, 0.95).unwrap();
        assert!(plan.include_audio);
        assert!(plan.audio_bitrate_kbps >= 32);
        assert!(plan.video_bitrate_kbps >= 50);
    }

    #[test]
    fn plan_bitrates_respects_video_floor_without_audio() {
        let plan = plan_bitrates(0.1, 120.0, false, 96, 32, 384, 0.95).unwrap();
        assert!(!plan.include_audio);
        assert_eq!(plan.audio_bitrate_kbps, 0);
        assert_eq!(plan.video_bitrate_kbps, 384);
    }

    #[test]
    fn plan_bitrates_drops_audio_when_remaining_budget_is_below_audio_minimum() {
        let plan = plan_bitrates(0.3, 60.0, true, 96, 32, 384, 0.95).unwrap();
        assert!(!plan.include_audio);
        assert_eq!(plan.audio_bitrate_kbps, 0);
        assert_eq!(plan.video_bitrate_kbps, 384);
    }

    #[test]
    fn size_limited_contract_records_audio_removal_for_tight_budget() {
        let mut req = base_request();
        req.size_limit_mb = 0.3;
        req.trim = Some(Trim {
            start_s: 0.0,
            end_s: Some(60.0),
        });
        let probe = VideoProbe {
            duration_s: 60.0,
            width: 1280,
            height: 720,
            frame_rate: Some(30.0),
            has_audio: true,
        };

        let contract =
            build_size_limited_encode_contract(&req, &probe, VideoCodec::LibX264, 60.0, true)
                .unwrap();

        assert!(!contract.plan.include_audio);
        assert!(contract.audio_removed_for_size_target);
        assert_eq!(contract.plan.audio_bitrate_kbps, 0);
        assert!(contract.plan.video_bitrate_kbps >= contract.min_video_kbps);
    }

    #[test]
    fn size_limited_contract_keeps_audio_when_budget_allows() {
        let req = base_request();
        let probe = probe_10s_1920x1080_audio();

        let contract =
            build_size_limited_encode_contract(&req, &probe, VideoCodec::LibX264, 10.0, true)
                .unwrap();

        assert!(contract.plan.include_audio);
        assert!(!contract.audio_removed_for_size_target);
        assert!(contract.plan.audio_bitrate_kbps >= 32);
        assert!(contract.plan.video_bitrate_kbps >= contract.min_video_kbps);
    }

    #[test]
    fn size_limited_completion_message_discloses_audio_removal() {
        let message =
            size_limited_completion_message(true, VideoCodec::LibX264, 50, 50, true).unwrap();
        assert!(message.contains("Audio was removed"));
        assert!(!message.contains("Still above"));
    }

    #[test]
    fn minimum_video_bitrate_for_mpeg4_scales_with_resolution() {
        let hd = minimum_video_bitrate_kbps(VideoCodec::Mpeg4, 1280, 720, Some(30.0));
        let full_hd = minimum_video_bitrate_kbps(VideoCodec::Mpeg4, 1920, 1080, Some(30.0));
        assert!(hd >= 384, "{hd}");
        assert!(full_hd >= 768, "{full_hd}");
        assert!(full_hd > hd);
    }

    #[test]
    fn map_mpeg4_size_limit_error_replaces_raw_ffmpeg_tail() {
        let mapped = map_mpeg4_size_limit_error(
            OutputFormat::Mp4,
            VideoCodec::Mpeg4,
            0.1,
            1612,
            906,
            "ffmpeg failed (exit -1).\n\n[mpeg4 @ 0x0] requested bitrate is too low".to_string(),
        );

        assert!(mapped.contains("bundled MP4 fallback codec"));
        assert!(mapped.contains("1612x906"));
        assert!(mapped.contains("0.10 MB"));
        assert!(!mapped.contains("ffmpeg failed"));
    }

    #[test]
    fn map_mpeg4_size_limit_error_keeps_unrelated_errors() {
        let original = "ffmpeg failed (exit -1).\n\nPermission denied".to_string();
        let mapped = map_mpeg4_size_limit_error(
            OutputFormat::Mp4,
            VideoCodec::Mpeg4,
            8.0,
            1920,
            1080,
            original.clone(),
        );
        assert_eq!(mapped, original);
    }

    #[test]
    fn estimated_output_dimensions_follow_crop_rotate_and_scale() {
        let mut req = base_request();
        req.crop = Some(Crop {
            x: 10,
            y: 10,
            width: 1001,
            height: 801,
        });
        req.rotate_deg = 90;
        req.max_edge_px = Some(720);

        let (width, height) =
            estimated_output_dimensions(&req, &probe_10s_1920x1080_audio()).unwrap();
        assert_eq!((width, height), (576, 720));
    }

    #[test]
    fn estimated_output_dimensions_follow_custom_resize() {
        let mut req = base_request();
        req.resize = Some(ResizeSettings {
            mode: ResizeMode::Custom,
            max_edge_px: None,
            width_px: Some(853),
            height_px: Some(481),
        });

        let (width, height) =
            estimated_output_dimensions(&req, &probe_10s_1920x1080_audio()).unwrap();
        assert_eq!((width, height), (852, 480));
    }

    #[test]
    fn crop_out_of_bounds_errors() {
        let mut req = base_request();
        req.crop = Some(Crop {
            x: 1919,
            y: 1079,
            width: 10,
            height: 10,
        });
        let probe = probe_10s_1920x1080_audio();
        let err = build_video_filters(&req, &probe).unwrap_err();
        assert!(err.to_lowercase().contains("outside"));
    }

    #[test]
    fn parse_cropdetect_prefers_most_common() {
        let probe = probe_10s_1920x1080_audio();
        let stderr = r#"
[Parsed_cropdetect_0 @ 0x000] x1:0 x2:0 crop=1920:800:0:140
[Parsed_cropdetect_0 @ 0x000] x1:0 x2:0 crop=1920:800:0:140
[Parsed_cropdetect_0 @ 0x000] x1:0 x2:0 crop=1900:800:10:140
[Parsed_cropdetect_0 @ 0x000] x1:0 x2:0 crop=1920:800:0:140
"#;
        let crop = parse_cropdetect_from_stderr(stderr, &probe).unwrap();
        assert_eq!(crop.x, 0);
        assert_eq!(crop.y, 140);
        assert_eq!(crop.width, 1920);
        assert_eq!(crop.height, 800);
    }

    #[test]
    fn parse_cropdetect_returns_none_when_absent() {
        let probe = probe_10s_1920x1080_audio();
        let stderr = "no crop here\n";
        assert!(parse_cropdetect_from_stderr(stderr, &probe).is_none());
    }
}
