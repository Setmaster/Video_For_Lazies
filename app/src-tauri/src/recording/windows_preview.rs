//! Preview never owns the recording stop flag or blocks its capture thread.
use super::{
    preview::{self, Frame, FrameHeader},
    source::Geometry,
};
use std::{
    sync::{Arc, Mutex, atomic::AtomicU64, mpsc},
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
        s.last = Some(lease.clone());
        s.lease = Some(lease);
        s.expired = false;
        s.renew_sequence = s.renew_sequence.saturating_add(1);
    }
    pub fn off(&self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.lease = None;
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
        Some(
            serde_json::json!({"previewId":lease.id,"configEpoch":lease.epoch,"sourceWidth":geometry.width,"sourceHeight":geometry.height,"expired":s.expired,"renewSequence":s.renew_sequence,"leaseRemainingMs":lease.until.saturating_duration_since(Instant::now()).as_millis()}),
        )
    }
}
pub struct Emitter {
    control: Arc<Controls>,
    send: Option<mpsc::SyncSender<Frame>>,
    last: Instant,
}
impl Emitter {
    pub fn new(control: Arc<Controls>) -> Result<Self, String> {
        let (send, receive) = mpsc::sync_channel::<Frame>(1);
        let writer_control = control.clone();
        std::thread::Builder::new()
            .name("recording-preview-pipe".into())
            .spawn(move || {
                let mut output = std::io::stderr().lock();
                while let Ok(frame) = receive.recv() {
                    if writer_control.current().is_none_or(|lease| {
                        lease.id != frame.header.preview_id
                            || lease.epoch != frame.header.config_epoch
                    }) {
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
            send: Some(send),
            last: Instant::now() - Duration::from_secs(1),
        })
    }
    pub fn offer(&mut self, qpc: i64, nv12: &[u8], geometry: Geometry) {
        if self.last.elapsed() < Duration::from_millis(200) {
            return;
        }
        let Some(lease) = self.control.current() else {
            return;
        };
        self.last = Instant::now();
        let Ok((width, height, bmp)) = preview::thumbnail(nv12, geometry.width, geometry.height)
        else {
            self.control.off();
            return;
        };
        let Some(sequence) = super::allocate_preview_sequence(&self.control.sequence) else {
            self.control.off();
            return;
        };
        let header = FrameHeader {
            preview_id: lease.id,
            config_epoch: lease.epoch,
            sequence,
            width,
            height,
            captured_at_qpc100ns: qpc.max(0).to_string(),
        };
        if let Ok(frame) = Frame::new(header, &bmp)
            && let Some(send) = &self.send
        {
            let _ = send.try_send(frame);
        }
    }
}
impl Drop for Emitter {
    fn drop(&mut self) {
        self.send = None; /* A blocked writer cannot delay media drain; process/job exit closes its pipe. */
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
