import type { RecordingPreviewSnapshot } from "./recordingPreview";
export type PreviewFrame = { previewId: string; configEpoch: number; sequence: number; width: number; height: number; capturedAtQpc100ns: string; bmp: ArrayBuffer };
export const INITIAL_PREVIEW: RecordingPreviewSnapshot;
export function isPreviewLive(phase: string): boolean;
export function acceptPreviewSnapshot(current: RecordingPreviewSnapshot, incoming: RecordingPreviewSnapshot): RecordingPreviewSnapshot;
export function decodePreviewEnvelope(buffer: ArrayBuffer): PreviewFrame | null;
export function frameMatchesPreview(frame: PreviewFrame | null, snapshot: RecordingPreviewSnapshot, afterSequence?: number): boolean;
