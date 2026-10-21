//! Persistent RGB8 inference with one decoded-presentation clock and lossless replay.
mod sequence;

use crate::interpolation_components::{ModelExecution, ResolvedModel};
use crate::interpolation_timeline::{Rate, is_scene_cut};
use crate::process::{self, ManagedChild, ProcessGroup};
use crate::video::{EncodeRequest, VideoProbe};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const MAX_SCRATCH_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const FREE_HEADROOM: u64 = 256 * 1024 * 1024;
const MAX_PIXELS: u64 = 3840 * 2160;
const IDLE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterpolationDiagnostics {
    pub target_fps: u32,
    pub model_id: String,
    pub model_digest: String,
    pub worker_digest: String,
    pub input_frames: u64,
    pub output_frames: u64,
    pub inferred_frames: u64,
    pub held_frames: u64,
    pub cut_pairs: u64,
    pub scratch_bytes: u64,
}

pub(crate) use crate::media_source::SourceIdentity;

pub(crate) struct PreparedVideo {
    _directory: TempDir,
    pub path: PathBuf,
    pub source: PathBuf,
    pub identity: SourceIdentity,
    pub start_s: f64,
    pub rate: Rate,
    pub diagnostics: InterpolationDiagnostics,
    pub output_budget: u64,
    pub video_stream_index: u32,
    pub format_mp4: bool,
    pub loop_video: bool,
    worker_path: PathBuf,
    matrix: String,
    range: String,
    color_tags: Vec<(String, String)>,
}
impl PreparedVideo {
    pub fn output_budget_for(&self, output: &Path) -> Result<u64, String> {
        let parent = output
            .parent()
            .ok_or("The output has no storage directory.")?;
        let free = available_space(&self.worker_path, parent)?;
        if free < FREE_HEADROOM * 2 {
            return Err(
                "Free at least 512 MiB on the output drive before encoding interpolated video."
                    .into(),
            );
        }
        Ok(self
            .output_budget
            .min(free.saturating_sub(FREE_HEADROOM) / 2))
    }
    /// Original input 0 continues to own audio and metadata. Only its video map
    /// is replaced. Restore source epoch around the existing source-time trim
    /// and subtitle chain; generated frames already passed exact trim admission.
    pub fn rewrite_args(&self, args: &[String], video_index: u32, format_mp4: bool) -> Vec<String> {
        let mut result = args.to_vec();
        let input_end = result
            .windows(2)
            .position(|pair| pair[0] == "-i")
            .map(|i| i + 2)
            .unwrap_or(0);
        result.splice(
            input_end..input_end,
            ["-i".to_string(), self.path.to_string_lossy().into_owned()],
        );
        for i in 0..result.len().saturating_sub(1) {
            if result[i] == "-map" && result[i + 1] == format!("0:{video_index}") {
                result[i + 1] = "1:0".into();
            }
        }
        // 1/target clock is representable before source trim/speed. The raw
        // lossless Matroska spool's millisecond timestamps are never reused as
        // the scheduling clock. Reconstruct each ideal timestamp from N.
        let start_ns = (self.start_s * 1_000_000_000.0).round() as u64;
        let prefix = format!(
            "settb=expr=1/1000000000,setpts=round(N*1000000000*{}/{})+{start_ns},scale=in_range=pc:out_range={}:out_color_matrix={},format=yuv444p",
            self.rate.denominator, self.rate.numerator, self.range, self.matrix
        );
        // Reverse/Loop concat consumes AVFrame durations, which setpts alone
        // does not scale. Reconstruct the final complete picture clock after
        // temporal filters and pin each frame duration with fps. The admitted
        // count is explicit, so EOF padding cannot add an extra picture.
        let final_frames = self.diagnostics.output_frames * if self.loop_video { 2 } else { 1 };
        let target = self.diagnostics.target_fps;
        let suffix = format!(
            "settb=expr=1/120000,setpts=N*{},fps={target}:round=near,trim=end_frame={final_frames}",
            120000 / target
        );
        if let Some(i) = result.iter().position(|arg| arg == "-vf") {
            result[i + 1] = format!("{prefix},{},{suffix}", result[i + 1]);
        } else {
            // Options must precede the final destination.
            let end = result.len().saturating_sub(1);
            result.splice(end..end, ["-vf".into(), format!("{prefix},{suffix}")]);
        }
        let end = result.len().saturating_sub(1);
        let mut cadence = vec![
            "-fps_mode".into(),
            "passthrough".into(),
            "-enc_time_base".into(),
            "1:120000".into(),
        ];
        let null_output = result
            .windows(2)
            .any(|pair| pair[0] == "-f" && pair[1] == "null");
        if format_mp4 && !null_output {
            cadence.extend(["-video_track_timescale".into(), "120000".into()]);
        }
        for (name, value) in &self.color_tags {
            cadence.extend([name.clone(), value.clone()]);
        }
        if result
            .windows(2)
            .any(|pair| pair[0] == "-c:v" && pair[1] == "libx264")
        {
            let mut parameters = self
                .color_tags
                .iter()
                .filter_map(|(name, value)| {
                    let key = match name.as_str() {
                        "-color_primaries" => "colorprim",
                        "-color_trc" => "transfer",
                        "-colorspace" => "colormatrix",
                        _ => return None,
                    };
                    Some(format!("{key}={value}"))
                })
                .collect::<Vec<_>>();
            let range = result
                .windows(2)
                .rev()
                .find(|pair| pair[0] == "-color_range")
                .map_or(self.range.as_str(), |pair| pair[1].as_str());
            parameters.push(format!("range={range}"));
            cadence.extend(["-x264-params".into(), parameters.join(":")]);
        }
        result.splice(end..end, cadence);
        result
    }
}

fn command(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = &mut command;
    command
}

fn inference_command(model: &ResolvedModel, gpu: &str) -> Result<Command, String> {
    match &model.execution {
        ModelExecution::Native => {
            let mut cmd = command(&model.worker_path);
            cmd.arg("--model")
                .arg(&model.model_dir)
                .args(["--gpu", gpu]);
            Ok(cmd)
        }
        ModelExecution::TorchPair {
            python_path,
            script_path,
            source_dir,
            cache_dir,
        }
        | ModelExecution::TorchSequence {
            python_path,
            script_path,
            source_dir,
            cache_dir,
        } => {
            let mut cmd = command(python_path);
            crate::optional_interpolation_components::configure_command(&mut cmd, cache_dir)?;
            cmd.env_remove("PYTHONHOME")
                .env_remove("PYTHONPATH")
                .args(["-I", "-B"])
                .arg(script_path)
                .arg("--model-id")
                .arg(&model.model_id)
                .arg("--model")
                .arg(&model.model_dir)
                .arg("--sources")
                .arg(source_dir)
                .arg("--cache")
                .arg(cache_dir)
                .args(["--gpu", gpu]);
            Ok(cmd)
        }
    }
}

pub(crate) fn available_space(worker: &Path, path: &Path) -> Result<u64, String> {
    let mut cmd = command(worker);
    cmd.arg("--available-space").arg(path);
    let mut child = ManagedChild::spawn(cmd)
        .map_err(|_| "Could not inspect interpolation storage.".to_string())?;
    let stdout = child.stdout.take().ok_or("Missing storage-check output.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Missing storage-check diagnostics.")?;
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(4097).read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.take(4097).read_to_end(&mut bytes)
    });
    let status = child
        .wait_timeout(Duration::from_secs(15))
        .map_err(|_| "Storage check failed.".to_string())?;
    if status.is_none() {
        let _ = child.control.kill();
    }
    let bytes = output
        .join()
        .map_err(|_| "Storage check stopped.".to_string())?
        .map_err(|_| "Storage check output failed.".to_string())?;
    let _ = errors.join();
    if !status.is_some_and(|status| status.success()) || bytes.len() > 4096 {
        return Err("Interpolation storage check failed or timed out.".into());
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Space {
        available_bytes: u64,
    }
    serde_json::from_slice::<Space>(&bytes)
        .map(|space| space.available_bytes)
        .map_err(|_| "Storage check returned invalid data.".into())
}

struct PipelineGuard {
    group: ProcessGroup,
    stop: Arc<AtomicBool>,
    watcher: Option<std::thread::JoinHandle<()>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}
impl Drop for PipelineGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        process::kill_process_group(&self.group);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn drain_errors<R: Read + Send + 'static>(pipe: R) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut buffer = [0u8; 4096];
        // Deliberately discard child diagnostics: paths and media metadata must
        // never enter public failure text. Always drain to avoid pipe stalls.
        while reader.read(&mut buffer).is_ok_and(|size| size != 0) {}
    })
}

fn parse_showinfo(line: &str) -> Option<(u64, u64)> {
    if !line.contains("Parsed_showinfo_") || !line.contains(" pts:") {
        return None;
    }
    let number_after = |key: &str| -> Option<&str> {
        let tail = line.split_once(key)?.1.trim_start();
        tail.split_whitespace().next()
    };
    Some((
        number_after(" n:")?.parse().ok()?,
        number_after(" pts:")?.parse().ok()?,
    ))
}

fn read_picture<R: Read>(pipe: &mut R, frame: &mut [u8]) -> Result<(), String> {
    pipe.read_exact(frame).map_err(|_| "Frame decoding or native interpolation ended before a complete RGB picture. No output was published.".into())
}

struct ColorDescription {
    matrix: &'static str,
    range: &'static str,
    color_tags: Vec<(String, String)>,
}

impl ColorDescription {
    fn from_probe(probe: &VideoProbe) -> Self {
        let matrix = match probe.color_space.as_deref() {
            Some("bt709") => "bt709",
            Some("bt2020nc" | "bt2020_ncl") => "bt2020",
            Some("smpte240m") => "smpte240m",
            _ => "bt601",
        };
        let range = if matches!(probe.color_range.as_deref(), Some("pc" | "jpeg")) {
            "pc"
        } else {
            "tv"
        };
        let color_tags = [
            ("-color_primaries", &probe.color_primaries),
            ("-color_trc", &probe.color_transfer),
            ("-colorspace", &probe.color_space),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|value| (name.to_string(), value.clone()))
        })
        .collect();
        Self {
            matrix,
            range,
            color_tags,
        }
    }
}

fn decoder_command(
    ffmpeg: &str,
    source: &Path,
    probe: &VideoProbe,
    start_s: f64,
    end_s: f64,
    matrix: &str,
) -> Command {
    let mut decoder_command = command(Path::new(ffmpeg));
    let start_ns = (start_s * 1_000_000_000.0).round() as u64;
    let trim = format!(
        "trim=start={start_s:.9}:end={end_s:.9},settb=expr=1/1000000000,setpts=PTS-{start_ns},scale=in_color_matrix={matrix}:in_range=auto:out_range=pc,format=rgb24,showinfo"
    );
    decoder_command
        .args([
            "-hide_banner",
            "-loglevel",
            "info",
            "-nostdin",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-i",
        ])
        .arg(source)
        .args([
            "-map",
            &format!("0:{}", probe.video_stream_index),
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &trim,
            "-fps_mode",
            "passthrough",
            "-c:v",
            "rawvideo",
            "-threads",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
    decoder_command
}

fn assembler_command(ffmpeg: &str, probe: &VideoProbe, rate: Rate, path: &Path) -> Command {
    let mut assembler_command = command(Path::new(ffmpeg));
    assembler_command
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
            "-filter_threads",
            "1",
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &format!("{}x{}", probe.width, probe.height),
            "-framerate",
            &rate.ffmpeg(),
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-pix_fmt",
            "gbrp",
            "-color_range",
            "pc",
            "-threads",
            "1",
        ])
        .arg(path);
    assembler_command
}

fn scratch_size(directory: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    for entry in
        fs::read_dir(directory).map_err(|_| "Could not inspect interpolation scratch storage.")?
    {
        let entry = entry.map_err(|_| "Could not inspect interpolation scratch storage.")?;
        let metadata = entry
            .file_type()
            .map_err(|_| "Could not inspect interpolation scratch storage.")?;
        if !metadata.is_file() || metadata.is_symlink() {
            return Err("Interpolation scratch storage contains an unexpected entry.".into());
        }
        total = total
            .checked_add(
                entry
                    .metadata()
                    .map_err(|_| "Could not inspect interpolation scratch storage.")?
                    .len(),
            )
            .ok_or("Interpolation scratch size overflow.")?;
    }
    Ok(total)
}

fn send_timing(
    sender: &mpsc::SyncSender<Result<(u64, u64), String>>,
    stop: &AtomicBool,
    mut message: Result<(u64, u64), String>,
) -> bool {
    while !stop.load(Ordering::Relaxed) {
        match sender.try_send(message) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
            Err(mpsc::TrySendError::Full(value)) => message = value,
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

fn spawn_timing_reader<R: Read + Send + 'static>(
    pipe: R,
    sender: mpsc::SyncSender<Result<(u64, u64), String>>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        loop {
            line.clear();
            let message = match reader.by_ref().take(16 * 1024).read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.len() >= 16 * 1024 => {
                    Some(Err("Decoder returned an oversized timing record.".into()))
                }
                Ok(_) if line.contains(" i:T") || line.contains(" i:B") => Some(Err(
                    "Interlaced video must be deinterlaced before AI interpolation.".into(),
                )),
                Ok(_) => parse_showinfo(&line).map(Ok),
                Err(_) => Some(Err("Decoder timing stream failed.".into())),
            };
            if let Some(message) = message {
                let failed = message.is_err();
                if !send_timing(&sender, &stop, message) || failed {
                    break;
                }
            }
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare(
    ffmpeg: &str,
    source: &Path,
    probe: &VideoProbe,
    request: &EncodeRequest,
    model: &ResolvedModel,
    cancel: &Arc<AtomicBool>,
    group: &ProcessGroup,
    mut progress: impl FnMut(f64),
) -> Result<PreparedVideo, String> {
    let settings = request
        .interpolation
        .as_ref()
        .ok_or("Missing interpolation settings.")?;
    let identity = SourceIdentity::capture(source)?;
    let start_s = request
        .trim
        .as_ref()
        .map_or(0.0, |trim| trim.start_s.max(0.0));
    let end_s = request
        .trim
        .as_ref()
        .and_then(|trim| trim.end_s)
        .unwrap_or(probe.duration_s)
        .min(probe.duration_s);
    let duration_ns = ((end_s - start_s) * 1_000_000_000.0).round() as u64;
    let rate = Rate::for_playback(settings.target_fps, request.speed)?;
    let count = rate.frame_count(duration_ns)?;
    let pixels = u64::from(probe.width) * u64::from(probe.height);
    if pixels == 0 || pixels > MAX_PIXELS || probe.width > 8192 || probe.height > 8192 {
        return Err("AI interpolation supports up to 3840×2160 pixels per picture. Trim/crop the source into a smaller video first.".into());
    }
    let frame_bytes =
        usize::try_from(pixels * 3).map_err(|_| "Interpolation picture is too large.")?;
    let estimate = (frame_bytes as u64)
        .checked_mul(count)
        .and_then(|value| value.checked_add(value / 4))
        .ok_or("Interpolation storage estimate overflow.")?;
    if estimate > MAX_SCRATCH_BYTES.saturating_sub(FREE_HEADROOM * 3) {
        return Err("Interpolation needs more than the 32 GiB per-job scratch allowance. Trim the clip or reduce resolution before interpolating.".into());
    }
    let directory = tempfile::Builder::new()
        .prefix("vfl-interpolation-")
        .tempdir()
        .map_err(|_| "Could not create interpolation scratch storage.".to_string())?;
    let path = directory.path().join("prepared.mkv");
    let free = available_space(&model.worker_path, directory.path())?;
    if free < estimate.saturating_add(FREE_HEADROOM * 3) {
        return Err("There is not enough free scratch space for interpolation and encoder headroom. Trim the clip or free storage and retry.".into());
    }
    let quota = MAX_SCRATCH_BYTES.min(free.saturating_sub(FREE_HEADROOM * 2));
    if matches!(model.execution, ModelExecution::TorchSequence { .. }) {
        return sequence::prepare(
            sequence::Job {
                ffmpeg,
                source,
                probe,
                request,
                model,
                cancel,
                group,
                identity,
                start_s,
                end_s,
                duration_ns,
                rate,
                count,
                frame_bytes,
                directory,
                path,
                quota,
            },
            &mut progress,
        );
    }
    let stop = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let clock = Instant::now();
    let last_progress = Arc::new(AtomicU64::new(0));
    let watcher = {
        let stop = stop.clone();
        let timed_out = timed_out.clone();
        let last = last_progress.clone();
        let group = group.clone();
        let cancel = cancel.clone();
        let worker_path = model.worker_path.clone();
        let scratch = directory.path().to_path_buf();
        std::thread::spawn(move || {
            let _scope = process::JobScope::enter(process::JobContext {
                cancel: cancel.clone(),
                child: group.clone(),
            });
            let mut last_space_check = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let idle = clock.elapsed().as_millis() as u64 - last.load(Ordering::Relaxed);
                let too_large = scratch_size(&scratch).map_or(true, |size| size > quota);
                let low_space = if last_space_check.elapsed() >= Duration::from_secs(1) {
                    last_space_check = Instant::now();
                    available_space(&worker_path, &scratch)
                        .map_or(true, |free| free < FREE_HEADROOM)
                } else {
                    false
                };
                if cancel.load(Ordering::Relaxed)
                    || idle > IDLE_TIMEOUT.as_millis() as u64
                    || too_large
                    || low_space
                {
                    timed_out.store(!cancel.load(Ordering::Relaxed), Ordering::Relaxed);
                    process::kill_process_group(&group);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let mut guard = PipelineGuard {
        group: group.clone(),
        stop,
        watcher: Some(watcher),
        readers: Vec::new(),
    };

    // showinfo is immediately before raw encoding: every admitted RGB frame
    // and its exact PTS come from the same graph, avoiding a separate timing scan.
    let ColorDescription {
        matrix,
        range,
        color_tags,
    } = ColorDescription::from_probe(probe);
    let decoder_command = decoder_command(ffmpeg, source, probe, start_s, end_s, matrix);
    let mut decoder = ManagedChild::spawn(decoder_command)
        .map_err(|_| "Could not start interpolation decoder.".to_string())?;
    let mut decoded = decoder
        .stdout
        .take()
        .ok_or("Missing decoded picture stream.")?;
    let decoder_errors = decoder
        .stderr
        .take()
        .ok_or("Missing decoder timing stream.")?;
    let (sender, receiver) = mpsc::sync_channel::<Result<(u64, u64), String>>(4);
    guard.readers.push(spawn_timing_reader(
        decoder_errors,
        sender,
        guard.stop.clone(),
    ));

    let gpu = crate::interpolation_components::gpu_choice();
    if gpu != "auto" && gpu != "-1" && gpu.parse::<u32>().is_err() {
        return Err("Invalid interpolation GPU override.".into());
    }
    let mut worker_command = inference_command(model, &gpu)?;
    worker_command.arg("--stream").args([
        "--width",
        &probe.width.to_string(),
        "--height",
        &probe.height.to_string(),
    ]);
    let mut worker = ManagedChild::spawn_with_stdin(worker_command)
        .map_err(|_| "Could not start native frame interpolation.".to_string())?;
    let mut worker_input = worker
        .stdin
        .take()
        .ok_or("Missing native interpolation input.")?;
    let mut worker_output = worker
        .stdout
        .take()
        .ok_or("Missing native interpolation output.")?;
    guard.readers.push(drain_errors(
        worker
            .stderr
            .take()
            .ok_or("Missing native interpolation diagnostics.")?,
    ));
    let assembler_command = assembler_command(ffmpeg, probe, rate, &path);
    let mut assembler = ManagedChild::spawn_with_stdin(assembler_command)
        .map_err(|_| "Could not start lossless interpolation storage.".to_string())?;
    let mut assembled = assembler
        .stdin
        .take()
        .ok_or("Missing lossless storage input.")?;
    guard.readers.push(drain_errors(
        assembler
            .stderr
            .take()
            .ok_or("Missing lossless storage diagnostics.")?,
    ));
    guard.readers.push(drain_errors(
        assembler
            .stdout
            .take()
            .ok_or("Missing lossless storage output.")?,
    ));
    let mut left = vec![0u8; frame_bytes];
    let mut right = vec![0u8; frame_bytes];
    let mut output = vec![0u8; frame_bytes];
    let mut input_frames = 0u64;
    let mut output_frames = 0u64;
    let mut inferred_frames = 0u64;
    let mut held_frames = 0u64;
    let mut cut_pairs = 0u64;
    let mut read_next = |frame: &mut [u8]| -> Result<Option<u64>, String> {
        match receiver.recv_timeout(IDLE_TIMEOUT) {
            Ok(Ok((index, pts))) => {
                if index != input_frames || pts >= duration_ns {
                    return Err(
                        "Decoded picture timing does not match the retained interval.".into(),
                    );
                }
                read_picture(&mut decoded, frame)?;
                input_frames += 1;
                if input_frames > 1_000_000 {
                    return Err(
                        "Interpolation input has more than one million retained pictures.".into(),
                    );
                }
                last_progress.store(clock.elapsed().as_millis() as u64, Ordering::Relaxed);
                Ok(Some(pts))
            }
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
            Err(_) => Err("Interpolation decoder timed out.".into()),
        }
    };
    let first_pts = read_next(&mut left)?;
    let only_gap =
        end_s <= probe.video_start_s || probe.video_end_s.is_some_and(|end| end <= start_s);
    let mut left_pts = match first_pts { Some(pts) => pts, None if only_gap => 0, None => return Err("The selected trim contains no retained pictures to interpolate. Expand the trim slightly.".into()) };
    let leading_gap_ns =
        ((probe.video_start_s - start_s).max(0.0) * 1_000_000_000.0).round() as u64;
    let video_end_ns =
        ((probe.video_end_s.unwrap_or(end_s) - start_s).max(0.0) * 1_000_000_000.0).round() as u64;
    loop {
        let next = read_next(&mut right)?;
        if let Some(right_pts) = next
            && right_pts <= left_pts
        {
            return Err(
                "AI interpolation requires strictly increasing presentation timestamps.".into(),
            );
        }
        let cut = next.is_some() && is_scene_cut(&left, &right);
        if cut {
            cut_pairs += 1;
        }
        while output_frames < count && next.is_none_or(|pts| rate.sample_before(output_frames, pts))
        {
            process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
            if timed_out.load(Ordering::Relaxed) {
                return Err(
                    "Interpolation stopped because a stage stalled or exceeded its scratch quota."
                        .into(),
                );
            }
            let alpha = next
                .map(|pts| rate.alpha(output_frames, left_pts, pts))
                .transpose()?
                .unwrap_or(0.0);
            let pad_black = rate.sample_before(output_frames, leading_gap_ns)
                || !rate.sample_before(output_frames, video_end_ns);
            let pixels: &[u8] = if pad_black {
                output.fill(0);
                held_frames += 1;
                &output
            } else if alpha > 0.0 && alpha < 1.0 && !cut && left != right {
                worker_input
                    .write_all(&alpha.to_le_bytes())
                    .and_then(|_| worker_input.write_all(&left))
                    .and_then(|_| worker_input.write_all(&right))
                    .and_then(|_| worker_input.flush())
                    .map_err(|_| {
                        "Native interpolation stopped while receiving a picture.".to_string()
                    })?;
                read_picture(&mut worker_output, &mut output)?;
                inferred_frames += 1;
                &output
            } else {
                held_frames += 1;
                &left
            };
            assembled.write_all(pixels).map_err(|_| {
                "Lossless interpolation storage failed. Free scratch space and retry.".to_string()
            })?;
            output_frames += 1;
            last_progress.store(clock.elapsed().as_millis() as u64, Ordering::Relaxed);
            progress(output_frames as f64 / count as f64);
        }
        if let Some(pts) = next {
            std::mem::swap(&mut left, &mut right);
            left_pts = pts;
        } else {
            break;
        }
    }
    // Every metadata record was paired with exactly one RGB picture. Any tail
    // bytes mean a decoder cadence/count mismatch, never an accepted export.
    let mut extra = [0u8; 1];
    if decoded
        .read(&mut extra)
        .map_err(|_| "Decoder tail check failed.".to_string())?
        != 0
    {
        return Err("Decoder emitted pictures without matching presentation timestamps.".into());
    }
    drop(worker_input);
    drop(assembled);
    if worker_output
        .read(&mut extra)
        .map_err(|_| "Native output tail check failed.".to_string())?
        != 0
    {
        return Err("Native worker returned an invalid extra picture.".into());
    }
    for child in [&decoder, &worker, &assembler] {
        let status = child
            .wait_timeout(IDLE_TIMEOUT)
            .map_err(|_| "Interpolation process waiter failed.".to_string())?;
        if !status.is_some_and(|status| status.success()) {
            return Err(if cancel.load(Ordering::Relaxed) {
                "Canceled.".into()
            } else {
                "A decoder, AI model or lossless storage process failed. Verify components and free space, then retry.".into()
            });
        }
    }
    identity.verify(source)?;
    let scratch_bytes = fs::metadata(&path)
        .map_err(|_| "Lossless interpolation storage was not created.".to_string())?
        .len();
    if scratch_bytes == 0 || scratch_bytes > quota {
        return Err("Interpolation scratch quota was exceeded or no frames were stored.".into());
    }
    let remaining = MAX_SCRATCH_BYTES
        .saturating_sub(scratch_bytes)
        .min(available_space(&model.worker_path, directory.path())?.saturating_sub(FREE_HEADROOM));
    if remaining < FREE_HEADROOM {
        return Err("Not enough scratch headroom remains for final encoding.".into());
    }
    Ok(PreparedVideo {
        _directory: directory,
        path,
        source: source.to_path_buf(),
        identity,
        start_s,
        rate,
        output_budget: remaining / 2,
        video_stream_index: probe.video_stream_index,
        format_mp4: false,
        loop_video: request.loop_video,
        worker_path: model.worker_path.clone(),
        matrix: matrix.into(),
        range: range.into(),
        color_tags,
        diagnostics: InterpolationDiagnostics {
            target_fps: settings.target_fps,
            model_id: model.model_id.clone(),
            model_digest: model.model_digest.clone(),
            worker_digest: model.worker_digest.clone(),
            input_frames,
            output_frames,
            inferred_frames,
            held_frames,
            cut_pairs,
            scratch_bytes,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_python_disables_bytecode_before_worker_imports() {
        let directory = tempfile::tempdir().unwrap();
        for sequence in [false, true] {
            let python_path = directory.path().join("python");
            let script_path = directory.path().join("worker.py");
            let source_dir = directory.path().join("sources");
            let cache_dir = directory
                .path()
                .join(if sequence { "sequence" } else { "pair" });
            let execution = if sequence {
                ModelExecution::TorchSequence {
                    python_path: python_path.clone(),
                    script_path: script_path.clone(),
                    source_dir,
                    cache_dir,
                }
            } else {
                ModelExecution::TorchPair {
                    python_path: python_path.clone(),
                    script_path: script_path.clone(),
                    source_dir,
                    cache_dir,
                }
            };
            let model = ResolvedModel {
                execution,
                worker_path: directory.path().join("storage-helper"),
                model_dir: directory.path().join("models"),
                model_id: if sequence {
                    "ldf-vfi"
                } else {
                    "gmfss-fortuna-union"
                }
                .into(),
                model_digest: String::new(),
                worker_digest: String::new(),
                _lease: None,
            };
            let command = inference_command(&model, "auto").unwrap();
            assert_eq!(command.get_program(), python_path.as_os_str());
            let arguments: Vec<_> = command.get_args().collect();
            // Isolated mode ignores PYTHONDONTWRITEBYTECODE. -B must precede
            // the script so even its earliest stdlib imports cannot mutate
            // the hash-verified portable runtime.
            assert_eq!(arguments[0], "-I");
            assert_eq!(arguments[1], "-B");
            assert_eq!(arguments[2], script_path.as_os_str());
            assert!(command.get_envs().any(|(name, value)| {
                name == "PYTHONDONTWRITEBYTECODE" && value == Some(std::ffi::OsStr::new("1"))
            }));
        }
    }

    #[test]
    #[ignore = "requires the built native sidecar, catalog models and FFmpeg; use VFL_INTERPOLATION_GPU=-1 for CPU"]
    fn native_prepared_video_preserves_cadence_audio_and_persistent_inference() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("input.mkv");
        let ffmpeg =
            std::env::var("VFL_INTERPOLATION_TEST_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        let status = Command::new(&ffmpeg)
            .args([
                "-y",
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x64:rate=24",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "0.25",
                "-c:v",
                "ffv1",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
        let request: EncodeRequest = serde_json::from_value(serde_json::json!({
            "inputPath":source,"outputPath":directory.path().join("output.mp4"),"format":"mp4","sizeLimitMb":0,"audioEnabled":true,
            "reverse":false,"speed":1,"rotateDeg":0,"interpolation":{"targetFps":60,"modelId":"rife-v4.25"}
        })).unwrap();
        let probe = crate::video::probe_video(source.to_string_lossy().into_owned()).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let group = Arc::new(std::sync::Mutex::new(Vec::new()));
        let _scope = process::JobScope::enter(process::JobContext {
            cancel: cancel.clone(),
            child: group.clone(),
        });
        let mut prepared = prepare(
            &ffmpeg,
            &source,
            &probe,
            &request,
            &model,
            &cancel,
            &group,
            |_| {},
        )
        .unwrap();
        assert_eq!(prepared.diagnostics.input_frames, 6);
        assert_eq!(prepared.diagnostics.output_frames, 15);
        assert!(prepared.diagnostics.inferred_frames > 0);
        assert!(group.lock().unwrap().is_empty());
        prepared.format_mp4 = true;
        let output = directory.path().join("output.mp4");
        let args = vec![
            "-y".into(),
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            source.to_string_lossy().into_owned(),
            "-map".into(),
            format!("0:{}", probe.video_stream_index),
            "-map".into(),
            format!("0:{}", probe.audio_stream_index.unwrap()),
            "-c:v".into(),
            "libx264".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-c:a".into(),
            "aac".into(),
            output.to_string_lossy().into_owned(),
        ];
        let rewritten = prepared.rewrite_args(&args, probe.video_stream_index, true);
        assert!(
            Command::new(&ffmpeg)
                .args(&rewritten)
                .status()
                .unwrap()
                .success()
        );
        let result = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-show_entries",
                "stream=nb_read_frames,codec_type,r_frame_rate",
                "-of",
                "json",
            ])
            .arg(&output)
            .output()
            .unwrap();
        assert!(result.status.success());
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let streams = json["streams"].as_array().unwrap();
        let video = streams
            .iter()
            .find(|stream| stream["codec_type"] == "video")
            .unwrap();
        assert_eq!(video["nb_read_frames"], "15");
        assert_eq!(video["r_frame_rate"], "60/1");
        assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
        prepared.identity.verify(&source).unwrap();
        for (speed, format) in [
            (2.0, "mp4"),
            (1.25, "mp4"),
            (0.5, "mp4"),
            (2.0, "webm"),
            (1.25, "webm"),
            (0.5, "webm"),
        ] {
            let mut next_request = request.clone();
            next_request.speed = speed;
            let next = prepare(
                &ffmpeg,
                &source,
                &probe,
                &next_request,
                &model,
                &cancel,
                &group,
                |_| {},
            )
            .unwrap();
            let output = directory.path().join(format!("speed-{speed}.{format}"));
            let codec = if format == "mp4" {
                "libx264"
            } else {
                "libvpx-vp9"
            };
            let audio = if format == "mp4" { "aac" } else { "libopus" };
            let mut args = args.clone();
            let codec_index = args.iter().position(|arg| arg == "-c:v").unwrap();
            args[codec_index + 1] = codec.into();
            let audio_index = args.iter().position(|arg| arg == "-c:a").unwrap();
            args[audio_index + 1] = audio.into();
            let end = args.len() - 1;
            args[end] = output.to_string_lossy().into_owned();
            args.splice(
                end..end,
                [
                    "-vf".into(),
                    format!("setpts=PTS/{speed}"),
                    "-af".into(),
                    format!("atempo={speed}"),
                ],
            );
            assert!(
                Command::new(&ffmpeg)
                    .args(next.rewrite_args(&args, probe.video_stream_index, format == "mp4"))
                    .status()
                    .unwrap()
                    .success()
            );
            let result = Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-select_streams",
                    "v:0",
                    "-show_frames",
                    "-show_entries",
                    "frame=best_effort_timestamp_time",
                    "-of",
                    "json",
                ])
                .arg(&output)
                .output()
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            let frames = json["frames"].as_array().unwrap();
            assert_eq!(frames.len() as u64, next.diagnostics.output_frames);
            for (index, frame) in frames.iter().enumerate() {
                let pts: f64 = frame["best_effort_timestamp_time"]
                    .as_str()
                    .unwrap()
                    .parse()
                    .unwrap();
                let tolerance = if format == "webm" { 0.0011 } else { 0.00002 };
                assert!(
                    (pts - index as f64 / 60.0).abs() < tolerance,
                    "speed {speed} {format} frame {index} PTS {pts}"
                );
            }
        }
    }

    #[test]
    fn timing_parser_uses_integer_pts_from_actual_decode() {
        assert_eq!(
            parse_showinfo(
                "[Parsed_showinfo_3 @ 0x123] n:   42 pts:  1001000000 pts_time:1.001 fmt:rgb24"
            ),
            Some((42, 1_001_000_000))
        );
        assert_eq!(parse_showinfo("frame=42 pts_time=1.001"), None);
        assert_eq!(parse_showinfo("[Parsed_showinfo_3] n:0 pts:-10"), None);
    }
    #[test]
    fn incomplete_rgb_is_rejected() {
        assert!(read_picture(&mut &b"abc"[..], &mut [0u8; 6]).is_err());
    }
    #[test]
    fn timing_producer_can_stop_while_the_bounded_receiver_remains_alive() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        sender.send(Ok((0, 0))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let producer = std::thread::spawn(move || send_timing(&sender, &stopping, Ok((1, 1))));
        std::thread::sleep(Duration::from_millis(15));
        stop.store(true, Ordering::Relaxed);
        assert!(!producer.join().unwrap());
    }

    #[test]
    fn scratch_size_counts_raw_active_and_prepared_files_together() {
        let directory = tempfile::tempdir().unwrap();
        for (name, size) in [
            ("decoded.rgb", 12),
            ("active-sequence.rgb", 7),
            ("prepared.mkv", 5),
        ] {
            fs::write(directory.path().join(name), vec![0; size]).unwrap();
        }
        assert_eq!(scratch_size(directory.path()).unwrap(), 24);
        fs::create_dir(directory.path().join("unexpected")).unwrap();
        assert!(scratch_size(directory.path()).is_err());
    }

    #[test]
    fn rewriting_video_does_not_replace_explicit_selected_audio() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mp4");
        fs::write(&source, b"inert source identity").unwrap();
        let identity = SourceIdentity::capture(&source).unwrap();
        // Source and scratch have different owners, as in the production path.
        // The held Windows source handle must close before its source directory.
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("prepared.mkv");
        let prepared = PreparedVideo {
            _directory: scratch,
            path,
            source,
            identity,
            start_s: 0.0,
            rate: Rate::for_playback(60, 1.0).unwrap(),
            diagnostics: InterpolationDiagnostics {
                target_fps: 60,
                model_id: "fixture".into(),
                model_digest: String::new(),
                worker_digest: String::new(),
                input_frames: 30,
                output_frames: 60,
                inferred_frames: 30,
                held_frames: 0,
                cut_pairs: 0,
                scratch_bytes: 0,
            },
            output_budget: 1024,
            video_stream_index: 0,
            format_mp4: true,
            loop_video: false,
            worker_path: PathBuf::new(),
            matrix: "bt709".into(),
            range: "tv".into(),
            color_tags: Vec::new(),
        };
        let args = [
            "-i",
            "source.mp4",
            "-map",
            "0:0",
            "-map",
            "0:9",
            "-c:v",
            "libx264",
            "out.mp4",
        ]
        .map(str::to_owned);
        let rewritten = prepared.rewrite_args(&args, 0, true);
        assert!(rewritten.windows(2).any(|p| p == ["-map", "1:0"]));
        assert!(rewritten.windows(2).any(|p| p == ["-map", "0:9"]));
        assert!(!rewritten.windows(2).any(|p| p == ["-map", "0:1"]));
    }

    #[test]
    fn changed_source_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.mp4");
        fs::write(&path, b"first").unwrap();
        let identity = SourceIdentity::capture(&path).unwrap();
        identity.verify(&path).unwrap();
        #[cfg(unix)]
        {
            fs::write(&path, b"different-length").unwrap();
            assert!(identity.verify(&path).is_err());
        }
        #[cfg(windows)]
        {
            assert!(fs::write(&path, b"different-length").is_err());
            identity.verify(&path).unwrap();
        }
    }
}
