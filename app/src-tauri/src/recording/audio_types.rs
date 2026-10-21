//! Shared audio wire descriptions. Enumeration and validation never activate capture.
use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 48_000;
pub const MIXED_TITLE: &str = "Mixed";
pub const SYSTEM_TITLE: &str = "System audio";
pub const APPLICATION_TITLE: &str = "Application audio";
pub const MICROPHONE_TITLE: &str = "Microphone";
pub const CONTROL_BYTES: usize = 512;
pub const STATUS_BYTES: usize = 65_536;
pub const CONTROL_CAPACITY: usize = 64;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RenderAudio {
    #[default]
    Off,
    System {
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    Application {
        #[serde(rename = "applicationId")]
        application_id: String,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MixLevel {
    pub gain: f32,
    pub muted: bool,
}
impl Default for MixLevel {
    fn default() -> Self {
        Self {
            gain: 1.0,
            muted: false,
        }
    }
}
impl MixLevel {
    pub fn validate(self) -> Result<(), String> {
        if self.gain.is_finite() && (0.0..=2.0).contains(&self.gain) {
            Ok(())
        } else {
            Err("Audio volume must be between 0 and 200 percent.".into())
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Suppression {
    #[default]
    Off,
    Save,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Microphone {
    pub device_id: String,
    pub suppression: Suppression,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingAudioConfig {
    pub render: RenderAudio,
    pub microphone: Option<Microphone>,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
}
pub fn bounded_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn device_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 2048 && !id.contains('\0')
}
impl RecordingAudioConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.render_level.validate()?;
        self.microphone_level.validate()?;
        let valid = match &self.render {
            RenderAudio::Off => true,
            RenderAudio::System { device_id: id } => device_id(id),
            RenderAudio::Application { application_id } => bounded_id(application_id),
        };
        if !valid
            || self
                .microphone
                .as_ref()
                .is_some_and(|m| !device_id(&m.device_id))
        {
            return Err("Choose an available audio source.".into());
        }
        Ok(())
    }
    pub fn render_enabled(&self) -> bool {
        !matches!(self.render, RenderAudio::Off)
    }
    pub fn enabled(&self) -> bool {
        self.render_enabled() || self.microphone.is_some()
    }
    pub fn role_mask(&self) -> u8 {
        u8::from(self.render_enabled()) | (u8::from(self.microphone.is_some()) << 1)
    }
    pub fn mute_mask(&self) -> u8 {
        (u8::from(self.render_level.muted) | (u8::from(self.microphone_level.muted) << 1))
            & self.role_mask()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pub process_id: u32,
    pub process_started: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioApplicationInfo {
    pub identity: ProcessIdentity,
    pub name: String,
    pub process_name: String,
    pub available: bool,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioApplicationListing {
    pub applications: Vec<AudioApplicationInfo>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum NativeRender {
    #[default]
    Off,
    System {
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    Application {
        identity: ProcessIdentity,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAudioConfig {
    pub session_id: String,
    pub source_generation: String,
    pub render: NativeRender,
    pub microphone: Option<Microphone>,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
}
impl NativeAudioConfig {
    pub fn role_mask(&self) -> u8 {
        u8::from(!matches!(self.render, NativeRender::Off))
            | (u8::from(self.microphone.is_some()) << 1)
    }
    pub fn mute_mask(&self) -> u8 {
        (u8::from(self.render_level.muted) | (u8::from(self.microphone_level.muted) << 1))
            & self.role_mask()
    }
    pub fn validate(&self) -> Result<(), String> {
        if !bounded_id(&self.session_id) || !bounded_id(&self.source_generation) {
            return Err("Invalid audio session.".into());
        }
        self.render_level.validate()?;
        self.microphone_level.validate()?;
        if self
            .microphone
            .as_ref()
            .is_some_and(|m| !device_id(&m.device_id))
        {
            return Err("Invalid microphone source.".into());
        }
        match &self.render {
            NativeRender::System { device_id: id } if !device_id(id) => {
                Err("Invalid system audio source.".into())
            }
            NativeRender::Application { identity }
                if identity.process_id == 0
                    || identity.process_started.len() != 16
                    || u64::from_str_radix(&identity.process_started, 16)
                        .ok()
                        .is_none_or(|v| {
                            v == 0 || format!("{v:016x}") != identity.process_started
                        }) =>
            {
                Err("Invalid application identity.".into())
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioLevels {
    pub session_id: String,
    pub source_generation: String,
    pub command_sequence: u64,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
}
impl AudioLevels {
    pub fn validate(&self) -> Result<(), String> {
        if !bounded_id(&self.session_id)
            || !bounded_id(&self.source_generation)
            || self.command_sequence == 0
            || self.command_sequence > 9_007_199_254_740_991
        {
            return Err("Invalid audio control identity.".into());
        }
        self.render_level.validate()?;
        self.microphone_level.validate()?;
        if serde_json::to_vec(&serde_json::json!({"operation":"audio-levels","value":self}))
            .map_err(|e| e.to_string())?
            .len()
            + 1
            > CONTROL_BYTES
        {
            return Err("Audio control exceeds its byte limit.".into());
        }
        Ok(())
    }
    pub fn same_mutes(&self, other: &Self) -> bool {
        self.render_level.muted == other.render_level.muted
            && self.microphone_level.muted == other.microphone_level.muted
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioControlSubmission {
    pub session_id: String,
    pub source_generation: String,
    pub accepted_command_sequence: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMeter {
    pub state: String,
    pub peak: f32,
    pub rms: f32,
    pub overload: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    pub session_id: String,
    pub source_generation: String,
    pub sequence: u64,
    pub accepted_command_sequence: u64,
    pub applied_command_sequence: u64,
    pub applied_at_sample: String,
    pub rejected_command_sequence: Option<u64>,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
    pub render: AudioMeter,
    pub microphone: AudioMeter,
    pub mixed: AudioMeter,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_enforce_real_serialized_bound_and_finite_gains() {
        let mut c = AudioLevels {
            session_id: "a".repeat(80),
            source_generation: "b".repeat(80),
            command_sequence: 9_007_199_254_740_991,
            render_level: MixLevel::default(),
            microphone_level: MixLevel::default(),
        };
        assert!(c.validate().is_ok());
        c.render_level.gain = f32::NAN;
        assert!(c.validate().is_err());
        c.render_level.gain = 2.01;
        assert!(c.validate().is_err());
        c.render_level.gain = 1.;
        c.session_id.push('a');
        assert!(c.validate().is_err());
    }
}
