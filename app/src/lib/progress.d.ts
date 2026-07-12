export const ACTIVE_PROGRESS_DISPLAY_CAP: 0.99;
export const FINALIZING_PROGRESS_THRESHOLD: 0.999;
export const ENCODE_PROGRESS_PHASES: readonly ["copying", "encoding", "finalizing"];

export type EncodeProgressPhase = "copying" | "encoding" | "finalizing";

export interface EncodeProgressState {
  attemptId: number | null;
  jobId: number | null;
  phase: EncodeProgressPhase;
  stepIndex: number;
  stepCount: number;
  pass: number;
  totalPasses: number;
  passPct: number;
  overallPct: number;
}

export interface EncodeProgressEvent {
  attemptId: number;
  jobId: number;
  phase: EncodeProgressPhase;
  stepIndex: number;
  stepCount: number;
  pass: number;
  totalPasses: number;
  passPct: number;
  overallPct: number;
}

export interface ProgressUiState {
  value: number;
  percent: number;
  isFinalizing: boolean;
  phase: EncodeProgressPhase;
  label: string;
  valueText: string;
}

export function clampProgress(value: unknown): number;
export function createEncodeProgressState(): EncodeProgressState;
export function reduceEncodeProgress(
  previous: EncodeProgressState | null | undefined,
  payload: Partial<EncodeProgressEvent> | null | undefined,
): EncodeProgressState;
export function getActiveProgressUi(rawProgress: unknown | EncodeProgressState, isActive: boolean): ProgressUiState;
