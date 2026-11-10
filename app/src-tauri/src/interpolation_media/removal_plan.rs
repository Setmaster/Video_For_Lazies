//! A bounded, source-bound detector pass before automatic speed is resolved.
use super::*;
const MAX_PICTURES: usize = 1_000_000;
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_ANALYSIS_TIME: Duration = Duration::from_secs(15 * 60);
const NO_INDEX: u32 = u32::MAX;

#[derive(Clone, Copy)]
pub(super) struct PlannedSpan {
    pub start: u32,
    pub end: u32,
    pub detail: u32,
    pub flags: u32,
    pub remove_start: Option<u64>,
}
impl PlannedSpan {
    pub fn repaired(self) -> bool {
        self.flags & 1 != 0
    }
    pub fn cut(self) -> bool {
        self.flags & 2 != 0
    }
    pub fn verify(self, points: &[u64], index: u32, pts: u64) -> Result<(), String> {
        if self.start != index
            || (!points.is_empty() && points.get(index as usize) != Some(&pts))
            || (self.end != NO_INDEX && (self.end <= index || self.end as usize >= points.len()))
            || (self.end == NO_INDEX && !points.is_empty() && index as usize + 1 != points.len())
        {
            return Err("Cleanup replay lost its analyzed source span/endpoint.".into());
        }
        Ok(())
    }
}
pub(crate) struct RemovalPlan {
    identity: SourceIdentity,
    source: PathBuf,
    width: u32,
    height: u32,
    stream: u32,
    start_ns: u64,
    end_ns: u64,
    strength: u8,
    clock_facts: (u64, u64, Option<u64>),
    pub(super) points: Vec<u64>,
    pub(super) spans: Vec<PlannedSpan>,
    pub(super) map: RemovalMap,
    pub(super) diagnostics: AntiStutterDiagnostics,
    pub(super) cut_pairs: u64,
}
impl RemovalPlan {
    pub(super) fn interval_s(&self) -> (f64, f64) {
        (self.start_ns as f64 / 1e9, self.end_ns as f64 / 1e9)
    }
    pub fn has_cuts(&self) -> bool {
        !self.map.cuts().is_empty()
    }
    pub fn retained_duration_s(&self) -> f64 {
        self.map.output_duration_ns() as f64 / 1e9
    }
    pub fn verify(
        &self,
        source: &Path,
        probe: &VideoProbe,
        request: &EncodeRequest,
    ) -> Result<(), String> {
        self.identity.verify(source)?;
        let (start, end) = interval(probe, request);
        let settings = request
            .interpolation
            .as_ref()
            .ok_or("Cleanup analysis no longer matches the request.")?;
        if source != self.source
            || probe.width != self.width
            || probe.height != self.height
            || probe.video_stream_index != self.stream
            || start != self.start_ns
            || end != self.end_ns
            || !settings.anti_stutter
            || settings.anti_stutter_mode != crate::video::AntiStutterMode::Remove
            || settings.anti_stutter_strength != self.strength
            || self.clock_facts
                != (
                    probe.input_origin_s.to_bits(),
                    probe.video_start_s.to_bits(),
                    probe.video_end_s.map(f64::to_bits),
                )
        {
            return Err(
                "Cleanup analysis no longer matches the selected source, trim or settings.".into(),
            );
        }
        Ok(())
    }
}
fn interval(probe: &VideoProbe, request: &EncodeRequest) -> (u64, u64) {
    let start = request.trim.as_ref().map_or(0., |t| t.start_s.max(0.));
    let end = request
        .trim
        .as_ref()
        .and_then(|t| t.end_s)
        .unwrap_or(probe.duration_s)
        .min(probe.duration_s);
    canonical_interval(start, end)
}
fn canonical_interval(start: f64, end: f64) -> (u64, u64) {
    ((start * 1e9).round() as u64, (end * 1e9).round() as u64)
}
pub(super) fn verify_point(points: &[u64], index: usize, pts: u64) -> Result<(), String> {
    if points.get(index) != Some(&pts) {
        return Err("Decoded source timing changed after cleanup analysis.".into());
    }
    Ok(())
}
fn reserve<T>(v: &mut Vec<T>, other_bytes: usize) -> Result<(), String> {
    if v.len() >= MAX_PICTURES {
        return Err(
            "Cleanup analysis is limited to one million source pictures/spans. Use a shorter trim."
                .into(),
        );
    }
    if v.len() == v.capacity() {
        let capacity = v.capacity().max(512).saturating_mul(2).min(MAX_PICTURES);
        if capacity
            .checked_mul(std::mem::size_of::<T>())
            .and_then(|b| b.checked_add(other_bytes))
            .is_none_or(|b| b > MAX_METADATA_BYTES)
        {
            return Err(
                "Cleanup analysis exceeds its 64 MiB metadata allowance. Use a shorter trim."
                    .into(),
            );
        }
        v.try_reserve_exact(capacity - v.len())
            .map_err(|_| "Could not allocate bounded cleanup analysis metadata.".to_string())?;
    }
    Ok(())
}

fn checkpoint(cancelled: bool, timed_out: bool, elapsed: Duration) -> Result<(), String> {
    if cancelled {
        return Err("Canceled.".into());
    }
    if timed_out || elapsed > MAX_ANALYSIS_TIME {
        return Err("Cleanup analysis exceeded its time allowance.".into());
    }
    Ok(())
}

pub(crate) fn analyze_removal(
    ffmpeg: &str,
    source: &Path,
    probe: &VideoProbe,
    request: &EncodeRequest,
    cancel: &Arc<AtomicBool>,
    group: &ProcessGroup,
    mut progress: impl FnMut(f64),
) -> Result<RemovalPlan, String> {
    let settings = request
        .interpolation
        .as_ref()
        .ok_or("Missing cleanup settings.")?;
    if !settings.anti_stutter || settings.anti_stutter_mode != crate::video::AntiStutterMode::Remove
    {
        return Err("Duration analysis requires Remove repeated time.".into());
    }
    let identity = SourceIdentity::capture(source)?;
    identity.verify(source)?;
    let (start_ns, end_ns) = interval(probe, request);
    let duration = end_ns
        .checked_sub(start_ns)
        .filter(|d| *d > 0)
        .ok_or("The retained trim is empty.")?;
    let pixels = u64::from(probe.width) * u64::from(probe.height);
    if pixels == 0 || pixels > MAX_PIXELS || probe.width > 8192 || probe.height > 8192 {
        return Err("Cleanup analysis requires a supported interpolation picture size.".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let last = Arc::new(AtomicU64::new(0));
    let clock = Instant::now();
    let watcher = {
        let stop = stop.clone();
        let timed_out = timed_out.clone();
        let last = last.clone();
        let group = group.clone();
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed)
                    || clock.elapsed() > MAX_ANALYSIS_TIME
                    || (clock.elapsed().as_millis() as u64)
                        .saturating_sub(last.load(Ordering::Relaxed))
                        > IDLE_TIMEOUT.as_millis() as u64
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
    let color = ColorDescription::from_probe(probe);
    let mut decoder = ManagedChild::spawn(decoder_command(
        ffmpeg,
        source,
        probe,
        start_ns as f64 / 1e9,
        end_ns as f64 / 1e9,
        color.matrix,
    ))
    .map_err(|_| "Could not start cleanup analysis decoder.".to_string())?;
    let mut decoded = decoder
        .stdout
        .take()
        .ok_or("Missing cleanup picture stream.")?;
    let (sender, receiver) = mpsc::sync_channel(4);
    guard.readers.push(spawn_timing_reader(
        decoder
            .stderr
            .take()
            .ok_or("Missing cleanup timing stream.")?,
        sender,
        guard.stop.clone(),
    ));
    let mut points = Vec::new();
    let mut spans = Vec::new();
    let mut read = |frame: &mut [u8]| -> Result<Option<u64>, String> {
        process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
        match receiver.recv_timeout(IDLE_TIMEOUT) {
            Ok(Ok((index, pts))) => {
                if timed_out.load(Ordering::Relaxed) {
                    return Err("Cleanup analysis exceeded its time allowance.".into());
                }
                if index != points.len() as u64
                    || pts >= duration
                    || points.last().is_some_and(|t| *t >= pts)
                {
                    return Err("Cleanup analysis received inconsistent picture timing.".into());
                }
                reserve(
                    &mut points,
                    MAX_PICTURES * std::mem::size_of::<PlannedSpan>(),
                )?;
                read_picture(&mut decoded, frame)?;
                points.push(pts);
                last.store(clock.elapsed().as_millis() as u64, Ordering::Relaxed);
                Ok(Some(pts))
            }
            Ok(Err(e)) => Err(e),
            Err(mpsc::RecvTimeoutError::Disconnected) => Ok(None),
            Err(_) => Err("Cleanup analysis decoder timed out.".into()),
        }
    };
    let mut first = vec![0; (pixels * 3) as usize];
    let pts = read(&mut first)?;
    let only_gap = end_ns as f64 / 1e9 <= probe.video_start_s
        || probe
            .video_end_s
            .is_some_and(|end| end <= start_ns as f64 / 1e9);
    let first_pts = match pts {
        Some(p) => p,
        None if only_gap => 0,
        None => return Err("The selected trim has no retained pictures.".into()),
    };
    let mut window = anti_stutter::Window::new(
        first,
        first_pts,
        probe.width as usize,
        probe.height as usize,
        duration,
        pts.is_some(),
        settings.anti_stutter_strength,
    )
    .at_source_beginning(
        start_ns as f64 / 1e9 <= probe.video_start_s
            && probe.video_start_s <= start_ns as f64 / 1e9
            && pts == Some(0),
    )
    .with_mode(settings.anti_stutter_mode);
    let mut map = RemovalMap::new(duration);
    let mut consumed = 0u32;
    while let Some(span) = window.next(&mut read)? {
        checkpoint(
            cancel.load(Ordering::Relaxed),
            timed_out.load(Ordering::Relaxed),
            clock.elapsed(),
        )?;
        process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
        let end = span.right.map(|i| consumed + i as u32);
        if let Some(start) = span.remove_start {
            map.push(
                start,
                window.pts(span.right.ok_or("Cleanup span has no endpoint.")?),
            )?;
        }
        reserve(&mut spans, MAX_PICTURES * std::mem::size_of::<u64>())?;
        spans.push(PlannedSpan {
            start: consumed,
            end: end.unwrap_or(NO_INDEX),
            detail: span.detail.map_or(NO_INDEX, |i| i as u32),
            flags: u32::from(span.repaired) | (u32::from(span.cut) << 1),
            remove_start: span.remove_start,
        });
        let logical_fill = u64::from(span.repaired && span.remove_start.is_none());
        let pos = span.right.map_or(duration, |i| window.pts(i));
        window.advance(span, logical_fill);
        progress(pos as f64 / duration as f64);
        if let Some(end) = end {
            consumed = end;
        }
    }
    let mut extra = [0];
    if decoded
        .read(&mut extra)
        .map_err(|_| "Cleanup decoder tail check failed.")?
        != 0
    {
        return Err("Cleanup decoder emitted unindexed pictures.".into());
    }
    let status = decoder
        .wait_timeout(IDLE_TIMEOUT)
        .map_err(|_| "Cleanup decoder waiter failed.")?;
    if !status.is_some_and(|s| s.success()) {
        return Err(if cancel.load(Ordering::Relaxed) {
            "Canceled.".into()
        } else {
            "Cleanup analysis decoder failed or timed out.".into()
        });
    }
    checkpoint(
        cancel.load(Ordering::Relaxed),
        timed_out.load(Ordering::Relaxed),
        clock.elapsed(),
    )?;
    identity.verify(source)?;
    Ok(RemovalPlan {
        identity,
        source: source.to_path_buf(),
        width: probe.width,
        height: probe.height,
        stream: probe.video_stream_index,
        start_ns,
        end_ns,
        strength: settings.anti_stutter_strength,
        clock_facts: (
            probe.input_origin_s.to_bits(),
            probe.video_start_s.to_bits(),
            probe.video_end_s.map(f64::to_bits),
        ),
        points,
        spans,
        map,
        diagnostics: window.diagnostics,
        cut_pairs: window.cut_pairs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_plan_deadline_or_cancel_cannot_accept_completed_decoder() {
        assert!(checkpoint(false, false, Duration::from_secs(1)).is_ok());
        assert!(checkpoint(false, true, Duration::from_secs(1)).is_err());
        assert!(checkpoint(false, false, MAX_ANALYSIS_TIME + Duration::from_millis(1)).is_err());
        assert!(
            checkpoint(true, false, Duration::ZERO)
                .unwrap_err()
                .contains("Canceled")
        );
    }
    #[test]
    fn cleanup_plan_preserves_canonical_ns_endpoints_and_exact_replay_bounds() {
        let (a, b) = canonical_interval(0.0000000006, 0.0000000024);
        assert_eq!((a, b, b - a), (1, 2, 1));
        assert_eq!(
            ((0.0000000024f64 - 0.0000000006) * 1e9).round() as u64,
            2,
            "rounding a difference is a different clock"
        );
        assert_eq!(RemovalMap::new(b - a).source_duration_ns(), 1);
        let points = [0, 10, 20];
        let span = PlannedSpan {
            start: 0,
            end: 2,
            detail: NO_INDEX,
            flags: 0,
            remove_start: None,
        };
        assert!(span.verify(&points, 0, 0).is_ok());
        assert!(span.verify(&points, 0, 1).is_err());
        assert!(span.verify(&points, 1, 10).is_err());
        assert!(
            PlannedSpan { end: 3, ..span }
                .verify(&points, 0, 0)
                .is_err()
        );
        assert!(
            PlannedSpan {
                end: NO_INDEX,
                ..span
            }
            .verify(&points, 0, 0)
            .is_err()
        );
        assert!(
            PlannedSpan {
                start: 2,
                end: NO_INDEX,
                ..span
            }
            .verify(&points, 2, 20)
            .is_ok()
        );
        assert!(verify_point(&points, 1, 10).is_ok());
        assert!(verify_point(&points, 1, 11).is_err());
        assert!(verify_point(&points, 3, 30).is_err());
    }
    #[test]
    fn cleanup_plan_quota_rejects_before_allocation() {
        let mut spans = Vec::<PlannedSpan>::new();
        assert!(reserve(&mut spans, MAX_METADATA_BYTES).is_err());
        assert_eq!(spans.capacity(), 0);
        reserve(&mut spans, MAX_PICTURES * std::mem::size_of::<u64>()).unwrap();
        assert!(
            spans.capacity() * std::mem::size_of::<PlannedSpan>() + MAX_PICTURES * 8
                <= MAX_METADATA_BYTES
        );
    }
}
