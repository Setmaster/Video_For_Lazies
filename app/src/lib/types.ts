export type OutputFormat = "mp4" | "webm" | "mp3";
export type VideoCodecPreference = "auto" | "h264" | "mpeg4" | "vp9" | "vp8";
export type VideoQualityPreference = "auto" | "smaller" | "balanced" | "higher";
export type EncodeSpeedPreference = "auto" | "faster" | "balanced" | "smaller";
export type AudioChannelPreference = "auto" | "stereo" | "mono";
export type ResizeMode = "source" | "maxEdge" | "custom";

export interface VideoProbe {
  durationS: number;
  width: number;
  height: number;
  frameRate?: number | null;
  hasAudio: boolean;
}

export interface Trim {
  startS: number;
  endS?: number | null;
}

export interface Crop {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface ColorAdjust {
  brightness: number;
  contrast: number;
  saturation: number;
}

export interface ResizeSettings {
  mode: ResizeMode;
  maxEdgePx?: number | null;
  widthPx?: number | null;
  heightPx?: number | null;
}

export interface EncodeRequest {
  inputPath: string;
  outputPath: string;
  format: OutputFormat;
  title?: string | null;
  sizeLimitMb: number;
  audioEnabled: boolean;
  normalizeAudio: boolean;
  stripMetadata: boolean;
  advanced?: AdvancedEncodeSettings | null;

  trim?: Trim | null;
  crop?: Crop | null;
  reverse: boolean;
  speed: number;
  rotateDeg: number;
  resize?: ResizeSettings | null;
  maxEdgePx?: number | null;
  color?: ColorAdjust | null;
  // Imperceptibly perturb the first frame so each export hashes differently
  // (off by default; the Forum recipe enables it). Seed is chosen by the backend.
  perturbFirstFrame: boolean;
  // Play the clip forward then in reverse (a seamless boomerang loop). Doubles
  // the output duration; N/A for audio-only mp3.
  loopVideo: boolean;
}

export interface AdvancedEncodeSettings {
  videoCodec?: VideoCodecPreference | null;
  audioBitrateKbps?: number | null;
  videoQuality?: VideoQualityPreference | null;
  encodeSpeed?: EncodeSpeedPreference | null;
  frameRateCapFps?: number | null;
  audioChannels?: AudioChannelPreference | null;
}

export interface VideoCodecCapability {
  format: OutputFormat;
  value: VideoCodecPreference;
  label: string;
  ffmpegName: string;
  available: boolean;
  isDefault: boolean;
}

export interface EncodeCapabilities {
  videoCodecs: VideoCodecCapability[];
  audioBitrateKbps: number[];
}

export interface EncodeProgressPayload {
  jobId: number;
  pass: number;
  totalPasses: number;
  passPct: number;
  overallPct: number;
}

export interface EncodeFinishedPayload {
  jobId: number;
  ok: boolean;
  outputPath?: string | null;
  outputSizeBytes?: number | null;
  message?: string | null;
  diagnostics?: ExportDiagnostics | null;
}

export interface ExportDiagnostics {
  mode: string;
  videoCodec?: string | null;
  audioCodec?: string | null;
  videoBitrateKbps?: number | null;
  audioBitrateKbps?: number | null;
  requestedSizeBytes?: number | null;
  actualSizeBytes?: number | null;
  passes: number;
  attempts: number;
  audioRemovedForSizeTarget: boolean;
  commandPreview: string;
}

export interface AppSmokeConfig {
  inputPath: string;
  outputPath: string;
  statusPath: string;
  format: OutputFormat;
  sizeLimitMb: number;
  trimStartS: number;
  trimEndS?: number | null;
  resizeMode?: ResizeMode | null;
  resizeMaxEdgePx?: number | null;
  resizeWidthPx?: number | null;
  resizeHeightPx?: number | null;
  skipPreviewInteractions?: boolean | null;
  perturbFirstFrame?: boolean | null;
  loopVideo?: boolean | null;
}

export interface AppSmokeStatus {
  stage: string;
  ok?: boolean | null;
  message?: string | null;
  outputPath?: string | null;
  outputSizeBytes?: number | null;
  trimStartS?: number | null;
  trimEndS?: number | null;
  expectedDurationS?: number | null;
  stageHistory?: string[] | null;
}

export interface UpdateNotes {
  title: string;
  summary: string;
  url: string;
}

export interface UpdateArtifactInfo {
  target: string;
  fileName: string;
  sizeBytes: number;
  url: string;
  sha256: string;
}

export interface UpdateCheckResponse {
  status: "available" | "current" | "skipped";
  currentVersion: string;
  latestVersion?: string | null;
  releaseUrl?: string | null;
  notes?: UpdateNotes | null;
  artifact?: UpdateArtifactInfo | null;
  checkedAtMs: number;
  reason?: string | null;
}

export interface UpdateApplyResponse {
  status: "restarting";
  version: string;
  message: string;
}
