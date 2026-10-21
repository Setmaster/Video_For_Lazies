//! Fixed, file-only microphone denoising. Original stems remain recovery input.
//! Compensation and the durable mute envelope are applied after the filter.
use super::audio_envelope::History;
use crate::process::{ManagedChild, ProcessControl};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const SAMPLE_RATE: u64 = 48_000;
const FRAME_BYTES: u64 = 8;
const DELAY_FRAMES: u64 = 1_200;
const PAD_FRAMES: u64 = 2_400;
const BUFFER_BYTES: usize = 32 * 1024;
const STDERR_BYTES: usize = 16 * 1024;
const STALL_TIMEOUT: Duration = Duration::from_secs(60);

fn budget(frames: u64) -> Result<Duration, String> {
    frames
        .div_ceil(SAMPLE_RATE)
        .checked_mul(4)
        .and_then(|seconds| seconds.checked_add(120))
        .map(Duration::from_secs)
        .ok_or_else(|| "Microphone processing duration exceeds its safe range.".into())
}

fn read_frame(reader: &mut impl Read) -> Result<[f32; 2], String> {
    let mut bytes = [0; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("Read complete microphone sample: {error}"))?;
    let frame = [
        f32::from_le_bytes(bytes[..4].try_into().unwrap()),
        f32::from_le_bytes(bytes[4..].try_into().unwrap()),
    ];
    if frame.iter().any(|sample| !sample.is_finite()) {
        return Err(
            "Microphone processing encountered a nonfinite sample; recovery files were kept."
                .into(),
        );
    }
    Ok(frame)
}

fn write_frame(writer: &mut impl Write, frame: [f32; 2]) -> Result<(), String> {
    let mut bytes = [0; 8];
    bytes[..4].copy_from_slice(&frame[0].to_le_bytes());
    bytes[4..].copy_from_slice(&frame[1].to_le_bytes());
    writer
        .write_all(&bytes)
        .map_err(|error| format!("Write microphone sample: {error}"))
}

/// Feed exactly the authoritative complete prefix, ignoring any crash-time
/// terminal partial frame. FFmpeg never opens the original file or its tail.
fn feed_prefix(reader: impl Read, writer: impl Write, frames: u64) -> Result<(), String> {
    let mut reader = BufReader::with_capacity(BUFFER_BYTES, reader);
    let mut writer = BufWriter::with_capacity(BUFFER_BYTES, writer);
    for _ in 0..frames {
        write_frame(&mut writer, read_frame(&mut reader)?)?;
    }
    writer
        .flush()
        .map_err(|error| format!("Flush microphone filter input: {error}"))
}

fn copy_filtered(
    reader: impl Read,
    writer: &mut impl Write,
    frames: u64,
    history: &History,
    mut progress: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if history.roles & 2 == 0 {
        return Err("The microphone mute history has no microphone role.".into());
    }
    let total = frames
        .checked_add(PAD_FRAMES)
        .ok_or("Microphone filter length overflow.")?;
    let end = frames
        .checked_add(DELAY_FRAMES)
        .ok_or("Microphone compensation length overflow.")?;
    let mut reader = BufReader::with_capacity(BUFFER_BYTES, reader);
    let mut writer = BufWriter::with_capacity(BUFFER_BYTES, writer);
    for position in 0..total {
        if position % 4096 == 0 {
            progress()?;
        }
        let mut frame = read_frame(&mut reader)?;
        if (DELAY_FRAMES..end).contains(&position) {
            // History's API takes a role index: microphone index 1 is mask bit 2.
            if history.muted(1, position - DELAY_FRAMES) {
                frame = [0.; 2];
            }
            write_frame(&mut writer, frame)?;
        }
    }
    let mut extra = [0];
    match reader.read(&mut extra) {
        Ok(0) => (),
        Ok(_) => {
            return Err("Microphone filter produced an unexpected extra sample or byte.".into());
        }
        Err(error) => return Err(format!("Drain microphone filter: {error}")),
    }
    writer
        .flush()
        .map_err(|error| format!("Flush compensated microphone samples: {error}"))?;
    progress()
}

struct Diagnostics {
    bytes: Vec<u8>,
    total: u64,
}
fn drain_diagnostics(mut reader: impl Read) -> io::Result<Diagnostics> {
    let mut bytes = Vec::with_capacity(STDERR_BYTES);
    let mut buffer = [0; 4096];
    let mut total = 0_u64;
    loop {
        let size = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => size,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        total = total.saturating_add(size as u64);
        let keep = size.min(STDERR_BYTES - bytes.len());
        bytes.extend_from_slice(&buffer[..keep]);
    }
    Ok(Diagnostics { bytes, total })
}

struct KillOnExit(ProcessControl);
impl Drop for KillOnExit {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Caller supplies a fresh owned `.partial` path and renames only after success.
/// Failure preserves the original stem and the incomplete derived output.
pub(crate) fn process_microphone(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    frames: u64,
    history: &History,
) -> Result<(), String> {
    crate::process::check_cancelled().map_err(|error| error.to_string())?;
    if frames == 0 || history.roles & 2 == 0 || output.extension() != Some(OsStr::new("partial")) {
        return Err("Microphone suppression requires samples, a microphone history and a fresh partial output.".into());
    }
    let expected = frames
        .checked_mul(FRAME_BYTES)
        .ok_or("Microphone input length overflow.")?;
    frames
        .checked_add(PAD_FRAMES)
        .and_then(|value| value.checked_mul(FRAME_BYTES))
        .ok_or("Microphone filter length overflow.")?;
    let source_metadata = std::fs::symlink_metadata(input)
        .map_err(|error| format!("Inspect microphone stem: {error}"))?;
    if !source_metadata.is_file()
        || source_metadata.file_type().is_symlink()
        || source_metadata.len() < expected
    {
        return Err("The complete authoritative microphone prefix is unavailable; recovery files were kept.".into());
    }
    let source = File::open(input).map_err(|error| format!("Open microphone stem: {error}"))?;
    if source.metadata().map_err(|error| error.to_string())?.len() < expected {
        return Err("The microphone stem changed before processing.".into());
    }
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("Create owned microphone partial output: {error}"))?;
    let limit = budget(frames)?;
    let started = Instant::now();
    started
        .checked_add(limit)
        .ok_or("Microphone processing deadline exceeds the supported clock range.")?;
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "f32le",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
            "pipe:0",
            "-af",
        ])
        .arg(format!(
            "atrim=end_sample={frames},apad=pad_len={PAD_FRAMES},afftdn=nr=12:nf=-50:tn=0"
        ))
        .args([
            "-ar",
            "48000",
            "-ac",
            "2",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
            "pipe:1",
        ]);
    let mut child = ManagedChild::spawn_with_stdin(command)
        .map_err(|error| format!("Start microphone suppression: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or("Microphone filter input pipe is unavailable.")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("Microphone filter output pipe is unavailable.")?;
    let stderr = child
        .stderr
        .take()
        .ok_or("Microphone filter diagnostics pipe is unavailable.")?;
    let progress = AtomicU64::new(0);
    let timed_out = AtomicU8::new(0);
    let result = thread::scope(|scope| -> Result<(), String> {
        // Runs before implicit scoped joins on every early-return/panic path.
        let _kill = KillOnExit(child.control.clone());
        let errors_control = child.control.clone();
        let errors = thread::Builder::new()
            .name("mic-filter-stderr".into())
            .spawn_scoped(scope, move || {
                let result = drain_diagnostics(stderr);
                if result.is_err() {
                    let _ = errors_control.kill();
                }
                result
            })
            .map_err(|error| format!("Own microphone diagnostics reader: {error}"))?;
        let feed_control = child.control.clone();
        let feeder = thread::Builder::new()
            .name("mic-filter-input".into())
            .spawn_scoped(scope, move || {
                let result = feed_prefix(source, stdin, frames);
                if result.is_err() {
                    let _ = feed_control.kill();
                }
                result
            })
            .map_err(|error| format!("Own microphone input feeder: {error}"))?;
        let (done, complete) = mpsc::channel();
        let watch_control = child.control.clone();
        let progress_ref = &progress;
        let timeout_ref = &timed_out;
        let watcher = thread::Builder::new()
            .name("mic-filter-watchdog".into())
            .spawn_scoped(scope, move || {
                loop {
                    match complete.recv_timeout(Duration::from_millis(250)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                    }
                    let elapsed = started.elapsed();
                    let last = Duration::from_millis(progress_ref.load(Ordering::Relaxed));
                    let reason = if elapsed >= limit {
                        1
                    } else if elapsed.saturating_sub(last) >= STALL_TIMEOUT {
                        2
                    } else {
                        0
                    };
                    if reason != 0 {
                        timeout_ref.store(reason, Ordering::Release);
                        return watch_control.kill();
                    }
                }
            })
            .map_err(|error| format!("Own microphone filter watchdog: {error}"))?;
        let copied = copy_filtered(stdout, &mut destination, frames, history, || {
            crate::process::check_cancelled().map_err(|error| error.to_string())?;
            progress.store(
                started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
            Ok(())
        });
        if copied.is_err() {
            let _ = child.control.kill();
        }
        let status = child
            .wait()
            .map_err(|error| format!("Reap microphone filter: {error}"));
        let fed = feeder
            .join()
            .map_err(|_| "Microphone input feeder stopped unexpectedly.".to_string())?;
        let diagnostics = errors
            .join()
            .map_err(|_| "Microphone diagnostics reader stopped unexpectedly.".to_string())?
            .map_err(|error| format!("Drain microphone diagnostics: {error}"))?;
        let _ = done.send(());
        watcher
            .join()
            .map_err(|_| "Microphone watchdog stopped unexpectedly.".to_string())?
            .map_err(|error| format!("Stop microphone filter process tree: {error}"))?;
        match timed_out.load(Ordering::Acquire) {
            1 => return Err("Microphone suppression exceeded its duration-scaled processing limit; recovery files were kept.".into()),
            2 => return Err("Microphone suppression made no output progress for 60 seconds; recovery files were kept.".into()),
            _ => (),
        }
        fed?;
        copied?;
        let status = status?;
        if !status.success() || diagnostics.total != 0 {
            let detail: String = String::from_utf8_lossy(&diagnostics.bytes)
                .chars()
                .take(2000)
                .collect();
            return Err(format!(
                "Microphone suppression failed ({status}): {detail}"
            ));
        }
        Ok(())
    });
    result?;
    if destination
        .metadata()
        .map_err(|error| error.to_string())?
        .len()
        != expected
    {
        return Err(
            "Compensated microphone output has the wrong length; recovery files were kept.".into(),
        );
    }
    destination
        .sync_all()
        .map_err(|error| format!("Commit compensated microphone samples: {error}"))
}

#[cfg(test)]
mod tests {
    use super::super::audio_envelope::Edge;
    use super::*;
    use std::io::Cursor;

    fn history() -> History {
        History {
            initial_mask: 0,
            roles: 3,
            edges: Vec::new(),
        }
    }
    fn encode(frames: &[[f32; 2]]) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| frame.iter().flat_map(|sample| sample.to_le_bytes()))
            .collect()
    }
    fn decode(bytes: &[u8]) -> Vec<[f32; 2]> {
        bytes
            .chunks_exact(8)
            .map(|b| {
                [
                    f32::from_le_bytes(b[..4].try_into().unwrap()),
                    f32::from_le_bytes(b[4..].try_into().unwrap()),
                ]
            })
            .collect()
    }
    fn filtered(frames: u64) -> Vec<[f32; 2]> {
        (0..frames + PAD_FRAMES)
            .map(|n| [n as f32 + 0.25, -(n as f32 + 0.5)])
            .collect()
    }
    fn copy(bytes: &[u8], frames: u64, h: &History) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        copy_filtered(Cursor::new(bytes), &mut out, frames, h, || Ok(()))?;
        Ok(out)
    }

    #[test]
    fn compensates_delay_preserves_last_sample_and_drains_exact_pad() {
        let n = 137;
        let source = encode(&filtered(n));
        let mut input = Cursor::new(&source);
        let mut out = Vec::new();
        copy_filtered(&mut input, &mut out, n, &history(), || Ok(())).unwrap();
        assert_eq!(input.position(), (n + PAD_FRAMES) * FRAME_BYTES);
        assert_eq!(out.len() as u64, n * FRAME_BYTES);
        let actual = decode(&out);
        assert_eq!(actual[0], [1200.25, -1200.5]);
        assert_eq!(actual[n as usize - 1], [1336.25, -1336.5]);
    }
    #[test]
    fn reapplies_mic_mask_after_filter_with_initial_and_same_sample_edges() {
        let n = 7;
        let h = History {
            initial_mask: 2,
            roles: 3,
            edges: vec![
                Edge {
                    ordinal: 1,
                    sample: 2,
                    command: 1,
                    mask: 0,
                },
                Edge {
                    ordinal: 2,
                    sample: 2,
                    command: 2,
                    mask: 2,
                },
                Edge {
                    ordinal: 3,
                    sample: 3,
                    command: 3,
                    mask: 1,
                },
                Edge {
                    ordinal: 4,
                    sample: 5,
                    command: 4,
                    mask: 3,
                },
            ],
        };
        let out = decode(&copy(&encode(&filtered(n)), n, &h).unwrap());
        assert_eq!(&out[..3], &[[0.; 2]; 3]);
        assert_ne!(out[3], [0.; 2]);
        assert_ne!(out[4], [0.; 2]);
        assert_eq!(&out[5..], &[[0.; 2]; 2]);
        assert!(out.iter().flatten().all(|f| f.is_finite()));
    }
    #[test]
    fn rejects_short_extra_partial_and_nonfinite_output_even_in_discarded_tail() {
        let n = 3;
        let bytes = encode(&filtered(n));
        assert!(copy(&bytes[..bytes.len() - 1], n, &history()).is_err());
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(copy(&extra, n, &history()).is_err());
        for position in [0, DELAY_FRAMES as usize, (n + PAD_FRAMES - 1) as usize] {
            for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
                let mut frames = filtered(n);
                frames[position][1] = value;
                assert!(copy(&encode(&frames), n, &history()).is_err());
            }
        }
    }
    #[test]
    fn feeder_uses_only_complete_authoritative_prefix_and_rejects_invalid_samples() {
        let mut source = encode(&[[0.25, -0.5], [0.75, -1.]]);
        source.extend([1, 2, 3]);
        let mut out = Vec::new();
        feed_prefix(Cursor::new(&source), &mut out, 2).unwrap();
        assert_eq!(out, source[..16]);
        assert!(feed_prefix(Cursor::new(&source[..15]), Vec::new(), 2).is_err());
        assert!(feed_prefix(Cursor::new(encode(&[[f32::NAN, 0.]])), Vec::new(), 1).is_err());
    }
    #[test]
    fn drains_noisy_stderr_with_fixed_memory_and_duration_limit_scales() {
        let bytes = vec![b'x'; 512 * 1024];
        let d = drain_diagnostics(Cursor::new(bytes)).unwrap();
        assert_eq!(d.bytes.len(), STDERR_BYTES);
        assert_eq!(d.total, 512 * 1024);
        assert_eq!(budget(48_000).unwrap(), Duration::from_secs(124));
        assert_eq!(
            budget(48_000 * 60 * 60 * 3).unwrap(),
            Duration::from_secs(43_320)
        );
        assert!(
            copy(
                &encode(&filtered(1)),
                1,
                &History {
                    roles: 1,
                    ..history()
                }
            )
            .is_err()
        );
    }
}
