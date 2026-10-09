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
            prepare_temporal_probe(&req, &mut probe).unwrap();
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
                .chunks_exact(4)
                .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
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
                prepare_temporal_probe(&req, &mut probe).unwrap();
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
            prepare_temporal_probe(&req, &mut probe).unwrap();
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
        prepare_temporal_probe(&req, &mut probe).unwrap();
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
    prepare_temporal_probe(&req, &mut probe).unwrap();
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
    prepare_temporal_probe(&req, &mut probe).unwrap(); // ordinary import/export
    req.reverse = true;
    prepare_temporal_probe(&req, &mut probe).unwrap(); // muted temporal export
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
        prepare_temporal_probe(&req, &mut probe)
            .unwrap_err()
            .contains("complete NUT media duration")
    );
}
