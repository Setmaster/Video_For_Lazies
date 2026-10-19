import type { RecordingSnapshot, RecordingSource, RecordingRegion } from "./recording";
export function isRecordingActive(phase: string): boolean;
export function acceptRecordingSnapshot(current: RecordingSnapshot, incoming: RecordingSnapshot): RecordingSnapshot;
export function isRecordingRegionWithinDisplay(region: RecordingRegion | null, display: RecordingSource | undefined): boolean;
export function recordingSourceProblem(region: RecordingRegion | null, display: RecordingSource | undefined): string | null;
export function formatRecordingDuration(milliseconds: number): string;
