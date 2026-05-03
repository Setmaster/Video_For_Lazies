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

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeRequest {
    pub input_path: String,
    pub output_path: String,
    pub format: OutputFormat,
    pub title: Option<String>,
    pub size_limit_mb: f64,
    pub audio_enabled: bool,

    pub trim: Option<Trim>,
    pub crop: Option<Crop>,
    pub reverse: bool,
    pub speed: f64,
    pub rotate_deg: u16,
    pub max_edge_px: Option<u32>,
    pub color: Option<ColorAdjust>,
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

    let started_at = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to collect {action} output: {e}"));
            }
            Ok(None) => {
                if started_at.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
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

fn select_codec_plan(format: OutputFormat, encoder_names: &HashSet<String>) -> CodecSelection {
    match format {
        OutputFormat::Mp4 => {
            let video_codec = if encoder_names.contains("libx264") {
                VideoCodec::LibX264
            } else {
                VideoCodec::Mpeg4
            };
            let quality_mode = if video_codec == VideoCodec::LibX264 {
                QualityMode::Crf {
                    crf: "23",
                    preset: Some("medium"),
                }
            } else {
                QualityMode::QScale { qscale: "5" }
            };
            CodecSelection {
                video_codec: Some(video_codec),
                audio_codec: Some(AudioCodec::Aac),
                quality_mode: Some(quality_mode),
            }
        }
        OutputFormat::Webm => {
            let video_codec = if encoder_names.contains("libvpx-vp9") {
                VideoCodec::LibVpxVp9
            } else {
                VideoCodec::LibVpx
            };
            let audio_codec = if encoder_names.contains("libopus") {
                AudioCodec::LibOpus
            } else {
                AudioCodec::LibVorbis
            };
            let quality_mode = if video_codec == VideoCodec::LibVpxVp9 {
                QualityMode::Cq {
                    crf: "32",
                    bitrate: "0",
                }
            } else {
                QualityMode::Cq {
                    crf: "10",
                    bitrate: "1M",
                }
            };
            CodecSelection {
                video_codec: Some(video_codec),
                audio_codec: Some(audio_codec),
                quality_mode: Some(quality_mode),
            }
        }
        OutputFormat::Mp3 => CodecSelection {
            video_codec: None,
            audio_codec: Some(if encoder_names.contains("libmp3lame") {
                AudioCodec::LibMp3Lame
            } else {
                AudioCodec::Mp3
            }),
            quality_mode: None,
        },
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

fn estimated_output_dimensions(req: &EncodeRequest, probe: &VideoProbe) -> (u32, u32) {
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

    if let Some(max_edge_px) = req.max_edge_px {
        let long_edge = width.max(height);
        if max_edge_px >= 16 && long_edge > max_edge_px {
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

    (width, height)
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

    let max_tries = 10_000u32;
    for i in start_n..start_n.saturating_add(max_tries) {
        let file = format!("{base}-{i}.{ext}");
        let candidate = parent.join(file);
        if !candidate.exists() {
            return Ok(candidate.to_string_lossy().to_string());
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
    cmd.arg("-y")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-nostdin")
        .arg("-i")
        .arg(input_path.as_os_str())
        .arg("-ss")
        .arg(format!("{time_s:.3}"))
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
struct FFProbeStream {
    codec_type: Option<String>,
    duration: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
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
        .arg("quiet")
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
    let parsed: FFProbeOutput =
        serde_json::from_str(&stdout).map_err(|e| format!("ffprobe returned invalid JSON: {e}"))?;

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

fn scale_filter(max_edge_px: u32) -> String {
    // Keep aspect ratio, never upscale, and keep dimensions divisible by 2.
    // Use quotes inside the filter string so commas inside expressions are not treated as filter separators.
    format!(
        "scale=w='if(gte(iw,ih),min(iw,{m}),-2)':h='if(gte(iw,ih),-2,min(ih,{m}))'",
        m = max_edge_px
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

    if let Some(max_edge_px) = req.max_edge_px {
        if max_edge_px < 16 {
            return Err("Max edge must be >= 16 px.".to_string());
        }
        filters.push(scale_filter(max_edge_px));
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

    Ok(if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    })
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
                "The bundled MP4 fallback codec cannot encode {width}x{height} video within a {target_size_mb:.2} MB size target. Try a larger size limit, lower Max edge, reduce crop, or export as WebM."
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
    let (planned_width, planned_height) = estimated_output_dimensions(request, probe);
    let min_video_kbps = minimum_video_bitrate_kbps(
        selected_video_codec,
        planned_width,
        planned_height,
        probe.frame_rate,
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
            messages.push("Still above size limit. The bundled MP4 fallback codec cannot go lower at this resolution. Try a larger size limit, WebM, or reduce Max edge.");
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

    let output_path = validate_output_path(
        &input_path,
        &PathBuf::from(request.output_path.trim()),
        output_extension(request.format),
    )?;

    let input_str = input_path.to_string_lossy().to_string();

    let ffmpeg_bin = default_ffmpeg();
    let probe = probe_video(input_path.to_string_lossy().to_string())?;
    let codec_selection = select_codec_plan(request.format, &cached_encoder_names(&ffmpeg_bin)?);

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
        if size_limit_enabled {
            let audio_kbps =
                plan_audio_only_kbps(request.size_limit_mb, output_duration_s, 0.98, 32, 320)?;
            args.push("-b:a".to_string());
            args.push(format!("{audio_kbps}k"));
        } else {
            args.push("-q:a".to_string());
            args.push("2".to_string());
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

        let temp_path = temp_output_path(&output_path, job_id, "mp3")?;
        args.push(temp_path.to_string_lossy().to_string());

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
        });
    }

    let target_bytes = (request.size_limit_mb * MB_BYTES as f64) as u64;

    // Fast path: no-op transforms (avoid inflating already-small files).
    let no_op_transforms = request.trim.is_none()
        && request.crop.is_none()
        && !request.reverse
        && request.rotate_deg == 0
        && (request.speed - 1.0).abs() <= 1e-9
        && request.max_edge_px.is_none()
        && color_is_noop(&request.color);

    if no_op_transforms {
        let input_size = fs::metadata(&input_path)
            .map_err(|e| format!("Failed to stat input file: {e}"))?
            .len();

        let should_try_stream_copy = if size_limit_enabled {
            input_size <= target_bytes || !request.audio_enabled
        } else {
            true
        };

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

        // NOTE: This "no size target" path uses the codec plan defaults.
        // If/when we expose advanced encoding knobs (CRF, preset, audio bitrate, etc),
        // user-provided settings should override the defaults set below.
        match quality_mode {
            QualityMode::Crf { crf, preset } => {
                args.extend(["-crf", crf].into_iter().map(|s| s.to_string()));
                if let Some(preset) = preset {
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

        if let Some(vf) = &video_filters {
            args.push("-vf".to_string());
            args.push(vf.clone());
        }

        if include_audio && probe.has_audio && request.audio_enabled {
            args.extend(["-map", "0:a:0?"].into_iter().map(|s| s.to_string()));
            args.extend(["-c:a", audio_codec].into_iter().map(|s| s.to_string()));
            args.push("-b:a".to_string());
            args.push(match request.format {
                OutputFormat::Mp4 => "192k".to_string(),
                OutputFormat::Webm => "128k".to_string(),
                OutputFormat::Mp3 => unreachable!(),
            });
            if let Some(af) = &audio_filters {
                args.push("-af".to_string());
                args.push(af.clone());
            }
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
            args.extend(
                ["-movflags", "+faststart", "-pix_fmt", "yuv420p"]
                    .into_iter()
                    .map(|s| s.to_string()),
            );
        }

        let temp_path = temp_output_path(&output_path, job_id, "encode")?;
        args.push(temp_path.to_string_lossy().to_string());

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

        if let Some(vf) = &video_filters {
            pass2.push("-vf".to_string());
            pass2.push(vf.clone());
        }

        if plan.include_audio && probe.has_audio && request.audio_enabled {
            pass2.extend(["-map", "0:a:0?"].into_iter().map(|s| s.to_string()));
            pass2.extend(["-c:a", audio_codec].into_iter().map(|s| s.to_string()));
            pass2.push("-b:a".to_string());
            pass2.push(format!("{}k", plan.audio_bitrate_kbps.max(32)));
            if let Some(af) = &audio_filters {
                pass2.push("-af".to_string());
                pass2.push(af.clone());
            }
        } else {
            pass2.push("-an".to_string());
        }

        if let Some(title) = request
            .title
            .as_deref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
        {
            pass2.push("-metadata".to_string());
            pass2.push(format!("title={title}"));
        }

        if matches!(request.format, OutputFormat::Mp4) {
            pass2.extend(
                ["-movflags", "+faststart", "-pix_fmt", "yuv420p"]
                    .into_iter()
                    .map(|s| s.to_string()),
            );
        }

        let temp_output = temp_output_path(&output_path, job_id, &format!("pass2-{attempt}"))?;
        pass2.push(temp_output.to_string_lossy().to_string());

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
            trim: None,
            crop: None,
            reverse: false,
            speed: 1.0,
            rotate_deg: 0,
            max_edge_px: None,
            color: None,
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
            suggest_output_path_unique(input.to_string_lossy().to_string(), OutputFormat::Mp4)
                .unwrap();
        assert!(suggested.ends_with("myvideo-4.mp4"), "{suggested}");

        let _ = fs::remove_dir_all(&dir);
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

        let mp4_plan = select_codec_plan(OutputFormat::Mp4, &encoder_names);
        assert_eq!(mp4_plan.video_codec, Some(VideoCodec::Mpeg4));
        assert_eq!(mp4_plan.audio_codec, Some(AudioCodec::Aac));
        assert_eq!(
            mp4_plan.quality_mode,
            Some(QualityMode::QScale { qscale: "5" })
        );

        let webm_plan = select_codec_plan(OutputFormat::Webm, &encoder_names);
        assert_eq!(webm_plan.video_codec, Some(VideoCodec::LibVpxVp9));
        assert_eq!(webm_plan.audio_codec, Some(AudioCodec::LibOpus));
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
            "scale=w='if(gte(iw,ih),min(iw,720),-2)':h='if(gte(iw,ih),-2,min(ih,720))',eq=brightness=0.100000:contrast=1.200000:saturation=0.900000"
        );
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

        let (width, height) = estimated_output_dimensions(&req, &probe_10s_1920x1080_audio());
        assert_eq!((width, height), (576, 720));
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
