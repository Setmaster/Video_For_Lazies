//! Focused opt-in proof using generated input and the actual bundled pair worker.
use super::*;

fn ffmpeg(args: &[&str], cwd: &Path) -> Vec<u8> {
    let bin = default_ffmpeg();
    let mut command = command_no_window(&bin);
    command
        .args(["-nostdin", "-y", "-v", "error"])
        .args(args)
        .current_dir(cwd);
    let result = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &bin,
        "Anti-stutter media fixture",
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

#[test]
#[ignore = "requires pinned FFmpeg and bundled RIFE; explicit CPU qualification is supported"]
fn bundled_anti_stutter_preserves_clock_off_bytes_audio_and_prepared_reuse() {
    let temp = TempDir::new().unwrap();
    let (width, height) = (96, 54);
    let mut raw = Vec::new();
    for frame in 0..24 {
        let position = if (10..12).contains(&frame) { 9 } else { frame };
        for y in 0..height {
            for x in 0..width {
                let x = x as f64 + position as f64 * 2.0;
                let y = y as f64;
                let value = (112.0
                    + 36.0 * (0.19 * x + 0.035 * y).sin()
                    + 24.0 * (0.071 * x - 0.139 * y).cos()
                    + 20.0 * (0.041 * x + 0.273 * y).sin())
                .round()
                .clamp(0.0, 255.0) as u8;
                raw.extend([value; 3]);
            }
        }
    }
    fs::write(temp.path().join("source.rgb"), raw).unwrap();
    ffmpeg(
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
            "sine=frequency=440:sample_rate=48000:duration=0.8",
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
        temp.path(),
    );
    let mut req = super::tests::base_request();
    req.size_limit_mb = 0.0;
    req.input_path = temp.path().join("source.mp4").to_string_lossy().into();
    req.audio_enabled = true;
    req.interpolation = Some(InterpolationSettings {
        anti_stutter_mode: crate::video::AntiStutterMode::Repair,
        anti_stutter: false,
        anti_stutter_strength: 0,
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    let source_path = PathBuf::from(&req.input_path);
    let source = source_path.as_path();
    let probe = probe_video(req.input_path.clone()).unwrap();
    validate_interpolation_request(&req, &probe).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    let mut baseline: Option<Vec<u8>> = None;
    let mut prepared_on = None;
    for enabled in [false, false, true] {
        req.interpolation.as_mut().unwrap().anti_stutter = enabled;
        let prepared = crate::interpolation_media::prepare(
            &default_ffmpeg(),
            source,
            &probe,
            &req,
            &model,
            &cancel,
            &group,
            |_| {},
        )
        .unwrap();
        let pixels = ffmpeg(
            &[
                "-i",
                &prepared.path.to_string_lossy(),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        assert_eq!(pixels.len(), 48 * width * height * 3);
        if enabled {
            let anti = prepared.diagnostics.anti_stutter.as_ref().unwrap();
            assert_eq!(
                (anti.repaired_runs, anti.repaired_frames),
                (1, 5),
                "{anti:?}"
            );
            let baseline = baseline.as_ref().unwrap();
            let size = width * height * 3;
            assert_eq!(&pixels[..19 * size], &baseline[..19 * size]);
            assert_eq!(&pixels[24 * size..], &baseline[24 * size..]);
            assert_ne!(
                &pixels[19 * size..24 * size],
                &baseline[19 * size..24 * size]
            );
            eprintln!(
                "Anti-stutter actual RIFE: {}",
                serde_json::to_string(anti).unwrap()
            );
            prepared_on = Some(prepared);
        } else {
            assert!(prepared.diagnostics.anti_stutter.is_none());
            assert!(
                !serde_json::to_string(&prepared.diagnostics)
                    .unwrap()
                    .contains("antiStutter")
            );
            if let Some(baseline) = &baseline {
                assert_eq!(&pixels, baseline);
            } else {
                baseline = Some(pixels);
            }
        }
        assert!(group.lock().unwrap().is_empty());
    }
    let prepared = prepared_on.unwrap();
    let size = fs::metadata(&prepared.path).unwrap().len();
    let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
    let codecs = select_codec_plan(req.format, &runtime.encoder_names, &req.advanced).unwrap();
    let thumbnail_stage = TempDir::new().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=40x40:d=0.04",
            "-frames:v",
            "1",
            "thumbnail.png",
        ],
        temp.path(),
    );
    let thumbnail_path = temp.path().join("thumbnail.png");
    let thumbnail_inspection = inspect_thumbnail(thumbnail_path.to_string_lossy().into()).unwrap();
    let thumbnail_settings = ThumbnailSettings {
        image_path: thumbnail_path.to_string_lossy().into(),
        fit: thumbnail::ThumbnailFit::Fit,
        source_identity: thumbnail_inspection.source_identity,
    };
    thumbnail::stage(
        &default_ffmpeg(),
        &thumbnail_settings,
        thumbnail_stage.path(),
    )
    .unwrap();
    let source_audio = ffmpeg(
        &[
            "-i",
            &req.input_path,
            "-map",
            "0:a:0",
            "-c:a",
            "copy",
            "-f",
            "adts",
            "-",
        ],
        temp.path(),
    );
    for attempt in 0..2 {
        req.thumbnail = if attempt == 1 {
            Some(thumbnail_settings.clone())
        } else {
            None
        };
        let plan = build_encode_command_plan(&req, &probe, codecs, None).unwrap();
        assert_eq!(plan.audio_action, StreamAction::Copy);
        let output = temp.path().join(format!("attempt-{attempt}.mp4"));
        let mut args = build_single_pass_args(&req.input_path, &req, &plan).unwrap();
        args.push(output.to_string_lossy().into());
        run_ffmpeg_with_progress(
            None,
            1,
            1,
            &default_ffmpeg(),
            &args,
            Some(thumbnail_stage.path()),
            EncodeProgressStep::single(EncodeProgressPhase::Encoding),
            800_000,
            true,
            ffmpeg_run_limits(false).with_output_activity(&output),
            &cancel,
            &group,
            Some(&prepared),
        )
        .unwrap();
        let result = probe_video(output.to_string_lossy().into()).unwrap();
        assert!(result.has_audio);
        assert!((result.duration_s - 0.8).abs() < 0.04);
        let audio = ffmpeg(
            &[
                "-i",
                &output.to_string_lossy(),
                "-map",
                "0:a:0",
                "-c:a",
                "copy",
                "-f",
                "adts",
                "-",
            ],
            temp.path(),
        );
        assert_eq!(audio, source_audio);
        if attempt == 1 {
            let first = ffmpeg(
                &[
                    "-i",
                    &output.to_string_lossy(),
                    "-frames:v",
                    "1",
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                temp.path(),
            );
            let offset = (height / 2 * width + width / 2) * 3;
            assert!(first[offset] < 10 && first[offset + 1] < 10 && first[offset + 2] > 230);
        }
        assert_eq!(fs::metadata(&prepared.path).unwrap().len(), size);
    }
}

#[test]
#[ignore = "requires pinned FFmpeg and bundled RIFE; explicit CPU qualification is supported"]
fn bundled_remove_compacts_video_audio_then_speed_reverse_loop_and_thumbnail() {
    let temp = TempDir::new().unwrap();
    let (width, height) = (96, 54);
    let mut raw = Vec::new();
    for frame in 0..24 {
        let position = if (10..12).contains(&frame) { 9 } else { frame };
        for y in 0..height {
            for x in 0..width {
                let x = x as f64 + position as f64 * 2.0;
                let y = y as f64;
                let value = (112.0
                    + 36.0 * (0.19 * x + 0.035 * y).sin()
                    + 24.0 * (0.071 * x - 0.139 * y).cos()
                    + 20.0 * (0.041 * x + 0.273 * y).sin())
                .round()
                .clamp(0.0, 255.0) as u8;
                raw.extend([value; 3]);
            }
        }
    }
    fs::write(temp.path().join("source.rgb"), raw).unwrap();
    ffmpeg(
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
        temp.path(),
    );

    let input = temp.path().join("source.mp4");
    let source_probe = probe_video(input.to_string_lossy().into()).unwrap();
    assert_eq!(source_probe.audio_sample_rate, Some(48_000));
    assert_eq!(source_probe.audio_channels, Some(1));
    // Decode the original clock independently of the removal helper. The
    // predeclared adaptive bridge keeps [0.300,0.350) and removes only
    // [0.350,0.400), including precisely the matching source audio suffix.
    let source_audio = ffmpeg(
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
        temp.path(),
    );
    assert_eq!(source_audio.len(), 38_400 * 8);
    let expected_audio = [&source_audio[..16_800 * 8], &source_audio[19_200 * 8..]].concat();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=40x40:d=0.04",
            "-frames:v",
            "1",
            "remove-thumbnail.png",
        ],
        temp.path(),
    );
    let image = temp.path().join("remove-thumbnail.png");
    let settings = ThumbnailSettings {
        image_path: image.to_string_lossy().into(),
        fit: thumbnail::ThumbnailFit::Fit,
        source_identity: inspect_thumbnail(image.to_string_lossy().into())
            .unwrap()
            .source_identity,
    };
    let stage = TempDir::new().unwrap();
    thumbnail::stage(&default_ffmpeg(), &settings, stage.path()).unwrap();
    for transformed in [false, true] {
        let mut req = super::tests::base_request();
        req.size_limit_mb = 0.0;
        req.input_path = input.to_string_lossy().into();
        req.audio_enabled = true;
        req.trim = Some(Trim {
            start_s: 0.0,
            end_s: Some(0.8),
        });
        req.reverse = transformed;
        req.loop_video = transformed;
        req.speed = if transformed { 2.0 } else { 1.0 };
        req.thumbnail = transformed.then(|| settings.clone());
        req.interpolation = Some(InterpolationSettings {
            anti_stutter_mode: AntiStutterMode::Remove,
            anti_stutter: true,
            anti_stutter_strength: 75,
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let mut progress = Vec::new();
        let prepared = crate::interpolation_media::prepare(
            &default_ffmpeg(),
            &input,
            &source_probe,
            &req,
            &model,
            &cancel,
            &group,
            |value| progress.push(value),
        )
        .unwrap();
        assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
        assert_eq!(progress.last(), Some(&1.0));
        let map = prepared
            .removal_map()
            .expect("isolated irregular hold must be removed");
        assert_eq!(map.cuts().len(), 1);
        assert!((map.cuts()[0].start_ns as i64 - 350_000_000).abs() <= 1);
        assert_eq!(map.cuts()[0].end_ns, 400_000_000);
        let anti = prepared.diagnostics.anti_stutter.as_ref().unwrap();
        assert_eq!((anti.removed_runs, anti.removed_frames), (Some(1), Some(1)));
        assert_eq!(anti.repaired_runs, 0);
        let diagnostic = serde_json::to_value(anti).unwrap();
        assert_eq!(diagnostic["bridgedRuns"].as_u64(), Some(1), "{anti:?}");
        assert!(
            diagnostic["retainedBridgeDurationS"].as_f64().unwrap() > 0.,
            "{anti:?}"
        );
        let bridge_s = diagnostic["intervals"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|interval| interval["bridgeDurationS"].as_f64())
            .expect("adaptive bridge interval");
        assert!((bridge_s - 0.05).abs() < 1e-9, "{anti:?}");
        assert!(bridge_s > 1.0 / 30.0);
        let audio = prepared.removal_audio().unwrap();
        assert_eq!(audio.sample_count, 36_000);
        assert_eq!(
            fs::metadata(&audio.path).unwrap().len(),
            36_000 * u64::from(audio.channels) * 8
        );
        let compacted_audio = fs::read(&audio.path).unwrap();
        assert_eq!(compacted_audio.len(), expected_audio.len());
        let first_difference = compacted_audio
            .iter()
            .zip(&expected_audio)
            .position(|(a, b)| a != b);
        assert!(
            first_difference.is_none(),
            "retained source sample mismatch at output sample {:?}",
            first_difference.map(|byte| byte / 8)
        );
        assert_eq!(
            prepared.diagnostics.output_frames,
            if transformed { 23 } else { 45 }
        );
        assert!((prepared.effective_source_duration_s() - 0.75).abs() < 1e-9);
        let output_frames = if transformed { 46 } else { 45 };
        assert!((prepared.output_duration_s() - output_frames as f64 / 60.).abs() < 1e-9);
        if !transformed {
            let bridge_pixels = ffmpeg(
                &[
                    "-i",
                    &prepared.path.to_string_lossy(),
                    "-pix_fmt",
                    "rgb24",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                temp.path(),
            );
            let size = width * height * 3;
            for frame in 18..21 {
                assert!(
                    bridge_pixels[frame * size..(frame + 1) * size]
                        != bridge_pixels[(frame + 1) * size..(frame + 2) * size],
                    "retained bridge must progress through actual interpolated frames"
                );
            }
        }
        let mut effective = source_probe.clone();
        apply_compacted_timeline(
            &mut req,
            &mut effective,
            prepared.effective_source_duration_s(),
            Some((audio.sample_rate, audio.channels)),
        )
        .unwrap();
        let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
        let codecs = select_codec_plan(req.format, &runtime.encoder_names, &req.advanced).unwrap();
        let plan = build_encode_command_plan(&req, &effective, codecs, None).unwrap();
        assert_eq!(plan.audio_action, StreamAction::Encode);
        let output = temp.path().join(format!("removed-{transformed}.mp4"));
        let mut args = build_single_pass_args(&req.input_path, &req, &plan).unwrap();
        args.push(output.to_string_lossy().into());
        run_ffmpeg_with_progress(
            None,
            1,
            1,
            &default_ffmpeg(),
            &args,
            Some(stage.path()),
            EncodeProgressStep::single(EncodeProgressPhase::Encoding),
            if transformed { 766_667 } else { 750_000 },
            true,
            ffmpeg_run_limits(transformed).with_output_activity(&output),
            &cancel,
            &group,
            Some(&prepared),
        )
        .unwrap();
        let result = probe_video(output.to_string_lossy().into()).unwrap();
        assert!(result.has_audio);
        assert!(
            (result.duration_s - prepared.output_duration_s()).abs() < 0.025,
            "{result:?}"
        );
        let pixels = ffmpeg(
            &[
                "-i",
                &output.to_string_lossy(),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        assert_eq!(pixels.len(), output_frames * width * height * 3);
        if transformed {
            let offset = (height / 2 * width + width / 2) * 3;
            assert!(pixels[offset] < 10 && pixels[offset + 1] < 10 && pixels[offset + 2] > 230);
        }
        assert!(group.lock().unwrap().is_empty());
        eprintln!(
            "Remove integration transformed={transformed}: {}",
            serde_json::to_string(anti).unwrap()
        );
    }
}
