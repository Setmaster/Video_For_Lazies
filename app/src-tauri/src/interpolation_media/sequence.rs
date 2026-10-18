//! Exact CFR scheduling for the LDF sequence worker. VFL owns the output clock.
use super::*;
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::ops::Range;
use std::process::ChildStdin;

const MAX_FRAMES: u64 = 1_000_000;
const NANOS: u128 = 1_000_000_000;
const COLD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const WINDOW_TIMEOUT: Duration = Duration::from_secs(10 * 60);

pub(super) struct Job<'a> {
    pub ffmpeg: &'a str,
    pub source: &'a Path,
    pub probe: &'a VideoProbe,
    pub request: &'a EncodeRequest,
    pub model: &'a ResolvedModel,
    pub cancel: &'a Arc<AtomicBool>,
    pub group: &'a ProcessGroup,
    pub identity: SourceIdentity,
    pub start_s: f64,
    pub end_s: f64,
    pub duration_ns: u64,
    pub rate: Rate,
    pub count: u64,
    pub frame_bytes: usize,
    pub directory: TempDir,
    pub path: PathBuf,
    pub quota: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    factor: u64,
    stride: u64,
    phase: u64,
}

impl Grid {
    fn derive(timestamps: &[u64], rate: Rate) -> Result<Self, String> {
        if timestamps.len() < 2 || timestamps.windows(2).any(|pair| pair[1] <= pair[0]) {
            return Err("LDF requires strictly increasing decoded timestamps.".into());
        }
        let intervals = (timestamps.len() - 1) as u128;
        let span = u128::from(timestamps[timestamps.len() - 1] - timestamps[0]);
        let period = NANOS * u128::from(rate.denominator);
        let denominator = u128::from(rate.numerator) * span;
        for factor in 2u64..=16 {
            let numerator = u128::from(factor) * period * intervals;
            let stride = (numerator + denominator / 2) / denominator;
            let Ok(stride) = u64::try_from(stride) else {
                continue;
            };
            if stride == 0 {
                continue;
            }
            let scale = u128::from(rate.numerator) * u128::from(stride);
            // Decode PTS are rounded to nanoseconds. Permit only that rounding,
            // never millisecond mux jitter or an implicit VFR resampling policy.
            let tolerance = 2 * scale;
            if timestamps.iter().enumerate().any(|(index, timestamp)| {
                let observed = u128::from(timestamp - timestamps[0]) * scale;
                let ideal = index as u128 * u128::from(factor) * period;
                observed.abs_diff(ideal) > tolerance
            }) {
                continue;
            }
            let observed_phase = u128::from(timestamps[0])
                .checked_mul(scale)
                .ok_or("LDF timestamp arithmetic overflow.")?;
            let phase = observed_phase
                .checked_add(period / 2)
                .ok_or("LDF timestamp arithmetic overflow.")?
                / period;
            let Some(ideal_phase) = phase.checked_mul(period) else {
                continue;
            };
            if observed_phase.abs_diff(ideal_phase) > tolerance {
                continue;
            }
            let Ok(phase) = u64::try_from(phase) else {
                continue;
            };
            let generated = (timestamps.len() as u64 - 1)
                .checked_mul(factor)
                .and_then(|value| value.checked_add(1))
                .ok_or("LDF frame count overflow.")?;
            if generated > MAX_FRAMES {
                return Err("LDF is limited to one million generated grid pictures per clip. Trim the clip or reduce the multiplier.".into());
            }
            return Ok(Self {
                factor,
                stride,
                phase,
            });
        }
        Err("LDF requires constant decoded frame timing and an exact 2–16× grid aligned with the trim and playback speed. This clip's timestamps or target rate cannot be represented. Use a compatible constant-frame-rate clip/trim or select a pair-based model.".into())
    }

    fn index(self, output: u64) -> i128 {
        i128::from(output) * i128::from(self.stride) - i128::from(self.phase)
    }
}

fn scenes(cuts: &[bool], count: usize) -> Vec<Range<usize>> {
    if count == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut start = 0;
    for (index, cut) in cuts.iter().enumerate() {
        if *cut {
            result.push(start..index + 1);
            start = index + 1;
        }
    }
    result.push(start..count);
    result
}

struct Monitor {
    clock: Instant,
    last: Arc<AtomicU64>,
    limit: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
}

impl Monitor {
    fn activity(&self) {
        self.last
            .store(self.clock.elapsed().as_millis() as u64, Ordering::Relaxed);
    }
    fn phase(&self, timeout: Duration) {
        self.activity();
        self.limit
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }
    fn check(&self, cancel: &AtomicBool) -> Result<(), String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("Canceled.".into());
        }
        if self.failed.load(Ordering::Relaxed) {
            return Err("LDF stopped because decoding, cold compilation or inference timed out, or total scratch storage exceeded its allowance.".into());
        }
        Ok(())
    }
}

fn watch(job: &Job<'_>) -> (Monitor, PipelineGuard) {
    let monitor = Monitor {
        clock: Instant::now(),
        last: Arc::new(AtomicU64::new(0)),
        limit: Arc::new(AtomicU64::new(IDLE_TIMEOUT.as_millis() as u64)),
        failed: Arc::new(AtomicBool::new(false)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let stop = stop.clone();
        let last = monitor.last.clone();
        let limit = monitor.limit.clone();
        let failed = monitor.failed.clone();
        let clock = monitor.clock;
        let cancel = job.cancel.clone();
        let group = job.group.clone();
        let worker = job.model.worker_path.clone();
        let directory = job.directory.path().to_path_buf();
        let quota = job.quota;
        std::thread::spawn(move || {
            let _scope = process::JobScope::enter(process::JobContext {
                cancel: cancel.clone(),
                child: group.clone(),
            });
            let mut space_check = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                let idle = (clock.elapsed().as_millis() as u64)
                    .saturating_sub(last.load(Ordering::Relaxed));
                let too_large = scratch_size(&directory).map_or(true, |size| size > quota);
                let low_space = if space_check.elapsed() >= Duration::from_secs(1) {
                    space_check = Instant::now();
                    available_space(&worker, &directory).map_or(true, |free| free < FREE_HEADROOM)
                } else {
                    false
                };
                if cancel.load(Ordering::Relaxed)
                    || idle > limit.load(Ordering::Relaxed)
                    || too_large
                    || low_space
                {
                    failed.store(!cancel.load(Ordering::Relaxed), Ordering::Relaxed);
                    process::kill_process_group(&group);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let guard = PipelineGuard {
        group: job.group.clone(),
        stop,
        watcher: Some(watcher),
        readers: Vec::new(),
    };
    (monitor, guard)
}

fn admit_write(job: &Job<'_>, incoming: u64) -> Result<(), String> {
    if scratch_size(job.directory.path())?
        .checked_add(incoming)
        .is_none_or(|size| size > job.quota)
    {
        return Err("LDF's decoded RGB, active sequence and prepared video exceed the combined 32 GiB scratch allowance. Trim the clip or reduce resolution.".into());
    }
    Ok(())
}

struct Spool {
    path: PathBuf,
    timestamps: Vec<u64>,
    cuts: Vec<bool>,
}

fn decode(
    job: &Job<'_>,
    monitor: &Monitor,
    guard: &mut PipelineGuard,
    progress: &mut dyn FnMut(f64),
    matrix: &str,
) -> Result<Spool, String> {
    let mut decoder = ManagedChild::spawn(decoder_command(
        job.ffmpeg,
        job.source,
        job.probe,
        job.start_s,
        job.end_s,
        matrix,
    ))
    .map_err(|_| "Could not start the LDF source decoder.")?;
    let mut pictures = decoder
        .stdout
        .take()
        .ok_or("Missing decoded LDF picture stream.")?;
    let errors = decoder
        .stderr
        .take()
        .ok_or("Missing decoded LDF timing stream.")?;
    let (sender, receiver) = mpsc::sync_channel(4);
    guard
        .readers
        .push(spawn_timing_reader(errors, sender, guard.stop.clone()));
    let path = job.directory.path().join("decoded.rgb");
    let mut raw = File::create(&path).map_err(|_| "Could not create LDF RGB scratch storage.")?;
    let mut timestamps = Vec::new();
    let mut cuts = Vec::new();
    let mut left = vec![0; job.frame_bytes];
    let mut next = vec![0; job.frame_bytes];
    loop {
        monitor.check(job.cancel)?;
        match receiver.recv_timeout(IDLE_TIMEOUT) {
            Ok(Ok((index, pts))) => {
                if index != timestamps.len() as u64 || pts >= job.duration_ns || index >= MAX_FRAMES
                {
                    return Err("LDF decoded timing/count does not match the retained interval or exceeds one million pictures.".into());
                }
                if timestamps.last().is_some_and(|previous| *previous >= pts) {
                    return Err("LDF requires strictly increasing decoded timestamps.".into());
                }
                read_picture(&mut pictures, &mut next)?;
                admit_write(job, job.frame_bytes as u64)?;
                raw.write_all(&next)
                    .map_err(|_| "LDF RGB scratch storage failed. Free space and retry.")?;
                if !timestamps.is_empty() {
                    cuts.push(is_scene_cut(&left, &next));
                }
                std::mem::swap(&mut left, &mut next);
                timestamps.push(pts);
                monitor.activity();
                progress(0.25 * pts as f64 / job.duration_ns as f64);
            }
            Ok(Err(error)) => return Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(_) => return Err("LDF source decoding timed out.".into()),
        }
    }
    raw.flush()
        .map_err(|_| "Could not finalize LDF RGB scratch storage.")?;
    let mut extra = [0];
    if pictures
        .read(&mut extra)
        .map_err(|_| "LDF decoder tail check failed.")?
        != 0
    {
        return Err("LDF decoder emitted pictures without matching timestamps.".into());
    }
    successful(&decoder, job.cancel, "LDF source decoding failed.")?;
    if let Some(reader) = guard.readers.pop() {
        let _ = reader.join();
    }
    drop(raw);
    Ok(Spool {
        path,
        timestamps,
        cuts,
    })
}

fn successful(child: &ManagedChild, cancel: &AtomicBool, message: &str) -> Result<(), String> {
    let result = child
        .wait_timeout(IDLE_TIMEOUT)
        .map_err(|_| "LDF process waiter failed.")?;
    if result.is_some_and(|status| status.success()) {
        Ok(())
    } else if cancel.load(Ordering::Relaxed) {
        Err("Canceled.".into())
    } else {
        Err(message.into())
    }
}

fn copy_scene(
    job: &Job<'_>,
    spool: &Spool,
    scene: &Range<usize>,
    monitor: &Monitor,
) -> Result<PathBuf, String> {
    let path = job.directory.path().join("active-sequence.rgb");
    let mut source = File::open(&spool.path).map_err(|_| "Could not open decoded LDF pictures.")?;
    source
        .seek(SeekFrom::Start(scene.start as u64 * job.frame_bytes as u64))
        .map_err(|_| "Could not seek decoded LDF pictures.")?;
    let mut output =
        File::create(&path).map_err(|_| "Could not create the active LDF sequence.")?;
    let mut remaining = scene.len() as u64 * job.frame_bytes as u64;
    let mut buffer = vec![0; 1024 * 1024];
    while remaining != 0 {
        monitor.check(job.cancel)?;
        let size = remaining.min(buffer.len() as u64) as usize;
        admit_write(job, size as u64)?;
        source
            .read_exact(&mut buffer[..size])
            .map_err(|_| "Decoded LDF pictures ended early.")?;
        output
            .write_all(&buffer[..size])
            .map_err(|_| "Active LDF sequence storage failed.")?;
        remaining -= size as u64;
        monitor.activity();
    }
    output
        .flush()
        .map_err(|_| "Could not finalize the active LDF sequence.")?;
    Ok(path)
}

struct Pixels {
    source: File,
    bytes: usize,
    count: usize,
    index: Option<usize>,
    left: Vec<u8>,
    right: Vec<u8>,
}
impl Pixels {
    fn new(spool: &Spool, bytes: usize) -> Result<Self, String> {
        Ok(Self {
            source: File::open(&spool.path).map_err(|_| "Could not open LDF source anchors.")?,
            bytes,
            count: spool.timestamps.len(),
            index: None,
            left: vec![0; bytes],
            right: vec![0; bytes],
        })
    }
    fn select(&mut self, grid: i128, factor: u64) -> Result<usize, String> {
        if self.count == 0 {
            return Err("The LDF interval has no retained source picture.".into());
        }
        let index = if grid < 0 {
            0
        } else {
            ((grid / i128::from(factor)) as usize).min(self.count - 1)
        };
        if self.index != Some(index) {
            self.source
                .seek(SeekFrom::Start(index as u64 * self.bytes as u64))
                .map_err(|_| "Could not seek an LDF source anchor.")?;
            self.source
                .read_exact(&mut self.left)
                .map_err(|_| "LDF source anchor is incomplete.")?;
            if index + 1 < self.count {
                self.source
                    .read_exact(&mut self.right)
                    .map_err(|_| "LDF next source anchor is incomplete.")?;
            }
            self.index = Some(index);
        }
        Ok(index)
    }
}

fn holds_source(
    grid: i128,
    factor: u64,
    index: usize,
    count: usize,
    cuts: &[bool],
    duplicate: bool,
) -> bool {
    grid < 0 || grid % i128::from(factor) == 0 || index + 1 == count || cuts[index] || duplicate
}

#[derive(Default)]
struct Counts {
    output: u64,
    inferred: u64,
    held: u64,
}

struct Output<'a> {
    assembled: &'a mut ChildStdin,
    pixels: Pixels,
    black: Vec<u8>,
    cuts: &'a [bool],
    directory: &'a Path,
    quota: u64,
    grid: Option<Grid>,
    rate: Rate,
    count: u64,
    leading_ns: u64,
    end_ns: u64,
    monitor: &'a Monitor,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(f64),
    counts: Counts,
}
impl Output<'_> {
    fn desired(&self) -> i128 {
        self.grid.map_or(0, |grid| grid.index(self.counts.output))
    }
    fn emit(&mut self, prediction: Option<&[u8]>) -> Result<(), String> {
        self.monitor.check(self.cancel)?;
        let grid = self.desired();
        let black = self.rate.sample_before(self.counts.output, self.leading_ns)
            || !self.rate.sample_before(self.counts.output, self.end_ns);
        let pixels = if black {
            self.counts.held += 1;
            &self.black
        } else {
            let factor = self.grid.map_or(2, |grid| grid.factor);
            let index = self.pixels.select(grid, factor)?;
            let held = self.grid.is_none()
                || holds_source(
                    grid,
                    factor,
                    index,
                    self.pixels.count,
                    self.cuts,
                    self.pixels.left == self.pixels.right,
                );
            if held {
                self.counts.held += 1;
                &self.pixels.left
            } else {
                self.counts.inferred += 1;
                prediction.ok_or("LDF returned no picture at an exact required output sample.")?
            }
        };
        let reserve = (self.black.len() as u64)
            .saturating_mul(3)
            .saturating_add(256 * 1024);
        if scratch_size(self.directory)?.saturating_add(reserve) > self.quota {
            return Err("LDF's RGB and prepared video exceed the combined scratch allowance. Trim the clip or reduce resolution.".into());
        }
        self.assembled
            .write_all(pixels)
            .map_err(|_| "Lossless LDF storage failed. Free scratch space and retry.")?;
        self.counts.output += 1;
        self.monitor.activity();
        (self.progress)(0.25 + 0.75 * self.counts.output as f64 / self.count as f64);
        Ok(())
    }
    fn before(&mut self, grid: i128) -> Result<(), String> {
        while self.counts.output < self.count && self.desired() < grid {
            self.emit(None)?;
        }
        Ok(())
    }
}

fn infer_scene(
    job: &Job<'_>,
    spool: &Spool,
    scene: &Range<usize>,
    output: &mut Output<'_>,
    guard: &mut PipelineGuard,
) -> Result<(), String> {
    let grid = output.grid.ok_or("Missing LDF sequence grid.")?;
    let start = scene.start as i128 * i128::from(grid.factor);
    let end = (scene.end - 1) as i128 * i128::from(grid.factor);
    output.before(start)?;
    if output.counts.output == output.count || output.desired() > end {
        return Ok(());
    }
    if scene.len() == 1 {
        if output.desired() == start {
            output.emit(None)?;
        }
        return Ok(());
    }
    let raw_path = copy_scene(job, spool, scene, output.monitor)?;
    let gpu = crate::interpolation_components::gpu_choice();
    if gpu != "auto" && gpu.parse::<u32>().is_err() {
        return Err("LDF requires a valid CUDA GPU selection.".into());
    }
    let mut cmd = inference_command(job.model, &gpu)?;
    cmd.arg("--sequence").arg("--frames").arg(&raw_path).args([
        "--frame-count",
        &scene.len().to_string(),
        "--width",
        &job.probe.width.to_string(),
        "--height",
        &job.probe.height.to_string(),
        "--factor",
        &grid.factor.to_string(),
    ]);
    output.monitor.phase(COLD_TIMEOUT);
    let mut worker =
        ManagedChild::spawn(cmd).map_err(|_| "Could not start LDF sequence inference.")?;
    let mut predictions = worker.stdout.take().ok_or("Missing LDF sequence output.")?;
    guard.readers.push(drain_errors(
        worker
            .stderr
            .take()
            .ok_or("Missing LDF sequence diagnostics.")?,
    ));
    let expected = (scene.len() as u64 - 1) * grid.factor + 1;
    let mut picture = vec![0; job.frame_bytes];
    for index in 0..expected {
        output.monitor.check(job.cancel)?;
        read_picture(&mut predictions, &mut picture)?;
        if index == 0 {
            output.monitor.phase(WINDOW_TIMEOUT);
        }
        output.monitor.activity();
        let position = start + i128::from(index);
        if output.counts.output < output.count {
            if output.desired() < position {
                return Err("LDF skipped an exact requested output sample.".into());
            }
            if output.desired() == position {
                output.emit(Some(&picture))?;
            }
        }
    }
    let mut extra = [0];
    if predictions
        .read(&mut extra)
        .map_err(|_| "LDF sequence tail check failed.")?
        != 0
    {
        return Err("LDF returned an extra generated picture.".into());
    }
    successful(
        &worker,
        job.cancel,
        "LDF sequence inference failed. Verify the optional model and runtime, then retry.",
    )?;
    if let Some(reader) = guard.readers.pop() {
        let _ = reader.join();
    }
    drop(predictions);
    drop(worker);
    output.monitor.phase(IDLE_TIMEOUT);
    fs::remove_file(raw_path)
        .map_err(|_| "Could not remove the owned LDF sequence scratch file.")?;
    Ok(())
}

pub(super) fn prepare(
    job: Job<'_>,
    progress: &mut dyn FnMut(f64),
) -> Result<PreparedVideo, String> {
    let _scope = process::JobScope::enter(process::JobContext {
        cancel: job.cancel.clone(),
        child: job.group.clone(),
    });
    let (monitor, mut guard) = watch(&job);
    let color = ColorDescription::from_probe(job.probe);
    let spool = decode(&job, &monitor, &mut guard, progress, color.matrix)?;
    let only_gap = job.end_s <= job.probe.video_start_s
        || job.probe.video_end_s.is_some_and(|end| end <= job.start_s);
    if spool.timestamps.is_empty() && !only_gap {
        return Err("The selected trim contains no retained pictures to interpolate. Expand the trim slightly.".into());
    }
    let grid = if spool.timestamps.len() >= 2 {
        Some(Grid::derive(&spool.timestamps, job.rate)?)
    } else {
        None
    };
    let mut assembler = ManagedChild::spawn_with_stdin(assembler_command(
        job.ffmpeg, job.probe, job.rate, &job.path,
    ))
    .map_err(|_| "Could not start lossless LDF storage.")?;
    let mut assembled = assembler
        .stdin
        .take()
        .ok_or("Missing lossless LDF storage input.")?;
    guard.readers.push(drain_errors(
        assembler
            .stderr
            .take()
            .ok_or("Missing lossless LDF diagnostics.")?,
    ));
    guard.readers.push(drain_errors(
        assembler
            .stdout
            .take()
            .ok_or("Missing lossless LDF storage output.")?,
    ));
    let counts = {
        let mut output = Output {
            assembled: &mut assembled,
            pixels: Pixels::new(&spool, job.frame_bytes)?,
            black: vec![0; job.frame_bytes],
            cuts: &spool.cuts,
            directory: job.directory.path(),
            quota: job.quota,
            grid,
            rate: job.rate,
            count: job.count,
            leading_ns: ((job.probe.video_start_s - job.start_s).max(0.0) * 1_000_000_000.0).round()
                as u64,
            end_ns: ((job.probe.video_end_s.unwrap_or(job.end_s) - job.start_s).max(0.0)
                * 1_000_000_000.0)
                .round() as u64,
            monitor: &monitor,
            cancel: job.cancel,
            progress,
            counts: Counts::default(),
        };
        if grid.is_some() {
            for scene in scenes(&spool.cuts, spool.timestamps.len()) {
                infer_scene(&job, &spool, &scene, &mut output, &mut guard)?;
            }
        }
        while output.counts.output < output.count {
            output.emit(None)?;
        }
        output.counts
    };
    drop(assembled);
    successful(
        &assembler,
        job.cancel,
        "Lossless LDF storage failed. Free scratch space and retry.",
    )?;
    job.identity.verify(job.source)?;
    fs::remove_file(&spool.path)
        .map_err(|_| "Could not remove owned LDF decoded scratch pictures.")?;
    let bytes = fs::metadata(&job.path)
        .map_err(|_| "Lossless LDF video was not created.")?
        .len();
    if bytes == 0 || bytes > job.quota {
        return Err("LDF scratch quota was exceeded or no video was stored.".into());
    }
    let remaining = MAX_SCRATCH_BYTES.saturating_sub(bytes).min(
        available_space(&job.model.worker_path, job.directory.path())?
            .saturating_sub(FREE_HEADROOM),
    );
    if remaining < FREE_HEADROOM {
        return Err("Not enough scratch headroom remains for final encoding.".into());
    }
    Ok(PreparedVideo {
        _directory: job.directory,
        path: job.path,
        source: job.source.to_path_buf(),
        identity: job.identity,
        start_s: job.start_s,
        rate: job.rate,
        output_budget: remaining / 2,
        video_stream_index: job.probe.video_stream_index,
        format_mp4: false,
        loop_video: job.request.loop_video,
        worker_path: job.model.worker_path.clone(),
        matrix: color.matrix.into(),
        range: color.range.into(),
        color_tags: color.color_tags,
        diagnostics: InterpolationDiagnostics {
            target_fps: job
                .request
                .interpolation
                .as_ref()
                .ok_or("Missing interpolation settings.")?
                .target_fps,
            model_id: job.model.model_id.clone(),
            model_digest: job.model.model_digest.clone(),
            worker_digest: job.model.worker_digest.clone(),
            input_frames: spool.timestamps.len() as u64,
            output_frames: counts.output,
            inferred_frames: counts.inferred,
            held_frames: counts.held,
            cut_pairs: spool.cuts.iter().filter(|cut| **cut).count() as u64,
            scratch_bytes: bytes,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamps(count: usize, numerator: u64, denominator: u64, start: u64) -> Vec<u64> {
        (0..count)
            .map(|index| {
                start
                    + ((index as u128 * NANOS * u128::from(denominator)
                        + u128::from(numerator) / 2)
                        / u128::from(numerator)) as u64
            })
            .collect()
    }

    #[test]
    fn exact_24_and_25_fps_clips_use_supported_dense_grids() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        let grid = Grid::derive(&timestamps(120, 24, 1, 0), rate).unwrap();
        assert_eq!(
            grid,
            Grid {
                factor: 5,
                stride: 2,
                phase: 0
            }
        );
        assert_eq!(
            (0..6).map(|frame| grid.index(frame)).collect::<Vec<_>>(),
            [0, 2, 4, 6, 8, 10]
        );
        let grid = Grid::derive(&timestamps(125, 25, 1, 0), rate).unwrap();
        assert_eq!(
            grid,
            Grid {
                factor: 12,
                stride: 5,
                phase: 0
            }
        );
        assert_eq!(grid.index(12), 60);
    }

    #[test]
    fn actual_timestamps_reject_vfr_coarse_mux_jitter_and_fractional_rates() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        for pts in [
            vec![0, 40_000_000, 85_000_000, 120_000_000],
            vec![0, 42_000_000, 83_000_000, 125_000_000],
            timestamps(100, 24_000, 1001, 0),
        ] {
            assert!(
                Grid::derive(&pts, rate)
                    .unwrap_err()
                    .contains("constant decoded frame timing")
            );
        }
        assert!(Grid::derive(&[10, 10], rate).is_err());
    }

    #[test]
    fn phase_and_speed_preserve_the_existing_output_epoch() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        let grid = Grid::derive(&timestamps(20, 24, 1, 41_666_667), rate).unwrap();
        assert_eq!(
            grid,
            Grid {
                factor: 5,
                stride: 2,
                phase: 5
            }
        );
        assert_eq!(
            (0..4).map(|frame| grid.index(frame)).collect::<Vec<_>>(),
            [-5, -3, -1, 1]
        );
        assert!(Grid::derive(&timestamps(20, 24, 1, 10_000_000), rate).is_err());
        let grid = Grid::derive(
            &timestamps(20, 30, 1, 0),
            Rate::for_playback(60, 2.0).unwrap(),
        )
        .unwrap();
        assert_eq!(
            grid,
            Grid {
                factor: 2,
                stride: 2,
                phase: 0
            }
        );
        let grid = Grid::derive(
            &timestamps(20, 30, 1, 0),
            Rate::for_playback(60, 0.5).unwrap(),
        )
        .unwrap();
        assert_eq!(
            grid,
            Grid {
                factor: 4,
                stride: 1,
                phase: 0
            }
        );
    }

    #[test]
    fn sequence_work_never_exceeds_one_million_model_pictures() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        assert!(
            Grid::derive(&timestamps(500_001, 30, 1, 0), rate)
                .unwrap_err()
                .contains("one million")
        );
        let grid = Grid::derive(&timestamps(500_000, 30, 1, 0), rate).unwrap();
        assert_eq!((500_000 - 1) * grid.factor + 1, 999_999);
    }

    #[test]
    fn cuts_anchors_duplicates_leading_samples_and_tail_have_explicit_hold_ownership() {
        let cuts = [false, true, false, true];
        assert_eq!(scenes(&cuts, 5), [0..2, 2..4, 4..5]);
        assert!(scenes(&[], 0).is_empty());
        assert!(holds_source(-1, 5, 0, 5, &cuts, false));
        assert!(holds_source(5, 5, 1, 5, &cuts, false));
        assert!(holds_source(7, 5, 1, 5, &cuts, false)); // Across the first cut.
        assert!(!holds_source(12, 5, 2, 5, &cuts, false));
        assert!(holds_source(12, 5, 2, 5, &cuts, true));
        assert!(holds_source(23, 5, 4, 5, &cuts, false)); // Final sample hold.
    }

    #[test]
    #[ignore = "requires local FFmpeg/FFprobe; no model or GPU is used"]
    fn decoded_mp4_24_and_25_fps_rgb_and_pts_share_the_exact_grid() {
        let directory = tempfile::tempdir().unwrap();
        let ffmpeg =
            std::env::var("VFL_INTERPOLATION_TEST_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
        for (fps, factor, stride) in [(24, 5, 2), (25, 12, 5)] {
            let source = directory.path().join(format!("source-{fps}.mp4"));
            let status = Command::new(&ffmpeg)
                .args([
                    "-y",
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size=64x64:rate={fps}"),
                    "-frames:v",
                    "6",
                    "-c:v",
                    "libx264",
                    "-video_track_timescale",
                    &(fps * 1000).to_string(),
                ])
                .arg(&source)
                .status()
                .unwrap();
            assert!(status.success());
            let probe = crate::video::probe_video(source.to_string_lossy().into_owned()).unwrap();
            let decoded = decoder_command(&ffmpeg, &source, &probe, 0.0, probe.duration_s, "bt601")
                .output()
                .unwrap();
            assert!(decoded.status.success());
            let pts = String::from_utf8_lossy(&decoded.stderr)
                .lines()
                .filter_map(parse_showinfo)
                .collect::<Vec<_>>();
            assert_eq!(pts.len(), 6);
            assert_eq!(decoded.stdout.len(), 6 * 64 * 64 * 3);
            assert_eq!(
                pts.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
                [0, 1, 2, 3, 4, 5]
            );
            let grid = Grid::derive(
                &pts.iter().map(|(_, pts)| *pts).collect::<Vec<_>>(),
                Rate::for_playback(60, 1.0).unwrap(),
            )
            .unwrap();
            assert_eq!(
                grid,
                Grid {
                    factor,
                    stride,
                    phase: 0
                }
            );
        }
    }

    #[test]
    fn source_anchor_reader_is_bounded_and_restores_exact_raw_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("decoded.rgb");
        fs::write(&path, [1, 2, 3, 4, 5, 6, 7, 8, 9]).unwrap();
        let spool = Spool {
            path,
            timestamps: vec![0, 1, 2],
            cuts: vec![false, false],
        };
        let mut pixels = Pixels::new(&spool, 3).unwrap();
        assert_eq!(pixels.select(-1, 2).unwrap(), 0);
        assert_eq!(pixels.left, [1, 2, 3]);
        assert_eq!(pixels.right, [4, 5, 6]);
        assert_eq!(pixels.select(3, 2).unwrap(), 1);
        assert_eq!(pixels.left, [4, 5, 6]);
        assert_eq!(pixels.select(10, 2).unwrap(), 2);
        assert_eq!(pixels.left, [7, 8, 9]);
    }
}
