//! One reliable ready/terminal slot and one latest progress slot; never pipe-wait in capture.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
};
#[derive(Default)]
struct Pending {
    ready: Option<Vec<u8>>,
    latest: Option<Vec<u8>>,
    terminal: Option<Vec<u8>>,
    saw_ready: bool,
    done: bool,
    reliable: VecDeque<Vec<u8>>,
}
impl Pending {
    fn progress(&mut self, bytes: Vec<u8>, ready: bool, reliable: bool) -> bool {
        if self.done || self.terminal.is_some() {
            return true;
        }
        if ready && !self.saw_ready {
            self.latest = None;
            self.saw_ready = true;
            self.ready = Some(bytes);
        } else if reliable {
            if self.reliable.len() == 16 {
                return false;
            }
            self.latest = None;
            self.reliable.push_back(bytes);
        } else {
            self.latest = Some(bytes);
        }
        true
    }
    fn take(&mut self) -> Option<Vec<u8>> {
        if self.ready.is_some() {
            return self.ready.take();
        }
        if let Some(bytes) = self.reliable.pop_front() {
            return Some(bytes);
        }
        if self.terminal.is_some() {
            self.done = true;
            self.latest = None;
            return self.terminal.take();
        }
        self.latest.take()
    }
}
pub struct Reporter {
    state: Arc<(Mutex<Pending>, Condvar)>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
fn encode(mut value: Value) -> Result<Vec<u8>, String> {
    value["protocolVersion"] = json!(4);
    let mut bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    if bytes.len() > 65_536 {
        return Err("Recording status exceeded its byte limit.".into());
    }
    Ok(bytes)
}
impl Reporter {
    pub fn new() -> Result<Arc<Self>, String> {
        let state = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let output = state.clone();
        let worker = std::thread::Builder::new()
            .name("recording-status".into())
            .spawn(move || {
                let mut stdout = std::io::stdout().lock();
                loop {
                    let bytes = {
                        let (state, signal) = (&output.0, &output.1);
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(bytes) = guard.take() {
                                break Some(bytes);
                            }
                            if guard.done {
                                break None;
                            }
                            guard = signal.wait(guard).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    let Some(bytes) = bytes else {
                        break;
                    };
                    if stdout
                        .write_all(&bytes)
                        .and_then(|_| stdout.flush())
                        .is_err()
                    {
                        std::process::exit(2);
                    }
                }
            })
            .map_err(|e| format!("Cannot start recording status writer: {e}"))?;
        Ok(Arc::new(Self {
            state,
            worker: Mutex::new(Some(worker)),
        }))
    }
    pub fn progress(&self, value: Value) {
        let ready = value["ready"] == true;
        let reliable = value["transition"].is_object()
            || value["audioReceipts"]
                .as_array()
                .is_some_and(|a| !a.is_empty())
            || value["reconcileAck"].is_object()
            || value["reconcileError"].is_object()
            || value["hudIntent"].is_string()
            || value["stopHudVisible"] == true;
        let Ok(bytes) = encode(json!({"type":"progress","value":value})) else {
            std::process::exit(2);
        };
        let mut state = self.state.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.progress(bytes, ready, reliable) {
            std::process::exit(2);
        }
        self.state.1.notify_one();
    }
    pub fn finish(&self, value: Value) {
        let bytes = encode(value).unwrap_or_else(|_| {
            encode(json!({"type":"error","message":"Recording status exceeded its byte limit."}))
                .unwrap()
        });
        {
            let mut state = self.state.0.lock().unwrap_or_else(|e| e.into_inner());
            state.terminal = Some(bytes);
            self.state.1.notify_one();
        }
        // Media is already stopped. Parent/job watchdog owns a stalled terminal pipe.
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalescing_preserves_first_ready_and_terminal_summary() {
        let mut p = Pending {
            ready: Some(vec![1]),
            latest: Some(vec![2]),
            terminal: Some(vec![3]),
            saw_ready: true,
            done: false,
            reliable: VecDeque::new(),
        };
        assert_eq!(p.take(), Some(vec![1]));
        assert_eq!(p.take(), Some(vec![3]));
        assert!(p.take().is_none());
        assert!(p.done);
    }
    #[test]
    fn reliable_boundary_discards_older_latest_but_preserves_newer_progress() {
        let mut p = Pending::default();
        assert!(p.progress(vec![1], false, false));
        assert!(p.progress(vec![2], true, true));
        assert_eq!(p.take(), Some(vec![2]));
        assert_eq!(p.take(), None);
        assert!(p.progress(vec![3], false, false));
        assert!(p.progress(vec![4], true, true));
        assert!(p.progress(vec![5], false, false));
        assert_eq!(p.take(), Some(vec![4]));
        assert_eq!(p.take(), Some(vec![5]));
        assert_eq!(p.take(), None);
    }
    #[test]
    fn encoded_messages_include_version_and_enforce_full_newline_bound() {
        assert!(encode(json!({"message":"x".repeat(65_536)})).is_err());
        let b = encode(json!({"type":"result","value":{}})).unwrap();
        assert!(b.ends_with(b"\n"));
        assert_eq!(
            serde_json::from_slice::<Value>(&b).unwrap()["protocolVersion"],
            4
        );
    }
}
