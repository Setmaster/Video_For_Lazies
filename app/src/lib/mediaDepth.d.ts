import type { VideoProbe } from "./types";

export const TRANSFORM_MEMORY_WARN_BYTES: number;
export const TRANSFORM_MEMORY_BLOCK_BYTES: number;
export const TRANSFORM_MEMORY_OVERHEAD: number;
export const TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES: number;
export const TRANSFORM_AUDIO_FRAME_OVERHEAD_BYTES: number;
export const TRANSFORM_AUDIO_FRAME_SAMPLES: number;

export function hasNonSquarePixels(probe: VideoProbe | null | undefined): boolean;
export function effectiveSampleAspectRatio(probe: VideoProbe | null | undefined, manualRotateDeg?: number): number;
export function squarePixelDimensions(options: {
  probe: VideoProbe;
  width: number;
  height: number;
  manualRotateDeg?: number;
}): { width: number; height: number };
export function fitMaxEdgeDimensions(width: number, height: number, maxEdge: number): { width: number; height: number };
export function fitMaxEdgeDisplayDimensions(options: {
  probe: VideoProbe;
  width: number;
  height: number;
  maxEdge: number;
  manualRotateDeg?: number;
}): { width: number; height: number };
export function sourceRotationBlockingReason(probe: VideoProbe | null | undefined): string | null;
export function plannedDimensionBlockingReason(width: number, height: number, maxDimension?: number): string | null;
export function videoCodecDimensionLimit(codec: string | null | undefined): number;
export function codecOutputDimensionBlockingReason(
  codec: string | null | undefined,
  width: number,
  height: number,
): string | null;
export function trimRequestIsActive(options: {
  startS?: number;
  endS?: number | null;
  durationS?: number | null;
}): boolean;
export function encodedOutputDimensions(
  width: number,
  height: number,
  requiresEncode: boolean,
): { width: number; height: number };
export function timelineSpeedChanges(value: number | string): boolean;
export function minimumVideoBitrateKbps(options: {
  codec: string;
  width: number;
  height: number;
  sourceFrameRate?: number | null;
  speed?: number;
  frameRateCapFps?: number | null;
}): number;
export function sizeTargetRetainsAudio(options: {
  audioRequested: boolean;
  hasAudio: boolean;
  sizeLimitEnabled: boolean;
  totalKbps: number | null;
  codec: string;
  width: number;
  height: number;
  sourceFrameRate?: number | null;
  speed?: number;
  frameRateCapFps?: number | null;
}): boolean;
export function classifyColorSource(probe: VideoProbe | null | undefined):
  | { kind: "standard"; label: string }
  | { kind: "convertible"; label: string }
  | { kind: "unsupported"; reason: string };
export function estimateTransformMemory(options: {
  probe: VideoProbe;
  reverse: boolean;
  loopVideo: boolean;
  trimStartS?: number;
  trimEndS?: number | null;
  speed?: number;
  frameRateCapFps?: number | null;
  width: number;
  height: number;
  decodedVideoBytesPerPixel?: number | null;
  normalizeAudio?: boolean;
  audioEnabled?: boolean;
  videoEnabled?: boolean;
}): {
  bytes: number | null;
  severity: "ok" | "warning" | "blocked";
  reason: string | null;
  videoBytes: number | null;
  audioBytes: number | null;
};
