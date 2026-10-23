use crate::recording::{RecordingConfig, Source, audio_types::RecordingAudioConfig};
use serde::{Deserialize, Serialize};

pub(crate) const MAX_REVISION: u64 = 9_007_199_254_740_991;
pub(crate) const LEASE_MS: u64 = 3_000;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Action {
    Start,
    Pause,
    Resume,
    Stop,
}
impl Action {
    pub(crate) const ALL: [Self; 4] = [Self::Start, Self::Pause, Self::Resume, Self::Stop];
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::enum_variant_names)] // Every supported chord explicitly includes Ctrl.
pub(crate) enum Modifiers {
    CtrlShift,
    CtrlAlt,
    CtrlShiftAlt,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Chord {
    pub modifiers: Modifiers,
    pub key: String,
}
impl Chord {
    pub(crate) fn virtual_key(&self) -> Result<u32, String> {
        if self.key.len() == 1 {
            let b = self.key.as_bytes()[0];
            if b.is_ascii_uppercase() || b.is_ascii_digit() {
                return Ok(u32::from(b));
            }
        }
        if let Some(s) = self.key.strip_prefix('F')
            && let Ok(n) = s.parse::<u32>()
            && (1..=24).contains(&n)
            && n != 12
            && s == n.to_string()
        {
            return Ok(0x70 + n - 1);
        }
        Err("Choose A-Z, 0-9 or F1-F24, except reserved F12.".into())
    }
    pub(crate) fn modifier_bits(&self) -> u32 {
        match self.modifiers {
            Modifiers::CtrlShift => 6,
            Modifiers::CtrlAlt => 3,
            Modifiers::CtrlShiftAlt => 7,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.virtual_key().map(|_| ())
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Bindings {
    pub start: Option<Chord>,
    pub pause: Option<Chord>,
    pub resume: Option<Chord>,
    pub stop: Option<Chord>,
}
impl Bindings {
    pub(crate) fn get(&self, action: Action) -> Option<&Chord> {
        match action {
            Action::Start => self.start.as_ref(),
            Action::Pause => self.pause.as_ref(),
            Action::Resume => self.resume.as_ref(),
            Action::Stop => self.stop.as_ref(),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        let mut seen = Vec::new();
        for action in Action::ALL {
            if let Some(chord) = self.get(action) {
                chord.validate()?;
                if seen.contains(chord) {
                    return Err("Each action needs a different shortcut.".into());
                }
                seen.push(chord.clone());
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn empty(&self) -> bool {
        Action::ALL.iter().all(|a| self.get(*a).is_none())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartDraft {
    pub source: Source,
    pub fps: u32,
    pub quality: String,
    pub countdown_seconds: u32,
    pub show_cursor: bool,
    #[serde(default)]
    pub audio: Option<RecordingAudioConfig>,
    pub output_directory: String,
}
impl StartDraft {
    pub(crate) fn config(self, request_id: String) -> RecordingConfig {
        RecordingConfig {
            request_id,
            source: self.source,
            fps: self.fps,
            quality: self.quality,
            countdown_seconds: self.countdown_seconds,
            show_cursor: self.show_cursor,
            audio: self.audio,
            system_audio_device_id: None,
            microphone_device_id: None,
            output_directory: self.output_directory,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "operation",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum ControlsRequest {
    Snapshot,
    AttachOwner {
        expected_revision: u64,
        expected_owner_token: Option<String>,
        renderer_instance_id: String,
    },
    BeginAssignment {
        expected_revision: u64,
        owner_token: String,
        operation_id: String,
    },
    RenewAssignment {
        owner_token: String,
        assignment_token: String,
    },
    ApplyBindings {
        expected_revision: u64,
        owner_token: String,
        assignment_token: String,
        operation_id: String,
        capture_closed: bool,
        bindings: Bindings,
    },
    EndAssignment {
        expected_revision: u64,
        owner_token: String,
        assignment_token: String,
        capture_closed: bool,
    },
    Reset {
        expected_revision: u64,
        owner_token: String,
        operation_id: String,
    },
    PublishDraft {
        owner_token: String,
        invalidation_epoch: u64,
        minimize_cycle: u64,
        expected_arm_token: Option<String>,
        client_revision: u64,
        config_revision: u64,
        draft: StartDraft,
    },
    RenewDraft {
        owner_token: String,
        arm_token: String,
        config_revision: u64,
    },
    InvalidateDraft {
        owner_token: String,
        client_revision: u64,
    },
    ClassifyVisibility {
        owner_token: String,
    },
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct KeyCapture {
    pub key: String,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RowStatus {
    Unset,
    Registered,
    Suppressed,
    Conflict,
    Unavailable,
    CleanupFailed,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BindingRow {
    pub configured: Option<Chord>,
    pub registered: Option<Chord>,
    pub dispatch_enabled: bool,
    pub status: RowStatus,
    pub reason: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Rows {
    pub start: BindingRow,
    pub pause: BindingRow,
    pub resume: BindingRow,
    pub stop: BindingRow,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssignmentSnapshot {
    pub token: String,
    pub expires_in_ms: u64,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ArmMode {
    Leased,
    Retained,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ArmSnapshot {
    pub token: String,
    pub config_revision: u64,
    pub mode: ArmMode,
    pub expires_in_ms: Option<u64>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) enum NativeWindow {
    Visible,
    Minimized,
    Hidden,
    Unavailable,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ControlsSnapshot {
    pub revision: u64,
    pub owner_token: Option<String>,
    pub invalidation_epoch: u64,
    pub minimize_cycle: u64,
    pub bindings: Bindings,
    pub rows: Rows,
    pub assignment: Option<AssignmentSnapshot>,
    pub arm: Option<ArmSnapshot>,
    pub native_window: NativeWindow,
    pub message: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ControlsFailure {
    pub code: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum ControlsReply {
    Ok {
        ok: bool,
        value: ControlsSnapshot,
    },
    Err {
        ok: bool,
        error: ControlsFailure,
        state: ControlsSnapshot,
    },
}
pub(crate) fn valid_token(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn bounded_message(s: impl AsRef<str>) -> String {
    s.as_ref().chars().take(256).collect()
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UiStartContext {
    pub owner_token: String,
    pub invalidation_epoch: u64,
    pub minimize_cycle: u64,
}
