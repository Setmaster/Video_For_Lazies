export type MixLevel = { gain: number; muted: boolean };
export type RecordingAudioConfig = {
  render: { kind: "off" } | { kind: "system"; deviceId: string } | { kind: "application"; applicationId: string };
  microphone: { deviceId: string; suppression: "off" | "save" } | null;
  renderLevel: MixLevel;
  microphoneLevel: MixLevel;
};
export type AudioApplication = { id: string; name: string; processName: string; scope: "processTree"; available: boolean; reason: string | null };
export type AudioMeter = { state: "off" | "pending" | "active" | "silent" | "muted" | "failed"; peak: number; rms: number; overload: boolean };
export type AudioStatus = {
  sessionId: string; sourceGeneration: string; sequence: number;
  acceptedCommandSequence: number; appliedCommandSequence: number; appliedAtSample: string; rejectedCommandSequence: number | null;
  renderLevel: MixLevel; microphoneLevel: MixLevel;
  render: AudioMeter; microphone: AudioMeter; mixed: AudioMeter; message: string | null;
};
export type AudioControlSubmission = { sessionId: string; sourceGeneration: string; acceptedCommandSequence: number };
export type AudioLevels = { renderLevel: MixLevel; microphoneLevel: MixLevel };
export type AudioControl = AudioLevels & { commandSequence: number; submitted: boolean; muteEdge: boolean };
