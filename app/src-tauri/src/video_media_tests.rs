//! Opt-in semantic tests against the configured FFmpeg/FFprobe runtime.
//! Run `cargo test --lib video::media_integration_tests -- --ignored` with
//! VFL_FFMPEG_PATH and VFL_FFPROBE_PATH pointing to the bundled sidecars.
use super::*;

fn ffmpeg(args: &[&str], cwd: &Path) -> Vec<u8> {
    let bin = default_ffmpeg();
    let mut command = command_no_window(&bin);
    command.args(["-nostdin", "-y", "-v", "error"]);
    command.args(args).current_dir(cwd);
    let output = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &bin,
        "media integration fixture",
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

fn source(cwd: &Path, size: &str, sar: &str) -> VideoProbe {
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=s={size}:r=30:d=4"),
            "-vf",
            &format!("setsar={sar}"),
            "-c:v",
            "ffv1",
            "source.nut",
        ],
        cwd,
    );
    probe_video(cwd.join("source.nut").to_string_lossy().into()).unwrap()
}

fn request() -> EncodeRequest {
    let mut req = super::tests::base_request();
    req.size_limit_mb = 0.0;
    req.audio_enabled = false;
    req
}

fn probe_streams(path: &Path) -> FFProbeOutput {
    let bin = default_ffprobe();
    let mut command = command_no_window(&bin);
    command
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path);
    let output = run_command_output_with_timeout(
        command,
        "ffprobe",
        "VFL_FFPROBE_PATH",
        &bin,
        "media integration output probe",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_frame_export_rejects_empty_and_preserves_existing_files() {
    let temp = tempfile::tempdir().unwrap();
    source(temp.path(), "160x90", "1");
    let input = temp.path().join("source.nut").to_string_lossy().to_string();
    let output = temp.path().join("frame.png");
    extract_frame(input.clone(), 0.5, output.to_string_lossy().into()).unwrap();
    validate_frame_output(&output).unwrap();
    let saved = fs::read(&output).unwrap();
    assert!(extract_frame(input.clone(), 0.5, output.to_string_lossy().into()).is_err());
    assert_eq!(fs::read(&output).unwrap(), saved);
    for (index, timestamp) in [4.0, 5.0].into_iter().enumerate() {
        let missing = temp.path().join(format!("missing-{index}.png"));
        assert!(extract_frame(input.clone(), timestamp, missing.to_string_lossy().into()).is_err());
        assert!(!missing.exists());
    }
    for length in [0, 8, 24, saved.len() - 1] {
        fs::write(temp.path().join("truncated.png"), &saved[..length]).unwrap();
        assert!(validate_frame_output(&temp.path().join("truncated.png")).is_err());
    }
    assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".vfl-")
    }));
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_custom_and_max_edge_geometry_match_plans() {
    let temp = tempfile::tempdir().unwrap();
    for sar in ["1", "4/3"] {
        let probe = source(temp.path(), "960x540", sar);
        for (width, height, rotation) in [(160, 120, 0), (159, 119, 90)] {
            let mut req = request();
            req.rotate_deg = rotation;
            req.resize = Some(ResizeSettings {
                mode: ResizeMode::Custom,
                max_edge_px: None,
                width_px: Some(width),
                height_px: Some(height),
            });
            let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
            let filters = build_video_filters_with_policy(&req, &probe, policy)
                .unwrap()
                .unwrap();
            ffmpeg(
                &[
                    "-i",
                    "source.nut",
                    "-vf",
                    &filters,
                    "-frames:v",
                    "1",
                    "-c:v",
                    "libx264",
                    "custom.mp4",
                ],
                temp.path(),
            );
            let actual =
                probe_video(temp.path().join("custom.mp4").to_string_lossy().into()).unwrap();
            assert_eq!(
                (actual.width, actual.height),
                estimated_output_dimensions(&req, &probe).unwrap()
            );
            assert!(actual.sample_aspect_ratio.is_square());
        }
        for rotation in [0, 90] {
            for cap in [499, 500, 501, 1280] {
                let mut req = request();
                req.rotate_deg = rotation;
                req.max_edge_px = Some(cap);
                let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
                let filters = build_video_filters_with_policy(&req, &probe, policy)
                    .unwrap()
                    .unwrap();
                ffmpeg(
                    &[
                        "-i",
                        "source.nut",
                        "-vf",
                        &filters,
                        "-frames:v",
                        "1",
                        "-c:v",
                        "libx264",
                        "cap.mp4",
                    ],
                    temp.path(),
                );
                let actual =
                    probe_video(temp.path().join("cap.mp4").to_string_lossy().into()).unwrap();
                assert_eq!(
                    (actual.width, actual.height),
                    estimated_output_dimensions(&req, &probe).unwrap(),
                    "SAR {sar}, rotation {rotation}, cap {cap}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_early_subtitle_trim_preserves_pixels_and_timestamps() {
    let temp = tempfile::tempdir().unwrap();
    let probe = source(temp.path(), "320x180", "1");
    let srt = temp.path().join("input.srt");
    fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,500\nCrossing cue\n\n2\n00:00:02,500 --> 00:00:03,500\nNext cue\n").unwrap();
    let prepared = prepare_external_subtitle(&srt).unwrap();
    let input = temp.path().join("source.nut").to_string_lossy().to_string();
    for (reverse, loop_video, speed) in
        [(false, false, 1.0), (true, false, 1.0), (false, true, 2.0)]
    {
        let mut req = request();
        req.trim = Some(Trim {
            start_s: 2.0,
            end_s: Some(3.5),
        });
        req.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.into());
        req.max_edge_px = Some(240);
        req.reverse = reverse;
        req.loop_video = loop_video;
        req.speed = speed;
        req.color = Some(ColorAdjust {
            brightness: 0.1,
            contrast: 1.1,
            saturation: 1.2,
        });
        let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
        let early = build_video_filters_with_policy(&req, &probe, policy)
            .unwrap()
            .unwrap();
        let late = early.replacen("trim=start=2:end=3.5,", "", 1).replacen(
            "setpts=PTS-2/TB",
            "trim=start=2:end=3.5,setpts=PTS-2/TB",
            1,
        );
        let early_frames = ffmpeg(
            &[
                "-i",
                &input,
                "-vf",
                &early,
                "-an",
                "-fps_mode",
                "passthrough",
                "-f",
                "framemd5",
                "-",
            ],
            prepared.working_dir(),
        );
        let late_frames = ffmpeg(
            &[
                "-i",
                &input,
                "-vf",
                &late,
                "-an",
                "-fps_mode",
                "passthrough",
                "-f",
                "framemd5",
                "-",
            ],
            prepared.working_dir(),
        );
        assert_eq!(
            early_frames, late_frames,
            "reverse={reverse} loop={loop_video} speed={speed}"
        );
    }
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_trim_keeps_audio_offsets_through_speed_reverse_and_loop() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=30:d=4",
            "-itsoffset",
            "1",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=if(lt(t\\,1.5)\\,sin(2*PI*440*t)\\,0):s=48000:d=3",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_f32le",
            "delayed.nut",
        ],
        temp.path(),
    );
    let probe = probe_video(temp.path().join("delayed.nut").to_string_lossy().into()).unwrap();
    for normalize_audio in [false, true] {
        for (start, reverse, loop_video, speed) in [
            (0.0, false, false, 1.0),
            (0.5, false, false, 1.0),
            (0.0, false, false, 2.0),
            (0.0, false, false, 0.5),
            (0.5, false, false, 2.0),
            (0.0, true, false, 1.0),
            (0.0, true, false, 2.0),
            (0.0, false, true, 1.0),
            (0.0, false, true, 2.0),
            (0.0, true, true, 1.0),
            (0.5, true, true, 2.0),
        ] {
            let mut req = request();
            req.audio_enabled = true;
            req.normalize_audio = normalize_audio;
            req.trim = Some(Trim {
                start_s: start,
                end_s: Some(3.0),
            });
            req.reverse = reverse;
            req.loop_video = loop_video;
            req.speed = speed;
            req.input_path = temp.path().join("delayed.nut").to_string_lossy().into();
            let mut probe = probe.clone();
            prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap();
            let filters = build_audio_filters(&req, &probe).unwrap().unwrap();
            // Raw PCM has no timestamps. Materialize the output's leading offset
            // solely for the assertion; production linear trims keep stream PTS.
            let render = format!("{filters},aresample=48000:async=1:first_pts=0");
            let raw = ffmpeg(
                &[
                    "-i",
                    "delayed.nut",
                    "-vn",
                    "-af",
                    &render,
                    "-ac",
                    "1",
                    "-c:a",
                    "pcm_f32le",
                    "-f",
                    "f32le",
                    "-",
                ],
                temp.path(),
            );
            let samples: Vec<f32> = raw
                .as_chunks::<4>()
                .0
                .iter()
                .map(|v| f32::from_le_bytes(*v))
                .collect();
            let duration = (3.0 - start) / speed;
            let (tone_start, tone_end) = if reverse {
                (0.5 / speed, 2.0 / speed)
            } else {
                ((1.0 - start) / speed, (2.5 - start) / speed)
            };
            let expected_duration = duration * if loop_video { 2.0 } else { 1.0 };
            assert!(
                // Linear atempo retains its existing finite-window tail behavior.
                (samples.len() as f64 / 48_000.0 - expected_duration).abs() < 0.08,
                "sample duration: {start}/{reverse}/{loop_video}/{speed}: {}",
                samples.len() as f64 / 48_000.0
            );
            for index in 0..(expected_duration * 10.0) as usize {
                let time = index as f64 / 10.0 + 0.05;
                let local = if loop_video && time >= duration {
                    2.0 * duration - time
                } else {
                    time
                };
                if (local - tone_start).abs() < 0.08 || (local - tone_end).abs() < 0.08 {
                    continue;
                }
                let center = (time * 48_000.0) as usize;
                if center + 240 > samples.len() {
                    continue;
                }
                let slice = &samples[center.saturating_sub(240)..(center + 240).min(samples.len())];
                let power =
                    slice.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / slice.len() as f64;
                let expected_tone = local > tone_start && local < tone_end;
                assert_eq!(
                    power > 0.001,
                    expected_tone,
                    "{start}/{reverse}/{loop_video}/{speed} at {time}: power={power}; {filters}"
                );
            }
            if !reverse && !loop_video {
                // Also exercise the real command builder and MP4/AAC muxer: raw
                // filter output alone would not prove that publication keeps PTS.
                let capabilities = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
                let codecs =
                    select_codec_plan(req.format, &capabilities.encoder_names, &req.advanced)
                        .unwrap();
                let plan = build_encode_command_plan(&req, &probe, codecs, None).unwrap();
                let mut args = build_single_pass_args("delayed.nut", &req, &plan).unwrap();
                args.push("timed.mp4".into());
                ffmpeg(
                    &args.iter().map(String::as_str).collect::<Vec<_>>(),
                    temp.path(),
                );
                let parsed = probe_streams(&temp.path().join("timed.mp4"));
                let audio = select_primary_audio_stream(&parsed.streams).unwrap();
                let audio_start = audio.start_time.as_ref().unwrap().parse::<f64>().unwrap();
                assert!(
                    (audio_start - (1.0 - start) / speed).abs() < 0.03,
                    "AAC start {audio_start}: trim {start}, speed {speed}"
                );
            }
        }
    }
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_video_gaps_stay_on_the_common_timeline() {
    let temp = tempfile::tempdir().unwrap();
    // Leading, trailing, and both gaps cover video starting after audio as well
    // as audio/container duration outlasting the final actual picture.
    for (video_start, video_duration) in [(1.0, 3.0), (0.0, 2.0), (1.0, 1.0)] {
        ffmpeg(
            &[
                "-itsoffset",
                &video_start.to_string(),
                "-f",
                "lavfi",
                "-i",
                &format!("color=red:s=160x90:r=30:d={video_duration}"),
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000:duration=4",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-c:v",
                "ffv1",
                "-c:a",
                "pcm_f32le",
                "video-gap.nut",
            ],
            temp.path(),
        );
        let probe =
            probe_video(temp.path().join("video-gap.nut").to_string_lossy().into()).unwrap();
        assert!((probe.video_start_s - video_start).abs() < 0.001);
        for trim_start in [0.0, 0.5, 2.5] {
            for (reverse, loop_video, speed) in [
                (true, false, 1.0),
                (true, false, 2.0),
                (false, true, 1.0),
                (false, true, 0.5),
                (true, true, 2.0),
            ] {
                let mut req = request();
                req.trim = Some(Trim {
                    start_s: trim_start,
                    end_s: Some(3.0),
                });
                req.reverse = reverse;
                req.loop_video = loop_video;
                req.speed = speed;
                req.audio_enabled = true;
                req.input_path = temp.path().join("video-gap.nut").to_string_lossy().into();
                let mut probe = probe.clone();
                prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap();
                let policy = resolve_media_policy(&req, &probe, Some(VideoCodec::LibX264)).unwrap();
                let filters = build_video_filters_with_policy(&req, &probe, policy)
                    .unwrap()
                    .unwrap();
                let frames = ffmpeg(
                    &[
                        "-i",
                        "video-gap.nut",
                        "-vf",
                        &format!("{filters},fps=30,format=gray"),
                        "-an",
                        "-f",
                        "rawvideo",
                        "-",
                    ],
                    temp.path(),
                );
                let duration = (3.0 - trim_start) / speed;
                let total = duration * if loop_video { 2.0 } else { 1.0 };
                let frame_size = 160 * 90;
                assert!(
                    (frames.len() as f64 / frame_size as f64 / 30.0 - total).abs() < 0.07,
                    "video {video_start}/{video_duration}, {reverse}/{loop_video}/{speed}"
                );
                for (index, frame) in frames.chunks_exact(frame_size).enumerate() {
                    let time = (index as f64 + 0.5) / 30.0;
                    let local = if loop_video && time >= duration {
                        2.0 * duration - time
                    } else {
                        time
                    };
                    let source_time = if reverse {
                        3.0 - local * speed
                    } else {
                        trim_start + local * speed
                    };
                    if (source_time - video_start).abs() < 0.08 * speed
                        || (source_time - video_start - video_duration).abs() < 0.08 * speed
                    {
                        continue;
                    }
                    let expected_red =
                        source_time > video_start && source_time < video_start + video_duration;
                    let mean =
                        frame.iter().map(|v| *v as u64).sum::<u64>() as f64 / frame_size as f64;
                    assert_eq!(
                        mean > 30.0,
                        expected_red,
                        "video {video_start}/{video_duration}, {reverse}/{loop_video}/{speed}, {time}, {mean}; {filters}"
                    );
                }
            }
        }
    }
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_common_origin_and_untrimmed_endpoints_preserve_all_media() {
    let temp = tempfile::tempdir().unwrap();
    for (extension, video_codec, audio_codec) in [
        ("nut", "ffv1", "pcm_f32le"),
        ("mkv", "ffv1", "pcm_f32le"),
        ("webm", "libvpx-vp9", "libopus"),
        ("mp4", "libx264", "aac"),
        ("ts", "libx264", "aac"),
    ] {
        for common_origin in [0, 5] {
            let file = format!("origin.{extension}");
            let origin = common_origin.to_string();
            ffmpeg(
                &[
                    "-itsoffset",
                    &origin,
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=s=160x90:r=30:d=4",
                    "-itsoffset",
                    &origin,
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=sample_rate=48000:duration=4",
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    video_codec,
                    "-c:a",
                    audio_codec,
                    &file,
                ],
                temp.path(),
            );
            let mut probe = probe_video(temp.path().join(&file).to_string_lossy().into()).unwrap();
            assert!(
                (probe.duration_s - 4.0).abs() < 0.08,
                "{extension}/{common_origin}: {}",
                probe.duration_s
            );
            let mut req = request();
            req.audio_enabled = true;
            req.reverse = true;
            req.input_path = temp.path().join(&file).to_string_lossy().into();
            prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap();
            let vf = build_video_filters(&req, &probe).unwrap().unwrap();
            let af = build_audio_filters(&req, &probe).unwrap().unwrap();
            let frames = ffmpeg(
                &[
                    "-i",
                    &file,
                    "-vf",
                    &format!("{vf},format=gray"),
                    "-an",
                    "-fps_mode",
                    "passthrough",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                temp.path(),
            );
            // Lossless fixtures must preserve all120 actual pictures exactly.
            // AAC/Opus start delays may require one bounded black boundary frame.
            let count = frames.len() / (160 * 90);
            assert!(
                (120..=122).contains(&count),
                "{extension}/{common_origin}: {count}; {vf}"
            );
            if matches!(extension, "nut" | "mkv") {
                assert_eq!(count, 120);
            }
            let samples = ffmpeg(
                &[
                    "-i",
                    &file,
                    "-af",
                    &af,
                    "-vn",
                    "-c:a",
                    "pcm_f32le",
                    "-f",
                    "f32le",
                    "-",
                ],
                temp.path(),
            );
            let duration = samples.len() as f64 / 4.0 / 48_000.0;
            assert!(
                (duration - 4.0).abs() < 0.08,
                "{extension}/{common_origin}: {duration}; {af}"
            );
            if extension == "nut" {
                assert_eq!(samples.len() / 4, 4 * 48_000);
            }
        }
    }
    // Video-only NUT's nominal duration is the last PTS (3.966667), not the end.
    let probe = source(temp.path(), "160x90", "1");
    assert!((probe.duration_s - 3.966_667).abs() < 0.000_01);
    let mut req = request();
    req.reverse = true;
    let vf = build_video_filters(&req, &probe).unwrap().unwrap();
    let frames = ffmpeg(
        &[
            "-i",
            "source.nut",
            "-vf",
            &format!("{vf},format=gray"),
            "-an",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (160 * 90), 120);
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_unselected_long_track_does_not_extend_primary_timeline() {
    let temp = tempfile::tempdir().unwrap();
    for (extension, video_codec, audio_codec) in [
        ("mp4", "libx264", "aac"),
        ("mkv", "ffv1", "pcm_f32le"),
        ("nut", "ffv1", "pcm_f32le"),
    ] {
        let file = format!("selected.{extension}");
        ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=160x90:r=30:d=4",
                "-f",
                "lavfi",
                "-i",
                "sine=sample_rate=48000:duration=4",
                "-f",
                "lavfi",
                "-i",
                "sine=sample_rate=48000:duration=10",
                "-map",
                "0:v",
                "-map",
                "1:a",
                "-map",
                "2:a",
                "-c:v",
                video_codec,
                "-c:a",
                audio_codec,
                "-disposition:a:0",
                "default",
                "-disposition:a:1",
                "0",
                &file,
            ],
            temp.path(),
        );
        let mut probe = probe_video(temp.path().join(&file).to_string_lossy().into()).unwrap();
        let mut req = request();
        req.audio_enabled = true;
        req.reverse = true;
        req.input_path = temp.path().join(&file).to_string_lossy().into();
        prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap();
        assert_eq!(probe.audio_stream_index, Some(1));
        assert!(
            (probe.duration_s - 4.0).abs() < 0.04,
            "{extension}: {}",
            probe.duration_s
        );
        let vf = build_video_filters(&req, &probe).unwrap().unwrap();
        let af = build_audio_filters(&req, &probe).unwrap().unwrap();
        let frames = ffmpeg(
            &[
                "-i",
                &file,
                "-map",
                "0:0",
                "-vf",
                &format!("{vf},format=gray"),
                "-an",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        assert_eq!(frames.len() / (160 * 90), 120, "{extension}; {vf}");
        let samples = ffmpeg(
            &[
                "-i",
                &file,
                "-map",
                "0:1",
                "-af",
                &af,
                "-vn",
                "-c:a",
                "pcm_f32le",
                "-f",
                "f32le",
                "-",
            ],
            temp.path(),
        );
        assert!(
            (samples.len() as f64 / 4.0 / 48000.0 - 4.0).abs() < 0.04,
            "{extension}; {af}"
        );
    }
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_muted_and_high_rate_sources_preserve_existing_temporal_paths() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=160x90:r=30:d=2",
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=4",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_f32le",
            "muted.mkv",
        ],
        temp.path(),
    );
    let mut probe = probe_video(temp.path().join("muted.mkv").to_string_lossy().into()).unwrap();
    let mut req = request();
    req.reverse = true;
    req.input_path = temp.path().join("muted.mkv").to_string_lossy().into();
    prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap();
    let vf = build_video_filters(&req, &probe).unwrap().unwrap();
    assert!(!vf.contains("tpad"));
    let frames = ffmpeg(
        &[
            "-i",
            "muted.mkv",
            "-vf",
            &format!("{vf},format=gray"),
            "-an",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (160 * 90), 60);
    req.audio_enabled = true;
    req.format = OutputFormat::Mp3;
    assert!(
        !build_audio_filters(&req, &probe)
            .unwrap()
            .unwrap()
            .contains("apad")
    );

    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=16x16:r=5000:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=1",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_f32le",
            "dense.nut",
        ],
        temp.path(),
    );
    let mut probe = probe_video(temp.path().join("dense.nut").to_string_lossy().into()).unwrap();
    let mut req = request();
    req.input_path = temp.path().join("dense.nut").to_string_lossy().into();
    prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap(); // ordinary import/export
    req.reverse = true;
    prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe).unwrap(); // muted temporal export
    let vf = build_video_filters(&req, &probe).unwrap().unwrap();
    let frames = ffmpeg(
        &[
            "-i",
            "dense.nut",
            "-vf",
            &format!("{vf},format=gray"),
            "-an",
            "-fps_mode",
            "passthrough",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (16 * 16), 5000);
    req.audio_enabled = true;
    assert!(
        prepare_temporal_probe(&req, Path::new(req.input_path.trim()), &mut probe)
            .unwrap_err()
            .contains("complete NUT media duration")
    );
}

#[test]
#[ignore = "requires the bundled FFmpeg and FFprobe"]
fn bundled_rounded_endpoints_do_not_add_black_frames_before_reverse() {
    let temp = tempfile::tempdir().unwrap();
    for variable_timestamps in [false, true] {
        let duration = if variable_timestamps {
            "0.683667"
        } else {
            "0.666667"
        };
        let mut fixture = vec![
            "-f",
            "lavfi",
            "-i",
            "nullsrc=size=160x90:rate=12,geq=lum='16+200*N/7':cb=128:cr=128,format=yuv420p",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=990:sample_rate=48000",
            "-t",
            duration,
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "flac",
        ];
        if variable_timestamps {
            fixture.extend([
                "-vf",
                "settb=1/1000,setpts=PTS+if(gte(N\\,4)\\,17\\,0)",
                "-fps_mode",
                "passthrough",
                "-enc_time_base:v",
                "1/1000",
            ]);
        }
        fixture.push("rounded.mkv");
        ffmpeg(&fixture, temp.path());
        let input = temp.path().join("rounded.mkv");
        let probe = probe_video(input.to_string_lossy().into()).unwrap();
        assert_eq!(
            probe.video_end_s,
            Some(if variable_timestamps { 0.683 } else { 0.666 })
        );
        assert_eq!(
            probe.duration_s,
            if variable_timestamps { 0.684 } else { 0.667 }
        );
        let gray_means = |raw: &[u8]| -> Vec<f64> {
            assert_eq!(raw.len() % (160 * 90), 0);
            raw.as_chunks::<{ 160 * 90 }>()
                .0
                .iter()
                .map(|frame| frame.iter().map(|v| *v as u64).sum::<u64>() as f64 / (160.0 * 90.0))
                .collect()
        };
        let source_frames = ffmpeg(
            &[
                "-i",
                "rounded.mkv",
                "-map",
                "0:v:0",
                "-vf",
                "format=gray",
                "-fps_mode",
                "passthrough",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        let source_means = gray_means(&source_frames);
        assert_eq!(source_means.len(), 8);
        for loop_video in [false, true] {
            let mut req = request();
            req.input_path = input.to_string_lossy().into();
            req.audio_enabled = true;
            req.reverse = true;
            req.loop_video = loop_video;
            let filter = build_video_filters(&req, &probe).unwrap().unwrap();
            let baseline = if loop_video {
                "reverse,split[fv][rv0];[rv0]reverse[rv];[fv][rv]concat=n=2:v=1"
            } else {
                "reverse"
            };
            let frames = |graph: &str| {
                ffmpeg(
                    &[
                        "-i",
                        "rounded.mkv",
                        "-an",
                        "-vf",
                        graph,
                        "-fps_mode",
                        "passthrough",
                        "-f",
                        "framemd5",
                        "-",
                    ],
                    temp.path(),
                )
            };
            // Exact frame hashes and PTS must stay unchanged for both constant
            // and variable timestamps when the endpoints differ by one tick.
            assert_eq!(
                frames(&filter),
                frames(baseline),
                "vfr={variable_timestamps}, loop={loop_video}; {filter}"
            );
            if !variable_timestamps {
                let capabilities = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
                let codecs =
                    select_codec_plan(req.format, &capabilities.encoder_names, &req.advanced)
                        .unwrap();
                let plan = build_encode_command_plan(&req, &probe, codecs, None).unwrap();
                let mut args = build_single_pass_args("rounded.mkv", &req, &plan).unwrap();
                args.push("rounded.mp4".into());
                ffmpeg(
                    &args.iter().map(String::as_str).collect::<Vec<_>>(),
                    temp.path(),
                );
                let output = ffmpeg(
                    &[
                        "-i",
                        "rounded.mp4",
                        "-map",
                        "0:v:0",
                        "-vf",
                        "format=gray",
                        "-fps_mode",
                        "passthrough",
                        "-f",
                        "rawvideo",
                        "-",
                    ],
                    temp.path(),
                );
                let actual = gray_means(&output);
                let mut expected: Vec<f64> = source_means.iter().rev().copied().collect();
                if loop_video {
                    expected.extend_from_slice(&source_means);
                }
                assert_eq!(actual.len(), expected.len());
                assert!(
                    actual[0] >= 150.0,
                    "the packaged Reverse-before-Loop assertion must stay valid"
                );
                for (actual, expected) in actual.iter().zip(expected) {
                    assert!((actual - expected).abs() < 3.0);
                }
            }
        }
        if !variable_timestamps {
            // A real audio tail still needs black frames before the reversed
            // pictures; padding must include exactly that retained gap.
            ffmpeg(
                &[
                    "-i",
                    "rounded.mkv",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=990:sample_rate=48000:duration=1",
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "flac",
                    "real-gap.mkv",
                ],
                temp.path(),
            );
            let probe =
                probe_video(temp.path().join("real-gap.mkv").to_string_lossy().into()).unwrap();
            let mut req = request();
            req.audio_enabled = true;
            req.reverse = true;
            req.loop_video = true;
            let filter = build_video_filters(&req, &probe).unwrap().unwrap();
            let frames = ffmpeg(
                &[
                    "-i",
                    "real-gap.mkv",
                    "-an",
                    "-vf",
                    &format!("{filter},format=gray"),
                    "-fps_mode",
                    "passthrough",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                temp.path(),
            );
            let means = gray_means(&frames);
            assert_eq!(means.len(), 24);
            assert!(means[..4].iter().all(|mean| *mean < 1.0));
            assert!(means[4] >= 150.0);

            // A coarse source time base must not suppress a genuine one-frame
            // gap. MP4's 1/12 clock represents this 1/12-second tail exactly.
            ffmpeg(
                &[
                    "-i",
                    "rounded.mkv",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=990:sample_rate=48000:duration=0.75",
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:v",
                    "libx264",
                    "-video_track_timescale",
                    "12",
                    "-c:a",
                    "aac",
                    "coarse-gap.mp4",
                ],
                temp.path(),
            );
            let probe =
                probe_video(temp.path().join("coarse-gap.mp4").to_string_lossy().into()).unwrap();
            assert!((probe.duration_s - 0.75).abs() < 0.000_001);
            let filter = build_video_filters(&req, &probe).unwrap().unwrap();
            let frames = ffmpeg(
                &[
                    "-i",
                    "coarse-gap.mp4",
                    "-an",
                    "-vf",
                    &format!("{filter},format=gray"),
                    "-fps_mode",
                    "passthrough",
                    "-f",
                    "rawvideo",
                    "-",
                ],
                temp.path(),
            );
            let means = gray_means(&frames);
            assert_eq!(means.len(), 18);
            assert!(means[0] < 1.0);
            assert!(means[1] >= 150.0);
        }
    }
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_composes_trim_subtitles_geometry_speed_reverse_loop() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=64x48:r=24:d=1.5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=1.5",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "source.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("source.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
    for (index, speed, reverse, loop_video, trim, subtitle) in [
        (
            0,
            1.0,
            false,
            false,
            Some(Trim {
                start_s: 0.017,
                end_s: Some(0.225),
            }),
            false,
        ),
        (1, 2.0, true, false, None, false),
        (2, 0.5, false, true, None, false),
        (
            3,
            1.0,
            true,
            true,
            Some(Trim {
                start_s: 0.0,
                end_s: Some(0.25),
            }),
            true,
        ),
        (
            5,
            1.0,
            false,
            false,
            Some(Trim {
                start_s: -0.5,
                end_s: Some(0.25),
            }),
            false,
        ),
        (
            4,
            1.25,
            false,
            false,
            Some(Trim {
                start_s: 0.123,
                end_s: Some(1.123),
            }),
            true,
        ),
    ] {
        let mut req = request();
        req.input_path = source.to_string_lossy().into_owned();
        req.output_path = temp
            .path()
            .join(format!("ai-{index}.mp4"))
            .to_string_lossy()
            .into_owned();
        req.audio_enabled = true;
        req.speed = speed;
        req.reverse = reverse;
        req.loop_video = loop_video;
        req.trim = trim;
        req.crop = Some(Crop {
            x: 2,
            y: 2,
            width: 60,
            height: 44,
        });
        req.rotate_deg = 90;
        req.resize = Some(ResizeSettings {
            mode: ResizeMode::Custom,
            width_px: Some(48),
            height_px: Some(64),
            max_edge_px: None,
        });
        req.interpolation = Some(InterpolationSettings {
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let subtitle_stage = if subtitle {
            let srt = temp.path().join("source.srt");
            fs::write(&srt, "1\n00:00:00,000 --> 00:00:00,250\nAI timing\n").unwrap();
            Some(prepare_external_subtitle(&srt).unwrap())
        } else {
            None
        };
        if subtitle {
            req.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.into());
        }
        validate_interpolation_request(&req, &probe).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let group = Arc::new(Mutex::new(Vec::new()));
        let _scope = JobScope::enter(JobContext {
            cancel: cancel.clone(),
            child: group.clone(),
        });
        let mut prepared = crate::interpolation_media::prepare(
            &default_ffmpeg(),
            &source,
            &probe,
            &req,
            &model,
            &cancel,
            &group,
            |_| {},
        )
        .unwrap();
        prepared.format_mp4 = true;
        let selection =
            select_codec_plan(req.format, &runtime.encoder_names, &req.advanced).unwrap();
        let plan = build_encode_command_plan(&req, &probe, selection, None).unwrap();
        assert_eq!(plan.video_action, StreamAction::Encode);
        let mut args = build_single_pass_args(&source.to_string_lossy(), &req, &plan).unwrap();
        args.push(req.output_path.clone());
        let duration =
            estimate_output_duration_s(probe.duration_s, &req.trim, speed, loop_video).unwrap();
        run_ffmpeg_with_progress(
            None,
            1,
            1,
            &default_ffmpeg(),
            &args,
            subtitle_stage.as_ref().map(PreparedSubtitle::working_dir),
            EncodeProgressStep::single(EncodeProgressPhase::Encoding),
            (duration * 1e6) as u64,
            true,
            ffmpeg_run_limits(reverse).with_output_activity(Path::new(&req.output_path)),
            &cancel,
            &group,
            Some(&prepared),
        )
        .unwrap();
        let streams = probe_streams(Path::new(&req.output_path));
        let selected = select_primary_video_stream(&streams.streams).unwrap();
        assert_eq!((selected.width, selected.height), (Some(48), Some(64)));
        assert!(select_primary_audio_stream(&streams.streams).is_some());
        let mut command = command_no_window(&default_ffprobe());
        command
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
            .arg(&req.output_path);
        let result = run_command_output_with_timeout(
            command,
            "ffprobe",
            "VFL_FFPROBE_PATH",
            &default_ffprobe(),
            "AI output timestamps",
            Duration::from_secs(30),
        )
        .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let frames = json["frames"].as_array().unwrap();
        assert_eq!(
            frames.len() as u64,
            prepared.diagnostics.output_frames * if loop_video { 2 } else { 1 }
        );
        for (frame_index, frame) in frames.iter().enumerate() {
            let pts: f64 = frame["best_effort_timestamp_time"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                (pts - frame_index as f64 / 60.0).abs() < 0.00003,
                "case {index}, frame {frame_index}, pts {pts}"
            );
        }
    }
}

fn interpolation_fixture_export(
    req: &EncodeRequest,
    probe: &VideoProbe,
    model: &crate::interpolation_components::ResolvedModel,
) -> crate::interpolation_media::InterpolationDiagnostics {
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    let mut prepared = crate::interpolation_media::prepare(
        &default_ffmpeg(),
        Path::new(&req.input_path),
        probe,
        req,
        model,
        &cancel,
        &group,
        |_| {},
    )
    .unwrap();
    prepared.format_mp4 = req.format == OutputFormat::Mp4;
    let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
    let codec = select_codec_plan(req.format, &runtime.encoder_names, &req.advanced).unwrap();
    let staged_subtitle = req
        .subtitle_path
        .as_deref()
        .map(|path| prepare_external_subtitle(Path::new(path)).unwrap());
    let mut resolved = req.clone();
    if staged_subtitle.is_some() {
        resolved.subtitle_path = Some(EXTERNAL_SUBTITLE_FILE_NAME.into());
    }
    let plan = build_encode_command_plan(&resolved, probe, codec, None).unwrap();
    let mut args = build_single_pass_args(&req.input_path, &resolved, &plan).unwrap();
    if let Some(index) = args.iter().position(|arg| arg == "-crf") {
        args[index + 1] = "0".into();
    }
    args.push(req.output_path.clone());
    let duration =
        estimate_output_duration_s(probe.duration_s, &req.trim, req.speed, req.loop_video).unwrap();
    run_ffmpeg_with_progress(
        None,
        1,
        1,
        &default_ffmpeg(),
        &args,
        staged_subtitle.as_ref().map(PreparedSubtitle::working_dir),
        EncodeProgressStep::single(EncodeProgressPhase::Encoding),
        (duration * 1e6) as u64,
        true,
        ffmpeg_run_limits(req.reverse).with_output_activity(Path::new(&req.output_path)),
        &cancel,
        &group,
        Some(&prepared),
    )
    .unwrap();
    assert!(group.lock().unwrap().is_empty());
    prepared.diagnostics.clone()
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_retains_real_video_gaps_through_temporal_transforms() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-itsoffset",
            "0.1",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=64x48:r=20:d=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.5",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "gap.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("gap.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    assert!((probe.video_start_s - 0.1).abs() < 0.00001);
    assert!((probe.video_end_s.unwrap() - 0.3).abs() < 0.00001);
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    for (index, (start, reverse, loop_video, speed)) in [
        (0.0, false, false, 1.0),
        (0.0, true, false, 1.0),
        (0.0, false, true, 1.0),
        (0.15, true, true, 1.0),
        (0.15, true, false, 1.25),
        (0.0, false, true, 2.0),
    ]
    .into_iter()
    .enumerate()
    {
        let mut req = request();
        req.audio_enabled = true;
        req.input_path = source.to_string_lossy().into_owned();
        req.output_path = temp
            .path()
            .join(format!("gap-ai-{index}.mp4"))
            .to_string_lossy()
            .into_owned();
        req.trim = Some(Trim {
            start_s: start,
            end_s: Some(0.5),
        });
        req.reverse = reverse;
        req.loop_video = loop_video;
        req.speed = speed;
        req.interpolation = Some(InterpolationSettings {
            target_fps: 60,
            model_id: "rife-v4.25".into(),
        });
        let diagnostics = interpolation_fixture_export(&req, &probe, &model);
        let frames = ffmpeg(
            &[
                "-i",
                &req.output_path,
                "-vf",
                "format=gray",
                "-an",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        let size = 64 * 48;
        if frames.len() / size
            != diagnostics.output_frames as usize * if loop_video { 2 } else { 1 }
        {
            let output = Command::new(default_ffprobe())
                .args([
                    "-v",
                    "error",
                    "-count_frames",
                    "-show_streams",
                    "-show_format",
                    "-of",
                    "json",
                ])
                .arg(&req.output_path)
                .output()
                .unwrap();
            eprintln!(
                "gap case {index}, probe {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        assert_eq!(
            frames.len() / size,
            diagnostics.output_frames as usize * if loop_video { 2 } else { 1 },
            "gap case {index}"
        );
        let segment = diagnostics.output_frames as f64 / 60.0;
        for (frame_index, frame) in frames.chunks_exact(size).enumerate() {
            let time = (frame_index as f64 + 0.5) / 60.0;
            let local = if loop_video && time >= segment {
                2.0 * segment - time
            } else {
                time
            };
            let source_time = if reverse {
                0.5 - local * speed
            } else {
                start + local * speed
            };
            if (source_time - 0.1).abs() < 0.04 * speed || (source_time - 0.3).abs() < 0.04 * speed
            {
                continue;
            }
            let red = source_time > 0.1 && source_time < 0.3;
            let mean = frame.iter().map(|x| u64::from(*x)).sum::<u64>() as f64 / size as f64;
            assert_eq!(
                mean > 30.0,
                red,
                "gap case {index} frame {frame_index} source {source_time} mean {mean}"
            );
        }
    }
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_saturated_bt709_roundtrip_matches_existing_encoder() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "smptehdbars=s=128x96:r=24:d=0.25",
            "-vf",
            "setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=limited",
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-colorspace",
            "bt709",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "bars.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("bars.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    assert_eq!(
        probe.color_primaries.as_deref(),
        Some("bt709"),
        "Fixture must contain real source primaries"
    );
    assert_eq!(probe.color_transfer.as_deref(), Some("bt709"));
    assert_eq!(probe.color_space.as_deref(), Some("bt709"));
    assert_eq!(probe.color_range.as_deref(), Some("tv"));
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.input_path = source.to_string_lossy().into_owned();
    req.output_path = temp
        .path()
        .join("bars-ai.mp4")
        .to_string_lossy()
        .into_owned();
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    let diagnostics = interpolation_fixture_export(&req, &probe, &model);
    assert_eq!(
        diagnostics.inferred_frames, 0,
        "Static frames should not be inferred or changed"
    );
    ffmpeg(
        &[
            "-i",
            "bars.mkv",
            "-c:v",
            "libx264",
            "-crf",
            "0",
            "-pix_fmt",
            "yuv420p",
            "bars-baseline.mp4",
        ],
        temp.path(),
    );
    let baseline = ffmpeg(
        &[
            "-i",
            "bars-baseline.mp4",
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
    let actual = ffmpeg(
        &[
            "-i",
            "bars-ai.mp4",
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
    assert_eq!(actual.len(), baseline.len());
    let mean = actual
        .iter()
        .zip(baseline.iter())
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum::<u64>() as f64
        / actual.len() as f64;
    assert!(
        mean < 2.0,
        "Saturated BT709 roundtrip mean RGB difference {mean}"
    );
    let output = probe_video(req.output_path).unwrap();
    assert_eq!(output.color_space.as_deref(), Some("bt709"));
    assert_eq!(output.color_primaries.as_deref(), Some("bt709"));
    assert_eq!(output.color_transfer.as_deref(), Some("bt709"));
    assert_eq!(output.color_range.as_deref(), Some("tv"));
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_cuts_never_infer_excluded_picture_mixtures_and_cancel_quiesces() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=red:s=64x48:r=10:d=0.1",
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=64x48:r=10:d=0.1",
            "-filter_complex",
            "[0:v][1:v]concat=n=2:v=1:a=0[v]",
            "-map",
            "[v]",
            "-c:v",
            "ffv1",
            "cut.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("cut.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.input_path = source.to_string_lossy().into_owned();
    req.output_path = temp
        .path()
        .join("cut-ai.mp4")
        .to_string_lossy()
        .into_owned();
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    let diagnostics = interpolation_fixture_export(&req, &probe, &model);
    assert_eq!(diagnostics.inferred_frames, 0);
    assert_eq!(diagnostics.cut_pairs, 1);
    let frames = ffmpeg(
        &[
            "-i",
            &req.output_path,
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (64 * 48 * 3), 12);
    for (index, frame) in frames.as_chunks::<{ 64 * 48 * 3 }>().0.iter().enumerate() {
        let r = frame
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| u64::from(p[0]))
            .sum::<u64>()
            / (64 * 48);
        let b = frame
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| u64::from(p[2]))
            .sum::<u64>()
            / (64 * 48);
        assert!(
            if index < 6 {
                r > 180 && b < 30
            } else {
                b > 180 && r < 30
            },
            "cut frame {index} R{r} B{b}"
        );
    }
    // Cancel a real inference pair rather than the cut-only optimization.
    // Distinct gray steps stay far below the cut threshold; target pictures
    // one and two therefore require successful native stream responses.
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x48:r=24:d=0.5",
            "-vf",
            "geq=lum='40+10*N':cb=128:cr=128",
            "-c:v",
            "ffv1",
            "cancel-motion.mkv",
        ],
        temp.path(),
    );
    let cancel_source = temp.path().join("cancel-motion.mkv");
    let cancel_probe = probe_video(cancel_source.to_string_lossy().into_owned()).unwrap();
    let first_pair = ffmpeg(
        &[
            "-i",
            "cancel-motion.mkv",
            "-frames:v",
            "2",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    let frame_size = 64 * 48 * 3;
    assert_eq!(first_pair.len(), frame_size * 2);
    assert_ne!(&first_pair[..frame_size], &first_pair[frame_size..]);
    assert!(!crate::interpolation_timeline::is_scene_cut(
        &first_pair[..frame_size],
        &first_pair[frame_size..]
    ));
    req.input_path = cancel_source.to_string_lossy().into_owned();
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    req.output_path = temp
        .path()
        .join("never-published.mp4")
        .to_string_lossy()
        .into_owned();
    let started = Instant::now();
    let result = crate::interpolation_media::prepare(
        &default_ffmpeg(),
        &cancel_source,
        &cancel_probe,
        &req,
        &model,
        &cancel,
        &group,
        |fraction| {
            if fraction >= 0.1 {
                cancel.store(true, Ordering::Relaxed);
                process::kill_process_group(&group);
            }
        },
    );
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(group.lock().unwrap().is_empty());
    assert!(!Path::new(&req.output_path).exists());
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_reuses_one_lossless_spool_for_four_two_pass_candidates() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=64x48:r=24:d=0.25",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.25",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "source.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("source.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.input_path = source.to_string_lossy().into_owned();
    req.audio_enabled = true;
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    req.strict_fit = true;
    req.size_limit_mb = 0.1;
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    // Exactly one native preparation. All eight encoder passes below receive
    // this immutable file and cannot invoke the inference module themselves.
    let prepared = crate::interpolation_media::prepare(
        &default_ffmpeg(),
        &source,
        &probe,
        &req,
        &model,
        &cancel,
        &group,
        |_| {},
    )
    .unwrap();
    assert!(prepared.diagnostics.inferred_frames > 0);
    let saved = fs::read(&prepared.path).unwrap();
    let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
    let selection = select_codec_plan(req.format, &runtime.encoder_names, &req.advanced).unwrap();
    let plan = build_mutated_size_reencode_plan(&req, &probe, selection, None).unwrap();
    for (index, bitrate) in [120, 100, 80, 60].into_iter().enumerate() {
        let log = temp.path().join(format!("plan{index}"));
        for pass in [1, 2] {
            let output = temp.path().join(format!("candidate-{index}.mp4"));
            let mut args = base_ffmpeg_args(&req.input_path);
            push_absolute_map(&mut args, probe.video_stream_index);
            args.extend([
                "-c:v".into(),
                "libx264".into(),
                "-b:v".into(),
                format!("{bitrate}k"),
                "-pass".into(),
                pass.to_string(),
                "-passlogfile".into(),
                log.to_string_lossy().into_owned(),
                "-pix_fmt".into(),
                "yuv420p".into(),
            ]);
            if let Some(filters) = &plan.video_filters {
                args.extend(["-vf".into(), filters.clone()]);
            }
            if pass == 1 {
                args.extend(["-an".into(), "-f".into(), "null".into(), null_sink().into()]);
            } else {
                push_absolute_map(&mut args, probe.audio_stream_index.unwrap());
                args.extend([
                    "-c:a".into(),
                    "aac".into(),
                    "-b:a".into(),
                    "96k".into(),
                    output.to_string_lossy().into_owned(),
                ]);
            }
            run_ffmpeg_with_progress(
                None,
                1,
                1,
                &default_ffmpeg(),
                &args,
                None,
                EncodeProgressStep::new(
                    EncodeProgressPhase::Encoding,
                    (index * 2 + pass) as u8,
                    8,
                    pass as u8,
                    2,
                ),
                250000,
                false,
                if pass == 1 {
                    ffmpeg_run_limits(false)
                } else {
                    ffmpeg_run_limits(false).with_output_activity(&output)
                },
                &cancel,
                &group,
                Some(&prepared),
            )
            .unwrap();
            if pass == 2 {
                assert!(
                    probe_video(output.to_string_lossy().into_owned())
                        .unwrap()
                        .has_audio
                );
            }
        }
        assert_eq!(fs::read(&prepared.path).unwrap(), saved);
    }
    assert!(group.lock().unwrap().is_empty());
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_vfr_clock_and_excluded_trim_context_are_exact() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=black:s=32x32:r=25:d=0.12",
            "-vf",
            "drawbox=color=red:t=fill:enable='eq(n,0)',drawbox=color=blue:t=fill:enable='eq(n,1)',drawbox=color=green:t=fill:enable='eq(n,2)',settb=1/1000,setpts=if(eq(N\\,0)\\,0\\,if(eq(N\\,1)\\,40\\,100))",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "1:1000",
            "-c:v",
            "ffv1",
            "vfr.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("vfr.mkv");
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    assert!((probe.duration_s - 0.14).abs() < 0.00001);
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.input_path = source.to_string_lossy().into_owned();
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    for trimmed in [false, true] {
        req.output_path = temp
            .path()
            .join(format!("vfr-{trimmed}.mp4"))
            .to_string_lossy()
            .into_owned();
        req.trim = trimmed.then_some(Trim {
            start_s: 0.01,
            end_s: Some(0.09),
        });
        let diagnostic = interpolation_fixture_export(&req, &probe, &model);
        assert_eq!(diagnostic.input_frames, if trimmed { 1 } else { 3 });
        assert_eq!(diagnostic.inferred_frames, 0);
        let frames = ffmpeg(
            &[
                "-i",
                &req.output_path,
                "-fps_mode",
                "passthrough",
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ],
            temp.path(),
        );
        assert_eq!(frames.len() / (32 * 32 * 3), if trimmed { 5 } else { 9 });
        for (index, frame) in frames.as_chunks::<{ 32 * 32 * 3 }>().0.iter().enumerate() {
            let channel = if trimmed || (3..6).contains(&index) {
                2
            } else if index < 3 {
                0
            } else {
                1
            };
            let center = &frame[(16 * 32 + 16) * 3..][..3];
            assert!(
                center[channel] > if channel == 1 { 80 } else { 180 },
                "VFR frame {index} {center:?}"
            );
            for other in 0..3 {
                if other != channel {
                    assert!(center[other] < 30, "VFR frame {index} {center:?}");
                }
            }
        }
    }
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_source_epoch_subtitle_pixels_survive_point123_trim() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=128x96:r=24:d=1.5",
            "-c:v",
            "ffv1",
            "blue.mkv",
        ],
        temp.path(),
    );
    let source = temp.path().join("blue.mkv");
    let srt = temp.path().join("cue.srt");
    fs::write(&srt, "1\n00:00:00,200 --> 00:00:00,233\nX\n").unwrap();
    let probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.input_path = source.to_string_lossy().into_owned();
    req.output_path = temp
        .path()
        .join("subtitle.mp4")
        .to_string_lossy()
        .into_owned();
    req.trim = Some(Trim {
        start_s: 0.123,
        end_s: Some(1.123),
    });
    req.subtitle_path = Some(srt.to_string_lossy().into_owned());
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    let diagnostic = interpolation_fixture_export(&req, &probe, &model);
    assert_eq!(diagnostic.output_frames, 60);
    let frames = ffmpeg(
        &[
            "-i",
            &req.output_path,
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (128 * 96 * 3), 60);
    for (index, frame) in frames.as_chunks::<{ 128 * 96 * 3 }>().0.iter().enumerate() {
        // This fixed font scales to about eight pixels in the 128x96
        // fixture. Its antialiased glyph peaks at 170, not pure white.
        // Non-cue blue frames have exactly zero RGB minimum brightness.
        let peak = frame
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| *pixel.iter().min().unwrap())
            .max()
            .unwrap();
        assert_eq!(
            peak > 150,
            matches!(index, 5 | 6),
            "subtitle frame {index}: glyph peak {peak}"
        );
        if !matches!(index, 5 | 6) {
            assert!(
                peak <= 3,
                "Unexpected subtitle pixels in frame {index}: {peak}"
            );
        }
    }
}

#[test]
#[ignore = "requires the built native interpolation worker and catalog models"]
fn bundled_interpolation_nut_endpoint_probe_preserves_normal_export_gaps() {
    let temp = tempfile::tempdir().unwrap();
    ffmpeg(
        &[
            "-itsoffset",
            "0.1",
            "-f",
            "lavfi",
            "-i",
            "color=red:s=64x48:r=20:d=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=0.5",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c:v",
            "ffv1",
            "-c:a",
            "pcm_s16le",
            "gap.nut",
        ],
        temp.path(),
    );
    let source = temp.path().join("gap.nut");
    let mut probe = probe_video(source.to_string_lossy().into_owned()).unwrap();
    let model = crate::interpolation_components::resolve_test_model("rife-v4.25").unwrap();
    let mut req = request();
    req.audio_enabled = true;
    req.input_path = source.to_string_lossy().into_owned();
    req.output_path = temp
        .path()
        .join("nut-ai.mp4")
        .to_string_lossy()
        .into_owned();
    req.interpolation = Some(InterpolationSettings {
        target_fps: 60,
        model_id: "rife-v4.25".into(),
    });
    // Ordinary AI (without reverse/loop) still needs exact selected endpoints.
    assert!(!req.reverse && !req.loop_video);
    prepare_temporal_probe(&req, &source, &mut probe).unwrap();
    assert!((probe.duration_s - 0.5).abs() < 0.00001);
    assert!((probe.video_end_s.unwrap() - 0.3).abs() < 0.00001);
    let diagnostic = interpolation_fixture_export(&req, &probe, &model);
    assert_eq!(diagnostic.output_frames, 30);
    let frames = ffmpeg(
        &[
            "-i",
            &req.output_path,
            "-fps_mode",
            "passthrough",
            "-vf",
            "format=gray",
            "-an",
            "-f",
            "rawvideo",
            "-",
        ],
        temp.path(),
    );
    assert_eq!(frames.len() / (64 * 48), 30);
    for (index, frame) in frames.as_chunks::<{ 64 * 48 }>().0.iter().enumerate() {
        let mean =
            frame.iter().map(|pixel| u64::from(*pixel)).sum::<u64>() as f64 / (64 * 48) as f64;
        assert_eq!(
            mean > 30.0,
            (6..18).contains(&index),
            "NUT gap frame {index} mean{mean}"
        );
    }
}
