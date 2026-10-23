//! A continuous 48 kHz media clock made from active physical QPC intervals.
//! Pause boundaries are global 480-sample boundaries, never rounded per epoch.
pub const RATE: u64 = 48_000;
pub const QUANTUM: u64 = 480;
const TICKS: i128 = 10_000_000;

#[derive(Clone, Debug)]
pub struct ActiveClock {
    base_sample: u64,
    epoch: Option<i64>,
}
impl ActiveClock {
    pub fn new(first_qpc: i64) -> Self {
        Self {
            base_sample: 0,
            epoch: Some(first_qpc),
        }
    }
    pub fn sample_at(&self, qpc: i64) -> u64 {
        self.base_sample
            .saturating_add(self.epoch.map_or(0, |zero| {
                ((i128::from(qpc) - i128::from(zero)).max(0) * i128::from(RATE) / TICKS)
                    .min(i128::from(u64::MAX)) as u64
            }))
    }
    pub fn pause_boundary(&self, qpc: i64) -> Result<u64, String> {
        let zero = self.epoch.ok_or("The media clock is paused.")?;
        let elapsed = (i128::from(qpc) - i128::from(zero)).max(0);
        let quantum_ticks = TICKS * i128::from(QUANTUM) / i128::from(RATE);
        let quanta = (elapsed + quantum_ticks - 1) / quantum_ticks;
        u64::try_from(i128::from(self.base_sample) + quanta * i128::from(QUANTUM))
            .map_err(|_| "Recording clock exhausted its safe range.".into())
    }
    pub fn physical_at(&self, sample: u64) -> Result<i64, String> {
        let zero = self.epoch.ok_or("The media clock is paused.")?;
        let delta = sample
            .checked_sub(self.base_sample)
            .ok_or("Sample precedes this capture epoch.")?;
        i64::try_from(i128::from(zero) + i128::from(delta) * TICKS / i128::from(RATE))
            .map_err(|_| "Recording clock exhausted its safe range.".into())
    }
    pub fn freeze(&mut self, sample: u64) -> Result<(), String> {
        if self.epoch.is_none() || sample < self.base_sample || !sample.is_multiple_of(QUANTUM) {
            return Err("Invalid active-clock Pause boundary.".into());
        }
        self.base_sample = sample;
        self.epoch = None;
        Ok(())
    }
    pub fn resume(&mut self, first_qpc: i64) -> Result<(), String> {
        if self.epoch.is_some() {
            return Err("The media clock is already active.".into());
        }
        self.epoch = Some(first_qpc);
        Ok(())
    }
    /// Virtual physical zero for existing interval-aware audio rendering.
    pub fn render_zero(&self) -> Result<i64, String> {
        let zero = self.epoch.ok_or("The media clock is paused.")?;
        i64::try_from(i128::from(zero) - i128::from(self.base_sample) * TICKS / i128::from(RATE))
            .map_err(|_| "Recording clock exhausted its safe range.".into())
    }
    pub fn media_ticks(sample: u64) -> i64 {
        (i128::from(sample) * TICKS / i128::from(RATE)).min(i128::from(i64::MAX)) as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn many_pauses_preserve_global_samples_and_exclude_wall_time() {
        let mut c = ActiveClock::new(10_000_000);
        let mut wall = 10_000_000;
        for cycle in 0..1000 {
            let cut = c.pause_boundary(wall + 100_001).unwrap();
            assert_eq!(cut, (cycle + 1) * 960);
            let physical = c.physical_at(cut).unwrap();
            assert_eq!(c.sample_at(physical), cut);
            c.freeze(cut).unwrap();
            wall = physical + 1_234_567_890;
            assert_eq!(c.sample_at(wall), cut);
            c.resume(wall).unwrap();
            assert_eq!(c.physical_at(cut).unwrap(), wall);
            assert_eq!(c.render_zero().unwrap() + SelfClock::ticks(cut), wall);
        }
    }
    struct SelfClock;
    impl SelfClock {
        fn ticks(n: u64) -> i64 {
            ActiveClock::media_ticks(n)
        }
    }
    #[test]
    fn invalid_transitions_and_old_samples_fail_closed() {
        let mut c = ActiveClock::new(100);
        assert!(c.resume(200).is_err());
        assert!(c.freeze(1).is_err());
        c.freeze(480).unwrap();
        assert!(c.physical_at(480).is_err());
        assert!(c.freeze(480).is_err());
        c.resume(10_000_000).unwrap();
        assert!(c.physical_at(479).is_err());
    }
    #[test]
    fn every_pre_barrier_receipt_targets_at_or_before_the_ceil_cut() {
        // Native receipt and capture-loop barrier use the same 100ns QPC
        // origin. Closing admission precedes the fresh barrier timestamp.
        let epoch = 17_654_321;
        let clock = ActiveClock::new(epoch);
        for elapsed in [1, 99_999, 100_000, 100_001, 9_999_999, 10_000_001] {
            let cut = clock.pause_boundary(epoch + elapsed).unwrap();
            for lag in [0, 1, 99_999] {
                let receipt = (epoch + elapsed - lag).max(epoch);
                let samples =
                    ((i128::from(receipt - epoch) * 48_000 + 9_999_999) / 10_000_000) as u64;
                let target = samples.div_ceil(480) * 480;
                assert!(target <= cut);
            }
        }
    }
}
