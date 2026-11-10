//! Focused desired-duration proof with generated media and the actual CPU worker.
use super::*;

fn media(args: &[&str], cwd: &Path) -> Vec<u8> {
    let bin = default_ffmpeg();
    let mut command = command_no_window(&bin);
    command
        .args(["-nostdin", "-y", "-v", "error"])
        .args(args)
        .current_dir(cwd);
    let output = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &bin,
        "Target-duration media fixture",
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn fixture(directory: &Path) -> PathBuf {
    let mut rgb = Vec::new();
    for frame in 0..24 {
        let position = if (10..12).contains(&frame) { 9 } else { frame };
        for y in 0..54 {
            for x in 0..96 {
                let x = x as f64 + position as f64 * 2.;
                let y = y as f64;
                let value = (112.
                    + 36. * (0.19 * x + 0.035 * y).sin()
                    + 24. * (0.071 * x - 0.139 * y).cos()
                    + 20. * (0.041 * x + 0.273 * y).sin())
                .round()
                .clamp(0., 255.) as u8;
                rgb.extend([value; 3]);
            }
        }
    }
    fs::write(directory.join("source.rgb"), rgb).unwrap();
    media(
        &[
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            "96x54",
            "-framerate",
            "30",
            "-i",
            "source.rgb",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=0.2*sin(2*PI*(220*t+170*t*t))+0.1*sin(2*PI*997*t):s=48000:d=0.8",
            "-vf",
            "scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=yuv420p,setparams=range=limited:color_primaries=bt709:color_trc=bt709:colorspace=bt709",
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-video_track_timescale",
            "30000",
            "-c:a",
            "aac",
            "source.mp4",
        ],
        directory,
    );
    directory.join("source.mp4")
}

fn original_pcm(input: &Path, directory: &Path) -> Vec<u8> {
    let pcm = media(
        &[
            "-i",
            &input.to_string_lossy(),
            "-map",
            "0:a:0",
            "-af",
            "atrim=start=0:end=0.8,asetpts=PTS-0/TB,aresample=async=1:first_pts=0:osr=48000:osf=dbl,apad=whole_len=38400,atrim=end_sample=38400",
            "-ar",
            "48000",
            "-channel_layout",
            "mono",
            "-c:a",
            "pcm_f64le",
            "-f",
            "f64le",
            "-",
        ],
        directory,
    );
    assert_eq!(pcm.len(), 38_400 * 8);
    pcm
}

fn samples(bytes: &[u8]) -> Vec<f64> {
    assert_eq!(bytes.len() % 8, 0);
    bytes
        .chunks_exact(8)
        .map(|sample| f64::from_le_bytes(sample.try_into().unwrap()))
        .collect()
}

fn reference_audio(pcm: &[u8], speed: f64, target_s: f64, directory: &Path) -> Vec<f64> {
    fs::write(directory.join("reference.raw"), pcm).unwrap();
    let segment_samples = (target_s * 0.5 * 48_000.).round() as u64;
    // The oracle starts with manually selected/reversed samples. It knows
    // neither the production removal map nor calculated playback speed.
    let filters = format!(
        "atempo={speed},apad=whole_len={segment_samples},atrim=end_sample={segment_samples},asplit[f][r0];[r0]areverse[r];[f][r]concat=n=2:v=0:a=1"
    );
    samples(&media(
        &[
            "-f",
            "f64le",
            "-ar",
            "48000",
            "-ac",
            "1",
            "-i",
            "reference.raw",
            "-af",
            &filters,
            "-c:a",
            "pcm_f64le",
            "-f",
            "f64le",
            "-",
        ],
        directory,
    ))
}

fn assert_audio_alignment(actual: &[f64], expected: &[f64], target_s: f64) {
    let target_samples = (target_s * 48_000.).round() as usize;
    assert_eq!(expected.len(), target_samples);
    // AAC can expose at most one padded final block on decoding. Its stream
    // end is checked separately; the active signal must match in both legs.
    assert!(
        (target_samples..=target_samples + 1024).contains(&actual.len()),
        "audio samples {}, expected {target_samples} plus <=1024 AAC padding",
        actual.len()
    );
    for fraction in [0.15, 0.35, 0.65, 0.85] {
        let start = (target_samples as f64 * fraction) as usize;
        let window = &expected[start..start + 2048];
        let mut best = (-1., 0isize);
        for lag in -128isize..=128 {
            let offset = (start as isize + lag) as usize;
            let candidate = &actual[offset..offset + window.len()];
            let cross = window
                .iter()
                .zip(candidate)
                .map(|(a, b)| a * b)
                .sum::<f64>();
            let energy_a = window.iter().map(|value| value * value).sum::<f64>();
            let energy_b = candidate.iter().map(|value| value * value).sum::<f64>();
            let correlation = cross / (energy_a * energy_b).sqrt();
            if correlation > best.0 {
                best = (correlation, lag);
            }
        }
        assert!(
            best.0 > 0.90,
            "AAC reference correlation {} at fraction {fraction}, lag {} samples",
            best.0,
            best.1
        );
        eprintln!(
            "Target audio fraction={fraction}: correlation={}, lag={} samples",
            best.0, best.1
        );
    }
}

#[test]
fn target_duration_chooses_the_nearest_complete_boomerang_frame_grid() {
    for (target, frames) in [(1.010, 30), (1.025, 31)] {
        let mut request = super::tests::base_request();
        request.target_duration_s = Some(target);
        request.loop_video = true;
        request.interpolation = Some(InterpolationSettings {
            anti_stutter_mode: AntiStutterMode::Remove,
            anti_stutter: false,
            anti_stutter_strength: 75,
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let result = resolve_target_duration(&mut request, 0.75)
            .unwrap()
            .unwrap();
        let rate = crate::interpolation_timeline::Rate::for_playback(60, request.speed).unwrap();
        assert_eq!(rate.frame_count(750_000_000).unwrap(), frames);
        assert!((result.estimated_output_duration_s - frames as f64 * 2. / 60.).abs() < 1e-12);
        assert!(result.frame_rounded);
        assert_eq!(request.target_duration_s, Some(target));
    }
}

#[test]
#[ignore = "requires pinned FFmpeg and bundled RIFE; explicit CPU qualification is supported"]
fn bundled_target_duration_uses_cleanup_then_fast_and_slow_inference_grids_with_audio() {
    let directory = TempDir::new().unwrap();
    let input = fixture(directory.path());
    let source_probe = probe_video(input.to_string_lossy().into()).unwrap();
    assert_eq!(source_probe.audio_sample_rate, Some(48_000));
    assert_eq!(source_probe.audio_channels, Some(1));
    let source_pcm = original_pcm(&input, directory.path());
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    for (remove, start, end, requested_target, speed, prepared_count, final_count) in [
        (true, 0., 0.8, 1.0, 1.5, 30u64, 60usize),
        (false, 0.1, 0.7, 2.41, 0.5, 72, 144),
    ] {
        // Audio follows the achievable complete-picture clock, including when
        // the requested boomerang duration falls between two frame counts.
        let target = final_count as f64 / 60.;
        let mut request = super::tests::base_request();
        request.size_limit_mb = 0.;
        request.input_path = input.to_string_lossy().into();
        request.audio_enabled = true;
        request.trim = Some(Trim {
            start_s: start,
            end_s: Some(end),
        });
        request.loop_video = true;
        request.reverse = !remove;
        request.target_duration_s = Some(requested_target);
        request.speed = 7.; // Stale manual speed must not choose the inference grid.
        request.interpolation = Some(InterpolationSettings {
            anti_stutter_mode: AntiStutterMode::Remove,
            anti_stutter: remove,
            anti_stutter_strength: 75,
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let mut analysis_progress = Vec::new();
        let analysis = remove.then(|| {
            crate::interpolation_media::analyze_removal(
                &default_ffmpeg(),
                &input,
                &source_probe,
                &request,
                &cancel,
                &group,
                |value| analysis_progress.push(value),
            )
            .unwrap()
        });
        assert!(analysis_progress.windows(2).all(|w| w[0] <= w[1]));
        let retained = analysis.as_ref().map_or_else(
            || retained_source_duration_s(&request, &source_probe).unwrap(),
            |analysis| analysis.retained_duration_s(),
        );
        assert!((retained - if remove { 0.75 } else { 0.6 }).abs() < 1e-9);
        let target_diagnostics = resolve_target_duration(&mut request, retained)
            .unwrap()
            .unwrap();
        assert!(
            (request.speed - speed).abs() < 1e-9,
            "{target_diagnostics:?}"
        );
        assert!((target_diagnostics.retained_duration_s - retained).abs() < 1e-9);
        assert!((target_diagnostics.estimated_output_duration_s - target).abs() < 1e-9);
        assert_eq!(target_diagnostics.frame_rounded, !remove);
        assert_eq!(target_diagnostics.requested_duration_s, requested_target);
        let mut progress = Vec::new();
        let prepared = crate::interpolation_media::prepare_planned(
            &default_ffmpeg(),
            &input,
            &source_probe,
            &request,
            &model,
            &cancel,
            &group,
            analysis.as_ref(),
            |value| progress.push(value),
        )
        .unwrap();
        assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
        assert_eq!(progress.last(), Some(&1.));
        assert_eq!(prepared.diagnostics.output_frames, prepared_count);
        assert!((prepared.output_duration_s() - target).abs() < 1e-9);
        let prepared_rgb = media(
            &[
                "-i",
                &prepared.path.to_string_lossy(),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ],
            directory.path(),
        );
        assert_eq!(
            prepared_rgb.len(),
            prepared_count as usize * 96 * 54 * 3,
            "speed must choose the inference grid before final encoding"
        );
        let mut effective = source_probe.clone();
        let reference_pcm = if remove {
            let map = prepared.removal_map().unwrap();
            assert_eq!(map.cuts().len(), 1);
            assert_eq!(
                (map.cuts()[0].start_ns, map.cuts()[0].end_ns),
                (350_000_000, 400_000_000)
            );
            let expected = [&source_pcm[..16_800 * 8], &source_pcm[19_200 * 8..]].concat();
            let audio = prepared.removal_audio().unwrap();
            assert_eq!(audio.sample_count, 36_000);
            let spool = fs::read(&audio.path).unwrap();
            assert_eq!(spool.len(), expected.len());
            assert!(
                spool == expected,
                "cleanup must retain the exact declared source audio samples"
            );
            apply_compacted_timeline(
                &mut request,
                &mut effective,
                retained,
                Some((audio.sample_rate, audio.channels)),
            )
            .unwrap();
            expected
        } else {
            assert!(prepared.removal_map().is_none());
            source_pcm[4_800 * 8..33_600 * 8]
                .chunks_exact(8)
                .rev()
                .flat_map(|sample| sample.iter().copied())
                .collect()
        };
        let expected_audio = reference_audio(&reference_pcm, speed, target, directory.path());
        let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
        let codecs =
            select_codec_plan(request.format, &runtime.encoder_names, &request.advanced).unwrap();
        let plan = build_encode_command_plan(&request, &effective, codecs, None).unwrap();
        assert_eq!(plan.audio_action, StreamAction::Encode);
        let output = directory.path().join(format!("target-remove-{remove}.mp4"));
        let mut args = build_single_pass_args(&request.input_path, &request, &plan).unwrap();
        args.push(output.to_string_lossy().into());
        run_ffmpeg_with_progress(
            None,
            1,
            1,
            &default_ffmpeg(),
            &args,
            None,
            EncodeProgressStep::single(EncodeProgressPhase::Encoding),
            (target * 1e6).round() as u64,
            true,
            ffmpeg_run_limits(true).with_output_activity(&output),
            &cancel,
            &group,
            Some(&prepared),
        )
        .unwrap();
        let result = probe_video(output.to_string_lossy().into()).unwrap();
        assert!(result.has_audio);
        assert!((result.duration_s - target).abs() < 1. / 60., "{result:?}");
        assert!(
            (result.audio_end_s.unwrap() - target).abs() < 1. / 60.,
            "{result:?}"
        );
        let rgb = media(
            &[
                "-i",
                &output.to_string_lossy(),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ],
            directory.path(),
        );
        assert_eq!(rgb.len(), final_count * 96 * 54 * 3);
        let final_audio = samples(&media(
            &[
                "-i",
                &output.to_string_lossy(),
                "-map",
                "0:a:0",
                "-c:a",
                "pcm_f64le",
                "-f",
                "f64le",
                "-",
            ],
            directory.path(),
        ));
        assert_audio_alignment(&final_audio, &expected_audio, target);
        assert!(group.lock().unwrap().is_empty());
        if remove {
            let mut changed = request.clone();
            changed.trim = Some(Trim {
                start_s: 0.01,
                end_s: Some(0.8),
            });
            let error = crate::interpolation_media::prepare_planned(
                &default_ffmpeg(),
                &input,
                &source_probe,
                &changed,
                &model,
                &cancel,
                &group,
                analysis.as_ref(),
                |_| panic!("stale cleanup plan reached rendering"),
            )
            .err()
            .expect("a changed trim must reject the source-bound cleanup plan");
            assert!(error.contains("no longer matches"), "{error}");
            assert!(group.lock().unwrap().is_empty());
        }
        eprintln!(
            "Target integration remove={remove}: speed={}, retained={retained}, prepared_frames={}, final_frames={final_count}, audio_samples={}, diagnostics={}",
            request.speed,
            prepared.diagnostics.output_frames,
            final_audio.len(),
            serde_json::to_string(&target_diagnostics).unwrap()
        );
    }
}
