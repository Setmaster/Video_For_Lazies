import type { RecordingPhase } from "./recording";
import type { AudioLevels, AudioStatus } from "./recordingAudioTypes";
export type LevelIntent = AudioLevels & { commandSequence: number };
export type AcceptedIntent = LevelIntent & { targetSample: string | null; placement: "awaitingReceipt" | "active" | "resumeBound" };
export type AudioLedger = { pending: AcceptedIntent[]; terminalUnapplied: LevelIntent[] };
export type LevelDisposition =
  | { kind: "settled"; commandSequence: number; identityVerified: true; appliedAsRequested: null }
  | { kind: "acceptedPending"; commandSequence: number; targetSample: string | null; placement: "awaitingReceipt" | "active" | "resumeBound" }
  | { kind: "deferredNotAccepted"; commandSequence: number; reason: "paused" | "transitioning"; retryAfterRevision: number };
export type PauseSeal = { pauseRevision: number; cutoffSample: string; audio: null | {
  sourceGeneration: string; sealedAcceptedSequence: number;
  fence: { state: "waiting" | "ready" | "replacementReviewRequired"; token: string | null; replacementReviewToken: string | null };
} };
export type TransitionRequest = { ownerToken: string; sessionId: string; operationId: string; expectedControlRevision: number; reconcileFenceToken?: string };
export type PendingTransition = { operationId: string; action: "pause" | "resume"; controlRevision: number };
export type ReconcileRequest = { ownerToken: string; sessionId: string; sourceGeneration: string; pauseRevision: number; batchId: string; commands: LevelIntent[] } & (
  | { mode: "retainedQueue" }
  | { mode: "authoritativeOnly"; replacementReviewToken: string; acknowledgeLostLocalIntent: true }
);
export type ReconcileRetryRequest = ReconcileRequest & { operationId: string; expectedTimedOutAttempt: number };
export type ReconcileReply = { ownerToken: string; sessionId: string; sourceGeneration: string; pauseRevision: number;
  batchId: string; batchDigest: string; fenceToken: string; dispositions: LevelDisposition[]; audioStatus: AudioStatus; ledger: AudioLedger };
export type LevelRequest = LevelIntent & { ownerToken: string; expectedControlRevision: number; sessionId: string; sourceGeneration: string };
export type LevelReply = { ownerToken: string; sessionId: string; sourceGeneration: string; controlRevision: number; disposition: LevelDisposition; audioStatus: AudioStatus; ledger: AudioLedger };
export type FailureCode = "invalidRequest" | "oversizedRequest" | "staleOwner" | "staleSession" | "sourceChanged" | "staleAdmission" |
  "identityConflict" | "staleUnknown" | "batchConflict" | "staleAttempt" | "batchBusy" | "transitionBusy" | "capacity" | "envelopeCapacity" |
  "replacementReviewRequired" | "reconciliationTimeout" | "stageExpired" | "resumeTimeout" | "transitionTimeout" | "acquisitionFailed" |
  "sourceLost" | "protocolFailure" | "sourceUnavailable" | "counterExhausted" | "sessionTerminal" | "audioControlsOff";
export type ControlFailure = {
  ok: false; error: { code: FailureCode; retry: "sameExactToken" | "freshRequest" | "never";
    action: "refreshState" | "retrySynchronization" | "keepPaused" | "stopAndSave" | "restartApp"; commandSequence: number | null; message: string };
  context: { sessionId: string | null; sourceGeneration: string | null; controlRevision: number | null; pauseRevision: number | null; timedOutAttempt: number | null };
  state: { phase: RecordingPhase; audioStatus: AudioStatus | null; ledger: AudioLedger | null; pause: PauseSeal | null };
};
export type ControlResult<T> = { ok: true; value: T } | ControlFailure;
