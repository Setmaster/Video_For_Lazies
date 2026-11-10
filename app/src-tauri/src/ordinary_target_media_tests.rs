//! Ordinary target-duration exports preserve source pictures and presentation time.
use super::*;

const WIDTH: usize = 128;
const HEIGHT: usize = 96;

fn media(args: &[&str], directory: &Path) -> Vec<u8> {
    let bin = default_ffmpeg();
    let mut command = command_no_window(&bin);
    command
        .args(["-nostdin", "-y", "-v", "error"])
        .args(args)
        .current_dir(directory);
    let result = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &bin,
        "Ordinary target fixture",
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

fn fixture(directory: &Path, variable: bool) -> PathBuf {
    let count = if variable { 119 } else { 120 };
    let mut rgb = Vec::with_capacity(count * WIDTH * HEIGHT * 3);
    for frame in 0..count {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let mut value = (50 + ((x + frame * 2) % 64) * 2 + y % 16) as u8;
                // Seven large, neutral marker cells encode the source frame ID.
                // Reading their centers survives normal H.264/MPEG-4 quality.
                if (4..28).contains(&y) {
                    for bit in 0..7 {
                        if (4 + bit * 17..19 + bit * 17).contains(&x) {
                            value = if frame & (1 << bit) == 0 { 20 } else { 235 };
                        }
                    }
                }
                rgb.extend([value; 3]);
            }
        }
    }
    let stem = if variable { "vfr" } else { "cfr" };
    let raw = format!("{stem}.rgb");
    let filename = format!("{stem}.mp4");
    fs::write(directory.join(&raw), rgb).unwrap();
    let duration = if variable { 89. / 15. } else { 4. };
    let audio = format!(
        "aevalsrc=0.3*sin(2*PI*(170*t+9*t*t))+0.1*sin(2*PI*997*t):s=48000:d={duration:.12}"
    );
    let clock = if variable {
        "settb=expr=1/30000,setpts=floor(N/2)*3000+(N-2*floor(N/2))*1000"
    } else {
        "settb=expr=1/30000,setpts=N*1000"
    };
    media(
        &[
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            "128x96",
            "-framerate",
            "30",
            "-i",
            &raw,
            "-f",
            "lavfi",
            "-i",
            &audio,
            "-vf",
            clock,
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "1:30000",
            "-c:v",
            "libx264",
            "-crf",
            "18",
            "-preset",
            "medium",
            "-x264-params",
            "b-adapt=0:scenecut=0",
            "-video_track_timescale",
            "30000",
            "-c:a",
            "aac",
            &filename,
        ],
        directory,
    );
    directory.join(filename)
}

fn frame_ids(input: &Path, directory: &Path) -> Vec<usize> {
    // The full VFR boomerang exceeds the bounded inspection stdout allowance.
    // Decode this self-owned fixture to disk, then inspect its marker cells.
    media(
        &[
            "-i",
            &input.to_string_lossy(),
            "-map",
            "0:v:0",
            "-fps_mode",
            "passthrough",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "decoded.rgb",
        ],
        directory,
    );
    let rgb = fs::read(directory.join("decoded.rgb")).unwrap();
    let size = WIDTH * HEIGHT * 3;
    assert_eq!(rgb.len() % size, 0);
    rgb.chunks_exact(size)
        .map(|picture| {
            let mut id = 0;
            for bit in 0..7 {
                let mut total = 0usize;
                for y in 10..22 {
                    for x in 8 + bit * 17..15 + bit * 17 {
                        total += usize::from(picture[(y * WIDTH + x) * 3]);
                    }
                }
                if total > 128 * 12 * 7 {
                    id |= 1 << bit;
                }
            }
            id
        })
        .collect()
}

fn probe_json(input: &Path, entries: &str, frames: bool) -> serde_json::Value {
    let bin = default_ffprobe();
    let mut command = command_no_window(&bin);
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            if frames {
                "-show_frames"
            } else {
                "-show_packets"
            },
            "-show_streams",
            "-show_entries",
            entries,
            "-of",
            "json",
        ])
        .arg(input);
    let result = run_command_output_with_timeout(
        command,
        "ffprobe",
        "VFL_FFPROBE_PATH",
        &bin,
        "Ordinary target timestamps",
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(result.status.success());
    serde_json::from_slice(&result.stdout).unwrap()
}

struct Timing {
    pts: Vec<f64>,
    duration: f64,
    duration_num: i128,
    duration_den: i128,
    end: f64,
    b_frames: usize,
    time_base: String,
}

fn timing(input: &Path) -> Timing {
    let json = probe_json(
        input,
        "frame=best_effort_timestamp,best_effort_timestamp_time,duration,duration_time,pict_type:stream=duration_ts,duration,time_base",
        true,
    );
    let frames = json["frames"].as_array().unwrap();
    let time_base = json["streams"][0]["time_base"].as_str().unwrap();
    let (num, den) = time_base.split_once('/').unwrap();
    let duration_den = den.parse::<i128>().unwrap();
    let duration_num = i128::from(json["streams"][0]["duration_ts"].as_i64().unwrap())
        * num.parse::<i128>().unwrap();
    let scale = num.parse::<f64>().unwrap() / den.parse::<f64>().unwrap();
    let pts: Vec<_> = frames
        .iter()
        .map(|frame| frame["best_effort_timestamp"].as_i64().unwrap() as f64 * scale)
        .collect();
    assert!(
        pts.windows(2).all(|p| p[0] < p[1]),
        "presentation timestamps must increase"
    );
    let last = frames.last().unwrap();
    let end = pts.last().unwrap() + last["duration"].as_i64().unwrap() as f64 * scale;
    let packets = probe_json(
        input,
        "packet=pts,dts,duration,pts_time,dts_time,duration_time",
        false,
    );
    if let Some(evidence) = std::env::var_os("VFL_MEDIA_EVIDENCE_DIR") {
        let evidence = PathBuf::from(evidence);
        fs::create_dir_all(&evidence).unwrap();
        let filename = input.file_name().unwrap().to_string_lossy();
        fs::write(
            evidence.join(format!("{filename}.frames.json")),
            serde_json::to_vec_pretty(&json).unwrap(),
        )
        .unwrap();
        fs::write(
            evidence.join(format!("{filename}.packets.json")),
            serde_json::to_vec_pretty(&packets).unwrap(),
        )
        .unwrap();
    }
    let dts: Vec<_> = packets["packets"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|packet| packet["dts"].as_i64())
        .collect();
    assert!(
        dts.windows(2).all(|p| p[0] < p[1]),
        "encoded packet DTS must increase, including B-frame reorder"
    );
    Timing {
        pts,
        duration: duration_num as f64 / duration_den as f64,
        duration_num,
        duration_den,
        end,
        b_frames: frames
            .iter()
            .filter(|frame| frame["pict_type"] == "B")
            .count(),
        time_base: json["streams"][0]["time_base"].as_str().unwrap().into(),
    }
}

fn audio_seam(input: &Path, video_seam_s: f64, directory: &Path) -> f64 {
    let raw = media(
        &[
            "-i",
            &input.to_string_lossy(),
            "-map",
            "0:a:0",
            "-c:a",
            "pcm_f64le",
            "-f",
            "f64le",
            "-",
        ],
        directory,
    );
    let samples: Vec<_> = raw
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| f64::from_le_bytes(*b))
        .collect();
    let expected = (video_seam_s * 48000.).round() as isize;
    let mut best = (-1., 0isize);
    // Search wider than the accepted 25 ms, so the bound is a real assertion.
    for offset in -4800isize..=4800 {
        let seam = (expected + offset) as usize;
        let before = &samples[seam - 3072..seam - 2048];
        let after = &samples[seam + 2048..seam + 3072];
        let cross = before
            .iter()
            .zip(after.iter().rev())
            .map(|(a, b)| a * b)
            .sum::<f64>();
        let ea = before.iter().map(|v| v * v).sum::<f64>();
        let eb = after.iter().map(|v| v * v).sum::<f64>();
        let score = cross / (ea * eb).sqrt();
        if score > best.0 {
            best = (score, offset);
        }
    }
    assert!(best.0 > 0.90, "boomerang audio seam correlation {}", best.0);
    assert!(
        best.1.abs() <= 1200,
        "audio seam differs by {} samples",
        best.1
    );
    eprintln!(
        "Ordinary VFR audio seam: correlation={}, offset={} samples",
        best.0, best.1
    );
    (expected + best.1) as f64 / 48000.
}

#[test]
fn concat_timing_metric_rounds_error_at_one_microsecond_precision() {
    assert_eq!(
        timing_error_nearest_microsecond(5_958_333_500, 1_000_000_000, 89, 15),
        25_000
    );
    assert_eq!(
        timing_error_nearest_microsecond(25_000_600, 1_000_000_000, 0, 1),
        25_001
    );
    assert_eq!(
        timing_error_nearest_microsecond(0, 1, 25_000_600, 1_000_000_000),
        25_001
    );
    // Separately rounding these endpoints would yield 25,001 us, incorrectly.
    assert_eq!(
        timing_error_nearest_microsecond(25_000_600, 1_000_000_000, 400, 1_000_000_000),
        25_000
    );
}

/// Quantize the error itself to concat's 1 us clock, not either endpoint.
fn timing_error_nearest_microsecond(
    actual_num: i128,
    actual_den: i128,
    target_num: i128,
    target_den: i128,
) -> i128 {
    let denominator = actual_den * target_den;
    let error = (actual_num * target_den - target_num * actual_den).abs();
    (error * 1_000_000 + denominator / 2) / denominator
}

#[test]
#[ignore = "requires pinned bundled FFmpeg/FFprobe; no model or GPU"]
fn bundled_ordinary_targets_preserve_all_pictures_cadence_caps_and_audio() {
    require_target_setts(&default_ffmpeg()).unwrap();
    let directory = TempDir::new().unwrap();
    let cfr = fixture(directory.path(), false);
    let vfr = fixture(directory.path(), true);
    assert_eq!(
        frame_ids(&cfr, directory.path()),
        (0..120).collect::<Vec<_>>()
    );
    assert_eq!(
        frame_ids(&vfr, directory.path()),
        (0..119).collect::<Vec<_>>()
    );
    let source_vfr = timing(&vfr);
    // A reordered VFR decoder can infer the final AVFrame duration from
    // decode-order packets. The MP4 video-track duration owns its end clock.
    assert_eq!(source_vfr.duration_num * 15, 89 * source_vfr.duration_den);
    assert!((source_vfr.pts.last().unwrap() - 5.9).abs() < 0.00001);
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    for (label, target, cap, variable, mpeg4) in [
        ("fast", 2., None, false, false),
        ("slow", 8., None, false, false),
        ("binding-cap", 2., Some(30), false, false),
        ("nonbinding-cap", 2., Some(120), false, false),
        ("mpeg4", 2., None, false, true),
        ("vfr-reverse-loop", 89. / 15., None, true, false),
    ] {
        let input = if variable { &vfr } else { &cfr };
        let probe = probe_video(input.to_string_lossy().into()).unwrap();
        let mut request = super::tests::base_request();
        request.input_path = input.to_string_lossy().into();
        request.size_limit_mb = 0.;
        request.target_duration_s = Some(target);
        request.reverse = variable;
        request.loop_video = variable;
        request.advanced.frame_rate_cap_fps = cap;
        request.advanced.video_codec = Some(if mpeg4 {
            VideoCodecPreference::Mpeg4
        } else {
            VideoCodecPreference::H264
        });
        assert!(request.interpolation.is_none());
        let retained = retained_source_duration_s(&request, &probe).unwrap();
        let diagnostics = resolve_target_duration(&mut request, retained)
            .unwrap()
            .unwrap();
        let runtime = cached_ffmpeg_capabilities(&default_ffmpeg()).unwrap();
        let codecs =
            select_codec_plan(request.format, &runtime.encoder_names, &request.advanced).unwrap();
        let plan = build_encode_command_plan(&request, &probe, codecs, None).unwrap();
        assert_eq!(plan.video_action, StreamAction::Encode);
        assert_eq!(plan.audio_action, StreamAction::Encode);
        let output = directory.path().join(format!("ordinary-{label}.mp4"));
        let mut args = build_single_pass_args(&request.input_path, &request, &plan).unwrap();
        args.push(output.to_string_lossy().into());
        if let Some(evidence) = std::env::var_os("VFL_MEDIA_EVIDENCE_DIR") {
            let evidence = PathBuf::from(evidence);
            fs::create_dir_all(&evidence).unwrap();
            fs::write(
                evidence.join(format!("ordinary-{label}.args.json")),
                serde_json::to_vec_pretty(&args).unwrap(),
            )
            .unwrap();
        }
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
            ffmpeg_run_limits(variable).with_output_activity(&output),
            &cancel,
            &group,
            None,
        )
        .unwrap();
        if let Some(evidence) = std::env::var_os("VFL_MEDIA_EVIDENCE_DIR") {
            let evidence = PathBuf::from(evidence);
            fs::create_dir_all(&evidence).unwrap();
            fs::copy(
                input,
                evidence.join(if variable {
                    "source-vfr.mp4"
                } else {
                    "source-cfr.mp4"
                }),
            )
            .unwrap();
            fs::copy(&output, evidence.join(format!("ordinary-{label}.mp4"))).unwrap();
        }
        let ids = frame_ids(&output, directory.path());
        let times = timing(&output);
        let result = probe_video(output.to_string_lossy().into()).unwrap();
        assert!(result.has_audio);
        assert_eq!(ids.len(), times.pts.len());
        eprintln!(
            "Ordinary measured {label}: source_duration={}, pictures={}, final_pts={:?}, decoded_frame_end={}, video_stream_duration={}, audio_end={:?}, speed={}",
            probe.duration_s,
            ids.len(),
            times.pts.last(),
            times.end,
            times.duration,
            result.audio_end_s,
            request.speed
        );
        if variable {
            let expected: Vec<_> = (0..119).rev().chain(0..119).collect();
            assert!(
                ids == expected,
                "{label}: source IDs or boomerang endpoints changed"
            );
            let differences: Vec<_> = times.pts.windows(2).map(|p| p[1] - p[0]).collect();
            let short = differences
                .iter()
                .filter(|d| (**d - 1. / 60.).abs() < 0.0001)
                .count();
            let long = differences
                .iter()
                .filter(|d| (**d - 1. / 30.).abs() < 0.0001)
                .count();
            assert!(
                short >= 110 && long >= 110,
                "VFR cadence collapsed: short={short}, long={long}"
            );
            assert!(differences.len() - short - long <= 3);
            let video_seam = times.pts[119];
            let seam = audio_seam(&output, video_seam, directory.path());
            assert!((seam - video_seam).abs() < 0.025);
            eprintln!(
                "Ordinary {label}: short={short}, long={long}, video_seam={video_seam}, measured_audio_seam={seam}"
            );
            let duration_error_us =
                timing_error_nearest_microsecond(times.duration_num, times.duration_den, 89, 15);
            eprintln!(
                "Ordinary {label}: raw_video_error_s={}, nearest_microsecond_error_us={duration_error_us}, nominal_budget_us=25000, raw_audio_video_end_gap_s={}",
                (times.duration - target).abs(),
                (result.audio_end_s.unwrap() - times.duration).abs()
            );
            assert!(
                duration_error_us <= 25_000,
                "{label}: nearest-us timing error {duration_error_us}, video duration {} target {target}",
                times.duration
            );
            assert!(
                (result.audio_end_s.unwrap() - target).abs() < 0.025,
                "{label}: audio end differs from requested duration"
            );
        } else {
            if cap == Some(30) {
                assert_eq!(ids.len(), 60);
                assert!(ids.windows(2).all(|p| p[0] < p[1]));
                assert!(
                    ids[0] <= 1 && *ids.last().unwrap() >= 118,
                    "binding cap lost source endpoints"
                );
            } else {
                assert!(
                    ids == (0..120).collect::<Vec<_>>(),
                    "{label}: target export dropped, duplicated or reordered source pictures"
                );
            }
            assert!(
                (times.end - target).abs() < 0.0001,
                "{label}: final picture end {} target {target}",
                times.end
            );
            assert!(
                (times.duration - target).abs() < 0.0001,
                "{label}: video stream duration {}",
                times.duration
            );
            let post_frame = if target == 8. {
                1. / 15.
            } else if cap == Some(30) {
                1. / 30.
            } else {
                1. / 60.
            };
            assert!(
                (result.audio_end_s.unwrap() - times.end).abs() < post_frame,
                "{label}: AAC end {:?}, video end {}",
                result.audio_end_s,
                times.end
            );
        }
        if mpeg4 {
            assert_eq!(times.time_base, "1/60000");
        } else {
            assert!(
                times.b_frames > 0,
                "{label}: H.264 fixture/export must exercise B frames"
            );
        }
        assert!(group.lock().unwrap().is_empty());
        eprintln!(
            "Ordinary {label}: pictures={}, video_end={}, stream_duration={}, audio_end={:?}, B_frames={}, time_base={}, diagnostics={}",
            ids.len(),
            times.end,
            times.duration,
            result.audio_end_s,
            times.b_frames,
            times.time_base,
            serde_json::to_string(&diagnostics).unwrap()
        );
    }
}
