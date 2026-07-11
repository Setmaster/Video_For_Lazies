export type OutputFormat = "mp4" | "webm" | "mp3";
export type VideoCodecPreference = "auto" | "h264" | "mpeg4" | "vp9" | "vp8";
export type VideoQualityPreference = "auto" | "smaller" | "balanced" | "higher";
export type EncodeSpeedPreference = "auto" | "faster" | "balanced" | "smaller";
export type AudioChannelPreference = "auto" | "stereo" | "mono";
export type ResizeMode = "source" | "maxEdge" | "custom";
export type StreamAction = "copy" | "encode" | "drop";
export type ColorPolicy = "auto" | "standardSdr";
export type DynamicRange = "sdr" | "hdr10" | "hlg" | "dolbyVision" | "unknown";

export interface Rational {
  numerator: number;
  denominator: number;
}

export interface StreamDispositions {
  default: boolean;
  original: boolean;
  comment: boolean;
  forced: boolean;
  hearingImpaired: boolean;
  visualImpaired: boolean;
  attachedPic: boolean;
  timedThumbnails: boolean;
  stillImage: boolean;
}

export interface VideoProbe {
  durationS: number;
  width: number;
  height: number;
  frameRate?: number | null;
  hasAudio: boolean;
  sourceFormat?: string | null;
  videoStreamIndex: number;
  videoCodec?: string | null;
  videoIsDefault: boolean;
  audioStreamIndex?: number | null;
  audioCodec?: string | null;
  audioIsDefault: boolean;
  codedWidth?: number | null;
  codedHeight?: number | null;
  rotationDeg?: number | null;
  unsupportedRotationDeg?: number | null;
  pixelFormat?: string | null;
  bitDepth?: number | null;
  colorRange?: string | null;
  colorPrimaries?: string | null;
  colorTransfer?: string | null;
  colorSpace?: string | null;
  dynamicRange?: DynamicRange | null;
  sampleAspectRatio?: Rational | null;
  displayAspectRatio?: Rational | null;
  attachedPictureCount?: number | null;
  selectedVideoDispositions?: StreamDispositions | null;
  audioSampleRate?: number | null;
  audioChannels?: number | null;
  audioSampleFormat?: string | null;
  decodedVideoBytesPerPixel?: number | null;
  decodedAudioBytesPerSample?: number | null;
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
  colorPolicy: ColorPolicy;
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
  strictFit: boolean;
  strictFitAllowAudioRemoval: boolean;
  subtitlePath?: string | null;
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
  ffmpegVersion?: string | null;
  contractSchemaVersion?: number | null;
  features?: EncodeFeatureCapability[] | null;
}

export interface EncodeFeatureCapability {
  name: string;
  available: boolean;
  releaseRequired: boolean;
  missingEncoders: string[];
  missingFilters: string[];
}

export interface EncodeProgressPayload {
  attemptId: number;
  jobId: number;
  pass: number;
  totalPasses: number;
  passPct: number;
  overallPct: number;
}

export interface EncodeFinishedPayload {
  attemptId: number;
  jobId: number;
  ok: boolean;
  outputPath?: string | null;
  outputSizeBytes?: number | null;
  targetResult?: TargetResult | null;
  message?: string | null;
  diagnostics?: ExportDiagnostics | null;
}

export type SizeTargetStatus = "met" | "missed";

export interface FitPlanResult {
  planNumber: number;
  label: string;
  mutations: string[];
  width?: number | null;
  height?: number | null;
  videoBitrateKbps?: number | null;
  audioAction?: StreamAction | null;
  audioBitrateKbps?: number | null;
  actualSizeBytes: number;
  status: SizeTargetStatus;
  ffmpegInvocations: number;
  selected: boolean;
}

export interface TargetResult {
  status: SizeTargetStatus;
  targetBytes: number;
  actualBytes: number;
  overshootBytes: number;
  strictFit: boolean;
  selectedPlanNumber: number;
  plans: FitPlanResult[];
}

export interface SubtitleInspection {
  cueCount: number;
  firstCueStartS: number;
  lastCueEndS: number;
}

export interface ExportDiagnostics {
  mode: string;
  videoAction?: StreamAction | null;
  audioAction?: StreamAction | null;
  sourceFormat?: string | null;
  sourceVideoCodec?: string | null;
  sourceAudioCodec?: string | null;
  videoCodec?: string | null;
  audioCodec?: string | null;
  videoBitrateKbps?: number | null;
  audioBitrateKbps?: number | null;
  requestedSizeBytes?: number | null;
  actualSizeBytes?: number | null;
  passes: number;
  attempts: number;
  audioRemovedForSizeTarget: boolean;
  subtitleBurnedIn: boolean;
  subtitleCueCount?: number | null;
  copyFallbackReason?: string | null;
  colorAction?: string | null;
  sarAction?: string | null;
  reverseBufferEstimateBytes?: number | null;
  reverseBufferAction?: string | null;
  failureStage?: string | null;
  failureReason?: string | null;
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
  workflowQueueExport?: boolean | null;
  g5QueueTargetMiss?: boolean | null;
  perturbFirstFrame?: boolean | null;
  loopVideo?: boolean | null;
  colorPolicy?: ColorPolicy | null;
  reverse?: boolean | null;
  strictFit?: boolean | null;
  strictFitAllowAudioRemoval?: boolean | null;
  // Launch-only input. It is never copied into AppSmokeStatus.
  subtitlePath?: string | null;
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
  targetResult?: TargetResult | null;
  diagnostics?: ExportDiagnostics | null;
  queueOutcomeKind?: "done" | "target-missed" | null;
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
