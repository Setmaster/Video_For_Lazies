//! Protected audio intent ledger and finite reconciliation attempts. No device activation.
use super::{
    audio_controls::*, audio_reconcile_wire as wire, audio_types::*,
    session_controls::ControlContext,
};
use std::collections::VecDeque;
use std::sync::atomic::Ordering;

#[derive(Clone)]
struct Pending {
    value: AcceptedIntent,
    sent: bool,
    edge: bool,
}
#[derive(Clone)]
struct Settled {
    intent: LevelIntent,
    at: u64,
}
#[derive(Clone)]
struct Attempt {
    number: u64,
    deadline: u64,
    outcome: Option<Result<ReconcileReply, FailureCode>>,
    messages: Vec<wire::Message>,
    cursor: usize,
    expected: Option<wire::Ack>,
}
#[derive(Clone)]
struct Operation {
    request: ReconcileRetryRequest,
    attempt: Attempt,
    at: u64,
}
#[derive(Clone)]
struct Batch {
    request: ReconcileRequest,
    digest: String,
    private_token: String,
    first: Attempt,
    current: Attempt,
    operations: VecDeque<Operation>,
    at: u64,
}
pub(super) struct Engine {
    pub context: ControlContext,
    config: NativeAudioConfig,
    pub status: AudioStatus,
    pending: Vec<Pending>,
    settled: VecDeque<Settled>,
    terminal: Vec<LevelIntent>,
    accepted: u64,
    sealed: u64,
    cutoff: Option<u64>,
    pause_revision: u64,
    review: Option<String>,
    review_fingerprint: Option<String>,
    fence: Option<String>,
    batch: Option<Batch>,
    history: VecDeque<Batch>,
    nonce: u64,
    closed: bool,
}
impl Engine {
    pub fn new(config: NativeAudioConfig, context: ControlContext) -> Self {
        let meter = AudioMeter {
            state: "pending".into(),
            ..Default::default()
        };
        let status = AudioStatus {
            session_id: config.session_id.clone(),
            source_generation: config.source_generation.clone(),
            sequence: 0,
            accepted_command_sequence: 0,
            applied_command_sequence: 0,
            applied_at_sample: "0".into(),
            rejected_command_sequence: None,
            render_level: config.render_level,
            microphone_level: config.microphone_level,
            render: meter.clone(),
            microphone: meter.clone(),
            mixed: meter,
            message: None,
        };
        Self {
            context,
            config,
            status,
            pending: vec![],
            settled: VecDeque::new(),
            terminal: vec![],
            accepted: 0,
            sealed: 0,
            cutoff: None,
            pause_revision: 0,
            review: None,
            review_fingerprint: None,
            fence: None,
            batch: None,
            history: VecDeque::new(),
            nonce: 0,
            closed: false,
        }
    }
    fn refresh_review(&mut self) {
        if self.review.is_none() {
            self.review_fingerprint = None;
            return;
        }
        // Meters and publication sequence are deliberately excluded. Visible
        // acknowledgement binds exact effective values and the protected prefix.
        let fingerprint=hash(&serde_json::json!({"owner":self.context.owner_token,"session":self.config.session_id,"generation":self.config.source_generation,"pauseRevision":self.pause_revision,"cutoff":self.cutoff.map(|v|v.to_string()),"render":self.status.render_level,"microphone":self.status.microphone_level,"pending":self.ledger().map(|l|l.pending)})).ok();
        if fingerprint != self.review_fingerprint {
            match self.next_token() {
                Ok(token) => {
                    self.review = Some(token);
                    self.review_fingerprint = fingerprint;
                }
                Err(_) => {
                    if let Some(stop) = &self.context.stop {
                        stop.store(true, Ordering::Release);
                    }
                    self.stop();
                }
            }
        }
    }
    fn next_token(&mut self) -> Result<String, FailureCode> {
        let next = self
            .nonce
            .checked_add(1)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(FailureCode::CounterExhausted)?;
        self.nonce = next;
        Ok(format!("{next:032x}"))
    }
    fn prune(&mut self, now: u64) {
        self.settled
            .retain(|v| now.saturating_sub(v.at) < HISTORY_MS);
        self.history
            .retain(|v| now.saturating_sub(v.at) < HISTORY_MS);
        if let Some(b) = &mut self.batch {
            let current = b.current.number;
            b.operations
                .retain(|v| v.attempt.number == current || now.saturating_sub(v.at) < HISTORY_MS);
        }
        while self.history.len() > BATCH_HISTORY_LIMIT {
            self.history.pop_front();
        }
        // The sixteen-operation bound is global across the eight historical batches.
        while self.operation_count() > OPERATION_HISTORY_LIMIT {
            if let Some(b) = self.history.iter_mut().find(|b| !b.operations.is_empty()) {
                b.operations.pop_front();
            } else if let Some(b) = &mut self.batch {
                if let Some(index) = b
                    .operations
                    .iter()
                    .position(|o| o.attempt.number != b.current.number)
                {
                    b.operations.remove(index);
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        self.evict_historical_bytes(CACHE_BYTES);
    }
    fn evict_historical_bytes(&mut self, limit: usize) {
        // Production always uses CACHE_BYTES. A lower private test budget can
        // exercise this loop even when count limits already bound legal shapes.
        // Current batch and pending identities are never eviction candidates.
        while self.cached_bytes() > limit && !self.history.is_empty() {
            self.history.pop_front();
        }
    }
    fn operation_count(&self) -> usize {
        self.history
            .iter()
            .map(|b| b.operations.len())
            .sum::<usize>()
            + self.batch.as_ref().map_or(0, |b| b.operations.len())
    }
    fn cached_bytes(&self) -> usize {
        fn attempt_bytes(a: &Attempt) -> usize {
            a.outcome.as_ref().map_or(0, |r| match r {
                Ok(v) => serde_json::to_vec(v).map_or(CACHE_BYTES + 1, |v| v.len()),
                Err(_) => 64,
            })
        }
        fn bytes(b: &Batch) -> usize {
            serde_json::to_vec(&b.request).map_or(CACHE_BYTES + 1, |v| v.len())
                + b.operations
                    .iter()
                    .map(|o| {
                        serde_json::to_vec(&o.request).map_or(CACHE_BYTES + 1, |v| v.len())
                            + attempt_bytes(&o.attempt)
                    })
                    .sum::<usize>()
                + attempt_bytes(&b.first)
                + attempt_bytes(&b.current)
        }
        self.history.iter().map(bytes).sum::<usize>() + self.batch.as_ref().map_or(0, bytes)
    }
    fn terminal(&self) -> bool {
        self.closed
            || self
                .context
                .stop
                .as_ref()
                .is_some_and(|v| v.load(Ordering::Acquire))
            || matches!(
                self.context.phase.as_str(),
                "stopping"
                    | "finalizing"
                    | "saved"
                    | "completed"
                    | "recoverable"
                    | "failed"
                    | "idle"
            )
    }
    pub fn accepted_sequence(&self) -> u64 {
        self.accepted
    }
    pub fn set_context(&mut self, next: ControlContext, now: u64) {
        if next.session_id.as_deref() != Some(&self.config.session_id) {
            return;
        }
        if next.control_revision < self.context.control_revision {
            return;
        }
        if next.control_revision == self.context.control_revision
            && next.phase != self.context.phase
        {
            let advances = matches!(
                (self.context.phase.as_str(), next.phase.as_str()),
                ("preparing", "countdown" | "starting" | "recording")
                    | ("countdown", "starting" | "recording")
                    | ("starting", "recording")
                    | ("pausing", "paused")
                    | ("paused", "resuming" | "recording")
                    | ("resuming", "recording")
            );
            let terminal = matches!(
                next.phase.as_str(),
                "stopping"
                    | "finalizing"
                    | "saved"
                    | "completed"
                    | "recoverable"
                    | "failed"
                    | "idle"
            );
            if !advances && !terminal {
                return;
            }
        }
        if next.owner_token != self.context.owner_token {
            self.archive(now);
            self.fence = None;
            if self.context.owner_token.is_some() || self.pause_revision > 0 {
                self.review = Some(String::new());
                self.review_fingerprint = None;
            }
        }
        if next.phase == "pausing"
            && (self.context.phase != "pausing" || next.pause_revision != self.pause_revision)
        {
            self.archive(now);
            self.sealed = self.accepted;
            self.pause_revision = next.pause_revision;
            self.cutoff = None;
            self.fence = None;
        }
        self.context = next;
        self.refresh_review();
        if self.terminal() {
            self.stop();
        }
    }
    fn archive(&mut self, now: u64) {
        if let Some(mut b) = self.batch.take() {
            if b.current.outcome.is_none() {
                Self::end_attempt(&mut b, Err(FailureCode::StaleAdmission));
            }
            b.at = now;
            self.history.push_back(b);
        }
        self.prune(now);
    }
    pub fn stop(&mut self) {
        self.closed = true;
        if !self.pending.is_empty() {
            self.terminal
                .extend(self.pending.drain(..).map(|v| v.value.intent));
        }
        self.fence = None;
        self.review = None;
        if let Some(b) = &mut self.batch
            && b.current.outcome.is_none()
        {
            Self::end_attempt(b, Err(FailureCode::SessionTerminal));
        }
    }
    pub fn ledger(&self) -> Option<AudioLedger> {
        (self.config.role_mask() != 0).then(|| AudioLedger {
            pending: self.pending.iter().map(|v| v.value.clone()).collect(),
            terminal_unapplied: self.terminal.clone(),
        })
    }
    pub fn pause_audio(&self) -> Option<PauseAudio> {
        if self.config.role_mask() == 0 || self.pause_revision == 0 {
            return None;
        }
        Some(PauseAudio {
            source_generation: self.config.source_generation.clone(),
            sealed_accepted_sequence: self.sealed,
            fence: FenceState {
                state: if self.review.is_some() {
                    FencePhase::ReplacementReviewRequired
                } else if self.fence.is_some() {
                    FencePhase::Ready
                } else {
                    FencePhase::Waiting
                },
                token: self.fence.clone(),
                replacement_review_token: self.review.clone(),
            },
        })
    }
    pub fn pause_seal(
        &mut self,
        revision: u64,
        cutoff: u64,
        host: Vec<AcceptedIntent>,
    ) -> Result<(), FailureCode> {
        if self.terminal() {
            return Err(FailureCode::SessionTerminal);
        }
        if revision != self.context.pause_revision
            || revision != self.pause_revision
            || host.len() > PENDING_LIMIT
            || host.len() != self.pending.len()
            || host
                .windows(2)
                .any(|w| w[0].intent.command_sequence >= w[1].intent.command_sequence)
        {
            return Err(FailureCode::ProtocolFailure);
        }
        for item in &host {
            if item.target_sample.as_deref().and_then(sample) != Some(cutoff)
                || item.placement == Placement::AwaitingReceipt
            {
                return Err(FailureCode::ProtocolFailure);
            }
            let Some(p) = self
                .pending
                .iter()
                .find(|p| p.value.intent.command_sequence == item.intent.command_sequence)
            else {
                return Err(FailureCode::ProtocolFailure);
            };
            if p.value.intent != item.intent
                || p.value
                    .target_sample
                    .as_ref()
                    .is_some_and(|t| Some(t) != item.target_sample.as_ref())
            {
                return Err(FailureCode::ProtocolFailure);
            }
        }
        for item in host {
            self.receipt(AudioReceipt {
                command_sequence: item.intent.command_sequence,
                target_sample: item.target_sample.unwrap(),
                placement: item.placement,
            })?;
        }
        self.cutoff = Some(cutoff);
        self.refresh_review();
        Ok(())
    }
    fn identity(&self, owner: &str, session: &str, generation: &str) -> Result<(), FailureCode> {
        if self.context.owner_token.as_deref() != Some(owner) {
            return Err(FailureCode::StaleOwner);
        }
        if self.config.session_id != session {
            return Err(FailureCode::StaleSession);
        }
        if self.config.source_generation != generation {
            return Err(FailureCode::SourceChanged);
        }
        Ok(())
    }
    fn known(&self, intent: &LevelIntent) -> Result<Option<LevelDisposition>, FailureCode> {
        if let Some(p) = self
            .pending
            .iter()
            .find(|v| v.value.intent.command_sequence == intent.command_sequence)
        {
            return if p.value.intent == *intent {
                Ok(Some(LevelDisposition::pending(&p.value)))
            } else {
                Err(FailureCode::IdentityConflict)
            };
        }
        if let Some(p) = self
            .settled
            .iter()
            .find(|v| v.intent.command_sequence == intent.command_sequence)
        {
            return if p.intent == *intent {
                Ok(Some(LevelDisposition::settled(intent.command_sequence)))
            } else {
                Err(FailureCode::IdentityConflict)
            };
        }
        if let Some(old) = self
            .terminal
            .iter()
            .find(|v| v.command_sequence == intent.command_sequence)
        {
            return Err(if old == intent {
                FailureCode::SessionTerminal
            } else {
                FailureCode::IdentityConflict
            });
        }
        Ok(None)
    }
    fn remember(&mut self, intent: LevelIntent, now: u64) {
        self.settled.push_back(Settled { intent, at: now });
        while self.settled.len() > SETTLED_LIMIT {
            self.settled.pop_front();
        }
    }
    pub fn submit(&mut self, request: LevelRequest, now: u64) -> Result<LevelReply, FailureCode> {
        self.prune(now);
        let request = request.normalized()?;
        self.identity(
            &request.owner_token,
            &request.session_id,
            &request.source_generation,
        )?;
        let intent = request.intent();
        // Full identity lookup deliberately precedes phase/revision/terminal admission gates.
        let disposition = if let Some(d) = self.known(&intent)? {
            d
        } else {
            if intent.command_sequence <= self.accepted {
                return Err(FailureCode::StaleUnknown);
            }
            if self.terminal() {
                return Err(FailureCode::SessionTerminal);
            }
            if self.config.role_mask() == 0 {
                return Err(FailureCode::AudioControlsOff);
            }
            if self.context.phase != "recording" {
                LevelDisposition::DeferredNotAccepted {
                    command_sequence: intent.command_sequence,
                    reason: if self.context.phase == "paused" {
                        DeferredReason::Paused
                    } else {
                        DeferredReason::Transitioning
                    },
                    retry_after_revision: self.context.control_revision,
                }
            } else {
                if self.status.sequence == MAX_SAFE {
                    return Err(FailureCode::CounterExhausted);
                }
                if request.expected_control_revision != self.context.control_revision {
                    return Err(FailureCode::StaleAdmission);
                }
                let previous = self.pending.last().map(|p| &p.value.intent);
                let same_mutes = previous.map_or(
                    self.status.render_level.muted == intent.render_level.muted
                        && self.status.microphone_level.muted == intent.microphone_level.muted,
                    |p| p.same_mutes(&intent),
                );
                // Coalesce only unsubmitted consecutive gain changes. Sent/mute identities are protected.
                if same_mutes
                    && self
                        .pending
                        .last()
                        .is_some_and(|p| !p.sent && !p.edge && p.value.intent.same_mutes(&intent))
                {
                    let old = self.pending.pop().unwrap();
                    self.remember(old.value.intent, now);
                }
                if self.pending.len() >= PENDING_LIMIT {
                    return Err(FailureCode::Capacity);
                }
                let value = AcceptedIntent {
                    intent: intent.clone(),
                    target_sample: None,
                    placement: Placement::AwaitingReceipt,
                };
                let d = LevelDisposition::pending(&value);
                self.pending.push(Pending {
                    value,
                    sent: false,
                    edge: !same_mutes,
                });
                self.accepted = intent.command_sequence;
                self.status.sequence += 1;
                d
            }
        };
        self.status.accepted_command_sequence = self.accepted;
        Ok(LevelReply {
            owner_token: request.owner_token,
            session_id: request.session_id,
            source_generation: request.source_generation,
            control_revision: self.context.control_revision,
            disposition,
            audio_status: self.status.clone(),
            ledger: self.ledger().unwrap_or_default(),
        })
    }
    pub fn receipt(&mut self, receipt: AudioReceipt) -> Result<(), FailureCode> {
        if sample(&receipt.target_sample).is_none()
            || receipt.placement == Placement::AwaitingReceipt
        {
            return Err(FailureCode::ProtocolFailure);
        }
        if let Some(p) = self
            .pending
            .iter_mut()
            .find(|p| p.value.intent.command_sequence == receipt.command_sequence)
        {
            if p.value
                .target_sample
                .as_ref()
                .is_some_and(|t| *t != receipt.target_sample)
            {
                return Err(FailureCode::ProtocolFailure);
            }
            p.value.target_sample = Some(receipt.target_sample);
            p.value.placement = receipt.placement;
        } else if receipt.command_sequence > self.accepted {
            return Err(FailureCode::ProtocolFailure);
        }
        self.refresh_review();
        Ok(())
    }
    pub fn update(&mut self, mut status: AudioStatus, now: u64) {
        let mut removed = vec![];
        self.pending.retain(|p| {
            if p.value.intent.command_sequence <= status.applied_command_sequence {
                removed.push(p.value.intent.clone());
                false
            } else {
                true
            }
        });
        self.terminal.retain(|intent| {
            if intent.command_sequence <= status.applied_command_sequence {
                removed.push(intent.clone());
                false
            } else {
                true
            }
        });
        for v in removed {
            self.remember(v, now);
        }
        status.accepted_command_sequence = self.accepted.max(status.accepted_command_sequence);
        self.status = status;
        self.refresh_review();
    }
    pub fn ordinary(&self) -> Vec<AudioLevels> {
        if !matches!(self.context.phase.as_str(), "recording" | "pausing") || self.terminal() {
            return vec![];
        }
        let fresh: Vec<_> = self
            .pending
            .iter()
            .filter(|p| !p.sent)
            .map(|p| {
                p.value
                    .intent
                    .levels(&self.config.session_id, &self.config.source_generation)
            })
            .collect();
        if fresh.is_empty() && self.context.phase == "pausing" {
            return vec![];
        }
        if fresh.is_empty() {
            self.pending
                .first()
                .map(|p| {
                    vec![
                        p.value
                            .intent
                            .levels(&self.config.session_id, &self.config.source_generation),
                    ]
                })
                .unwrap_or_default()
        } else {
            fresh
        }
    }
    pub fn sent(&mut self, seq: u64) {
        if let Some(p) = self
            .pending
            .iter_mut()
            .find(|p| p.value.intent.command_sequence == seq)
        {
            p.sent = true;
        }
    }
    fn end_attempt(b: &mut Batch, outcome: Result<ReconcileReply, FailureCode>) {
        b.current.outcome = Some(outcome);
        if b.current.number == 1 {
            b.first = b.current.clone();
        }
        for op in &mut b.operations {
            if op.attempt.number == b.current.number {
                op.attempt = b.current.clone();
            }
        }
    }
    pub fn tick(&mut self, now: u64) {
        if self.terminal() {
            self.stop();
            return;
        }
        if let Some(b) = &mut self.batch
            && b.current.outcome.is_none()
            && now >= b.current.deadline
        {
            Self::end_attempt(b, Err(FailureCode::ReconciliationTimeout));
            self.fence = None;
        }
        self.prune(now);
    }
    pub fn reconcile(&mut self, request: ReconcileRequest, now: u64) -> Result<u64, FailureCode> {
        self.tick(now);
        let request = request.normalized()?;
        self.identity(
            &request.owner_token,
            &request.session_id,
            &request.source_generation,
        )?;
        if let Some(b) = self
            .batch
            .as_ref()
            .filter(|b| b.request.batch_id == request.batch_id)
            .or_else(|| {
                self.history
                    .iter()
                    .find(|b| b.request.batch_id == request.batch_id)
            })
        {
            return if b.request == request {
                Ok(1)
            } else {
                Err(FailureCode::BatchConflict)
            };
        }
        if self.terminal() {
            return Err(FailureCode::SessionTerminal);
        }
        if self.config.role_mask() == 0 {
            return Err(FailureCode::AudioControlsOff);
        }
        if self.context.phase != "paused" || request.pause_revision != self.pause_revision {
            return Err(FailureCode::StaleAdmission);
        }
        if self.batch.is_some() {
            return Err(FailureCode::BatchBusy);
        }
        if self.status.sequence == MAX_SAFE {
            return Err(FailureCode::CounterExhausted);
        }
        let cutoff = self.cutoff.ok_or(FailureCode::TransitionBusy)?;
        if self.review.is_some()
            && (request.mode != ReconcileMode::AuthoritativeOnly
                || request.replacement_review_token != self.review)
        {
            return Err(FailureCode::ReplacementReviewRequired);
        }
        if self.review.is_none() && request.mode == ReconcileMode::AuthoritativeOnly {
            return Err(FailureCode::ReplacementReviewRequired);
        }
        let mut additions = vec![];
        let mut high = self.accepted;
        for command in &request.commands {
            if self.known(command)?.is_none() {
                if command.command_sequence <= high {
                    return Err(FailureCode::StaleUnknown);
                }
                high = command.command_sequence;
                additions.push(Pending {
                    value: AcceptedIntent {
                        intent: command.clone(),
                        target_sample: Some(cutoff.to_string()),
                        placement: Placement::ResumeBound,
                    },
                    sent: false,
                    edge: true,
                });
            }
        }
        if self.pending.len() + additions.len() > PENDING_LIMIT {
            return Err(FailureCode::Capacity);
        }
        // Validate complete prefix and wire bounds before publishing any admission mutation.
        let mut all = self.pending.clone();
        all.extend(additions.clone());
        if all.iter().any(|p| p.value.target_sample.is_none()) {
            return Err(FailureCode::TransitionBusy);
        }
        let private_token = self.next_token()?;
        let attempt = self.make_attempt(&all, &private_token, 1, high, now)?;
        let batch = Batch {
            digest: request.batch_digest()?,
            request,
            private_token,
            first: attempt.clone(),
            current: attempt,
            operations: VecDeque::new(),
            at: now,
        };
        self.pending.extend(additions);
        self.accepted = high;
        self.status.sequence += 1;
        self.status.accepted_command_sequence = high;
        self.review = None;
        self.fence = None;
        self.batch = Some(batch);
        Ok(1)
    }
    fn make_attempt(
        &self,
        pending: &[Pending],
        token: &str,
        number: u64,
        high: u64,
        now: u64,
    ) -> Result<Attempt, FailureCode> {
        let entries: Vec<_> = pending
            .iter()
            .enumerate()
            .map(|(ordinal, p)| wire::Entry {
                batch_token: token.into(),
                attempt: number,
                ordinal,
                command_sequence: p.value.intent.command_sequence,
                render_level: p.value.intent.render_level,
                microphone_level: p.value.intent.microphone_level,
                target_sample: p.value.target_sample.clone().unwrap_or_default(),
                placement: p.value.placement,
            })
            .collect();
        let digest = wire::prefix_digest(&entries).map_err(|_| FailureCode::ProtocolFailure)?;
        let begin = wire::Begin {
            session_id: self.config.session_id.clone(),
            source_generation: self.config.source_generation.clone(),
            pause_revision: self.pause_revision,
            batch_token: token.into(),
            attempt: number,
            count: entries.len(),
            prefix_digest: digest.clone(),
            sealed_sequence: self.sealed,
            accepted_sequence: high,
        };
        let mut messages = vec![wire::Message::Begin(begin)];
        messages.extend(entries.into_iter().map(wire::Message::Entry));
        messages.push(wire::Message::Seal(wire::Seal {
            batch_token: token.into(),
            attempt: number,
            count: pending.len(),
            prefix_digest: digest.clone(),
        }));
        for m in &messages {
            wire::encode(m).map_err(|_| FailureCode::OversizedRequest)?;
        }
        Ok(Attempt {
            number,
            deadline: now.saturating_add(RECONCILE_MS),
            outcome: None,
            messages,
            cursor: 0,
            expected: Some(wire::Ack {
                batch_token: token.into(),
                attempt: number,
                count: pending.len(),
                prefix_digest: digest,
            }),
        })
    }
    pub fn retry(&mut self, request: ReconcileRetryRequest, now: u64) -> Result<u64, FailureCode> {
        self.tick(now);
        let request = request.normalized()?;
        self.identity(
            &request.request.owner_token,
            &request.request.session_id,
            &request.request.source_generation,
        )?;
        // Lost retry replies join exact current or historical operations before any CAS/admission gate.
        if let Some(op) = self
            .batch
            .iter()
            .chain(self.history.iter())
            .flat_map(|b| b.operations.iter())
            .find(|o| o.request.operation_id == request.operation_id)
        {
            return if op.request == request {
                Ok(op.attempt.number)
            } else {
                Err(FailureCode::IdentityConflict)
            };
        }
        let b = self.batch.as_ref().ok_or(FailureCode::StaleAttempt)?;
        if b.request != request.request {
            return Err(FailureCode::BatchConflict);
        }
        if self.terminal() {
            return Err(FailureCode::SessionTerminal);
        }
        if self.context.phase != "paused" || self.pause_revision != request.request.pause_revision {
            return Err(FailureCode::StaleAdmission);
        }
        if b.current.number != request.expected_timed_out_attempt
            || !matches!(
                b.current.outcome,
                Some(Err(
                    FailureCode::ReconciliationTimeout | FailureCode::StageExpired
                ))
            )
        {
            return Err(FailureCode::StaleAttempt);
        }
        let number = b
            .current
            .number
            .checked_add(1)
            .filter(|n| *n <= MAX_SAFE)
            .ok_or(FailureCode::CounterExhausted)?;
        let attempt =
            self.make_attempt(&self.pending, &b.private_token, number, self.accepted, now)?;
        let b = self.batch.as_mut().unwrap();
        // Historical operation receipts only; current attempt is separately protected.
        while b.operations.len() >= OPERATION_HISTORY_LIMIT {
            b.operations.pop_front();
        }
        b.operations.push_back(Operation {
            request,
            attempt: attempt.clone(),
            at: now,
        });
        b.current = attempt;
        self.fence = None;
        self.prune(now);
        Ok(number)
    }
    pub fn outcome(
        &mut self,
        batch_id: &str,
        attempt: u64,
        now: u64,
    ) -> Option<Result<ReconcileReply, FailureCode>> {
        self.tick(now);
        let b = self
            .batch
            .as_ref()
            .filter(|b| b.request.batch_id == batch_id)
            .or_else(|| self.history.iter().find(|b| b.request.batch_id == batch_id))?;
        if attempt == 1 {
            b.first.outcome.clone()
        } else if b.current.number == attempt {
            b.current.outcome.clone()
        } else {
            b.operations
                .iter()
                .find(|o| o.attempt.number == attempt)
                .and_then(|o| o.attempt.outcome.clone())
        }
    }
    pub fn wire_front(&mut self, now: u64) -> Option<wire::Message> {
        self.tick(now);
        let b = self.batch.as_ref()?;
        if b.current.outcome.is_some() {
            return None;
        }
        b.current.messages.get(b.current.cursor).cloned()
    }
    pub fn wire_sent(&mut self) {
        if let Some(b) = &mut self.batch {
            b.current.cursor += 1;
        }
    }
    pub fn ack(&mut self, ack: wire::Ack, now: u64) -> Result<(), FailureCode> {
        self.tick(now);
        let Some(b) = self.batch.as_ref() else {
            return Ok(());
        };
        if b.private_token != ack.batch_token
            || b.current.number != ack.attempt
            || b.current.outcome.is_some()
        {
            return Ok(());
        }
        if b.current.expected.as_ref() != Some(&ack) || b.current.cursor != b.current.messages.len()
        {
            return Err(FailureCode::ProtocolFailure);
        }
        let mut dispositions = vec![];
        for c in &b.request.commands {
            dispositions.push(self.known(c)?.ok_or(FailureCode::ProtocolFailure)?);
        }
        let reply = ReconcileReply {
            owner_token: b.request.owner_token.clone(),
            session_id: b.request.session_id.clone(),
            source_generation: b.request.source_generation.clone(),
            pause_revision: self.pause_revision,
            batch_id: b.request.batch_id.clone(),
            batch_digest: b.digest.clone(),
            fence_token: b.private_token.clone(),
            dispositions,
            audio_status: self.status.clone(),
            ledger: self.ledger().unwrap_or_default(),
        };
        checked_bytes(&reply, RESULT_BYTES)?;
        self.fence = Some(b.private_token.clone());
        Self::end_attempt(self.batch.as_mut().unwrap(), Ok(reply));
        self.prune(now);
        Ok(())
    }
    pub fn stage_error(
        &mut self,
        token: &str,
        attempt: u64,
        code: &str,
        now: u64,
    ) -> Result<(), FailureCode> {
        self.tick(now);
        if let Some(b) = &mut self.batch
            && b.private_token == token
            && b.current.number == attempt
            && b.current.outcome.is_none()
        {
            if code != "stageExpired" {
                return Err(FailureCode::ProtocolFailure);
            }
            Self::end_attempt(b, Err(FailureCode::StageExpired));
            self.fence = None;
        }
        Ok(())
    }
    pub fn fence_ready(&self, revision: u64, token: &str) -> bool {
        !self.terminal()
            && self.context.phase == "paused"
            && revision == self.pause_revision
            && self.fence.as_deref() == Some(token)
    }
    pub fn failure(&self, code: FailureCode, sequence: Option<u64>) -> ControlFailure {
        let (retry, action, message) = code.policy();
        let mut audio = self.pause_audio();
        if code == FailureCode::StaleOwner
            && let Some(a) = &mut audio
        {
            a.fence.token = None;
            a.fence.replacement_review_token = None;
        }
        ControlFailure {
            error: FailureDetail {
                code,
                retry,
                action,
                command_sequence: sequence,
                message: message.into(),
            },
            context: FailureContext {
                session_id: Some(self.config.session_id.clone()),
                source_generation: Some(self.config.source_generation.clone()),
                control_revision: Some(self.context.control_revision),
                pause_revision: Some(self.pause_revision),
                timed_out_attempt: self
                    .batch
                    .as_ref()
                    .filter(|b| {
                        matches!(
                            b.current.outcome,
                            Some(Err(
                                FailureCode::StageExpired | FailureCode::ReconciliationTimeout
                            ))
                        )
                    })
                    .map(|b| b.current.number),
            },
            state: FailureState {
                phase: self.context.phase.clone(),
                audio_status: Some(self.status.clone()),
                ledger: self.ledger(),
                pause: self.cutoff.map(|n| PauseSeal {
                    pause_revision: self.pause_revision,
                    cutoff_sample: n.to_string(),
                    audio,
                }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    fn engine() -> Engine {
        let config = NativeAudioConfig {
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            render: NativeRender::System {
                device_id: "default".into(),
            },
            microphone: None,
            render_level: MixLevel::default(),
            microphone_level: MixLevel::default(),
        };
        let context = ControlContext {
            owner_token: Some("1".repeat(32)),
            session_id: Some("session".into()),
            phase: "recording".into(),
            control_revision: 1,
            pause_revision: 0,
            resolved_role_mask: 1,
            stop: Some(Arc::new(AtomicBool::new(false))),
        };
        Engine::new(config, context)
    }
    fn level(seq: u64, muted: bool) -> LevelRequest {
        LevelRequest {
            owner_token: "1".repeat(32),
            expected_control_revision: 1,
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            command_sequence: seq,
            render_level: MixLevel { gain: 1., muted },
            microphone_level: MixLevel::default(),
        }
    }
    fn pause(e: &mut Engine) {
        let mut c = e.context.clone();
        c.phase = "pausing".into();
        c.control_revision += 1;
        c.pause_revision += 1;
        e.set_context(c.clone(), 10);
        c.phase = "paused".into();
        e.set_context(c, 11);
        let host = e
            .pending
            .iter()
            .map(|p| AcceptedIntent {
                intent: p.value.intent.clone(),
                target_sample: p.value.target_sample.clone().or_else(|| Some("480".into())),
                placement: if p.value.placement == Placement::AwaitingReceipt {
                    Placement::Active
                } else {
                    p.value.placement
                },
            })
            .collect();
        e.pause_seal(e.pause_revision, 480, host).unwrap();
    }
    fn request() -> ReconcileRequest {
        ReconcileRequest {
            owner_token: "1".repeat(32),
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            pause_revision: 1,
            batch_id: "2".repeat(32),
            commands: vec![],
            mode: ReconcileMode::RetainedQueue,
            replacement_review_token: None,
            acknowledge_lost_local_intent: None,
        }
    }
    fn ack(e: &mut Engine, now: u64) -> wire::Ack {
        while e.wire_front(now).is_some() {
            e.wire_sent();
        }
        e.batch.as_ref().unwrap().current.expected.clone().unwrap()
    }
    #[test]
    fn pending_lookup_precedes_admission_and_every_mute_edge_is_protected() {
        let mut e = engine();
        for seq in 1..=64 {
            e.submit(level(seq, seq % 2 == 1), 0).unwrap();
            e.sent(seq);
        }
        assert_eq!(
            e.submit(level(65, true), 1).unwrap_err(),
            FailureCode::Capacity
        );
        assert_eq!(e.accepted, 64);
        pause(&mut e);
        let reply = e.submit(level(1, true), HISTORY_MS * 10).unwrap();
        assert!(matches!(
            reply.disposition,
            LevelDisposition::AcceptedPending { .. }
        ));
        let mut changed = level(1, true);
        changed.render_level.gain = 0.5;
        assert_eq!(
            e.submit(changed, 3).unwrap_err(),
            FailureCode::IdentityConflict
        );
        assert!(matches!(
            e.submit(level(65, true), 4).unwrap().disposition,
            LevelDisposition::DeferredNotAccepted { .. }
        ));
        assert_eq!(e.accepted, 64);
        assert_eq!(e.ledger().unwrap().pending.len(), 64);
    }
    #[test]
    fn unsent_gain_coalescing_does_not_remove_a_mute_edge_or_claim_pcm_effect() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        let mut r = level(2, true);
        r.render_level.gain = 0.5;
        e.submit(r, 0).unwrap();
        assert_eq!(e.pending.len(), 2);
        let mut r = level(3, true);
        r.render_level.gain = 0.7;
        e.submit(r, 0).unwrap();
        assert_eq!(e.pending.len(), 2);
        assert!(matches!(
            e.submit(level(2, true), 0),
            Err(FailureCode::IdentityConflict)
        ));
        let mut exact = level(2, true);
        exact.render_level.gain = 0.5;
        assert!(matches!(
            e.submit(exact, 0).unwrap().disposition,
            LevelDisposition::Settled {
                identity_verified: true,
                applied_as_requested: None,
                ..
            }
        ));
    }
    #[test]
    fn first_receipt_and_pause_seal_survive_reconciliation_without_rebasing_target() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        e.sent(1);
        e.receipt(AudioReceipt {
            command_sequence: 1,
            target_sample: "480".into(),
            placement: Placement::Active,
        })
        .unwrap();
        pause(&mut e);
        let mut r = request();
        r.commands = vec![level(1, true).intent(), level(3, false).intent()];
        e.reconcile(r.clone(), 20).unwrap();
        assert_eq!(e.sealed, 1);
        assert_eq!(e.accepted, 3);
        assert_eq!(e.pending[0].value.target_sample.as_deref(), Some("480"));
        assert_eq!(e.pending[1].value.target_sample.as_deref(), Some("480"));
        let a = ack(&mut e, 21);
        e.ack(a, 22).unwrap();
        let out = e.outcome(&r.batch_id, 1, 23).unwrap().unwrap();
        assert!(e.fence_ready(1, &out.fence_token));
        assert_eq!(out.ledger.pending.len(), 2);
        assert_eq!(
            e.receipt(AudioReceipt {
                command_sequence: 1,
                target_sample: "960".into(),
                placement: Placement::Active
            }),
            Err(FailureCode::ProtocolFailure)
        );
    }
    #[test]
    fn initial_duplicates_never_restart_and_retry_cas_is_idempotent_before_consumption_check() {
        let mut e = engine();
        pause(&mut e);
        let r = request();
        e.reconcile(r.clone(), 100).unwrap();
        e.reconcile(r.clone(), 5099).unwrap();
        e.tick(5100);
        assert!(matches!(
            e.outcome(&r.batch_id, 1, 5100),
            Some(Err(FailureCode::ReconciliationTimeout))
        ));
        assert_eq!(e.reconcile(r.clone(), 5200).unwrap(), 1);
        let retry = ReconcileRetryRequest {
            request: r.clone(),
            operation_id: "3".repeat(32),
            expected_timed_out_attempt: 1,
        };
        assert_eq!(e.retry(retry.clone(), 5200).unwrap(), 2);
        assert_eq!(e.retry(retry.clone(), 6000).unwrap(), 2);
        let mut conflict = retry.clone();
        conflict.expected_timed_out_attempt = 2;
        assert_eq!(e.retry(conflict, 6000), Err(FailureCode::IdentityConflict));
        assert_eq!(e.batch.as_ref().unwrap().current.deadline, 10200);
        let mut bad = retry.clone();
        bad.operation_id = "4".repeat(32);
        assert_eq!(e.retry(bad, 6001), Err(FailureCode::StaleAttempt));
        let old = wire::Ack {
            batch_token: e.batch.as_ref().unwrap().private_token.clone(),
            attempt: 1,
            count: 0,
            prefix_digest: wire::prefix_digest(&[]).unwrap(),
        };
        e.ack(old, 6001).unwrap();
        assert!(e.fence.is_none());
        let a = ack(&mut e, 6002);
        e.ack(a, 6003).unwrap();
        assert!(e.outcome(&r.batch_id, 2, 6004).unwrap().is_ok());
        assert!(e.outcome(&r.batch_id, 1, 6004).unwrap().is_err());
        e.tick(200000);
        assert_eq!(e.retry(retry, 200001).unwrap(), 2); // current operation remains protected
    }
    #[test]
    fn owner_replacement_requires_review_and_stop_remains_independent_of_parked_attempt() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        e.receipt(AudioReceipt {
            command_sequence: 1,
            target_sample: "480".into(),
            placement: Placement::Active,
        })
        .unwrap();
        pause(&mut e);
        let old = e.context.clone();
        let mut next = old.clone();
        next.owner_token = Some("4".repeat(32));
        next.control_revision += 1;
        e.set_context(next, 12);
        e.set_context(old, 13);
        assert_eq!(
            e.context.owner_token.as_deref(),
            Some("44444444444444444444444444444444")
        );
        let mut r = request();
        r.owner_token = "4".repeat(32);
        assert_eq!(
            e.reconcile(r.clone(), 14),
            Err(FailureCode::ReplacementReviewRequired)
        );
        r.mode = ReconcileMode::AuthoritativeOnly;
        r.replacement_review_token = e.review.clone();
        r.acknowledge_lost_local_intent = Some(true);
        e.reconcile(r, 15).unwrap();
        e.context
            .stop
            .as_ref()
            .unwrap()
            .store(true, Ordering::Release);
        e.tick(16);
        assert!(e.pending.is_empty());
        assert_eq!(e.ledger().unwrap().terminal_unapplied.len(), 1);
        assert!(e.wire_front(17).is_none());
        assert!(e.fence.is_none());
    }
    #[test]
    fn resolved_no_audio_is_null_not_a_fence_even_when_config_object_exists() {
        let mut e = engine();
        e.config.render = NativeRender::Off;
        e.context.resolved_role_mask = 0;
        pause(&mut e);
        assert!(e.pause_audio().is_none());
        assert!(e.ledger().is_none());
        assert_eq!(
            e.reconcile(request(), 20),
            Err(FailureCode::AudioControlsOff)
        );
        assert_eq!(
            e.submit(level(1, false), 20).unwrap_err(),
            FailureCode::AudioControlsOff
        );
        assert_eq!(e.accepted, 0);
    }
    #[test]
    fn older_same_revision_phases_cannot_reopen_admission_or_reseal_a_pause() {
        let mut e = engine();
        pause(&mut e);
        let paused = e.context.clone();
        let mut stale = paused.clone();
        stale.phase = "pausing".into();
        e.set_context(stale, 12);
        assert_eq!(e.context.phase, "paused");
        let mut resume = paused;
        resume.control_revision += 1;
        e.set_context(resume.clone(), 13);
        resume.phase = "resuming".into();
        e.set_context(resume.clone(), 14);
        resume.phase = "recording".into();
        e.set_context(resume.clone(), 15);
        resume.phase = "paused".into();
        e.set_context(resume, 16);
        assert_eq!(e.context.phase, "recording");
        e.stop();
        assert_eq!(
            e.submit(level(2, false), 17).unwrap_err(),
            FailureCode::SessionTerminal
        );
    }
    #[test]
    fn stage_expiration_requires_explicit_retry_and_combined_prefix_capacity_is_atomic() {
        let mut e = engine();
        for n in 1..=64 {
            e.submit(level(n, n % 2 == 1), 0).unwrap();
            e.sent(n);
            e.receipt(AudioReceipt {
                command_sequence: n,
                target_sample: "480".into(),
                placement: Placement::Active,
            })
            .unwrap();
        }
        pause(&mut e);
        let mut r = request();
        r.commands = vec![level(65, true).intent()];
        assert_eq!(e.reconcile(r, 20), Err(FailureCode::Capacity));
        assert_eq!(e.accepted, 64);
        assert!(e.batch.is_none());
        let r = request();
        e.reconcile(r.clone(), 21).unwrap();
        let token = e.batch.as_ref().unwrap().private_token.clone();
        e.stage_error(&token, 1, "stageExpired", 22).unwrap();
        assert!(e.wire_front(23).is_none());
        assert_eq!(e.reconcile(r.clone(), 24).unwrap(), 1);
        let retry = ReconcileRetryRequest {
            request: r,
            operation_id: "3".repeat(32),
            expected_timed_out_attempt: 1,
        };
        assert_eq!(e.retry(retry, 25).unwrap(), 2);
    }
    #[test]
    fn settled_identity_expiration_never_rearms_and_reply_failure_bytes_are_bounded() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        let mut status = e.status.clone();
        status.applied_command_sequence = 1;
        e.update(status, 1);
        assert!(matches!(
            e.submit(level(1, true), 2).unwrap().disposition,
            LevelDisposition::Settled { .. }
        ));
        assert_eq!(
            e.submit(level(1, true), HISTORY_MS + 2).unwrap_err(),
            FailureCode::StaleUnknown
        );
        let mut e = engine();
        pause(&mut e);
        let mut r = request();
        r.commands = (1..=64)
            .map(|n| {
                let mut i = level(n, n % 2 == 1).intent();
                i.render_level.gain = 1.2345678;
                i
            })
            .collect();
        e.reconcile(r.clone(), 20).unwrap();
        let ack = ack(&mut e, 21);
        e.ack(ack, 22).unwrap();
        let reply = e.outcome(&r.batch_id, 1, 23).unwrap().unwrap();
        let result = ControlResult::success(reply).unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < RESULT_BYTES);
        let fail = e.failure(FailureCode::StaleOwner, None);
        assert!(
            fail.state
                .pause
                .as_ref()
                .unwrap()
                .audio
                .as_ref()
                .unwrap()
                .fence
                .token
                .is_none()
        );
        assert!(serde_json::to_vec(&fail.error).unwrap().len() <= 4096);
        assert!(
            serde_json::to_vec(&ControlResult::<ReconcileReply>::failure(fail))
                .unwrap()
                .len()
                <= RESULT_BYTES
        );
    }
    #[test]
    fn global_operation_history_is_finite_and_expired_consumed_attempt_never_restarts() {
        let mut e = engine();
        pause(&mut e);
        let r = request();
        e.reconcile(r.clone(), 0).unwrap();
        for n in 1..=20 {
            let now = n * 6000;
            e.tick(now);
            let retry = ReconcileRetryRequest {
                request: r.clone(),
                operation_id: format!("{n:032x}"),
                expected_timed_out_attempt: n,
            };
            e.retry(retry, now).unwrap();
            assert!(e.operation_count() <= 16);
            assert!(e.cached_bytes() <= CACHE_BYTES);
        }
        let old = ReconcileRetryRequest {
            request: r,
            operation_id: format!("{:032x}", 1),
            expected_timed_out_attempt: 1,
        };
        assert_eq!(e.retry(old, 121000), Err(FailureCode::StaleAttempt));
    }
    #[test]
    fn main_and_host_protect_received_gains_through_pause_and_complete_prefix_restore() {
        use super::super::audio_mixer::{Command, ControlQueue};
        let mut e = engine();
        let mut q = ControlQueue::new(MixLevel::default(), MixLevel::default());
        let mut requests = vec![];
        for (seq, gain) in [(1, 0.8), (2, 0.5)] {
            let mut r = level(seq, false);
            r.render_level.gain = gain;
            requests.push(r.intent());
            e.submit(r.clone(), seq).unwrap();
            let levels = r.intent().levels("session", "audio-session");
            q.push_preserving(levels, seq as i64 * 100).unwrap();
            q.bind_target(seq, 480).unwrap();
            e.sent(seq);
            e.receipt(AudioReceipt {
                command_sequence: seq,
                target_sample: "480".into(),
                placement: Placement::Active,
            })
            .unwrap();
        }
        assert_eq!(q.len(), 2);
        pause(&mut e);
        let host = q
            .pending_commands()
            .iter()
            .map(|c| AcceptedIntent {
                intent: LevelIntent::from(&c.levels),
                target_sample: c.target_sample.map(|n| n.to_string()),
                placement: Placement::Active,
            })
            .collect();
        e.pause_seal(1, 480, host).unwrap();
        let mut request = request();
        request.commands = requests;
        e.reconcile(request, 20).unwrap();
        let mut stager = wire::Stager::default();
        let context = wire::HostContext {
            session_id: "session".into(),
            source_generation: "audio-session".into(),
            pause_revision: 1,
            sealed_sequence: 2,
            accepted_sequence: 2,
        };
        let mut ready = None;
        while let Some(m) = e.wire_front(21) {
            match m {
                wire::Message::Begin(v) => stager.begin(v, &context, 21).unwrap(),
                wire::Message::Entry(v) => stager.entry(v, 21).unwrap(),
                wire::Message::Seal(v) => ready = Some(stager.seal(v, 21).unwrap()),
            }
            e.wire_sent();
        }
        let ready = ready.unwrap();
        let entries: Vec<_> = ready
            .entries
            .iter()
            .map(|v| Command {
                levels: v.intent().levels("session", "audio-session"),
                received_qpc: 9999,
                target_sample: sample(&v.target_sample),
                edge: false,
            })
            .collect();
        q.restore_prefix(&entries).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.lookup_command(1).unwrap().received_qpc, 100);
        e.ack(ready.ack(), 22).unwrap();
        assert!(e.fence.is_some());
        assert_eq!(
            q.claim_due_at(480, None).unwrap().levels.command_sequence,
            1
        );
        assert_eq!(
            q.claim_due_at(480, None).unwrap().levels.command_sequence,
            2
        );
    }
    #[test]
    fn unsent_coalesced_sequence_gap_is_settled_without_entering_host_prefix() {
        let mut e = engine();
        let mut first = level(1, false);
        first.render_level.gain = 0.8;
        e.submit(first.clone(), 0).unwrap();
        let mut next = level(3, false);
        next.render_level.gain = 0.5;
        e.submit(next.clone(), 0).unwrap();
        e.sent(3);
        e.receipt(AudioReceipt {
            command_sequence: 3,
            target_sample: "480".into(),
            placement: Placement::Active,
        })
        .unwrap();
        pause(&mut e);
        let mut r = request();
        r.commands = vec![first.intent(), next.intent()];
        e.reconcile(r.clone(), 20).unwrap();
        let ack = ack(&mut e, 21);
        assert_eq!(ack.count, 1);
        e.ack(ack, 22).unwrap();
        let reply = e.outcome(&r.batch_id, 1, 23).unwrap().unwrap();
        assert!(matches!(
            reply.dispositions[0],
            LevelDisposition::Settled {
                applied_as_requested: None,
                ..
            }
        ));
        assert_eq!(reply.ledger.pending[0].intent.command_sequence, 3);
    }
    #[test]
    fn final_pcm_ack_removes_only_actually_committed_controls_from_terminal_ledger() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        e.sent(1);
        e.submit(level(2, false), 0).unwrap();
        e.sent(2);
        e.context
            .stop
            .as_ref()
            .unwrap()
            .store(true, Ordering::Release);
        e.tick(1);
        assert_eq!(e.terminal.len(), 2);
        let mut status = e.status.clone();
        status.applied_command_sequence = 1;
        status.accepted_command_sequence = 2;
        e.update(status, 2);
        assert_eq!(e.terminal.len(), 1);
        assert_eq!(e.terminal[0].command_sequence, 2);
        assert_eq!(e.settled.back().unwrap().intent.command_sequence, 1);
    }
    #[test]
    fn replacement_review_token_binds_effective_prefix_but_not_meter_ticks() {
        let mut e = engine();
        pause(&mut e);
        let mut context = e.context.clone();
        context.owner_token = Some("4".repeat(32));
        context.control_revision += 1;
        e.set_context(context, 20);
        let first = e.review.clone();
        let mut status = e.status.clone();
        status.sequence += 1;
        status.mixed.peak = 0.25;
        e.update(status, 21);
        assert_eq!(first, e.review);
        let mut status = e.status.clone();
        status.render_level.gain = 0.5;
        e.update(status, 22);
        assert_ne!(first, e.review);
        let mut r = request();
        r.owner_token = "4".repeat(32);
        r.mode = ReconcileMode::AuthoritativeOnly;
        r.replacement_review_token = first;
        r.acknowledge_lost_local_intent = Some(true);
        assert_eq!(
            e.reconcile(r, 23),
            Err(FailureCode::ReplacementReviewRequired)
        );
    }
    #[test]
    fn exhausted_token_counter_never_wraps_or_admits_a_prefix() {
        let mut e = engine();
        pause(&mut e);
        e.nonce = MAX_SAFE;
        assert_eq!(e.next_token(), Err(FailureCode::CounterExhausted));
        assert_eq!(e.nonce, MAX_SAFE);
        let mut r = request();
        r.commands = vec![level(1, true).intent()];
        assert_eq!(e.reconcile(r, 20), Err(FailureCode::CounterExhausted));
        assert!(e.pending.is_empty());
        assert_eq!(e.accepted, 0);
        assert!(e.batch.is_none());
    }
    #[test]
    fn maximum_escaped_legacy_status_and_64_entry_reply_fit_actual_public_bound() {
        let mut e = engine();
        pause(&mut e);
        e.config.session_id = "s".repeat(80);
        e.config.source_generation = "g".repeat(80);
        e.context.session_id = Some(e.config.session_id.clone());
        e.status.session_id = e.config.session_id.clone();
        e.status.source_generation = e.config.source_generation.clone();
        e.status.message = Some("\0".repeat(4096));
        e.cutoff = Some(u64::MAX);
        e.pause_revision = MAX_SAFE;
        e.context.pause_revision = MAX_SAFE;
        let mut r = request();
        r.session_id = e.config.session_id.clone();
        r.source_generation = e.config.source_generation.clone();
        r.pause_revision = MAX_SAFE;
        r.commands = (0..64)
            .map(|n| LevelIntent {
                command_sequence: MAX_SAFE - 63 + n,
                render_level: MixLevel {
                    gain: 1.2345678,
                    muted: n % 2 == 1,
                },
                microphone_level: MixLevel {
                    gain: 0.12345678,
                    muted: false,
                },
            })
            .collect();
        e.reconcile(r.clone(), 20).unwrap();
        let ack = ack(&mut e, 21);
        e.ack(ack, 22).unwrap();
        let reply = e.outcome(&r.batch_id, 1, 23).unwrap().unwrap();
        let result = ControlResult::success(reply).unwrap();
        assert!(serde_json::to_vec(&result).unwrap().len() < RESULT_BYTES);
        let failure =
            ControlResult::<ReconcileReply>::failure(e.failure(FailureCode::StaleOwner, None));
        assert!(serde_json::to_vec(&failure).unwrap().len() < RESULT_BYTES);
    }
    #[test]
    fn paused_ack_cannot_omit_or_duplicate_a_protected_received_identity() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        e.sent(1);
        e.receipt(AudioReceipt {
            command_sequence: 1,
            target_sample: "480".into(),
            placement: Placement::Active,
        })
        .unwrap();
        let mut c = e.context.clone();
        c.phase = "pausing".into();
        c.control_revision += 1;
        c.pause_revision = 1;
        e.set_context(c, 1);
        assert_eq!(
            e.pause_seal(1, 480, vec![]),
            Err(FailureCode::ProtocolFailure)
        );
        assert!(e.cutoff.is_none());
        let host = e.pending[0].value.clone();
        assert_eq!(
            e.pause_seal(1, 480, vec![host.clone(), host]),
            Err(FailureCode::ProtocolFailure)
        );
        assert_eq!(e.pending.len(), 1);
    }
    #[test]
    fn pause_barrier_pumps_unsent_prefix_but_never_retries_after_transport_acceptance() {
        let mut e = engine();
        e.submit(level(1, true), 0).unwrap();
        let mut c = e.context.clone();
        c.phase = "pausing".into();
        c.control_revision += 1;
        c.pause_revision = 1;
        e.set_context(c, 1);
        assert_eq!(e.ordinary().len(), 1);
        e.sent(1);
        assert!(e.ordinary().is_empty());
        assert_eq!(e.accepted_sequence(), 1);
    }
    #[test]
    fn serialized_history_budget_evicts_receipts_without_truncating_current_64_intent_prefix() {
        let mut e = engine();
        e.config.session_id = "s".repeat(80);
        e.config.source_generation = "g".repeat(80);
        e.context.session_id = Some(e.config.session_id.clone());
        e.status.session_id = e.config.session_id.clone();
        e.status.source_generation = e.config.source_generation.clone();
        e.status.message = Some("\0".repeat(4096));
        let base = MAX_SAFE - 12 * 64;
        for cycle in 0..12u64 {
            if cycle > 0 {
                let mut status = e.status.clone();
                status.applied_command_sequence = e.accepted;
                e.update(status, cycle * 6000);
                let mut c = e.context.clone();
                c.control_revision += 1;
                c.phase = "paused".into();
                e.set_context(c.clone(), cycle * 6000);
                c.phase = "recording".into();
                e.set_context(c, cycle * 6000);
            }
            pause(&mut e);
            let mut r = request();
            r.session_id = e.config.session_id.clone();
            r.source_generation = e.config.source_generation.clone();
            r.pause_revision = e.pause_revision;
            e.cutoff = Some(u64::MAX / 480 * 480);
            r.batch_id = format!("{:032x}", 100 + cycle);
            r.commands = (1..=64)
                .map(|n| {
                    let mut intent = level(base + cycle * 64 + n, n % 2 == 1).intent();
                    intent.render_level.gain = 1.2345678;
                    intent.microphone_level.gain = 0.12345678;
                    intent
                })
                .collect();
            let now = cycle * 6000;
            e.reconcile(r.clone(), now).unwrap();
            e.tick(now + 5000);
            e.retry(
                ReconcileRetryRequest {
                    request: r.clone(),
                    operation_id: format!("{:032x}", 1000 + cycle),
                    expected_timed_out_attempt: 1,
                },
                now + 5001,
            )
            .unwrap();
            let ack = ack(&mut e, now + 5002);
            e.ack(ack, now + 5003).unwrap();
            assert_eq!(e.pending.len(), 64);
            assert_eq!(
                e.pending[0].value.intent.command_sequence,
                base + cycle * 64 + 1
            );
            assert_eq!(
                e.pending[63].value.intent.command_sequence,
                base + cycle * 64 + 64
            );
            assert!(e.history.len() <= BATCH_HISTORY_LIMIT);
            assert!(e.operation_count() <= OPERATION_HISTORY_LIMIT);
            assert!(e.cached_bytes() <= CACHE_BYTES);
            assert!(e.outcome(&r.batch_id, 1, now + 5004).unwrap().is_err());
        }
        // Legal maximum-width receipts remain within the production cap before
        // count eviction. Exercise the exact production eviction loop at a
        // private budget one byte below this measured, valid serialized cache.
        let budget = e.cached_bytes() - 1;
        let old_count = e.history.len();
        let oldest = e.history.front().unwrap().request.batch_id.clone();
        let batch = e.batch.as_ref().unwrap().request.clone();
        let prefix = e.ledger().unwrap().pending;
        e.evict_historical_bytes(budget);
        assert_eq!(e.history.len(), old_count - 1);
        assert!(e.cached_bytes() <= budget);
        assert_ne!(e.history.front().unwrap().request.batch_id, oldest);
        assert_eq!(e.batch.as_ref().unwrap().request, batch);
        assert_eq!(e.ledger().unwrap().pending, prefix);
        assert_eq!(e.pending.len(), 64);
        assert!(matches!(
            e.outcome(&batch.batch_id, 1, 72000),
            Some(Err(FailureCode::ReconciliationTimeout))
        ));
    }
}
