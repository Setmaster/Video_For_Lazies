//! Resolution-preserving H.264 fallback. This process receives already-clocked
//! NV12 samples; it never opens a screen or audio device.
use super::Config;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) struct Writer {
    child: Child,
    sender: Option<SyncSender<Arc<Vec<u8>>>>,
    worker: Option<JoinHandle<Result<(), String>>>,
    diagnostics: Arc<Mutex<Vec<u8>>>,
    stderr_thread: Option<JoinHandle<()>>,
    pcm: Option<File>,
    audio_samples: u64,
    video_frames: u64,
    fps: u32,
    video_bytes: usize,
    finished: bool,
}
impl Writer {
    pub(super) fn new(config: &Config) -> Result<Self, String> {
        let ffmpeg = config.ffmpeg_path.as_ref().ok_or("The recording encoder cannot handle this resolution and the bundled fallback is unavailable. Restore the complete app folder.")?;
        let width = (config.region.width + 1) & !1;
        let height = (config.region.height + 1) & !1;
        let has_audio =
            config.system_audio_device_id.is_some() || config.microphone_device_id.is_some();
        let pcm = if has_audio {
            Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(config.spool_path.with_file_name("capture.pcm"))
                    .map_err(|e| format!("Prepare common-clock audio spool: {e}"))?,
            )
        } else {
            None
        };
        let mut child = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-f",
                "rawvideo",
                "-pixel_format",
                "nv12",
                "-video_size",
            ])
            .arg(format!("{width}x{height}"))
            .args([
                "-framerate",
                &config.fps.to_string(),
                "-i",
                "pipe:0",
                "-an",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-crf",
            ])
            .arg(if config.quality == "high" { "18" } else { "23" })
            .args([
                "-g",
                &(config.fps * 2).to_string(),
                "-bf",
                "0",
                "-color_range",
                "tv",
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-bsf:v",
                "h264_metadata=colour_primaries=1:transfer_characteristics=1:matrix_coefficients=1:video_full_range_flag=0",
                "-movflags",
                "+frag_keyframe+empty_moov+default_base_moof",
                "-frag_duration",
                "2000000",
                "-flush_packets",
                "1",
                "-f",
                "mp4",
                "-n",
            ])
            .arg(&config.spool_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // The child inherits the recorder host's enclosing Windows job.
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| format!("Start bundled H.264 fallback: {e}"))?;
        let mut input = child
            .stdin
            .take()
            .ok_or("H.264 fallback input is unavailable")?;
        let mut errors = child
            .stderr
            .take()
            .ok_or("H.264 fallback diagnostics are unavailable")?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let tail = diagnostics.clone();
        let stderr_thread = thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(size) = errors.read(&mut buffer) {
                if size == 0 {
                    break;
                }
                if let Ok(mut log) = tail.lock() {
                    log.extend_from_slice(&buffer[..size]);
                    let excess = log.len().saturating_sub(65_536);
                    if excess > 0 {
                        log.drain(..excess);
                    }
                }
            }
        });
        // Rendezvous: no queued frame allocation. A stalled pipe cannot build
        // an unbounded backlog or stall the capture thread indefinitely.
        let (sender, receiver) = mpsc::sync_channel::<Arc<Vec<u8>>>(0);
        let worker = thread::spawn(move || {
            while let Ok(frame) = receiver.recv() {
                input
                    .write_all(&frame)
                    .map_err(|e| format!("H.264 fallback pipe: {e}"))?;
            }
            input
                .flush()
                .map_err(|e| format!("Flush H.264 fallback input: {e}"))
        });
        Ok(Self {
            child,
            sender: Some(sender),
            worker: Some(worker),
            diagnostics,
            stderr_thread: Some(stderr_thread),
            pcm,
            audio_samples: 0,
            video_frames: 0,
            fps: config.fps,
            video_bytes: (width as usize * height as usize * 3 / 2),
            finished: false,
        })
    }
    fn error(&self, context: &str) -> String {
        let detail = self
            .diagnostics
            .lock()
            .ok()
            .map(|log| {
                String::from_utf8_lossy(&log)
                    .chars()
                    .take(2000)
                    .collect::<String>()
            })
            .unwrap_or_default();
        format!("{context}. {detail}").trim().to_string()
    }
    pub(super) fn sample(&mut self, index: u32, bytes: &[u8], pts: i64) -> Result<(), String> {
        if index == 1 {
            let expected = (self.audio_samples as i128 * 10_000_000 / 48_000) as i64;
            if pts != expected || !bytes.len().is_multiple_of(4) {
                return Err("Common-clock PCM audio sample order is invalid".into());
            }
            self.pcm
                .as_mut()
                .ok_or("Unexpected fallback audio stream")?
                .write_all(bytes)
                .map_err(|e| format!("Write common-clock audio spool: {e}"))?;
            self.audio_samples += (bytes.len() / 4) as u64;
            return Ok(());
        }
        if index != 0 {
            return Err("Unexpected fallback video stream".into());
        }
        self.video_sample(Arc::new(bytes.to_vec()), pts)
    }
    pub(super) fn video_sample(&mut self, frame: Arc<Vec<u8>>, pts: i64) -> Result<(), String> {
        let expected_pts = (self.video_frames as i128 * 10_000_000 / i128::from(self.fps)) as i64;
        if pts != expected_pts || frame.len() != self.video_bytes {
            return Err("Common-clock NV12 video sample order or size is invalid".into());
        }
        if self
            .child
            .try_wait()
            .map_err(|e| format!("Check H.264 fallback: {e}"))?
            .is_some()
        {
            return Err(self.error("The bundled H.264 fallback stopped"));
        }
        let mut frame = frame;
        let deadline = Instant::now() + Duration::from_millis(100);
        loop {
            match self
                .sender
                .as_ref()
                .ok_or("H.264 fallback input was closed")?
                .try_send(frame)
            {
                Ok(()) => {
                    self.video_frames += 1;
                    return Ok(());
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(self.error("The H.264 fallback input stopped"));
                }
                Err(TrySendError::Full(pending)) => {
                    if Instant::now() >= deadline {
                        return Err(self.error("The encoder cannot keep up with this recording; the partial recording was retained"));
                    }
                    frame = pending;
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
    pub(super) fn finish(&mut self) -> Result<(), String> {
        self.sender.take();
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|e| format!("Finalize H.264 fallback: {e}"))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(self.error("The H.264 fallback did not finish within 20 seconds"));
            }
            thread::sleep(Duration::from_millis(5));
        };
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "H.264 fallback writer thread failed")??;
        }
        if !status.success() {
            return Err(self.error("The H.264 fallback could not finalize the recording"));
        }
        if let Some(pcm) = &mut self.pcm {
            pcm.flush()
                .and_then(|_| pcm.sync_all())
                .map_err(|e| format!("Synchronize common-clock audio spool: {e}"))?;
        }
        self.finished = true;
        Ok(())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        self.sender.take();
        if !self.finished {
            let _ = self.child.kill();
        }
        // Killing/closing the child releases a blocked pipe write. Never wait
        // without a bound while unwinding a native recording error.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if self.child.try_wait().ok().flatten().is_some() || Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.worker.take().unwrap().join();
        }
        if self
            .stderr_thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            let _ = self.stderr_thread.take().unwrap().join();
        }
    }
}
