import type { CaptureSource, RecordingSnapshot, RecordingSource, RecordingRegion, RecordingWindow } from "./recording";
export function isCaptureSource(source: unknown): source is CaptureSource;
export function captureSourceKey(source: CaptureSource | null): string;
export function recordingWindowProblem(windowId: string, windows: RecordingWindow[]): string | null;
export function isRecordingActive(phase: string): boolean;
export function acceptRecordingSnapshot(current: RecordingSnapshot, incoming: RecordingSnapshot): RecordingSnapshot;
export function isRecordingRegionWithinDisplay(region: RecordingRegion | null, display: RecordingSource | undefined): boolean;
export function recordingSourceProblem(region: RecordingRegion | null, display: RecordingSource | undefined): string | null;
export function formatRecordingDuration(milliseconds: number): string;
