export const ACTIVE_PROGRESS_DISPLAY_CAP: 0.99;
export const FINALIZING_PROGRESS_THRESHOLD: 0.999;

export interface ProgressUiState {
  value: number;
  percent: number;
  isFinalizing: boolean;
  label: string;
}

export function clampProgress(value: unknown): number;
export function getActiveProgressUi(rawProgress: unknown, isActive: boolean): ProgressUiState;
