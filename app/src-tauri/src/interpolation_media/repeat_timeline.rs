//! One trim-relative clock for removing repeated video time and matching audio.
use super::*;

const NS_PER_SECOND: u64 = 1_000_000_000;
const MAX_CUTS: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TimeCut {
    pub start_ns: u64,
    pub end_ns: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct RemovalMap {
    duration_ns: u64,
    cuts: Vec<TimeCut>,
    // Removed duration through each corresponding cut, inclusive.
    removed_prefix_ns: Vec<u64>,
}

impl RemovalMap {
    pub fn new(source_duration_ns: u64) -> Self {
        Self {
            duration_ns: source_duration_ns,
            cuts: Vec::new(),
            removed_prefix_ns: Vec::new(),
        }
    }

    /// Add a complete half-open cut in source presentation order. A hold A@a,
    /// A@p, B@b cuts [p,b), retaining the first copy's interval [a,p).
    pub fn push(&mut self, start_ns: u64, end_ns: u64) -> Result<(), String> {
        if start_ns >= end_ns || end_ns > self.duration_ns {
            return Err("Repeated-time removal has an invalid source interval.".into());
        }
        if self.cuts.last().is_some_and(|cut| start_ns < cut.end_ns) {
            return Err("Repeated-time removal intervals overlap or are out of order.".into());
        }
        // Adjacent intervals form one complete cut, avoiding an artificial
        // sample seam and keeping the bounded map independent of diagnostics.
        if let Some(last) = self.cuts.last_mut()
            && last.end_ns == start_ns
        {
            last.end_ns = end_ns;
            *self.removed_prefix_ns.last_mut().unwrap() += end_ns - start_ns;
            return Ok(());
        }
        if self.cuts.len() == MAX_CUTS {
            return Err(
                "Repeated-time removal exceeds 65,536 complete intervals. Use a shorter trim."
                    .into(),
            );
        }
        let removed = self.removed_duration_ns() + end_ns - start_ns;
        self.cuts.push(TimeCut { start_ns, end_ns });
        self.removed_prefix_ns.push(removed);
        Ok(())
    }

    pub fn source_duration_ns(&self) -> u64 {
        self.duration_ns
    }

    pub fn removed_duration_ns(&self) -> u64 {
        self.removed_prefix_ns.last().copied().unwrap_or(0)
    }

    pub fn output_duration_ns(&self) -> u64 {
        self.duration_ns - self.removed_duration_ns()
    }

    pub fn cuts(&self) -> &[TimeCut] {
        &self.cuts
    }

    /// Points inside a removed half-open interval clamp to its output seam.
    pub fn map_ns(&self, source_ns: u64) -> u64 {
        let source_ns = source_ns.min(self.duration_ns);
        let index = self.cuts.partition_point(|cut| cut.start_ns <= source_ns);
        if index == 0 {
            return source_ns;
        }
        let cut = self.cuts[index - 1];
        let previous = if index > 1 {
            self.removed_prefix_ns[index - 2]
        } else {
            0
        };
        source_ns - previous - (source_ns - cut.start_ns).min(cut.end_ns - cut.start_ns)
    }

    /// Each cut starts at round(source_start * rate). Its length is the
    /// difference between rounded cumulative removed durations, so thousands
    /// of fractional cuts cannot accumulate independent rounding drift. The
    /// output seam differs from the ideal nanosecond map by at most one sample.
    pub fn sample_map(&self, sample_rate: u32) -> Result<SampleMap, String> {
        if sample_rate == 0 {
            return Err("Repeated-time audio has no valid sample rate.".into());
        }
        let source_samples = rounded_samples(self.duration_ns, sample_rate)?;
        let mut cuts = Vec::with_capacity(self.cuts.len());
        let mut previous_removed = 0;
        let mut previous_end = 0;
        for (cut, prefix) in self.cuts.iter().zip(&self.removed_prefix_ns) {
            let start_sample = rounded_samples(cut.start_ns, sample_rate)?;
            let removed = rounded_samples(*prefix, sample_rate)?;
            let end_sample = start_sample
                .checked_add(removed - previous_removed)
                .ok_or("Repeated-time audio sample count overflowed.")?;
            if start_sample < previous_end || end_sample > source_samples {
                return Err("Repeated-time audio intervals cannot be represented without overlapping sample boundaries.".into());
            }
            if end_sample > start_sample {
                cuts.push(SampleCut {
                    start_sample,
                    end_sample,
                });
            }
            previous_removed = removed;
            previous_end = end_sample;
        }
        Ok(SampleMap {
            source_samples,
            output_samples: source_samples - previous_removed,
            cuts,
        })
    }
}

fn rounded_samples(ns: u64, sample_rate: u32) -> Result<u64, String> {
    u64::try_from(
        (u128::from(ns) * u128::from(sample_rate) + 500_000_000) / u128::from(NS_PER_SECOND),
    )
    .map_err(|_| "Repeated-time audio sample count overflowed.".into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SampleCut {
    pub start_sample: u64,
    pub end_sample: u64,
}

#[derive(Debug)]
pub(crate) struct SampleMap {
    pub source_samples: u64,
    pub output_samples: u64,
    pub cuts: Vec<SampleCut>,
}

pub(crate) struct PreparedAudio {
    pub path: PathBuf,
    pub sample_rate: u32,
    pub channels: u32,
    pub channel_layout: String,
    pub source_stream_index: u32,
    pub sample_count: u64,
}

fn audio_layout(probe: &VideoProbe, channels: u32) -> Result<String, String> {
    match probe.audio_channel_layout.as_deref() {
        Some(layout) if !layout.is_empty() && layout != "unknown" => Ok(layout.into()),
        _ if channels == 1 => Ok("mono".into()),
        _ if channels == 2 => Ok("stereo".into()),
        _ => Err("Repeated-time removal needs a known source audio channel layout. Select another audio track.".into()),
    }
}

fn audio_decoder_command(
    ffmpeg: &str,
    source: &Path,
    audio: &PreparedAudio,
    start_s: f64,
    map: &RemovalMap,
    sample_count: u64,
) -> Command {
    let sample_rate = audio.sample_rate;
    let stream_index = audio.source_stream_index;
    let end_s = start_s + map.source_duration_ns() as f64 / NS_PER_SECOND as f64;
    // PTS-start, rather than STARTPTS, retains a selected track's real delay.
    // first_pts inserts its leading silence; pad/trim pins the source interval.
    let filters = format!(
        "atrim=start={start_s:.9}:end={end_s:.9},asetpts=PTS-{start_s:.9}/TB,aresample=async=1:first_pts=0:osr={sample_rate}:osf=dbl,apad=whole_len={sample_count},atrim=end_sample={sample_count}"
    );
    let mut decoder = command(Path::new(ffmpeg));
    decoder
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
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
            &format!("0:{stream_index}"),
            "-vn",
            "-sn",
            "-dn",
            "-af",
            &filters,
            "-ar",
            &sample_rate.to_string(),
            "-channel_layout",
            &audio.channel_layout,
            "-c:a",
            "pcm_f64le",
            "-f",
            "f64le",
            "pipe:1",
        ]);
    decoder
}

/// Called only when requested audio is enabled. The caller retains its outer
/// PipelineGuard and updates the watchdog in `progress`, including during audio.
/// ManagedChild inherits the job's cancellation/process group.
#[allow(clippy::too_many_arguments)] // One shared job quota and watchdog cover all stages.
pub(crate) fn prepare_audio(
    ffmpeg: &str,
    source: &Path,
    probe: &VideoProbe,
    start_s: f64,
    map: &RemovalMap,
    directory: &Path,
    quota_bytes: u64,
    mut progress: impl FnMut() -> Result<(), String>,
) -> Result<Option<PreparedAudio>, String> {
    if map.cuts().is_empty() || !probe.has_audio {
        return Ok(None);
    }
    let stream_index = probe
        .audio_stream_index
        .ok_or("The selected audio stream is unavailable.")?;
    let sample_rate = probe
        .audio_sample_rate
        .filter(|rate| *rate > 0)
        .ok_or("Repeated-time removal needs a known source audio sample rate.")?;
    let channels = probe
        .audio_channels
        .filter(|channels| *channels > 0)
        .ok_or("Repeated-time removal needs a known source audio channel count.")?;
    if !start_s.is_finite() || start_s < 0.0 {
        return Err("Repeated-time audio has an invalid source start.".into());
    }
    let layout = audio_layout(probe, channels)?;
    let samples = map.sample_map(sample_rate)?;
    let frame_bytes = usize::try_from(channels)
        .ok()
        .and_then(|count| count.checked_mul(8))
        .filter(|size| *size <= 1_048_576)
        .ok_or("Repeated-time audio channel count is too large.")?;
    let bytes = samples
        .output_samples
        .checked_mul(frame_bytes as u64)
        .ok_or("Repeated-time audio scratch size overflowed.")?;
    if scratch_size(directory)?
        .checked_add(bytes)
        .is_none_or(|total| total > quota_bytes.min(MAX_SCRATCH_BYTES))
    {
        return Err("Repeated-time audio exceeds the shared interpolation scratch quota. Use a shorter trim.".into());
    }
    let path = directory.join("audio.raw");
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| "Could not create repeated-time audio storage.".to_string())?;
    let prepared = PreparedAudio {
        path: path.clone(),
        sample_rate,
        channels,
        channel_layout: layout,
        source_stream_index: stream_index,
        sample_count: samples.output_samples,
    };
    let result = (|| {
        progress()?;
        let end_s = start_s + map.source_duration_ns() as f64 / NS_PER_SECOND as f64;
        if probe.audio_start_s.is_some_and(|start| start >= end_s)
            || probe.audio_end_s.is_some_and(|end| end <= start_s)
        {
            // A known gap is silence on the common source clock. FFmpeg's
            // apad receives no frames at all for a wholly disjoint interval.
            let buffer = [0u8; 65_536];
            let mut remaining = bytes;
            while remaining > 0 {
                process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
                progress()?;
                let count = remaining.min(buffer.len() as u64) as usize;
                output
                    .write_all(&buffer[..count])
                    .map_err(|_| "Could not store repeated-time audio silence.".to_string())?;
                remaining -= count as u64;
            }
            progress()?;
            return output
                .flush()
                .map_err(|_| "Could not finish repeated-time audio storage.".to_string());
        }
        let mut child = ManagedChild::spawn(audio_decoder_command(
            ffmpeg,
            source,
            &prepared,
            start_s,
            map,
            samples.source_samples,
        ))
        .map_err(|_| "Could not start repeated-time audio decoding.".to_string())?;
        let mut decoded = child
            .stdout
            .take()
            .ok_or("Missing repeated-time audio output.")?;
        let errors = drain_errors(
            child
                .stderr
                .take()
                .ok_or("Missing repeated-time audio diagnostics.")?,
        );
        let copy_result = compact_samples(
            &mut decoded,
            &mut output,
            &samples,
            frame_bytes,
            &mut progress,
        );
        if copy_result.is_err() {
            let _ = child.control.kill();
        }
        let status = child.wait_timeout(IDLE_TIMEOUT);
        if !matches!(&status, Ok(Some(_))) {
            let _ = child.control.kill();
        }
        let _ = errors.join();
        copy_result?;
        if !status.is_ok_and(|status| status.is_some_and(|status| status.success())) {
            return Err(
                "Repeated-time audio decoding failed or timed out. No output was published.".into(),
            );
        }
        progress()?;
        output
            .flush()
            .map_err(|_| "Could not finish repeated-time audio storage.".to_string())?;
        Ok(())
    })();
    drop(output);
    if let Err(error) = result {
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(Some(prepared))
}

fn compact_samples(
    decoded: &mut impl Read,
    output: &mut impl Write,
    samples: &SampleMap,
    frame_bytes: usize,
    progress: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let chunk_samples = (65_536 / frame_bytes).max(1);
    let mut buffer = vec![0u8; chunk_samples * frame_bytes];
    let mut source_sample = 0u64;
    let mut written = 0u64;
    let mut cut_index = 0;
    while source_sample < samples.source_samples {
        process::check_cancelled().map_err(|_| "Canceled.".to_string())?;
        progress()?;
        while samples
            .cuts
            .get(cut_index)
            .is_some_and(|cut| cut.end_sample <= source_sample)
        {
            cut_index += 1;
        }
        let cut = samples.cuts.get(cut_index);
        let dropping = cut.is_some_and(|cut| cut.start_sample <= source_sample);
        let boundary = cut.map_or(samples.source_samples, |cut| {
            if dropping {
                cut.end_sample
            } else {
                cut.start_sample
            }
        });
        let count = (boundary - source_sample).min(chunk_samples as u64) as usize;
        let bytes = &mut buffer[..count * frame_bytes];
        decoded.read_exact(bytes).map_err(|_| {
            "Repeated-time audio ended before its complete source interval.".to_string()
        })?;
        if !dropping {
            output.write_all(bytes).map_err(|_| {
                "Repeated-time audio storage failed. Free scratch space and retry.".to_string()
            })?;
            written += count as u64;
        }
        source_sample += count as u64;
    }
    progress()?;
    let mut tail = [0u8; 1];
    if decoded
        .read(&mut tail)
        .map_err(|_| "Repeated-time audio decoding failed.".to_string())?
        != 0
        || written != samples.output_samples
    {
        return Err("Repeated-time audio did not match its declared sample interval.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(channels: u32, layout: Option<&str>) -> VideoProbe {
        crate::video::parse_probe_output(&serde_json::json!({
            "format":{"duration":"2.5","start_time":"0"},
            "streams":[
                {"index":0,"codec_type":"video","codec_name":"ffv1","width":16,"height":16,"r_frame_rate":"10/1"},
                {"index":1,"codec_type":"audio","codec_name":"pcm_f64le","sample_rate":"48000","channels":channels,"channel_layout":layout,"sample_fmt":"dbl"}
            ]
        }).to_string()).unwrap()
    }

    #[test]
    fn one_copy_irregular_vfr_and_seam_endpoints() {
        let mut map = RemovalMap::new(1_000);
        map.push(173, 601).unwrap(); // A@101, first repeat@173, B@601
        map.push(800, 900).unwrap();
        for (source, output) in [
            (0, 0),
            (101, 101),
            (172, 172),
            (173, 173),
            (600, 173),
            (601, 173),
            (602, 174),
            (800, 372),
            (899, 372),
            (900, 372),
            (1_000, 472),
            (2_000, 472),
        ] {
            assert_eq!(map.map_ns(source), output);
        }
        assert_eq!(map.removed_duration_ns(), 528);
        assert_eq!(map.output_duration_ns(), 472);
    }

    #[test]
    fn invalid_cuts_do_not_mutate_and_adjacent_cuts_merge() {
        let mut map = RemovalMap::new(100);
        for cut in [(1, 1), (4, 3), (1, 101)] {
            assert!(map.push(cut.0, cut.1).is_err());
        }
        map.push(20, 40).unwrap();
        assert!(map.push(30, 50).is_err());
        assert!(map.push(0, 10).is_err());
        map.push(40, 50).unwrap();
        assert_eq!(
            map.cuts(),
            &[TimeCut {
                start_ns: 20,
                end_ns: 50
            }]
        );
        assert_eq!(map.removed_duration_ns(), 30);
    }

    #[test]
    fn full_map_cap_is_independent_of_diagnostic_limit() {
        let mut map = RemovalMap::new(200_000);
        for index in 0..MAX_CUTS {
            map.push(index as u64 * 2, index as u64 * 2 + 1).unwrap();
        }
        assert_eq!(map.cuts().len(), MAX_CUTS);
        assert!(map.push(150_000, 150_001).is_err());
        assert_eq!(map.map_ns(200_000), 200_000 - MAX_CUTS as u64);
    }

    #[test]
    fn fractional_cuts_round_cumulative_removal_without_drift() {
        let mut map = RemovalMap::new(100_000_000_000);
        for index in 0..10_000u64 {
            map.push(
                index * 10_000_000 + 1_234_567,
                index * 10_000_000 + 1_267_900,
            )
            .unwrap();
        }
        for rate in [8_000, 44_100, 48_000, 192_000] {
            let samples = map.sample_map(rate).unwrap();
            let mut removed = 0u64;
            for (index, cut) in samples.cuts.iter().enumerate() {
                removed += cut.end_sample - cut.start_sample;
                // At these rates every cut has at least one sample except
                // 8 kHz; verify each represented prefix below independently.
                if rate >= 44_100 {
                    assert_eq!(
                        removed,
                        rounded_samples(map.removed_prefix_ns[index], rate).unwrap()
                    );
                }
            }
            assert_eq!(
                removed,
                rounded_samples(map.removed_duration_ns(), rate).unwrap()
            );
            assert_eq!(samples.source_samples - samples.output_samples, removed);
            let mut before = 0;
            let mut cut_index = 0;
            for source in &map.cuts {
                let start_sample = rounded_samples(source.start_ns, rate).unwrap();
                if let Some(cut) = samples.cuts.get(cut_index)
                    && cut.start_sample == start_sample
                {
                    assert!(
                        (i128::from(cut.start_sample - before)
                            - i128::from(
                                rounded_samples(map.map_ns(source.start_ns), rate).unwrap()
                            ))
                        .abs()
                            <= 1
                    );
                    before += cut.end_sample - cut.start_sample;
                    cut_index += 1;
                }
            }
        }
    }

    #[test]
    fn streaming_compactor_retains_markers_and_all_channels_exactly() {
        let mut map = RemovalMap::new(NS_PER_SECOND);
        map.push(200_000_000, 400_000_000).unwrap();
        map.push(600_000_000, 700_000_000).unwrap();
        let samples = map.sample_map(10).unwrap();
        let input: Vec<u8> = (0..10u64)
            .flat_map(|sample| [sample as f64, -(sample as f64)])
            .flat_map(f64::to_le_bytes)
            .collect();
        let mut output = Vec::new();
        let mut callbacks = 0;
        compact_samples(&mut &input[..], &mut output, &samples, 16, &mut || {
            callbacks += 1;
            Ok(())
        })
        .unwrap();
        let values: Vec<f64> = output
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(
            values,
            vec![0., 0., 1., -1., 4., -4., 5., -5., 7., -7., 8., -8., 9., -9.]
        );
        assert!(callbacks > 0);
    }

    #[test]
    fn no_cuts_never_decode_and_unknown_multichannel_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let probe = probe(6, None);
        assert!(
            prepare_audio(
                "not-a-program",
                Path::new("absent"),
                &probe,
                0.,
                &RemovalMap::new(NS_PER_SECOND),
                directory.path(),
                MAX_SCRATCH_BYTES,
                || panic!("unexpected decoding")
            )
            .unwrap()
            .is_none()
        );
        assert!(audio_layout(&probe, 6).is_err());
        assert_eq!(audio_layout(&probe, 1).unwrap(), "mono");
        assert_eq!(audio_layout(&probe, 2).unwrap(), "stereo");
    }

    #[test]
    fn wholly_disjoint_audio_materializes_exact_shortened_silence() {
        for (audio_start, audio_end) in [(Some(2.), Some(3.)), (Some(0.), Some(0.1))] {
            let directory = tempfile::tempdir().unwrap();
            let mut probe = probe(2, Some("stereo"));
            probe.audio_start_s = audio_start;
            probe.audio_end_s = audio_end;
            let mut map = RemovalMap::new(NS_PER_SECOND);
            map.push(200_000_000, 400_000_000).unwrap();
            let audio = prepare_audio(
                "not-a-program",
                Path::new("absent"),
                &probe,
                0.5,
                &map,
                directory.path(),
                MAX_SCRATCH_BYTES,
                || Ok(()),
            )
            .unwrap()
            .unwrap();
            let bytes = fs::read(audio.path).unwrap();
            assert_eq!(bytes.len(), 38_400 * 2 * 8);
            assert!(bytes.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn sample_overlap_and_truncated_decoder_fail_closed() {
        let mut map = RemovalMap::new(NS_PER_SECOND);
        map.push(51_000_000, 102_000_000).unwrap();
        map.push(103_000_000, 154_000_000).unwrap();
        assert!(map.sample_map(10).is_err());
        let map = RemovalMap::new(NS_PER_SECOND).sample_map(10).unwrap();
        assert!(
            compact_samples(
                &mut &[0u8; 71][..],
                &mut Vec::new(),
                &map,
                8,
                &mut || Ok(())
            )
            .is_err()
        );
        assert!(
            compact_samples(
                &mut &[0u8; 81][..],
                &mut Vec::new(),
                &map,
                8,
                &mut || Ok(())
            )
            .is_err()
        );
    }

    #[test]
    fn callback_failure_prevents_sample_reads_and_stops_partial_silence() {
        let samples = RemovalMap::new(NS_PER_SECOND).sample_map(10).unwrap();
        let input = [0u8; 80];
        let mut reader = &input[..];
        let mut output = Vec::new();
        let error = compact_samples(&mut reader, &mut output, &samples, 8, &mut || {
            Err("quota stopped".into())
        })
        .unwrap_err();
        assert_eq!(error, "quota stopped");
        assert_eq!(reader.len(), input.len());
        assert!(output.is_empty());

        let directory = tempfile::tempdir().unwrap();
        let mut probe = probe(2, Some("stereo"));
        probe.audio_start_s = Some(2.);
        let mut map = RemovalMap::new(NS_PER_SECOND);
        map.push(200_000_000, 400_000_000).unwrap();
        let mut calls = 0;
        let result = prepare_audio(
            "not-a-program",
            Path::new("absent"),
            &probe,
            0.,
            &map,
            directory.path(),
            MAX_SCRATCH_BYTES,
            || {
                calls += 1;
                if calls >= 3 {
                    assert!(
                        fs::metadata(directory.path().join("audio.raw"))
                            .unwrap()
                            .len()
                            > 0
                    );
                    Err("watchdog stopped".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result,Err(error) if error=="watchdog stopped"));
        assert!(!directory.path().join("audio.raw").exists());
    }

    #[test]
    fn actual_job_quota_includes_existing_scratch() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("video.raw"), [0u8; 16]).unwrap();
        let mut map = RemovalMap::new(NS_PER_SECOND);
        map.push(200_000_000, 400_000_000).unwrap();
        let result = prepare_audio(
            "not-a-program",
            Path::new("absent"),
            &probe(2, Some("stereo")),
            0.,
            &map,
            directory.path(),
            38_400 * 2 * 8 + 15,
            || panic!("quota should reject before processing"),
        );
        assert!(matches!(result,Err(error) if error.contains("scratch quota")));
        assert!(!directory.path().join("audio.raw").exists());
    }

    #[test]
    #[ignore = "requires pinned FFmpeg; no interpolation model or GPU"]
    fn pinned_audio_preserves_delay_trim_markers_and_six_channel_precision() {
        let ffmpeg =
            std::env::var("VFL_INTERPOLATION_TEST_FFMPEG").expect("set pinned FFmpeg path");
        let directory = tempfile::tempdir().unwrap();
        let raw = directory.path().join("source.raw");
        let source = directory.path().join("source.mov");
        let mut raw_file = fs::File::create(&raw).unwrap();
        for sample in 0..96_000u64 {
            for channel in 0..6 {
                raw_file
                    .write_all(&(sample as f64 / 100_000. + channel as f64 / 10.).to_le_bytes())
                    .unwrap();
            }
        }
        drop(raw_file);
        let result = command(Path::new(&ffmpeg))
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=16x16:r=10:d=2.5",
                "-f",
                "f64le",
                "-ar",
                "48000",
                "-ac",
                "6",
                "-channel_layout",
                "5.1",
                "-i",
            ])
            .arg(&raw)
            .args([
                "-af",
                "asetpts=PTS+0.25/TB",
                "-c:v",
                "mpeg4",
                "-c:a",
                "pcm_f64le",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(result.status.success(), "fixture mux failed");
        let ffprobe = Path::new(&ffmpeg).parent().unwrap().join(if cfg!(windows) {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        let actual = command(&ffprobe)
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_format",
                "-of",
                "json",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(actual.status.success());
        let probe =
            crate::video::parse_probe_output(std::str::from_utf8(&actual.stdout).unwrap()).unwrap();
        assert_eq!(probe.audio_channel_layout.as_deref(), Some("5.1"));
        assert_eq!(probe.audio_start_s, Some(0.25));
        let mut map = RemovalMap::new(2_000_000_000);
        map.push(400_000_000, 600_000_000).unwrap();
        let prepared = prepare_audio(
            &ffmpeg,
            &source,
            &probe,
            0.1,
            &map,
            directory.path(),
            MAX_SCRATCH_BYTES,
            || Ok(()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(prepared.sample_count, 86_400);
        assert_eq!(prepared.channels, 6);
        assert_eq!(prepared.channel_layout, "5.1");
        let bytes = fs::read(prepared.path).unwrap();
        assert_eq!(bytes.len(), 86_400 * 6 * 8);
        let value = |sample: usize, channel: usize| {
            f64::from_le_bytes(
                bytes[(sample * 6 + channel) * 8..(sample * 6 + channel + 1) * 8]
                    .try_into()
                    .unwrap(),
            )
        };
        for sample in [0, 3_000, 7_199] {
            for channel in 0..6 {
                assert_eq!(value(sample, channel), 0.);
            }
        }
        // Original starts at .25 s; trim starts at .1 s. A .2 s removal
        // starts at trim-relative .4 s, so later markers advance by 9600.
        for sample in [8_000usize, 19_199, 19_200, 40_000, 86_399] {
            let original = sample - 7_200 + if sample >= 19_200 { 9_600 } else { 0 };
            for channel in 0..6 {
                assert!(
                    (value(sample, channel) - (original as f64 / 100_000. + channel as f64 / 10.))
                        .abs()
                        < 1e-12,
                    "marker sample {sample}, channel {channel}: {}",
                    value(sample, channel)
                );
            }
        }
    }
}
