import type { ExportDiagnostics, TargetResult } from "./types";

export interface EncodeAttemptOutcomeDetails {
  outputPath?: string;
  outputSizeBytes?: number;
  targetResult?: TargetResult;
  diagnostics?: ExportDiagnostics;
}

export type EncodeAttemptState =
  | { kind: "idle" }
  | { kind: "starting"; attemptId: number }
  | { kind: "running"; attemptId: number; jobId: number }
  | { kind: "cancelling"; attemptId: number; jobId: number }
  | ({
      kind: "succeeded";
      attemptId: number;
      jobId: number;
      completedAtMs: number;
      message?: string;
    } & EncodeAttemptOutcomeDetails)
  | ({
      kind: "target-missed";
      attemptId: number;
      jobId: number;
      message: string;
      completedAtMs: number;
    } & EncodeAttemptOutcomeDetails)
  | ({
      kind: "failed";
      attemptId: number;
      jobId: number | null;
      message: string;
      completedAtMs: number;
    } & EncodeAttemptOutcomeDetails)
  | ({
      kind: "cancelled";
      attemptId: number;
      jobId: number;
      message: string;
      completedAtMs: number;
    } & EncodeAttemptOutcomeDetails);

export interface PendingEncodeIdentity {
  attemptId: number;
  jobId: number | null;
  outputPath?: string;
}

export interface EncodeEventIdentity {
  attemptId: number;
  jobId: number;
}

export interface EncodeFinishedIdentity extends EncodeEventIdentity {
  ok: boolean;
  outputPath?: string | null;
  outputSizeBytes?: number | null;
  targetResult?: TargetResult | null;
  message?: string | null;
  diagnostics?: ExportDiagnostics | null;
}

export interface EncodeAttemptPresentation {
  isActive: boolean;
  isSuccess: boolean;
  isFailure: boolean;
  isCancelled: boolean;
  isTargetMissed: boolean;
  kicker: string | null;
  summary: string | null;
  message: string | null;
}

export interface EncodeCoordinatorResult<T extends PendingEncodeIdentity> {
  accepted: boolean;
  pending: T | null;
  state: EncodeAttemptState;
  context: T | null;
}

export function createIdleEncodeAttempt(): EncodeAttemptState;
export function beginEncodeAttempt(attemptId: number): EncodeAttemptState;
export function bindEncodeAttempt(
  state: EncodeAttemptState,
  attemptId: number,
  jobId: number,
): EncodeAttemptState;
export function bindStartedEncode<T extends PendingEncodeIdentity>(
  pending: T | null,
  state: EncodeAttemptState,
  attemptId: number,
  jobId: number,
): EncodeCoordinatorResult<T>;
export function failEncodeAttemptStart(
  state: EncodeAttemptState,
  attemptId: number,
  message: string,
  completedAtMs: number,
  outputPath?: string,
): EncodeAttemptState;
export function requestEncodeCancellation(
  state: EncodeAttemptState,
  attemptId: number,
  jobId: number,
): EncodeAttemptState;
export function acceptsEncodeEvent(
  pending: PendingEncodeIdentity | null,
  payload: EncodeEventIdentity | null,
): boolean;
export function finishEncodeAttempt(
  state: EncodeAttemptState,
  payload: EncodeFinishedIdentity,
  completedAtMs: number,
): EncodeAttemptState;
export function settleEncodeFinished<T extends PendingEncodeIdentity>(
  pending: T | null,
  state: EncodeAttemptState,
  payload: EncodeFinishedIdentity,
  completedAtMs: number,
): EncodeCoordinatorResult<T>;
export function deriveEncodeAttemptPresentation(
  state: EncodeAttemptState,
): EncodeAttemptPresentation;
