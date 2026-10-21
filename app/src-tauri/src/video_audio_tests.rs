use super::*;
use serde_json::{Value, json};
fn fixture() -> Value {
    json!({"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2","start_time":"0","duration":"12"},"streams":[
        {"index":0,"codec_type":"video","codec_name":"h264","width":160,"height":90,"pix_fmt":"yuv420p","avg_frame_rate":"30/1","start_time":"0","duration":"5"},
        {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"sample_fmt":"fltp","start_time":"0","duration":"5","tags":{"handler_name":"Mixed"},"disposition":{"default":1}},
        {"index":2,"codec_type":"audio","codec_name":"pcm_f64le","sample_rate":"96000","channels":1,"sample_fmt":"dbl","start_time":"1","duration":"11","tags":{"title":"Microphone"},"disposition":{"default":0}}
    ]})
}
fn selected(fixture: &Value, index: Option<u32>) -> Result<VideoProbe, String> {
    parse_probe_output_selected(&fixture.to_string(), None, index)
}
#[test]
fn selected_projection_precedes_duration_and_depth_not_only_the_map() {
    let input = fixture();
    let default = selected(&input, None).unwrap();
    let mic = selected(&input, Some(2)).unwrap();
    assert_eq!(default.duration_s, 5.0);
    assert_eq!(mic.duration_s, 12.0);
    assert_eq!(mic.audio_stream_index, Some(2));
    assert_eq!(mic.audio_codec.as_deref(), Some("pcm_f64le"));
    assert_eq!(mic.audio_sample_rate, Some(96000));
    assert_eq!(mic.audio_channels, Some(1));
    assert_eq!(mic.decoded_audio_bytes_per_sample, Some(8));
    assert_eq!(mic.audio_start_s, Some(1.0));
    assert_eq!(mic.audio_end_s, Some(12.0));
    assert!(!mic.audio_is_default);
    let mut request = super::tests::base_request();
    request.size_limit_mb = 0.0;
    let encoders = HashSet::from([
        "libx264".to_owned(),
        "aac".to_owned(),
        "libmp3lame".to_owned(),
    ]);
    let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
    let plan = build_encode_command_plan(&request, &mic, codecs, None).unwrap();
    assert_eq!(plan.audio_stream_index, Some(2));
    assert_eq!(plan.audio_action, StreamAction::Encode);
    let args = build_single_pass_args("input.mp4", &request, &plan).unwrap();
    assert!(args.windows(2).any(|p| p == ["-map", "0:2"]));
    assert!(!args.windows(2).any(|p| p == ["-map", "0:1"]));
    request.format = OutputFormat::Mp3;
    let codecs = select_codec_plan(request.format, &encoders, &request.advanced).unwrap();
    let plan = build_encode_command_plan(&request, &mic, codecs, None).unwrap();
    assert_eq!(plan.audio_stream_index, Some(2));
    request.reverse = true;
    request.format = OutputFormat::Mp4;
    let policy = resolve_media_policy(&request, &mic, Some(VideoCodec::LibX264)).unwrap();
    let estimate = reverse_buffer_estimate(&request, &mic, policy, true)
        .unwrap()
        .unwrap();
    let default_estimate = reverse_buffer_estimate(&request, &default, policy, true)
        .unwrap()
        .unwrap();
    assert!(estimate.bytes > default_estimate.bytes);
}
#[test]
fn selection_never_falls_back_for_missing_non_audio_or_duplicate_indices() {
    let mut input = fixture();
    assert!(selected(&input, Some(0)).is_err());
    assert!(selected(&input, Some(99)).is_err());
    input["streams"][2]["index"] = json!(1);
    assert!(selected(&input, None).unwrap_err().contains("duplicate"));
    assert!(
        probe_video_selected("not-read.mp4".into(), Some(1), None)
            .unwrap_err()
            .contains("identity")
    );
}
#[test]
fn catalog_is_bounded_but_retains_explicit_and_default_selection() {
    let mut input = fixture();
    let template = input["streams"][2].clone();
    let streams = input["streams"].as_array_mut().unwrap();
    for index in 3..=100 {
        let mut s = template.clone();
        s["index"] = json!(index);
        s["tags"]["title"] = json!("🎤".repeat(100));
        streams.push(s);
    }
    let probe = selected(&input, Some(100)).unwrap();
    assert!(probe.audio_tracks_truncated);
    assert_eq!(probe.audio_tracks.len(), 64);
    assert!(probe.audio_tracks.iter().any(|a| a.stream_index == 1));
    assert!(probe.audio_tracks.iter().any(|a| a.stream_index == 100));
    assert!(
        probe
            .audio_tracks
            .iter()
            .all(|a| a.title.as_ref().is_none_or(|t| t.len() <= 128))
    );
}
#[test]
fn original_mixed_requires_the_entire_recorder_layout_and_selected_track() {
    let mut input = fixture();
    input["streams"][2]["codec_name"] = json!("aac");
    input["streams"][2]["sample_rate"] = json!("48000");
    input["streams"][2]["channels"] = json!(2);
    assert!(selected(&input, None).unwrap().preview_original_safe);
    assert!(!selected(&input, Some(2)).unwrap().preview_original_safe);
    input["streams"][2]["disposition"]["default"] = json!(1);
    assert!(!selected(&input, Some(1)).unwrap().preview_original_safe);
    input["streams"][2]["disposition"]["default"] = json!(0);
    input["streams"][2]["channels"] = json!(1);
    assert!(!selected(&input, Some(1)).unwrap().preview_original_safe);
    input["streams"][2]["channels"] = json!(2);
    input["streams"][2]["tags"]["title"] = json!("Foreign title");
    assert!(!selected(&input, Some(1)).unwrap().preview_original_safe);
}

#[cfg(unix)]
#[test]
fn bounded_inspection_rejects_oversized_child_output_and_reaps() {
    let directory = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new("/usr/bin/python3");
    command
        .env_clear()
        .env("HOME", directory.path())
        .env("TMPDIR", directory.path())
        .args([
            "-I",
            "-c",
            "import sys;sys.stdout.buffer.write(b'x'*(9*1024*1024));sys.stdout.flush()",
        ]);
    let result = run_command_output_with_timeout(
        command,
        "fixture",
        "UNUSED",
        "/usr/bin/python3",
        "bounded inspection fixture",
        Duration::from_secs(5),
    );
    assert!(result.unwrap_err().contains("byte limit"));
}

#[test]
fn sole_audio_keeps_legacy_browser_decode_path_without_claiming_codec_support() {
    let mut input = fixture();
    input["streams"].as_array_mut().unwrap().remove(1);
    input["streams"][0]["codec_name"] = json!("hevc");
    assert!(selected(&input, Some(2)).unwrap().preview_original_safe);
    assert!(selected(&input, Some(1)).is_err());
}
