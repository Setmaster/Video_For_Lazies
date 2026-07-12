import type { UpdatePhase, UpdateProgressEvent } from "./types";

export const UPDATE_PHASES: readonly UpdatePhase[];

export interface UpdateProgressState {
  operationId: string | null;
  phase: UpdatePhase | null;
  completedBytes: number | null;
  totalBytes: number | null;
  message: string | null;
}

export interface UpdateProgressUi {
  phase: UpdatePhase | null;
  label: string;
  determinate: boolean;
  percent: number | null;
  valueText: string;
}

export function createUpdateProgressState(): UpdateProgressState;
export function reduceUpdateProgress(
  previous: UpdateProgressState | null | undefined,
  event: Partial<UpdateProgressEvent> | null | undefined,
): UpdateProgressState;
export function getUpdateProgressUi(state: UpdateProgressState | null | undefined): UpdateProgressUi;
