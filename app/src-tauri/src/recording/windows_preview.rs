//! Preview never owns the recording stop flag or blocks its capture thread.
use super::{
    preview::{self, Frame, FrameHeader},
    source::Geometry,
};
use std::{
    sync::{Arc, Condvar, Mutex, atomic::AtomicU64},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Lease {
    pub id: String,
    pub epoch: u64,
    pub until: Instant,
}
#[derive(Default)]
struct State {
    lease: Option<Lease>,
    last: Option<Lease>,
    geometry: Option<Geometry>,
    expired: bool,
    renew_sequence: u64,
    viewport: Option<preview::Viewport>,
    presented_frames: u64,
    presented_sequence: u64,
    presented_epoch: u64,
    presented_id: String,
    presented_at: Option<String>,
    captured_at: Option<String>,
    native_window: Option<String>,
    native_visible: bool,
    native_content: Option<preview::ContentRect>,
    presented_width: u32,
    presented_height: u32,
    border_info: Option<serde_json::Value>,
    failure: Option<String>,
    transport: Option<&'static str>,
}
#[derive(Default)]
pub struct Controls {
    state: Mutex<State>,
    sequence: AtomicU64,
}
impl Controls {
    pub fn renew(&self, id: &str, epoch: u64, lease_ms: u64) {
        if id.is_empty()
            || id.len() > 100
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || epoch == 0
            || epoch > 9_007_199_254_740_991
            || lease_ms == 0
        {
            return;
        }
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(old) = &s.lease
            && old.id == id
            && old.epoch > epoch
        {
            return;
        }
        let lease = Lease {
            id: id.into(),
            epoch,
            until: Instant::now() + Duration::from_millis(lease_ms.min(preview::LEASE_MS)),
        };
        if s.last
            .as_ref()
            .is_none_or(|old| old.id != id || old.epoch != epoch)
        {
            s.failure = None;
            s.native_visible = false;
            s.native_window = None;
        }
        s.last = Some(lease.clone());
        s.lease = Some(lease);
        s.expired = false;
        s.renew_sequence = s.renew_sequence.saturating_add(1);
    }
    pub fn renew_viewport(
        &self,
        id: &str,
        epoch: u64,
        lease_ms: u64,
        viewport: Option<preview::Viewport>,
    ) {
        self.renew(id, epoch, lease_ms);
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.lease
            .as_ref()
            .is_some_and(|lease| lease.id == id && lease.epoch == epoch)
        {
            s.viewport = viewport.filter(|viewport| viewport.validate().is_ok());
        }
    }
    pub fn target(&self) -> Option<(Lease, preview::Viewport)> {
        let lease = self.current()?;
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s
            .lease
            .as_ref()
            .is_some_and(|actual| actual.id == lease.id && actual.epoch == lease.epoch)
        {
            return None;
        }
        Some((lease, s.viewport.clone()?))
    }
    pub fn target_matches(&self, lease: &Lease, viewport: &preview::Viewport) -> bool {
        self.target().is_some_and(|(current, target)| {
            current.id == lease.id
                && current.epoch == lease.epoch
                && target.revision == viewport.revision
        })
    }
    pub fn fail(&self, reason: &str) {
        self.off();
        self.state.lock().unwrap_or_else(|e| e.into_inner()).failure = Some(reason.into());
    }
    pub fn has_presented(&self, lease: &Lease) -> bool {
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.native_visible
            && s.presented_id == lease.id
            && s.presented_epoch == lease.epoch
            && s.lease.as_ref().is_some_and(|current| {
                current.id == lease.id
                    && current.epoch == lease.epoch
                    && Instant::now() < current.until
            })
    }
    pub fn hidden(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.native_visible = false;
        s.native_window = None;
    }
    pub fn transport(&self, bitmap: bool) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .transport = Some(if bitmap { "bitmap" } else { "native" });
    }
    pub fn border_info(&self, value: serde_json::Value) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .border_info = Some(value);
    }
    pub fn presented(
        &self,
        lease: &Lease,
        viewport: &preview::Viewport,
        frame: &super::surface::Presented,
        captured_qpc: i64,
        presented_qpc: i64,
    ) {
        if !self.target_matches(lease, viewport) {
            return;
        }
        let Some(sequence) = super::allocate_preview_sequence(&self.sequence) else {
            self.off();
            return;
        };
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !s
            .lease
            .as_ref()
            .is_some_and(|current| current.id == lease.id && current.epoch == lease.epoch)
        {
            return;
        }
        s.presented_frames = s
            .presented_frames
            .saturating_add(1)
            .min(9_007_199_254_740_991);
        s.presented_sequence = sequence;
        s.presented_epoch = lease.epoch;
        s.presented_id = lease.id.clone();
        s.presented_at = Some(presented_qpc.max(0).to_string());
        s.captured_at = Some(captured_qpc.max(0).to_string());
        s.native_window = Some(frame.hwnd.clone());
        s.native_visible = true;
        s.native_content = Some(frame.content);
        s.presented_width = frame.width;
        s.presented_height = frame.height;
    }
    pub fn off(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.lease = None;
        s.viewport = None;
        s.native_visible = false;
        s.native_window = None;
        s.expired = true;
    }
    pub fn current(&self) -> Option<Lease> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.lease
            .as_ref()
            .is_some_and(|lease| Instant::now() >= lease.until)
        {
            s.lease = None;
            s.expired = true;
        }
        s.lease.clone()
    }
    pub fn resized(&self, geometry: Geometry) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.geometry = Some(geometry);
        if let Some(lease) = s.lease.as_mut() {
            lease.epoch = lease.epoch.saturating_add(1);
            s.last = Some(lease.clone());
        }
    }
    pub fn geometry(&self, geometry: Geometry) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .geometry = Some(geometry);
    }
    pub fn notice(&self) -> Option<serde_json::Value> {
        let _ = self.current();
        let s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let lease = s.last.as_ref()?;
        let geometry = s.geometry.unwrap_or(Geometry {
            width: 0,
            height: 0,
        });
        let current = s.presented_epoch == lease.epoch && s.presented_id == lease.id;
        Some(
            serde_json::json!({"previewId":lease.id,"configEpoch":lease.epoch,"sourceWidth":geometry.width,"sourceHeight":geometry.height,
            "expired":s.expired,"renewSequence":s.renew_sequence,"leaseRemainingMs":lease.until.saturating_duration_since(Instant::now()).as_millis(),
            "transport":s.transport.unwrap_or("native"),"presentedFrames":s.presented_frames,"presentedSequence":s.presented_sequence,
            "presentedAtQpc100ns":if current {s.presented_at.as_deref()} else {None},"capturedAtQpc100ns":if current {s.captured_at.as_deref()} else {None},
            "nativeWindow":if current {s.native_window.as_deref()} else {None},"nativeVisible":current && s.native_visible,
            "viewportWidth":if current{s.presented_width}else{0},"viewportHeight":if current{s.presented_height}else{0},
            "nativeContentRect":if current {s.native_content} else {None},"captureBorder":s.border_info,"error":s.failure}),
        )
    }
}
struct Job {
    lease: Lease,
    qpc: i64,
    pixels: Arc<Vec<u8>>,
    geometry: Geometry,
    frame: Option<Frame>,
}
#[derive(Default)]
struct Pending {
    job: Option<Job>,
    closed: bool,
}
#[derive(Default)]
struct Latest {
    pending: Mutex<Pending>,
    ready: Condvar,
}
impl Latest {
    fn offer(&self, job: Job) {
        // Never wait behind preview work on the recording capture thread.
        if let Ok(mut pending) = self.pending.try_lock()
            && !pending.closed
        {
            pending.job = Some(job);
            self.ready.notify_one();
        }
    }
    fn discard_pending(&self) -> bool {
        if let Ok(mut pending) = self.pending.try_lock()
            && !pending.closed
        {
            pending.job = None;
            return true;
        }
        false
    }
    fn take(&self) -> Option<Job> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if pending.closed {
                return None;
            }
            if let Some(job) = pending.job.take() {
                return Some(job);
            }
            pending = self.ready.wait(pending).unwrap_or_else(|e| e.into_inner());
        }
    }
    fn close(&self) {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        pending.closed = true;
        pending.job = None;
        self.ready.notify_one();
    }
}
pub struct Emitter {
    control: Arc<Controls>,
    latest: Arc<Latest>,
    legacy_last: Option<Instant>,
}
impl Emitter {
    pub fn new(control: Arc<Controls>) -> Result<Self, String> {
        let latest = Arc::new(Latest::default());
        let writer_latest = latest.clone();
        let writer_control = control.clone();
        std::thread::Builder::new()
            .name("recording-preview-pipe".into())
            .spawn(move || {
                let mut output = std::io::stderr().lock();
                let valid = |job: &Job| {
                    writer_control.current().is_some_and(|lease| {
                        lease.id == job.lease.id && lease.epoch == job.lease.epoch
                    })
                };
                while let Some(mut job) = writer_latest.take() {
                    if !valid(&job) {
                        continue;
                    }
                    let frame = if let Some(frame) = job.frame.take() {
                        frame
                    } else {
                        let Ok((width, height, bmp)) = preview::thumbnail(
                            &job.pixels,
                            job.geometry.width,
                            job.geometry.height,
                        ) else {
                            writer_control.off();
                            continue;
                        };
                        let Some(sequence) =
                            super::allocate_preview_sequence(&writer_control.sequence)
                        else {
                            writer_control.off();
                            continue;
                        };
                        let header = FrameHeader {
                            preview_id: job.lease.id.clone(),
                            config_epoch: job.lease.epoch,
                            sequence,
                            width,
                            height,
                            captured_at_qpc100ns: job.qpc.max(0).to_string(),
                        };
                        let Ok(frame) = Frame::from_bmp(header, bmp) else {
                            writer_control.off();
                            continue;
                        };
                        frame
                    };
                    if !valid(&job) {
                        continue;
                    }
                    if preview::write_frame(&mut output, &frame).is_err() {
                        writer_control.off();
                        break;
                    }
                }
            })
            .map_err(|_| "Cannot start bounded preview delivery.")?;
        Ok(Self {
            control,
            latest,
            legacy_last: None,
        })
    }
    /// Rare legacy-admission sources retain their previous 480x270/5Hz tap.
    /// Inline conversion retains no additional full-resolution NV12; the sole
    /// writer and replaceable pending slot contain only bounded encoded frames.
    pub fn wants_legacy_frame(&self) -> bool {
        self.control.current().is_some()
            && self
                .legacy_last
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(200))
    }
    pub fn offer_legacy(&mut self, qpc: i64, pixels: &[u8], geometry: Geometry) {
        if self
            .legacy_last
            .is_some_and(|last| last.elapsed() < Duration::from_millis(200))
        {
            return;
        }
        let Some(lease) = self.control.current() else {
            return;
        };
        if !self.latest.discard_pending() {
            return;
        }
        self.legacy_last = Some(Instant::now());
        let Ok((width, height, bmp)) =
            preview::thumbnail_with_bounds(pixels, geometry.width, geometry.height, (480, 270))
        else {
            self.control.off();
            return;
        };
        let Some(sequence) = super::allocate_preview_sequence(&self.control.sequence) else {
            self.control.off();
            return;
        };
        let header = FrameHeader {
            preview_id: lease.id.clone(),
            config_epoch: lease.epoch,
            sequence,
            width,
            height,
            captured_at_qpc100ns: qpc.max(0).to_string(),
        };
        let Ok(frame) = Frame::from_bmp(header, bmp) else {
            self.control.off();
            return;
        };
        self.latest.offer(Job {
            lease,
            qpc,
            pixels: Arc::new(Vec::new()),
            geometry,
            frame: Some(frame),
        });
    }
}
impl Drop for Emitter {
    fn drop(&mut self) {
        // A blocked pipe cannot delay media drain; process/job exit closes it.
        // Only one working frame and one replaceable pending frame are retained.
        self.latest.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(qpc: i64) -> Job {
        Job {
            lease: Lease {
                id: "preview-1".into(),
                epoch: 1,
                until: Instant::now() + Duration::from_secs(1),
            },
            qpc,
            pixels: Arc::new(vec![0]),
            geometry: Geometry {
                width: 1,
                height: 1,
            },
            frame: None,
        }
    }
    #[test]
    fn native_readiness_requires_actual_present_for_current_live_generation() {
        let controls = Controls::default();
        let target = preview::Viewport {
            parent_hwnd: "100".into(),
            parent_pid: 123,
            x: 0,
            y: 0,
            width: 800,
            height: 600,
            revision: 1,
        };
        controls.renew_viewport("preview-1", 1, preview::LEASE_MS, Some(target.clone()));
        let lease = controls.current().unwrap();
        assert!(!controls.has_presented(&lease));
        let frame = super::super::surface::Presented {
            hwnd: "200".into(),
            content: preview::ContentRect {
                left: 0,
                top: 0,
                width: 800,
                height: 600,
            },
            width: 800,
            height: 600,
        };
        controls.presented(&lease, &target, &frame, 10, 20);
        assert!(controls.has_presented(&lease));
        controls.resized(Geometry {
            width: 1000,
            height: 600,
        });
        assert!(!controls.has_presented(&lease));
        let next = controls.current().unwrap();
        assert!(!controls.has_presented(&next));
        controls.off();
        assert!(!controls.has_presented(&next));
    }
    #[test]
    fn scaled_producer_releases_pending_before_allocating_replacement() {
        let latest = Latest::default();
        latest.offer(job(1));
        let working = latest.take().unwrap();
        let obsolete = job(2);
        let obsolete_pixels = Arc::downgrade(&obsolete.pixels);
        latest.offer(obsolete);
        assert!(latest.discard_pending());
        assert!(obsolete_pixels.upgrade().is_none());
        let replacement = job(3);
        latest.offer(replacement);
        assert_eq!(working.qpc, 1);
        assert_eq!(latest.take().unwrap().qpc, 3);
        latest.close();
    }
    #[test]
    fn slow_worker_keeps_only_newest_pending_frame_and_close_drops_it() {
        let latest = Latest::default();
        latest.offer(job(1));
        let working = latest.take().unwrap();
        let overwritten = job(2);
        let weak = Arc::downgrade(&overwritten.pixels);
        latest.offer(overwritten);
        latest.offer(job(3));
        assert!(weak.upgrade().is_none());
        assert_eq!(working.qpc, 1);
        assert_eq!(latest.take().unwrap().qpc, 3);
        let pending = job(4);
        let weak = Arc::downgrade(&pending.pixels);
        latest.offer(pending);
        latest.close();
        assert!(weak.upgrade().is_none());
        assert!(latest.take().is_none());
    }
    #[test]
    fn busy_preview_slot_never_waits_on_capture_producer() {
        let latest = Latest::default();
        let held = latest.pending.lock().unwrap();
        latest.offer(job(1));
        drop(held);
        assert!(latest.pending.lock().unwrap().job.is_none());
    }
    #[test]
    fn stale_epochs_do_not_renew_and_expiry_does_not_own_recording_stop() {
        let controls = Controls::default();
        controls.renew("preview-1", 1, preview::LEASE_MS);
        controls.resized(Geometry {
            width: 640,
            height: 480,
        });
        assert_eq!(controls.current().unwrap().epoch, 2);
        controls.renew("preview-1", 1, preview::LEASE_MS);
        assert_eq!(controls.current().unwrap().epoch, 2);
        controls.off();
        assert!(controls.current().is_none());
    }
}
