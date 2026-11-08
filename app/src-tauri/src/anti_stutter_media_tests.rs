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
        anti_stutter: false,
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
