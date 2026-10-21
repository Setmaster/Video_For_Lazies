//! Source-independent sample-clock controls and deterministic post-gain meters.
use super::audio_types::*;
use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Command {
    pub levels: AudioLevels,
    pub received_qpc: i64,
    pub edge: bool,
}
#[derive(Clone)]
pub struct ControlQueue {
    claimed: VecDeque<Command>,
    pending: VecDeque<Command>,
    recent: VecDeque<Command>,
    last: Option<AudioLevels>,
    initial: [bool; 2],
}
impl ControlQueue {
    pub fn new(render: MixLevel, microphone: MixLevel) -> Self {
        Self {
            claimed: VecDeque::new(),
            pending: VecDeque::new(),
            recent: VecDeque::new(),
            last: None,
            initial: [render.muted, microphone.muted],
        }
    }
    /// Identical retries retain their first receipt timestamp. Old settled
    /// sequences beyond the bounded retry window are stale, never rearmed.
    pub fn push(&mut self, levels: AudioLevels, received_qpc: i64) -> Result<bool, String> {
        levels.validate()?;
        if let Some(old) = self
            .claimed
            .iter()
            .chain(self.pending.iter())
            .chain(self.recent.iter())
            .find(|c| c.levels.command_sequence == levels.command_sequence)
        {
            return if old.levels == levels {
                Ok(false)
            } else {
                Err("Conflicting duplicate audio control.".into())
            };
        }
        if self
            .last
            .as_ref()
            .is_some_and(|last| levels.command_sequence <= last.command_sequence)
        {
            return Err("This audio control is stale.".into());
        }
        let previous = self
            .last
            .as_ref()
            .map(|l| [l.render_level.muted, l.microphone_level.muted])
            .unwrap_or(self.initial);
        let edge = previous != [levels.render_level.muted, levels.microphone_level.muted];
        let command = Command {
            levels: levels.clone(),
            received_qpc,
            edge,
        };
        let replace = !edge
            && self
                .pending
                .back()
                .is_some_and(|old| !old.edge && old.levels.same_mutes(&levels));
        if !replace && self.len() >= CONTROL_CAPACITY {
            return Err("Audio controls exceeded their safe queue; recording stopped without losing mute history.".into());
        }
        if replace {
            self.pending.pop_back();
        }
        self.pending.push_back(command.clone());
        self.recent.push_back(command);
        if self.recent.len() > CONTROL_CAPACITY {
            self.recent.pop_front();
        }
        self.last = Some(levels);
        Ok(true)
    }
    /// Retry the oldest in-flight command, retaining its first receipt.
    pub fn claim(&mut self) -> Option<Command> {
        if self.claimed.is_empty() {
            self.claim_next()
        } else {
            self.claimed.front().cloned()
        }
    }
    /// Claim another command without settling earlier in-flight mute edges.
    pub fn claim_next(&mut self) -> Option<Command> {
        let command = self.pending.pop_front()?;
        self.claimed.push_back(command.clone());
        Some(command)
    }
    pub fn claim_due(&mut self, zero: i64, sample: u64, cutoff: Option<u64>) -> Option<Command> {
        let target = control_sample(zero, self.pending.front()?.received_qpc);
        if target > sample || cutoff.is_some_and(|cutoff| target >= cutoff) {
            return None;
        }
        self.claim_next()
    }
    pub fn pending_front(&self) -> Option<&Command> {
        self.pending.front()
    }
    pub fn settle(&mut self, sequence: u64) {
        while self
            .claimed
            .front()
            .is_some_and(|c| c.levels.command_sequence <= sequence)
        {
            self.claimed.pop_front();
        }
    }
    pub fn front(&self) -> Option<&Command> {
        self.claimed.front().or_else(|| self.pending.front())
    }
    pub fn edge_count(&self) -> u64 {
        self.pending
            .iter()
            .chain(self.claimed.iter())
            .filter(|c| c.edge)
            .count() as u64
    }
    pub fn last_sequence(&self) -> u64 {
        self.last.as_ref().map_or(0, |l| l.command_sequence)
    }
    pub fn len(&self) -> usize {
        self.pending.len() + self.claimed.len()
    }
}
/// Convert real native receipt time into a future common 10ms media boundary.
/// This intentionally does not use the currently rendered (100ms-old) block.
pub fn control_sample(zero: i64, received: i64) -> u64 {
    let elapsed = i128::from(received).saturating_sub(i128::from(zero)).max(0);
    let samples = (elapsed * 48_000 + 9_999_999) / 10_000_000;
    ((samples + 479) / 480 * 480).min(i128::from(u64::MAX)) as u64
}
#[derive(Clone, Copy)]
struct Level {
    current: f32,
    target: f32,
    remaining: u32,
    muted: bool,
}
impl Level {
    fn new(level: MixLevel) -> Self {
        Self {
            current: level.gain,
            target: level.gain,
            remaining: 0,
            muted: level.muted,
        }
    }
    fn set(&mut self, value: MixLevel) {
        if self.target != value.gain || self.muted != value.muted {
            if self.muted && !value.muted {
                self.current = 0.;
            }
            self.target = value.gain;
            self.remaining = 240;
        }
        self.muted = value.muted;
    }
    fn sample(&mut self, input: [f32; 2]) -> Result<[f32; 2], String> {
        if self.remaining > 0 {
            self.current += (self.target - self.current) / self.remaining as f32;
            self.remaining -= 1;
        }
        if self.muted {
            return Ok([0.; 2]);
        }
        let out = input.map(|v| v * self.current);
        if out.iter().any(|v| !v.is_finite()) {
            return Err("Audio processing produced an invalid sample; recording stopped.".into());
        }
        Ok(out)
    }
}
pub type SourceBlocks = [Vec<[f32; 2]>; 2];
pub type MixedBlock = (SourceBlocks, [AudioMeter; 3]);
pub struct Mixer {
    levels: [Level; 2],
    pub effective: [MixLevel; 2],
    pub roles: u8,
    pub applied_sequence: u64,
    pub applied_sample: u64,
}
impl Mixer {
    pub fn new(config: &NativeAudioConfig) -> Self {
        Self {
            levels: [
                Level::new(config.render_level),
                Level::new(config.microphone_level),
            ],
            effective: [config.render_level, config.microphone_level],
            roles: config.role_mask(),
            applied_sequence: 0,
            applied_sample: 0,
        }
    }
    pub fn apply(&mut self, levels: &AudioLevels, sample: u64) {
        self.effective = [levels.render_level, levels.microphone_level];
        for (state, value) in self.levels.iter_mut().zip(self.effective) {
            state.set(value);
        }
        self.applied_sequence = levels.command_sequence;
        self.applied_sample = sample;
    }
    pub fn mask(&self) -> u8 {
        (u8::from(self.effective[0].muted) | (u8::from(self.effective[1].muted) << 1)) & self.roles
    }
    pub fn process(&mut self, mut blocks: [Vec<[f32; 2]>; 2]) -> Result<MixedBlock, String> {
        if blocks[0].len() != blocks[1].len() || blocks[0].len() > 480 {
            return Err("Audio block exceeded its common sample bound.".into());
        }
        for (role, block) in blocks.iter_mut().enumerate() {
            for sample in block.iter_mut() {
                *sample = if self.roles & (1 << role) == 0 {
                    [0.; 2]
                } else {
                    self.levels[role].sample(*sample)?
                };
            }
        }
        let mixed: Vec<[f32; 2]> = blocks[0]
            .iter()
            .zip(&blocks[1])
            .map(|(a, b)| [a[0] + b[0], a[1] + b[1]])
            .collect();
        if mixed.iter().flatten().any(|v| !v.is_finite()) {
            return Err("Mixed audio overflowed; reduce source volume.".into());
        }
        let meters = [
            meter(&blocks[0], self.roles & 1 != 0, self.effective[0].muted),
            meter(&blocks[1], self.roles & 2 != 0, self.effective[1].muted),
            meter(&mixed, self.roles != 0, false),
        ];
        Ok((blocks, meters))
    }
}
fn meter(samples: &[[f32; 2]], enabled: bool, muted: bool) -> AudioMeter {
    let peak = samples
        .iter()
        .flatten()
        .map(|v| v.abs())
        .fold(0f32, f32::max);
    let energy = samples
        .iter()
        .flatten()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>();
    let rms = if samples.is_empty() {
        0.
    } else {
        (energy / (samples.len() * 2) as f64)
            .sqrt()
            .min(f64::from(f32::MAX)) as f32
    };
    AudioMeter {
        state: if !enabled {
            "off"
        } else if muted {
            "muted"
        } else if peak < 0.00001 {
            "silent"
        } else {
            "active"
        }
        .into(),
        peak,
        rms,
        overload: peak > 1.,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn levels(n: u64, gain: f32, muted: bool) -> AudioLevels {
        AudioLevels {
            session_id: "session".into(),
            source_generation: "audio-1".into(),
            command_sequence: n,
            render_level: MixLevel { gain, muted },
            microphone_level: MixLevel::default(),
        }
    }
    #[test]
    fn gain_coalesces_but_never_across_a_mute_edge_and_retries_keep_receipt() {
        let mut q = ControlQueue::new(MixLevel::default(), MixLevel::default());
        q.push(levels(1, 0.5, false), 100).unwrap();
        q.push(levels(2, 0.6, false), 200).unwrap();
        assert_eq!(q.len(), 1);
        q.push(levels(3, 0.6, true), 300).unwrap();
        q.push(levels(4, 0.7, true), 400).unwrap();
        q.push(levels(5, 0.7, false), 500).unwrap();
        assert_eq!(q.len(), 4);
        assert_eq!(q.edge_count(), 2);
        assert!(!q.push(levels(3, 0.6, true), 999).unwrap());
        assert!(q.push(levels(3, 0.9, true), 999).is_err());
        let c = q.claim().unwrap();
        assert_eq!(c.received_qpc, 200);
        q.settle(c.levels.command_sequence);
        assert_eq!(q.claim().unwrap().received_qpc, 300);
    }
    #[test]
    fn pending_and_inflight_edges_keep_retry_identity_beyond_recent_window() {
        for claimed in [false, true] {
            let mut q = ControlQueue::new(MixLevel::default(), MixLevel::default());
            q.push(levels(1, 1., true), 100).unwrap();
            if claimed {
                q.claim();
            }
            for n in 2..=80 {
                q.push(levels(n, 0.5, true), n as i64).unwrap();
            }
            assert!(!q.push(levels(1, 1., true), 999).unwrap());
            assert!(q.push(levels(1, 0.8, true), 999).is_err());
            assert_eq!(q.front().unwrap().received_qpc, 100);
            assert_eq!(q.len(), 2);
        }
    }
    #[test]
    fn alternating_mutes_fail_at_queue_bound_instead_of_losing_an_edge() {
        let mut q = ControlQueue::new(MixLevel::default(), MixLevel::default());
        for n in 1..=64 {
            q.push(levels(n, 1., n % 2 == 1), n as i64).unwrap();
        }
        assert!(q.push(levels(65, 1., true), 65).is_err());
        assert_eq!(q.len(), 64);
    }
    #[test]
    fn buffered_audio_unmute_targets_receipt_time_not_old_rendered_block() {
        let zero = 10_000_000;
        assert_eq!(control_sample(zero, zero + 10_000_000), 48_000);
        assert_eq!(control_sample(zero, zero + 10_000_001), 48_480);
        assert_eq!(control_sample(zero, zero - 1), 0);
    }
    #[test]
    fn mute_zeros_stems_and_gain_ramp_meter_matches_saved_samples() {
        let c = NativeAudioConfig {
            session_id: "session".into(),
            source_generation: "audio-1".into(),
            render: NativeRender::System {
                device_id: "default".into(),
            },
            microphone: None,
            render_level: MixLevel::default(),
            microphone_level: MixLevel::default(),
        };
        let mut m = Mixer::new(&c);
        let (b, meters) = m
            .process([vec![[0.5; 2]; 480], vec![[0.; 2]; 480]])
            .unwrap();
        assert_eq!(meters[0].peak, 0.5);
        assert_eq!(meters[0].rms, 0.5);
        assert_eq!(b[0][479], [0.5; 2]);
        m.apply(&levels(1, 1., true), 480);
        let (b, meters) = m
            .process([vec![[0.5; 2]; 480], vec![[0.; 2]; 480]])
            .unwrap();
        assert!(b[0].iter().all(|x| *x == [0.; 2]));
        assert_eq!(meters[0].state, "muted");
        m.apply(&levels(2, 0.5, false), 960);
        let (b, _) = m.process([vec![[1.; 2]; 480], vec![[0.; 2]; 480]]).unwrap();
        assert!(b[0][0][0] < 0.01);
        assert!((b[0][239][0] - 0.5).abs() < 0.00001);
    }
    #[test]
    fn same_boundary_edges_are_drained_without_inventing_a_block_of_mute() {
        let mut queue = ControlQueue::new(MixLevel::default(), MixLevel::default());
        queue.push(levels(1, 1., true), 10_000).unwrap();
        queue.push(levels(2, 1., false), 10_001).unwrap();
        assert!(queue.claim_due(0, 0, None).is_none());
        let first = queue.claim_due(0, 480, None).unwrap();
        let second = queue.claim_due(0, 480, None).unwrap();
        assert!(first.levels.render_level.muted);
        assert!(!second.levels.render_level.muted);
        assert_eq!(control_sample(0, first.received_qpc), 480);
        assert_eq!(control_sample(0, second.received_qpc), 480);
        assert!(queue.claim_due(0, 480, None).is_none());
        assert_eq!(queue.len(), 2); // both remain retry protected through the one PCM write
        assert!(!queue.push(first.levels.clone(), 999_999).unwrap());
        queue.settle(2);
        assert_eq!(queue.len(), 0);
    }
    #[test]
    fn a_full_same_boundary_batch_and_stop_tail_keep_exact_order_and_cutoff() {
        let mut queue = ControlQueue::new(MixLevel::default(), MixLevel::default());
        for sequence in 1..=64 {
            queue
                .push(levels(sequence, 1., sequence % 2 == 1), 10_000)
                .unwrap();
        }
        for sequence in 1..=64 {
            assert_eq!(
                queue
                    .claim_due(0, 480, None)
                    .unwrap()
                    .levels
                    .command_sequence,
                sequence
            );
        }
        assert_eq!(queue.len(), 64);
        assert!(queue.claim_due(0, 480, None).is_none());
        assert!(!queue.push(levels(1, 1., true), 99_999).unwrap());
        queue.settle(64);
        assert_eq!(queue.len(), 0);
        let mut queue = ControlQueue::new(MixLevel::default(), MixLevel::default());
        queue.push(levels(1, 1., true), 12_000_000).unwrap(); // mute1.2s
        queue.push(levels(2, 1., false), 12_600_000).unwrap(); // afterStop1.25s
        assert!(queue.claim_due(0, 55_200, Some(60_000)).is_none());
        assert_eq!(
            queue
                .claim_due(0, 57_600, Some(60_000))
                .unwrap()
                .levels
                .command_sequence,
            1
        );
        queue.settle(1);
        assert!(queue.claim_due(0, 60_480, Some(60_000)).is_none());
        assert_eq!(queue.front().unwrap().levels.command_sequence, 2); // explicit unapplied tail
    }
}
