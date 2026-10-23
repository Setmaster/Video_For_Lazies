//! Bounded, renderer-owned audio intent identities and reconciliation payloads.
use super::audio_types::{AudioLevels, AudioStatus, MixLevel, bounded_id};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_SAFE: u64 = 9_007_199_254_740_991;
pub const REQUEST_BYTES: usize = 24_576;
pub const RESULT_BYTES: usize = 65_536;
pub const CACHE_BYTES: usize = 1024 * 1024;
pub const PENDING_LIMIT: usize = 64;
pub const SETTLED_LIMIT: usize = 64;
pub const BATCH_HISTORY_LIMIT: usize = 8;
pub const OPERATION_HISTORY_LIMIT: usize = 16;
pub const HISTORY_MS: u64 = 120_000;
pub const RECONCILE_MS: u64 = 5_000;
pub const STAGE_MS: u64 = 2_000;

pub fn token(value: &str) -> bool {
    hex(value, 32)
}
pub fn digest(value: &str) -> bool {
    hex(value, 64)
}
fn hex(value: &str, count: usize) -> bool {
    value.len() == count
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn sample(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|n| n.to_string() == value)
}
pub fn hash<T: Serialize>(value: &T) -> Result<String, FailureCode> {
    let bytes = serde_json::to_vec(value).map_err(|_| FailureCode::InvalidRequest)?;
    Ok(Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn checked_bytes<T: Serialize>(value: &T, limit: usize) -> Result<usize, FailureCode> {
    let size = serde_json::to_vec(value)
        .map_err(|_| FailureCode::InvalidRequest)?
        .len();
    if size > limit {
        Err(FailureCode::OversizedRequest)
    } else {
        Ok(size)
    }
}
fn normalize(level: &mut MixLevel) -> Result<(), FailureCode> {
    level.validate().map_err(|_| FailureCode::InvalidRequest)?;
    if level.gain == 0. {
        level.gain = 0.;
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LevelIntent {
    pub command_sequence: u64,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
}
impl LevelIntent {
    pub fn normalize(&mut self) -> Result<(), FailureCode> {
        if self.command_sequence == 0 || self.command_sequence > MAX_SAFE {
            return Err(FailureCode::InvalidRequest);
        }
        normalize(&mut self.render_level)?;
        normalize(&mut self.microphone_level)
    }
    pub fn levels(&self, session: &str, generation: &str) -> AudioLevels {
        AudioLevels {
            session_id: session.into(),
            source_generation: generation.into(),
            command_sequence: self.command_sequence,
            render_level: self.render_level,
            microphone_level: self.microphone_level,
        }
    }
    pub fn same_mutes(&self, other: &Self) -> bool {
        self.render_level.muted == other.render_level.muted
            && self.microphone_level.muted == other.microphone_level.muted
    }
}
impl From<&AudioLevels> for LevelIntent {
    fn from(value: &AudioLevels) -> Self {
        Self {
            command_sequence: value.command_sequence,
            render_level: value.render_level,
            microphone_level: value.microphone_level,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Placement {
    AwaitingReceipt,
    Active,
    ResumeBound,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptedIntent {
    #[serde(flatten)]
    pub intent: LevelIntent,
    pub target_sample: Option<String>,
    pub placement: Placement,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioReceipt {
    pub command_sequence: u64,
    pub target_sample: String,
    pub placement: Placement,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AudioLedger {
    pub pending: Vec<AcceptedIntent>,
    pub terminal_unapplied: Vec<LevelIntent>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LevelDisposition {
    Settled {
        #[serde(rename = "commandSequence")]
        command_sequence: u64,
        #[serde(rename = "identityVerified")]
        identity_verified: bool,
        #[serde(rename = "appliedAsRequested")]
        applied_as_requested: Option<bool>,
    },
    AcceptedPending {
        #[serde(rename = "commandSequence")]
        command_sequence: u64,
        #[serde(rename = "targetSample")]
        target_sample: Option<String>,
        placement: Placement,
    },
    DeferredNotAccepted {
        #[serde(rename = "commandSequence")]
        command_sequence: u64,
        reason: DeferredReason,
        #[serde(rename = "retryAfterRevision")]
        retry_after_revision: u64,
    },
}
impl LevelDisposition {
    pub fn settled(sequence: u64) -> Self {
        Self::Settled {
            command_sequence: sequence,
            identity_verified: true,
            applied_as_requested: None,
        }
    }
    pub fn pending(value: &AcceptedIntent) -> Self {
        Self::AcceptedPending {
            command_sequence: value.intent.command_sequence,
            target_sample: value.target_sample.clone(),
            placement: value.placement,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeferredReason {
    Paused,
    Transitioning,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FenceState {
    pub state: FencePhase,
    pub token: Option<String>,
    pub replacement_review_token: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FencePhase {
    Waiting,
    Ready,
    ReplacementReviewRequired,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PauseAudio {
    pub source_generation: String,
    pub sealed_accepted_sequence: u64,
    pub fence: FenceState,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PauseSeal {
    pub pause_revision: u64,
    pub cutoff_sample: String,
    pub audio: Option<PauseAudio>,
}
#[derive(Clone, Debug, Default)]
pub struct ControlSnapshot {
    pub pause_audio: Option<PauseAudio>,
    pub ledger: Option<AudioLedger>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LevelRequest {
    pub owner_token: String,
    pub expected_control_revision: u64,
    pub session_id: String,
    pub source_generation: String,
    pub command_sequence: u64,
    pub render_level: MixLevel,
    pub microphone_level: MixLevel,
}
impl LevelRequest {
    pub fn normalized(mut self) -> Result<Self, FailureCode> {
        if !token(&self.owner_token)
            || !bounded_id(&self.session_id)
            || !bounded_id(&self.source_generation)
            || self.expected_control_revision == 0
            || self.expected_control_revision > MAX_SAFE
        {
            return Err(FailureCode::InvalidRequest);
        }
        let mut intent = self.intent();
        intent.normalize()?;
        self.render_level = intent.render_level;
        self.microphone_level = intent.microphone_level;
        checked_bytes(&self, REQUEST_BYTES)?;
        Ok(self)
    }
    pub fn intent(&self) -> LevelIntent {
        LevelIntent {
            command_sequence: self.command_sequence,
            render_level: self.render_level,
            microphone_level: self.microphone_level,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LevelReply {
    pub owner_token: String,
    pub session_id: String,
    pub source_generation: String,
    pub control_revision: u64,
    pub disposition: LevelDisposition,
    pub audio_status: AudioStatus,
    pub ledger: AudioLedger,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ReconcileMode {
    RetainedQueue,
    AuthoritativeOnly,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileRequest {
    pub owner_token: String,
    pub session_id: String,
    pub source_generation: String,
    pub pause_revision: u64,
    pub batch_id: String,
    pub commands: Vec<LevelIntent>,
    pub mode: ReconcileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_review_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledge_lost_local_intent: Option<bool>,
}
impl ReconcileRequest {
    pub fn normalized(mut self) -> Result<Self, FailureCode> {
        if !token(&self.owner_token)
            || !token(&self.batch_id)
            || !bounded_id(&self.session_id)
            || !bounded_id(&self.source_generation)
            || self.pause_revision == 0
            || self.pause_revision > MAX_SAFE
            || self.commands.len() > PENDING_LIMIT
        {
            return Err(FailureCode::InvalidRequest);
        }
        if self
            .commands
            .windows(2)
            .any(|v| v[0].command_sequence >= v[1].command_sequence)
        {
            return Err(FailureCode::InvalidRequest);
        }
        for command in &mut self.commands {
            command.normalize()?;
        }
        match self.mode {
            ReconcileMode::RetainedQueue
                if self.replacement_review_token.is_some()
                    || self.acknowledge_lost_local_intent.is_some() =>
            {
                return Err(FailureCode::InvalidRequest);
            }
            ReconcileMode::AuthoritativeOnly
                if !self.commands.is_empty()
                    || !self.replacement_review_token.as_deref().is_some_and(token)
                    || self.acknowledge_lost_local_intent != Some(true) =>
            {
                return Err(FailureCode::InvalidRequest);
            }
            _ => {}
        }
        checked_bytes(&self, REQUEST_BYTES)?;
        Ok(self)
    }
    pub fn batch_digest(&self) -> Result<String, FailureCode> {
        hash(self)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileRetryRequest {
    #[serde(flatten)]
    pub request: ReconcileRequest,
    pub operation_id: String,
    pub expected_timed_out_attempt: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetryFields {
    owner_token: String,
    session_id: String,
    source_generation: String,
    pause_revision: u64,
    batch_id: String,
    commands: Vec<LevelIntent>,
    mode: ReconcileMode,
    #[serde(default)]
    replacement_review_token: Option<String>,
    #[serde(default)]
    acknowledge_lost_local_intent: Option<bool>,
    operation_id: String,
    expected_timed_out_attempt: u64,
}
impl<'de> Deserialize<'de> for ReconcileRetryRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let f = RetryFields::deserialize(deserializer)?;
        Ok(Self {
            request: ReconcileRequest {
                owner_token: f.owner_token,
                session_id: f.session_id,
                source_generation: f.source_generation,
                pause_revision: f.pause_revision,
                batch_id: f.batch_id,
                commands: f.commands,
                mode: f.mode,
                replacement_review_token: f.replacement_review_token,
                acknowledge_lost_local_intent: f.acknowledge_lost_local_intent,
            },
            operation_id: f.operation_id,
            expected_timed_out_attempt: f.expected_timed_out_attempt,
        })
    }
}
impl ReconcileRetryRequest {
    pub fn normalized(mut self) -> Result<Self, FailureCode> {
        self.request = self.request.normalized()?;
        if !token(&self.operation_id)
            || self.expected_timed_out_attempt == 0
            || self.expected_timed_out_attempt > MAX_SAFE
        {
            return Err(FailureCode::InvalidRequest);
        }
        checked_bytes(&self, REQUEST_BYTES)?;
        Ok(self)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileReply {
    pub owner_token: String,
    pub session_id: String,
    pub source_generation: String,
    pub pause_revision: u64,
    pub batch_id: String,
    pub batch_digest: String,
    pub fence_token: String,
    pub dispositions: Vec<LevelDisposition>,
    pub audio_status: AudioStatus,
    pub ledger: AudioLedger,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FailureCode {
    InvalidRequest,
    OversizedRequest,
    StaleOwner,
    StaleSession,
    SourceChanged,
    StaleAdmission,
    IdentityConflict,
    StaleUnknown,
    BatchConflict,
    StaleAttempt,
    BatchBusy,
    TransitionBusy,
    Capacity,
    EnvelopeCapacity,
    ReplacementReviewRequired,
    ReconciliationTimeout,
    StageExpired,
    ResumeTimeout,
    TransitionTimeout,
    AcquisitionFailed,
    SourceLost,
    ProtocolFailure,
    SourceUnavailable,
    CounterExhausted,
    SessionTerminal,
    AudioControlsOff,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Retry {
    SameExactToken,
    FreshRequest,
    Never,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FailureAction {
    RefreshState,
    RetrySynchronization,
    KeepPaused,
    StopAndSave,
    RestartApp,
}
impl FailureCode {
    pub fn policy(self) -> (Retry, FailureAction, &'static str) {
        use FailureAction as A;
        use FailureCode as C;
        use Retry as R;
        match self {
            C::InvalidRequest | C::OversizedRequest => (
                R::Never,
                A::RefreshState,
                "The audio request is invalid or exceeds its bound.",
            ),
            C::StaleOwner | C::StaleSession | C::SourceChanged | C::StaleAdmission => (
                R::FreshRequest,
                A::RefreshState,
                "Refresh the current recording context before another action.",
            ),
            C::IdentityConflict | C::StaleUnknown | C::BatchConflict | C::StaleAttempt => (
                R::Never,
                A::KeepPaused,
                "The original audio identity cannot be replaced or replayed.",
            ),
            C::BatchBusy | C::TransitionBusy => (
                R::SameExactToken,
                A::RefreshState,
                "Only the exact current operation may join its original attempt.",
            ),
            C::Capacity | C::EnvelopeCapacity => (
                R::Never,
                A::StopAndSave,
                "Audio control capacity is exhausted; stop and save the retained recording.",
            ),
            C::ReplacementReviewRequired => (
                R::FreshRequest,
                A::RefreshState,
                "Review restored recording audio and acknowledge lost local changes.",
            ),
            C::ReconciliationTimeout | C::StageExpired => (
                R::SameExactToken,
                A::RetrySynchronization,
                "Synchronization is parked; use an explicit synchronization retry.",
            ),
            C::ResumeTimeout => (
                R::FreshRequest,
                A::KeepPaused,
                "Resume expired without opening inputs; a fresh explicit action is required.",
            ),
            C::TransitionTimeout | C::AcquisitionFailed | C::SourceLost | C::ProtocolFailure => (
                R::Never,
                A::StopAndSave,
                "Recording control failed; stop and save the retained prefix.",
            ),
            C::SourceUnavailable => (
                R::FreshRequest,
                A::KeepPaused,
                "The selected source is temporarily unavailable; remain paused.",
            ),
            C::CounterExhausted => (
                R::Never,
                A::RestartApp,
                "Stop and save, then restart before further recording controls.",
            ),
            C::SessionTerminal => (
                R::Never,
                A::RefreshState,
                "This recording is terminal; its unapplied audio ledger is retained.",
            ),
            C::AudioControlsOff => (
                R::Never,
                A::RefreshState,
                "This recording has no enabled audio sources.",
            ),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureDetail {
    pub code: FailureCode,
    pub retry: Retry,
    pub action: FailureAction,
    pub command_sequence: Option<u64>,
    pub message: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureContext {
    pub session_id: Option<String>,
    pub source_generation: Option<String>,
    pub control_revision: Option<u64>,
    pub pause_revision: Option<u64>,
    pub timed_out_attempt: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailureState {
    pub phase: String,
    pub audio_status: Option<AudioStatus>,
    pub ledger: Option<AudioLedger>,
    pub pause: Option<PauseSeal>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlFailure {
    pub error: FailureDetail,
    pub context: FailureContext,
    pub state: FailureState,
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ControlResult<T: Serialize> {
    Ok {
        ok: bool,
        value: T,
    },
    Err {
        ok: bool,
        #[serde(flatten)]
        failure: Box<ControlFailure>,
    },
}
impl<T: Serialize> ControlResult<T> {
    pub fn success(value: T) -> Result<Self, FailureCode> {
        let result = Self::Ok { ok: true, value };
        checked_bytes(&result, RESULT_BYTES)?;
        Ok(result)
    }
    pub fn failure(failure: ControlFailure) -> Self {
        Self::Err {
            ok: false,
            failure: Box::new(failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> ReconcileRequest {
        ReconcileRequest {
            owner_token: "1".repeat(32),
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            pause_revision: 1,
            batch_id: "2".repeat(32),
            commands: vec![LevelIntent {
                command_sequence: 1,
                render_level: MixLevel {
                    gain: -0.,
                    muted: false,
                },
                microphone_level: MixLevel::default(),
            }],
            mode: ReconcileMode::RetainedQueue,
            replacement_review_token: None,
            acknowledge_lost_local_intent: None,
        }
    }
    #[test]
    fn f32_canonical_retry_wire_is_flat_strict_and_bounded() {
        let r = request().normalized().unwrap();
        assert_eq!(r.commands[0].render_level.gain.to_bits(), 0);
        let retry = ReconcileRetryRequest {
            request: r,
            operation_id: "3".repeat(32),
            expected_timed_out_attempt: 1,
        };
        let mut value = serde_json::to_value(&retry).unwrap();
        assert!(value.get("request").is_none());
        assert!(
            serde_json::from_value::<ReconcileRetryRequest>(value.clone())
                .unwrap()
                .normalized()
                .is_ok()
        );
        value["extra"] = true.into();
        assert!(serde_json::from_value::<ReconcileRetryRequest>(value).is_err());
        let mut r = request();
        r.commands[0].render_level.gain = f32::INFINITY;
        assert!(r.normalized().is_err());
        let mut r = request();
        r.commands = vec![r.commands[0].clone(); 65];
        assert!(r.normalized().is_err());
    }
    #[test]
    fn full_maximum_renderer_queue_and_reply_budget_use_actual_serializer() {
        let mut r = request();
        r.session_id = "s".repeat(80);
        r.source_generation = "g".repeat(80);
        r.pause_revision = MAX_SAFE;
        r.commands = (0..64)
            .map(|i| LevelIntent {
                command_sequence: MAX_SAFE - 63 + i,
                render_level: MixLevel {
                    gain: 1.2345678,
                    muted: i % 2 == 1,
                },
                microphone_level: MixLevel {
                    gain: 0.12345678,
                    muted: false,
                },
            })
            .collect();
        assert!(checked_bytes(&r, REQUEST_BYTES).unwrap() < REQUEST_BYTES);
        let mut invalid = r.clone();
        invalid.commands[63].command_sequence = MAX_SAFE + 1;
        assert!(invalid.normalized().is_err());
        let mut invalid = r;
        invalid.commands.swap(0, 1);
        assert!(invalid.normalized().is_err());
        assert_eq!(
            checked_bytes(&"x".repeat(REQUEST_BYTES), REQUEST_BYTES),
            Err(FailureCode::OversizedRequest)
        );
    }
    #[test]
    fn flattened_accepted_identity_roundtrips_without_unknown_fields() {
        let v = AcceptedIntent {
            intent: request().commands.remove(0),
            target_sample: Some("480".into()),
            placement: Placement::Active,
        };
        let value = serde_json::to_value(&v).unwrap();
        assert!(serde_json::from_value::<AcceptedIntent>(value.clone()).is_ok());
        let mut extra = value;
        extra["injected"] = true.into();
        assert!(serde_json::from_value::<AcceptedIntent>(extra).is_err());
    }
}
