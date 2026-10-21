import type { AudioPreviewAsset } from "./audioTrackPreview";
import type { AudioTrackDescriptor } from "./types";
export function isAudioSelection(value: unknown): value is { audioStreamIndex: number | null; audioSourceIdentity: string | null };
export function validateAudioPreviewAsset(asset: AudioPreviewAsset, request: { requestId: string; sourceIdentity: string; streamIndex: number }): AudioPreviewAsset;
export function audioTrackLabel(track: AudioTrackDescriptor): string;
