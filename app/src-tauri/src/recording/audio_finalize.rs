//! Version-two float stems and fixed mute history share the native video epoch.
use super::{audio_envelope, audio_types::*, *};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AudioJournal {
    schema: u32,
    format: String,
    sample_rate: u32,
    channels: u8,
    roles: u8,
    initial_mute_mask: u8,
    envelope: String,
    render_file: Option<String>,
    microphone_file: Option<String>,
    suppression: Suppression,
    pub samples: Option<u64>,
}
impl AudioJournal {
    pub fn new(config: &RecordingAudioConfig) -> Option<Self> {
        let roles = config.role_mask();
        (roles != 0).then(|| Self {
            schema: 1,
            format: "f32le".into(),
            sample_rate: 48_000,
            channels: 2,
            roles,
            initial_mute_mask: config.mute_mask(),
            envelope: audio_envelope::FILE_NAME.into(),
            render_file: (roles & 1 != 0).then(|| "render.f32le".into()),
            microphone_file: (roles & 2 != 0).then(|| "microphone.f32le".into()),
            suppression: config
                .microphone
                .as_ref()
                .map_or(Suppression::Off, |m| m.suppression),
            samples: None,
        })
    }
    pub(super) fn owned_files(&self, config: &RecordingAudioConfig) -> Result<Vec<&str>, String> {
        self.validate(config)?;
        let mut files = vec![self.envelope.as_str()];
        files.extend(self.render_file.as_deref());
        files.extend(self.microphone_file.as_deref());
        if self.microphone_file.is_some() && self.suppression == Suppression::Save {
            files.extend(["microphone-processed.f32le", "microphone-processed.partial"]);
        }
        Ok(files)
    }
    pub fn validate(&self, config: &RecordingAudioConfig) -> Result<(), String> {
        let mut expected = Self::new(config).ok_or("Unexpected audio recovery descriptor.")?;
        expected.samples = self.samples;
        if *self != expected || self.samples == Some(0) {
            return Err(
                "The audio recovery descriptor is invalid. Its files have been kept.".into(),
            );
        }
        Ok(())
    }
}
pub(super) struct Plan {
    pub frames: u64,
    fps: u32,
    pub titles: Vec<&'static str>,
    inputs: Vec<PathBuf>,
    pub recovered_prefix: bool,
}
fn complete_frames(path: &Path) -> Result<u64, String> {
    regular_ancestors(path)?;
    let meta = fs::metadata(path).map_err(error)?;
    if !meta.is_file() || meta.len() < 8 {
        return Err(
            "An audio stem contains no complete sample frames. Its files have been kept.".into(),
        );
    }
    Ok(meta.len() / 8)
}
fn video_prefix_samples(duration: f64, rate: Option<f64>, fps: u32) -> Result<u64, String> {
    if ![24, 25, 30, 50, 60].contains(&fps)
        || !duration.is_finite()
        || duration <= 0.
        || !rate.is_some_and(|rate| (rate - f64::from(fps)).abs() < 0.05)
    {
        return Err("The recovery video has no validated CFR clock.".into());
    }
    let frames = (duration * f64::from(fps)).round();
    if frames < 1. || frames > u64::MAX as f64 || (duration - frames / f64::from(fps)).abs() > 0.002
    {
        return Err("The recovery video does not end at a complete CFR frame.".into());
    }
    (frames as u64)
        .checked_mul(48_000 / u64::from(fps))
        .ok_or_else(|| "The recovery video clock overflowed.".into())
}
pub(super) fn prepare(app: &AppHandle, j: &Journal, spool: &Path) -> Result<Option<Plan>, String> {
    let config = j.config.audio_config();
    let Some(descriptor) = &j.audio_capture else {
        if config.enabled() {
            return Err("The audio recovery descriptor is missing.".into());
        }
        return Ok(None);
    };
    descriptor.validate(&config)?;
    // No path from a journal is joined until its exact fixed-owned role is checked.
    regular_ancestors(&j.directory.join(audio_envelope::FILE_NAME))?;
    let history = audio_envelope::read(
        &j.directory,
        &j.session_id,
        descriptor.roles,
        descriptor.initial_mute_mask,
    )?;
    let probe = video::probe_video(spool.to_string_lossy().into_owned())?;
    if !probe.duration_s.is_finite() || probe.duration_s <= 0. {
        return Err("No complete video prefix is available to recover.".into());
    }
    let video_frames = video_prefix_samples(probe.duration_s, probe.frame_rate, j.config.fps)?;
    let mut frames = descriptor.samples.unwrap_or(u64::MAX).min(video_frames);
    let mut inputs = Vec::new();
    let mut titles = vec![MIXED_TITLE];
    for (name, title) in [
        (
            &descriptor.render_file,
            if matches!(config.render, RenderAudio::Application { .. }) {
                APPLICATION_TITLE
            } else {
                SYSTEM_TITLE
            },
        ),
        (&descriptor.microphone_file, MICROPHONE_TITLE),
    ] {
        if let Some(name) = name {
            let path = j.directory.join(name);
            frames = frames.min(complete_frames(&path)?);
            inputs.push(path);
            titles.push(title);
        }
    }
    if frames == 0 {
        return Err("The recording has no common audio/video prefix.".into());
    }
    let recovered_prefix = descriptor.samples.is_none_or(|expected| frames < expected);
    if descriptor.suppression == Suppression::Save {
        let partial = j.directory.join("microphone-processed.partial");
        let processed = j.directory.join("microphone-processed.f32le");
        // Derived data is recomputed from the checked source/envelope on every retry.
        for path in [&partial, &processed] {
            if path.exists() {
                regular_ancestors(path)?;
                fs::remove_file(path).map_err(error)?;
            }
        }
        ensure_local_space(
            &j.directory,
            frames.saturating_mul(8).saturating_add(RESERVE_BYTES),
        )?;
        super::audio_suppression::process_microphone(
            &bundled_media(app, "ffmpeg")?,
            &j.directory.join("microphone.f32le"),
            &partial,
            frames,
            &history,
        )?;
        fs::rename(&partial, &processed).map_err(error)?;
        *inputs.last_mut().ok_or("Missing microphone source.")? = processed;
    }
    Ok(Some(Plan {
        frames,
        fps: j.config.fps,
        titles,
        inputs,
        recovered_prefix,
    }))
}
impl Plan {
    fn video_end(&self) -> f64 {
        ((u128::from(self.frames) * u128::from(self.fps)).div_ceil(48_000)) as f64
            / f64::from(self.fps)
    }
    pub fn command(&self, ffmpeg: &Path, spool: &Path, candidate: &Path) -> Command {
        let mut command = Command::new(ffmpeg);
        command
            .args(["-hide_banner", "-v", "error", "-n", "-i"])
            .arg(spool);
        for input in &self.inputs {
            command
                .args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-i"])
                .arg(input);
        }
        let mut graph = String::new();
        for index in 0..self.inputs.len() {
            graph.push_str(&format!(
                "[{}:a]atrim=end_sample={},apad=whole_len={},asplit=2[s{}][m{}];",
                index + 1,
                self.frames,
                self.frames,
                index,
                index
            ));
        }
        for index in 0..self.inputs.len() {
            graph.push_str(&format!("[m{index}]"));
        }
        graph.push_str(&format!("amix=inputs={}:duration=longest:normalize=0,aeval=exprs='clip(val(0),-1,1)|clip(val(1),-1,1)',atrim=end_sample={}[mixed]",self.inputs.len(),self.frames));
        command.args([
            "-filter_complex",
            &graph,
            "-map",
            "0:v:0",
            "-map",
            "[mixed]",
        ]);
        for index in 0..self.inputs.len() {
            command.args(["-map", &format!("[s{index}]")]);
        }
        command.args([
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-map_metadata",
            "-1",
        ]);
        for (index, title) in self.titles.iter().enumerate() {
            command
                .arg(format!("-metadata:s:a:{index}"))
                .arg(format!("title={title}"))
                .arg(format!("-metadata:s:a:{index}"))
                .arg(format!("handler_name={title}"))
                .arg(format!("-disposition:a:{index}"))
                .arg(if index == 0 { "default" } else { "0" });
        }
        command
            .args([
                "-t",
                &format!("{:.12}", self.video_end()),
                "-movflags",
                "+faststart",
            ])
            .arg(candidate);
        command
    }
    pub fn budget(&self, video_bytes: u64) -> (u64, Duration) {
        let seconds = self.frames.div_ceil(48_000);
        (
            video_bytes
                .saturating_add(seconds.saturating_mul(self.titles.len() as u64 * 40_000))
                .saturating_add(RESERVE_BYTES),
            Duration::from_secs(60 + video_bytes / 1_000_000 + seconds / 2),
        )
    }
}
/// Raw stream timestamps are checked independently from the editor's selected projection.
pub(super) fn validate_tracks(
    app: &AppHandle,
    candidate: &Path,
    plan: &Plan,
) -> Result<(), String> {
    let mut command = Command::new(bundled_media(app, "ffprobe")?);
    command
        .args(["-v", "error", "-show_streams", "-of", "json"])
        .arg(candidate);
    let mut child = process::ManagedChild::spawn(command).map_err(error)?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("Missing audio validation output.")?;
    let stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .by_ref()
            .take(PROTOCOL_LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(error)?;
        if bytes.len() > PROTOCOL_LIMIT as usize {
            return Err("Audio validation output exceeded its bound.".into());
        }
        Ok::<_, String>(bytes)
    });
    let drain = std::thread::spawn(move || {
        if let Some(mut stderr) = stderr {
            let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        }
    });
    let status = child.wait_timeout(Duration::from_secs(30)).map_err(error)?;
    if status.is_none() {
        let _ = child.control.kill();
    }
    drop(child);
    let bytes = reader
        .join()
        .map_err(|_| "Audio validation worker failed.")??;
    let _ = drain.join();
    if !status.is_some_and(|status| status.success()) {
        return Err("Cannot validate the recorded audio tracks.".into());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(error)?;
    validate_inventory(&value, plan)
}
fn validate_inventory(value: &Value, plan: &Plan) -> Result<(), String> {
    let streams = value["streams"]
        .as_array()
        .ok_or("Missing recorded streams.")?;
    let audio: Vec<_> = streams
        .iter()
        .filter(|s| s["codec_type"] == "audio")
        .collect();
    if streams.len() != plan.titles.len() + 1 || audio.len() != plan.titles.len() {
        return Err("The recorded audio track inventory differs from its selected sources.".into());
    }
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .ok_or("Missing recorded video.")?;
    let video_start = video["start_time"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("Missing recorded video start time.")?;
    let video_duration = video["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .ok_or("Missing recorded video duration.")?;
    if video["index"] != 0
        || video["codec_name"] != "h264"
        || !video_start.is_finite()
        || !video_duration.is_finite()
        || video_start.abs() > 0.002
        || (video_start + video_duration - plan.video_end()).abs() > 0.002
    {
        return Err("The recovered video does not end at its common audio/video CFR boundary. Recovery files have been kept.".into());
    }
    let expected = plan.frames as f64 / 48_000.;
    let mut first_duration = None;
    for (index, (stream, title)) in audio.iter().zip(&plan.titles).enumerate() {
        let duration = stream["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("Missing recorded audio duration.")?;
        let start = stream["start_time"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or("Missing recorded audio start time.")?;
        if stream["index"].as_u64() != Some(index as u64 + 1)
            || stream["codec_name"] != "aac"
            || stream["sample_rate"] != "48000"
            || stream["channels"] != 2
            || stream["tags"]["handler_name"] != *title
            || stream["disposition"]["default"] != u64::from(index == 0)
            || !duration.is_finite()
            || !start.is_finite()
            || start.abs() > 0.002
            || (duration - expected).abs() > 0.002
            || first_duration
                .is_some_and(|first: f64| (duration - first).abs() > 1. / 48_000. + 0.000002)
        {
            return Err("The recorded audio timing, role, or format did not pass validation. Recovery files have been kept.".into());
        }
        first_duration = Some(duration);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_binds_roles_format_and_fixed_owned_names() {
        let config = RecordingAudioConfig {
            render: RenderAudio::System {
                device_id: "default".into(),
            },
            ..Default::default()
        };
        let mut j = AudioJournal::new(&config).unwrap();
        assert!(j.validate(&config).is_ok());
        j.render_file = Some("../wrong".into());
        assert!(j.validate(&config).is_err());
        assert!(
            serde_json::to_vec(&AudioJournal::new(&config))
                .unwrap()
                .len()
                < 1024
        );
    }
    #[test]
    fn mux_preserves_roles_and_never_uses_a_lookahead_limiter() {
        let plan = Plan {
            frames: 480137,
            fps: 30,
            titles: vec![MIXED_TITLE, APPLICATION_TITLE, MICROPHONE_TITLE],
            inputs: vec!["render".into(), "mic".into()],
            recovered_prefix: false,
        };
        let command = plan.command(
            Path::new("ffmpeg"),
            Path::new("capture"),
            Path::new("candidate"),
        );
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy()).collect();
        assert!(args.iter().any(|a| a.contains("amix=inputs=2")
            && a.contains("normalize=0")
            && a.contains("clip(val(0)")));
        assert!(args.iter().any(|a| a == "handler_name=Application audio"));
        assert!(!args.iter().any(|a| a.contains("alimiter")));
    }
    #[test]
    fn candidate_requires_all_literal_roles_one_default_and_common_timing() {
        let plan = Plan {
            frames: 480137,
            fps: 30,
            titles: vec![MIXED_TITLE, APPLICATION_TITLE, MICROPHONE_TITLE],
            inputs: vec![],
            recovered_prefix: false,
        };
        let mut streams = vec![
            json!({"index":0,"codec_type":"video","codec_name":"h264","start_time":"0.000000","duration":format!("{:.9}",plan.video_end())}),
        ];
        for (index, title) in plan.titles.iter().enumerate() {
            streams.push(json!({"index":index+1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,"duration":format!("{:.9}",plan.frames as f64/48000.),"start_time":"0.000000","tags":{"handler_name":title},"disposition":{"default":u8::from(index==0)}}));
        }
        let good = json!({"streams":streams});
        assert!(validate_inventory(&good, &plan).is_ok());
        for mutation in 0..4 {
            let mut bad = good.clone();
            match mutation {
                0 => bad["streams"][3]["disposition"]["default"] = json!(1),
                1 => bad["streams"][2]["tags"]["handler_name"] = json!("System audio"),
                2 => bad["streams"][1]["duration"] = json!("10.1"),
                _ => bad["streams"][3]["start_time"] = json!("0.025"),
            };
            assert!(validate_inventory(&bad, &plan).is_err());
        }
    }
    #[test]
    fn unequal_recovery_prefix_bounds_copied_video_to_covering_cfr_frame() {
        let plan = Plan {
            frames: 7 * 48000 + 137,
            fps: 30,
            titles: vec![MIXED_TITLE, MICROPHONE_TITLE],
            inputs: vec!["mic".into()],
            recovered_prefix: true,
        };
        assert!((plan.video_end() - 7.033333333333).abs() < 0.000001);
        let command = plan.command(
            Path::new("ffmpeg"),
            Path::new("ten-second-video"),
            Path::new("candidate"),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-t" && pair[1] == "7.033333333333")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-c:v" && pair[1] == "copy")
        );
        assert!(plan.video_end() - plan.frames as f64 / 48000. < 1. / 30.);
    }
    #[test]
    fn rounded_probe_duration_does_not_discard_a_sample_or_a_whole_cfr_frame() {
        assert_eq!(
            video_prefix_samples(12.033333, Some(30.), 30).unwrap(),
            361 * 1600
        );
        assert_eq!(
            video_prefix_samples(12.033334, Some(30.), 30).unwrap(),
            361 * 1600
        );
        assert!(video_prefix_samples(12.02, Some(30.), 30).is_err());
    }
}
