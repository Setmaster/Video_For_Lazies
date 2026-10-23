//! Fixed-size source clock receipts, retained in status without a history buffer.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ClockEpoch {
    pub active_sample: String,
    pub physical_qpc100ns: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PauseClock {
    pub pause_revision: u64,
    pub cutoff_sample: String,
    pub physical_cutoff_qpc100ns: String,
    pub released_qpc100ns: String,
}
fn number(value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .ok()
        .filter(|n| n.to_string() == value)
        .ok_or_else(|| "Invalid canonical recording clock value.".into())
}
fn qpc(value: &str) -> Result<u64, String> {
    number(value).and_then(|n| {
        if n > 0 && n <= i64::MAX as u64 {
            Ok(n)
        } else {
            Err("Invalid physical recording clock.".into())
        }
    })
}
impl ClockEpoch {
    pub fn read(value: Value) -> Result<Self, String> {
        let epoch: Self =
            serde_json::from_value(value).map_err(|_| "Invalid recording clock epoch.")?;
        if !number(&epoch.active_sample)?.is_multiple_of(480) {
            return Err("The recording epoch is not on its global sample boundary.".into());
        }
        qpc(&epoch.physical_qpc100ns)?;
        Ok(epoch)
    }
    pub fn resumed_after(&self, pause: &PauseClock) -> Result<(), String> {
        if self.active_sample != pause.cutoff_sample
            || qpc(&self.physical_qpc100ns)? < qpc(&pause.released_qpc100ns)?
        {
            return Err("The resumed epoch precedes its released Pause boundary.".into());
        }
        Ok(())
    }
}
impl PauseClock {
    pub fn read(value: Value, epoch: &ClockEpoch, revision: u64) -> Result<Self, String> {
        let pause: Self =
            serde_json::from_value(value).map_err(|_| "Invalid recording Pause clock.")?;
        if pause.pause_revision == 0
            || pause.pause_revision > 9_007_199_254_740_991
            || pause.pause_revision != revision
        {
            return Err("Pause clock revision changed.".into());
        }
        let n = number(&pause.cutoff_sample)?;
        let base = number(&epoch.active_sample)?;
        let physical = qpc(&pause.physical_cutoff_qpc100ns)?;
        if !n.is_multiple_of(480)
            || n < base
            || qpc(&pause.released_qpc100ns)? < physical
            || i128::from(qpc(&epoch.physical_qpc100ns)?)
                + i128::from(n - base) * 10_000_000 / 48_000
                != i128::from(physical)
        {
            return Err("Pause clock does not match the active capture epoch.".into());
        }
        Ok(pause)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_exact_cut_and_release_map_without_accepting_escaped_or_partial_fields() {
        let epoch =
            ClockEpoch::read(json!({"activeSample":"0","physicalQpc100ns":"1000000"})).unwrap();
        let valid = json!({"pauseRevision":1,"cutoffSample":"480","physicalCutoffQpc100ns":"1100000","releasedQpc100ns":"2200000"});
        let pause = PauseClock::read(valid.clone(), &epoch, 1).unwrap();
        let resumed =
            ClockEpoch::read(json!({"activeSample":"480","physicalQpc100ns":"5000000"})).unwrap();
        resumed.resumed_after(&pause).unwrap();
        for (field, value) in [
            ("cutoffSample", json!("0480")),
            ("physicalCutoffQpc100ns", json!("1100001")),
            ("releasedQpc100ns", json!("1099999")),
            ("pauseRevision", json!(2)),
        ] {
            let mut bad = valid.clone();
            bad[field] = value;
            assert!(PauseClock::read(bad, &epoch, 1).is_err());
        }
        assert!(ClockEpoch::read(json!({"activeSample":"480","physicalQpc100ns":"-1"})).is_err());
        assert!(
            ClockEpoch::read(json!({"activeSample":"481","physicalQpc100ns":"5000000"})).is_err()
        );
        assert!(
            ClockEpoch::read(json!({"activeSample":"0","physicalQpc100ns":"1000000","extra":1}))
                .is_err()
        );
    }
}
