import type { EncodeRequest, FastTrimConsent, FastTrimInspection, TrimMode, TrimResult } from "./types";

export type FastTrimUiPhase = "idle" | "checking" | "ready" | "blocked" | "stale" | "error";

export interface FastTrimUiState {
  phase: FastTrimUiPhase;
  requestId: number;
  fingerprint: string | null;
  inspection: FastTrimInspection | null;
  error: string | null;
  acceptedConfirmationToken: string | null;
}

export function fastTrimRequestFingerprint(request: EncodeRequest): string | null;
export function createFastTrimState(): FastTrimUiState;
export function beginFastTrimCheck(state: FastTrimUiState, fingerprint: string, requestId: number): FastTrimUiState;
export function settleFastTrimCheck(
  state: FastTrimUiState,
  fingerprint: string,
  requestId: number,
  inspection: FastTrimInspection,
): FastTrimUiState;
export function failFastTrimCheck(
  state: FastTrimUiState,
  fingerprint: string,
  requestId: number,
  message: string,
): FastTrimUiState;
export function invalidateFastTrimState(state: FastTrimUiState, message?: string): FastTrimUiState;
export function acceptFastTrimBounds(state: FastTrimUiState, accepted: boolean): FastTrimUiState;
export function fastTrimStateMatches(state: FastTrimUiState, fingerprint: string | null): boolean;
export function fastTrimStateForPresentation(state: FastTrimUiState, fingerprint: string | null): FastTrimUiState;
export function fastTrimStateIsAccepted(state: FastTrimUiState, fingerprint: string | null): boolean;
export function fastTrimConsentForRequest(state: FastTrimUiState, fingerprint: string | null): FastTrimConsent | null;
export function fastTrimConsentsMatch(left: FastTrimConsent | null | undefined, right: FastTrimConsent | null | undefined): boolean;
export function fastTrimDurationFromRequest(request: EncodeRequest | null | undefined): number | null;
export function fastTrimEffectiveDurationS(inspection: FastTrimInspection | null): number | null;
export function formatFastTrimTimeUs(value: number | null | undefined): string;
export function formatFastTrimDeltaUs(value: number | null | undefined): string;
export function fastTrimModeLabel(mode: TrimMode | null | undefined): string;
export function summarizeFastTrimInspection(inspection: FastTrimInspection | null): string;
export function summarizeTrimResult(result: TrimResult | null | undefined): string | null;
export function pathFreeFastTrimMessage(error: unknown, paths?: string[]): string;
