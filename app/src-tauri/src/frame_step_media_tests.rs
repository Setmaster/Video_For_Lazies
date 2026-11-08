//! Opt-in tests using generated media and an optional authorized read-only input.
use super::*;

fn media(args: &[&str], cwd: &Path) {
    let binary = default_ffmpeg();
    let mut command = command_no_window(&binary);
    command
        .args(["-nostdin", "-v", "error", "-y"])
        .args(args)
        .current_dir(cwd);
    let result = run_command_output_with_timeout(
        command,
        "ffmpeg",
        "VFL_FFMPEG_PATH",
        &binary,
        "Frame timing fixture",
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn request(path: &Path, duration_s: f64, time_s: f64, step: i32) -> FrameStepRequest {
    let identity = crate::media_source::SourceIdentity::capture(path).unwrap();
    FrameStepRequest {
        path: path.to_string_lossy().into_owned(),
        source_identity: identity.fingerprint().into(),
        video_stream_index: 0,
        min_s: 0.0,
        max_s: duration_s,
        duration_s,
        time_s,
        step,
    }
}

#[test]
#[ignore = "requires FFmpeg/FFprobe; generated CFR, VFR, B-frame and offset media"]
fn native_frame_step_media() {
    let temp = tempfile::tempdir().unwrap();
    media(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=192x108:rate=30:duration=8",
            "-vf",
            "setpts=if(lt(N\\,21)\\,N\\,N+3)/(30*TB)",
            "-fps_mode",
            "vfr",
            "-c:v",
            "libx264",
            "-g",
            "60",
            "-bf",
            "3",
            "-video_track_timescale",
            "30000",
            "vfr.mp4",
        ],
        temp.path(),
    );
    let path = temp.path().join("vfr.mp4");
    let next = step_source_frame(request(&path, 8.1, 20.0 / 30.0, 1)).unwrap();
    assert_eq!(next.time_s, 0.8);
    assert_eq!(
        step_source_frame(request(&path, 8.1, next.time_s, -1))
            .unwrap()
            .time_s,
        20.0 / 30.0
    );
    assert_eq!(
        step_source_frame(request(&path, 8.1, 0.0, 10))
            .unwrap()
            .time_s,
        1.0 / 3.0
    );
    // Common format origin 10.5s, video offset 2s. Neither is a proxy offset.
    media(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=96x54:rate=30:duration=3",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=stereo:d=5",
            "-vf",
            "setpts=PTS+2/TB",
            "-c:v",
            "libx264",
            "-c:a",
            "pcm_s16le",
            "-output_ts_offset",
            "10.5",
            "offset.mkv",
        ],
        temp.path(),
    );
    let offset = temp.path().join("offset.mkv");
    let binary = default_ffprobe();
    let meta = metadata(&offset, 0, Instant::now() + COMMAND_BUDGET, &binary).unwrap();
    assert_eq!(meta.origin_us, 10_500_000);
    assert_eq!(timestamp(meta.first_pts.unwrap(), &meta), 2.0);
    assert!(
        (step_source_frame(request(&offset, 5.0, 2.0, 1))
            .unwrap()
            .time_s
            - 2.033)
            .abs()
            < 1e-12
    );
    // Full-resolution input must not decode the maximum 1024 packets for a
    // nearby step: the initial 64-packet window proves this neighbor.
    media(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:size=3840x2160:rate=30:duration=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-g",
            "30",
            "-bf",
            "0",
            "-threads",
            "2",
            "large.mp4",
        ],
        temp.path(),
    );
    let large = temp.path().join("large.mp4");
    let req = request(&large, 8.0, 4.0, 10);
    let start = Instant::now();
    let meta = metadata(&large, 0, start + COMMAND_BUDGET, &binary).unwrap();
    let window = window(&large, &req, meta, start + COMMAND_BUDGET, &binary, 64).unwrap();
    assert!(window.times.len() <= 64);
    assert!(!window.at_end);
    assert_eq!(select(&window, &req).unwrap().time_s, 13.0 / 3.0);
    eprintln!(
        "4K64-packet timing window: {:?}, {} decoded frames",
        start.elapsed(),
        window.times.len()
    );
    if let Some(input) = std::env::var_os("VFL_FRAME_STEP_TEST_INPUT") {
        // Full scan is test-only reference evidence. Product stays bounded.
        let input = PathBuf::from(input);
        let probe = probe_video(input.to_string_lossy().into_owned()).unwrap();
        let identity = crate::media_source::SourceIdentity::capture(&input).unwrap();
        let meta = metadata(
            &input,
            probe.video_stream_index,
            Instant::now() + COMMAND_BUDGET,
            &binary,
        )
        .unwrap();
        let mut command = command_no_window(&binary);
        command
            .args([
                "-v",
                "error",
                "-select_streams",
                &probe.video_stream_index.to_string(),
                "-show_frames",
                "-show_entries",
                "frame=pts,best_effort_timestamp",
            ])
            .arg(&input);
        let times = picture_times(
            inspect(command, Instant::now() + COMMAND_BUDGET, &binary)
                .unwrap()
                .frames,
            &meta,
        )
        .unwrap();
        assert!(times.len() > 30);
        let reference = Cached {
            identity: identity.fingerprint().into(),
            stream: probe.video_stream_index,
            meta,
            usable: times.len(),
            times,
            at_start: true,
            at_end: true,
        };
        for index in [0, 20, 21, reference.times.len() - 3] {
            for step in [-10, -1, 1, 10] {
                let mut req = request(&input, probe.duration_s, reference.times[index], step);
                req.video_stream_index = probe.video_stream_index;
                let expected = select(&reference, &req).unwrap();
                let result = step_source_frame(req).unwrap();
                assert_eq!(result.time_s, expected.time_s);
                assert_eq!(result.did_move, expected.did_move);
                assert_eq!(result.source_identity, identity.fingerprint());
            }
        }
        eprintln!(
            "Private source: {} picture timestamps, duration {}, all16 navigation comparisons passed",
            reference.times.len(),
            probe.duration_s
        );
    }
}
