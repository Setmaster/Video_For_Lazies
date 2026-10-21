import type { AudioControl, AudioLevels, AudioStatus, MixLevel, RecordingAudioConfig } from "./recordingAudioTypes";
export const DEFAULT_AUDIO: RecordingAudioConfig;
export function isMixLevel(value: unknown): value is MixLevel;
export function isRecordingAudioConfig(value: unknown): value is RecordingAudioConfig;
export function isAudioStatus(value: unknown): value is AudioStatus;
export function acceptAudioStatus(current: AudioStatus | null, incoming: AudioStatus): AudioStatus;
export function sameMuteState(a: AudioLevels, b: AudioLevels): boolean;
export function enqueueAudioControl(queue: AudioControl[], levels: AudioLevels, sequence: number, previous: AudioLevels): AudioControl[];
