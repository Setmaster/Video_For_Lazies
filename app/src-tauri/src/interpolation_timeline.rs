//! Integer presentation-time scheduling. No inference code owns the clock.

const NANOS: u128 = 1_000_000_000;
pub(crate) const MAX_OUTPUT_FRAMES: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rate {
    pub numerator: u64,
    pub denominator: u64,
}

impl Rate {
    pub fn for_playback(target_fps: u32, speed: f64) -> Result<Self, String> {
        if !matches!(target_fps, 60 | 120) || !speed.is_finite() || speed <= 0.0 {
            return Err(
                "AI interpolation requires 60 or 120 fps and a valid playback speed.".into(),
            );
        }
        // Match the existing video/audio filter's nine-decimal speed contract.
        let speed_units = (speed * 1_000_000_000.0).round() as u64;
        if speed_units == 0 {
            return Err("Playback speed is too small for interpolation.".into());
        }
        let numerator = u64::from(target_fps) * 1_000_000_000;
        let divisor = gcd(numerator, speed_units);
        let rate = Self {
            numerator: numerator / divisor,
            denominator: speed_units / divisor,
        };
        if rate.numerator > i32::MAX as u64 || rate.denominator > i32::MAX as u64 {
            return Err("Use a simple decimal playback speed for AI interpolation (for example 0.5, 1.25 or 2).".into());
        }
        Ok(rate)
    }

    pub fn frame_count(self, duration_ns: u64) -> Result<u64, String> {
        let divisor = NANOS * u128::from(self.denominator);
        let count = (u128::from(duration_ns) * u128::from(self.numerator)).div_ceil(divisor);
        let count =
            u64::try_from(count).map_err(|_| "Interpolation frame count overflow.".to_string())?;
        if count == 0 || count > MAX_OUTPUT_FRAMES {
            return Err("Interpolation is limited to one million output pictures per clip. Trim the clip or choose a lower frame rate.".into());
        }
        Ok(count)
    }

    pub fn sample_before(self, frame: u64, timestamp_ns: u64) -> bool {
        u128::from(frame) * NANOS * u128::from(self.denominator)
            < u128::from(timestamp_ns) * u128::from(self.numerator)
    }

    pub fn alpha(self, frame: u64, left_ns: u64, right_ns: u64) -> Result<f32, String> {
        if right_ns <= left_ns {
            return Err("Decoded video timestamps are not strictly increasing.".into());
        }
        let sample = u128::from(frame) * NANOS * u128::from(self.denominator);
        let left = u128::from(left_ns) * u128::from(self.numerator);
        let span = u128::from(right_ns - left_ns) * u128::from(self.numerator);
        if sample <= left {
            return Ok(0.0);
        }
        if sample >= left + span {
            return Ok(1.0);
        }
        Ok(((sample - left) as f64 / span as f64) as f32)
    }

    pub fn ffmpeg(self) -> String {
        format!("{}/{}", self.numerator, self.denominator)
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

/// Histograms distinguish hard image changes from a small moving object.
/// This conservative guard may also hold flashes; it never changes cadence.
pub(crate) fn is_scene_cut(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() || left.is_empty() {
        return true;
    }
    let step = (left.len() / (4096 * 3)).max(1) * 3;
    let mut difference = 0u64;
    let mut count = 0u64;
    let mut a = [0u64; 48];
    let mut b = [0u64; 48];
    for offset in (0..left.len().saturating_sub(2)).step_by(step) {
        for channel in 0..3 {
            let l = left[offset + channel];
            let r = right[offset + channel];
            difference += u64::from(l.abs_diff(r));
            count += 1;
            a[channel * 16 + usize::from(l) / 16] += 1;
            b[channel * 16 + usize::from(r) / 16] += 1;
        }
    }
    if count == 0 {
        return false;
    }
    let histogram: u64 = a.iter().zip(b.iter()).map(|(x, y)| x.abs_diff(*y)).sum();
    difference * 100 >= count * 255 * 25 && histogram * 100 >= count * 2 * 30
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_integer_target_and_fractional_source_alpha() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        assert_eq!(rate.frame_count(1_001_000_000).unwrap(), 61);
        // Source anchor at 1001/24000 seconds, expressed at nanosecond precision.
        let a = rate.alpha(1, 0, 41_708_333).unwrap();
        assert!((a - 0.3996004).abs() < 0.000001);
        assert_eq!(rate.alpha(0, 0, 41_708_333).unwrap(), 0.0);
    }
    #[test]
    fn vfr_positions_and_tail_count_do_not_use_frame_index() {
        let rate = Rate::for_playback(120, 1.0).unwrap();
        assert!((rate.alpha(6, 20_000_000, 80_000_000).unwrap() - 0.5).abs() < f32::EPSILON);
        assert_eq!(rate.frame_count(1_000_000_000).unwrap(), 120);
        assert!(rate.alpha(1, 10, 10).is_err());
    }
    #[test]
    fn speed_changes_prepared_cadence_not_output_duration_by_multiplier() {
        let rate = Rate::for_playback(60, 2.0).unwrap();
        assert_eq!(rate.ffmpeg(), "30/1");
        assert_eq!(rate.frame_count(1_000_000_000).unwrap(), 30);
        assert_eq!(Rate::for_playback(120, 0.5).unwrap().ffmpeg(), "240/1");
    }
    #[test]
    fn cut_and_static_pairs_are_distinct() {
        let black = vec![0; 64 * 64 * 3];
        let white = vec![255; black.len()];
        assert!(is_scene_cut(&black, &white));
        assert!(!is_scene_cut(&black, &black));
        let mut motion = black.clone();
        motion[..300].fill(255);
        assert!(!is_scene_cut(&black, &motion));
    }
    #[test]
    fn resource_count_is_bounded_and_boundaries_have_single_ownership() {
        let rate = Rate::for_playback(60, 1.0).unwrap();
        assert!(rate.frame_count(u64::MAX).is_err());
        assert!(!rate.sample_before(1, 16_666_666));
        assert!(rate.sample_before(1, 16_666_667));
        assert_eq!(rate.alpha(1, 16_666_667, 30_000_000).unwrap(), 0.0);
    }
}
