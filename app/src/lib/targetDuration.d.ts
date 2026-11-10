import type { EncodeRequest } from "./types";
export const MIN_TARGET_SPEED: number;
export const MAX_TARGET_SPEED: number;
export function validTargetDuration(value: unknown): value is number;
export function resolveTargetSpeed(retained: number, target: number, multiplier?: number, fps?: number | null): { effectiveSpeed: number; estimatedOutputDurationS: number } | null;
export interface TargetDurationPlan {
  targetDurationS: number | null; calculatedSpeed: number | null; effectiveSpeed: number | null;
  estimatedOutputDurationS: number | null; retainedDurationS: number | null;
  speedLimited: boolean; pending: boolean; waitingForCleanup: boolean; waitingForTimeline: boolean; error: string | null;
}
export function planTargetDuration(options?: {
  enabled?: boolean; targetS?: string | number | null; sourceDurationS?: number | null;
  trimStartS?: string | number; trimEndS?: string | number | null;
  loopVideo?: boolean; format?: string; manualSpeed?: string | number; cleanupPending?: boolean; timelinePending?: boolean; interpolationTargetFps?: number | null;
}): TargetDurationPlan;
export const TARGET_REMOVE_SAMPLE_REASON: string;
export const TARGET_NUT_SAMPLE_REASON: string;
export function targetSampleBlockingReason(options?: { enabled?: boolean; format?: string; interpolation?: import("./types").InterpolationSettings | null; sourceFormat?: string | null }): string | null;
export function prepareTargetSample(request: EncodeRequest, fullPlan: TargetDurationPlan): EncodeRequest;
export function targetDurationSummary(plan: TargetDurationPlan): string | null;
