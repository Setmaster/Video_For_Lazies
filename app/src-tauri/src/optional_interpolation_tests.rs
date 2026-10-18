//! Explicit native qualification using a supplied, verified optional component.
//! These tests never fetch artifacts or use a system Python environment.
use crate::interpolation_components::{ModelExecution, ResolvedModel};
use crate::interpolation_media::prepare;
use crate::process::{self, JobContext, JobScope};
use crate::video::EncodeRequest;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn model(cache: PathBuf) -> ResolvedModel {
    let id = std::env::var("VFL_TEST_OPTIONAL_MODEL").expect("Provide the qualified model ID");
    let root = PathBuf::from(
        std::env::var_os("VFL_TEST_OPTIONAL_COMPONENT")
            .expect("Provide the verified component root"),
    );
    let python_path = PathBuf::from(
        std::env::var_os("VFL_TEST_OPTIONAL_PYTHON")
            .expect("Provide the pinned private interpreter"),
    );
    let script_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../optional-interpolation/worker.py");
    let source_dir = root.join("sources");
    let execution = if id == "ldf-vfi" {
        ModelExecution::TorchSequence {
            python_path,
            script_path,
            source_dir,
            cache_dir: cache,
        }
    } else {
        assert!(matches!(
            id.as_str(),
            "gmfss-fortuna-union" | "gimm-vfi-r-p"
        ));
        ModelExecution::TorchPair {
            python_path,
            script_path,
            source_dir,
            cache_dir: cache,
        }
    };
    let worker_path = PathBuf::from(
        std::env::var_os("VFL_TEST_RIFE_WORKER").expect("Provide the built storage helper"),
    );
    ResolvedModel {
        execution,
        worker_path,
        model_dir: root.join("models"),
        model_id: id,
        model_digest: "explicit-qualification".into(),
        worker_digest: "explicit-qualification".into(),
        _lease: None,
    }
}

#[test]
#[ignore = "requires explicit hash-qualified model/interpreter paths, a CUDA GPU and FFmpeg"]
fn optional_native_media_exports_exact_cadence_audio_and_cleans_cancelled_processes() {
    let cache = tempfile::tempdir().unwrap();
    let model = model(cache.path().join("cache"));
    exercise_native_media(&model);
}

pub(crate) fn exercise_native_media(model: &ResolvedModel) {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.mp4");
    let sequence = model.model_id == "ldf-vfi";
    let source_graph = if sequence {
        "testsrc2=size=128x128:rate=24"
    } else {
        "testsrc2=size=128x128:rate=30"
    };
    let duration = if sequence { "0.25" } else { "0.1" };
    let (input_count, output_count) = if sequence { (6, 15) } else { (3, 6) };
    let ffmpeg = std::env::var("VFL_INTERPOLATION_TEST_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    assert!(
        Command::new(&ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                source_graph,
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                duration,
                "-c:v",
                "libx264",
                "-video_track_timescale",
                "30000",
                "-c:a",
                "aac"
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let output = directory.path().join("output.mp4");
    let request: EncodeRequest = serde_json::from_value(serde_json::json!({
        "inputPath":source,"outputPath":output,"format":"mp4","sizeLimitMb":0,"audioEnabled":true,
        "reverse":false,"speed":1,"rotateDeg":0,"interpolation":{"targetFps":60,"modelId":model.model_id}
    })).unwrap();
    let probe = crate::video::probe_video(source.to_string_lossy().into_owned()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let group = Arc::new(Mutex::new(Vec::new()));
    let _scope = JobScope::enter(JobContext {
        cancel: cancel.clone(),
        child: group.clone(),
    });
    let prepared = prepare(
        &ffmpeg,
        &source,
        &probe,
        &request,
        model,
        &cancel,
        &group,
        |_| {},
    )
    .unwrap();
    assert_eq!(prepared.diagnostics.input_frames, input_count);
    assert_eq!(prepared.diagnostics.output_frames, output_count);
    assert!(prepared.diagnostics.inferred_frames > 0);
    assert!(group.lock().unwrap().is_empty());
    let args = ["-y", "-v", "error", "-i"]
        .into_iter()
        .map(str::to_string)
        .chain([
            source.to_string_lossy().into_owned(),
            "-map".into(),
            format!("0:{}", probe.video_stream_index),
            "-map".into(),
            format!("0:{}", probe.audio_stream_index.unwrap()),
            "-c:v".into(),
            "libx264".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-c:a".into(),
            "aac".into(),
            output.to_string_lossy().into_owned(),
        ])
        .collect::<Vec<_>>();
    assert!(
        Command::new(&ffmpeg)
            .args(prepared.rewrite_args(&args, probe.video_stream_index, true))
            .status()
            .unwrap()
            .success()
    );
    let inspected = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_frames",
            "-show_entries",
            "stream=codec_type,nb_read_frames,r_frame_rate,duration",
            "-of",
            "json",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let data: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let streams = data["streams"].as_array().unwrap();
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .unwrap();
    assert_eq!(video["nb_read_frames"], output_count.to_string());
    assert_eq!(video["r_frame_rate"], "60/1");
    assert!(streams.iter().any(|stream| stream["codec_type"] == "audio"));
    assert!(
        (video["duration"].as_str().unwrap().parse::<f64>().unwrap()
            - duration.parse::<f64>().unwrap())
        .abs()
            < 0.001
    );
    prepared.identity.verify(&source).unwrap();
    drop(prepared);

    // A black/white/black source must be held across hard cuts. Any blended or
    // synthesized cut picture would violate the product's scene-boundary rule.
    let cut_raw = directory.path().join("cuts.rgb");
    let picture_bytes = 128 * 128 * 3;
    let mut cut_pixels = vec![0; picture_bytes * 3];
    cut_pixels[picture_bytes..picture_bytes * 2].fill(255);
    std::fs::write(&cut_raw, cut_pixels).unwrap();
    let cut_source = directory.path().join("cuts.mp4");
    assert!(
        Command::new(&ffmpeg)
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "rawvideo",
                "-pixel_format",
                "rgb24",
                "-video_size",
                "128x128",
                "-framerate",
                "30",
                "-i"
            ])
            .arg(&cut_raw)
            .args([
                "-c:v",
                "libx264",
                "-crf",
                "0",
                "-pix_fmt",
                "yuv420p",
                "-video_track_timescale",
                "30000"
            ])
            .arg(&cut_source)
            .status()
            .unwrap()
            .success()
    );
    let cut_probe = crate::video::probe_video(cut_source.to_string_lossy().into_owned()).unwrap();
    let mut cut_request = request.clone();
    cut_request.input_path = cut_source.to_string_lossy().into_owned();
    cut_request.audio_enabled = false;
    let cut_prepared = prepare(
        &ffmpeg,
        &cut_source,
        &cut_probe,
        &cut_request,
        model,
        &cancel,
        &group,
        |_| {},
    )
    .unwrap();
    assert_eq!(cut_prepared.diagnostics.cut_pairs, 2);
    assert_eq!(cut_prepared.diagnostics.inferred_frames, 0);
    assert_eq!(cut_prepared.diagnostics.output_frames, 6);
    let cut_output = Command::new(&ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(&cut_prepared.path)
        .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
        .output()
        .unwrap();
    assert!(cut_output.status.success());
    assert_eq!(cut_output.stdout.len(), picture_bytes * 6);
    for (index, frame) in cut_output.stdout.chunks_exact(picture_bytes).enumerate() {
        assert!(
            if (2..4).contains(&index) {
                frame.iter().all(|v| *v >= 254)
            } else {
                frame.iter().all(|v| *v <= 1)
            },
            "Hard cut frame {index} was altered"
        );
    }
    drop(cut_prepared);
    if sequence {
        let mut synthesized = false;
        let interrupted = prepare(
            &ffmpeg,
            &source,
            &probe,
            &request,
            model,
            &cancel,
            &group,
            |fraction| {
                // Decode occupies the first quarter. At this point several exact
                // output samples, including actual predictions, have arrived.
                if fraction >= 0.4 && !synthesized {
                    synthesized = true;
                    cancel.store(true, Ordering::Relaxed);
                    process::kill_process_group(&group);
                }
            },
        );
        assert!(
            synthesized,
            "LDF never reached generated output before cancellation"
        );
        assert!(interrupted.is_err());
        assert!(group.lock().unwrap().is_empty());
        cancel.store(false, Ordering::Relaxed);
    }
    let timer_cancel = cancel.clone();
    let timer_group = group.clone();
    let timer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        timer_cancel.store(true, Ordering::Relaxed);
        process::kill_process_group(&timer_group);
    });
    assert!(
        prepare(
            &ffmpeg,
            &source,
            &probe,
            &request,
            model,
            &cancel,
            &group,
            |_| {}
        )
        .is_err()
    );
    timer.join().unwrap();
    assert!(group.lock().unwrap().is_empty());
    eprintln!(
        "{} actual prepare/export: {input_count} source frames, {output_count} frames at 60 fps, audio retained; canceled child group empty.",
        model.model_id
    );
}
