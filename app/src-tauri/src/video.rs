use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Window};
use tempfile::{Builder as TempFileBuilder, TempDir, TempPath};

const MB_BYTES: u64 = 1_000_000;
const JS_MAX_SAFE_INTEGER_BYTES: u64 = 9_007_199_254_740_991;
const PROGRESS_EMIT_EVERY: Duration = Duration::from_millis(200);
const ENCODER_PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const CROP_DETECT_TIMEOUT: Duration = Duration::from_secs(60);
const FRAME_EXTRACT_TIMEOUT: Duration = Duration::from_secs(45);
const ENCODE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const ENCODE_INITIAL_BUFFER_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SIZE_COPY_MONITOR_HEADROOM_BYTES: u64 = 1024 * 1024;
const FFMPEG_SIDECAR_DIR: &str = "ffmpeg-sidecar";
const EXTERNAL_SUBTITLE_FILE_NAME: &str = "vfl_external.srt";
const EXTERNAL_SUBTITLE_FONT_DIR_NAME: &str = "fonts";
const EXTERNAL_SUBTITLE_FONT_FILE_NAME: &str = "DejaVuSans.ttf";
const EXTERNAL_SUBTITLE_FONT_BYTES: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const EXTERNAL_SUBTITLE_MAX_BYTES: u64 = 5 * 1024 * 1024;
const EXTERNAL_SUBTITLE_MAX_CUES: usize = 10_000;
const EXTERNAL_SUBTITLE_MAX_LINE_CHARS: usize = 10_000;
const STRICT_FIT_MAX_PLANS: u32 = 4;
const STRICT_FIT_REDUCED_AUDIO_KBPS: u32 = 32;
const STRICT_FIT_MAX_EDGE_TIERS: &[u32] = &[1280, 960, 720, 540, 360];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static FFMPEG_CAPABILITY_CACHE: OnceLock<Mutex<HashMap<String, FfmpegRuntimeCapabilities>>> =
    OnceLock::new();
static FFMPEG_PIXEL_FORMAT_CACHE: OnceLock<
    Mutex<HashMap<String, HashMap<String, PixelFormatDescriptor>>>,
> = OnceLock::new();
const AUDIO_BITRATE_PRESETS_KBPS: &[u32] = &[96, 128, 192, 256, 320];
const REVERSE_BUFFER_SAFETY_FACTOR: f64 = 1.5;
const REVERSE_BUFFER_VIDEO_FRAME_OVERHEAD_BYTES: u128 = 8 * 1024;
const REVERSE_BUFFER_AUDIO_FRAME_OVERHEAD_BYTES: u128 = 8 * 1024;
const REVERSE_AUDIO_FRAME_SAMPLES: u128 = 1024;
const REVERSE_BUFFER_WARNING_BYTES: u64 = 512 * 1024 * 1024;
const REVERSE_BUFFER_HARD_LIMIT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const OUTPUT_DIMENSION_MAX_PX: u32 = 32_768;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Rational {
    pub numerator: u32,
    pub denominator: u32,
}

impl Rational {
    const SQUARE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    fn as_f64(self) -> f64 {
        self.numerator as f64 / self.denominator as f64
    }

    fn is_square(self) -> bool {
        self.numerator == self.denominator
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DynamicRange {
    Sdr,
    Hdr10,
    Hlg,
    DolbyVision,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamDispositions {
    pub default: bool,
    pub original: bool,
    pub comment: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub visual_impaired: bool,
    pub attached_pic: bool,
    pub timed_thumbnails: bool,
    pub still_image: bool,
}

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
    /// Display-oriented sample-grid width. This intentionally retains the
    /// pre-v1.10 meaning used by crop coordinates.
    pub width: u32,
    /// Display-oriented sample-grid height. This intentionally retains the
    /// pre-v1.10 meaning used by crop coordinates.
    pub height: u32,
    pub coded_width: u32,
    pub coded_height: u32,
    pub rotation_deg: u32,
    pub unsupported_rotation_deg: Option<f64>,
    pub frame_rate: Option<f64>,
    pub has_audio: bool,
    pub source_format: Option<String>,
    pub video_stream_index: u32,
    pub video_codec: Option<String>,
    pub video_is_default: bool,
    pub audio_stream_index: Option<u32>,
    pub audio_codec: Option<String>,
    pub audio_is_default: bool,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<u8>,
    pub color_range: Option<String>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub dynamic_range: DynamicRange,
    pub sample_aspect_ratio: Rational,
    pub display_aspect_ratio: Rational,
    pub attached_picture_count: u32,
    pub selected_video_dispositions: StreamDispositions,
    pub audio_sample_rate: Option<u32>,
    pub audio_channels: Option<u32>,
    pub audio_sample_format: Option<String>,
    pub decoded_video_bytes_per_pixel: Option<f64>,
    pub decoded_audio_bytes_per_sample: Option<u8>,
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ColorPolicy {
    #[default]
    Auto,
    StandardSdr,
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
    pub strict_fit: bool,
    #[serde(default)]
    pub strict_fit_allow_audio_removal: bool,
    #[serde(default)]
    pub subtitle_path: Option<String>,
    #[serde(default)]
    pub normalize_audio: bool,
    #[serde(default)]
    pub strip_metadata: bool,
    #[serde(default)]
    pub color_policy: ColorPolicy,
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
    /// When true, the export plays forward then in reverse (a seamless
    /// boomerang loop): the video/audio chains are wrapped with
    /// split/reverse/concat, which doubles the output duration. N/A for mp3.
    #[serde(default)]
    pub loop_video: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeProgressPayload {
    pub attempt_id: u64,
    pub job_id: u64,
    pub pass: u8,
    pub total_passes: u8,
    pub pass_pct: f64,
    pub overall_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncodeFinishedPayload {
    pub attempt_id: u64,
    pub job_id: u64,
    pub ok: bool,
    pub output_path: Option<String>,
    pub output_size_bytes: Option<u64>,
    pub target_result: Option<TargetResult>,
    pub message: Option<String>,
    pub diagnostics: Option<ExportDiagnostics>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SizeTargetStatus {
    Met,
    Missed,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FitPlanResult {
    pub plan_number: u32,
    pub label: String,
    pub mutations: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub video_bitrate_kbps: Option<u32>,
    pub audio_action: Option<StreamAction>,
    pub audio_bitrate_kbps: Option<u32>,
    pub actual_size_bytes: u64,
    pub status: SizeTargetStatus,
    pub ffmpeg_invocations: u32,
    pub selected: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetResult {
    pub status: SizeTargetStatus,
    pub target_bytes: u64,
    pub actual_bytes: u64,
    pub overshoot_bytes: u64,
    pub strict_fit: bool,
    pub selected_plan_number: u32,
    pub plans: Vec<FitPlanResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleInspection {
    pub cue_count: u32,
    pub first_cue_start_s: f64,
    pub last_cue_end_s: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportDiagnostics {
    pub mode: String,
    pub video_action: Option<StreamAction>,
    pub audio_action: Option<StreamAction>,
    pub source_format: Option<String>,
    pub source_video_codec: Option<String>,
    pub source_audio_codec: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub video_bitrate_kbps: Option<u32>,
    pub audio_bitrate_kbps: Option<u32>,
    pub requested_size_bytes: Option<u64>,
    pub actual_size_bytes: Option<u64>,
    pub passes: u8,
    pub attempts: u32,
    pub audio_removed_for_size_target: bool,
    pub subtitle_burned_in: bool,
    pub subtitle_cue_count: Option<u32>,
    pub copy_fallback_reason: Option<String>,
    pub color_action: String,
    pub sar_action: String,
    pub reverse_buffer_estimate_bytes: Option<u64>,
    pub reverse_buffer_action: Option<String>,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub command_preview: String,
}

fn target_bytes_from_size_limit_mb(size_limit_mb: f64) -> Option<u64> {
    if !size_limit_mb.is_finite() || size_limit_mb <= 0.0 {
        return None;
    }
    let target_bytes = size_limit_mb * MB_BYTES as f64;
    if !target_bytes.is_finite()
        || target_bytes < 1.0
        || target_bytes > JS_MAX_SAFE_INTEGER_BYTES as f64
    {
        return None;
    }
    Some(target_bytes.trunc() as u64)
}

pub fn failed_encode_diagnostics(request: &EncodeRequest, reason: &str) -> ExportDiagnostics {
    let requested_size_bytes = target_bytes_from_size_limit_mb(request.size_limit_mb);
    let redacted_reason = safe_failure_diagnostic_reason(request, reason);
    ExportDiagnostics {
        mode: "failed".to_string(),
        video_action: None,
        audio_action: None,
        source_format: None,
        source_video_codec: None,
        source_audio_codec: None,
        video_codec: None,
        audio_codec: None,
        video_bitrate_kbps: None,
        audio_bitrate_kbps: None,
        requested_size_bytes,
        actual_size_bytes: None,
        passes: 0,
        attempts: 0,
        audio_removed_for_size_target: false,
        subtitle_burned_in: false,
        subtitle_cue_count: None,
        copy_fallback_reason: None,
        color_action: "Not completed".to_string(),
        sar_action: "Not completed".to_string(),
        reverse_buffer_estimate_bytes: None,
        reverse_buffer_action: None,
        failure_stage: Some("backend".to_string()),
        failure_reason: Some(redacted_reason),
        command_preview:
            "No FFmpeg command evidence was retained because the export failed before completion."
                .to_string(),
    }
}

fn safe_failure_diagnostic_reason(request: &EncodeRequest, reason: &str) -> String {
    let first_line = reason
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .unwrap_or("Export failed.");
    let mut redactions: Vec<(String, &'static str)> = Vec::new();

    fn add_path_redaction(
        redactions: &mut Vec<(String, &'static str)>,
        path: impl AsRef<str>,
        replacement: &'static str,
    ) {
        let path = path.as_ref().trim();
        if path.is_empty() || matches!(path, "." | "/" | "\\") {
            return;
        }
        for spelling in [
            path.to_string(),
            path.replace('\\', "/"),
            path.replace('/', "\\"),
        ] {
            if !spelling.is_empty() && !redactions.iter().any(|(existing, _)| existing == &spelling)
            {
                redactions.push((spelling, replacement));
            }
        }
    }

    let input_path = PathBuf::from(request.input_path.trim());
    add_path_redaction(&mut redactions, request.input_path.as_str(), "<input>");
    if let Ok(canonical_input) = input_path.canonicalize() {
        add_path_redaction(
            &mut redactions,
            canonical_input.to_string_lossy(),
            "<input>",
        );
    }

    let output_path = PathBuf::from(request.output_path.trim());
    add_path_redaction(&mut redactions, request.output_path.as_str(), "<output>");
    if let Some(output_parent) = output_path.parent() {
        add_path_redaction(
            &mut redactions,
            output_parent.to_string_lossy(),
            "<output-folder>",
        );
        if let Ok(canonical_parent) = output_parent.canonicalize() {
            add_path_redaction(
                &mut redactions,
                canonical_parent.to_string_lossy(),
                "<output-folder>",
            );
            if let Some(file_name) = output_path.file_name() {
                add_path_redaction(
                    &mut redactions,
                    canonical_parent.join(file_name).to_string_lossy(),
                    "<output>",
                );
            }
        }
    }

    if let Some(subtitle_path) = request
        .subtitle_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let subtitle_path_buf = PathBuf::from(subtitle_path);
        add_path_redaction(&mut redactions, subtitle_path, "<subtitle>");
        if let Ok(canonical_subtitle) = subtitle_path_buf.canonicalize() {
            add_path_redaction(
                &mut redactions,
                canonical_subtitle.to_string_lossy(),
                "<subtitle>",
            );
        }
    }

    redactions.sort_by_key(|(path, _)| std::cmp::Reverse(path.len()));
    let mut redacted = first_line.to_string();
    for (path, replacement) in redactions {
        redacted = redacted.replace(&path, replacement);
    }

    const MAX_FAILURE_REASON_CHARS: usize = 500;
    if redacted.chars().count() > MAX_FAILURE_REASON_CHARS {
        let mut truncated: String = redacted
            .chars()
            .take(MAX_FAILURE_REASON_CHARS - 3)
            .collect();
        truncated.push_str("...");
        truncated
    } else {
        redacted
    }
}

#[derive(Debug, Clone)]
struct ValidatedSubtitle {
    normalized_text: String,
    inspection: SubtitleInspection,
}

#[derive(Debug)]
struct PreparedSubtitle {
    temp_dir: TempDir,
    inspection: SubtitleInspection,
}

impl PreparedSubtitle {
    fn working_dir(&self) -> &Path {
        self.temp_dir.path()
    }
}

fn parse_srt_timestamp_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    let mut time_parts = value.split(':');
    let hours = time_parts.next()?.parse::<u64>().ok()?;
    let minutes = time_parts.next()?.parse::<u64>().ok()?;
    let seconds_and_millis = time_parts.next()?;
    if time_parts.next().is_some() || minutes >= 60 {
        return None;
    }
    let (seconds, millis) = seconds_and_millis
        .split_once(',')
        .or_else(|| seconds_and_millis.split_once('.'))?;
    if seconds.len() != 2
        || millis.len() != 3
        || !seconds.chars().all(|character| character.is_ascii_digit())
        || !millis.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }
    let seconds = seconds.parse::<u64>().ok()?;
    let millis = millis.parse::<u64>().ok()?;
    if seconds >= 60 || millis >= 1_000 {
        return None;
    }
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(millis)
}

fn contains_subtitle_style_markup(line: &str) -> bool {
    let mut remainder = line;
    while let Some(start) = remainder.find('<') {
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find('>') else {
            break;
        };
        let tag = after_start[..end]
            .trim_start()
            .strip_prefix('/')
            .unwrap_or(after_start[..end].trim_start())
            .trim_start();
        if tag.starts_with(|character: char| character.is_ascii_alphabetic()) {
            return true;
        }
        remainder = &after_start[end + 1..];
    }

    let mut remainder = line;
    while let Some(start) = remainder.find('{') {
        let after_start = remainder[start + 1..].trim_start();
        if after_start.starts_with('\\') {
            return true;
        }
        remainder = &remainder[start + 1..];
    }
    false
}

fn validate_external_srt_text(text: &str) -> Result<ValidatedSubtitle, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.contains('\0') {
        return Err("The selected SRT contains an unsupported NUL character.".to_string());
    }
    let normalized_text = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized_text.trim().is_empty() {
        return Err("The selected SRT is empty.".to_string());
    }
    if normalized_text
        .lines()
        .any(|line| line.chars().count() > EXTERNAL_SUBTITLE_MAX_LINE_CHARS)
    {
        return Err("The selected SRT contains a line that is too long.".to_string());
    }
    if normalized_text.lines().any(contains_subtitle_style_markup) {
        return Err(
            "The selected SRT contains inline styling markup. Remove HTML or ASS styling tags; Video For Lazies applies one fixed subtitle style."
                .to_string(),
        );
    }

    let mut blocks: Vec<Vec<&str>> = Vec::new();
    let mut current = Vec::new();
    for line in normalized_text.lines() {
        if line.trim().is_empty() {
            if !current.is_empty() {
                if blocks.len() >= EXTERNAL_SUBTITLE_MAX_CUES {
                    return Err(format!(
                        "The selected SRT has more than {EXTERNAL_SUBTITLE_MAX_CUES} cues."
                    ));
                }
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        if blocks.len() >= EXTERNAL_SUBTITLE_MAX_CUES {
            return Err(format!(
                "The selected SRT has more than {EXTERNAL_SUBTITLE_MAX_CUES} cues."
            ));
        }
        blocks.push(current);
    }
    if blocks.is_empty() {
        return Err("The selected SRT has no subtitle cues.".to_string());
    }
    let mut first_cue_start_ms = u64::MAX;
    let mut last_cue_end_ms = 0u64;
    for (index, block) in blocks.iter().enumerate() {
        let timing_index = if block.first().is_some_and(|line| {
            line.trim()
                .chars()
                .all(|character| character.is_ascii_digit())
        }) {
            1
        } else {
            0
        };
        let timing_line = block
            .get(timing_index)
            .ok_or_else(|| format!("Subtitle cue {} is missing its timing line.", index + 1))?;
        let (start, end_and_settings) = timing_line
            .split_once("-->")
            .ok_or_else(|| format!("Subtitle cue {} has an invalid timing line.", index + 1))?;
        let mut end_fields = end_and_settings.split_whitespace();
        let end = end_fields
            .next()
            .ok_or_else(|| format!("Subtitle cue {} has no end time.", index + 1))?;
        if end_fields.next().is_some() {
            return Err(format!(
                "Subtitle cue {} contains timing-line settings that can override the fixed subtitle position. Remove all text after the cue end timestamp.",
                index + 1
            ));
        }
        let start_ms = parse_srt_timestamp_ms(start)
            .ok_or_else(|| format!("Subtitle cue {} has an invalid start time.", index + 1))?;
        let end_ms = parse_srt_timestamp_ms(end)
            .ok_or_else(|| format!("Subtitle cue {} has an invalid end time.", index + 1))?;
        if end_ms <= start_ms {
            return Err(format!(
                "Subtitle cue {} must end after it starts.",
                index + 1
            ));
        }
        if !block
            .iter()
            .skip(timing_index + 1)
            .any(|line| !line.trim().is_empty())
        {
            return Err(format!("Subtitle cue {} has no text.", index + 1));
        }
        first_cue_start_ms = first_cue_start_ms.min(start_ms);
        last_cue_end_ms = last_cue_end_ms.max(end_ms);
    }

    let mut normalized_text = normalized_text.trim_matches('\n').to_string();
    normalized_text.push('\n');
    Ok(ValidatedSubtitle {
        normalized_text,
        inspection: SubtitleInspection {
            cue_count: blocks.len() as u32,
            first_cue_start_s: first_cue_start_ms as f64 / 1_000.0,
            last_cue_end_s: last_cue_end_ms as f64 / 1_000.0,
        },
    })
}

fn validate_external_srt_path(path: &Path) -> Result<ValidatedSubtitle, String> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("srt"))
    {
        return Err("Choose a file with the .srt extension.".to_string());
    }
    let file =
        fs::File::open(path).map_err(|_| "The selected SRT could not be read.".to_string())?;
    let metadata = file
        .metadata()
        .map_err(|_| "The selected SRT could not be read.".to_string())?;
    if !metadata.is_file() {
        return Err("The selected SRT path must point to a regular file.".to_string());
    }
    if metadata.len() == 0 {
        return Err("The selected SRT is empty.".to_string());
    }
    if metadata.len() > EXTERNAL_SUBTITLE_MAX_BYTES {
        return Err(format!(
            "The selected SRT is larger than {} MiB.",
            EXTERNAL_SUBTITLE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len().min(EXTERNAL_SUBTITLE_MAX_BYTES) as usize);
    file.take(EXTERNAL_SUBTITLE_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The selected SRT could not be read.".to_string())?;
    if bytes.is_empty() {
        return Err("The selected SRT is empty.".to_string());
    }
    if bytes.len() as u64 > EXTERNAL_SUBTITLE_MAX_BYTES {
        return Err(format!(
            "The selected SRT is larger than {} MiB.",
            EXTERNAL_SUBTITLE_MAX_BYTES / (1024 * 1024)
        ));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "The selected SRT must be UTF-8 text.".to_string())?;
    validate_external_srt_text(text)
}

pub fn inspect_srt(path: String) -> Result<SubtitleInspection, String> {
    validate_external_srt_path(Path::new(path.trim())).map(|validated| validated.inspection)
}

fn prepare_external_subtitle(path: &Path) -> Result<PreparedSubtitle, String> {
    let validated = validate_external_srt_path(path)?;
    let temp_dir = TempDir::new()
        .map_err(|_| "Could not create private subtitle staging storage.".to_string())?;
    fs::write(
        temp_dir.path().join(EXTERNAL_SUBTITLE_FILE_NAME),
        validated.normalized_text.as_bytes(),
    )
    .map_err(|_| "Could not stage the selected SRT for FFmpeg.".to_string())?;
    let font_dir = temp_dir.path().join(EXTERNAL_SUBTITLE_FONT_DIR_NAME);
    fs::create_dir(&font_dir)
        .map_err(|_| "Could not create private subtitle font staging storage.".to_string())?;
    fs::write(
        font_dir.join(EXTERNAL_SUBTITLE_FONT_FILE_NAME),
        EXTERNAL_SUBTITLE_FONT_BYTES,
    )
    .map_err(|_| "Could not stage the fixed subtitle font for FFmpeg.".to_string())?;
    Ok(PreparedSubtitle {
        temp_dir,
        inspection: validated.inspection,
    })
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
}

struct SizeCandidateOutput {
    temp_output: TempPath,
    actual_size_bytes: u64,
    plan_number: u32,
    command_plan: EncodeCommandPlan,
    video_bitrate_kbps: Option<u32>,
    audio_bitrate_kbps: Option<u32>,
    audio_action: StreamAction,
    passes: u8,
    command_preview: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrictFitNextStage {
    BitrateCorrection,
    LowerMaxEdge,
    AudioFallback,
    Exhausted,
}

impl StrictFitNextStage {
    fn successor(self) -> Self {
        match self {
            Self::BitrateCorrection => Self::LowerMaxEdge,
            Self::LowerMaxEdge => Self::AudioFallback,
            Self::AudioFallback | Self::Exhausted => Self::Exhausted,
        }
    }
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

    fn as_codec_name(self) -> &'static str {
        match self {
            VideoCodec::LibX264 => "h264",
            VideoCodec::Mpeg4 => "mpeg4",
            VideoCodec::LibVpxVp9 => "vp9",
            VideoCodec::LibVpx => "vp8",
        }
    }

    fn max_output_dimension(self) -> u32 {
        match self {
            VideoCodec::LibX264 => 16_384,
            VideoCodec::Mpeg4 => 8_190,
            VideoCodec::LibVpxVp9 => 32_768,
            VideoCodec::LibVpx => 16_382,
        }
    }
}

fn validate_codec_output_dimensions(
    codec: VideoCodec,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let limit = codec.max_output_dimension();
    if width > limit || height > limit {
        return Err(format!(
            "The {} encoder supports at most {limit} pixels per output dimension; planned output is {width}x{height}. Choose a bounded resize or another codec.",
            codec.as_ffmpeg_name()
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioCodec {
    Aac,
    LibOpus,
    LibVorbis,
    LibMp3Lame,
}

impl AudioCodec {
    fn as_ffmpeg_name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::LibOpus => "libopus",
            AudioCodec::LibVorbis => "libvorbis",
            AudioCodec::LibMp3Lame => "libmp3lame",
        }
    }

    fn as_codec_name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::LibOpus => "opus",
            AudioCodec::LibVorbis => "vorbis",
            AudioCodec::LibMp3Lame => "mp3",
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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StreamAction {
    Copy,
    Encode,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EncodeMode {
    Remux,
    VideoCopyAudioEncode,
    VideoEncodeAudioCopy,
    FullEncode,
    SizeTargeted,
    AudioOnlyEncode,
}

impl EncodeMode {
    fn label(self) -> &'static str {
        match self {
            EncodeMode::Remux => "Stream copy",
            EncodeMode::VideoCopyAudioEncode => "Video copy + audio re-encode",
            EncodeMode::VideoEncodeAudioCopy => "Video re-encode + audio copy",
            EncodeMode::FullEncode => "Video re-encode",
            EncodeMode::SizeTargeted => "Size-targeted two-pass encode",
            EncodeMode::AudioOnlyEncode => "Audio-only encode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanReason {
    CompatibleSourceCodec,
    IncompatibleSourceCodec,
    MissingStream,
    AudioDisabled,
    VideoTransform,
    TimelineTransform,
    ExplicitVideoCodec,
    VideoQuality,
    EncodeSpeed,
    FrameRateCap,
    AudioNormalization,
    AudioBitrate,
    AudioChannels,
    SizeTarget,
    AudioOnlyOutput,
    ColorConversion,
    SampleAspectRatio,
    SubtitleBurnIn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaColorAction {
    Unchanged,
    Hdr10ToStandardSdr,
    HighBitDepthSdrToStandardSdr,
    NotApplicable,
}

impl MediaColorAction {
    fn diagnostic(self) -> &'static str {
        match self {
            Self::Unchanged => "Source color unchanged",
            Self::Hdr10ToStandardSdr => "HDR10 converted to 8-bit limited-range BT.709 SDR",
            Self::HighBitDepthSdrToStandardSdr => {
                "High-bit-depth SDR converted to 8-bit limited-range BT.709 SDR"
            }
            Self::NotApplicable => "Not applicable to audio-only output",
        }
    }

    fn converts_to_standard_sdr(self) -> bool {
        matches!(
            self,
            Self::Hdr10ToStandardSdr | Self::HighBitDepthSdrToStandardSdr
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SarAction {
    Unchanged,
    Normalize { width: u32, height: u32 },
    NotApplicable,
}

impl SarAction {
    fn diagnostic(self, source_sar: Rational) -> String {
        match self {
            Self::Unchanged => "Source sample aspect ratio unchanged".to_string(),
            Self::Normalize { width, height } => format!(
                "Sample aspect ratio {}:{} normalized to square pixels at {width}x{height}",
                source_sar.numerator, source_sar.denominator
            ),
            Self::NotApplicable => "Not applicable to audio-only output".to_string(),
        }
    }

    fn normalizes(self) -> bool {
        matches!(self, Self::Normalize { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReverseBufferAction {
    WithinLimit,
    Warning,
}

impl ReverseBufferAction {
    fn diagnostic(self) -> &'static str {
        match self {
            Self::WithinLimit => "Decoded reverse buffer is within the guarded limit",
            Self::Warning => "Decoded reverse buffer exceeds the 512 MiB warning threshold",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReverseBufferEstimate {
    bytes: u64,
    action: ReverseBufferAction,
}

#[derive(Debug, Clone)]
struct FfmpegRunLimits {
    initial_progress_timeout: Duration,
    idle_timeout: Duration,
    output_size_limit: Option<(PathBuf, u64)>,
}

fn ffmpeg_run_limits(reverse: bool) -> FfmpegRunLimits {
    FfmpegRunLimits {
        initial_progress_timeout: if reverse {
            ENCODE_INITIAL_BUFFER_TIMEOUT
        } else {
            ENCODE_IDLE_TIMEOUT
        },
        idle_timeout: ENCODE_IDLE_TIMEOUT,
        output_size_limit: None,
    }
}

fn size_copy_run_limits(target_bytes: u64, output_path: &Path) -> FfmpegRunLimits {
    let mut limits = ffmpeg_run_limits(false);
    limits.output_size_limit = Some((
        output_path.to_path_buf(),
        target_bytes.saturating_add(SIZE_COPY_MONITOR_HEADROOM_BYTES),
    ));
    limits
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MediaPolicyPlan {
    color_action: MediaColorAction,
    sar_action: SarAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CopyCandidatePlan {
    video_stream_index: u32,
    audio_stream_index: Option<u32>,
    audio_action: StreamAction,
}

#[derive(Debug, Clone)]
struct EncodeCommandPlan {
    mode: EncodeMode,
    video_stream_index: Option<u32>,
    audio_stream_index: Option<u32>,
    source_video_codec: Option<String>,
    source_audio_codec: Option<String>,
    video_action: StreamAction,
    audio_action: StreamAction,
    video_encoder: Option<VideoCodec>,
    audio_encoder: Option<AudioCodec>,
    output_video_codec: Option<String>,
    output_audio_codec: Option<String>,
    quality_mode: Option<QualityMode>,
    encode_speed: EncodeSpeedPreference,
    audio_bitrate_kbps: Option<u32>,
    audio_channels: Option<u8>,
    video_filters: Option<String>,
    audio_filters: Option<String>,
    #[allow(dead_code)] // retained for plan inspection and focused tests
    video_reasons: Vec<PlanReason>,
    #[allow(dead_code)] // retained for plan inspection and focused tests
    audio_reasons: Vec<PlanReason>,
    size_contract: Option<SizeLimitedEncodeContract>,
    size_copy_candidates: Vec<CopyCandidatePlan>,
    media_policy: MediaPolicyPlan,
    reverse_buffer_estimate: Option<ReverseBufferEstimate>,
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
    pub ffmpeg_version: String,
    pub contract_schema_version: u32,
    pub features: Vec<EncodeFeatureCapability>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncodeFeatureCapability {
    pub name: String,
    pub release_required: bool,
    pub available: bool,
    pub missing_encoders: Vec<String>,
    pub missing_filters: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FfmpegCapabilityContract {
    schema_version: u32,
    features: HashMap<String, FfmpegFeatureRequirement>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FfmpegFeatureRequirement {
    release_required: bool,
    encoders: Vec<String>,
    filters: Vec<String>,
}

#[derive(Debug, Clone)]
struct FfmpegRuntimeCapabilities {
    version: String,
    encoder_names: HashSet<String>,
    filter_names: HashSet<String>,
}

fn apply_smoke_capability_mask(
    mut runtime: FfmpegRuntimeCapabilities,
    env: &HashMap<String, String>,
) -> Result<FfmpegRuntimeCapabilities, String> {
    let smoke_active = ["VFL_SMOKE_INPUT", "VFL_SMOKE_STATUS"].iter().all(|key| {
        env.get(*key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    });
    if !smoke_active {
        return Ok(runtime);
    }
    let Some(raw) = env
        .get("VFL_SMOKE_MISSING_CAPABILITY_FILTERS")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return Ok(runtime);
    };

    for filter in raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if filter != "subtitles" {
            return Err(format!(
                "VFL_SMOKE_MISSING_CAPABILITY_FILTERS cannot mask non-allowlisted filter: {filter}."
            ));
        }
        runtime.filter_names.remove(filter);
    }
    Ok(runtime)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct PixelFormatDescriptor {
    bit_depth: u8,
    decoded_bytes_per_pixel: f64,
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

fn base_ffmpeg_args(input: &str) -> Vec<String> {
    vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-i".to_string(),
        input.to_string(),
        "-progress".to_string(),
        "pipe:1".to_string(),
        "-nostats".to_string(),
    ]
}

fn push_absolute_map(args: &mut Vec<String>, stream_index: u32) {
    args.push("-map".to_string());
    args.push(format!("0:{stream_index}"));
}

fn build_copy_candidate_args(
    input: &str,
    request: &EncodeRequest,
    candidate: &CopyCandidatePlan,
) -> Vec<String> {
    let mut args = base_ffmpeg_args(input);
    push_absolute_map(&mut args, candidate.video_stream_index);
    args.extend(["-c:v", "copy"].into_iter().map(String::from));

    match candidate.audio_action {
        StreamAction::Copy => {
            if let Some(stream_index) = candidate.audio_stream_index {
                push_absolute_map(&mut args, stream_index);
                args.extend(["-c:a", "copy"].into_iter().map(String::from));
            } else {
                args.push("-an".to_string());
            }
        }
        StreamAction::Drop => args.push("-an".to_string()),
        StreamAction::Encode => unreachable!("copy candidate cannot encode audio"),
    }

    push_metadata_args(&mut args, request);

    if matches!(request.format, OutputFormat::Mp4) {
        args.push("-movflags".to_string());
        args.push("+faststart".to_string());
    }

    args
}

fn push_encoded_video_format_args(args: &mut Vec<String>, media_policy: MediaPolicyPlan) {
    args.extend(["-pix_fmt", "yuv420p"].into_iter().map(String::from));
    if media_policy.color_action.converts_to_standard_sdr() {
        args.extend(
            [
                "-color_range",
                "tv",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-colorspace",
                "bt709",
            ]
            .into_iter()
            .map(String::from),
        );
        // libx264 needs explicit VUI signalling in addition to FFmpeg's generic
        // color options. Without this, the exact bundled sidecar can mux range
        // and matrix while leaving primaries/transfer unspecified.
        args.extend(
            [
                "-x264-params",
                "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=tv",
            ]
            .into_iter()
            .map(String::from),
        );
    }
}

fn push_video_output_policy_args(
    args: &mut Vec<String>,
    format: OutputFormat,
    video_action: StreamAction,
    media_policy: MediaPolicyPlan,
) {
    if format != OutputFormat::Mp4 {
        return;
    }
    args.extend(["-movflags", "+faststart"].into_iter().map(String::from));
    if video_action == StreamAction::Encode {
        push_encoded_video_format_args(args, media_policy);
    }
}

fn build_single_pass_args(
    input: &str,
    request: &EncodeRequest,
    plan: &EncodeCommandPlan,
) -> Result<Vec<String>, String> {
    let mut args = base_ffmpeg_args(input);
    let video_stream_index = plan
        .video_stream_index
        .ok_or_else(|| "Missing selected video stream index.".to_string())?;
    push_absolute_map(&mut args, video_stream_index);

    match plan.video_action {
        StreamAction::Copy => {
            args.extend(["-c:v", "copy"].into_iter().map(String::from));
        }
        StreamAction::Encode => {
            let video_encoder = plan
                .video_encoder
                .ok_or_else(|| "Missing video encoder for re-encode plan.".to_string())?;
            args.extend(
                ["-c:v", video_encoder.as_ffmpeg_name()]
                    .into_iter()
                    .map(String::from),
            );
            match plan
                .quality_mode
                .ok_or_else(|| "Missing quality settings for export format.".to_string())?
            {
                QualityMode::Crf { crf, preset } => {
                    args.extend(["-crf", crf].into_iter().map(String::from));
                    if let Some(preset) = preset
                        && plan.encode_speed == EncodeSpeedPreference::Auto
                    {
                        args.extend(["-preset", preset].into_iter().map(String::from));
                    }
                }
                QualityMode::Cq { crf, bitrate } => {
                    args.extend(["-crf", crf, "-b:v", bitrate].into_iter().map(String::from));
                }
                QualityMode::QScale { qscale } => {
                    args.extend(["-q:v", qscale].into_iter().map(String::from));
                }
            }
            args.extend(encode_speed_args_for_codec(
                video_encoder,
                plan.encode_speed,
            ));
            if let Some(video_filters) = &plan.video_filters {
                args.push("-vf".to_string());
                args.push(video_filters.clone());
            }
        }
        StreamAction::Drop => return Err("Video output plan cannot drop video.".to_string()),
    }

    match plan.audio_action {
        StreamAction::Copy => {
            let audio_stream_index = plan
                .audio_stream_index
                .ok_or_else(|| "Missing selected audio stream index.".to_string())?;
            push_absolute_map(&mut args, audio_stream_index);
            args.extend(["-c:a", "copy"].into_iter().map(String::from));
        }
        StreamAction::Encode => {
            let audio_stream_index = plan
                .audio_stream_index
                .ok_or_else(|| "Missing selected audio stream index.".to_string())?;
            let audio_encoder = plan
                .audio_encoder
                .ok_or_else(|| "Missing audio encoder for re-encode plan.".to_string())?;
            push_absolute_map(&mut args, audio_stream_index);
            args.extend(
                ["-c:a", audio_encoder.as_ffmpeg_name()]
                    .into_iter()
                    .map(String::from),
            );
            if let Some(channels) = plan.audio_channels {
                args.extend(["-ac".to_string(), channels.to_string()]);
            }
            let audio_kbps = plan.audio_bitrate_kbps.unwrap_or(match request.format {
                OutputFormat::Mp4 => 192,
                OutputFormat::Webm => 128,
                OutputFormat::Mp3 => unreachable!(),
            });
            args.extend(["-b:a".to_string(), format!("{audio_kbps}k")]);
            if let Some(audio_filters) = &plan.audio_filters {
                args.push("-af".to_string());
                args.push(audio_filters.clone());
            }
        }
        StreamAction::Drop => args.push("-an".to_string()),
    }

    push_metadata_args(&mut args, request);
    push_video_output_policy_args(
        &mut args,
        request.format,
        plan.video_action,
        plan.media_policy,
    );
    Ok(args)
}

fn full_reencode_fallback(plan: &EncodeCommandPlan) -> Result<EncodeCommandPlan, String> {
    let mut fallback = plan.clone();
    if fallback.video_action == StreamAction::Copy {
        let video_encoder = fallback.video_encoder.ok_or_else(|| {
            "No compatible video encoder is available for stream-copy fallback.".to_string()
        })?;
        fallback.video_action = StreamAction::Encode;
        fallback.output_video_codec = Some(video_encoder.as_codec_name().to_string());
    }
    if fallback.audio_action == StreamAction::Copy {
        let audio_encoder = fallback.audio_encoder.ok_or_else(|| {
            "No compatible audio encoder is available for stream-copy fallback.".to_string()
        })?;
        fallback.audio_action = StreamAction::Encode;
        fallback.output_audio_codec = Some(audio_encoder.as_codec_name().to_string());
    }
    fallback.mode = match (fallback.video_action, fallback.audio_action) {
        (StreamAction::Encode, StreamAction::Copy) => EncodeMode::VideoEncodeAudioCopy,
        (StreamAction::Copy, StreamAction::Encode) => EncodeMode::VideoCopyAudioEncode,
        (StreamAction::Copy, StreamAction::Copy | StreamAction::Drop) => EncodeMode::Remux,
        (StreamAction::Encode, StreamAction::Encode | StreamAction::Drop) => EncodeMode::FullEncode,
        (StreamAction::Drop, _) => unreachable!("video output cannot drop video"),
    };
    Ok(fallback)
}

fn ensure_output_destination_available(output_path: &Path) -> Result<(), String> {
    if output_path.exists() {
        Err("Output file already exists. Choose a new filename.".to_string())
    } else {
        Ok(())
    }
}

fn create_temp_output(output_path: &Path, job_id: u64, label: &str) -> Result<TempPath, String> {
    ensure_output_destination_available(output_path)?;
    let parent = output_path
        .parent()
        .ok_or_else(|| "Output path must include a folder.".to_string())?;
    let extension = output_path
        .extension()
        .and_then(|extension| extension.to_str())
        .ok_or_else(|| "Output path must include an extension.".to_string())?;
    let prefix = format!(".vfl-{}-{job_id}-{label}-", std::process::id());
    let suffix = format!(".tmp.{extension}");
    let mut builder = TempFileBuilder::new();
    builder.prefix(&prefix).suffix(&suffix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Match an ordinary created output (0666 filtered by the process
        // umask). Tempfile defaults to 0600, which would otherwise persist as
        // an unexpected owner-only mode after no-clobber publication.
        builder.permissions(fs::Permissions::from_mode(0o666));
    }
    builder
        .tempfile_in(parent)
        .map(|file| file.into_temp_path())
        .map_err(|error| format!("Failed to reserve temporary output file: {error}"))
}

fn publish_output_file(temp_path: TempPath, output_path: &Path) -> Result<(), String> {
    temp_path.persist_noclobber(output_path).map_err(|error| {
        if output_path.exists() {
            "Output file already exists. Choose a new filename.".to_string()
        } else {
            format!("Failed to publish output file: {}", error.error)
        }
    })
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

fn parse_filter_names(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            if flags.len() != 2
                || !flags
                    .chars()
                    .all(|ch| ch.is_ascii_alphabetic() || ch == '.')
            {
                return None;
            }
            let name = parts.next()?;
            (!name.contains('=')).then(|| name.to_string())
        })
        .collect()
}

fn parse_pixel_format_descriptors(stdout: &str) -> HashMap<String, PixelFormatDescriptor> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            if flags.len() != 5
                || !flags
                    .chars()
                    .all(|character| character.is_ascii_alphabetic() || character == '.')
            {
                return None;
            }
            let name = parts.next()?;
            let component_count = parts.next()?.parse::<u8>().ok()?;
            let logical_bits_per_pixel = parts.next()?.parse::<u32>().ok()?;
            let component_depths = parts
                .next()?
                .split('-')
                .map(str::parse::<u8>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if component_count == 0
                || logical_bits_per_pixel == 0
                || component_depths.len() != component_count as usize
            {
                return None;
            }
            let bit_depth = *component_depths.iter().max()?;
            if bit_depth == 0 || bit_depth > 32 {
                return None;
            }
            // FFmpeg's BITS_PER_PIXEL column describes logical component bits.
            // AVFrame storage uses 8, 16, or 32-bit component slots, so scale
            // to the next slot width. This is exact for ordinary planar/float
            // formats and conservative for packed mixed-depth formats.
            let storage_bits = if bit_depth <= 8 {
                8.0
            } else if bit_depth <= 16 {
                16.0
            } else {
                32.0
            };
            let decoded_bytes_per_pixel =
                logical_bits_per_pixel as f64 * storage_bits / bit_depth as f64 / 8.0;
            decoded_bytes_per_pixel.is_finite().then_some((
                name.to_string(),
                PixelFormatDescriptor {
                    bit_depth,
                    decoded_bytes_per_pixel,
                },
            ))
        })
        .collect()
}

fn parse_ffmpeg_version(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("ffmpeg version "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
}

fn run_ffmpeg_capability_probe(
    ffmpeg_bin: &str,
    argument: &str,
    label: &str,
) -> Result<String, String> {
    let mut cmd = command_no_window(ffmpeg_bin);
    cmd.arg("-hide_banner");
    if argument != "-version" {
        cmd.arg("-loglevel").arg("error");
    }
    cmd.arg(argument);
    let output = run_command_output_with_timeout(
        cmd,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        ffmpeg_bin,
        label,
        ENCODER_PROBE_TIMEOUT,
    )?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{label} failed.\n\n{stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn cached_ffmpeg_capabilities(ffmpeg_bin: &str) -> Result<FfmpegRuntimeCapabilities, String> {
    let cache = FFMPEG_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(existing) = guard.get(ffmpeg_bin)
    {
        return apply_smoke_capability_mask(
            existing.clone(),
            &std::env::vars().collect::<HashMap<_, _>>(),
        );
    }

    let encoder_output =
        run_ffmpeg_capability_probe(ffmpeg_bin, "-encoders", "ffmpeg encoder probe")?;
    let filter_output = run_ffmpeg_capability_probe(ffmpeg_bin, "-filters", "ffmpeg filter probe")?;
    let version_output =
        run_ffmpeg_capability_probe(ffmpeg_bin, "-version", "ffmpeg version probe")?;
    let parsed = FfmpegRuntimeCapabilities {
        version: parse_ffmpeg_version(&version_output)
            .ok_or_else(|| "ffmpeg version probe returned no recognizable version.".to_string())?,
        encoder_names: parse_encoder_names(&encoder_output),
        filter_names: parse_filter_names(&filter_output),
    };
    if let Ok(mut guard) = cache.lock() {
        guard.insert(ffmpeg_bin.to_string(), parsed.clone());
    }
    apply_smoke_capability_mask(parsed, &std::env::vars().collect::<HashMap<_, _>>())
}

fn cached_ffmpeg_pixel_formats(
    ffmpeg_bin: &str,
) -> Result<HashMap<String, PixelFormatDescriptor>, String> {
    let cache = FFMPEG_PIXEL_FORMAT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock()
        && let Some(existing) = guard.get(ffmpeg_bin)
    {
        return Ok(existing.clone());
    }

    let output = run_ffmpeg_capability_probe(ffmpeg_bin, "-pix_fmts", "ffmpeg pixel-format probe")?;
    let parsed = parse_pixel_format_descriptors(&output);
    if parsed.is_empty() {
        return Err("ffmpeg pixel-format probe returned no usable descriptors.".to_string());
    }
    if let Ok(mut guard) = cache.lock() {
        guard.insert(ffmpeg_bin.to_string(), parsed.clone());
    }
    Ok(parsed)
}

fn is_feature_contract_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|character| character.is_ascii_alphanumeric())
}

fn is_ffmpeg_capability_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
}

fn validate_capability_names(values: &[String], label: &str) -> Result<(), String> {
    let mut seen = HashSet::new();
    for value in values {
        if !is_ffmpeg_capability_name(value) {
            return Err(format!(
                "{label} contains an invalid FFmpeg capability name: {value}."
            ));
        }
        if !seen.insert(value.as_str()) {
            return Err(format!(
                "{label} contains a duplicate FFmpeg capability name: {value}."
            ));
        }
    }
    Ok(())
}

fn parse_ffmpeg_capability_contract(raw: &str) -> Result<FfmpegCapabilityContract, String> {
    let contract: FfmpegCapabilityContract = serde_json::from_str(raw)
        .map_err(|error| format!("Bundled FFmpeg capability contract is invalid: {error}"))?;
    if contract.schema_version != 1 {
        return Err(format!(
            "Unsupported FFmpeg capability contract schema version: {}.",
            contract.schema_version
        ));
    }
    if contract.features.is_empty() {
        return Err("Bundled FFmpeg capability contract has no features.".to_string());
    }
    for (name, requirement) in &contract.features {
        if !is_feature_contract_name(name) {
            return Err(format!(
                "Bundled FFmpeg capability contract has an invalid feature name: {name}."
            ));
        }
        validate_capability_names(
            &requirement.encoders,
            &format!("FFmpeg capability feature {name}.encoders"),
        )?;
        validate_capability_names(
            &requirement.filters,
            &format!("FFmpeg capability feature {name}.filters"),
        )?;
    }
    for required_name in [
        "coreExport",
        "externalSubtitles",
        "hdrToSdr",
        "sarNormalize",
        "reverseLoop",
    ] {
        if !contract.features.contains_key(required_name) {
            return Err(format!(
                "Bundled FFmpeg capability contract is missing feature: {required_name}."
            ));
        }
    }
    Ok(contract)
}

fn ffmpeg_capability_contract() -> Result<FfmpegCapabilityContract, String> {
    parse_ffmpeg_capability_contract(include_str!("../../ffmpeg-capabilities.json"))
}

fn feature_capability(
    name: &str,
    requirement: &FfmpegFeatureRequirement,
    runtime: &FfmpegRuntimeCapabilities,
) -> EncodeFeatureCapability {
    let mut missing_encoders = requirement
        .encoders
        .iter()
        .filter(|encoder| !runtime.encoder_names.contains(encoder.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let mut missing_filters = requirement
        .filters
        .iter()
        .filter(|filter| !runtime.filter_names.contains(filter.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    missing_encoders.sort();
    missing_filters.sort();
    EncodeFeatureCapability {
        name: name.to_string(),
        release_required: requirement.release_required,
        available: missing_encoders.is_empty() && missing_filters.is_empty(),
        missing_encoders,
        missing_filters,
    }
}

fn require_ffmpeg_feature(
    name: &str,
    contract: &FfmpegCapabilityContract,
    runtime: &FfmpegRuntimeCapabilities,
) -> Result<(), String> {
    let requirement = contract
        .features
        .get(name)
        .ok_or_else(|| format!("FFmpeg capability contract is missing feature: {name}."))?;
    let capability = feature_capability(name, requirement, runtime);
    if capability.available {
        return Ok(());
    }
    let mut missing = capability
        .missing_encoders
        .iter()
        .map(|value| format!("encoder {value}"))
        .chain(
            capability
                .missing_filters
                .iter()
                .map(|value| format!("filter {value}")),
        )
        .collect::<Vec<_>>();
    missing.sort();
    Err(format!(
        "This FFmpeg build cannot provide {name}; missing {}.",
        missing.join(", ")
    ))
}

fn filter_names_from_graph(graph: Option<&str>) -> HashSet<String> {
    let Some(graph) = graph else {
        return HashSet::new();
    };
    let mut segments = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut parentheses = 0u32;
    for (index, ch) in graph.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => parentheses = parentheses.saturating_add(1),
            ')' => parentheses = parentheses.saturating_sub(1),
            ',' | ';' if parentheses == 0 => {
                segments.push(&graph[start..index]);
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    segments.push(&graph[start..]);

    segments
        .into_iter()
        .filter_map(|segment| {
            let mut token = segment.trim();
            while let Some(remainder) = token.strip_prefix('[') {
                let end = remainder.find(']')?;
                token = remainder[end + 1..].trim_start();
            }
            let end = token
                .find(|ch: char| ch == '=' || ch == '[' || ch.is_whitespace())
                .unwrap_or(token.len());
            let name = token[..end].trim();
            (!name.is_empty()).then(|| name.to_string())
        })
        .collect()
}

fn require_ffmpeg_capability_subset<'a>(
    label: &str,
    required_encoders: impl IntoIterator<Item = &'a str>,
    required_filters: impl IntoIterator<Item = &'a str>,
    contract: &FfmpegCapabilityContract,
    runtime: &FfmpegRuntimeCapabilities,
) -> Result<(), String> {
    let declared_encoders = contract
        .features
        .values()
        .flat_map(|feature| feature.encoders.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let declared_filters = contract
        .features
        .values()
        .flat_map(|feature| feature.filters.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    let mut undeclared = Vec::new();
    let mut missing = Vec::new();

    for encoder in required_encoders {
        if !declared_encoders.contains(encoder) {
            undeclared.push(format!("encoder {encoder}"));
        } else if !runtime.encoder_names.contains(encoder) {
            missing.push(format!("encoder {encoder}"));
        }
    }
    for filter in required_filters {
        if !declared_filters.contains(filter) {
            undeclared.push(format!("filter {filter}"));
        } else if !runtime.filter_names.contains(filter) {
            missing.push(format!("filter {filter}"));
        }
    }
    undeclared.sort();
    undeclared.dedup();
    missing.sort();
    missing.dedup();
    if !undeclared.is_empty() {
        return Err(format!(
            "The FFmpeg capability contract does not declare {label}: {}.",
            undeclared.join(", ")
        ));
    }
    if !missing.is_empty() {
        return Err(format!(
            "The active FFmpeg build cannot provide {label}; missing {}.",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn require_encode_plan_capabilities(
    plan: &EncodeCommandPlan,
    contract: &FfmpegCapabilityContract,
    runtime: &FfmpegRuntimeCapabilities,
) -> Result<(), String> {
    let mut encoders = Vec::new();
    if plan.video_action == StreamAction::Encode {
        encoders.push(
            plan.video_encoder
                .ok_or_else(|| "Missing video encoder for the encode plan.".to_string())?
                .as_ffmpeg_name(),
        );
    }
    if plan.audio_action == StreamAction::Encode {
        encoders.push(
            plan.audio_encoder
                .ok_or_else(|| "Missing audio encoder for the encode plan.".to_string())?
                .as_ffmpeg_name(),
        );
    }
    let video_filters = if plan.video_action == StreamAction::Encode {
        filter_names_from_graph(plan.video_filters.as_deref())
    } else {
        HashSet::new()
    };
    let audio_filters = if plan.audio_action == StreamAction::Encode {
        filter_names_from_graph(plan.audio_filters.as_deref())
    } else {
        HashSet::new()
    };
    let filters = video_filters
        .into_iter()
        .chain(audio_filters)
        .collect::<HashSet<_>>();
    require_ffmpeg_capability_subset(
        "the selected export plan",
        encoders,
        filters.iter().map(String::as_str),
        contract,
        runtime,
    )
}

fn require_initial_encode_plan_capabilities(
    plan: &EncodeCommandPlan,
    contract: &FfmpegCapabilityContract,
    runtime: &FfmpegRuntimeCapabilities,
) -> Result<(), String> {
    if !plan.size_copy_candidates.is_empty() {
        // A compatible size-target stream copy uses no encoder or filter. Its
        // re-encode fallback is checked only if the bounded copy misses.
        return Ok(());
    }
    require_encode_plan_capabilities(plan, contract, runtime)
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
        OutputFormat::Mp4 => encoder_names
            .contains("libx264")
            .then_some(VideoCodec::LibX264)
            .or_else(|| encoder_names.contains("mpeg4").then_some(VideoCodec::Mpeg4)),
        OutputFormat::Webm => encoder_names
            .contains("libvpx-vp9")
            .then_some(VideoCodec::LibVpxVp9)
            .or_else(|| {
                encoder_names
                    .contains("libvpx")
                    .then_some(VideoCodec::LibVpx)
            }),
        OutputFormat::Mp3 => None,
    }
}

fn audio_codec_for_format(
    format: OutputFormat,
    encoder_names: &HashSet<String>,
) -> Option<AudioCodec> {
    match format {
        OutputFormat::Mp4 => encoder_names.contains("aac").then_some(AudioCodec::Aac),
        OutputFormat::Webm => encoder_names
            .contains("libopus")
            .then_some(AudioCodec::LibOpus)
            .or_else(|| {
                encoder_names
                    .contains("libvorbis")
                    .then_some(AudioCodec::LibVorbis)
            }),
        OutputFormat::Mp3 => encoder_names
            .contains("libmp3lame")
            .then_some(AudioCodec::LibMp3Lame),
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
                .or_else(|| auto_video_codec(format, encoder_names));
            Ok(CodecSelection {
                video_codec,
                audio_codec: audio_codec_for_format(format, encoder_names),
                quality_mode: video_codec
                    .map(|codec| quality_mode_for_codec(codec, video_quality_preference(advanced))),
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
    let runtime = cached_ffmpeg_capabilities(&ffmpeg_bin)?;
    let contract = ffmpeg_capability_contract()?;
    let mut video_codecs = Vec::new();
    video_codecs.extend(video_codec_capabilities_for_format(
        OutputFormat::Mp4,
        &runtime.encoder_names,
    ));
    video_codecs.extend(video_codec_capabilities_for_format(
        OutputFormat::Webm,
        &runtime.encoder_names,
    ));
    let mut feature_names = contract.features.keys().cloned().collect::<Vec<_>>();
    feature_names.sort();
    let features = feature_names
        .iter()
        .filter_map(|name| {
            contract
                .features
                .get(name)
                .map(|requirement| feature_capability(name, requirement, &runtime))
        })
        .collect();

    Ok(EncodeCapabilities {
        video_codecs,
        audio_bitrate_kbps: AUDIO_BITRATE_PRESETS_KBPS.to_vec(),
        ffmpeg_version: runtime.version,
        contract_schema_version: contract.schema_version,
        features,
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

fn normalized_codec_name(codec: Option<&str>) -> Option<String> {
    codec
        .map(str::trim)
        .filter(|codec| !codec.is_empty())
        .map(str::to_ascii_lowercase)
}

fn compatible_source_video_codec(format: OutputFormat, codec: Option<&str>) -> Option<VideoCodec> {
    let codec = codec.map(str::trim)?;
    match format {
        OutputFormat::Mp4 if codec.eq_ignore_ascii_case("h264") => Some(VideoCodec::LibX264),
        OutputFormat::Mp4 if codec.eq_ignore_ascii_case("mpeg4") => Some(VideoCodec::Mpeg4),
        OutputFormat::Webm if codec.eq_ignore_ascii_case("vp9") => Some(VideoCodec::LibVpxVp9),
        OutputFormat::Webm if codec.eq_ignore_ascii_case("vp8") => Some(VideoCodec::LibVpx),
        OutputFormat::Mp4 | OutputFormat::Webm | OutputFormat::Mp3 => None,
    }
}

fn video_copy_compatible(format: OutputFormat, codec: Option<&str>) -> bool {
    compatible_source_video_codec(format, codec).is_some()
}

fn audio_copy_compatible(format: OutputFormat, codec: Option<&str>) -> bool {
    let Some(codec) = codec.map(str::trim) else {
        return false;
    };
    match format {
        OutputFormat::Mp4 => codec.eq_ignore_ascii_case("aac"),
        OutputFormat::Webm => {
            codec.eq_ignore_ascii_case("opus") || codec.eq_ignore_ascii_case("vorbis")
        }
        OutputFormat::Mp3 => false,
    }
}

fn timeline_requires_encode(request: &EncodeRequest) -> bool {
    request.trim.is_some()
        || request.reverse
        || (request.speed - 1.0).abs() > 1e-9
        || request.loop_video
}

fn video_transform_requires_encode(request: &EncodeRequest) -> Result<bool, String> {
    Ok(timeline_requires_encode(request)
        || request.crop.is_some()
        || request.rotate_deg != 0
        || !matches!(requested_resize(request)?, ResizePlan::Source)
        || !color_is_noop(&request.color)
        || request.perturb_first_frame
        || request.subtitle_path.is_some())
}

fn color_metadata_is_complete(probe: &VideoProbe) -> bool {
    probe.color_range.is_some()
        && probe.color_primaries.is_some()
        && probe.color_transfer.is_some()
        && probe.color_space.is_some()
}

fn require_standard_sdr_contract(
    request: &EncodeRequest,
    selected_video_encoder: Option<VideoCodec>,
) -> Result<(), String> {
    if request.color_policy != ColorPolicy::StandardSdr {
        return Err(
            "This source requires the explicit Standard SDR color policy before video export."
                .to_string(),
        );
    }
    if request.format != OutputFormat::Mp4 {
        return Err("Standard SDR conversion currently requires MP4 output.".to_string());
    }
    if selected_video_encoder != Some(VideoCodec::LibX264) {
        return Err(
            "Standard SDR conversion currently requires the H.264 (libx264) encoder.".to_string(),
        );
    }
    Ok(())
}

fn require_supported_source_rotation(probe: &VideoProbe) -> Result<(), String> {
    if let Some(rotation_deg) = probe.unsupported_rotation_deg {
        return Err(format!(
            "The source uses a {rotation_deg:.3}° display rotation. Video export supports only exact 0°, 90°, 180°, or 270° source rotations because other display matrices cannot be mapped safely to crop and pixel-aspect geometry. Audio-only MP3 export remains available."
        ));
    }
    Ok(())
}

fn require_minimum_video_dimensions(probe: &VideoProbe) -> Result<(), String> {
    if probe.width < 2 || probe.height < 2 {
        return Err(format!(
            "Video export requires a source at least 2x2 pixels; this source is {}x{}. Audio-only MP3 export remains available.",
            probe.width, probe.height
        ));
    }
    Ok(())
}

fn require_safe_frame_export_color(probe: &VideoProbe) -> Result<(), String> {
    let bit_depth = probe.bit_depth.ok_or_else(|| {
        "Frame export is unavailable because the source pixel-component depth could not be determined."
            .to_string()
    })?;
    let unsafe_color = matches!(
        probe.dynamic_range,
        DynamicRange::Hdr10 | DynamicRange::Hlg | DynamicRange::DolbyVision
    ) || bit_depth > 8
        || (probe.dynamic_range == DynamicRange::Unknown
            && probe
                .color_transfer
                .as_deref()
                .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67")));
    if unsafe_color {
        return Err(
            "Frame export is available only for standard 8-bit SDR sources. This source requires color handling that PNG frame export does not apply safely."
                .to_string(),
        );
    }
    Ok(())
}

fn require_safe_frame_export_geometry(probe: &VideoProbe) -> Result<(), String> {
    if !probe.sample_aspect_ratio.is_square() {
        return Err(
            "Frame export is unavailable for non-square-pixel sources because PNG output would not preserve the visible display shape. Export a square-pixel video first."
                .to_string(),
        );
    }
    Ok(())
}

fn resolve_media_policy(
    request: &EncodeRequest,
    probe: &VideoProbe,
    selected_video_encoder: Option<VideoCodec>,
) -> Result<MediaPolicyPlan, String> {
    if request.format == OutputFormat::Mp3 {
        return Ok(MediaPolicyPlan {
            color_action: MediaColorAction::NotApplicable,
            sar_action: SarAction::NotApplicable,
        });
    }

    require_minimum_video_dimensions(probe)?;
    require_supported_source_rotation(probe)?;

    let source_bit_depth = probe.bit_depth.ok_or_else(|| {
        "The source pixel-component depth could not be determined, so video export cannot choose a safe color policy. Audio-only MP3 export remains available."
            .to_string()
    })?;
    let high_bit_depth = source_bit_depth > 8;
    let color_action = match probe.dynamic_range {
        DynamicRange::DolbyVision => {
            return Err(
                "Dolby Vision input is not supported for video export. Convert it to standard SDR first."
                    .to_string(),
            );
        }
        DynamicRange::Hlg => {
            return Err(
                "HLG HDR input is not supported for video export. Convert it to standard SDR first."
                    .to_string(),
            );
        }
        DynamicRange::Hdr10 => {
            require_standard_sdr_contract(request, selected_video_encoder)?;
            MediaColorAction::Hdr10ToStandardSdr
        }
        DynamicRange::Sdr if high_bit_depth => {
            if !color_metadata_is_complete(probe) {
                return Err(
                    "High-bit-depth SDR input has incomplete color metadata and cannot be converted safely."
                        .to_string(),
                );
            }
            require_standard_sdr_contract(request, selected_video_encoder)?;
            MediaColorAction::HighBitDepthSdrToStandardSdr
        }
        DynamicRange::Unknown
            if high_bit_depth
                || probe
                    .color_transfer
                    .as_deref()
                    .is_some_and(|transfer| matches!(transfer, "smpte2084" | "arib-std-b67")) =>
        {
            return Err(
                "The source has contradictory or incomplete high-bit-depth/HDR metadata, so Video For Lazies cannot choose a safe color conversion."
                    .to_string(),
            );
        }
        DynamicRange::Sdr | DynamicRange::Unknown => MediaColorAction::Unchanged,
    };

    let sar_action = if probe.sample_aspect_ratio.is_square() {
        SarAction::Unchanged
    } else {
        let (width, height) = estimated_output_dimensions(request, probe)?;
        SarAction::Normalize { width, height }
    };

    Ok(MediaPolicyPlan {
        color_action,
        sar_action,
    })
}

fn retained_source_duration_s(request: &EncodeRequest, probe: &VideoProbe) -> Result<f64, String> {
    let mut start = 0.0;
    let mut end = probe.duration_s;
    if let Some(trim) = &request.trim {
        start = trim.start_s.max(0.0);
        end = trim.end_s.unwrap_or(probe.duration_s).min(probe.duration_s);
    }
    if !start.is_finite() || !end.is_finite() || end <= start {
        return Err("Trim end must be greater than start.".to_string());
    }
    Ok(end - start)
}

fn guard_reverse_buffer_bytes(bytes: u64) -> Result<ReverseBufferEstimate, String> {
    if bytes >= REVERSE_BUFFER_HARD_LIMIT_BYTES {
        return Err(format!(
            "Reverse/Loop would require about {:.2} GiB of decoded buffering, above the 2 GiB safety limit. Trim shorter, crop or resize smaller, or disable Reverse/Loop.",
            bytes as f64 / (1024.0 * 1024.0 * 1024.0)
        ));
    }
    let action = if bytes >= REVERSE_BUFFER_WARNING_BYTES {
        ReverseBufferAction::Warning
    } else {
        ReverseBufferAction::WithinLimit
    };
    Ok(ReverseBufferEstimate { bytes, action })
}

fn reverse_buffer_estimate(
    request: &EncodeRequest,
    probe: &VideoProbe,
    media_policy: MediaPolicyPlan,
    retain_audio: bool,
) -> Result<Option<ReverseBufferEstimate>, String> {
    let reverse_video = request.reverse && request.format != OutputFormat::Mp3;
    let loop_video = request.loop_video && request.format != OutputFormat::Mp3;
    let reverse_audio = request.reverse && retain_audio && probe.has_audio;
    let loop_audio = loop_video && retain_audio && probe.has_audio;
    if !reverse_video && !loop_video && !reverse_audio && !loop_audio {
        return Ok(None);
    }

    let retained_duration_s = retained_source_duration_s(request, probe)?;
    let mut total_bytes = 0u128;

    if reverse_video || loop_video {
        let frame_rate = probe
            .frame_rate
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .ok_or_else(|| {
                "Reverse/Loop is unavailable because the source frame rate could not be determined."
                    .to_string()
            })?;
        let bytes_per_pixel = if media_policy.color_action.converts_to_standard_sdr() {
            1.5
        } else {
            probe.decoded_video_bytes_per_pixel.ok_or_else(|| {
                "Reverse/Loop is unavailable because the decoded pixel layout could not be determined."
                    .to_string()
            })?
        };
        let (width, height) = estimated_output_dimensions(request, probe)?;
        let source_frame_count = (retained_duration_s * frame_rate).ceil();
        if !source_frame_count.is_finite() || source_frame_count <= 0.0 {
            return Err("Reverse/Loop decoded frame count is invalid.".to_string());
        }
        let bytes_per_frame = ((width as f64) * (height as f64) * bytes_per_pixel).ceil() as u128
            + REVERSE_BUFFER_VIDEO_FRAME_OVERHEAD_BYTES;
        if reverse_video {
            total_bytes = total_bytes
                .saturating_add(bytes_per_frame.saturating_mul(source_frame_count as u128));
        }
        if loop_video {
            // The Loop reverse stage wraps the completed linear chain. setpts
            // changes timestamps but not frame count; only an active fps filter
            // drops frames before this stage.
            let loop_frame_count = if let Some(cap_fps) = frame_rate_cap_filter_fps(request, probe)?
            {
                (retained_duration_s / effective_speed(request) * cap_fps as f64).ceil()
            } else {
                source_frame_count
            };
            if !loop_frame_count.is_finite() || loop_frame_count <= 0.0 {
                return Err("Loop decoded frame count is invalid.".to_string());
            }
            total_bytes = total_bytes
                .saturating_add(bytes_per_frame.saturating_mul(loop_frame_count as u128));
        }
    }

    if reverse_audio || loop_audio {
        // FFprobe normally supplies these layout facts. Missing source facts are
        // not safe to guess because high-rate, multichannel layouts can exceed
        // ordinary 48 kHz stereo by a large factor. When normalization is active,
        // loudnorm/aresample make the post-filter rate and conservative sample
        // storage known, but the retained channel count must still be known.
        let sample_rate = if request.normalize_audio {
            48_000
        } else {
            probe.audio_sample_rate.ok_or_else(|| {
                "Reverse/Loop is unavailable because the retained audio sample rate could not be determined."
                    .to_string()
            })?
        } as u128;
        let channels = probe.audio_channels.ok_or_else(|| {
            "Reverse/Loop is unavailable because the retained audio channel count could not be determined."
                .to_string()
        })? as u128;
        let bytes_per_sample = if request.normalize_audio {
            8
        } else {
            probe.decoded_audio_bytes_per_sample.ok_or_else(|| {
                "Reverse/Loop is unavailable because the retained decoded audio sample format could not be determined."
                    .to_string()
            })?
        } as u128;
        let bytes_per_audio_frame = channels.saturating_mul(bytes_per_sample);
        if reverse_audio {
            let sample_count = (retained_duration_s * sample_rate as f64).ceil() as u128;
            let frame_count = sample_count.div_ceil(REVERSE_AUDIO_FRAME_SAMPLES);
            total_bytes = total_bytes
                .saturating_add(sample_count.saturating_mul(bytes_per_audio_frame))
                .saturating_add(
                    frame_count.saturating_mul(REVERSE_BUFFER_AUDIO_FRAME_OVERHEAD_BYTES),
                );
        }
        if loop_audio {
            let sample_count = (retained_duration_s / effective_speed(request) * sample_rate as f64)
                .ceil() as u128;
            let frame_count = sample_count.div_ceil(REVERSE_AUDIO_FRAME_SAMPLES);
            total_bytes = total_bytes
                .saturating_add(sample_count.saturating_mul(bytes_per_audio_frame))
                .saturating_add(
                    frame_count.saturating_mul(REVERSE_BUFFER_AUDIO_FRAME_OVERHEAD_BYTES),
                );
        }
    }

    total_bytes = total_bytes.saturating_mul((REVERSE_BUFFER_SAFETY_FACTOR * 100.0) as u128) / 100;
    let bytes = total_bytes.min(u64::MAX as u128) as u64;
    guard_reverse_buffer_bytes(bytes).map(Some)
}

fn build_encode_command_plan(
    request: &EncodeRequest,
    probe: &VideoProbe,
    codec_selection: CodecSelection,
    resolved_perturb_seed: Option<u32>,
) -> Result<EncodeCommandPlan, String> {
    let size_limit_enabled = request.size_limit_mb > 0.0;
    let advanced_audio_bitrate = advanced_audio_bitrate_kbps(request)?;
    let advanced_audio_channels = audio_channel_count(request);
    let encode_speed = encode_speed_preference(&request.advanced);

    let mut resolved_request = request.clone();
    if request.perturb_first_frame {
        resolved_request.perturb_seed = Some(
            resolved_perturb_seed
                .ok_or_else(|| "Missing resolved first-frame perturbation seed.".to_string())?,
        );
    }

    let media_policy = resolve_media_policy(request, probe, codec_selection.video_codec)?;
    let audio_filters = build_audio_filters(&resolved_request, probe)?;
    let video_filters = if matches!(request.format, OutputFormat::Mp3) {
        None
    } else {
        build_video_filters_with_policy(&resolved_request, probe, media_policy)?
    };
    if matches!(request.format, OutputFormat::Mp3) {
        if !request.audio_enabled {
            return Err("Audio must be enabled for MP3 output.".to_string());
        }
        let audio_stream_index = probe
            .audio_stream_index
            .ok_or_else(|| "Input file has no audio stream.".to_string())?;
        let audio_encoder = codec_selection.audio_codec.ok_or_else(|| {
            "No MP3 audio encoder is available in the active FFmpeg build.".to_string()
        })?;
        let reverse_buffer_estimate = reverse_buffer_estimate(request, probe, media_policy, true)?;
        return Ok(EncodeCommandPlan {
            mode: EncodeMode::AudioOnlyEncode,
            video_stream_index: None,
            audio_stream_index: Some(audio_stream_index),
            source_video_codec: probe.video_codec.clone(),
            source_audio_codec: probe.audio_codec.clone(),
            video_action: StreamAction::Drop,
            audio_action: StreamAction::Encode,
            video_encoder: None,
            audio_encoder: Some(audio_encoder),
            output_video_codec: None,
            output_audio_codec: Some(audio_encoder.as_codec_name().to_string()),
            quality_mode: None,
            encode_speed,
            audio_bitrate_kbps: advanced_audio_bitrate,
            audio_channels: advanced_audio_channels,
            video_filters: None,
            audio_filters,
            video_reasons: vec![PlanReason::AudioOnlyOutput],
            audio_reasons: vec![PlanReason::AudioOnlyOutput],
            size_contract: None,
            size_copy_candidates: Vec::new(),
            media_policy,
            reverse_buffer_estimate,
        });
    }

    let selected_video_encoder = codec_selection.video_codec;
    let selected_audio_encoder = codec_selection.audio_codec;
    let timeline_change = timeline_requires_encode(request);

    let mut video_reasons = Vec::new();
    let mut natural_video_action =
        if video_copy_compatible(request.format, probe.video_codec.as_deref()) {
            video_reasons.push(PlanReason::CompatibleSourceCodec);
            StreamAction::Copy
        } else {
            video_reasons.push(PlanReason::IncompatibleSourceCodec);
            StreamAction::Encode
        };

    if video_transform_requires_encode(request)? {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(if timeline_change {
            PlanReason::TimelineTransform
        } else {
            PlanReason::VideoTransform
        });
    }
    if request.subtitle_path.is_some() {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::SubtitleBurnIn);
    }
    if media_policy.color_action.converts_to_standard_sdr() {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::ColorConversion);
    }
    if media_policy.sar_action.normalizes() {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::SampleAspectRatio);
    }
    if request
        .advanced
        .video_codec
        .unwrap_or(VideoCodecPreference::Auto)
        != VideoCodecPreference::Auto
    {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::ExplicitVideoCodec);
    }
    if !size_limit_enabled
        && video_quality_preference(&request.advanced) != VideoQualityPreference::Auto
    {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::VideoQuality);
    }
    if encode_speed != EncodeSpeedPreference::Auto {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::EncodeSpeed);
    }
    if frame_rate_cap_filter_fps(request, probe)?.is_some() {
        natural_video_action = StreamAction::Encode;
        video_reasons.push(PlanReason::FrameRateCap);
    }

    let mut audio_reasons = Vec::new();
    let mut natural_audio_action = if !request.audio_enabled {
        audio_reasons.push(PlanReason::AudioDisabled);
        StreamAction::Drop
    } else if probe.audio_stream_index.is_none() {
        audio_reasons.push(PlanReason::MissingStream);
        StreamAction::Drop
    } else if audio_copy_compatible(request.format, probe.audio_codec.as_deref()) {
        audio_reasons.push(PlanReason::CompatibleSourceCodec);
        StreamAction::Copy
    } else {
        audio_reasons.push(PlanReason::IncompatibleSourceCodec);
        StreamAction::Encode
    };

    if natural_audio_action != StreamAction::Drop {
        if timeline_change {
            natural_audio_action = StreamAction::Encode;
            audio_reasons.push(PlanReason::TimelineTransform);
        }
        if request.normalize_audio {
            natural_audio_action = StreamAction::Encode;
            audio_reasons.push(PlanReason::AudioNormalization);
        }
        if !size_limit_enabled && advanced_audio_bitrate.is_some() {
            natural_audio_action = StreamAction::Encode;
            audio_reasons.push(PlanReason::AudioBitrate);
        }
        if advanced_audio_channels.is_some() {
            natural_audio_action = StreamAction::Encode;
            audio_reasons.push(PlanReason::AudioChannels);
        }
    }

    if size_limit_enabled {
        let copy_audio_action = if natural_video_action == StreamAction::Copy {
            planned_size_copy_audio_action(
                request,
                probe,
                natural_audio_action,
                selected_video_encoder,
            )?
        } else {
            natural_audio_action
        };
        let mut size_copy_candidates = Vec::new();
        if natural_video_action == StreamAction::Copy {
            if natural_audio_action == StreamAction::Copy {
                size_copy_candidates.push(CopyCandidatePlan {
                    video_stream_index: probe.video_stream_index,
                    audio_stream_index: probe.audio_stream_index,
                    audio_action: StreamAction::Copy,
                });
            }
            if copy_audio_action == StreamAction::Drop {
                size_copy_candidates.push(CopyCandidatePlan {
                    video_stream_index: probe.video_stream_index,
                    audio_stream_index: probe.audio_stream_index,
                    audio_action: StreamAction::Drop,
                });
            }
        }
        let copy_only_plan =
            |candidates: Vec<CopyCandidatePlan>| -> Result<EncodeCommandPlan, String> {
                let candidate = candidates
                    .first()
                    .ok_or_else(|| "Missing size-copy candidate.".to_string())?;
                let reverse_buffer_estimate = reverse_buffer_estimate(
                    request,
                    probe,
                    media_policy,
                    candidate.audio_action != StreamAction::Drop,
                )?;
                let mut copy_video_reasons = video_reasons.clone();
                let mut copy_audio_reasons = audio_reasons.clone();
                copy_video_reasons.push(PlanReason::SizeTarget);
                copy_audio_reasons.push(PlanReason::SizeTarget);
                Ok(EncodeCommandPlan {
                    mode: EncodeMode::SizeTargeted,
                    video_stream_index: Some(probe.video_stream_index),
                    audio_stream_index: probe.audio_stream_index,
                    source_video_codec: probe.video_codec.clone(),
                    source_audio_codec: probe.audio_codec.clone(),
                    video_action: StreamAction::Copy,
                    audio_action: candidate.audio_action,
                    video_encoder: selected_video_encoder,
                    audio_encoder: selected_audio_encoder,
                    output_video_codec: probe.video_codec.clone(),
                    output_audio_codec: (candidate.audio_action == StreamAction::Copy)
                        .then(|| probe.audio_codec.clone())
                        .flatten(),
                    quality_mode: codec_selection.quality_mode,
                    encode_speed,
                    audio_bitrate_kbps: None,
                    audio_channels: advanced_audio_channels,
                    video_filters: video_filters.clone(),
                    audio_filters: audio_filters.clone(),
                    video_reasons: copy_video_reasons,
                    audio_reasons: copy_audio_reasons,
                    size_contract: None,
                    size_copy_candidates: candidates,
                    media_policy,
                    reverse_buffer_estimate,
                })
            };
        if !size_copy_candidates.is_empty() {
            return copy_only_plan(size_copy_candidates);
        }
        let selected_video_encoder = selected_video_encoder.ok_or_else(|| {
            format!(
                "No compatible video encoder is available for {} size-targeted export.",
                match request.format {
                    OutputFormat::Mp4 => "MP4",
                    OutputFormat::Webm => "WebM",
                    OutputFormat::Mp3 => unreachable!(),
                }
            )
        })?;
        let output_duration_s = estimate_output_duration_s(
            probe.duration_s,
            &request.trim,
            request.speed,
            request.loop_video,
        )?;
        let include_audio = request.audio_enabled && probe.audio_stream_index.is_some();
        let size_contract = build_size_limited_encode_contract(
            request,
            probe,
            selected_video_encoder,
            output_duration_s,
            include_audio,
        )?;
        let audio_action = if size_contract.plan.include_audio {
            StreamAction::Encode
        } else {
            StreamAction::Drop
        };
        let planned_audio_encoder = if audio_action == StreamAction::Encode {
            match selected_audio_encoder {
                Some(encoder) => Some(encoder),
                None => return Err(
                    "No compatible audio encoder is available for the selected size-targeted output."
                        .to_string(),
                ),
            }
        } else {
            None
        };
        let reverse_buffer_estimate = reverse_buffer_estimate(
            request,
            probe,
            media_policy,
            audio_action == StreamAction::Encode,
        )?;
        video_reasons.push(PlanReason::SizeTarget);
        audio_reasons.push(PlanReason::SizeTarget);

        return Ok(EncodeCommandPlan {
            mode: EncodeMode::SizeTargeted,
            video_stream_index: Some(probe.video_stream_index),
            audio_stream_index: probe.audio_stream_index,
            source_video_codec: probe.video_codec.clone(),
            source_audio_codec: probe.audio_codec.clone(),
            video_action: StreamAction::Encode,
            audio_action,
            video_encoder: Some(selected_video_encoder),
            audio_encoder: planned_audio_encoder,
            output_video_codec: Some(selected_video_encoder.as_codec_name().to_string()),
            output_audio_codec: planned_audio_encoder
                .map(|codec| codec.as_codec_name().to_string()),
            quality_mode: codec_selection.quality_mode,
            encode_speed,
            audio_bitrate_kbps: None,
            audio_channels: advanced_audio_channels,
            video_filters,
            audio_filters,
            video_reasons,
            audio_reasons,
            size_contract: Some(size_contract),
            size_copy_candidates: Vec::new(),
            media_policy,
            reverse_buffer_estimate,
        });
    }

    let mode = match (natural_video_action, natural_audio_action) {
        (StreamAction::Copy, StreamAction::Copy | StreamAction::Drop) => EncodeMode::Remux,
        (StreamAction::Copy, StreamAction::Encode) => EncodeMode::VideoCopyAudioEncode,
        (StreamAction::Encode, StreamAction::Copy) => EncodeMode::VideoEncodeAudioCopy,
        (StreamAction::Encode, StreamAction::Encode | StreamAction::Drop) => EncodeMode::FullEncode,
        (StreamAction::Drop, _) => unreachable!("video output must retain a video stream"),
    };
    if natural_video_action == StreamAction::Encode && selected_video_encoder.is_none() {
        return Err(format!(
            "No compatible video encoder is available for {} export with the selected transforms.",
            match request.format {
                OutputFormat::Mp4 => "MP4",
                OutputFormat::Webm => "WebM",
                OutputFormat::Mp3 => unreachable!(),
            }
        ));
    }
    if natural_video_action == StreamAction::Encode {
        let selected_video_encoder = selected_video_encoder
            .ok_or_else(|| "Missing video encoder for output-dimension validation.".to_string())?;
        let (width, height) = estimated_output_dimensions(request, probe)?;
        validate_codec_output_dimensions(selected_video_encoder, width, height)?;
    }
    if natural_audio_action == StreamAction::Encode && selected_audio_encoder.is_none() {
        return Err(
            "No compatible audio encoder is available for the selected output and transforms."
                .to_string(),
        );
    }
    let reverse_buffer_estimate = reverse_buffer_estimate(
        request,
        probe,
        media_policy,
        natural_audio_action != StreamAction::Drop,
    )?;
    let output_video_codec = match natural_video_action {
        StreamAction::Copy => probe.video_codec.clone(),
        StreamAction::Encode => {
            selected_video_encoder.map(|codec| codec.as_codec_name().to_string())
        }
        StreamAction::Drop => None,
    };
    let output_audio_codec = match natural_audio_action {
        StreamAction::Copy => probe.audio_codec.clone(),
        StreamAction::Encode => {
            selected_audio_encoder.map(|codec| codec.as_codec_name().to_string())
        }
        StreamAction::Drop => None,
    };

    Ok(EncodeCommandPlan {
        mode,
        video_stream_index: Some(probe.video_stream_index),
        audio_stream_index: probe.audio_stream_index,
        source_video_codec: probe.video_codec.clone(),
        source_audio_codec: probe.audio_codec.clone(),
        video_action: natural_video_action,
        audio_action: natural_audio_action,
        video_encoder: selected_video_encoder,
        audio_encoder: (natural_audio_action != StreamAction::Drop)
            .then_some(selected_audio_encoder)
            .flatten(),
        output_video_codec,
        output_audio_codec,
        quality_mode: codec_selection.quality_mode,
        encode_speed,
        audio_bitrate_kbps: advanced_audio_bitrate,
        audio_channels: advanced_audio_channels,
        video_filters,
        audio_filters,
        video_reasons,
        audio_reasons,
        size_contract: None,
        size_copy_candidates: Vec::new(),
        media_policy,
        reverse_buffer_estimate,
    })
}

fn build_size_reencode_fallback_plan(
    request: &EncodeRequest,
    probe: &VideoProbe,
    initial_plan: &EncodeCommandPlan,
) -> Result<EncodeCommandPlan, String> {
    let selected_video_encoder = initial_plan.video_encoder.ok_or_else(|| {
        "Compatible stream copy could not satisfy the size target, and the active FFmpeg build has no compatible video encoder fallback."
            .to_string()
    })?;
    let output_duration_s = estimate_output_duration_s(
        probe.duration_s,
        &request.trim,
        request.speed,
        request.loop_video,
    )?;
    let include_audio = request.audio_enabled && probe.audio_stream_index.is_some();
    let size_contract = build_size_limited_encode_contract(
        request,
        probe,
        selected_video_encoder,
        output_duration_s,
        include_audio,
    )?;
    let audio_action = if size_contract.plan.include_audio {
        StreamAction::Encode
    } else {
        StreamAction::Drop
    };
    let planned_audio_encoder = if audio_action == StreamAction::Encode {
        Some(initial_plan.audio_encoder.ok_or_else(|| {
            "Compatible stream copy could not satisfy the size target, and the active FFmpeg build has no compatible audio encoder fallback."
                .to_string()
        })?)
    } else {
        None
    };
    let reverse_buffer_estimate = reverse_buffer_estimate(
        request,
        probe,
        initial_plan.media_policy,
        audio_action == StreamAction::Encode,
    )?;
    let mut fallback = initial_plan.clone();
    fallback.video_action = StreamAction::Encode;
    fallback.audio_action = audio_action;
    fallback.audio_encoder = planned_audio_encoder;
    fallback.output_video_codec = Some(selected_video_encoder.as_codec_name().to_string());
    fallback.output_audio_codec =
        planned_audio_encoder.map(|codec| codec.as_codec_name().to_string());
    fallback.size_contract = Some(size_contract);
    fallback.size_copy_candidates.clear();
    fallback.reverse_buffer_estimate = reverse_buffer_estimate;
    Ok(fallback)
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

fn oriented_sample_aspect_ratio(req: &EncodeRequest, probe: &VideoProbe) -> f64 {
    let mut ratio = probe.sample_aspect_ratio.as_f64();
    if matches!(probe.rotation_deg, 90 | 270) {
        ratio = 1.0 / ratio;
    }
    if matches!(req.rotate_deg, 90 | 270) {
        ratio = 1.0 / ratio;
    }
    ratio
}

#[cfg(test)]
fn fit_max_edge_dimensions(width: u32, height: u32, max_edge_px: u32) -> (u32, u32) {
    let long_edge = width.max(height);
    if long_edge <= max_edge_px {
        return (even_at_least_two(width), even_at_least_two(height));
    }
    if width >= height {
        let scaled_height =
            (((height as f64) * (max_edge_px as f64)) / (width as f64)).round() as u32;
        (
            even_at_least_two(max_edge_px),
            even_at_least_two(scaled_height),
        )
    } else {
        let scaled_width =
            (((width as f64) * (max_edge_px as f64)) / (height as f64)).round() as u32;
        (
            even_at_least_two(scaled_width),
            even_at_least_two(max_edge_px),
        )
    }
}

fn estimated_output_dimensions(
    req: &EncodeRequest,
    probe: &VideoProbe,
) -> Result<(u32, u32), String> {
    let mut width = probe.width.max(2);
    let mut height = probe.height.max(2);

    if let Some(crop) = &req.crop {
        validate_crop(crop, probe)?;
        width = even_at_least_two(crop.width);
        height = even_at_least_two(crop.height);
    }

    match req.rotate_deg {
        90 | 270 => std::mem::swap(&mut width, &mut height),
        _ => {}
    }

    let resize_plan = requested_resize(req)?;
    let mut logical_width = width as f64;
    let logical_height = height as f64;
    if !probe.sample_aspect_ratio.is_square() && !matches!(resize_plan, ResizePlan::Custom { .. }) {
        let effective_sar = oriented_sample_aspect_ratio(req, probe);
        if !effective_sar.is_finite() || effective_sar <= 0.0 {
            return Err("Could not normalize the source sample aspect ratio.".to_string());
        }
        logical_width *= effective_sar;
    }

    match resize_plan {
        ResizePlan::Source => {
            if !logical_width.is_finite() || !logical_height.is_finite() {
                return Err("Could not calculate finite square-pixel source dimensions. Choose a bounded resize or correct the source aspect metadata.".to_string());
            }
            width = even_at_least_two(logical_width.round() as u32);
            height = even_at_least_two(logical_height.round() as u32);
        }
        ResizePlan::MaxEdge(max_edge_px) => {
            if !logical_width.is_finite() || logical_width <= 0.0 || logical_height <= 0.0 {
                return Err(
                    "Could not calculate bounded square-pixel output dimensions.".to_string(),
                );
            }
            let long_edge = logical_width.max(logical_height);
            let scale = if long_edge > max_edge_px as f64 {
                max_edge_px as f64 / long_edge
            } else {
                1.0
            };
            width = even_at_least_two((logical_width * scale).round() as u32);
            height = even_at_least_two((logical_height * scale).round() as u32);
        }
        ResizePlan::Custom {
            width_px,
            height_px,
        } => {
            width = even_at_least_two(width_px);
            height = even_at_least_two(height_px);
        }
    }

    // Apply the same final bound to every path, including metadata-derived
    // dimensions. Custom dimensions stay authoritative and a max-edge resize
    // may safely tame extreme source aspect metadata before this check.
    let width = even_at_least_two(width);
    let height = even_at_least_two(height);
    if width > OUTPUT_DIMENSION_MAX_PX || height > OUTPUT_DIMENSION_MAX_PX {
        return Err(format!(
            "Planned output dimensions must each be <= {OUTPUT_DIMENSION_MAX_PX} px; calculated {width}x{height}."
        ));
    }
    Ok((width, height))
}

fn validate_crop(crop: &Crop, probe: &VideoProbe) -> Result<(), String> {
    if crop.width < 2 || crop.height < 2 {
        return Err("Crop width and height must each be at least 2 pixels.".to_string());
    }
    if crop.x.saturating_add(crop.width) > probe.width
        || crop.y.saturating_add(crop.height) > probe.height
    {
        return Err("Crop area is outside the video bounds.".to_string());
    }
    Ok(())
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

fn planned_size_copy_audio_action(
    request: &EncodeRequest,
    probe: &VideoProbe,
    natural_audio_action: StreamAction,
    selected_video_encoder: Option<VideoCodec>,
) -> Result<StreamAction, String> {
    if natural_audio_action == StreamAction::Drop {
        return Ok(StreamAction::Drop);
    }
    let planning_codec = selected_video_encoder
        .or_else(|| compatible_source_video_codec(request.format, probe.video_codec.as_deref()))
        .ok_or_else(|| {
            "Could not identify the compatible source video codec for size planning.".to_string()
        })?;
    let output_duration_s = estimate_output_duration_s(
        probe.duration_s,
        &request.trim,
        request.speed,
        request.loop_video,
    )?;
    let min_video_kbps = minimum_video_bitrate_kbps(
        planning_codec,
        probe.width.max(2),
        probe.height.max(2),
        output_frame_rate_for_planning(request, probe)?,
    );
    let plan = plan_bitrates(
        request.size_limit_mb,
        output_duration_s,
        request.audio_enabled && probe.audio_stream_index.is_some(),
        96,
        32,
        min_video_kbps,
        0.95,
    )?;
    Ok(if plan.include_audio {
        natural_audio_action
    } else {
        StreamAction::Drop
    })
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

fn output_path_identity(path: &str) -> String {
    let looks_windows = (path.as_bytes().get(1) == Some(&b':')
        && path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic))
        || path.starts_with("\\\\")
        || path.starts_with("//")
        || path.contains('\\');
    if !looks_windows {
        return format!("posix:{path}");
    }

    let mut normalized = String::with_capacity(path.len());
    let mut previous_was_slash = false;
    for character in path.chars() {
        let character = if character == '\\' { '/' } else { character };
        if character == '/' {
            if previous_was_slash {
                continue;
            }
            previous_was_slash = true;
        } else {
            previous_was_slash = false;
        }
        normalized.extend(character.to_lowercase());
    }
    format!("windows:{normalized}")
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
    let taken: HashSet<String> = taken_paths
        .iter()
        .map(|path| output_path_identity(path))
        .collect();

    let max_tries = 10_000u32;
    for i in start_n..start_n.saturating_add(max_tries) {
        let file = format!("{base}-{i}.{ext}");
        let candidate = parent.join(file);
        let candidate_str = candidate.to_string_lossy().to_string();
        if !candidate.exists() && !taken.contains(&output_path_identity(&candidate_str)) {
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
    let temp_path = create_temp_output(&output_path, 0, "frame")?;

    let ffmpeg_bin = default_ffmpeg();
    let probe = probe_video(input_path.to_string_lossy().to_string())?;
    require_supported_source_rotation(&probe)?;
    require_safe_frame_export_color(&probe)?;
    require_safe_frame_export_geometry(&probe)?;
    let runtime_capabilities = cached_ffmpeg_capabilities(&ffmpeg_bin)?;
    let capability_contract = ffmpeg_capability_contract()?;
    require_ffmpeg_capability_subset(
        "PNG frame export",
        ["png"],
        std::iter::empty::<&str>(),
        &capability_contract,
        &runtime_capabilities,
    )?;

    let mut cmd = command_no_window(&ffmpeg_bin);
    cmd.args(frame_extract_command_args(
        &input_path,
        time_s,
        probe.video_stream_index,
        temp_path.as_ref(),
    ));
    let output = run_command_output_with_timeout(
        cmd,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &ffmpeg_bin,
        "frame export",
        FRAME_EXTRACT_TIMEOUT,
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = stderr.lines().collect();
        let start = lines.len().saturating_sub(20);
        let tail = lines[start..].join("\n");
        return Err(format!("Frame export failed.\n\n{tail}"));
    }

    publish_output_file(temp_path, &output_path)?;

    Ok(())
}

fn frame_extract_command_args(
    input_path: &Path,
    time_s: f64,
    video_stream_index: u32,
    output_path: &Path,
) -> Vec<OsString> {
    // Input-side seeking: output-side -ss decodes the whole stream up to the
    // timestamp and times out on long sources.
    [
        OsString::from("-y"),
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("error"),
        OsString::from("-nostdin"),
        OsString::from("-ss"),
        OsString::from(format!("{time_s:.3}")),
        OsString::from("-i"),
        input_path.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from(format!("0:{video_stream_index}")),
        OsString::from("-frames:v"),
        OsString::from("1"),
        OsString::from("-an"),
        output_path.as_os_str().to_os_string(),
    ]
    .into_iter()
    .collect()
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
    format_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FFProbeSideData {
    side_data_type: Option<String>,
    rotation: Option<serde_json::Value>,
    displaymatrix: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FFProbeStreamTags {
    rotate: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct FFProbeDisposition {
    #[serde(default)]
    default: u8,
    #[serde(default)]
    original: u8,
    #[serde(default)]
    comment: u8,
    #[serde(default)]
    forced: u8,
    #[serde(default)]
    hearing_impaired: u8,
    #[serde(default)]
    visual_impaired: u8,
    #[serde(default)]
    attached_pic: u8,
    #[serde(default)]
    timed_thumbnails: u8,
    #[serde(default)]
    still_image: u8,
}

#[derive(Debug, Deserialize)]
struct FFProbeStream {
    index: Option<u32>,
    codec_name: Option<String>,
    codec_type: Option<String>,
    duration: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<String>,
    bits_per_sample: Option<u8>,
    color_range: Option<String>,
    color_primaries: Option<String>,
    color_transfer: Option<String>,
    color_space: Option<String>,
    sample_aspect_ratio: Option<String>,
    display_aspect_ratio: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    sample_fmt: Option<String>,
    codec_tag_string: Option<String>,
    profile: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    #[serde(default)]
    side_data_list: Vec<FFProbeSideData>,
    tags: Option<FFProbeStreamTags>,
    #[serde(default)]
    disposition: FFProbeDisposition,
}

fn select_primary_video_stream(streams: &[FFProbeStream]) -> Option<&FFProbeStream> {
    streams
        .iter()
        .filter(|stream| {
            stream.codec_type.as_deref() == Some("video")
                && stream.index.is_some()
                && stream.width.is_some()
                && stream.height.is_some()
                && stream.disposition.attached_pic == 0
                && stream.disposition.timed_thumbnails == 0
                && stream.disposition.still_image == 0
        })
        .min_by_key(|stream| {
            (
                stream.disposition.default == 0,
                stream.index.unwrap_or(u32::MAX),
            )
        })
}

fn select_primary_audio_stream(streams: &[FFProbeStream]) -> Option<&FFProbeStream> {
    streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("audio") && stream.index.is_some())
        .min_by_key(|stream| {
            (
                stream.disposition.default == 0,
                stream.index.unwrap_or(u32::MAX),
            )
        })
}

fn json_number_to_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SourceRotation {
    quarter_turn_deg: u32,
    unsupported_deg: Option<f64>,
}

fn parse_display_matrix(value: &str) -> Option<[[i64; 3]; 3]> {
    let rows = value
        .lines()
        .filter_map(|line| {
            let values = line
                .split_once(':')?
                .1
                .split_whitespace()
                .filter_map(|value| value.parse::<i64>().ok())
                .collect::<Vec<_>>();
            (values.len() == 3).then(|| [values[0], values[1], values[2]])
        })
        .collect::<Vec<_>>();
    (rows.len() == 3).then(|| [rows[0], rows[1], rows[2]])
}

fn pure_quarter_turn_from_display_matrix(matrix: [[i64; 3]; 3]) -> Option<u32> {
    const UNIT: i64 = 65_536;
    const PERSPECTIVE_UNIT: i64 = 1_073_741_824;
    const TOLERANCE: i64 = 2;
    let near = |actual: i64, expected: i64| actual.abs_diff(expected) <= TOLERANCE as u64;
    if !near(matrix[0][2], 0)
        || !near(matrix[1][2], 0)
        || !near(matrix[2][0], 0)
        || !near(matrix[2][1], 0)
        || !near(matrix[2][2], PERSPECTIVE_UNIT)
    {
        return None;
    }
    let [a, b, _, c, d] = [
        matrix[0][0],
        matrix[0][1],
        matrix[0][2],
        matrix[1][0],
        matrix[1][1],
    ];
    if near(a, UNIT) && near(b, 0) && near(c, 0) && near(d, UNIT) {
        Some(0)
    } else if near(a, -UNIT) && near(b, 0) && near(c, 0) && near(d, -UNIT) {
        Some(180)
    } else if near(a, 0) && near(b, -UNIT) && near(c, UNIT) && near(d, 0) {
        Some(90)
    } else if near(a, 0) && near(b, UNIT) && near(c, -UNIT) && near(d, 0) {
        Some(270)
    } else {
        None
    }
}

fn display_matrix_rotation_deg(matrix: [[i64; 3]; 3]) -> f64 {
    let rotation = (matrix[1][0] as f64)
        .atan2(matrix[0][0] as f64)
        .to_degrees();
    ((rotation % 360.0) + 360.0) % 360.0
}

/// Read display rotation from the matrix or legacy tag. Only exact quarter
/// turns are modeled by the crop/SAR geometry. FFmpeg can apply arbitrary
/// matrices without changing the coded canvas, so preserve every other angle
/// as an explicit unsupported fact instead of coercing or ignoring it.
fn stream_rotation(stream: &FFProbeStream) -> SourceRotation {
    let display_matrix = stream.side_data_list.iter().find(|sd| {
        sd.side_data_type
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("Display Matrix"))
    });
    let raw = display_matrix
        .and_then(|sd| sd.rotation.as_ref().and_then(json_number_to_f64))
        .or_else(|| {
            stream
                .tags
                .as_ref()
                .and_then(|t| t.rotate.as_deref())
                .and_then(|r| r.trim().parse::<f64>().ok())
        });
    if display_matrix.is_some()
        && display_matrix
            .and_then(|side_data| side_data.displaymatrix.as_deref())
            .is_none()
    {
        let normalized = raw
            .filter(|value| value.is_finite())
            .map(|value| ((value % 360.0) + 360.0) % 360.0)
            .unwrap_or(0.0);
        return SourceRotation {
            quarter_turn_deg: 0,
            unsupported_deg: Some(normalized),
        };
    }
    if let Some(matrix_text) =
        display_matrix.and_then(|side_data| side_data.displaymatrix.as_deref())
    {
        let Some(matrix) = parse_display_matrix(matrix_text) else {
            return SourceRotation {
                quarter_turn_deg: 0,
                unsupported_deg: Some(raw.filter(|value| value.is_finite()).unwrap_or(0.0)),
            };
        };
        if let Some(matrix_quarter_turn) = pure_quarter_turn_from_display_matrix(matrix) {
            return SourceRotation {
                // The matrix is authoritative. ffprobe's convenience rotation
                // value is rounded and can disagree for 89°/91° matrices.
                quarter_turn_deg: matrix_quarter_turn,
                unsupported_deg: None,
            };
        }
        return SourceRotation {
            quarter_turn_deg: 0,
            unsupported_deg: Some(display_matrix_rotation_deg(matrix)),
        };
    }
    let Some(raw) = raw else {
        return SourceRotation {
            quarter_turn_deg: 0,
            unsupported_deg: None,
        };
    };
    if !raw.is_finite() {
        return SourceRotation {
            quarter_turn_deg: 0,
            unsupported_deg: None,
        };
    }
    let normalized = ((raw % 360.0) + 360.0) % 360.0;
    let nearest = (normalized / 90.0).round() * 90.0;
    if (normalized - nearest).abs() <= 0.001 || (normalized - 360.0).abs() <= 0.001 {
        return SourceRotation {
            quarter_turn_deg: ((nearest as u32) % 360),
            unsupported_deg: None,
        };
    }
    SourceRotation {
        quarter_turn_deg: 0,
        unsupported_deg: Some(normalized),
    }
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

fn gcd_u64(mut left: u64, mut right: u64) -> u64 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn reduced_rational(numerator: u64, denominator: u64) -> Option<Rational> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let gcd = gcd_u64(numerator, denominator);
    let numerator = numerator / gcd;
    let denominator = denominator / gcd;
    if numerator > u32::MAX as u64 || denominator > u32::MAX as u64 {
        return None;
    }
    Some(Rational {
        numerator: numerator as u32,
        denominator: denominator as u32,
    })
}

fn parse_ffprobe_rational(value: Option<&str>) -> Option<Rational> {
    let value = value?.trim();
    let (numerator, denominator) = value.split_once(':').or_else(|| value.split_once('/'))?;
    reduced_rational(
        numerator.trim().parse::<u64>().ok()?,
        denominator.trim().parse::<u64>().ok()?,
    )
}

fn normalized_probe_field(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "unknown" | "unspecified" | "reserved" | "reserved0" | "n/a"
                )
        })
        .map(str::to_ascii_lowercase)
}

fn stream_dispositions(disposition: &FFProbeDisposition) -> StreamDispositions {
    StreamDispositions {
        default: disposition.default != 0,
        original: disposition.original != 0,
        comment: disposition.comment != 0,
        forced: disposition.forced != 0,
        hearing_impaired: disposition.hearing_impaired != 0,
        visual_impaired: disposition.visual_impaired != 0,
        attached_pic: disposition.attached_pic != 0,
        timed_thumbnails: disposition.timed_thumbnails != 0,
        still_image: disposition.still_image != 0,
    }
}

fn pixel_format_bit_depth(pixel_format: Option<&str>) -> Option<u8> {
    let pixel_format = pixel_format?.trim().to_ascii_lowercase();
    if pixel_format.is_empty() {
        return None;
    }
    if pixel_format.starts_with("gbrpf32") || pixel_format.starts_with("gbrapf32") {
        return Some(32);
    }
    if pixel_format.starts_with("gbrpf16")
        || pixel_format.starts_with("gbrapf16")
        || pixel_format.starts_with("rgb48")
        || pixel_format.starts_with("bgr48")
        || pixel_format.starts_with("rgba64")
        || pixel_format.starts_with("bgra64")
    {
        return Some(16);
    }
    // These numbers describe chroma layout, not component depth. Handle them
    // before scanning pixel-format suffixes for an explicit bit depth.
    if pixel_format.starts_with("nv12")
        || pixel_format.starts_with("nv16")
        || pixel_format.starts_with("nv21")
        || pixel_format.starts_with("nv24")
        || pixel_format.starts_with("nv42")
    {
        return Some(8);
    }
    if pixel_format.starts_with("nv20") {
        return Some(10);
    }
    for depth in [16u8, 14, 12, 10, 9] {
        if pixel_format.contains(&depth.to_string()) {
            return Some(depth);
        }
    }
    if pixel_format.starts_with("p010") {
        return Some(10);
    }
    if pixel_format.starts_with("p012") {
        return Some(12);
    }
    if pixel_format.starts_with("p016") || pixel_format.contains("48") {
        return Some(16);
    }
    [
        "yuv", "yuva", "nv", "rgb", "bgr", "rgba", "bgra", "argb", "abgr", "gbrp", "gray", "ya",
        "uyvy", "yuyv", "pal8", "monob", "monow",
    ]
    .iter()
    .any(|prefix| pixel_format.starts_with(prefix))
    .then_some(8)
}

fn stream_bit_depth(stream: &FFProbeStream) -> Option<u8> {
    stream
        .bits_per_raw_sample
        .as_deref()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|depth| *depth > 0)
        .or_else(|| stream.bits_per_sample.filter(|depth| *depth > 0))
        .or_else(|| pixel_format_bit_depth(stream.pix_fmt.as_deref()))
}

fn decoded_video_bytes_per_pixel(pixel_format: Option<&str>, bit_depth: Option<u8>) -> Option<f64> {
    let pixel_format = pixel_format?.trim().to_ascii_lowercase();
    if pixel_format.is_empty() {
        return None;
    }
    if pixel_format.starts_with("gbrapf32") {
        return Some(16.0);
    }
    if pixel_format.starts_with("gbrpf32") {
        return Some(12.0);
    }
    if pixel_format.starts_with("gbrapf16") {
        return Some(8.0);
    }
    if pixel_format.starts_with("gbrpf16") {
        return Some(6.0);
    }
    let storage_bytes = if bit_depth.unwrap_or(8) > 8 { 2.0 } else { 1.0 };
    if pixel_format.starts_with("yuva420") {
        return Some(2.5 * storage_bytes);
    }
    if pixel_format.starts_with("yuva422") {
        return Some(3.0 * storage_bytes);
    }
    if pixel_format.starts_with("yuva444") {
        return Some(4.0 * storage_bytes);
    }
    if pixel_format.starts_with("yuv420")
        || pixel_format.starts_with("nv12")
        || pixel_format.starts_with("nv21")
        || pixel_format.starts_with("p010")
        || pixel_format.starts_with("p012")
        || pixel_format.starts_with("p016")
    {
        return Some(1.5 * storage_bytes);
    }
    if pixel_format.starts_with("yuv422")
        || pixel_format.starts_with("nv16")
        || pixel_format.starts_with("nv20")
    {
        return Some(2.0 * storage_bytes);
    }
    if pixel_format.starts_with("yuv444") || pixel_format.starts_with("gbrp") {
        return Some(3.0 * storage_bytes);
    }
    if pixel_format.starts_with("gray") || pixel_format.starts_with("y8") {
        return Some(storage_bytes);
    }
    if pixel_format.starts_with("uyvy422") || pixel_format.starts_with("yuyv422") {
        return Some(2.0);
    }
    if pixel_format.starts_with("pal8") {
        return Some(1.0);
    }
    if pixel_format.starts_with("rgb24") || pixel_format.starts_with("bgr24") {
        return Some(3.0);
    }
    if ["rgba", "bgra", "argb", "abgr"]
        .iter()
        .any(|prefix| pixel_format.starts_with(prefix))
    {
        return Some(4.0);
    }
    if pixel_format.starts_with("rgb48") || pixel_format.starts_with("bgr48") {
        return Some(6.0);
    }
    if pixel_format.starts_with("rgba64") || pixel_format.starts_with("bgra64") {
        return Some(8.0);
    }
    None
}

fn decoded_audio_bytes_per_sample(sample_format: Option<&str>) -> Option<u8> {
    let sample_format = sample_format?.trim().to_ascii_lowercase();
    let packed = sample_format.strip_suffix('p').unwrap_or(&sample_format);
    match packed {
        "u8" | "s8" => Some(1),
        "s16" => Some(2),
        "s32" | "flt" => Some(4),
        "s64" | "dbl" => Some(8),
        _ => None,
    }
}

fn stream_has_dolby_vision(stream: &FFProbeStream) -> bool {
    stream
        .codec_tag_string
        .as_deref()
        .is_some_and(|tag| matches!(tag.to_ascii_lowercase().as_str(), "dvh1" | "dvhe"))
        || stream.profile.as_deref().is_some_and(|profile| {
            let profile = profile.to_ascii_lowercase();
            profile.contains("dolby vision") || profile.contains("dovi")
        })
        || stream.side_data_list.iter().any(|side_data| {
            side_data.side_data_type.as_deref().is_some_and(|kind| {
                let kind = kind.to_ascii_lowercase();
                kind.contains("dolby vision") || kind.contains("dovi")
            })
        })
}

fn classify_dynamic_range(
    stream: &FFProbeStream,
    bit_depth: Option<u8>,
    color_range: Option<&str>,
    color_primaries: Option<&str>,
    color_transfer: Option<&str>,
    color_space: Option<&str>,
) -> DynamicRange {
    if stream_has_dolby_vision(stream) {
        return DynamicRange::DolbyVision;
    }
    if color_transfer.is_some_and(|transfer| transfer.eq_ignore_ascii_case("arib-std-b67")) {
        return DynamicRange::Hlg;
    }
    if color_transfer.is_some_and(|transfer| transfer.eq_ignore_ascii_case("smpte2084")) {
        let complete_hdr10 = bit_depth.is_some_and(|depth| depth >= 10)
            && color_range.is_some_and(|range| matches!(range, "tv" | "mpeg"))
            && color_primaries.is_some_and(|primaries| primaries == "bt2020")
            && color_space.is_some_and(|space| matches!(space, "bt2020nc" | "bt2020c"));
        return if complete_hdr10 {
            DynamicRange::Hdr10
        } else {
            DynamicRange::Unknown
        };
    }
    if color_range.is_some()
        && color_primaries.is_some()
        && color_transfer.is_some()
        && color_space.is_some()
    {
        DynamicRange::Sdr
    } else {
        DynamicRange::Unknown
    }
}

fn oriented_display_aspect_ratio(
    coded_width: u32,
    coded_height: u32,
    sample_aspect_ratio: Rational,
    rotation_deg: u32,
) -> Rational {
    let unrotated = reduced_rational(
        coded_width as u64 * sample_aspect_ratio.numerator as u64,
        coded_height as u64 * sample_aspect_ratio.denominator as u64,
    )
    .unwrap_or(Rational::SQUARE);
    if matches!(rotation_deg, 90 | 270) {
        Rational {
            numerator: unrotated.denominator,
            denominator: unrotated.numerator,
        }
    } else {
        unrotated
    }
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
    let ffmpeg_bin = default_ffmpeg();
    let pixel_formats = cached_ffmpeg_pixel_formats(&ffmpeg_bin)?;
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
    parse_probe_output_with_pixel_formats(&stdout, Some(&pixel_formats))
}

#[cfg(test)]
fn parse_probe_output(stdout: &str) -> Result<VideoProbe, String> {
    parse_probe_output_with_pixel_formats(stdout, None)
}

fn parse_probe_output_with_pixel_formats(
    stdout: &str,
    pixel_formats: Option<&HashMap<String, PixelFormatDescriptor>>,
) -> Result<VideoProbe, String> {
    let parsed: FFProbeOutput =
        serde_json::from_str(stdout).map_err(|e| format!("ffprobe returned invalid JSON: {e}"))?;

    let selected_video = select_primary_video_stream(&parsed.streams).ok_or_else(|| {
        "Could not find a usable non-attached video stream (ffprobe).".to_string()
    })?;
    let selected_audio = select_primary_audio_stream(&parsed.streams);

    let mut duration_s = parsed
        .format
        .duration
        .as_deref()
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);

    if duration_s <= 0.0 || !duration_s.is_finite() {
        duration_s = selected_video
            .duration
            .as_deref()
            .and_then(|duration| duration.parse::<f64>().ok())
            .unwrap_or(0.0);
    }

    if duration_s <= 0.0 || !duration_s.is_finite() {
        return Err("Could not determine video duration (ffprobe).".to_string());
    }

    let coded_width = selected_video
        .width
        .ok_or_else(|| "Could not determine video width (ffprobe).".to_string())?;
    let coded_height = selected_video
        .height
        .ok_or_else(|| "Could not determine video height (ffprobe).".to_string())?;
    let source_rotation = stream_rotation(selected_video);
    let rotation_deg = source_rotation.quarter_turn_deg;
    let mut width = coded_width;
    let mut height = coded_height;
    // The webview preview, ffmpeg autorotation, and cropdetect all work in
    // display space, so report display-oriented dimensions.
    if matches!(rotation_deg, 90 | 270) {
        std::mem::swap(&mut width, &mut height);
    }
    // Reverse/Loop buffer safety needs a conservative frame-rate ceiling.
    // VFR media can report a low whole-file average while a retained segment
    // reaches the much higher r_frame_rate, so use the maximum valid fact.
    let frame_rate = [
        parse_ffprobe_rate(selected_video.avg_frame_rate.as_deref()),
        parse_ffprobe_rate(selected_video.r_frame_rate.as_deref()),
    ]
    .into_iter()
    .flatten()
    .max_by(|left, right| left.total_cmp(right));
    let video_stream_index = selected_video
        .index
        .ok_or_else(|| "Selected video stream has no absolute index (ffprobe).".to_string())?;
    let audio_stream_index = selected_audio.and_then(|stream| stream.index);
    let parsed_sample_aspect_ratio =
        parse_ffprobe_rational(selected_video.sample_aspect_ratio.as_deref());
    let declared_display_aspect_ratio =
        parse_ffprobe_rational(selected_video.display_aspect_ratio.as_deref());
    let derived_sample_aspect_ratio = declared_display_aspect_ratio.and_then(|display_aspect| {
        reduced_rational(
            display_aspect.numerator as u64 * coded_height as u64,
            display_aspect.denominator as u64 * coded_width as u64,
        )
    });
    let sample_aspect_ratio = parsed_sample_aspect_ratio
        .or(derived_sample_aspect_ratio)
        .unwrap_or(Rational::SQUARE);
    let display_aspect_ratio =
        oriented_display_aspect_ratio(coded_width, coded_height, sample_aspect_ratio, rotation_deg);
    let pixel_format = normalized_probe_field(selected_video.pix_fmt.as_deref());
    let pixel_descriptor = pixel_format
        .as_deref()
        .and_then(|name| pixel_formats.and_then(|formats| formats.get(name)));
    let declared_bit_depth = selected_video
        .bits_per_raw_sample
        .as_deref()
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|depth| *depth > 0)
        .or_else(|| selected_video.bits_per_sample.filter(|depth| *depth > 0));
    let descriptor_bit_depth = pixel_descriptor.map(|descriptor| descriptor.bit_depth);
    let bit_depth = match (declared_bit_depth, descriptor_bit_depth) {
        (Some(declared), Some(described)) => Some(declared.max(described)),
        (Some(declared), None) => Some(declared),
        (None, Some(described)) => Some(described),
        (None, None) if pixel_formats.is_none() => stream_bit_depth(selected_video),
        (None, None) => None,
    };
    let color_range = normalized_probe_field(selected_video.color_range.as_deref());
    let color_primaries = normalized_probe_field(selected_video.color_primaries.as_deref());
    let color_transfer = normalized_probe_field(selected_video.color_transfer.as_deref());
    let color_space = normalized_probe_field(selected_video.color_space.as_deref());
    let dynamic_range = classify_dynamic_range(
        selected_video,
        bit_depth,
        color_range.as_deref(),
        color_primaries.as_deref(),
        color_transfer.as_deref(),
        color_space.as_deref(),
    );
    let decoded_video_bytes_per_pixel = pixel_descriptor
        .map(|descriptor| descriptor.decoded_bytes_per_pixel)
        .or_else(|| {
            pixel_formats
                .is_none()
                .then(|| decoded_video_bytes_per_pixel(pixel_format.as_deref(), bit_depth))
                .flatten()
        });
    let audio_sample_rate = selected_audio
        .and_then(|stream| stream.sample_rate.as_deref())
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|value| *value > 0);
    let audio_channels = selected_audio
        .and_then(|stream| stream.channels)
        .filter(|value| *value > 0);
    let audio_sample_format =
        selected_audio.and_then(|stream| normalized_probe_field(stream.sample_fmt.as_deref()));
    let decoded_audio_bytes_per_sample =
        decoded_audio_bytes_per_sample(audio_sample_format.as_deref());
    let attached_picture_count = parsed
        .streams
        .iter()
        .filter(|stream| {
            stream.codec_type.as_deref() == Some("video") && stream.disposition.attached_pic != 0
        })
        .count()
        .min(u32::MAX as usize) as u32;

    Ok(VideoProbe {
        duration_s,
        width,
        height,
        coded_width,
        coded_height,
        rotation_deg,
        unsupported_rotation_deg: source_rotation.unsupported_deg,
        frame_rate,
        has_audio: selected_audio.is_some(),
        source_format: parsed.format.format_name,
        video_stream_index,
        video_codec: normalized_codec_name(selected_video.codec_name.as_deref()),
        video_is_default: selected_video.disposition.default != 0,
        audio_stream_index,
        audio_codec: selected_audio
            .and_then(|stream| normalized_codec_name(stream.codec_name.as_deref())),
        audio_is_default: selected_audio.is_some_and(|stream| stream.disposition.default != 0),
        pixel_format,
        bit_depth,
        color_range,
        color_primaries,
        color_transfer,
        color_space,
        dynamic_range,
        sample_aspect_ratio,
        display_aspect_ratio,
        attached_picture_count,
        selected_video_dispositions: stream_dispositions(&selected_video.disposition),
        audio_sample_rate,
        audio_channels,
        audio_sample_format,
        decoded_video_bytes_per_pixel,
        decoded_audio_bytes_per_sample,
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

        // Encoded width/height stay even, but exact=1 preserves an odd source
        // origin without shifting the detected content.
        let w = (crop.width / 2) * 2;
        let h = (crop.height / 2) * 2;
        let x = crop.x;
        let y = crop.y;

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
    require_supported_source_rotation(&probe)?;
    let ffmpeg_bin = default_ffmpeg();
    let runtime_capabilities = cached_ffmpeg_capabilities(&ffmpeg_bin)?;
    let capability_contract = ffmpeg_capability_contract()?;
    require_ffmpeg_capability_subset(
        "crop detection",
        std::iter::empty::<&str>(),
        ["cropdetect"],
        &capability_contract,
        &runtime_capabilities,
    )?;

    let mut cmd = command_no_window(&ffmpeg_bin);
    cmd.args(crop_detect_command_args(
        &input_path,
        probe.video_stream_index,
    ));
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

fn crop_detect_command_args(input_path: &Path, video_stream_index: u32) -> Vec<OsString> {
    [
        OsString::from("-hide_banner"),
        OsString::from("-loglevel"),
        OsString::from("info"),
        OsString::from("-nostdin"),
        OsString::from("-i"),
        input_path.as_os_str().to_os_string(),
        OsString::from("-map"),
        OsString::from(format!("0:{video_stream_index}")),
        OsString::from("-an"),
        OsString::from("-sn"),
        OsString::from("-vf"),
        OsString::from("cropdetect=24:2:0"),
        OsString::from("-frames:v"),
        OsString::from("200"),
        OsString::from("-f"),
        OsString::from("null"),
        OsString::from(null_sink()),
    ]
    .into_iter()
    .collect()
}

fn estimate_output_duration_s(
    probe_duration_s: f64,
    trim: &Option<Trim>,
    speed: f64,
    loop_video: bool,
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

    // The boomerang loop plays the clip forward then in reverse, so the output
    // is twice as long. Size planning and the progress bar both depend on this.
    let base = duration / speed;
    Ok(if loop_video { base * 2.0 } else { base })
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

    let target_size_bytes = target_bytes_from_size_limit_mb(target_size_mb)
        .ok_or_else(|| "Size limit is too large to report in exact bytes.".to_string())?;
    let target_bits = (target_size_bytes as f64) * 8.0 * margin;
    let total_kbps_budget = (target_bits / duration_s / 1000.0).floor().max(10.0) as u32;

    if !include_audio {
        return Ok(EncodePlan {
            video_bitrate_kbps: total_kbps_budget.max(min_video_kbps),
            audio_bitrate_kbps: 0,
            include_audio: false,
        });
    }

    // Audio presence is a user choice. A tight target may force both streams
    // to their practical floors and therefore produce a measured miss, but it
    // must never silently turn an audio-enabled request into video-only output.
    let audio_kbps_max = total_kbps_budget.saturating_sub(min_video_kbps);
    let audio_kbps = if audio_kbps_max >= min_audio_kbps {
        preferred_audio_kbps.max(min_audio_kbps).min(audio_kbps_max)
    } else {
        min_audio_kbps
    };
    let video_kbps = total_kbps_budget
        .saturating_sub(audio_kbps)
        .max(min_video_kbps);

    Ok(EncodePlan {
        video_bitrate_kbps: video_kbps,
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

    let target_size_bytes = target_bytes_from_size_limit_mb(target_size_mb)
        .ok_or_else(|| "Size limit is too large to report in exact bytes.".to_string())?;
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

#[cfg(test)]
fn build_video_filters(req: &EncodeRequest, probe: &VideoProbe) -> Result<Option<String>, String> {
    let selected_encoder = match req.format {
        OutputFormat::Mp4 => Some(VideoCodec::LibX264),
        OutputFormat::Webm => Some(VideoCodec::LibVpxVp9),
        OutputFormat::Mp3 => None,
    };
    let media_policy = resolve_media_policy(req, probe, selected_encoder)?;
    build_video_filters_with_policy(req, probe, media_policy)
}

fn build_video_filters_with_policy(
    req: &EncodeRequest,
    probe: &VideoProbe,
    media_policy: MediaPolicyPlan,
) -> Result<Option<String>, String> {
    let mut filters: Vec<String> = Vec::new();
    let mut deferred_trim_filters: Vec<String> = Vec::new();
    let subtitle_active = req.subtitle_path.is_some();

    if let Some(t) = &req.trim {
        let start = t.start_s.max(0.0);
        let end = t.end_s.unwrap_or(probe.duration_s).min(probe.duration_s);
        if end <= start {
            return Err("Trim end must be greater than start.".to_string());
        }
        let trim_filters = [
            format!("trim=start={start}:end={end}"),
            "setpts=PTS-STARTPTS".to_string(),
        ];
        if subtitle_active {
            deferred_trim_filters.extend(trim_filters);
        } else {
            filters.extend(trim_filters);
        }
    }

    if let Some(c) = &req.crop {
        validate_crop(c, probe)?;

        // Help H.264/YUV420 compatibility by snapping to even dimensions.
        let w = (c.width / 2) * 2;
        let h = (c.height / 2) * 2;
        let x = c.x;
        let y = c.y;

        filters.push(format!("crop=w={w}:h={h}:x={x}:y={y}:exact=1"));
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
    match media_policy.sar_action {
        SarAction::Normalize { width, height } => {
            filters.push(custom_scale_filter(width, height));
            filters.push("setsar=1".to_string());
        }
        SarAction::Unchanged | SarAction::NotApplicable => match resize_plan {
            ResizePlan::Source => {}
            ResizePlan::MaxEdge(max_edge_px) => filters.push(max_edge_scale_filter(max_edge_px)),
            ResizePlan::Custom {
                width_px,
                height_px,
            } => filters.push(custom_scale_filter(width_px, height_px)),
        },
    }

    // Crop and the scale filters already guarantee even output; an untouched
    // odd-dimension source would otherwise fail yuv420p encoders.
    if req.crop.is_none()
        && resize_plan == ResizePlan::Source
        && !media_policy.sar_action.normalizes()
        && (probe.width % 2 == 1 || probe.height % 2 == 1)
    {
        filters.push("crop=w=trunc(iw/2)*2:h=trunc(ih/2)*2".to_string());
    }

    match media_policy.color_action {
        MediaColorAction::Hdr10ToStandardSdr => filters.push(
            "zscale=t=linear:npl=100,format=gbrpf32le,tonemap=tonemap=mobius:desat=0,zscale=p=bt709:t=bt709:m=bt709:r=tv,format=yuv420p"
                .to_string(),
        ),
        MediaColorAction::HighBitDepthSdrToStandardSdr => filters.push(
            "zscale=p=bt709:t=bt709:m=bt709:r=tv:dither=error_diffusion,format=yuv420p"
                .to_string(),
        ),
        MediaColorAction::Unchanged | MediaColorAction::NotApplicable => {}
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

    if let Some(subtitle_path) = req.subtitle_path.as_deref() {
        if subtitle_path != EXTERNAL_SUBTITLE_FILE_NAME {
            return Err("Internal subtitle staging path was not normalized.".to_string());
        }
        filters.push(
            "subtitles=filename=vfl_external.srt:fontsdir=fonts:force_style='FontName=DejaVu Sans,FontSize=24,PrimaryColour=&H00FFFFFF,OutlineColour=&H00000000,BorderStyle=1,Outline=2,Shadow=0,Alignment=2,MarginL=24,MarginR=24,MarginV=24'"
                .to_string(),
        );
        filters.append(&mut deferred_trim_filters);
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

    // Must be LAST in the linear chain: reverse/trim/fps reorder frames, and
    // enable='eq(n,0)' targets the first frame entering this filter, so this
    // lands on the true first output frame (the boomerang wrap below keeps the
    // forward segment first, so the first output frame is still this one).
    if let Some(perturb) = first_frame_perturb_filter(req) {
        filters.push(perturb);
    }

    let linear = if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    };
    Ok(apply_boomerang_video(linear, req))
}

/// Wraps a linear video chain so the clip plays forward then in reverse (a
/// seamless boomerang loop), via split/reverse/concat. A simple filtergraph
/// (-vf) accepts this internal split/concat as long as it stays 1-in-1-out
/// (verified). Returns the chain unchanged when loop is off. NOTE: the reverse
/// filter buffers every frame of the clip in memory, so this targets short
/// clips. The audio chain is boomeranged symmetrically in build_audio_filters,
/// so the two stay in sync without a filter_complex.
fn apply_boomerang_video(chain: Option<String>, req: &EncodeRequest) -> Option<String> {
    if !req.loop_video {
        return chain;
    }
    let prefix = chain.map(|c| format!("{c},")).unwrap_or_default();
    Some(format!(
        "{prefix}split[fv][rv0];[rv0]reverse[rv];[fv][rv]concat=n=2:v=1"
    ))
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
        filters.push(format!("asetnsamples=n={REVERSE_AUDIO_FRAME_SAMPLES}:p=0"));
        filters.push("areverse".to_string());
    }

    if (req.speed - 1.0).abs() > 1e-9 {
        filters.push(atempo_chain(req.speed)?);
    }

    let linear = if filters.is_empty() {
        None
    } else {
        Some(filters.join(","))
    };
    Ok(apply_boomerang_audio(linear, req))
}

/// Boomerangs the audio (forward then reversed) so it stays in sync with the
/// video boomerang. Only applies for loop on a video format; audio-only (mp3)
/// is left untouched because Loop is a video effect and reversed audio there is
/// not wanted. Reached only when audio is actually included.
fn apply_boomerang_audio(chain: Option<String>, req: &EncodeRequest) -> Option<String> {
    if !req.loop_video || matches!(req.format, OutputFormat::Mp3) {
        return chain;
    }
    let prefix = chain.map(|c| format!("{c},")).unwrap_or_default();
    Some(format!(
        "{prefix}asetnsamples=n={REVERSE_AUDIO_FRAME_SAMPLES}:p=0,asplit[fa][ra0];[ra0]areverse[ra];[fa][ra]concat=n=2:v=0:a=1"
    ))
}

fn null_sink() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn temp_dir_for_job(job_id: u64, attempt: u32) -> Result<TempDir, String> {
    TempFileBuilder::new()
        .prefix(&format!(
            "video_for_lazies_{}_{}_{}_",
            std::process::id(),
            job_id,
            attempt
        ))
        .tempdir()
        .map_err(|error| format!("Failed to create temporary passlog directory: {error}"))
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
    attempt_id: u64,
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
            attempt_id,
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
    attempt_id: u64,
    job_id: u64,
    ffmpeg_bin: &str,
    args: &[String],
    working_dir: Option<&Path>,
    pass: u8,
    total_passes: u8,
    duration_us: u64,
    limits: FfmpegRunLimits,
    cancel: &AtomicBool,
    child_slot: &Arc<Mutex<Option<Child>>>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Canceled.".to_string());
    }

    let mut cmd = command_no_window(ffmpeg_bin);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(working_dir) = working_dir {
        cmd.current_dir(working_dir);
    }

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
    let watchdog_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let saw_progress_output = Arc::new(AtomicBool::new(false));
    let last_output_at = Arc::new(Mutex::new(Instant::now()));
    let process_started_at = Instant::now();
    let watchdog_child_slot = child_slot.clone();
    let watchdog_done = process_done.clone();
    let watchdog_error_thread = watchdog_error.clone();
    let watchdog_saw_progress = saw_progress_output.clone();
    let watchdog_last_output = last_output_at.clone();
    let watchdog_thread = std::thread::spawn(move || {
        while !watchdog_done.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            let output_size_error = limits.output_size_limit.as_ref().and_then(
                |(output_path, max_bytes)| {
                    fs::metadata(output_path)
                        .ok()
                        .filter(|metadata| metadata.len() > *max_bytes)
                        .map(|metadata| {
                            format!(
                                "Compatible stream-copy output exceeded the bounded {} byte temporary-file limit ({} bytes written).",
                                max_bytes,
                                metadata.len()
                            )
                        })
                },
            );
            let timeout_error = if watchdog_saw_progress.load(Ordering::Relaxed) {
                let idle_for = watchdog_last_output
                    .lock()
                    .ok()
                    .map(|last_output| last_output.elapsed())
                    .unwrap_or_default();
                (idle_for >= limits.idle_timeout).then(|| {
                    format!(
                        "ffmpeg made no progress for {} seconds and was stopped.",
                        limits.idle_timeout.as_secs()
                    )
                })
            } else {
                (process_started_at.elapsed() >= limits.initial_progress_timeout).then(|| {
                    format!(
                        "ffmpeg produced no initial progress for {} seconds and was stopped.",
                        limits.initial_progress_timeout.as_secs()
                    )
                })
            };
            if let Some(error) = output_size_error.or(timeout_error) {
                if let Ok(mut slot) = watchdog_error_thread.lock() {
                    *slot = Some(error);
                }
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
        saw_progress_output.store(true, Ordering::Relaxed);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(out_time_us) = parse_out_time_us(line) {
            let now = Instant::now();
            if now.duration_since(last_emit) >= PROGRESS_EMIT_EVERY {
                last_emit = now;
                emit_progress(
                    window,
                    attempt_id,
                    job_id,
                    pass,
                    total_passes,
                    out_time_us,
                    duration_us,
                );
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

    if let Some(error) = watchdog_error.lock().ok().and_then(|slot| slot.clone()) {
        return Err(error);
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

    emit_progress(
        window,
        attempt_id,
        job_id,
        pass,
        total_passes,
        duration_us,
        duration_us,
    );
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
    validate_codec_output_dimensions(selected_video_codec, planned_width, planned_height)?;
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
    Ok(SizeLimitedEncodeContract {
        planned_width,
        planned_height,
        min_video_kbps,
        plan,
    })
}

fn target_status(actual_bytes: u64, target_bytes: u64) -> SizeTargetStatus {
    if actual_bytes <= target_bytes {
        SizeTargetStatus::Met
    } else {
        SizeTargetStatus::Missed
    }
}

fn should_replace_measured_candidate(best_bytes: Option<u64>, candidate_bytes: u64) -> bool {
    best_bytes
        .map(|best| candidate_bytes < best)
        .unwrap_or(true)
}

fn corrected_video_bitrate_kbps(
    current_video_kbps: u32,
    min_video_kbps: u32,
    actual_bytes: u64,
    target_bytes: u64,
) -> Option<u32> {
    if actual_bytes <= target_bytes || actual_bytes == 0 {
        return None;
    }
    let reduction_factor = (target_bytes as f64 / actual_bytes as f64) * 0.95;
    let corrected = ((current_video_kbps as f64) * reduction_factor).floor() as u32;
    let corrected = corrected.max(min_video_kbps);
    (corrected < current_video_kbps).then_some(corrected)
}

fn next_strict_fit_max_edge(width: u32, height: u32) -> Option<u32> {
    let current_long_edge = width.max(height);
    STRICT_FIT_MAX_EDGE_TIERS
        .iter()
        .copied()
        .find(|tier| *tier < current_long_edge)
}

fn exact_target_message(status: SizeTargetStatus, actual_bytes: u64, target_bytes: u64) -> String {
    match status {
        SizeTargetStatus::Met => {
            format!("Target met by exact output bytes: {actual_bytes} of {target_bytes} bytes.")
        }
        SizeTargetStatus::Missed => format!(
            "Target missed by {} exact bytes. The smallest measured output was published at {actual_bytes} bytes for a {target_bytes} byte target.",
            actual_bytes.saturating_sub(target_bytes)
        ),
    }
}

fn validate_strict_fit_options(
    strict_fit: bool,
    allow_audio_removal: bool,
    size_limit_enabled: bool,
    format: OutputFormat,
) -> Result<(), String> {
    if allow_audio_removal && !strict_fit {
        return Err("Audio-removal permission requires Strict Fit.".to_string());
    }
    if strict_fit && (!size_limit_enabled || format == OutputFormat::Mp3) {
        return Err("Strict Fit requires an MP4 or WebM size target.".to_string());
    }
    Ok(())
}

fn build_mutated_size_reencode_plan(
    request: &EncodeRequest,
    probe: &VideoProbe,
    codec_selection: CodecSelection,
    resolved_perturb_seed: Option<u32>,
) -> Result<EncodeCommandPlan, String> {
    let initial =
        build_encode_command_plan(request, probe, codec_selection, resolved_perturb_seed)?;
    if initial.size_contract.is_some() {
        Ok(initial)
    } else {
        build_size_reencode_fallback_plan(request, probe, &initial)
    }
}

pub fn run_encode_job(
    window: &Window,
    attempt_id: u64,
    job_id: u64,
    cancel: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    mut request: EncodeRequest,
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
    validate_strict_fit_options(
        request.strict_fit,
        request.strict_fit_allow_audio_removal,
        size_limit_enabled,
        request.format,
    )?;
    if request.format == OutputFormat::Mp3 && request.subtitle_path.is_some() {
        return Err("External subtitles require MP4 or WebM video output.".to_string());
    }
    let target_bytes = if size_limit_enabled {
        target_bytes_from_size_limit_mb(request.size_limit_mb)
            .ok_or_else(|| "Size limit is too large to report in exact bytes.".to_string())?
    } else {
        0
    };

    let output_path = validate_output_path(
        &input_path,
        &PathBuf::from(request.output_path.trim()),
        output_extension(request.format),
    )?;

    let input_path = input_path
        .canonicalize()
        .map_err(|error| format!("Failed to resolve input file: {error}"))?;
    let input_str = input_path.to_string_lossy().to_string();

    let mut ffmpeg_bin = default_ffmpeg();
    let probe = probe_video(input_path.to_string_lossy().to_string())?;
    let runtime_capabilities = cached_ffmpeg_capabilities(&ffmpeg_bin)?;
    let capability_contract = ffmpeg_capability_contract()?;
    let prepared_subtitle = if let Some(subtitle_path) = request
        .subtitle_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        require_ffmpeg_feature(
            "externalSubtitles",
            &capability_contract,
            &runtime_capabilities,
        )?;
        let prepared = prepare_external_subtitle(Path::new(subtitle_path))?;
        request.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.to_string());
        Some(prepared)
    } else {
        request.subtitle_path = None;
        None
    };
    if prepared_subtitle.is_some() {
        let binary_path = Path::new(&ffmpeg_bin);
        if binary_path.components().count() > 1 && binary_path.is_relative() {
            ffmpeg_bin = std::env::current_dir()
                .map_err(|error| format!("Could not resolve the FFmpeg working folder: {error}"))?
                .join(binary_path)
                .canonicalize()
                .map_err(|error| format!("Could not resolve the configured FFmpeg path: {error}"))?
                .to_string_lossy()
                .to_string();
        }
    }
    let subtitle_working_dir = prepared_subtitle
        .as_ref()
        .map(PreparedSubtitle::working_dir);
    let subtitle_inspection = prepared_subtitle
        .as_ref()
        .map(|prepared| prepared.inspection.clone());
    let codec_selection = select_codec_plan(
        request.format,
        &runtime_capabilities.encoder_names,
        &request.advanced,
    )?;
    let resolved_perturb_seed = request
        .perturb_first_frame
        .then(|| request.perturb_seed.unwrap_or_else(random_perturb_seed));
    let mut command_plan =
        build_encode_command_plan(&request, &probe, codec_selection, resolved_perturb_seed)?;
    require_initial_encode_plan_capabilities(
        &command_plan,
        &capability_contract,
        &runtime_capabilities,
    )?;

    let output_duration_s = estimate_output_duration_s(
        probe.duration_s,
        &request.trim,
        request.speed,
        request.loop_video && !matches!(request.format, OutputFormat::Mp3),
    )?;
    let duration_us = (output_duration_s * 1_000_000.0).max(1.0) as u64;

    if matches!(request.format, OutputFormat::Mp3) {
        let audio_encoder = command_plan
            .audio_encoder
            .ok_or_else(|| "Missing MP3 audio encoder.".to_string())?;
        let audio_stream_index = command_plan
            .audio_stream_index
            .ok_or_else(|| "Input file has no audio stream.".to_string())?;
        let mut args = base_ffmpeg_args(&input_str);
        push_absolute_map(&mut args, audio_stream_index);
        args.push("-vn".to_string());

        if let Some(af) = &command_plan.audio_filters {
            args.push("-af".to_string());
            args.push(af.clone());
        }

        args.push("-c:a".to_string());
        args.push(audio_encoder.as_ffmpeg_name().to_string());
        if let Some(channels) = command_plan.audio_channels {
            args.push("-ac".to_string());
            args.push(channels.to_string());
        }
        let selected_audio_bitrate_kbps = if size_limit_enabled {
            let audio_kbps =
                plan_audio_only_kbps(request.size_limit_mb, output_duration_s, 0.98, 32, 320)?;
            args.push("-b:a".to_string());
            args.push(format!("{audio_kbps}k"));
            Some(audio_kbps)
        } else if let Some(audio_kbps) = command_plan.audio_bitrate_kbps {
            args.push("-b:a".to_string());
            args.push(format!("{audio_kbps}k"));
            Some(audio_kbps)
        } else {
            args.push("-q:a".to_string());
            args.push("2".to_string());
            None
        };

        push_metadata_args(&mut args, &request);

        let temp_path = create_temp_output(&output_path, job_id, "mp3")?;
        args.push(temp_path.to_string_lossy().to_string());
        let temp_str = temp_path.to_string_lossy().to_string();
        let command_preview =
            ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

        run_ffmpeg_with_progress(
            window,
            attempt_id,
            job_id,
            &ffmpeg_bin,
            &args,
            subtitle_working_dir,
            1,
            1,
            duration_us,
            ffmpeg_run_limits(request.reverse),
            cancel.as_ref(),
            child_slot,
        )?;

        let out_size = fs::metadata(&temp_path)
            .map_err(|e| format!("Failed to stat output file: {e}"))?
            .len();
        let mp3_target_result = size_limit_enabled.then(|| {
            let status = target_status(out_size, target_bytes);
            TargetResult {
                status,
                target_bytes,
                actual_bytes: out_size,
                overshoot_bytes: out_size.saturating_sub(target_bytes),
                strict_fit: false,
                selected_plan_number: 1,
                plans: vec![FitPlanResult {
                    plan_number: 1,
                    label: "Requested audio encode".to_string(),
                    mutations: Vec::new(),
                    width: None,
                    height: None,
                    video_bitrate_kbps: None,
                    audio_action: Some(StreamAction::Encode),
                    audio_bitrate_kbps: selected_audio_bitrate_kbps,
                    actual_size_bytes: out_size,
                    status,
                    ffmpeg_invocations: 1,
                    selected: true,
                }],
            }
        });
        if cancel.load(Ordering::Relaxed) {
            return Err("Canceled.".to_string());
        }
        publish_output_file(temp_path, &output_path)?;

        return Ok(EncodeFinishedPayload {
            attempt_id,
            job_id,
            ok: true,
            output_path: Some(output_path.to_string_lossy().to_string()),
            output_size_bytes: Some(out_size),
            target_result: mp3_target_result.clone(),
            message: mp3_target_result
                .as_ref()
                .map(|result| exact_target_message(result.status, out_size, target_bytes)),
            diagnostics: Some(ExportDiagnostics {
                mode: command_plan.mode.label().to_string(),
                video_action: None,
                audio_action: Some(StreamAction::Encode),
                source_format: probe.source_format.clone(),
                source_video_codec: probe.video_codec.clone(),
                source_audio_codec: probe.audio_codec.clone(),
                video_codec: None,
                audio_codec: command_plan.output_audio_codec.clone(),
                video_bitrate_kbps: None,
                audio_bitrate_kbps: selected_audio_bitrate_kbps,
                requested_size_bytes: size_limit_enabled.then_some(target_bytes),
                actual_size_bytes: Some(out_size),
                passes: 1,
                attempts: 1,
                audio_removed_for_size_target: false,
                subtitle_burned_in: subtitle_inspection.is_some(),
                subtitle_cue_count: subtitle_inspection
                    .as_ref()
                    .map(|inspection| inspection.cue_count),
                copy_fallback_reason: None,
                color_action: command_plan
                    .media_policy
                    .color_action
                    .diagnostic()
                    .to_string(),
                sar_action: command_plan
                    .media_policy
                    .sar_action
                    .diagnostic(probe.sample_aspect_ratio),
                reverse_buffer_estimate_bytes: command_plan
                    .reverse_buffer_estimate
                    .map(|estimate| estimate.bytes),
                reverse_buffer_action: command_plan
                    .reverse_buffer_estimate
                    .map(|estimate| estimate.action.diagnostic().to_string()),
                failure_stage: None,
                failure_reason: None,
                command_preview,
            }),
        });
    }

    let mut size_copy_fallback_reason: Option<String> = None;
    let mut fit_plan_history: Vec<FitPlanResult> = Vec::new();
    let mut best_output: Option<SizeCandidateOutput> = None;
    if !request.strict_fit {
        for (candidate_index, candidate) in command_plan.size_copy_candidates.iter().enumerate() {
            let mut args = build_copy_candidate_args(&input_str, &request, candidate);
            let label = format!("size-copy-{}", candidate_index + 1);
            let temp_path = create_temp_output(&output_path, job_id, &label)?;
            args.push(temp_path.to_string_lossy().to_string());
            let temp_str = temp_path.to_string_lossy().to_string();
            let command_preview =
                ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

            match run_ffmpeg_with_progress(
                window,
                attempt_id,
                job_id,
                &ffmpeg_bin,
                &args,
                subtitle_working_dir,
                1,
                1,
                duration_us,
                size_copy_run_limits(target_bytes, temp_path.as_ref()),
                cancel.as_ref(),
                child_slot,
            ) {
                Ok(()) => {
                    let out_size = fs::metadata(&temp_path)
                        .map_err(|error| format!("Failed to stat output file: {error}"))?
                        .len();
                    let met_target = out_size <= target_bytes;
                    let plan_number = fit_plan_history.len() as u32 + 1;
                    fit_plan_history.push(FitPlanResult {
                        plan_number,
                        label: "Compatible stream copy".to_string(),
                        mutations: Vec::new(),
                        width: Some(probe.width),
                        height: Some(probe.height),
                        video_bitrate_kbps: None,
                        audio_action: probe.has_audio.then_some(candidate.audio_action),
                        audio_bitrate_kbps: None,
                        actual_size_bytes: out_size,
                        status: if met_target {
                            SizeTargetStatus::Met
                        } else {
                            SizeTargetStatus::Missed
                        },
                        ffmpeg_invocations: 1,
                        selected: met_target,
                    });
                    if met_target {
                        if cancel.load(Ordering::Relaxed) {
                            return Err("Canceled.".to_string());
                        }
                        let audio_removed_for_size_target = request.audio_enabled
                            && probe.has_audio
                            && candidate.audio_action == StreamAction::Drop;
                        let target_result = TargetResult {
                            status: SizeTargetStatus::Met,
                            target_bytes,
                            actual_bytes: out_size,
                            overshoot_bytes: 0,
                            strict_fit: false,
                            selected_plan_number: plan_number,
                            plans: fit_plan_history.clone(),
                        };
                        publish_output_file(temp_path, &output_path)?;
                        return Ok(EncodeFinishedPayload {
                            attempt_id,
                            job_id,
                            ok: true,
                            output_path: Some(output_path.to_string_lossy().to_string()),
                            output_size_bytes: Some(out_size),
                            target_result: Some(target_result),
                            message: Some("Fit confirmed by exact output bytes.".to_string()),
                            diagnostics: Some(ExportDiagnostics {
                                mode: EncodeMode::Remux.label().to_string(),
                                video_action: Some(StreamAction::Copy),
                                audio_action: Some(candidate.audio_action),
                                source_format: probe.source_format.clone(),
                                source_video_codec: probe.video_codec.clone(),
                                source_audio_codec: probe.audio_codec.clone(),
                                video_codec: probe.video_codec.clone(),
                                audio_codec: (candidate.audio_action == StreamAction::Copy)
                                    .then(|| probe.audio_codec.clone())
                                    .flatten(),
                                video_bitrate_kbps: None,
                                audio_bitrate_kbps: None,
                                requested_size_bytes: Some(target_bytes),
                                actual_size_bytes: Some(out_size),
                                passes: 1,
                                attempts: (candidate_index + 1) as u32,
                                audio_removed_for_size_target,
                                subtitle_burned_in: subtitle_inspection.is_some(),
                                subtitle_cue_count: subtitle_inspection
                                    .as_ref()
                                    .map(|inspection| inspection.cue_count),
                                copy_fallback_reason: size_copy_fallback_reason.clone(),
                                color_action: command_plan
                                    .media_policy
                                    .color_action
                                    .diagnostic()
                                    .to_string(),
                                sar_action: command_plan
                                    .media_policy
                                    .sar_action
                                    .diagnostic(probe.sample_aspect_ratio),
                                reverse_buffer_estimate_bytes: command_plan
                                    .reverse_buffer_estimate
                                    .map(|estimate| estimate.bytes),
                                reverse_buffer_action: command_plan
                                    .reverse_buffer_estimate
                                    .map(|estimate| estimate.action.diagnostic().to_string()),
                                failure_stage: None,
                                failure_reason: None,
                                command_preview,
                            }),
                        });
                    }
                    let copy_candidate = SizeCandidateOutput {
                        temp_output: temp_path,
                        actual_size_bytes: out_size,
                        plan_number,
                        command_plan: command_plan.clone(),
                        video_bitrate_kbps: None,
                        audio_bitrate_kbps: None,
                        audio_action: candidate.audio_action,
                        passes: 1,
                        command_preview,
                    };
                    let replace_best = should_replace_measured_candidate(
                        best_output.as_ref().map(|best| best.actual_size_bytes),
                        out_size,
                    );
                    if replace_best {
                        best_output = Some(copy_candidate);
                    }
                    let detail = if candidate.audio_action == StreamAction::Copy {
                        "Compatible A/V copy exceeded the size target."
                    } else {
                        "Compatible video-only copy exceeded the size target."
                    };
                    size_copy_fallback_reason = Some(match size_copy_fallback_reason {
                        Some(previous) => format!("{previous} {detail}"),
                        None => detail.to_string(),
                    });
                }
                Err(error) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(error);
                    }
                    let detail = if candidate.audio_action == StreamAction::Copy {
                        "Compatible A/V copy was rejected by FFmpeg."
                    } else {
                        "Compatible video-only copy was rejected by FFmpeg."
                    };
                    size_copy_fallback_reason = Some(match size_copy_fallback_reason {
                        Some(previous) => format!("{previous} {detail}"),
                        None => detail.to_string(),
                    });
                }
            }
        }
    }

    if size_limit_enabled && command_plan.size_contract.is_none() {
        command_plan = build_size_reencode_fallback_plan(&request, &probe, &command_plan)?;
        require_encode_plan_capabilities(
            &command_plan,
            &capability_contract,
            &runtime_capabilities,
        )?;
    }

    if !size_limit_enabled {
        let mut executed_plan = command_plan.clone();
        let mut copy_fallback_reason: Option<String> = None;
        let mut execution_attempts = 0u32;

        loop {
            execution_attempts += 1;
            let mut args = build_single_pass_args(&input_str, &request, &executed_plan)?;
            let temp_path = create_temp_output(
                &output_path,
                job_id,
                if execution_attempts == 1 {
                    "single"
                } else {
                    "fallback"
                },
            )?;
            args.push(temp_path.to_string_lossy().to_string());
            let temp_str = temp_path.to_string_lossy().to_string();
            let command_preview =
                ffmpeg_command_preview(&args, &[(&input_str, "<input>"), (&temp_str, "<output>")]);

            match run_ffmpeg_with_progress(
                window,
                attempt_id,
                job_id,
                &ffmpeg_bin,
                &args,
                subtitle_working_dir,
                1,
                1,
                duration_us,
                ffmpeg_run_limits(request.reverse),
                cancel.as_ref(),
                child_slot,
            ) {
                Ok(()) => {
                    let out_size = fs::metadata(&temp_path)
                        .map_err(|error| format!("Failed to stat output file: {error}"))?
                        .len();
                    if cancel.load(Ordering::Relaxed) {
                        return Err("Canceled.".to_string());
                    }
                    publish_output_file(temp_path, &output_path)?;
                    let selected_audio_bitrate_kbps =
                        (executed_plan.audio_action == StreamAction::Encode).then(|| {
                            executed_plan
                                .audio_bitrate_kbps
                                .unwrap_or(match request.format {
                                    OutputFormat::Mp4 => 192,
                                    OutputFormat::Webm => 128,
                                    OutputFormat::Mp3 => unreachable!(),
                                })
                        });
                    return Ok(EncodeFinishedPayload {
                        attempt_id,
                        job_id,
                        ok: true,
                        output_path: Some(output_path.to_string_lossy().to_string()),
                        output_size_bytes: Some(out_size),
                        target_result: None,
                        message: None,
                        diagnostics: Some(ExportDiagnostics {
                            mode: executed_plan.mode.label().to_string(),
                            video_action: Some(executed_plan.video_action),
                            audio_action: Some(executed_plan.audio_action),
                            source_format: probe.source_format.clone(),
                            source_video_codec: executed_plan.source_video_codec.clone(),
                            source_audio_codec: executed_plan.source_audio_codec.clone(),
                            video_codec: executed_plan.output_video_codec.clone(),
                            audio_codec: executed_plan.output_audio_codec.clone(),
                            video_bitrate_kbps: None,
                            audio_bitrate_kbps: selected_audio_bitrate_kbps,
                            requested_size_bytes: None,
                            actual_size_bytes: Some(out_size),
                            passes: 1,
                            attempts: execution_attempts,
                            audio_removed_for_size_target: false,
                            subtitle_burned_in: subtitle_inspection.is_some(),
                            subtitle_cue_count: subtitle_inspection
                                .as_ref()
                                .map(|inspection| inspection.cue_count),
                            copy_fallback_reason,
                            color_action: executed_plan
                                .media_policy
                                .color_action
                                .diagnostic()
                                .to_string(),
                            sar_action: executed_plan
                                .media_policy
                                .sar_action
                                .diagnostic(probe.sample_aspect_ratio),
                            reverse_buffer_estimate_bytes: executed_plan
                                .reverse_buffer_estimate
                                .map(|estimate| estimate.bytes),
                            reverse_buffer_action: executed_plan
                                .reverse_buffer_estimate
                                .map(|estimate| estimate.action.diagnostic().to_string()),
                            failure_stage: None,
                            failure_reason: None,
                            command_preview,
                        }),
                    });
                }
                Err(error) => {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(error);
                    }
                    let contains_copy = executed_plan.video_action == StreamAction::Copy
                        || executed_plan.audio_action == StreamAction::Copy;
                    if !contains_copy || execution_attempts >= 2 {
                        return Err(error);
                    }
                    let Ok(fallback_plan) = full_reencode_fallback(&executed_plan) else {
                        // Preserve the real stream-copy failure when this
                        // partial FFmpeg build has no valid encode fallback.
                        return Err(error);
                    };
                    if fallback_plan.video_action == StreamAction::Encode {
                        let codec = fallback_plan.video_encoder.ok_or_else(|| {
                            "Missing video encoder for stream-copy fallback.".to_string()
                        })?;
                        let (width, height) = estimated_output_dimensions(&request, &probe)?;
                        validate_codec_output_dimensions(codec, width, height)?;
                    }
                    require_encode_plan_capabilities(
                        &fallback_plan,
                        &capability_contract,
                        &runtime_capabilities,
                    )?;
                    executed_plan = fallback_plan;
                    copy_fallback_reason =
                        Some("Stream-copy execution was rejected by FFmpeg.".to_string());
                }
            }
        }
    }

    let mut active_request = request.clone();
    let mut active_command_plan = command_plan;
    let mut active_contract = active_command_plan
        .size_contract
        .clone()
        .ok_or_else(|| "Missing size-target encode contract.".to_string())?;
    let mut active_plan = active_contract.plan.clone();
    let mut current_label = "Requested encode".to_string();
    let mut current_mutations: Vec<String> = Vec::new();
    let mut next_strict_stage = StrictFitNextStage::BitrateCorrection;
    let mut reencode_attempt = 0u32;

    loop {
        if request.strict_fit && fit_plan_history.len() as u32 >= STRICT_FIT_MAX_PLANS {
            break;
        }
        // A newly claimed destination makes every remaining candidate
        // unpublishable. Stop before advancing the ladder or running pass 1.
        ensure_output_destination_available(&output_path)?;
        reencode_attempt += 1;
        if cancel.load(Ordering::Relaxed) {
            return Err("Canceled.".to_string());
        }

        let selected_video_codec = active_command_plan
            .video_encoder
            .ok_or_else(|| "Missing video codec selection.".to_string())?;
        let video_codec = selected_video_codec.as_ffmpeg_name();
        let planned_width = active_contract.planned_width;
        let planned_height = active_contract.planned_height;
        let min_video_kbps = active_contract.min_video_kbps;
        let video_bitrate_kbps = active_plan.video_bitrate_kbps.max(min_video_kbps);
        let encode_speed_preference = active_command_plan.encode_speed;
        let temp_dir = temp_dir_for_job(job_id, reencode_attempt)?;
        let passlog_prefix = temp_dir.path().join("ffmpeg2pass");
        let passlog_str = passlog_prefix.to_string_lossy().to_string();
        let plan_number = fit_plan_history.len() as u32 + 1;
        let (progress_pass_1, progress_pass_2, progress_total) = if request.strict_fit {
            let first = ((plan_number - 1) * 2 + 1) as u8;
            (first, first + 1, (STRICT_FIT_MAX_PLANS * 2) as u8)
        } else {
            (1, 2, 2)
        };

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
            format!("0:{}", probe.video_stream_index),
            "-c:v".to_string(),
            video_codec.to_string(),
            "-b:v".to_string(),
            format!("{video_bitrate_kbps}k"),
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
        if let Some(vf) = &active_command_plan.video_filters {
            pass1.push("-vf".to_string());
            pass1.push(vf.clone());
        }
        if active_request.format == OutputFormat::Mp4 {
            push_encoded_video_format_args(&mut pass1, active_command_plan.media_policy);
        }
        pass1.push("-f".to_string());
        pass1.push("null".to_string());
        pass1.push(null_sink().to_string());

        if let Err(error) = run_ffmpeg_with_progress(
            window,
            attempt_id,
            job_id,
            &ffmpeg_bin,
            &pass1,
            subtitle_working_dir,
            progress_pass_1,
            progress_total,
            duration_us,
            ffmpeg_run_limits(active_request.reverse),
            cancel.as_ref(),
            child_slot,
        ) {
            return Err(map_mpeg4_size_limit_error(
                active_request.format,
                selected_video_codec,
                active_request.size_limit_mb,
                planned_width,
                planned_height,
                error,
            ));
        }

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
            format!("0:{}", probe.video_stream_index),
            "-c:v".to_string(),
            video_codec.to_string(),
            "-b:v".to_string(),
            format!("{video_bitrate_kbps}k"),
            "-pass".to_string(),
            "2".to_string(),
            "-passlogfile".to_string(),
            passlog_str.clone(),
        ];
        pass2.extend(encode_speed_args_for_codec(
            selected_video_codec,
            encode_speed_preference,
        ));
        if let Some(vf) = &active_command_plan.video_filters {
            pass2.push("-vf".to_string());
            pass2.push(vf.clone());
        }

        let pass2_audio_bitrate_kbps =
            if active_plan.include_audio && probe.has_audio && active_request.audio_enabled {
                Some(
                    active_plan
                        .audio_bitrate_kbps
                        .max(STRICT_FIT_REDUCED_AUDIO_KBPS),
                )
            } else {
                None
            };
        let audio_action = if pass2_audio_bitrate_kbps.is_some() {
            let audio_stream_index = probe
                .audio_stream_index
                .ok_or_else(|| "Missing selected audio stream index.".to_string())?;
            let audio_encoder = active_command_plan.audio_encoder.ok_or_else(|| {
                "Missing audio encoder for the selected size-targeted plan.".to_string()
            })?;
            push_absolute_map(&mut pass2, audio_stream_index);
            pass2.extend(
                ["-c:a", audio_encoder.as_ffmpeg_name()]
                    .into_iter()
                    .map(str::to_string),
            );
            if let Some(channels) = active_command_plan.audio_channels {
                pass2.push("-ac".to_string());
                pass2.push(channels.to_string());
            }
            pass2.push("-b:a".to_string());
            pass2.push(format!("{}k", pass2_audio_bitrate_kbps.unwrap_or(32)));
            if let Some(af) = &active_command_plan.audio_filters {
                pass2.push("-af".to_string());
                pass2.push(af.clone());
            }
            StreamAction::Encode
        } else {
            pass2.push("-an".to_string());
            StreamAction::Drop
        };

        push_metadata_args(&mut pass2, &active_request);
        push_video_output_policy_args(
            &mut pass2,
            active_request.format,
            StreamAction::Encode,
            active_command_plan.media_policy,
        );

        let temp_output =
            create_temp_output(&output_path, job_id, &format!("pass2-{reencode_attempt}"))?;
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
            attempt_id,
            job_id,
            &ffmpeg_bin,
            &pass2,
            subtitle_working_dir,
            progress_pass_2,
            progress_total,
            duration_us,
            ffmpeg_run_limits(active_request.reverse),
            cancel.as_ref(),
            child_slot,
        ) {
            return Err(map_mpeg4_size_limit_error(
                active_request.format,
                selected_video_codec,
                active_request.size_limit_mb,
                planned_width,
                planned_height,
                error,
            ));
        }

        let out_size = fs::metadata(&temp_output)
            .map_err(|error| format!("Failed to stat output file: {error}"))?
            .len();
        drop(temp_dir);
        let status = target_status(out_size, target_bytes);
        fit_plan_history.push(FitPlanResult {
            plan_number,
            label: current_label.clone(),
            mutations: current_mutations.clone(),
            width: Some(planned_width),
            height: Some(planned_height),
            video_bitrate_kbps: Some(video_bitrate_kbps),
            audio_action: probe.has_audio.then_some(audio_action),
            audio_bitrate_kbps: pass2_audio_bitrate_kbps,
            actual_size_bytes: out_size,
            status,
            ffmpeg_invocations: 2,
            selected: false,
        });
        let candidate = SizeCandidateOutput {
            temp_output,
            actual_size_bytes: out_size,
            plan_number,
            command_plan: active_command_plan.clone(),
            video_bitrate_kbps: Some(video_bitrate_kbps),
            audio_bitrate_kbps: pass2_audio_bitrate_kbps,
            audio_action,
            passes: 2,
            command_preview,
        };
        if status == SizeTargetStatus::Met {
            best_output = Some(candidate);
            break;
        }
        let replace_best = should_replace_measured_candidate(
            best_output.as_ref().map(|best| best.actual_size_bytes),
            out_size,
        );
        if replace_best {
            best_output = Some(candidate);
        }

        if !request.strict_fit {
            if reencode_attempt >= 3 {
                break;
            }
            let Some(next_video_kbps) = corrected_video_bitrate_kbps(
                video_bitrate_kbps,
                min_video_kbps,
                out_size,
                target_bytes,
            ) else {
                break;
            };
            current_label = "Bitrate correction".to_string();
            current_mutations.push(format!(
                "Video bitrate corrected from {video_bitrate_kbps} to {next_video_kbps} kbps after exact-byte measurement."
            ));
            active_plan.video_bitrate_kbps = next_video_kbps;
            continue;
        }

        let mut prepared_next_plan = false;
        while !prepared_next_plan && next_strict_stage != StrictFitNextStage::Exhausted {
            if fit_plan_history.len() as u32 >= STRICT_FIT_MAX_PLANS {
                next_strict_stage = StrictFitNextStage::Exhausted;
                break;
            }
            match next_strict_stage {
                StrictFitNextStage::BitrateCorrection => {
                    next_strict_stage = next_strict_stage.successor();
                    let Some(next_video_kbps) = corrected_video_bitrate_kbps(
                        video_bitrate_kbps,
                        min_video_kbps,
                        out_size,
                        target_bytes,
                    ) else {
                        continue;
                    };
                    current_label = "Bitrate correction".to_string();
                    current_mutations = vec![format!(
                        "Video bitrate corrected from {video_bitrate_kbps} to {next_video_kbps} kbps after exact-byte measurement."
                    )];
                    active_plan.video_bitrate_kbps = next_video_kbps;
                    prepared_next_plan = true;
                }
                StrictFitNextStage::LowerMaxEdge => {
                    next_strict_stage = next_strict_stage.successor();
                    let Some(max_edge_px) = next_strict_fit_max_edge(planned_width, planned_height)
                    else {
                        continue;
                    };
                    let mut next_request = active_request.clone();
                    next_request.resize = Some(ResizeSettings {
                        mode: ResizeMode::MaxEdge,
                        max_edge_px: Some(max_edge_px),
                        width_px: None,
                        height_px: None,
                    });
                    next_request.max_edge_px = None;
                    let next_command_plan = build_mutated_size_reencode_plan(
                        &next_request,
                        &probe,
                        codec_selection,
                        resolved_perturb_seed,
                    )?;
                    require_encode_plan_capabilities(
                        &next_command_plan,
                        &capability_contract,
                        &runtime_capabilities,
                    )?;
                    let next_contract =
                        next_command_plan.size_contract.clone().ok_or_else(|| {
                            "Strict Fit max-edge plan has no size contract.".to_string()
                        })?;
                    if next_contract.planned_width == planned_width
                        && next_contract.planned_height == planned_height
                    {
                        continue;
                    }
                    active_request = next_request;
                    active_command_plan = next_command_plan;
                    active_plan = next_contract.plan.clone();
                    active_contract = next_contract;
                    current_label = "Lower max edge".to_string();
                    current_mutations =
                        vec![format!("Maximum output edge reduced to {max_edge_px} px.")];
                    prepared_next_plan = true;
                }
                StrictFitNextStage::AudioFallback => {
                    next_strict_stage = next_strict_stage.successor();
                    if !request.audio_enabled || !probe.has_audio || !active_request.audio_enabled {
                        continue;
                    }
                    if request.strict_fit_allow_audio_removal {
                        let mut next_request = active_request.clone();
                        next_request.audio_enabled = false;
                        let next_command_plan = build_mutated_size_reencode_plan(
                            &next_request,
                            &probe,
                            codec_selection,
                            resolved_perturb_seed,
                        )?;
                        require_encode_plan_capabilities(
                            &next_command_plan,
                            &capability_contract,
                            &runtime_capabilities,
                        )?;
                        let next_contract =
                            next_command_plan.size_contract.clone().ok_or_else(|| {
                                "Strict Fit audio-removal plan has no size contract.".to_string()
                            })?;
                        active_request = next_request;
                        active_command_plan = next_command_plan;
                        active_plan = next_contract.plan.clone();
                        active_contract = next_contract;
                        current_label = "Permitted audio removal".to_string();
                        current_mutations
                            .push("Audio removed with explicit Strict Fit permission.".to_string());
                        prepared_next_plan = true;
                    } else if active_plan.include_audio
                        && active_plan.audio_bitrate_kbps > STRICT_FIT_REDUCED_AUDIO_KBPS
                    {
                        let previous_audio_kbps = active_plan.audio_bitrate_kbps;
                        active_plan.audio_bitrate_kbps = STRICT_FIT_REDUCED_AUDIO_KBPS;
                        current_label = "Reduced audio bitrate".to_string();
                        current_mutations.push(format!(
                            "Audio bitrate reduced from {previous_audio_kbps} to {STRICT_FIT_REDUCED_AUDIO_KBPS} kbps; audio was preserved."
                        ));
                        prepared_next_plan = true;
                    }
                }
                StrictFitNextStage::Exhausted => {}
            }
        }
        if !prepared_next_plan {
            break;
        }
    }

    let selected = best_output
        .ok_or_else(|| "No size-targeted candidate completed successfully.".to_string())?;
    if cancel.load(Ordering::Relaxed) {
        return Err("Canceled.".to_string());
    }
    for plan_result in &mut fit_plan_history {
        plan_result.selected = plan_result.plan_number == selected.plan_number;
    }
    let final_status = target_status(selected.actual_size_bytes, target_bytes);
    let target_result = TargetResult {
        status: final_status,
        target_bytes,
        actual_bytes: selected.actual_size_bytes,
        overshoot_bytes: selected.actual_size_bytes.saturating_sub(target_bytes),
        strict_fit: request.strict_fit,
        selected_plan_number: selected.plan_number,
        plans: fit_plan_history,
    };
    let audio_removed_for_size_target =
        request.audio_enabled && probe.has_audio && selected.audio_action == StreamAction::Drop;
    let selected_audio_codec = selected
        .audio_bitrate_kbps
        .and(selected.command_plan.audio_encoder)
        .map(|encoder| encoder.as_codec_name().to_string());
    let message = exact_target_message(final_status, selected.actual_size_bytes, target_bytes);
    let diagnostics = ExportDiagnostics {
        mode: selected.command_plan.mode.label().to_string(),
        video_action: Some(selected.command_plan.video_action),
        audio_action: Some(selected.audio_action),
        source_format: probe.source_format.clone(),
        source_video_codec: probe.video_codec.clone(),
        source_audio_codec: probe.audio_codec.clone(),
        video_codec: selected.command_plan.output_video_codec.clone(),
        audio_codec: match selected.audio_action {
            StreamAction::Copy => probe.audio_codec.clone(),
            StreamAction::Encode => selected_audio_codec,
            StreamAction::Drop => None,
        },
        video_bitrate_kbps: selected.video_bitrate_kbps,
        audio_bitrate_kbps: selected.audio_bitrate_kbps,
        requested_size_bytes: Some(target_bytes),
        actual_size_bytes: Some(selected.actual_size_bytes),
        passes: selected.passes,
        attempts: target_result.plans.len() as u32,
        audio_removed_for_size_target,
        subtitle_burned_in: subtitle_inspection.is_some(),
        subtitle_cue_count: subtitle_inspection
            .as_ref()
            .map(|inspection| inspection.cue_count),
        copy_fallback_reason: size_copy_fallback_reason,
        color_action: selected
            .command_plan
            .media_policy
            .color_action
            .diagnostic()
            .to_string(),
        sar_action: selected
            .command_plan
            .media_policy
            .sar_action
            .diagnostic(probe.sample_aspect_ratio),
        reverse_buffer_estimate_bytes: selected
            .command_plan
            .reverse_buffer_estimate
            .map(|estimate| estimate.bytes),
        reverse_buffer_action: selected
            .command_plan
            .reverse_buffer_estimate
            .map(|estimate| estimate.action.diagnostic().to_string()),
        failure_stage: None,
        failure_reason: None,
        command_preview: selected.command_preview,
    };
    let output_size_bytes = selected.actual_size_bytes;
    publish_output_file(selected.temp_output, &output_path)?;
    Ok(EncodeFinishedPayload {
        attempt_id,
        job_id,
        ok: true,
        output_path: Some(output_path.to_string_lossy().to_string()),
        output_size_bytes: Some(output_size_bytes),
        target_result: Some(target_result),
        message: Some(message),
        diagnostics: Some(diagnostics),
    })
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
            strict_fit: false,
            strict_fit_allow_audio_removal: false,
            subtitle_path: None,
            normalize_audio: false,
            strip_metadata: false,
            color_policy: ColorPolicy::Auto,
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
            loop_video: false,
        }
    }

    #[test]
    fn failed_encode_diagnostics_are_truthful_and_path_free() {
        let mut request = base_request();
        request.input_path = "/private/source.mp4".to_string();
        request.output_path = "/private/result.mp4".to_string();
        request.title = Some("Private title".to_string());
        let reason = "File not found: /private/source.mp4";

        let diagnostics = failed_encode_diagnostics(&request, reason);
        assert_eq!(diagnostics.mode, "failed");
        assert_eq!(diagnostics.video_action, None);
        assert_eq!(diagnostics.audio_action, None);
        assert_eq!(diagnostics.failure_stage.as_deref(), Some("backend"));
        assert_eq!(diagnostics.attempts, 0);
        assert_eq!(
            diagnostics.failure_reason.as_deref(),
            Some("File not found: <input>")
        );
        assert_eq!(diagnostics.requested_size_bytes, Some(8_000_000));
        let serialized = serde_json::to_string(&diagnostics).unwrap();
        assert!(!serialized.contains("/private/"));
        assert!(!serialized.contains("Private title"));
        assert!(serialized.contains("No FFmpeg command evidence was retained"));

        let temp_path_reason = "ffmpeg failed. Output #0, mp4, to '/private/.vfl-123-1-encode-AbCd.tmp.mp4': Permission denied";
        let temp_path_diagnostics = failed_encode_diagnostics(&request, temp_path_reason);
        let temp_path_serialized = serde_json::to_string(&temp_path_diagnostics).unwrap();
        assert!(!temp_path_serialized.contains("/private/"));
        assert!(
            temp_path_diagnostics
                .failure_reason
                .as_deref()
                .is_some_and(|failure| failure.contains("<output-folder>"))
        );

        let multiline_reason =
            "ffmpeg failed (exit 1).\n\n/private/passlog-0.log: Permission denied";
        let multiline_diagnostics = failed_encode_diagnostics(&request, multiline_reason);
        assert_eq!(
            multiline_diagnostics.failure_reason.as_deref(),
            Some("ffmpeg failed (exit 1).")
        );

        request.subtitle_path = Some("/private/subtitles/café [draft].srt".to_string());
        let subtitle_diagnostics = failed_encode_diagnostics(
            &request,
            "Could not open /private/subtitles/café [draft].srt",
        );
        assert_eq!(
            subtitle_diagnostics.failure_reason.as_deref(),
            Some("Could not open <subtitle>")
        );
        assert!(!subtitle_diagnostics.subtitle_burned_in);
        assert!(
            !serde_json::to_string(&subtitle_diagnostics)
                .unwrap()
                .contains("café")
        );
    }

    #[test]
    fn srt_timestamp_parser_is_strict_and_overflow_safe() {
        assert_eq!(parse_srt_timestamp_ms("00:01:02,345"), Some(62_345));
        assert_eq!(parse_srt_timestamp_ms("12:59:59.999"), Some(46_799_999));
        assert_eq!(parse_srt_timestamp_ms("00:60:00,000"), None);
        assert_eq!(parse_srt_timestamp_ms("00:00:60,000"), None);
        assert_eq!(parse_srt_timestamp_ms("00:00:01,00"), None);
        assert_eq!(parse_srt_timestamp_ms("x:00:01,000"), None);
        assert_eq!(
            parse_srt_timestamp_ms("18446744073709551615:00:00,000"),
            None
        );
    }

    #[test]
    fn srt_validation_accepts_bom_crlf_unicode_and_optional_indices() {
        let validated = validate_external_srt_text(
            "\u{feff}1\r\n00:00:01,250 --> 00:00:02,500\r\nOlá, 世界 🌍\r\n\r\n00:01:00.000 --> 00:01:01.125\r\nSecond cue\r\n",
        )
        .unwrap();
        assert_eq!(validated.inspection.cue_count, 2);
        assert_eq!(validated.inspection.first_cue_start_s, 1.25);
        assert_eq!(validated.inspection.last_cue_end_s, 61.125);
        assert!(!validated.normalized_text.contains('\r'));
        assert!(!validated.normalized_text.starts_with('\u{feff}'));
        assert!(validated.normalized_text.ends_with('\n'));
        assert!(validated.normalized_text.contains("世界"));
    }

    #[test]
    fn srt_validation_rejects_malformed_or_unbounded_cues() {
        for (raw, expected) in [
            ("", "empty"),
            ("1\n00:00:00,000 --> 00:00:01,000\n\0\n", "NUL"),
            ("1\nnot timing\nText\n", "invalid timing"),
            ("1\n00:00:02,000 --> 00:00:01,000\nText\n", "must end after"),
            ("1\n00:00:00,000 --> 00:00:01,000\n", "no text"),
        ] {
            assert!(
                validate_external_srt_text(raw)
                    .unwrap_err()
                    .contains(expected),
                "expected {expected}"
            );
        }

        let long_line = "x".repeat(EXTERNAL_SUBTITLE_MAX_LINE_CHARS + 1);
        assert!(
            validate_external_srt_text(&format!("1\n00:00:00,000 --> 00:00:01,000\n{long_line}\n"))
                .unwrap_err()
                .contains("too long")
        );

        let too_many = (0..=EXTERNAL_SUBTITLE_MAX_CUES)
            .map(|index| format!("{}\n00:00:00,000 --> 00:00:01,000\ncue\n\n", index + 1))
            .collect::<String>();
        assert!(
            validate_external_srt_text(&too_many)
                .unwrap_err()
                .contains("more than")
        );
    }

    #[test]
    fn srt_validation_rejects_inline_styling_but_allows_literal_punctuation() {
        for text in [
            "<font color=\"#ff0000\">red</font>",
            "<b>bold</b>",
            "{\\an8}top aligned",
            "{   \\fs40}large",
        ] {
            let raw = format!("1\n00:00:00,000 --> 00:00:01,000\n{text}\n");
            assert!(
                validate_external_srt_text(&raw)
                    .unwrap_err()
                    .contains("inline styling"),
                "expected inline styling rejection for {text:?}"
            );
        }

        let validated =
            validate_external_srt_text("1\n00:00:00,000 --> 00:00:01,000\n1 < 2 and {literal}\n")
                .unwrap();
        assert_eq!(validated.inspection.cue_count, 1);
    }

    #[test]
    fn srt_validation_rejects_timing_line_position_overrides() {
        for settings in ["align:start", "X1:40 X2:600 Y1:20 Y2:50"] {
            let raw = format!("1\n00:00:00,000 --> 00:00:01,000 {settings}\nPlain subtitle\n");
            assert!(
                validate_external_srt_text(&raw)
                    .unwrap_err()
                    .contains("timing-line settings"),
                "expected fixed-position rejection for {settings:?}"
            );
        }
    }

    #[test]
    fn srt_path_validation_and_staging_are_private_and_fixed_name() {
        let root = TempDir::new().unwrap();
        let source = root.path().join("café [source] 'quote'.srt");
        fs::write(&source, "1\r\n00:00:00,000 --> 00:00:01,000\r\nHello\r\n").unwrap();
        let inspection = inspect_srt(source.to_string_lossy().to_string()).unwrap();
        assert_eq!(inspection.cue_count, 1);

        let prepared = prepare_external_subtitle(&source).unwrap();
        let working_dir = prepared.working_dir().to_path_buf();
        let staged = working_dir.join(EXTERNAL_SUBTITLE_FILE_NAME);
        let staged_font = working_dir
            .join(EXTERNAL_SUBTITLE_FONT_DIR_NAME)
            .join(EXTERNAL_SUBTITLE_FONT_FILE_NAME);
        assert!(staged.is_file());
        assert!(staged_font.is_file());
        assert_eq!(
            fs::read(&staged_font).unwrap(),
            EXTERNAL_SUBTITLE_FONT_BYTES
        );
        assert!(!staged.to_string_lossy().contains("café"));
        assert_eq!(
            fs::read_to_string(&staged).unwrap(),
            "1\n00:00:00,000 --> 00:00:01,000\nHello\n"
        );
        drop(prepared);
        assert!(!working_dir.exists());

        let invalid_utf8 = root.path().join("invalid.srt");
        fs::write(&invalid_utf8, [0xff, 0xfe]).unwrap();
        assert!(
            inspect_srt(invalid_utf8.to_string_lossy().to_string())
                .unwrap_err()
                .contains("UTF-8")
        );
        let wrong_extension = root.path().join("captions.txt");
        fs::write(&wrong_extension, "caption").unwrap();
        assert!(
            inspect_srt(wrong_extension.to_string_lossy().to_string())
                .unwrap_err()
                .contains(".srt")
        );

        let oversized = root.path().join("oversized.srt");
        fs::write(
            &oversized,
            vec![b'x'; EXTERNAL_SUBTITLE_MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(
            inspect_srt(oversized.to_string_lossy().to_string())
                .unwrap_err()
                .contains("larger than")
        );
    }

    fn probe_10s_1920x1080_audio() -> VideoProbe {
        VideoProbe {
            duration_s: 10.0,
            width: 1920,
            height: 1080,
            coded_width: 1920,
            coded_height: 1080,
            rotation_deg: 0,
            unsupported_rotation_deg: None,
            frame_rate: Some(30.0),
            has_audio: true,
            source_format: Some("mov,mp4,m4a,3gp,3g2,mj2".to_string()),
            video_stream_index: 0,
            video_codec: Some("h264".to_string()),
            video_is_default: true,
            audio_stream_index: Some(1),
            audio_codec: Some("aac".to_string()),
            audio_is_default: true,
            pixel_format: Some("yuv420p".to_string()),
            bit_depth: Some(8),
            color_range: Some("tv".to_string()),
            color_primaries: Some("bt709".to_string()),
            color_transfer: Some("bt709".to_string()),
            color_space: Some("bt709".to_string()),
            dynamic_range: DynamicRange::Sdr,
            sample_aspect_ratio: Rational::SQUARE,
            display_aspect_ratio: Rational {
                numerator: 16,
                denominator: 9,
            },
            attached_picture_count: 0,
            selected_video_dispositions: StreamDispositions {
                default: true,
                ..StreamDispositions::default()
            },
            audio_sample_rate: Some(48_000),
            audio_channels: Some(2),
            audio_sample_format: Some("fltp".to_string()),
            decoded_video_bytes_per_pixel: Some(1.5),
            decoded_audio_bytes_per_sample: Some(4),
        }
    }

    fn test_command_plan(
        request: &EncodeRequest,
        probe: &VideoProbe,
        _source_size_bytes: u64,
    ) -> EncodeCommandPlan {
        let encoders = HashSet::from([
            "libx264".to_string(),
            "mpeg4".to_string(),
            "aac".to_string(),
            "libvpx-vp9".to_string(),
            "libvpx".to_string(),
            "libopus".to_string(),
            "libvorbis".to_string(),
            "libmp3lame".to_string(),
        ]);
        let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
        build_encode_command_plan(
            request,
            probe,
            codecs,
            request.perturb_first_frame.then_some(123),
        )
        .unwrap()
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
    fn output_path_identity_matches_windows_and_unc_spellings() {
        assert_eq!(
            output_path_identity("C:\\Videos\\Clip-2.MP4"),
            output_path_identity("c:/videos/clip-2.mp4")
        );
        assert_eq!(
            output_path_identity("\\\\Server\\Share\\Clip-2.MP4"),
            output_path_identity("//server/share/clip-2.mp4")
        );
        assert_ne!(
            output_path_identity("/videos/Clip-2.mp4"),
            output_path_identity("/videos/clip-2.mp4")
        );
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
        let dest = dir.join("output.mp4");
        let temp = create_temp_output(&dest, 7, "test").unwrap();
        let temp_path = temp.to_path_buf();
        fs::write(&temp, b"new").unwrap();
        fs::write(&dest, b"old").unwrap();

        let err = publish_output_file(temp, &dest).unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(fs::read(&dest).unwrap(), b"old");
        assert!(!temp_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn candidate_boundary_stops_when_destination_becomes_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output.mp4");
        ensure_output_destination_available(&destination).unwrap();

        fs::write(&destination, b"claimed by another writer").unwrap();
        assert!(
            ensure_output_destination_available(&destination)
                .unwrap_err()
                .contains("already exists")
        );
        assert!(
            create_temp_output(&destination, 8, "next-rung")
                .unwrap_err()
                .contains("already exists")
        );
        assert_eq!(fs::read(destination).unwrap(), b"claimed by another writer");
    }

    #[test]
    fn temporary_outputs_are_unique_and_publish_without_clobbering() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output.mp4");
        let first = create_temp_output(&destination, 9, "encode").unwrap();
        let second = create_temp_output(&destination, 9, "encode").unwrap();
        assert_ne!(first.to_path_buf(), second.to_path_buf());
        assert_eq!(first.parent(), Some(dir.path()));
        assert_eq!(second.parent(), Some(dir.path()));
        assert!(first.to_string_lossy().ends_with(".tmp.mp4"));
        assert!(second.to_string_lossy().ends_with(".tmp.mp4"));

        fs::write(&first, b"encoded").unwrap();
        drop(second);
        publish_output_file(first, &destination).unwrap();
        assert_eq!(fs::read(destination).unwrap(), b"encoded");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_output_and_publication_honor_normal_creation_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("output.mp4");
        let control = dir.path().join("ordinary.mp4");
        fs::write(&control, b"control").unwrap();
        let expected_mode = fs::metadata(&control).unwrap().permissions().mode() & 0o777;

        let temp = create_temp_output(&destination, 10, "permissions").unwrap();
        assert_eq!(
            fs::metadata(&temp).unwrap().permissions().mode() & 0o777,
            expected_mode
        );
        fs::write(&temp, b"encoded").unwrap();
        publish_output_file(temp, &destination).unwrap();
        assert_eq!(
            fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
            expected_mode
        );
    }

    #[test]
    fn passlog_directories_are_process_unique_and_clean_up_on_drop() {
        let first = temp_dir_for_job(2, 1).unwrap();
        let second = temp_dir_for_job(2, 1).unwrap();
        let first_path = first.path().to_path_buf();
        let second_path = second.path().to_path_buf();
        assert_ne!(first_path, second_path);
        drop(first);
        drop(second);
        assert!(!first_path.exists());
        assert!(!second_path.exists());
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
    fn codec_output_dimension_limits_match_the_pinned_encoders() {
        for (codec, limit) in [
            (VideoCodec::LibX264, 16_384),
            (VideoCodec::Mpeg4, 8_190),
            (VideoCodec::LibVpx, 16_382),
            (VideoCodec::LibVpxVp9, 32_768),
        ] {
            validate_codec_output_dimensions(codec, limit, 2).unwrap();
            validate_codec_output_dimensions(codec, 2, limit).unwrap();
            let error = validate_codec_output_dimensions(codec, limit + 2, 2).unwrap_err();
            assert!(error.contains(codec.as_ffmpeg_name()));
            assert!(error.contains(&limit.to_string()));
        }
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
    fn advanced_overrides_affect_only_the_stream_they_change() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.size_limit_mb = 0.0;

        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Copy);

        req.advanced.video_codec = Some(VideoCodecPreference::H264);
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Copy);

        req.advanced.video_codec = None;
        req.advanced.audio_bitrate_kbps = Some(192);
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Encode);

        req.advanced.audio_bitrate_kbps = None;
        req.advanced.video_quality = Some(VideoQualityPreference::Higher);
        assert_eq!(
            test_command_plan(&req, &probe, 1_000_000).video_action,
            StreamAction::Encode
        );

        req.advanced.video_quality = None;
        req.advanced.encode_speed = Some(EncodeSpeedPreference::Faster);
        assert_eq!(
            test_command_plan(&req, &probe, 1_000_000).video_action,
            StreamAction::Encode
        );

        req.advanced.encode_speed = None;
        req.advanced.frame_rate_cap_fps = Some(24);
        assert_eq!(
            test_command_plan(&req, &probe, 1_000_000).video_action,
            StreamAction::Encode
        );

        req.advanced.frame_rate_cap_fps = Some(60);
        assert_eq!(
            test_command_plan(&req, &probe, 1_000_000).video_action,
            StreamAction::Copy
        );

        req.advanced.frame_rate_cap_fps = None;
        req.advanced.audio_channels = Some(AudioChannelPreference::Mono);
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Encode);
    }

    #[test]
    fn size_target_holds_no_size_only_quality_and_audio_bitrate_overrides() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.advanced.audio_bitrate_kbps = Some(192);
        req.advanced.video_quality = Some(VideoQualityPreference::Higher);

        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert!(!plan.size_copy_candidates.is_empty());

        req.advanced.encode_speed = Some(EncodeSpeedPreference::Smaller);
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert!(plan.size_copy_candidates.is_empty());
    }

    #[test]
    fn copy_compatibility_is_conservative_per_output_container() {
        assert!(video_copy_compatible(OutputFormat::Mp4, Some("h264")));
        assert!(video_copy_compatible(OutputFormat::Mp4, Some("mpeg4")));
        assert!(!video_copy_compatible(OutputFormat::Mp4, Some("vp9")));
        assert!(!video_copy_compatible(OutputFormat::Mp4, Some("hevc")));
        assert!(audio_copy_compatible(OutputFormat::Mp4, Some("aac")));
        assert!(!audio_copy_compatible(OutputFormat::Mp4, Some("opus")));

        assert!(video_copy_compatible(OutputFormat::Webm, Some("vp8")));
        assert!(video_copy_compatible(OutputFormat::Webm, Some("vp9")));
        assert!(!video_copy_compatible(OutputFormat::Webm, Some("h264")));
        assert!(audio_copy_compatible(OutputFormat::Webm, Some("opus")));
        assert!(audio_copy_compatible(OutputFormat::Webm, Some("vorbis")));
        assert!(!audio_copy_compatible(OutputFormat::Webm, Some("aac")));

        assert!(!video_copy_compatible(OutputFormat::Mp3, Some("h264")));
        assert!(!audio_copy_compatible(OutputFormat::Mp3, Some("mp3")));
        assert!(!video_copy_compatible(OutputFormat::Mp4, None));
        assert!(!audio_copy_compatible(OutputFormat::Webm, None));
    }

    #[test]
    fn mp4_plan_reencodes_vp9_and_opus() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        let probe = VideoProbe {
            source_format: Some("matroska,webm".to_string()),
            video_codec: Some("vp9".to_string()),
            audio_codec: Some("opus".to_string()),
            ..probe_10s_1920x1080_audio()
        };

        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.mode, EncodeMode::FullEncode);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Encode);
        assert_eq!(plan.output_video_codec.as_deref(), Some("h264"));
        assert_eq!(plan.output_audio_codec.as_deref(), Some("aac"));
    }

    #[test]
    fn audio_only_change_builds_video_copy_audio_encode_with_absolute_maps() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.normalize_audio = true;
        let probe = VideoProbe {
            video_stream_index: 3,
            audio_stream_index: Some(7),
            audio_codec: Some("opus".to_string()),
            ..probe_10s_1920x1080_audio()
        };

        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.mode, EncodeMode::VideoCopyAudioEncode);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Encode);
        let args = build_single_pass_args("in.mkv", &req, &plan).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:3"]));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:7"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
        assert!(!args.iter().any(|arg| arg == "-pix_fmt"));
    }

    #[test]
    fn video_only_change_can_preserve_compatible_audio() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.crop = Some(Crop {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        });
        let probe = VideoProbe {
            video_stream_index: 2,
            audio_stream_index: Some(5),
            video_codec: Some("vp9".to_string()),
            ..probe_10s_1920x1080_audio()
        };

        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.mode, EncodeMode::VideoEncodeAudioCopy);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Copy);
        let args = build_single_pass_args("in.mkv", &req, &plan).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:2"]));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:5"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-pix_fmt", "yuv420p"]));
    }

    #[test]
    fn timeline_change_reencodes_both_retained_streams() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.trim = Some(Trim {
            start_s: 1.0,
            end_s: Some(5.0),
        });

        let plan = test_command_plan(&req, &probe_10s_1920x1080_audio(), 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Encode);
        assert!(plan.video_reasons.contains(&PlanReason::TimelineTransform));
        assert!(plan.audio_reasons.contains(&PlanReason::TimelineTransform));
    }

    #[test]
    fn size_copy_candidate_requires_all_retained_streams_to_be_compatible() {
        let req = base_request();
        let selected_streams_with_large_unmapped_payload = VideoProbe {
            attached_picture_count: 1,
            ..probe_10s_1920x1080_audio()
        };
        // Whole-container size can be dominated by an attached picture or
        // extra unmapped streams. Always measure the selected-stream remux.
        let compatible = test_command_plan(
            &req,
            &selected_streams_with_large_unmapped_payload,
            100_000_000,
        );
        assert!(!compatible.size_copy_candidates.is_empty());

        let incompatible_audio = VideoProbe {
            audio_codec: Some("opus".to_string()),
            ..probe_10s_1920x1080_audio()
        };
        let plan = test_command_plan(&req, &incompatible_audio, 1_000_000);
        assert!(plan.size_copy_candidates.is_empty());
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Encode);
    }

    #[test]
    fn tight_size_target_preserves_requested_audio_before_bounded_video_copy() {
        let mut request = base_request();
        request.size_limit_mb = 0.1;
        let compatible_plan = test_command_plan(&request, &probe_10s_1920x1080_audio(), 12_494);
        assert_eq!(
            compatible_plan
                .size_copy_candidates
                .iter()
                .map(|candidate| candidate.audio_action)
                .collect::<Vec<_>>(),
            [StreamAction::Copy]
        );

        let probe = VideoProbe {
            audio_codec: Some("mp3".to_string()),
            ..probe_10s_1920x1080_audio()
        };

        let bundled_plan = test_command_plan(&request, &probe, 100_000);
        assert!(bundled_plan.size_copy_candidates.is_empty());
        assert_eq!(bundled_plan.video_action, StreamAction::Encode);
        assert_eq!(bundled_plan.audio_action, StreamAction::Encode);
        assert!(
            bundled_plan
                .size_contract
                .as_ref()
                .is_some_and(|contract| contract.plan.include_audio)
        );

        let no_encoders =
            select_codec_plan(request.format, &HashSet::new(), &request.advanced).unwrap();
        assert!(
            build_encode_command_plan(&request, &probe, no_encoders, None)
                .unwrap_err()
                .contains("No compatible video encoder")
        );
    }

    #[test]
    fn oversized_compatible_source_defers_fallback_bounds_until_copy_misses() {
        let mut request = base_request();
        request.format = OutputFormat::Webm;
        request.size_limit_mb = 0.1;
        request.audio_enabled = false;
        let probe = VideoProbe {
            width: 40_000,
            height: 2,
            coded_width: 40_000,
            coded_height: 2,
            source_format: Some("matroska,webm".to_string()),
            video_codec: Some("vp9".to_string()),
            has_audio: false,
            audio_stream_index: None,
            audio_codec: None,
            ..probe_10s_1920x1080_audio()
        };

        let initial = test_command_plan(&request, &probe, 1_489);
        assert_eq!(initial.video_action, StreamAction::Copy);
        assert_eq!(initial.audio_action, StreamAction::Drop);
        assert!(!initial.size_copy_candidates.is_empty());
        assert!(initial.size_contract.is_none());
        assert!(
            build_size_reencode_fallback_plan(&request, &probe, &initial)
                .unwrap_err()
                .contains("32768")
        );
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
    fn probe_uses_conservative_vfr_rate_for_reverse_buffer_planning() {
        let json = r#"{
            "format":{"duration":"11"},
            "streams":[{
                "index":0,"codec_type":"video","width":3840,"height":2160,
                "pix_fmt":"yuv420p10le","avg_frame_rate":"130/11","r_frame_rate":"120/1"
            }]
        }"#;
        let probe = parse_probe_output(json).unwrap();
        assert_eq!(probe.frame_rate, Some(120.0));

        let mut request = base_request();
        request.reverse = true;
        request.audio_enabled = false;
        request.trim = Some(Trim {
            start_s: 10.0,
            end_s: Some(11.0),
        });
        let error = reverse_buffer_estimate(
            &request,
            &probe,
            MediaPolicyPlan {
                color_action: MediaColorAction::Unchanged,
                sar_action: SarAction::Unchanged,
            },
            false,
        )
        .unwrap_err();
        assert!(error.contains("2 GiB safety limit"));
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
        let out = estimate_output_duration_s(10.0, &trim, 2.0, false).unwrap();
        assert!((out - 3.0).abs() < 1e-9);
    }

    #[test]
    fn estimate_duration_doubles_for_loop() {
        // Forward + reverse => 2x. Size planning and progress depend on this.
        let plain = estimate_output_duration_s(10.0, &None, 1.0, false).unwrap();
        let looped = estimate_output_duration_s(10.0, &None, 1.0, true).unwrap();
        assert!((plain - 10.0).abs() < 1e-9);
        assert!((looped - 20.0).abs() < 1e-9);
        // Composes with trim + speed: (8-2)/2 = 3, doubled = 6.
        let trim = Some(Trim {
            start_s: 2.0,
            end_s: Some(8.0),
        });
        let looped_trim = estimate_output_duration_s(10.0, &trim, 2.0, true).unwrap();
        assert!((looped_trim - 6.0).abs() < 1e-9);
    }

    #[test]
    fn loop_wraps_video_chain_as_boomerang() {
        let mut req = base_request();
        req.loop_video = true;
        let probe = probe_10s_1920x1080_audio();
        // With no other transforms, the whole -vf is the boomerang.
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            filters,
            "split[fv][rv0];[rv0]reverse[rv];[fv][rv]concat=n=2:v=1"
        );

        // With other transforms, the boomerang wraps the linear chain.
        req.color = Some(ColorAdjust {
            brightness: 0.1,
            contrast: 1.0,
            saturation: 1.0,
        });
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        assert!(filters.starts_with("eq=brightness"));
        assert!(filters.ends_with(",split[fv][rv0];[rv0]reverse[rv];[fv][rv]concat=n=2:v=1"));
    }

    #[test]
    fn loop_wraps_audio_chain_and_skips_mp3() {
        let mut req = base_request();
        req.loop_video = true;
        let probe = probe_10s_1920x1080_audio();
        // Audio is boomeranged symmetrically with the video for video formats.
        let af = build_audio_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            af,
            "asetnsamples=n=1024:p=0,asplit[fa][ra0];[ra0]areverse[ra];[fa][ra]concat=n=2:v=0:a=1"
        );
        req.reverse = true;
        let reverse_and_loop = build_audio_filters(&req, &probe).unwrap().unwrap();
        assert_eq!(
            reverse_and_loop.matches("asetnsamples=n=1024:p=0").count(),
            2
        );
        // Loop is a no-op for audio-only mp3 (no video to loop).
        req.reverse = false;
        req.format = OutputFormat::Mp3;
        assert_eq!(build_audio_filters(&req, &probe).unwrap(), None);
    }

    #[test]
    fn loop_and_perturb_compose_with_perturb_in_forward_segment() {
        let mut req = base_request();
        req.loop_video = true;
        req.perturb_first_frame = true;
        req.perturb_seed = Some(9);
        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        // Perturb sits in the linear chain (before split), so the forward
        // segment's first frame is the perturbed one and it stays first.
        assert!(filters.starts_with("noise=alls=3:allf=u:all_seed=9:enable='eq(n\\,0)',split[fv]"));
        assert!(filters.ends_with("concat=n=2:v=1"));
    }

    #[test]
    fn loop_disables_stream_copy_noop() {
        let mut req = base_request();
        req.loop_video = true;
        let no_op = req.trim.is_none()
            && req.crop.is_none()
            && !req.reverse
            && req.rotate_deg == 0
            && (req.speed - 1.0).abs() <= 1e-9
            && matches!(requested_resize(&req).unwrap(), ResizePlan::Source)
            && color_is_noop(&req.color)
            && !req.perturb_first_frame
            && !req.loop_video;
        assert!(!no_op);
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
            "trim=start=1:end=5,setpts=PTS-STARTPTS,crop=w=100:h=98:x=1:y=3:exact=1,transpose=1,reverse,setpts=PTS/2.000000000"
        );
    }

    #[test]
    fn subtitle_filter_uses_only_the_staged_name_and_source_timeline() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.to_string());
        req.trim = Some(Trim {
            start_s: 5.0,
            end_s: Some(9.0),
        });
        req.crop = Some(Crop {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        });
        let probe = probe_10s_1920x1080_audio();
        let filters = build_video_filters(&req, &probe).unwrap().unwrap();
        let subtitle_index = filters.find("subtitles=filename=vfl_external.srt").unwrap();
        let trim_index = filters.find("trim=start=5:end=9").unwrap();
        assert!(filters.starts_with("crop=w=1280:h=720:x=0:y=0:exact=1"));
        assert!(subtitle_index < trim_index);
        assert!(filters.contains("fontsdir=fonts"));
        assert!(filters.contains("FontName=DejaVu Sans"));
        assert!(!filters.contains("/"));
        assert!(!filters.contains("\\"));

        req.subtitle_path = Some("C:\\private\\captions [draft].srt".to_string());
        assert!(
            build_video_filters(&req, &probe)
                .unwrap_err()
                .contains("staging path")
        );
    }

    #[test]
    fn subtitle_burn_in_forces_video_encode_but_preserves_compatible_audio() {
        let mut request = base_request();
        request.size_limit_mb = 0.0;
        request.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.to_string());
        let plan = test_command_plan(&request, &probe_10s_1920x1080_audio(), 1_000_000);
        assert_eq!(plan.mode, EncodeMode::VideoEncodeAudioCopy);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Copy);
        assert!(plan.video_reasons.contains(&PlanReason::SubtitleBurnIn));
        assert!(
            plan.video_filters
                .as_deref()
                .is_some_and(|filters| filters.contains("subtitles=filename=vfl_external.srt"))
        );
    }

    #[test]
    fn crop_preserves_odd_origin_with_exact_chroma_positioning() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.crop = Some(Crop {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        });
        let mut probe = probe_10s_1920x1080_audio();
        probe.width = 3;
        probe.height = 3;
        probe.coded_width = 3;
        probe.coded_height = 3;
        assert_eq!(
            build_video_filters(&req, &probe).unwrap().as_deref(),
            Some("crop=w=2:h=2:x=1:y=1:exact=1")
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
            width: 853,
            height: 480,
            ..probe_10s_1920x1080_audio()
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
            "atrim=start=1:end=5,asetpts=PTS-STARTPTS,asetnsamples=n=1024:p=0,areverse,atempo=2.0,atempo=2.000000"
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
            audio_stream_index: None,
            audio_codec: None,
            audio_is_default: false,
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
    fn copy_candidate_args_strip_metadata_before_explicit_title() {
        let mut req = base_request();
        req.title = Some("  Beach day  ".to_string());
        req.strip_metadata = true;

        let candidate = CopyCandidatePlan {
            video_stream_index: 3,
            audio_stream_index: Some(7),
            audio_action: StreamAction::Copy,
        };
        let args = build_copy_candidate_args("in.mp4", &req, &candidate);
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:3"]));
        assert!(args.windows(2).any(|pair| pair == ["-map", "0:7"]));
        let strip_index = args
            .iter()
            .position(|arg| arg == "-map_metadata")
            .expect("stream-copy args should strip inherited metadata");
        assert_eq!(args.get(strip_index + 1).map(String::as_str), Some("-1"));

        let title_index = args
            .iter()
            .position(|arg| arg == "-metadata")
            .expect("stream-copy args should preserve an explicit title");
        assert!(strip_index < title_index);
        assert_eq!(
            args.get(title_index + 1).map(String::as_str),
            Some("title=Beach day")
        );
    }

    #[test]
    fn normalize_audio_forces_reencode() {
        let probe = probe_10s_1920x1080_audio();
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        req.normalize_audio = true;
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Encode);

        req.audio_enabled = false;
        let plan = test_command_plan(&req, &probe, 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Copy);
        assert_eq!(plan.audio_action, StreamAction::Drop);
    }

    #[test]
    fn parse_probe_output_swaps_dimensions_for_display_rotation() {
        let json = r#"{
            "format": {"duration": "12.5", "format_name": "mov,mp4,m4a,3gp,3g2,mj2"},
            "streams": [
                {
                    "index": 2,
                    "codec_name": "h264",
                    "codec_type": "video",
                    "width": 1920,
                    "height": 1080,
                    "avg_frame_rate": "30/1",
                    "side_data_list": [
                        {"side_data_type": "Display Matrix", "rotation": -90,
                         "displaymatrix": "\n00000000:           0      -65536           0\n00000001:       65536           0           0\n00000002:           0           0  1073741824"}
                    ]
                },
                {"index": 5, "codec_name": "aac", "codec_type": "audio"}
            ]
        }"#;
        let probe = parse_probe_output(json).unwrap();
        assert_eq!((probe.width, probe.height), (1080, 1920));
        assert!(probe.has_audio);
        assert_eq!(probe.video_stream_index, 2);
        assert_eq!(probe.audio_stream_index, Some(5));
        assert_eq!(probe.video_codec.as_deref(), Some("h264"));
        assert_eq!(probe.audio_codec.as_deref(), Some("aac"));
        assert!((probe.duration_s - 12.5).abs() < 1e-9);
    }

    #[test]
    fn display_matrix_is_authoritative_and_rejects_rounded_arbitrary_rotation() {
        let exact_quarter_turn = r#"{
            "format":{"duration":"1"},
            "streams":[{
                "index":0,"codec_type":"video","width":320,"height":180,
                "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
                "side_data_list":[{
                    "side_data_type":"Display Matrix","rotation":-90,
                    "displaymatrix":"\n00000000:           0      -65536           0\n00000001:       65536           0           0\n00000002:           0           0  1073741824"
                }]
            }]
        }"#;
        let exact = parse_probe_output(exact_quarter_turn).unwrap();
        assert_eq!(exact.rotation_deg, 90);
        assert_eq!(exact.unsupported_rotation_deg, None);
        assert_eq!((exact.width, exact.height), (180, 320));

        let matrix_without_rotation = exact_quarter_turn.replace(",\"rotation\":-90", "");
        let matrix_only = parse_probe_output(&matrix_without_rotation).unwrap();
        assert_eq!(matrix_only.rotation_deg, 90);

        let disagreeing_rotation =
            exact_quarter_turn.replace("\"rotation\":-90", "\"rotation\":180");
        let matrix_wins = parse_probe_output(&disagreeing_rotation).unwrap();
        assert_eq!(matrix_wins.rotation_deg, 90);

        let missing_matrix_text = r#"{
            "format":{"duration":"1"},
            "streams":[{
                "index":0,"codec_type":"video","width":320,"height":180,
                "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
                "side_data_list":[{"side_data_type":"Display Matrix","rotation":90}]
            }]
        }"#;
        let missing_matrix = parse_probe_output(missing_matrix_text).unwrap();
        assert_eq!(missing_matrix.rotation_deg, 0);
        assert_eq!(missing_matrix.unsupported_rotation_deg, Some(90.0));

        let rounded_91 = r#"{
            "format":{"duration":"1"},
            "streams":[{
                "index":0,"codec_type":"video","width":320,"height":180,
                "pix_fmt":"yuv420p","avg_frame_rate":"24/1",
                "side_data_list":[{
                    "side_data_type":"Display Matrix","rotation":90,
                    "displaymatrix":"\n00000000:       -1143      -65526           0\n00000001:       65526       -1143           0\n00000002:           0           0  1073741824"
                }]
            }]
        }"#;
        let arbitrary = parse_probe_output(rounded_91).unwrap();
        assert_eq!(arbitrary.rotation_deg, 0);
        assert_eq!((arbitrary.width, arbitrary.height), (320, 180));
        assert!((arbitrary.unsupported_rotation_deg.unwrap() - 91.0).abs() < 0.01);
        assert!(
            resolve_media_policy(&base_request(), &arbitrary, Some(VideoCodec::LibX264))
                .unwrap_err()
                .contains("supports only exact")
        );
        let mut mp3 = base_request();
        mp3.format = OutputFormat::Mp3;
        assert!(resolve_media_policy(&mp3, &arbitrary, None).is_ok());
    }

    #[test]
    fn parse_probe_output_handles_legacy_rotate_tag_and_180() {
        let rotated_tag = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"index": 0, "codec_name": "h264", "codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1", "tags": {"rotate": "90"}}
            ]
        }"#;
        let probe = parse_probe_output(rotated_tag).unwrap();
        assert_eq!((probe.width, probe.height), (480, 640));

        let upside_down = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"index": 0, "codec_name": "h264", "codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1",
                 "side_data_list": [
                    {"side_data_type": "Display Matrix", "rotation": 180,
                     "displaymatrix": "\n00000000:      -65536           0           0\n00000001:           0      -65536           0\n00000002:           0           0  1073741824"}
                 ]}
            ]
        }"#;
        let probe = parse_probe_output(upside_down).unwrap();
        assert_eq!((probe.width, probe.height), (640, 480));

        let no_rotation = r#"{
            "format": {"duration": "5"},
            "streams": [
                {"index": 0, "codec_name": "h264", "codec_type": "video", "width": 640, "height": 480,
                 "avg_frame_rate": "30/1"}
            ]
        }"#;
        let probe = parse_probe_output(no_rotation).unwrap();
        assert_eq!((probe.width, probe.height), (640, 480));
        assert!(!probe.has_audio);
    }

    #[test]
    fn parse_probe_selects_default_non_attached_video_and_default_audio() {
        let json = r#"{
            "format": {"duration": "4", "format_name": "matroska,webm"},
            "streams": [
                {"index": 0, "codec_name": "mjpeg", "codec_type": "video",
                 "width": 600, "height": 600,
                 "disposition": {"default": 1, "attached_pic": 1}},
                {"index": 3, "codec_name": "vp9", "codec_type": "video",
                 "width": 1280, "height": 720,
                 "avg_frame_rate": "24/1",
                 "disposition": {"default": 0, "attached_pic": 0}},
                {"index": 4, "codec_name": "vorbis", "codec_type": "audio",
                 "disposition": {"default": 0}},
                {"index": 7, "codec_name": "opus", "codec_type": "audio",
                 "disposition": {"default": 1}}
            ]
        }"#;

        let probe = parse_probe_output(json).unwrap();
        assert_eq!(probe.source_format.as_deref(), Some("matroska,webm"));
        assert_eq!(probe.video_stream_index, 3);
        assert_eq!(probe.video_codec.as_deref(), Some("vp9"));
        assert!(!probe.video_is_default);
        assert_eq!(probe.audio_stream_index, Some(7));
        assert_eq!(probe.audio_codec.as_deref(), Some("opus"));
        assert!(probe.audio_is_default);
        assert_eq!((probe.width, probe.height), (1280, 720));
    }

    #[test]
    fn parse_probe_rejects_attached_picture_as_only_video() {
        let json = r#"{
            "format": {"duration": "4", "format_name": "mp3"},
            "streams": [
                {"index": 0, "codec_name": "mjpeg", "codec_type": "video",
                 "width": 600, "height": 600,
                 "disposition": {"default": 1, "attached_pic": 1}},
                {"index": 1, "codec_name": "mp3", "codec_type": "audio"}
            ]
        }"#;

        let error = parse_probe_output(json).unwrap_err();
        assert!(error.contains("non-attached video stream"));
    }

    #[test]
    fn exact_target_classification_and_messages_use_integer_bytes() {
        assert_eq!(target_bytes_from_size_limit_mb(0.1), Some(100_000));
        assert_eq!(
            target_bytes_from_size_limit_mb(9_007_199_254.740_99),
            Some(9_007_199_254_740_990)
        );
        assert_eq!(target_bytes_from_size_limit_mb(9_007_199_254.740_992), None);
        assert_eq!(target_bytes_from_size_limit_mb(f64::MAX), None);
        assert_eq!(target_bytes_from_size_limit_mb(0.0), None);
        assert!(
            plan_bitrates(9_007_199_254.740_992, 1.0, false, 96, 32, 50, 0.95,)
                .unwrap_err()
                .contains("exact bytes")
        );
        assert_eq!(target_status(999_999, 1_000_000), SizeTargetStatus::Met);
        assert_eq!(target_status(1_000_000, 1_000_000), SizeTargetStatus::Met);
        assert_eq!(
            target_status(1_000_001, 1_000_000),
            SizeTargetStatus::Missed
        );
        assert_eq!(
            exact_target_message(SizeTargetStatus::Met, 1_000_000, 1_000_000),
            "Target met by exact output bytes: 1000000 of 1000000 bytes."
        );
        assert!(
            exact_target_message(SizeTargetStatus::Missed, 1_000_001, 1_000_000)
                .contains("missed by 1 exact bytes")
        );
        assert!(should_replace_measured_candidate(None, 1_200_000));
        assert!(should_replace_measured_candidate(
            Some(1_200_000),
            1_100_000
        ));
        assert!(!should_replace_measured_candidate(
            Some(1_100_000),
            1_100_000
        ));
        assert!(!should_replace_measured_candidate(
            Some(1_100_000),
            1_200_000
        ));
    }

    #[test]
    fn strict_fit_policy_has_one_bounded_non_nested_stage_sequence() {
        assert_eq!(STRICT_FIT_MAX_PLANS, 4);
        let mut stage = StrictFitNextStage::BitrateCorrection;
        let mut sequence = Vec::new();
        while stage != StrictFitNextStage::Exhausted {
            sequence.push(stage);
            stage = stage.successor();
        }
        assert_eq!(
            sequence,
            [
                StrictFitNextStage::BitrateCorrection,
                StrictFitNextStage::LowerMaxEdge,
                StrictFitNextStage::AudioFallback,
            ]
        );
        assert_eq!(stage.successor(), StrictFitNextStage::Exhausted);
    }

    #[test]
    fn strict_fit_correction_and_edge_tiers_skip_no_ops() {
        assert_eq!(
            corrected_video_bitrate_kbps(1_000, 50, 2_000_000, 1_000_000),
            Some(475)
        );
        assert_eq!(
            corrected_video_bitrate_kbps(50, 50, 2_000_000, 1_000_000),
            None
        );
        assert_eq!(
            corrected_video_bitrate_kbps(1_000, 50, 1_000_000, 1_000_000),
            None
        );
        assert_eq!(next_strict_fit_max_edge(1920, 1080), Some(1280));
        assert_eq!(next_strict_fit_max_edge(1280, 720), Some(960));
        assert_eq!(next_strict_fit_max_edge(800, 600), Some(720));
        assert_eq!(next_strict_fit_max_edge(360, 360), None);
    }

    #[test]
    fn strict_fit_flags_require_an_explicit_video_target() {
        assert!(validate_strict_fit_options(false, false, false, OutputFormat::Mp4).is_ok());
        assert!(validate_strict_fit_options(true, false, true, OutputFormat::Mp4).is_ok());
        assert!(validate_strict_fit_options(true, true, true, OutputFormat::Webm).is_ok());
        assert!(
            validate_strict_fit_options(false, true, true, OutputFormat::Mp4)
                .unwrap_err()
                .contains("requires Strict Fit")
        );
        assert!(
            validate_strict_fit_options(true, false, false, OutputFormat::Mp4)
                .unwrap_err()
                .contains("size target")
        );
        assert!(
            validate_strict_fit_options(true, false, true, OutputFormat::Mp3)
                .unwrap_err()
                .contains("MP4 or WebM")
        );
    }

    #[test]
    fn plan_bitrates_preserves_audio_when_budget_is_too_low() {
        let plan = plan_bitrates(1.0, 600.0, true, 96, 32, 50, 0.95).unwrap();
        assert!(plan.include_audio);
        assert_eq!(plan.audio_bitrate_kbps, 32);
        assert_eq!(plan.video_bitrate_kbps, 50);
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
    fn plan_bitrates_uses_stream_floors_when_remaining_budget_is_too_low() {
        let plan = plan_bitrates(0.3, 60.0, true, 96, 32, 384, 0.95).unwrap();
        assert!(plan.include_audio);
        assert_eq!(plan.audio_bitrate_kbps, 32);
        assert_eq!(plan.video_bitrate_kbps, 384);
    }

    #[test]
    fn size_limited_contract_preserves_audio_for_tight_budget() {
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
            ..probe_10s_1920x1080_audio()
        };

        let contract =
            build_size_limited_encode_contract(&req, &probe, VideoCodec::LibX264, 60.0, true)
                .unwrap();

        assert!(contract.plan.include_audio);
        assert_eq!(contract.plan.audio_bitrate_kbps, 32);
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
        assert!(contract.plan.audio_bitrate_kbps >= 32);
        assert!(contract.plan.video_bitrate_kbps >= contract.min_video_kbps);
    }

    #[test]
    fn strict_fit_mutated_plans_rebuild_geometry_and_audio_contracts() {
        let probe = probe_10s_1920x1080_audio();
        let encoders = HashSet::from(["libx264".to_string(), "aac".to_string()]);
        let mut request = base_request();
        request.strict_fit = true;
        request.resize = Some(ResizeSettings {
            mode: ResizeMode::MaxEdge,
            max_edge_px: Some(960),
            width_px: None,
            height_px: None,
        });
        let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
        let resized = build_mutated_size_reencode_plan(&request, &probe, codecs, None).unwrap();
        let resized_contract = resized.size_contract.as_ref().unwrap();
        assert_eq!(
            (
                resized_contract.planned_width,
                resized_contract.planned_height
            ),
            (960, 540)
        );
        assert!(resized_contract.plan.include_audio);
        assert_eq!(resized.audio_action, StreamAction::Encode);

        request.audio_enabled = false;
        let silent = build_mutated_size_reencode_plan(&request, &probe, codecs, None).unwrap();
        assert_eq!(silent.audio_action, StreamAction::Drop);
        assert!(!silent.size_contract.unwrap().plan.include_audio);
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
    fn source_dimension_bound_applies_after_even_encode_geometry() {
        let request = base_request();
        let at_boundary = VideoProbe {
            width: 32_769,
            height: 2,
            coded_width: 32_769,
            coded_height: 2,
            ..probe_10s_1920x1080_audio()
        };
        assert_eq!(
            estimated_output_dimensions(&request, &at_boundary).unwrap(),
            (32_768, 2)
        );

        let over_boundary = VideoProbe {
            width: 32_770,
            coded_width: 32_770,
            ..at_boundary
        };
        assert!(
            estimated_output_dimensions(&request, &over_boundary)
                .unwrap_err()
                .contains("32768")
        );
    }

    #[test]
    fn max_edge_dimension_rounding_matches_the_frontend_mirror() {
        assert_eq!(fit_max_edge_dimensions(853, 481, 720), (720, 406));
        assert_eq!(fit_max_edge_dimensions(853, 481, 900), (852, 480));
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
    fn crop_smaller_than_two_pixels_is_rejected_before_ffmpeg() {
        let mut req = base_request();
        req.crop = Some(Crop {
            x: 10,
            y: 10,
            width: 1,
            height: 20,
        });
        let probe = probe_10s_1920x1080_audio();
        let err = build_video_filters(&req, &probe).unwrap_err();
        assert!(err.contains("at least 2 pixels"));
        assert!(
            estimated_output_dimensions(&req, &probe)
                .unwrap_err()
                .contains("at least 2 pixels")
        );
    }

    #[test]
    fn sub_two_pixel_source_is_rejected_for_video_but_not_mp3() {
        let mut probe = probe_10s_1920x1080_audio();
        probe.width = 1;
        let video_error =
            resolve_media_policy(&base_request(), &probe, Some(VideoCodec::LibX264)).unwrap_err();
        assert!(video_error.contains("at least 2x2 pixels"));

        let mut mp3 = base_request();
        mp3.format = OutputFormat::Mp3;
        assert!(resolve_media_policy(&mp3, &probe, None).is_ok());
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
    fn parse_cropdetect_preserves_odd_exact_origin() {
        let probe = probe_10s_1920x1080_audio();
        let crop =
            parse_cropdetect_from_stderr("[Parsed_cropdetect_0 @ 0x000] crop=98:78:1:3\n", &probe)
                .unwrap();
        assert_eq!(crop.x, 1);
        assert_eq!(crop.y, 3);
        assert_eq!(crop.width, 98);
        assert_eq!(crop.height, 78);
    }

    #[test]
    fn parse_cropdetect_returns_none_when_absent() {
        let probe = probe_10s_1920x1080_audio();
        let stderr = "no crop here\n";
        assert!(parse_cropdetect_from_stderr(stderr, &probe).is_none());
    }

    fn hdr10_probe() -> VideoProbe {
        VideoProbe {
            pixel_format: Some("yuv420p10le".to_string()),
            bit_depth: Some(10),
            color_range: Some("tv".to_string()),
            color_primaries: Some("bt2020".to_string()),
            color_transfer: Some("smpte2084".to_string()),
            color_space: Some("bt2020nc".to_string()),
            dynamic_range: DynamicRange::Hdr10,
            decoded_video_bytes_per_pixel: Some(3.0),
            ..probe_10s_1920x1080_audio()
        }
    }

    fn ten_bit_sdr_probe() -> VideoProbe {
        VideoProbe {
            pixel_format: Some("yuv420p10le".to_string()),
            bit_depth: Some(10),
            dynamic_range: DynamicRange::Sdr,
            decoded_video_bytes_per_pixel: Some(3.0),
            ..probe_10s_1920x1080_audio()
        }
    }

    fn anamorphic_probe() -> VideoProbe {
        VideoProbe {
            width: 720,
            height: 576,
            coded_width: 720,
            coded_height: 576,
            sample_aspect_ratio: Rational {
                numerator: 16,
                denominator: 15,
            },
            display_aspect_ratio: Rational {
                numerator: 4,
                denominator: 3,
            },
            ..probe_10s_1920x1080_audio()
        }
    }

    #[test]
    fn parse_probe_exposes_hdr_sar_audio_and_disposition_facts() {
        let json = r#"{
            "format": {"duration": "12", "format_name": "matroska,webm"},
            "streams": [
                {"index": 0, "codec_name": "mjpeg", "codec_type": "video",
                 "width": 600, "height": 600,
                 "disposition": {"default": 1, "attached_pic": 1}},
                {"index": 3, "codec_name": "h264", "codec_type": "video",
                 "width": 720, "height": 576, "avg_frame_rate": "25/1",
                 "pix_fmt": "yuv420p10le", "bits_per_raw_sample": "10",
                 "color_range": "tv", "color_primaries": "bt2020",
                 "color_transfer": "smpte2084", "color_space": "bt2020nc",
                 "sample_aspect_ratio": "16:15", "display_aspect_ratio": "4:3",
                 "disposition": {"default": 0, "original": 1, "forced": 1}},
                {"index": 7, "codec_name": "aac", "codec_type": "audio",
                 "sample_rate": "48000", "channels": 2, "sample_fmt": "fltp",
                 "disposition": {"default": 1}}
            ]
        }"#;
        let probe = parse_probe_output(json).unwrap();
        assert_eq!((probe.width, probe.height), (720, 576));
        assert_eq!((probe.coded_width, probe.coded_height), (720, 576));
        assert_eq!(probe.dynamic_range, DynamicRange::Hdr10);
        assert_eq!(probe.pixel_format.as_deref(), Some("yuv420p10le"));
        assert_eq!(probe.bit_depth, Some(10));
        assert_eq!(
            probe.sample_aspect_ratio,
            Rational {
                numerator: 16,
                denominator: 15
            }
        );
        assert_eq!(
            probe.display_aspect_ratio,
            Rational {
                numerator: 4,
                denominator: 3
            }
        );
        assert_eq!(probe.attached_picture_count, 1);
        assert!(probe.selected_video_dispositions.original);
        assert!(probe.selected_video_dispositions.forced);
        assert_eq!(probe.audio_sample_rate, Some(48_000));
        assert_eq!(probe.audio_channels, Some(2));
        assert_eq!(probe.audio_sample_format.as_deref(), Some("fltp"));
        assert_eq!(probe.decoded_video_bytes_per_pixel, Some(3.0));
        assert_eq!(probe.decoded_audio_bytes_per_sample, Some(4));
    }

    #[test]
    fn probe_derives_sar_from_declared_dar_and_orients_it_for_rotation() {
        let json = r#"{
            "format": {"duration": "5"},
            "streams": [{
                "index": 0, "codec_name": "h264", "codec_type": "video",
                "width": 720, "height": 576, "display_aspect_ratio": "4:3",
                "pix_fmt": "yuv420p", "avg_frame_rate": "25/1",
                "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90,
                    "displaymatrix": "\n00000000:           0      -65536           0\n00000001:       65536           0           0\n00000002:           0           0  1073741824"}]
            }]
        }"#;
        let probe = parse_probe_output(json).unwrap();
        assert_eq!(
            probe.sample_aspect_ratio,
            Rational {
                numerator: 16,
                denominator: 15
            }
        );
        assert_eq!((probe.width, probe.height), (576, 720));
        assert_eq!(
            probe.display_aspect_ratio,
            Rational {
                numerator: 3,
                denominator: 4
            }
        );
    }

    #[test]
    fn probe_classifies_hlg_dolby_vision_and_ambiguous_pq() {
        let hlg = r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"pix_fmt":"yuv420p10le","color_transfer":"arib-std-b67"}]}"#;
        assert_eq!(
            parse_probe_output(hlg).unwrap().dynamic_range,
            DynamicRange::Hlg
        );

        let dovi = r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","codec_tag_string":"dvh1","width":16,"height":16,"pix_fmt":"yuv420p10le"}]}"#;
        assert_eq!(
            parse_probe_output(dovi).unwrap().dynamic_range,
            DynamicRange::DolbyVision
        );

        let ambiguous = r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"pix_fmt":"yuv420p10le","color_transfer":"smpte2084","color_primaries":"bt709","color_space":"bt709","color_range":"tv"}]}"#;
        assert_eq!(
            parse_probe_output(ambiguous).unwrap().dynamic_range,
            DynamicRange::Unknown
        );

        let sentinels = r#"{"format":{"duration":"1"},"streams":[{"index":0,"codec_type":"video","width":16,"height":16,"pix_fmt":"yuv420p10le","color_range":"unspecified","color_primaries":"reserved","color_transfer":"unknown","color_space":"reserved0"}]}"#;
        let sentinel_probe = parse_probe_output(sentinels).unwrap();
        assert_eq!(sentinel_probe.dynamic_range, DynamicRange::Unknown);
        assert_eq!(sentinel_probe.color_range, None);
        assert_eq!(sentinel_probe.color_primaries, None);
        assert_eq!(sentinel_probe.color_transfer, None);
        assert_eq!(sentinel_probe.color_space, None);
        let mut request = base_request();
        request.color_policy = ColorPolicy::StandardSdr;
        assert!(
            resolve_media_policy(&request, &sentinel_probe, Some(VideoCodec::LibX264))
                .unwrap_err()
                .contains("contradictory")
        );
    }

    #[test]
    fn semiplanar_pixel_formats_use_component_depth_not_layout_digits() {
        assert_eq!(pixel_format_bit_depth(Some("nv16")), Some(8));
        assert_eq!(
            decoded_video_bytes_per_pixel(Some("nv16"), Some(8)),
            Some(2.0)
        );
        assert_eq!(pixel_format_bit_depth(Some("nv20le")), Some(10));
        assert_eq!(
            decoded_video_bytes_per_pixel(Some("nv20le"), Some(10)),
            Some(4.0)
        );
    }

    #[test]
    fn ffmpeg_pixel_format_table_drives_exact_depth_and_conservative_storage() {
        let descriptors = parse_pixel_format_descriptors(
            "\
IO... yuv410p                3              9      8-8-8\n\
IO... nv20le                 3             20      10-10-10\n\
IO... yuv420p10le            3             15      10-10-10\n\
IO... grayf32le              1             32      32\n\
..... rgba128le              4            128      32-32-32-32\n\
IO... 0rgb                    3             24      8-8-8\n\
IO... rgb0                    3             24      8-8-8\n\
IO... vuyx                    3             24      8-8-8\n\
IO... xv36le                  3             36      12-12-12\n",
        );
        assert_eq!(descriptors["yuv410p"].bit_depth, 8);
        assert_eq!(descriptors["yuv410p"].decoded_bytes_per_pixel, 1.125);
        assert_eq!(descriptors["nv20le"].bit_depth, 10);
        assert_eq!(descriptors["nv20le"].decoded_bytes_per_pixel, 4.0);
        assert_eq!(descriptors["yuv420p10le"].decoded_bytes_per_pixel, 3.0);
        assert_eq!(descriptors["grayf32le"].bit_depth, 32);
        assert_eq!(descriptors["grayf32le"].decoded_bytes_per_pixel, 4.0);
        assert_eq!(descriptors["rgba128le"].bit_depth, 32);
        assert_eq!(descriptors["rgba128le"].decoded_bytes_per_pixel, 16.0);
        for padded in ["0rgb", "rgb0", "vuyx"] {
            assert_eq!(descriptors[padded].decoded_bytes_per_pixel, 3.0);
            assert!(
                descriptors[padded].decoded_bytes_per_pixel * REVERSE_BUFFER_SAFETY_FACTOR >= 4.0
            );
        }
        assert_eq!(descriptors["xv36le"].decoded_bytes_per_pixel, 6.0);
        assert!(
            descriptors["xv36le"].decoded_bytes_per_pixel * REVERSE_BUFFER_SAFETY_FACTOR >= 8.0
        );
    }

    #[test]
    fn pixel_descriptor_depth_cannot_be_downgraded_by_stream_metadata() {
        let descriptors = parse_pixel_format_descriptors(
            "IO... yuv420p10le            3             15      10-10-10\n",
        );
        let json = r#"{
            "format":{"duration":"1"},
            "streams":[{
                "index":0,"codec_type":"video","width":320,"height":180,
                "pix_fmt":"yuv420p10le","bits_per_raw_sample":"8",
                "avg_frame_rate":"24/1"
            }]
        }"#;
        let probe = parse_probe_output_with_pixel_formats(json, Some(&descriptors)).unwrap();
        assert_eq!(probe.bit_depth, Some(10));
        assert_eq!(probe.decoded_video_bytes_per_pixel, Some(3.0));
    }

    #[test]
    fn hdr10_and_high_bit_sdr_require_explicit_standard_sdr_mp4_h264() {
        let req = base_request();
        let error =
            resolve_media_policy(&req, &hdr10_probe(), Some(VideoCodec::LibX264)).unwrap_err();
        assert!(error.contains("explicit Standard SDR"));

        let mut standard = req.clone();
        standard.color_policy = ColorPolicy::StandardSdr;
        let hdr_policy =
            resolve_media_policy(&standard, &hdr10_probe(), Some(VideoCodec::LibX264)).unwrap();
        assert_eq!(
            hdr_policy.color_action,
            MediaColorAction::Hdr10ToStandardSdr
        );
        let sdr_policy =
            resolve_media_policy(&standard, &ten_bit_sdr_probe(), Some(VideoCodec::LibX264))
                .unwrap();
        assert_eq!(
            sdr_policy.color_action,
            MediaColorAction::HighBitDepthSdrToStandardSdr
        );

        standard.format = OutputFormat::Webm;
        assert!(
            resolve_media_policy(&standard, &hdr10_probe(), Some(VideoCodec::LibVpxVp9))
                .unwrap_err()
                .contains("requires MP4")
        );
        standard.format = OutputFormat::Mp4;
        assert!(
            resolve_media_policy(&standard, &hdr10_probe(), Some(VideoCodec::Mpeg4))
                .unwrap_err()
                .contains("libx264")
        );
    }

    #[test]
    fn unsupported_or_ambiguous_high_bit_color_fails_closed_but_mp3_bypasses() {
        let mut req = base_request();
        req.color_policy = ColorPolicy::StandardSdr;
        let hlg = VideoProbe {
            dynamic_range: DynamicRange::Hlg,
            bit_depth: Some(10),
            ..hdr10_probe()
        };
        assert!(
            resolve_media_policy(&req, &hlg, Some(VideoCodec::LibX264))
                .unwrap_err()
                .contains("HLG")
        );
        let dovi = VideoProbe {
            dynamic_range: DynamicRange::DolbyVision,
            ..hdr10_probe()
        };
        assert!(
            resolve_media_policy(&req, &dovi, Some(VideoCodec::LibX264))
                .unwrap_err()
                .contains("Dolby Vision")
        );
        let ambiguous = VideoProbe {
            dynamic_range: DynamicRange::Unknown,
            ..hdr10_probe()
        };
        assert!(
            resolve_media_policy(&req, &ambiguous, Some(VideoCodec::LibX264))
                .unwrap_err()
                .contains("contradictory")
        );

        req.format = OutputFormat::Mp3;
        let policy = resolve_media_policy(&req, &ambiguous, None).unwrap();
        assert_eq!(policy.color_action, MediaColorAction::NotApplicable);
    }

    #[test]
    fn frame_export_accepts_only_standard_eight_bit_sdr_color() {
        assert!(require_safe_frame_export_color(&probe_10s_1920x1080_audio()).is_ok());
        let undeclared_sdr = VideoProbe {
            dynamic_range: DynamicRange::Unknown,
            color_range: None,
            color_primaries: None,
            color_transfer: None,
            color_space: None,
            ..probe_10s_1920x1080_audio()
        };
        assert!(require_safe_frame_export_color(&undeclared_sdr).is_ok());

        for probe in [
            hdr10_probe(),
            VideoProbe {
                dynamic_range: DynamicRange::Hlg,
                ..hdr10_probe()
            },
            VideoProbe {
                dynamic_range: DynamicRange::DolbyVision,
                ..hdr10_probe()
            },
            VideoProbe {
                dynamic_range: DynamicRange::Unknown,
                ..hdr10_probe()
            },
            ten_bit_sdr_probe(),
        ] {
            assert!(
                require_safe_frame_export_color(&probe)
                    .unwrap_err()
                    .contains("standard 8-bit SDR")
            );
        }
        let unknown_depth = VideoProbe {
            bit_depth: None,
            ..probe_10s_1920x1080_audio()
        };
        assert!(
            require_safe_frame_export_color(&unknown_depth)
                .unwrap_err()
                .contains("could not be determined")
        );
    }

    #[test]
    fn frame_export_rejects_non_square_pixels_in_every_orientation() {
        let anamorphic = anamorphic_probe();
        assert!(
            require_safe_frame_export_geometry(&anamorphic)
                .unwrap_err()
                .contains("non-square-pixel")
        );
        let rotated_anamorphic = VideoProbe {
            width: 576,
            height: 720,
            rotation_deg: 90,
            ..anamorphic
        };
        assert!(
            require_safe_frame_export_geometry(&rotated_anamorphic)
                .unwrap_err()
                .contains("visible display shape")
        );
        assert!(require_safe_frame_export_geometry(&probe_10s_1920x1080_audio()).is_ok());
    }

    #[test]
    fn standard_sdr_filters_and_output_flags_are_explicit_bt709() {
        let mut req = base_request();
        req.color_policy = ColorPolicy::StandardSdr;
        let policy = resolve_media_policy(&req, &hdr10_probe(), Some(VideoCodec::LibX264)).unwrap();
        let filters = build_video_filters_with_policy(&req, &hdr10_probe(), policy)
            .unwrap()
            .unwrap();
        assert!(filters.contains("zscale=t=linear:npl=100"));
        assert!(filters.contains("tonemap=tonemap=mobius"));
        assert!(filters.contains("zscale=p=bt709:t=bt709:m=bt709:r=tv"));
        assert!(filters.contains("format=yuv420p"));

        let mut args = Vec::new();
        push_encoded_video_format_args(&mut args, policy);
        for expected in [
            ["-pix_fmt", "yuv420p"],
            ["-color_range", "tv"],
            ["-color_primaries", "bt709"],
            ["-color_trc", "bt709"],
            ["-colorspace", "bt709"],
        ] {
            assert!(args.windows(2).any(|pair| pair == expected));
        }
        assert!(args.windows(2).any(|pair| {
            pair == [
                "-x264-params",
                "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=tv",
            ]
        }));

        let mut ordinary_args = Vec::new();
        push_encoded_video_format_args(
            &mut ordinary_args,
            MediaPolicyPlan {
                color_action: MediaColorAction::Unchanged,
                sar_action: SarAction::Unchanged,
            },
        );
        assert_eq!(ordinary_args, ["-pix_fmt", "yuv420p"]);
    }

    #[test]
    fn sar_normalization_preserves_display_aspect_and_custom_is_authoritative() {
        let probe = anamorphic_probe();
        let req = base_request();
        assert_eq!(
            estimated_output_dimensions(&req, &probe).unwrap(),
            (768, 576)
        );
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        assert_eq!(
            policy.sar_action,
            SarAction::Normalize {
                width: 768,
                height: 576
            }
        );
        let filters = build_video_filters_with_policy(&req, &probe, policy)
            .unwrap()
            .unwrap();
        assert!(filters.contains("scale=w=768:h=576,setsar=1"));

        let mut custom = req.clone();
        custom.resize = Some(ResizeSettings {
            mode: ResizeMode::Custom,
            max_edge_px: None,
            width_px: Some(640),
            height_px: Some(360),
        });
        assert_eq!(
            estimated_output_dimensions(&custom, &probe).unwrap(),
            (640, 360)
        );
        let custom_policy =
            resolve_media_policy(&custom, &probe, Some(VideoCodec::LibX264)).unwrap();
        assert_eq!(
            custom_policy.sar_action,
            SarAction::Normalize {
                width: 640,
                height: 360
            }
        );
    }

    #[test]
    fn extreme_sar_is_bounded_after_resize_and_custom_remains_authoritative() {
        let probe = VideoProbe {
            width: 720,
            height: 576,
            coded_width: 720,
            coded_height: 576,
            sample_aspect_ratio: Rational {
                numerator: u32::MAX,
                denominator: 1,
            },
            ..probe_10s_1920x1080_audio()
        };
        let source = base_request();
        assert!(
            estimated_output_dimensions(&source, &probe)
                .unwrap_err()
                .contains("32768")
        );

        let mut bounded = source.clone();
        bounded.resize = Some(ResizeSettings {
            mode: ResizeMode::MaxEdge,
            max_edge_px: Some(720),
            width_px: None,
            height_px: None,
        });
        let bounded_dimensions = estimated_output_dimensions(&bounded, &probe).unwrap();
        assert!(bounded_dimensions.0 <= 720 && bounded_dimensions.1 <= 720);

        let mut custom = source;
        custom.resize = Some(ResizeSettings {
            mode: ResizeMode::Custom,
            max_edge_px: None,
            width_px: Some(640),
            height_px: Some(360),
        });
        assert_eq!(
            estimated_output_dimensions(&custom, &probe).unwrap(),
            (640, 360)
        );
    }

    #[test]
    fn sar_orientation_inverts_for_metadata_and_manual_quarter_turns() {
        let metadata_rotated = VideoProbe {
            width: 576,
            height: 720,
            rotation_deg: 90,
            ..anamorphic_probe()
        };
        let req = base_request();
        assert_eq!(
            estimated_output_dimensions(&req, &metadata_rotated).unwrap(),
            (540, 720)
        );

        let mut manually_rotated = req;
        manually_rotated.rotate_deg = 90;
        assert_eq!(
            estimated_output_dimensions(&manually_rotated, &metadata_rotated).unwrap(),
            (768, 576)
        );
    }

    #[test]
    fn sar_policy_forces_video_encode_and_discloses_reason() {
        let mut req = base_request();
        req.size_limit_mb = 0.0;
        let plan = test_command_plan(&req, &anamorphic_probe(), 1_000_000);
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert!(plan.video_reasons.contains(&PlanReason::SampleAspectRatio));
        assert!(plan.media_policy.sar_action.normalizes());
    }

    #[test]
    fn reverse_buffer_estimate_matches_reverse_stage_math_and_warns() {
        let mut req = base_request();
        req.reverse = true;
        let probe = probe_10s_1920x1080_audio();
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        let estimate = reverse_buffer_estimate(&req, &probe, policy, true)
            .unwrap()
            .unwrap();
        // Video pixels plus 8 KiB retained-frame overhead, audio samples, then
        // the shared 1.5 packed-layout/alignment safety factor.
        assert_eq!(estimate.bytes, 1_414_889_472);
        assert_eq!(estimate.action, ReverseBufferAction::Warning);
    }

    #[test]
    fn reverse_buffer_guard_thresholds_are_inclusive() {
        assert_eq!(
            guard_reverse_buffer_bytes(REVERSE_BUFFER_WARNING_BYTES - 1)
                .unwrap()
                .action,
            ReverseBufferAction::WithinLimit
        );
        assert_eq!(
            guard_reverse_buffer_bytes(REVERSE_BUFFER_WARNING_BYTES)
                .unwrap()
                .action,
            ReverseBufferAction::Warning
        );
        assert!(guard_reverse_buffer_bytes(REVERSE_BUFFER_HARD_LIMIT_BYTES - 1).is_ok());
        assert!(guard_reverse_buffer_bytes(REVERSE_BUFFER_HARD_LIMIT_BYTES).is_err());
    }

    #[test]
    fn reverse_watchdog_allows_bounded_initial_buffering_and_size_copy_is_disk_bounded() {
        let ordinary = ffmpeg_run_limits(false);
        assert_eq!(ordinary.initial_progress_timeout, ENCODE_IDLE_TIMEOUT);
        assert_eq!(ordinary.idle_timeout, ENCODE_IDLE_TIMEOUT);
        let reverse = ffmpeg_run_limits(true);
        assert_eq!(
            reverse.initial_progress_timeout,
            ENCODE_INITIAL_BUFFER_TIMEOUT
        );
        assert_eq!(reverse.idle_timeout, ENCODE_IDLE_TIMEOUT);

        let output = Path::new("candidate.tmp.mp4");
        let copy = size_copy_run_limits(1_000_000, output);
        assert_eq!(
            copy.output_size_limit,
            Some((
                output.to_path_buf(),
                1_000_000 + SIZE_COPY_MONITOR_HEADROOM_BYTES
            ))
        );
    }

    #[test]
    fn loop_buffer_estimate_uses_post_speed_active_fps_cap_and_audio_duration() {
        let mut req = base_request();
        req.loop_video = true;
        req.speed = 2.0;
        req.advanced.frame_rate_cap_fps = Some(30);
        let probe = probe_10s_1920x1080_audio();
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        let estimate = reverse_buffer_estimate(&req, &probe, policy, true)
            .unwrap()
            .unwrap();
        // video Loop stage: (10/2)*30 frames; audio Loop stage: 5 seconds.
        assert_eq!(estimate.bytes, 707_450_880);
        assert_eq!(estimate.action, ReverseBufferAction::Warning);
    }

    #[test]
    fn loop_setpts_without_active_fps_cap_keeps_source_frame_count() {
        let mut req = base_request();
        req.loop_video = true;
        req.speed = 2.0;
        let probe = probe_10s_1920x1080_audio();
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        let estimate = reverse_buffer_estimate(&req, &probe, policy, true)
            .unwrap()
            .unwrap();
        assert_eq!(estimate.bytes, 1_409_134_080);
    }

    #[test]
    fn reverse_and_loop_stage_costs_sum_and_hard_limit_rejects() {
        let mut req = base_request();
        req.reverse = true;
        req.loop_video = true;
        let error = reverse_buffer_estimate(
            &req,
            &probe_10s_1920x1080_audio(),
            MediaPolicyPlan {
                color_action: MediaColorAction::Unchanged,
                sar_action: SarAction::Unchanged,
            },
            true,
        )
        .unwrap_err();
        assert!(error.contains("2 GiB safety limit"));

        let mut small = probe_10s_1920x1080_audio();
        small.duration_s = 5.0;
        small.width = 640;
        small.height = 360;
        small.coded_width = 640;
        small.coded_height = 360;
        small.display_aspect_ratio = Rational {
            numerator: 16,
            denominator: 9,
        };
        let policy = resolve_media_policy(&req, &small, Some(VideoCodec::LibX264)).unwrap();
        let estimate = reverse_buffer_estimate(&req, &small, policy, true)
            .unwrap()
            .unwrap();
        assert_eq!(estimate.bytes, 170_741_760);
        assert_eq!(estimate.action, ReverseBufferAction::WithinLimit);
    }

    #[test]
    fn tiny_high_frame_count_video_is_bounded_by_per_frame_retention_overhead() {
        let mut request = base_request();
        request.reverse = true;
        request.audio_enabled = false;
        let probe = VideoProbe {
            duration_s: 170.0,
            width: 16,
            height: 16,
            coded_width: 16,
            coded_height: 16,
            frame_rate: Some(1_000.0),
            pixel_format: Some("pal8".to_string()),
            decoded_video_bytes_per_pixel: Some(1.0),
            has_audio: false,
            audio_stream_index: None,
            ..probe_10s_1920x1080_audio()
        };
        assert!(
            reverse_buffer_estimate(
                &request,
                &probe,
                MediaPolicyPlan {
                    color_action: MediaColorAction::Unchanged,
                    sar_action: SarAction::Unchanged,
                },
                false,
            )
            .unwrap_err()
            .contains("2 GiB safety limit")
        );
    }

    #[test]
    fn reverse_requires_known_video_rate_and_pixel_layout_only_when_enabled() {
        let mut probe = probe_10s_1920x1080_audio();
        probe.frame_rate = None;
        probe.decoded_video_bytes_per_pixel = None;
        let req = base_request();
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        assert_eq!(
            reverse_buffer_estimate(&req, &probe, policy, true).unwrap(),
            None
        );

        let mut reverse = req;
        reverse.reverse = true;
        assert!(
            reverse_buffer_estimate(&reverse, &probe, policy, true)
                .unwrap_err()
                .contains("frame rate")
        );
        probe.frame_rate = Some(30.0);
        assert!(
            reverse_buffer_estimate(&reverse, &probe, policy, true)
                .unwrap_err()
                .contains("pixel layout")
        );
    }

    #[test]
    fn normalized_audio_reverse_uses_the_post_resample_48khz_buffer() {
        let mut request = base_request();
        request.format = OutputFormat::Mp3;
        request.reverse = true;
        let mut probe = probe_10s_1920x1080_audio();
        probe.duration_s = 1_800.0;
        probe.audio_sample_rate = Some(8_000);
        probe.audio_channels = Some(2);
        probe.decoded_audio_bytes_per_sample = Some(8);
        let policy = resolve_media_policy(&request, &probe, None).unwrap();

        let source_rate = reverse_buffer_estimate(&request, &probe, policy, true)
            .unwrap()
            .unwrap();
        assert_eq!(source_rate.bytes, 518_406_144);
        assert_eq!(source_rate.action, ReverseBufferAction::WithinLimit);

        request.normalize_audio = true;
        let error = reverse_buffer_estimate(&request, &probe, policy, true).unwrap_err();
        assert!(error.contains("2 GiB safety limit"));
    }

    #[test]
    fn reverse_audio_fails_closed_on_unknown_retained_layout_but_ignores_dropped_audio() {
        let mut request = base_request();
        request.reverse = true;
        let policy = resolve_media_policy(
            &request,
            &probe_10s_1920x1080_audio(),
            Some(VideoCodec::LibX264),
        )
        .unwrap();

        for (field, probe) in [
            (
                "sample rate",
                VideoProbe {
                    audio_sample_rate: None,
                    ..probe_10s_1920x1080_audio()
                },
            ),
            (
                "channel count",
                VideoProbe {
                    audio_channels: None,
                    ..probe_10s_1920x1080_audio()
                },
            ),
            (
                "sample format",
                VideoProbe {
                    decoded_audio_bytes_per_sample: None,
                    ..probe_10s_1920x1080_audio()
                },
            ),
        ] {
            assert!(
                reverse_buffer_estimate(&request, &probe, policy, true)
                    .unwrap_err()
                    .contains(field),
                "{field}"
            );
            assert!(reverse_buffer_estimate(&request, &probe, policy, false).is_ok());
        }

        let normalized_unknown_source = VideoProbe {
            audio_sample_rate: None,
            decoded_audio_bytes_per_sample: None,
            ..probe_10s_1920x1080_audio()
        };
        request.normalize_audio = true;
        assert!(
            reverse_buffer_estimate(&request, &normalized_unknown_source, policy, true).is_ok()
        );
    }

    #[test]
    fn size_target_preserved_audio_fails_closed_on_unknown_reverse_memory() {
        let mut request = base_request();
        request.size_limit_mb = 0.3;
        request.reverse = true;
        let probe = VideoProbe {
            duration_s: 60.0,
            width: 16,
            height: 16,
            coded_width: 16,
            coded_height: 16,
            audio_sample_rate: None,
            audio_channels: None,
            decoded_audio_bytes_per_sample: None,
            ..probe_10s_1920x1080_audio()
        };
        let encoders = HashSet::from(["libx264".to_string(), "aac".to_string()]);
        let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
        let error = build_encode_command_plan(&request, &probe, codecs, None).unwrap_err();
        assert!(error.contains("retained audio sample rate"));
    }

    #[test]
    fn selected_stream_map_is_present_in_frame_and_crop_commands() {
        let frame =
            frame_extract_command_args(Path::new("input.mkv"), 1.25, 7, Path::new("frame.png"));
        let frame = frame
            .iter()
            .map(|value| value.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(frame.windows(2).any(|pair| pair == ["-map", "0:7"]));

        let crop = crop_detect_command_args(Path::new("input.mkv"), 9);
        let crop = crop
            .iter()
            .map(|value| value.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(crop.windows(2).any(|pair| pair == ["-map", "0:9"]));
    }

    #[test]
    fn capability_contract_and_inventory_parsers_are_machine_readable() {
        let contract = ffmpeg_capability_contract().unwrap();
        assert_eq!(contract.schema_version, 1);
        for name in ["coreExport", "hdrToSdr", "sarNormalize", "reverseLoop"] {
            assert!(contract.features.contains_key(name));
        }
        let filters = parse_filter_names(
            " .. scale             V->V\n TS zscale            V->V\n -- = legend\n",
        );
        assert!(filters.contains("scale"));
        assert!(filters.contains("zscale"));
        assert_eq!(
            parse_ffmpeg_version("ffmpeg version n8.1 Copyright\n"),
            Some("n8.1".to_string())
        );
    }

    #[test]
    fn capability_contract_rejects_schema_drift_and_duplicate_names() {
        let valid_features = r#"{
            "coreExport":{"releaseRequired":true,"encoders":[],"filters":[]},
            "hdrToSdr":{"releaseRequired":true,"encoders":[],"filters":[]},
            "sarNormalize":{"releaseRequired":true,"encoders":[],"filters":[]},
            "reverseLoop":{"releaseRequired":true,"encoders":[],"filters":[]}
        }"#;
        let unknown_top =
            format!(r#"{{"schemaVersion":1,"features":{valid_features},"unexpected":true}}"#);
        assert!(
            parse_ffmpeg_capability_contract(&unknown_top)
                .unwrap_err()
                .contains("unknown field")
        );

        let unknown_feature_field = r#"{
            "schemaVersion":1,
            "features":{
                "coreExport":{"releaseRequired":true,"encoders":[],"filters":[],"extra":true},
                "hdrToSdr":{"releaseRequired":true,"encoders":[],"filters":[]},
                "sarNormalize":{"releaseRequired":true,"encoders":[],"filters":[]},
                "reverseLoop":{"releaseRequired":true,"encoders":[],"filters":[]}
            }
        }"#;
        assert!(
            parse_ffmpeg_capability_contract(unknown_feature_field)
                .unwrap_err()
                .contains("unknown field")
        );

        let duplicate = r#"{
            "schemaVersion":1,
            "features":{
                "coreExport":{"releaseRequired":true,"encoders":["libx264","libx264"],"filters":[]},
                "hdrToSdr":{"releaseRequired":true,"encoders":[],"filters":[]},
                "sarNormalize":{"releaseRequired":true,"encoders":[],"filters":[]},
                "reverseLoop":{"releaseRequired":true,"encoders":[],"filters":[]}
            }
        }"#;
        assert!(
            parse_ffmpeg_capability_contract(duplicate)
                .unwrap_err()
                .contains("duplicate")
        );

        let missing_array = r#"{
            "schemaVersion":1,
            "features":{
                "coreExport":{"releaseRequired":true,"filters":[]},
                "hdrToSdr":{"releaseRequired":true,"encoders":[],"filters":[]},
                "sarNormalize":{"releaseRequired":true,"encoders":[],"filters":[]},
                "reverseLoop":{"releaseRequired":true,"encoders":[],"filters":[]}
            }
        }"#;
        assert!(
            parse_ffmpeg_capability_contract(missing_array)
                .unwrap_err()
                .contains("missing field")
        );
    }

    #[test]
    fn feature_capability_lists_missing_encoders_and_filters() {
        let requirement = FfmpegFeatureRequirement {
            release_required: true,
            encoders: vec!["libx264".to_string(), "missing-encoder".to_string()],
            filters: vec!["zscale".to_string(), "missing-filter".to_string()],
        };
        let runtime = FfmpegRuntimeCapabilities {
            version: "test".to_string(),
            encoder_names: HashSet::from(["libx264".to_string()]),
            filter_names: HashSet::from(["zscale".to_string()]),
        };
        let capability = feature_capability("hdrToSdr", &requirement, &runtime);
        assert!(!capability.available);
        assert_eq!(capability.missing_encoders, ["missing-encoder"]);
        assert_eq!(capability.missing_filters, ["missing-filter"]);
        assert!(
            require_ffmpeg_feature(
                "hdrToSdr",
                &FfmpegCapabilityContract {
                    schema_version: 1,
                    features: HashMap::from([("hdrToSdr".to_string(), requirement)]),
                },
                &runtime
            )
            .unwrap_err()
            .contains("missing encoder missing-encoder")
        );
    }

    #[test]
    fn smoke_capability_mask_is_gated_allowlisted_and_non_persistent() {
        let runtime = FfmpegRuntimeCapabilities {
            version: "test".to_string(),
            encoder_names: HashSet::from(["libx264".to_string()]),
            filter_names: HashSet::from(["scale".to_string(), "subtitles".to_string()]),
        };
        let ungated = HashMap::from([(
            "VFL_SMOKE_MISSING_CAPABILITY_FILTERS".to_string(),
            "not-allowlisted".to_string(),
        )]);
        assert!(
            apply_smoke_capability_mask(runtime.clone(), &ungated)
                .unwrap()
                .filter_names
                .contains("subtitles")
        );

        let gated = HashMap::from([
            ("VFL_SMOKE_INPUT".to_string(), "/tmp/input.mp4".to_string()),
            (
                "VFL_SMOKE_STATUS".to_string(),
                "/tmp/status.json".to_string(),
            ),
            (
                "VFL_SMOKE_MISSING_CAPABILITY_FILTERS".to_string(),
                "subtitles, subtitles".to_string(),
            ),
        ]);
        let masked = apply_smoke_capability_mask(runtime.clone(), &gated).unwrap();
        assert!(!masked.filter_names.contains("subtitles"));
        assert!(masked.filter_names.contains("scale"));
        assert!(masked.encoder_names.contains("libx264"));
        assert!(runtime.filter_names.contains("subtitles"));

        let invalid = HashMap::from([
            ("VFL_SMOKE_INPUT".to_string(), "/tmp/input.mp4".to_string()),
            (
                "VFL_SMOKE_STATUS".to_string(),
                "/tmp/status.json".to_string(),
            ),
            (
                "VFL_SMOKE_MISSING_CAPABILITY_FILTERS".to_string(),
                "scale".to_string(),
            ),
        ]);
        assert!(
            apply_smoke_capability_mask(runtime, &invalid)
                .unwrap_err()
                .contains("non-allowlisted")
        );
    }

    #[test]
    fn request_capability_gate_allows_partial_runtime_for_feasible_remuxes() {
        let contract = ffmpeg_capability_contract().unwrap();
        let empty_runtime = FfmpegRuntimeCapabilities {
            version: "partial".to_string(),
            encoder_names: HashSet::new(),
            filter_names: HashSet::new(),
        };
        let mut request = base_request();
        request.size_limit_mb = 0.0;
        let codecs = select_codec_plan(request.format, &HashSet::new(), &request.advanced).unwrap();
        assert_eq!(codecs.video_codec, None);
        let remux = build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, None)
            .unwrap();
        assert_eq!(remux.video_action, StreamAction::Copy);
        assert_eq!(remux.audio_action, StreamAction::Copy);
        require_encode_plan_capabilities(&remux, &contract, &empty_runtime).unwrap();
        assert!(full_reencode_fallback(&remux).is_err());

        request.size_limit_mb = 8.0;
        let size_copy =
            build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, None)
                .unwrap();
        assert_eq!(size_copy.video_action, StreamAction::Copy);
        assert!(!size_copy.size_copy_candidates.is_empty());
        assert!(size_copy.size_contract.is_none());
        require_encode_plan_capabilities(&size_copy, &contract, &empty_runtime).unwrap();

        let video_only_inventory = HashSet::from(["libx264".to_string()]);
        let video_only_codecs =
            select_codec_plan(request.format, &video_only_inventory, &request.advanced).unwrap();
        let no_audio_encoder_size_copy = build_encode_command_plan(
            &request,
            &probe_10s_1920x1080_audio(),
            video_only_codecs,
            None,
        )
        .unwrap();
        assert_eq!(no_audio_encoder_size_copy.video_action, StreamAction::Copy);
        assert_eq!(no_audio_encoder_size_copy.audio_action, StreamAction::Copy);
        assert!(no_audio_encoder_size_copy.size_contract.is_none());

        request.size_limit_mb = 0.0;
        request.crop = Some(Crop {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        });
        assert!(
            build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, None,)
                .unwrap_err()
                .contains("No compatible video encoder")
        );

        request.crop = None;
        request.perturb_first_frame = true;
        assert!(
            build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, Some(123),)
                .unwrap_err()
                .contains("No compatible video encoder")
        );

        let mut odd_probe = probe_10s_1920x1080_audio();
        odd_probe.width = 1919;
        request.perturb_first_frame = false;
        request.size_limit_mb = 8.0;
        let encoder_names = HashSet::from(["libx264".to_string(), "aac".to_string()]);
        let fallback_codecs =
            select_codec_plan(request.format, &encoder_names, &request.advanced).unwrap();
        let fallback_plan =
            build_encode_command_plan(&request, &odd_probe, fallback_codecs, None).unwrap();
        assert!(!fallback_plan.size_copy_candidates.is_empty());
        assert!(
            fallback_plan
                .video_filters
                .as_deref()
                .is_some_and(|filters| filters.contains("crop="))
        );
        let encoder_only_runtime = FfmpegRuntimeCapabilities {
            version: "partial".to_string(),
            encoder_names,
            filter_names: HashSet::new(),
        };
        require_initial_encode_plan_capabilities(&fallback_plan, &contract, &encoder_only_runtime)
            .unwrap();
        let encoded_fallback =
            build_size_reencode_fallback_plan(&request, &odd_probe, &fallback_plan).unwrap();
        assert!(
            require_encode_plan_capabilities(&encoded_fallback, &contract, &encoder_only_runtime,)
                .unwrap_err()
                .contains("filter crop")
        );
    }

    #[test]
    fn request_capability_gate_checks_only_actual_encoders_and_filter_graph_nodes() {
        let filters = filter_names_from_graph(Some(
            "scale=w='if(gte(iw,ih),trunc(min(iw,720)/2)*2,-2)',eq=contrast=1.1,split[f][r0];[r0]reverse[r];[f][r]concat=n=2:v=1",
        ));
        assert_eq!(
            filters,
            HashSet::from([
                "scale".to_string(),
                "eq".to_string(),
                "split".to_string(),
                "reverse".to_string(),
                "concat".to_string(),
            ])
        );

        let mut request = base_request();
        request.size_limit_mb = 0.0;
        request.audio_enabled = false;
        request.crop = Some(Crop {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        });
        let encoders = HashSet::from(["libx264".to_string()]);
        let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
        let plan = build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, None)
            .unwrap();
        let contract = ffmpeg_capability_contract().unwrap();
        let runtime = FfmpegRuntimeCapabilities {
            version: "partial".to_string(),
            encoder_names: encoders,
            filter_names: HashSet::from(["crop".to_string()]),
        };
        require_encode_plan_capabilities(&plan, &contract, &runtime).unwrap();

        let missing_filter = FfmpegRuntimeCapabilities {
            filter_names: HashSet::new(),
            ..runtime
        };
        assert!(
            require_encode_plan_capabilities(&plan, &contract, &missing_filter)
                .unwrap_err()
                .contains("filter crop")
        );
    }

    #[test]
    fn mp3_plan_requires_the_contract_declared_encoder() {
        let mut request = base_request();
        request.format = OutputFormat::Mp3;
        request.size_limit_mb = 0.0;
        let codecs = select_codec_plan(request.format, &HashSet::new(), &request.advanced).unwrap();
        assert!(
            build_encode_command_plan(&request, &probe_10s_1920x1080_audio(), codecs, None,)
                .unwrap_err()
                .contains("No MP3 audio encoder")
        );
    }
}
