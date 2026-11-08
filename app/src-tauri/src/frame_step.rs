//! Bounded source-PTS navigation. Never infer frame positions from average FPS.
use super::*;

const PACKET_LIMITS: [usize; 3] = [64, 256, 1024];
const FRAME_LIMIT: usize = 2048;
const COMMAND_BUDGET: Duration = Duration::from_secs(5);
static CACHE: OnceLock<Mutex<Option<Cached>>> = OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameStepRequest {
    pub path: String,
    pub source_identity: String,
    pub video_stream_index: u32,
    pub time_s: f64,
    pub step: i32,
    pub min_s: f64,
    pub max_s: f64,
    pub duration_s: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameStepResult {
    pub source_identity: String,
    pub time_s: f64,
    pub frame_end_s: f64,
    pub did_move: bool,
    pub at_limit: bool,
}

#[derive(Clone)]
struct Metadata {
    origin_us: i64,
    numerator: i64,
    denominator: i64,
    first_pts: Option<i64>,
    codec: String,
}
#[derive(Clone)]
struct Cached {
    identity: String,
    stream: u32,
    meta: Metadata,
    times: Vec<f64>,
    usable: usize,
    at_start: bool,
    at_end: bool,
}

#[derive(Deserialize)]
struct Inspection {
    #[serde(default)]
    streams: Vec<Stream>,
    #[serde(default)]
    frames: Vec<Picture>,
    format: Option<Format>,
}
#[derive(Deserialize)]
struct Format {
    start_time: Option<String>,
}
#[derive(Deserialize)]
struct Stream {
    index: Option<u32>,
    codec_type: Option<String>,
    codec_name: Option<String>,
    time_base: Option<String>,
    start_pts: Option<i64>,
    has_b_frames: Option<u32>,
    nb_read_packets: Option<String>,
    #[serde(default)]
    disposition: FFProbeDisposition,
}
#[derive(Deserialize)]
struct Picture {
    pts: Option<i64>,
    best_effort_timestamp: Option<i64>,
}

fn unavailable() -> String {
    "Frame stepping is unavailable: adjacent source frame times could not be verified within the bounded timing window.".into()
}

fn micros(text: &str) -> Result<i64, String> {
    let negative = text.starts_with('-');
    let unsigned = text.trim_start_matches(['-', '+']);
    let (seconds, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    if fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(unavailable());
    }
    let seconds = seconds.parse::<i64>().map_err(|_| unavailable())?;
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<i64>().map_err(|_| unavailable())? * 10i64.pow((6 - fraction.len()) as u32)
    };
    let value = seconds
        .checked_mul(1_000_000)
        .and_then(|n| n.checked_add(fractional))
        .ok_or_else(unavailable)?;
    Ok(if negative { -value } else { value })
}

fn timestamp(pts: i64, meta: &Metadata) -> f64 {
    let numerator = i128::from(pts) * i128::from(meta.numerator) * 1_000_000
        - i128::from(meta.origin_us) * i128::from(meta.denominator);
    numerator as f64 / (meta.denominator as f64 * 1_000_000.0)
}

fn inspect(mut command: Command, deadline: Instant, binary: &str) -> Result<Inspection, String> {
    command.args(["-of", "json"]);
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or_else(unavailable)?;
    // Existing runner caps stdout at 8 MiB and stderr at 64 KiB during reads,
    // kills the process tree on cap/deadline, then waits and drains both pipes.
    let result = run_command_output_with_timeout(
        command,
        "ffprobe",
        "VFL_FFPROBE_PATH",
        binary,
        "Source frame timing",
        remaining,
    )?;
    // ffprobe may report a decode/demux error yet exit successfully. Such a
    // sequence can omit pictures, so its ranks cannot support exact stepping.
    if !result.status.success() || result.stderr.iter().any(|byte| !byte.is_ascii_whitespace()) {
        return Err(
            "Frame stepping is unavailable: the source frame timing could not be read.".into(),
        );
    }
    serde_json::from_slice(&result.stdout).map_err(|_| unavailable())
}

fn metadata(path: &Path, stream: u32, deadline: Instant, binary: &str) -> Result<Metadata, String> {
    let mut command = command_no_window(binary);
    command
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,pipe",
            "-select_streams",
            &stream.to_string(),
            "-show_streams",
            "-show_format",
            "-show_entries",
            "stream=index,codec_type,codec_name,time_base,start_pts:stream_disposition=attached_pic:format=start_time",
        ])
        .arg(path);
    let result = inspect(command, deadline, binary)?;
    let selected = result
        .streams
        .iter()
        .find(|s| {
            s.index == Some(stream)
                && s.codec_type.as_deref() == Some("video")
                && s.disposition.attached_pic == 0
        })
        .ok_or_else(unavailable)?;
    let (numerator, denominator) = selected
        .time_base
        .as_deref()
        .and_then(|s| s.split_once('/'))
        .ok_or_else(unavailable)?;
    let numerator = numerator.parse::<i64>().map_err(|_| unavailable())?;
    let denominator = denominator.parse::<i64>().map_err(|_| unavailable())?;
    if !(1..=i64::from(i32::MAX)).contains(&numerator)
        || !(1..=i64::from(i32::MAX)).contains(&denominator)
    {
        return Err(unavailable());
    }
    let origin_us = micros(
        result
            .format
            .and_then(|f| f.start_time)
            .as_deref()
            .ok_or_else(unavailable)?,
    )?;
    Ok(Metadata {
        origin_us,
        numerator,
        denominator,
        first_pts: selected.start_pts,
        codec: selected.codec_name.clone().unwrap_or_default(),
    })
}

fn window(
    path: &Path,
    request: &FrameStepRequest,
    meta: Metadata,
    deadline: Instant,
    binary: &str,
    packet_limit: usize,
) -> Result<Cached, String> {
    let seek = (request.time_s - 0.5).max(0.0) + meta.origin_us as f64 / 1_000_000.0;
    let mut command = command_no_window(binary);
    command
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file,pipe",
            "-select_streams",
            &request.video_stream_index.to_string(),
            "-read_intervals",
            &format!("{seek:.6}%+#{packet_limit}"),
            "-count_packets",
            "-show_frames",
            "-show_streams",
            "-show_entries",
            "frame=pts,best_effort_timestamp:stream=index,nb_read_packets,has_b_frames,codec_name,time_base",
        ])
        .arg(path);
    let result = inspect(command, deadline, binary)?;
    if result.frames.is_empty() || result.frames.len() > FRAME_LIMIT {
        return Err(unavailable());
    }
    let stream = result
        .streams
        .iter()
        .find(|s| s.index == Some(request.video_stream_index))
        .ok_or_else(unavailable)?;
    if stream.codec_name.as_deref() != Some(meta.codec.as_str())
        || stream
            .time_base
            .as_deref()
            .and_then(|s| s.split_once('/'))
            .and_then(|(n, d)| Some((n.parse::<i64>().ok()?, d.parse::<i64>().ok()?)))
            != Some((meta.numerator, meta.denominator))
    {
        return Err(unavailable());
    }
    let packets = stream
        .nb_read_packets
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
        .ok_or_else(unavailable)?;
    let at_end = packets < packet_limit;
    let first = result.frames[0].pts;
    let times = picture_times(result.frames, &meta)?;
    // has_b_frames is libavcodec's decoder reordering-buffer size. A partial
    // packet scan cannot establish its flushed tail, so withhold that buffer
    // plus a next-picture boundary. Reserve the supported families’ conservative
    // maximum (16), not an earlier or smaller observed buffer that may change
    // after a parameter-set transition. Unknown partial families fail closed.
    let reserve = if at_end {
        0
    } else if matches!(
        meta.codec.as_str(),
        "h264" | "hevc" | "mpeg4" | "mpeg2video" | "mpeg1video" | "vp8" | "vp9" | "av1"
    ) {
        stream
            .has_b_frames
            .filter(|n| *n <= 16)
            .map_or(times.len(), |_| 17)
    } else {
        times.len()
    };
    let usable = times.len().saturating_sub(reserve);
    let at_start = meta.first_pts.is_some() && first == meta.first_pts;
    Ok(Cached {
        identity: request.source_identity.clone(),
        stream: request.video_stream_index,
        meta,
        times,
        usable,
        at_start,
        at_end,
    })
}

fn picture_times(frames: Vec<Picture>, meta: &Metadata) -> Result<Vec<f64>, String> {
    let mut times = Vec::with_capacity(frames.len());
    let mut previous = None;
    for frame in frames {
        let pts = frame.pts.ok_or_else(unavailable)?;
        if frame.best_effort_timestamp.is_some_and(|p| p != pts)
            || previous.is_some_and(|p| pts <= p)
        {
            return Err(unavailable());
        }
        let seconds = timestamp(pts, meta);
        if !seconds.is_finite() || times.last().is_some_and(|p| seconds <= *p) {
            return Err(unavailable());
        }
        times.push(seconds);
        previous = Some(pts);
    }
    Ok(times)
}

fn select(cache: &Cached, request: &FrameStepRequest) -> Option<FrameStepResult> {
    if cache.usable == 0 {
        return None;
    }
    let times = &cache.times[..cache.usable];
    let known_end = cache.at_end || times.last().is_some_and(|t| *t >= request.duration_s);
    if request.time_s < times[0] && !cache.at_start {
        return None;
    }
    if request.time_s > times[times.len() - 1] && !known_end {
        return None;
    }
    let base = if request.time_s >= request.duration_s {
        times.partition_point(|t| *t < request.duration_s) as isize
    } else {
        times.partition_point(|t| *t <= request.time_s) as isize - 1
    };
    let desired = base + request.step as isize;
    let lower = times.partition_point(|t| *t < request.min_s.max(0.0));
    let upper = times.partition_point(|t| *t <= request.max_s && *t < request.duration_s);
    if desired < lower as isize && !cache.at_start && request.min_s < times[0] {
        return None;
    }
    if desired >= upper as isize && !known_end && request.max_s > times[times.len() - 1] {
        return None;
    }
    let unchanged = || FrameStepResult {
        source_identity: request.source_identity.clone(),
        time_s: request.time_s,
        frame_end_s: request.time_s,
        did_move: false,
        at_limit: true,
    };
    if lower >= upper {
        return Some(unchanged());
    }
    let selected = desired.clamp(lower as isize, upper as isize - 1) as usize;
    let time = times[selected];
    if request.step > 0 && time <= request.time_s || request.step < 0 && time >= request.time_s {
        return Some(unchanged());
    }
    let end = cache
        .times
        .get(selected + 1)
        .copied()
        .unwrap_or(request.duration_s)
        .min(request.duration_s);
    if end <= time {
        return None;
    }
    Some(FrameStepResult {
        source_identity: request.source_identity.clone(),
        time_s: time,
        frame_end_s: end,
        did_move: true,
        at_limit: selected as isize != desired,
    })
}

pub fn step_source_frame(request: FrameStepRequest) -> Result<FrameStepResult, String> {
    if !matches!(request.step, -10 | -1 | 1 | 10)
        || ![
            request.time_s,
            request.min_s,
            request.max_s,
            request.duration_s,
        ]
        .iter()
        .all(|n| n.is_finite())
        || request.duration_s <= 0.0
        || request.min_s < 0.0
        || request.max_s > request.duration_s
        || request.min_s > request.max_s
        || request.time_s < request.min_s
        || request.time_s > request.max_s
    {
        return Err("Frame stepping needs a valid source time and trim range.".into());
    }
    let identity = crate::media_source::SourceIdentity::capture(Path::new(&request.path))?;
    identity
        .verify_expected(Some(&request.source_identity))
        .map_err(|_| "The source changed. Reload it before stepping frames.".to_string())?;
    let deadline = Instant::now() + COMMAND_BUDGET;
    let previous = CACHE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| unavailable())?
        .clone()
        .filter(|c| {
            c.identity == request.source_identity && c.stream == request.video_stream_index
        });
    if let Some(result) = previous.as_ref().and_then(|c| select(c, &request)) {
        identity.verify(identity.path())?;
        return Ok(result);
    }
    let binary = default_ffprobe();
    let meta = if let Some(previous) = previous {
        previous.meta
    } else {
        metadata(
            identity.path(),
            request.video_stream_index,
            deadline,
            &binary,
        )?
    };
    // Most steps need only a small neighborhood. Grow at most twice when a
    // keyframe seek or a ten-frame move needs more; all scans share one deadline.
    for packet_limit in PACKET_LIMITS {
        let cached = window(
            identity.path(),
            &request,
            meta.clone(),
            deadline,
            &binary,
            packet_limit,
        )?;
        identity.verify(identity.path())?;
        if let Some(result) = select(&cached, &request) {
            *CACHE
                .get_or_init(|| Mutex::new(None))
                .lock()
                .map_err(|_| unavailable())? = Some(cached);
            return Ok(result);
        }
    }
    Err(unavailable())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta() -> Metadata {
        Metadata {
            origin_us: 0,
            numerator: 1,
            denominator: 30,
            first_pts: Some(0),
            codec: "h264".into(),
        }
    }
    fn cached(times: Vec<f64>) -> Cached {
        Cached {
            identity: "test".into(),
            stream: 0,
            meta: meta(),
            usable: times.len(),
            times,
            at_start: true,
            at_end: true,
        }
    }
    fn request(time_s: f64, step: i32) -> FrameStepRequest {
        FrameStepRequest {
            path: String::new(),
            source_identity: "test".into(),
            video_stream_index: 0,
            time_s,
            step,
            min_s: 0.0,
            max_s: 10.0,
            duration_s: 10.0,
        }
    }

    #[cfg(unix)]
    #[test]
    fn successful_probe_with_decode_error_stderr_is_rejected() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf '{\"streams\":[],\"frames\":[]}'; printf 'decode error' >&2",
        ]);
        assert!(inspect(command, Instant::now() + COMMAND_BUDGET, "sh").is_err());
        let mut command = Command::new("sh");
        command.args(["-c", "printf '{\"streams\":[],\"frames\":[]}'"]);
        assert!(inspect(command, Instant::now() + COMMAND_BUDGET, "sh").is_ok());
    }

    #[test]
    fn integer_ticks_keep_precision_and_common_origin_is_subtracted_once() {
        assert_eq!(timestamp(1, &meta()), 1.0 / 30.0);
        let shifted = Metadata {
            origin_us: 10_500_000,
            denominator: 90_000,
            ..meta()
        };
        // Video begins two seconds after the common format origin.
        assert_eq!(timestamp(1_125_000, &shifted), 2.0);
        assert_eq!(micros("-0.125000").unwrap(), -125_000);
        assert!(micros("0.1234567").is_err());
    }

    #[test]
    fn variable_intervals_off_grid_and_ten_frames_use_source_rank() {
        let cache = cached(
            (0..30)
                .map(|n| {
                    if n < 21 {
                        n as f64 / 30.0
                    } else {
                        (n + 3) as f64 / 30.0
                    }
                })
                .collect(),
        );
        let result = select(&cache, &request(20.0 / 30.0, 1)).unwrap();
        assert_eq!(result.time_s, 0.8);
        assert_eq!(
            select(&cache, &request(0.8, -1)).unwrap().time_s,
            20.0 / 30.0
        );
        assert_eq!(
            select(&cache, &request(0.01, 1)).unwrap().time_s,
            1.0 / 30.0
        );
        assert_eq!(
            select(&cache, &request(1.0 / 30.0, 10)).unwrap().time_s,
            11.0 / 30.0
        );
        assert_eq!(
            select(&cache, &request(11.0 / 30.0, -10)).unwrap().time_s,
            1.0 / 30.0
        );
    }

    #[test]
    fn duration_is_exclusive_and_trim_limits_never_invent_pts_or_move_backwards() {
        let cache = cached(vec![0.0, 0.033, 0.067, 0.1, 0.133]);
        let mut req = request(0.133, -1);
        req.duration_s = 0.133;
        req.max_s = 0.133;
        assert_eq!(select(&cache, &req).unwrap().time_s, 0.1);
        req.time_s = 0.1;
        req.step = 1;
        assert!(!select(&cache, &req).unwrap().did_move);
        req.time_s = 0.06;
        req.max_s = 0.06;
        assert!(!select(&cache, &req).unwrap().did_move);
        req.time_s = 0.01;
        req.step = -1;
        req.min_s = 0.01;
        assert!(!select(&cache, &req).unwrap().did_move);
        req = request(0.0, 10);
        req.max_s = 0.09;
        assert_eq!(select(&cache, &req).unwrap().time_s, 0.067);
    }

    #[test]
    fn incomplete_brackets_and_tail_never_claim_a_neighbor() {
        let mut cache = cached(vec![1.0, 1.033, 1.067, 1.1]);
        cache.at_start = false;
        cache.at_end = false;
        cache.usable = 3;
        assert!(select(&cache, &request(0.99, 1)).is_none());
        assert!(select(&cache, &request(1.033, 10)).is_none());
        assert!(select(&cache, &request(1.1, -1)).is_none());
        assert_eq!(select(&cache, &request(1.0, 1)).unwrap().frame_end_s, 1.067);
    }

    #[test]
    fn missing_tied_nonmonotonic_and_synthesized_timestamps_are_rejected() {
        for values in [
            vec![Some(0), None],
            vec![Some(1), Some(1)],
            vec![Some(2), Some(1)],
        ] {
            assert!(
                picture_times(
                    values
                        .into_iter()
                        .map(|pts| Picture {
                            pts,
                            best_effort_timestamp: pts
                        })
                        .collect(),
                    &meta()
                )
                .is_err()
            );
        }
        assert!(
            picture_times(
                vec![Picture {
                    pts: Some(1),
                    best_effort_timestamp: Some(2)
                }],
                &meta()
            )
            .is_err()
        );
        let close = Metadata {
            denominator: 1_000_000_000,
            ..meta()
        };
        assert!(
            picture_times(
                vec![
                    Picture {
                        pts: Some(1),
                        best_effort_timestamp: None
                    },
                    Picture {
                        pts: Some(2),
                        best_effort_timestamp: None
                    }
                ],
                &close
            )
            .unwrap()[1]
                > 0.0
        );
    }

    #[test]
    fn source_identity_is_checked_even_when_timing_is_cached() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("source.bin");
        fs::write(&path, b"original").unwrap();
        let identity = crate::media_source::SourceIdentity::capture(&path).unwrap();
        let mut req = request(0.0, 1);
        req.path = path.to_string_lossy().into_owned();
        req.source_identity = identity.fingerprint().into();
        let mut entry = cached(vec![0.0, 0.1, 0.2]);
        entry.identity = req.source_identity.clone();
        *CACHE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(entry);
        assert_eq!(
            step_source_frame(FrameStepRequest {
                path: req.path.clone(),
                source_identity: req.source_identity.clone(),
                ..request(0.0, 1)
            })
            .unwrap()
            .time_s,
            0.1
        );
        drop(identity);
        fs::write(path, b"changed bytes").unwrap();
        assert!(
            step_source_frame(req)
                .unwrap_err()
                .contains("source changed")
        );
    }
}

#[cfg(test)]
#[path = "frame_step_media_tests.rs"]
mod media_tests;
