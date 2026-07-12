export const STRICT_FIT_MAX_PLANS: 4;
export const STRICT_FIT_REDUCED_AUDIO_KBPS: 32;
export const STRICT_FIT_FRAME_RATE_CAP_FPS: 30;
export const STRICT_FIT_MAX_EDGE_TIERS: readonly [1280, 960, 720, 540, 360];

export type SizeTargetStatus = "met" | "missed";

export interface ExactTargetResultInput {
  status: SizeTargetStatus;
  targetBytes: number;
  actualBytes: number;
  overshootBytes: number;
}

export interface CanonicalTargetResult extends ExactTargetResultInput {}

export interface TargetResultFormatData extends CanonicalTargetResult {
  met: boolean;
  missed: boolean;
  remainingBytes: number;
  overshootPercent: number;
  targetBytesText: string;
  actualBytesText: string;
  overshootBytesText: string;
  remainingBytesText: string;
  targetMegabytesText: string;
  actualMegabytesText: string;
  overshootPercentText: string;
}

export interface StrictFitOptions {
  strictFit?: boolean;
}

export interface StrictFitPolicyOptions extends StrictFitOptions {
  audioEnabled?: boolean;
}

export interface CanonicalStrictFitOptions {
  strictFit: boolean;
}

export function exactTargetBytesFromMegabytes(value: number): number | null;

export type StrictFitCorrectiveAction =
  | Readonly<{
      kind: "reduceMaxEdge";
      label: string;
      confirmation: string;
      maxEdgePx: number;
      changes: Readonly<{ resizeMode: "maxEdge"; maxEdgePx: number }>;
    }>
  | Readonly<{
      kind: "capFrameRate";
      label: string;
      confirmation: string;
      frameRateCapFps: 30;
      changes: Readonly<{ frameRateCapFps: 30 }>;
    }>
  | Readonly<{
      kind: "removeAudio";
      label: string;
      confirmation: string;
      changes: Readonly<{ audioEnabled: false }>;
    }>
  | Readonly<{
      kind: "enableStrictFit";
      label: string;
      confirmation: string;
      changes: Readonly<{ strictFit: true }>;
    }>;

export interface FitPlanSummaryInput {
  planNumber: number;
  label?: string | null;
  mutations?: readonly unknown[] | null;
  width?: number | null;
  height?: number | null;
  videoBitrateKbps?: number | null;
  audioAction?: string | null;
  audioBitrateKbps?: number | null;
  actualSizeBytes: number;
  status: SizeTargetStatus;
  selected?: boolean;
}

export function canonicalizeStrictFitOptions(
  value?: StrictFitOptions | null,
): Readonly<CanonicalStrictFitOptions>;
export function canonicalizeTargetResult(
  value: ExactTargetResultInput | null | undefined,
): Readonly<CanonicalTargetResult> | null;
export function targetResultFormatData(
  value: ExactTargetResultInput | null | undefined,
): Readonly<TargetResultFormatData> | null;
export function nextLowerStrictFitMaxEdge(
  width: number,
  height: number,
  currentMaxEdgePx?: number | null,
): number | null;
export function strictFitCorrectiveActions(options: {
  targetResult: ExactTargetResultInput | null;
  format: string | null;
  width?: number | null;
  height?: number | null;
  currentMaxEdgePx?: number | null;
  plannedFrameRateFps?: number | null;
  hasAudio?: boolean;
  audioEnabled?: boolean;
  strictFit?: boolean;
}): readonly StrictFitCorrectiveAction[];
export function summarizeStrictFitPolicy(value?: StrictFitPolicyOptions | null): string;
export function summarizeStrictFitPlan(value: FitPlanSummaryInput | null | undefined): string | null;
export function summarizeTargetResult(
  value: ExactTargetResultInput | null | undefined,
): string | null;
