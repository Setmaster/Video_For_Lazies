export type OutputFormat = "mp4" | "webm" | "mp3";

export interface VideoProbe {
  durationS: number;
  width: number;
  height: number;
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

export interface EncodeRequest {
  inputPath: string;
  outputPath: string;
  format: OutputFormat;
  title?: string | null;
  sizeLimitMb: number;
  audioEnabled: boolean;

  trim?: Trim | null;
  crop?: Crop | null;
  reverse: boolean;
  speed: number;
  rotateDeg: number;
  maxEdgePx?: number | null;
  color?: ColorAdjust | null;
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
}

export interface AppSmokeConfig {
  inputPath: string;
  outputPath: string;
  statusPath: string;
  format: OutputFormat;
  sizeLimitMb: number;
  trimStartS: number;
  trimEndS?: number | null;
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
