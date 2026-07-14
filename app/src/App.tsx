import { useEffect, useEffectEvent, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { Channel, convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm as confirmDialog, open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { CropPixelFields } from "./components/CropPixelFields";
import { ModalDialog } from "./components/ModalDialog";
import { TrimSliderHandle } from "./components/TrimSliderHandle";
import { UserRecipeDialog, type UserRecipeDialogState } from "./components/UserRecipeDialog";
import { VideoCropper, type NormalizedRect, type VideoCropperHandle } from "./components/VideoCropper";
import type {
  AppSmokeConfig,
  AppSmokeG7Evidence,
  AppSmokeMountedProgressSample,
  AppSmokeProgressSample,
  AppSmokeStatus,
  AudioChannelPreference,
  ColorPolicy,
  Crop,
  ColorAdjust,
  EncodeSpeedPreference,
  EncodeCapabilities,
  EncodeFinishedPayload,
  EncodeProgressPayload,
  EncodeRequest,
  ExportDiagnostics,
  OutputFormat,
  ResizeMode,
  StreamAction,
  SubtitleInspection,
  TargetResult,
  UpdateApplyResponse,
  UpdateCheckResponse,
  UpdateProgressEvent,
  UpdatePublicError,
  UpdateStartupResponse,
  VideoCodecPreference,
  VideoQualityPreference,
  VideoProbe,
} from "./lib/types";
import {
  bindWindowFileDrop,
  resolveDroppedVideoAction,
  SUPPORTED_INPUT_EXTENSIONS,
} from "./lib/dropInput";
import {
  createExportQueueState,
  exportQueueClaimsOutputPath,
  exportQueueOutputPaths,
  exportQueueRemainingCapacity,
  getActiveExportQueueItem,
  queuePathIdentity,
  reduceExportQueue,
  summarizeExportQueue,
  type ExportQueueAction,
  type ExportQueueItem,
  type ExportQueueOutcome,
  type ExportQueueState,
} from "./lib/exportQueue";
import { DEFAULT_OUTPUT_FORMAT, DEFAULT_SIZE_LIMIT_MB } from "./lib/defaults";
import { basename, dirname, ensureUniqueOutputPath, extname, formatPathForDisplay, replaceExtension, stem, suggestOutputPath } from "./lib/outputPath";
import { createEncodeProgressState, getActiveProgressUi, reduceEncodeProgress } from "./lib/progress";
import { formatClock } from "./lib/timeFormat";
import {
  acceptsEncodeEvent,
  beginEncodeAttempt,
  bindStartedEncode,
  createIdleEncodeAttempt,
  deriveEncodeAttemptPresentation,
  failEncodeAttemptStart,
  requestEncodeCancellation,
  settleEncodeFinished,
  type EncodeAttemptState,
} from "./lib/encodeAttempt";
import { installEncodeEventListeners } from "./lib/encodeEvents";
import { createUpdateProgressState, getUpdateProgressUi, reduceUpdateProgress } from "./lib/updateState";
import { parsePersistedSettings, serializePersistedSettings } from "./lib/settings";
import {
  EXPORT_RECIPES,
  normalizeRecipeResizeSettings,
  recipeMatchesSettings,
  type ExportRecipe,
  type ExportRecipeSettings,
} from "./lib/exportRecipes";
import {
  USER_RECIPE_STORAGE_KEY,
  createEmptyUserRecipeStore,
  createUserRecipe,
  deleteUserRecipe,
  findMatchingUserRecipe,
  loadUserRecipeStore,
  parseUserRecipeStore,
  persistUserRecipeStore,
  reusableSettingsFromEncodeRequest,
  updateUserRecipe,
  type UserRecipe,
  type UserRecipeStore,
} from "./lib/userRecipes";
import { buildPreviewColorFilter } from "./lib/previewFilter";
import { alignCropRectForEncoding, cropRectToPixels, isFullFramePixelCrop } from "./lib/accessibility";
import {
  classifyColorSource,
  codecOutputDimensionBlockingReason,
  encodedOutputDimensions,
  effectiveFrameRatePlan,
  estimateTransformMemory,
  fitMaxEdgeDisplayDimensions,
  fitMaxEdgeDimensions,
  hasNonSquarePixels,
  sourceRotationBlockingReason,
  squarePixelDimensions,
  timelineSpeedChanges,
  trimRequestIsActive,
} from "./lib/mediaDepth";
import {
  exactTargetBytesFromMegabytes,
  strictFitCorrectiveActions,
  summarizeStrictFitPlan,
  summarizeStrictFitPolicy,
  summarizeTargetResult,
  targetResultFormatData,
  type StrictFitCorrectiveAction,
} from "./lib/strictFit";
import "./App.css";

const SETTINGS_KEY = "vfl:settings:v1";
const SIZE_PRESETS_MB = [4, 10, 25, 50] as const;
const SIZE_PRESET_HINTS: Record<number, string> = {
  4: "Common forum attachment cap",
  10: "Discord free upload limit",
  25: "Gmail attachment limit",
  50: "Discord Nitro Basic / level 2 server boost",
};
const OUTPUT_DIMENSION_MIN_PX = 16;
const OUTPUT_DIMENSION_MAX_PX = 32768;
const AUDIO_BITRATE_PRESETS_KBPS = [96, 128, 192, 256, 320] as const;
const FRAME_RATE_CAP_PRESETS_FPS = [24, 30, 60] as const;
const SIZE_TARGET_EXACTNESS_ERROR = "Size limit is too large to track exactly in bytes. Enter a smaller MB value.";
const ENCODE_EVENT_SETUP_ERROR = "Export event handling could not start. Restart Video For Lazies, then try again. If the problem continues, reinstall the app and report the app version.";
const ENCODE_EVENT_SETUP_SMOKE_ERROR = "Packaged app smoke export event setup failed (code: encode-event-listener-registration). Restart the app, then rerun the packaged smoke.";
const CROP_DETECTION_PUBLIC_ERROR = "Crop detection could not analyze the selected video.";
const VIDEO_CODEC_LABELS: Record<VideoCodecPreference, string> = {
  auto: "Auto",
  h264: "H.264",
  mpeg4: "MPEG-4",
  vp9: "VP9",
  vp8: "VP8",
};
const VIDEO_CODEC_FFMPEG_NAMES: Partial<Record<VideoCodecPreference, string>> = {
  h264: "libx264",
  mpeg4: "mpeg4",
  vp9: "libvpx-vp9",
  vp8: "libvpx",
};
const VIDEO_QUALITY_LABELS: Record<VideoQualityPreference, string> = {
  auto: "Auto",
  smaller: "Smaller file",
  balanced: "Balanced",
  higher: "Higher quality",
};
const ENCODE_SPEED_LABELS: Record<EncodeSpeedPreference, string> = {
  auto: "Auto",
  faster: "Faster",
  balanced: "Balanced",
  smaller: "Smaller file",
};
const AUDIO_CHANNEL_LABELS: Record<AudioChannelPreference, string> = {
  auto: "Auto",
  stereo: "Stereo",
  mono: "Mono",
};
const MIN_WINDOW_WIDTH = 960;
const MIN_WINDOW_HEIGHT = 720;
const TRIM_MIN_GAP_S = 0.05;
const TRIM_DRAG_SNAP_MAX_S = 60;
const TRIM_FINE_NUDGE_S = 0.1;
const TRIM_COARSE_NUDGE_S = 1;
const RECIPE_NOTIFICATION_FADE_START_MS = 3000;
const RECIPE_NOTIFICATION_CLEAR_MS = 3200;
const SMOKE_SUCCESS_STAGE = "success";
const SMOKE_ERROR_STAGE = "error";
const SMOKE_WORKFLOW_SESSION_KEY = "vfl:smoke-workflow:v1";
const SMOKE_STAGE_ORDER = [
  "detected",
  "input-applied",
  "probe-ready",
  "workflow-recipe-ready",
  "workflow-recipe-saved",
  "workflow-queue-ready",
  "workflow-queue-complete",
  "workflow-ready",
  "g7-ui-ready",
  "preview-ready",
  "keyboard-trim-ready",
  "keyboard-trim-incremented",
  "keyboard-trim-complete",
  "keyboard-crop-ready",
  "keyboard-crop-complete",
  "keyboard-modal-ready",
  "keyboard-modal-open",
  "keyboard-complete",
  "accessibility-ready",
  "interaction-ready",
  "encoding",
  "g7-controls-ready",
  "g7-drop-queued",
  "g7-cancel-requested",
] as const;
const APP_VERSION = "2.1.0";
const APP_LINKS = {
  github: "https://github.com/Setmaster/Video_For_Lazies",
  releases: "https://github.com/Setmaster/Video_For_Lazies/releases",
  security: "https://github.com/Setmaster/Video_For_Lazies/security/advisories/new",
} as const;

type RailCardKey = "source" | "recipes" | "output" | "crop" | "transform" | "advanced" | "plan" | "queue";
const DEFAULT_OPEN_CARDS: Record<RailCardKey, boolean> = {
  source: true,
  recipes: true,
  output: true,
  crop: false,
  transform: false,
  advanced: false,
  plan: true,
  queue: false,
};
const SAMPLE_DURATION_CHOICES_S = [5, 10, 30] as const;

type RailCardProps = {
  title: string;
  summary?: string | null;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
};

function RailCard({ title, summary, open, onToggle, children }: RailCardProps) {
  return (
    <section className={`vfl-card ${open ? "is-open" : ""}`}>
      <button type="button" className="vfl-card-head" aria-expanded={open} onClick={onToggle}>
        <span className="vfl-card-title">{title}</span>
        {summary ? <span className="vfl-card-summary">{summary}</span> : null}
        <svg className="vfl-card-chevron" viewBox="0 0 16 16" aria-hidden="true">
          <path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" strokeWidth="1.6" />
        </svg>
      </button>
      {open ? <div className="vfl-card-body">{children}</div> : null}
    </section>
  );
}

type TrimFocusTarget = "preview" | "start" | "end";
type TrimTimeline = {
  start: number;
  end: number;
  hasCustomEnd: boolean;
  minGap: number;
};
type ComposeShortcutAction =
  | { kind: "toggle-playback" }
  | { kind: "apply-trim-start" }
  | { kind: "apply-trim-end" }
  | { kind: "nudge-timeline"; deltaS: number };

type StartEncodeResult =
  | { ok: true; jobId: number }
  | { ok: false; message: string };

type SmokeAccessibilityResult =
  | { ok: true; message: string }
  | { ok: false; message: string };

type SmokeWorkflowResult = SmokeAccessibilityResult;

type SmokeInteractionResult =
  | { ok: false; message: string }
  | {
      ok: true;
      message: string;
      trimStartS: number;
      trimEndS: number;
      expectedDurationS: number;
    };

type SmokeG7Operation = NonNullable<AppSmokeConfig["g7Operation"]>;

function createSmokeG7Evidence(operation: SmokeG7Operation): AppSmokeG7Evidence {
  return {
    operation,
    resetDialogRole: null,
    resetCancelFocused: false,
    resetCancelPreservedSettings: false,
    resetCancelRestoredFocus: false,
    resetConfirmed: false,
    resetConfirmRestoredFocus: false,
    previewRotationDeg: null,
    previewTransform: null,
    postSpeedFrameRateFps: null,
    frameRateCapFps: null,
    frameRateCapApplies: null,
    frameRateMountedCopyVerified: false,
    exportControlStable: false,
    exportControlInitiallyFocused: false,
    exportControlPreservedIdentity: false,
    exportControlPreservedGeometry: false,
    cancelControlSeparate: false,
    cancelInvokeCount: 0,
    dropActionKind: null,
    dropPreservedInput: null,
    queuedDropCount: null,
    progressHistory: [],
    mountedProgressHistory: [],
  };
}

type TargetCorrectiveContext = {
  sourcePathIdentity: string | null;
  format: OutputFormat;
  width: number | null;
  height: number | null;
  currentMaxEdgePx: number | null;
  plannedFrameRateFps: number | null;
  hasAudio: boolean;
  audioEnabled: boolean;
  strictFit: boolean;
};

type LastExportResult = {
  outputPath: string;
  outputSizeBytes: number | null;
  durationS: number | null;
  format: OutputFormat;
  message: string | null;
  diagnostics: ExportDiagnostics | null;
  targetResult: TargetResult | null;
  correctiveContext: TargetCorrectiveContext;
  completedAtMs: number;
};
const QUEUE_STATUS_LABELS: Record<ExportQueueItem["status"], string> = {
  queued: "Queued",
  running: "Running",
  done: "Done",
  "target-missed": "Target missed",
  failed: "Failed",
  cancelled: "Canceled",
};

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

function rangeFillStyle(value: number, min: number, max: number): CSSProperties {
  if (!Number.isFinite(value) || !Number.isFinite(min) || !Number.isFinite(max) || max <= min) return {};
  const fill = clamp(((value - min) / (max - min)) * 100, 0, 100);
  return {
    background: `linear-gradient(90deg, rgba(var(--accent), 0.66) 0%, rgba(var(--accent), 0.66) ${fill}%, rgba(255, 255, 255, 0.08) ${fill}%, rgba(255, 255, 255, 0.08) 100%)`,
  };
}

function formatNumberInput(value: number) {
  return Number.isInteger(value) ? String(value) : value.toFixed(3).replace(/\.?0+$/, "");
}

function formatByteSize(bytes: number | null | undefined) {
  if (!bytes || !Number.isFinite(bytes) || bytes <= 0) return "n/a";
  return `${(bytes / 1_000_000).toFixed(2)} MB`;
}

function cloneEncodeRequest(request: EncodeRequest): EncodeRequest {
  return JSON.parse(JSON.stringify(request)) as EncodeRequest;
}

function hasTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function coerceErrorMessage(error: unknown, fallback: string) {
  return typeof error === "string" ? error : error instanceof Error ? error.message : fallback;
}

function pathFreeSmokeMessage(error: unknown, config: AppSmokeConfig, fallback: string) {
  let message = coerceErrorMessage(error, fallback).trim() || fallback;
  const tokens = [config.inputPath, config.outputPath, config.subtitlePath, config.g7DropPath]
    .filter((value): value is string => typeof value === "string" && value.length > 0)
    .flatMap((value) => {
      const slashValue = value.split("\\").join("/");
      const backslashValue = value.split("/").join("\\");
      const name = slashValue.split("/").pop() ?? "";
      return [value, slashValue, backslashValue, name];
    })
    .filter(Boolean)
    .sort((left, right) => right.length - left.length);
  for (const token of new Set(tokens)) {
    const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    message = message.replace(new RegExp(escaped, "gi"), "the selected file");
  }
  return message.trim() || fallback;
}

function coerceUpdatePublicError(error: unknown, fallback: string): UpdatePublicError {
  const candidate = error && typeof error === "object" ? error as Partial<UpdatePublicError> : null;
  return {
    code: typeof candidate?.code === "string" ? candidate.code : "update-failed",
    phase: candidate?.phase ?? "failed",
    retryable: candidate?.retryable !== false,
    action: typeof candidate?.action === "string" ? candidate.action : "retryOrDownloadPortable",
    message: typeof candidate?.message === "string" && candidate.message ? candidate.message : fallback,
  };
}

function waitMs(ms: number) {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

async function waitForSmokeCondition(check: () => boolean, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (check()) return true;
    await waitMs(40);
  }
  return check();
}

async function focusSmokeWebviewTarget(target: HTMLElement) {
  try {
    await getCurrentWebview().setFocus();
  } catch {
    return false;
  }
  target.focus();
  return waitForSmokeCondition(() => document.hasFocus() && document.activeElement === target, 2_000);
}

function smokeStageRank(stage: string | null) {
  if (!stage) return -1;
  if (stage === SMOKE_SUCCESS_STAGE) return 999;
  if (stage === SMOKE_ERROR_STAGE) return 998;
  return SMOKE_STAGE_ORDER.indexOf(stage as (typeof SMOKE_STAGE_ORDER)[number]);
}

function formatTrimTargetLabel(target: Exclude<TrimFocusTarget, "preview">) {
  return target === "start" ? "Start" : "End";
}

function normalizeTrimDragSnapInput(rawValue: string, durationS: number | null) {
  const maxAllowed = durationS === null ? TRIM_DRAG_SNAP_MAX_S : Math.min(TRIM_DRAG_SNAP_MAX_S, Math.max(0, Math.floor(durationS)));
  const trimmed = rawValue.trim();
  if (trimmed === "") return "0";

  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return "0";

  return String(clamp(Math.floor(parsed), 0, maxAllowed));
}

function blocksComposeShortcutTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;

  const textEntry = target.closest("input, textarea, select");
  if (textEntry) return true;

  const button = target.closest("button");
  if (!button) return false;

  return !button.classList.contains("vfl-trim-timeline-grab") && !button.classList.contains("vfl-trim-timeline-value");
}

function isVideoCodecCompatible(format: OutputFormat, codec: VideoCodecPreference) {
  if (codec === "auto") return true;
  if (format === "mp4") return codec === "h264" || codec === "mpeg4";
  if (format === "webm") return codec === "vp9" || codec === "vp8";
  return false;
}

function formatVideoCodecLabel(codec: VideoCodecPreference) {
  const ffmpegName = VIDEO_CODEC_FFMPEG_NAMES[codec];
  return ffmpegName ? `${VIDEO_CODEC_LABELS[codec]} (${ffmpegName})` : VIDEO_CODEC_LABELS[codec];
}

function formatStreamAction(action: StreamAction | null | undefined) {
  if (action === "copy") return "copied";
  if (action === "encode") return "re-encoded";
  if (action === "drop") return "removed";
  return "not present";
}

function TargetResultDetails({ targetResult }: { targetResult: TargetResult }) {
  const data = targetResultFormatData(targetResult);
  if (!data) {
    return (
      <div className="vfl-error" role="alert">
        The backend returned inconsistent exact-byte target evidence. The artifact was not reclassified in the app.
      </div>
    );
  }

  const boundedPlans = targetResult.plans.slice(0, 4);
  const selectedPlans = targetResult.plans.filter((plan) => plan.selected);
  const planHistoryValid =
    targetResult.plans.length <= 4 &&
    selectedPlans.length === 1 &&
    selectedPlans[0]?.planNumber === targetResult.selectedPlanNumber;
  return (
    <section
      className={`vfl-target-result ${data.missed ? "missed" : "met"}`}
      aria-label={data.missed ? "Size target missed" : "Size target met"}
      data-target-status={data.status}
    >
      <div className="vfl-target-result-title">
        {data.missed ? "Target missed" : "Target met"}
      </div>
      <div className="vfl-target-result-summary">{summarizeTargetResult(targetResult)}</div>
      <div className="vfl-target-result-values">
        <div><span>Target</span><strong>{data.targetBytesText}</strong></div>
        <div><span>Actual</span><strong>{data.actualBytesText}</strong></div>
        <div>
          <span>{data.missed ? "Over target" : "Remaining"}</span>
          <strong>{data.missed ? `${data.overshootBytesText} (${data.overshootPercentText})` : data.remainingBytesText}</strong>
        </div>
      </div>
      {!planHistoryValid ? (
        <div className="vfl-error" role="alert">
          The backend returned an invalid or unbounded fit-plan history. Only the first four plans are shown.
        </div>
      ) : null}
      {boundedPlans.length ? (
        <details className="vfl-fit-plan-history">
          <summary>Fit plan history ({boundedPlans.length} of 4 maximum)</summary>
          <ol>
            {boundedPlans.map((plan) => (
              <li key={plan.planNumber} className={plan.selected ? "selected" : ""}>
                {summarizeStrictFitPlan(plan) ?? `Plan ${plan.planNumber} returned invalid display data.`}
              </li>
            ))}
          </ol>
        </details>
      ) : null}
    </section>
  );
}

function QueueDiagnosticsDetails({
  outcome,
  summary = "Diagnostics",
}: {
  outcome: ExportQueueOutcome;
  summary?: string;
}) {
  const diagnostics = outcome.diagnostics;
  if (!diagnostics) return null;

  return (
    <details className="vfl-export-diagnostics vfl-queue-diagnostics">
      <summary>{summary}</summary>
      <div className="vfl-diagnostic-grid">
        <div><span>Mode</span><strong>{diagnostics.mode}</strong></div>
        <div><span>Video</span><strong>{diagnostics.videoCodec ?? "none"} ({formatStreamAction(diagnostics.videoAction)})</strong></div>
        <div><span>Audio</span><strong>{diagnostics.audioCodec ?? "none"} ({formatStreamAction(diagnostics.audioAction)})</strong></div>
        <div><span>Attempts</span><strong>{diagnostics.attempts}</strong></div>
        <div><span>Target</span><strong>{formatByteSize(diagnostics.requestedSizeBytes)}</strong></div>
        <div><span>Actual</span><strong>{formatByteSize(diagnostics.actualSizeBytes)}</strong></div>
      </div>
      {diagnostics.copyFallbackReason ? (
        <div className="vfl-export-result-note">Copy fallback: {diagnostics.copyFallbackReason}</div>
      ) : null}
      {diagnostics.colorAction ? (
        <div className="vfl-export-result-note">Color: {diagnostics.colorAction}</div>
      ) : null}
      {diagnostics.sarAction ? (
        <div className="vfl-export-result-note">Display pixels: {diagnostics.sarAction}</div>
      ) : null}
      {diagnostics.reverseBufferAction ? (
        <div className="vfl-export-result-note">Transform buffer: {diagnostics.reverseBufferAction}</div>
      ) : null}
      {diagnostics.subtitleBurnedIn ? (
        <div className="vfl-export-result-note">
          External subtitles burned in{diagnostics.subtitleCueCount ? ` (${diagnostics.subtitleCueCount} cues)` : ""}.
        </div>
      ) : null}
      {diagnostics.failureStage ? (
        <div className="vfl-export-result-note">Failure stage: {diagnostics.failureStage}</div>
      ) : null}
      {diagnostics.failureReason ? (
        <div className="vfl-export-result-note">Failure: {diagnostics.failureReason}</div>
      ) : null}
      <pre className="vfl-command-preview">{diagnostics.commandPreview}</pre>
    </details>
  );
}

function colorIsDefault(brightness: string, contrast: string, saturation: string) {
  const brightnessNum = Number(brightness);
  const contrastNum = Number(contrast);
  const saturationNum = Number(saturation);
  return !(
    (Number.isFinite(brightnessNum) && Math.abs(brightnessNum) > 1e-9) ||
    (Number.isFinite(contrastNum) && Math.abs(contrastNum - 1) > 1e-9) ||
    (Number.isFinite(saturationNum) && Math.abs(saturationNum - 1) > 1e-9)
  );
}

function evenPixel(value: number) {
  if (!Number.isFinite(value)) return 2;
  return Math.max(2, Math.floor(value / 2) * 2);
}

function dimensionsAfterShape(
  probe: VideoProbe,
  cropEnabled: boolean,
  cropRect: NormalizedRect,
  rotateDeg: number,
) {
  let width = probe.width;
  let height = probe.height;

  if (cropEnabled) {
    const cropPx = cropRectToPixels(cropRect, probe.width, probe.height);
    const isFull = isFullFramePixelCrop(cropPx, probe.width, probe.height);
    if (!isFull) {
      const encodedCrop = alignCropRectForEncoding(cropPx);
      width = encodedCrop.width;
      height = encodedCrop.height;
    }
  }

  if (rotateDeg === 90 || rotateDeg === 270) {
    const tmp = width;
    width = height;
    height = tmp;
  }

  return { width, height };
}

function parseDimensionDraft(raw: string) {
  const parsed = Number(raw);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

function formatDimensionDraft(value: number) {
  return String(evenPixel(Math.round(value)));
}

function App() {
  const [openCards, setOpenCards] = useState<Record<RailCardKey, boolean>>(DEFAULT_OPEN_CARDS);
  const [sampleDurationS, setSampleDurationS] = useState<number>(10);
  const [sampleEstimate, setSampleEstimate] = useState<{
    sampleBytes: number;
    estimateBytes: number | null;
  } | null>(null);
  const [stripMetadata, setStripMetadata] = useState(true);

  function toggleCard(key: RailCardKey) {
    setOpenCards((cards) => ({ ...cards, [key]: !cards[key] }));
  }

  const [inputPath, setInputPath] = useState("");
  const [outputPath, setOutputPath] = useState("");
  const [outputAuto, setOutputAuto] = useState(true);
  const [outputSuggestNonce, setOutputSuggestNonce] = useState(0);
  const [probe, setProbe] = useState<VideoProbe | null>(null);
  const [probeError, setProbeError] = useState<string | null>(null);
  const [previewReady, setPreviewReady] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);

  const [format, setFormat] = useState<OutputFormat>(DEFAULT_OUTPUT_FORMAT);
  const [title, setTitle] = useState("");
  const [sizeLimitMb, setSizeLimitMb] = useState<string>(DEFAULT_SIZE_LIMIT_MB);
  const [resizeMode, setResizeMode] = useState<ResizeMode>("source");
  const [maxEdgePx, setMaxEdgePx] = useState("");
  const [customWidthPx, setCustomWidthPx] = useState("");
  const [customHeightPx, setCustomHeightPx] = useState("");
  const [outputAspectLocked, setOutputAspectLocked] = useState(true);
  const [audioEnabled, setAudioEnabled] = useState(true);
  const [strictFit, setStrictFit] = useState(false);
  const [subtitlePath, setSubtitlePath] = useState("");
  const [subtitleInspection, setSubtitleInspection] = useState<SubtitleInspection | null>(null);
  const [subtitleInspecting, setSubtitleInspecting] = useState(false);
  const [subtitleError, setSubtitleError] = useState<string | null>(null);
  const [subtitleStatus, setSubtitleStatus] = useState("No external subtitles selected.");
  const [normalizeAudio, setNormalizeAudio] = useState(false);
  const [perturbFirstFrame, setPerturbFirstFrame] = useState(false);
  const [colorPolicy, setColorPolicy] = useState<ColorPolicy>("auto");
  const [advancedVideoCodec, setAdvancedVideoCodec] = useState<VideoCodecPreference>("auto");
  const [advancedAudioBitrateKbps, setAdvancedAudioBitrateKbps] = useState("auto");
  const [advancedVideoQuality, setAdvancedVideoQuality] = useState<VideoQualityPreference>("auto");
  const [advancedEncodeSpeed, setAdvancedEncodeSpeed] = useState<EncodeSpeedPreference>("auto");
  const [advancedFrameRateCapFps, setAdvancedFrameRateCapFps] = useState("auto");
  const [advancedAudioChannels, setAdvancedAudioChannels] = useState<AudioChannelPreference>("auto");
  const [encodeCapabilities, setEncodeCapabilities] = useState<EncodeCapabilities | null>(null);
  const [encodeCapabilitiesError, setEncodeCapabilitiesError] = useState<string | null>(null);

  const [trimStart, setTrimStart] = useState("0");
  const [trimEnd, setTrimEnd] = useState("");
  const [trimDragSnapS, setTrimDragSnapS] = useState("0");
  const [reverse, setReverse] = useState(false);
  const [loopVideo, setLoopVideo] = useState(false);
  const [speed, setSpeed] = useState("1.0");
  const [rotateDeg, setRotateDeg] = useState(0);

  const [cropEnabled, setCropEnabled] = useState(false);
  const [cropRect, setCropRect] = useState<NormalizedRect>({ x: 0, y: 0, w: 1, h: 1 });
  const [aspectLocked, setAspectLocked] = useState(false);
  const [aspectPreset, setAspectPreset] = useState<"free" | "1:1" | "16:9" | "9:16" | "4:3" | "3:4">("free");
  const [cropDetecting, setCropDetecting] = useState(false);
  const [cropDetectHint, setCropDetectHint] = useState<string | null>(null);

  const [brightness, setBrightness] = useState("0");
  const [contrast, setContrast] = useState("1");
  const [saturation, setSaturation] = useState("1");
  const [previewTimeS, setPreviewTimeS] = useState(0);
  const [previewPlaying, setPreviewPlaying] = useState(false);
  const [activeTrimTarget, setActiveTrimTarget] = useState<TrimFocusTarget>("preview");
  const [frameSaving, setFrameSaving] = useState(false);

  const [jobId, setJobId] = useState<number | null>(null);
  const [encodeEventsReady, setEncodeEventsReady] = useState(false);
  const [encodeEventsError, setEncodeEventsError] = useState<string | null>(null);
  const [encodeProgress, setEncodeProgress] = useState(() => createEncodeProgressState());
  const [status, setStatus] = useState<string>("Pick a video to begin.");
  const [dragActive, setDragActive] = useState(false);
  const [settingsReady, setSettingsReady] = useState(false);
  const [smokeConfig, setSmokeConfig] = useState<AppSmokeConfig | null>(null);
  const [previewMediaReady, setPreviewMediaReady] = useState(false);
  const [lastExport, setLastExport] = useState<LastExportResult | null>(null);
  const [appliedCorrectiveKinds, setAppliedCorrectiveKinds] = useState<string[]>([]);
  const [latestAttempt, setLatestAttempt] = useState<EncodeAttemptState>(() => createIdleEncodeAttempt());
  const [exportQueueState, setExportQueueState] = useState<ExportQueueState>(() => createExportQueueState());
  const [queuePreparationBusy, setQueuePreparationBusy] = useState(false);
  const [queueSnapshotApplying, setQueueSnapshotApplying] = useState(false);
  const exportQueue = exportQueueState.items;
  const queueRunning = exportQueueState.autoRun;
  const queueActiveItemId = exportQueueState.active?.itemId ?? null;
  const [userRecipeStore, setUserRecipeStore] = useState<UserRecipeStore>(() => createEmptyUserRecipeStore());
  const [recipeDialog, setRecipeDialog] = useState<UserRecipeDialogState | null>(null);
  const [recipeNameDraft, setRecipeNameDraft] = useState("");
  const [recipeDescriptionDraft, setRecipeDescriptionDraft] = useState("");
  const [recipeResetToCurrentSettings, setRecipeResetToCurrentSettings] = useState(false);
  const [recipeDialogError, setRecipeDialogError] = useState<string | null>(null);
  const [recipeStatus, setRecipeStatus] = useState<string | null>(null);
  const [recipeNotification, setRecipeNotification] = useState<{
    message: string;
    isFading: boolean;
  } | null>(null);
  const [resetConfirmationOpen, setResetConfirmationOpen] = useState(false);
  const [updateNotice, setUpdateNotice] = useState<UpdateCheckResponse | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<string | null>(null);
  const [updateProgress, setUpdateProgress] = useState(() => createUpdateProgressState());
  const [updatePublicError, setUpdatePublicError] = useState<UpdatePublicError | null>(null);
  const [updateStartupNotice, setUpdateStartupNotice] = useState<{
    kind: "status" | "error";
    message: string;
  } | null>(null);
  const [manualUpdateBusy, setManualUpdateBusy] = useState(false);
  const [manualUpdateStatus, setManualUpdateStatus] = useState<string | null>(null);
  // True when this instance was relaunched elevated to finish installing an
  // update; the app applies it immediately and restarts on its own.
  const [elevatedUpdateRun, setElevatedUpdateRun] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const modalOpen = elevatedUpdateRun || aboutOpen || recipeDialog !== null || resetConfirmationOpen;

  const jobIdRef = useRef<number | null>(null);
  const updateBusyRef = useRef(false);
  const updatePhaseRef = useRef<UpdateProgressEvent["phase"] | null>(null);
  const encodeEventsReadyRef = useRef(false);
  const encodeEventsErrorRef = useRef<string | null>(null);
  const attemptIdRef = useRef(Math.floor(Date.now() * 1000));
  const latestAttemptRef = useRef<EncodeAttemptState>(latestAttempt);
  const inputPathRef = useRef("");
  const audioEnabledRef = useRef(true);
  const autoMutedRef = useRef(false);
  const formatRef = useRef<OutputFormat>(format);
  const outputAutoRef = useRef<boolean>(outputAuto);
  const cropperRef = useRef<VideoCropperHandle | null>(null);
  const cropDetectionRevisionRef = useRef(0);
  const trimTimelineTrackRef = useRef<HTMLDivElement | null>(null);
  const trimDragCleanupRef = useRef<(() => void) | null>(null);
  const exportQueueStateRef = useRef<ExportQueueState>(exportQueueState);
  const queuePreparationRef = useRef<Promise<void>>(Promise.resolve());
  const queuePreparationCountRef = useRef(0);
  const queueStopRevisionRef = useRef(0);
  const queueSnapshotApplyTokenRef = useRef(0);
  const queueSnapshotApplyingRef = useRef(false);
  const pendingQueueSnapshotRef = useRef<{
    token: number;
    inputPath: string;
    announce: boolean;
    message: string | null;
  } | null>(null);
  const recipeSaveButtonRef = useRef<HTMLButtonElement | null>(null);
  const recipeNotificationFadeTimerRef = useRef<number | null>(null);
  const recipeNotificationClearTimerRef = useRef<number | null>(null);
  const queueFallbackButtonRef = useRef<HTMLButtonElement | null>(null);
  const queueRunButtonRef = useRef<HTMLButtonElement | null>(null);
  const queueStopButtonRef = useRef<HTMLButtonElement | null>(null);
  const queueRegionRef = useRef<HTMLDivElement | null>(null);
  const subtitleBrowseButtonRef = useRef<HTMLButtonElement | null>(null);
  const subtitleInspectionTokenRef = useRef(0);
  const modalOpenRef = useRef(false);
  const smokeConfigRef = useRef<AppSmokeConfig | null>(null);
  const smokeStageRef = useRef<string | null>(null);
  const smokeStageHistoryRef = useRef<string[]>([]);
  const smokeStatusWriteRef = useRef<Promise<void>>(Promise.resolve());
  const smokeAppliedRef = useRef(false);
  const smokeStartRef = useRef(false);
  const smokeAttemptIdRef = useRef<number | null>(null);
  const smokeMetricsRef = useRef<{ trimStartS: number | null; trimEndS: number | null; expectedDurationS: number | null } | null>(null);
  const smokeInteractionRunningRef = useRef(false);
  const smokeInteractionDoneRef = useRef(false);
  const smokeWorkflowRunningRef = useRef(false);
  const smokeWorkflowDoneRef = useRef(false);
  const smokeG7UiRunningRef = useRef(false);
  const smokeG7UiDoneRef = useRef(false);
  const smokeG7ActiveChecksRunningRef = useRef(false);
  const smokeG7ActiveChecksDoneRef = useRef(false);
  const smokeG7EvidenceRef = useRef<AppSmokeG7Evidence | null>(null);
  const smokeG7ExportButtonRef = useRef<HTMLButtonElement | null>(null);
  const smokeG7ExportRectRef = useRef<{ x: number; y: number; width: number; height: number } | null>(null);
  const smokeG7BeforeCancelInvokeRef = useRef<(() => Promise<void>) | null>(null);
  const smokeG7CancelStagePromiseRef = useRef<Promise<void> | null>(null);
  const previewTimeRef = useRef(0);
  const previewSelectionTimeRef = useRef(0);
  const previewPlayingRef = useRef(false);
  const activeTrimTargetRef = useRef<TrimFocusTarget>(activeTrimTarget);
  const trimTimelineRef = useRef<TrimTimeline | null>(null);
  const pendingEncodeRef = useRef<{
    attemptId: number;
    jobId: number | null;
    cancelRequested: boolean;
    outputPath: string;
    durationS: number | null;
    format: OutputFormat;
    queueItemId: number | null;
    queueRunId: number | null;
    sample: { outputDurationS: number; fullDurationS: number | null } | null;
    correctiveContext: TargetCorrectiveContext;
  } | null>(null);

  function updateLatestAttempt(next: EncodeAttemptState) {
    latestAttemptRef.current = next;
    setLatestAttempt(next);
  }

  function dispatchExportQueue(action: ExportQueueAction) {
    const next = reduceExportQueue(exportQueueStateRef.current, action);
    if (next !== exportQueueStateRef.current) {
      exportQueueStateRef.current = next;
      setExportQueueState(next);
    }
    return next;
  }

  function supersedeQueueSnapshotApply() {
    queueSnapshotApplyTokenRef.current += 1;
    queueSnapshotApplyingRef.current = false;
    pendingQueueSnapshotRef.current = null;
    setQueueSnapshotApplying(false);
  }

  function preservePendingSnapshotCropWithoutAnnouncement() {
    if (pendingQueueSnapshotRef.current) {
      pendingQueueSnapshotRef.current = {
        ...pendingQueueSnapshotRef.current,
        announce: false,
      };
    }
  }

  function clearRecipeNotificationTimers() {
    if (recipeNotificationFadeTimerRef.current !== null) {
      window.clearTimeout(recipeNotificationFadeTimerRef.current);
      recipeNotificationFadeTimerRef.current = null;
    }
    if (recipeNotificationClearTimerRef.current !== null) {
      window.clearTimeout(recipeNotificationClearTimerRef.current);
      recipeNotificationClearTimerRef.current = null;
    }
  }

  function showPersistentRecipeStatus(message: string | null) {
    clearRecipeNotificationTimers();
    setRecipeNotification(null);
    setRecipeStatus(message);
  }

  function showRecipeNotification(message: string) {
    clearRecipeNotificationTimers();
    setRecipeStatus(null);
    setRecipeNotification({ message, isFading: false });
    recipeNotificationFadeTimerRef.current = window.setTimeout(() => {
      recipeNotificationFadeTimerRef.current = null;
      setRecipeNotification((current) =>
        current?.message === message ? { ...current, isFading: true } : current,
      );
    }, RECIPE_NOTIFICATION_FADE_START_MS);
    recipeNotificationClearTimerRef.current = window.setTimeout(() => {
      recipeNotificationClearTimerRef.current = null;
      setRecipeNotification((current) => current?.message === message ? null : current);
    }, RECIPE_NOTIFICATION_CLEAR_MS);
  }

  useEffect(() => () => clearRecipeNotificationTimers(), []);

  function focusButtonWhenAvailable(
    ref: { current: HTMLButtonElement | null },
    remainingAttempts = 25,
  ) {
    const button = ref.current;
    if (button?.isConnected && !button.disabled) {
      button.focus();
      return;
    }
    if (remainingAttempts <= 0) return;
    window.setTimeout(() => focusButtonWhenAvailable(ref, remainingAttempts - 1), 40);
  }

  function focusQueueAfterMutation(
    preferred?: { current: HTMLButtonElement | null },
    remainingAttempts = 25,
  ) {
    const preferredButton = preferred?.current ?? null;
    if (preferredButton?.isConnected && !preferredButton.disabled) {
      preferredButton.focus();
      return;
    }
    if (preferred && remainingAttempts > 0) {
      window.setTimeout(
        () => focusQueueAfterMutation(preferred, remainingAttempts - 1),
        40,
      );
      return;
    }

    const target = [
      queueRunButtonRef.current,
      queueFallbackButtonRef.current,
      queueStopButtonRef.current,
    ].find((button) => button?.isConnected && !button.disabled);
    if (target) {
      target.focus();
      return;
    }
    if (queueRegionRef.current?.isConnected) queueRegionRef.current.focus();
  }

  async function reportSmokeStatus(stage: string, extra: Omit<AppSmokeStatus, "stage"> = {}) {
    if (!smokeConfigRef.current) return;
    if (smokeStageRef.current === SMOKE_SUCCESS_STAGE || smokeStageRef.current === SMOKE_ERROR_STAGE) return;

    // G7's retained status is itself privacy evidence. The runner already
    // owns the expected output path, so never copy that private control value
    // into a G7 status file.
    const smokeOutputPath = smokeConfigRef.current.g7Operation
      ? null
      : extra.outputPath ?? null;
    const smokeMessage = smokeConfigRef.current.g7Operation && extra.message
      ? pathFreeSmokeMessage(
          extra.message,
          smokeConfigRef.current,
          "Packaged G7 smoke operation failed.",
        )
      : extra.message ?? null;

    const currentRank = smokeStageRank(smokeStageRef.current);
    const nextRank = smokeStageRank(stage);
    if (stage !== SMOKE_SUCCESS_STAGE && stage !== SMOKE_ERROR_STAGE && nextRank >= 0 && currentRank >= nextRank) return;

    smokeStageRef.current = stage;
    if (!smokeStageHistoryRef.current.includes(stage)) {
      smokeStageHistoryRef.current = [...smokeStageHistoryRef.current, stage];
    }
    smokeStatusWriteRef.current = smokeStatusWriteRef.current.then(async () => {
      try {
        await invoke("write_smoke_status", {
          status: {
            stage,
            ok: extra.ok ?? null,
            message: smokeMessage,
            outputPath: smokeOutputPath,
            outputSizeBytes: extra.outputSizeBytes ?? null,
            trimStartS: extra.trimStartS ?? null,
            trimEndS: extra.trimEndS ?? null,
            expectedDurationS: extra.expectedDurationS ?? null,
            stageHistory: smokeStageHistoryRef.current,
            targetResult: extra.targetResult ?? null,
            diagnostics: extra.diagnostics ?? null,
            queueOutcomeKind: extra.queueOutcomeKind ?? null,
            g7Evidence: extra.g7Evidence ?? null,
          },
        });
      } catch (error) {
        console.warn("Failed to write smoke status:", error);
      }
    });
    await smokeStatusWriteRef.current;
  }

  async function reportSmokeFailure(
    message: string,
    extra: Omit<AppSmokeStatus, "stage" | "ok" | "message"> = {},
  ) {
    if (!smokeConfigRef.current) return;
    if (smokeStageRef.current === SMOKE_SUCCESS_STAGE || smokeStageRef.current === SMOKE_ERROR_STAGE) return;
    await reportSmokeStatus(SMOKE_ERROR_STAGE, { ...extra, ok: false, message });
  }

  function updateSmokeG7Evidence(patch: Partial<AppSmokeG7Evidence>) {
    const current = smokeG7EvidenceRef.current;
    if (!current) return;
    smokeG7EvidenceRef.current = { ...current, ...patch };
  }

  function appendSmokeG7Progress(payload: EncodeProgressPayload) {
    const current = smokeG7EvidenceRef.current;
    if (!current || smokeAttemptIdRef.current !== payload.attemptId) return;
    const sample: AppSmokeProgressSample = {
      attemptId: payload.attemptId,
      jobId: payload.jobId,
      phase: payload.phase,
      stepIndex: payload.stepIndex,
      stepCount: payload.stepCount,
      pass: payload.pass,
      totalPasses: payload.totalPasses,
      passPct: payload.passPct,
      overallPct: payload.overallPct,
    };
    if (current.progressHistory.length >= 512) {
      void reportSmokeFailure("Packaged G7 progress telemetry exceeded its bounded 512-sample history.", {
        g7Evidence: current,
      });
      return;
    }
    smokeG7EvidenceRef.current = {
      ...current,
      progressHistory: [...current.progressHistory, sample],
    };
  }

  useEffect(() => {
    updateBusyRef.current = updateBusy;
  }, [updateBusy]);

  useEffect(() => {
    formatRef.current = format;
  }, [format]);

  useEffect(() => {
    outputAutoRef.current = outputAuto;
  }, [outputAuto]);

  useEffect(() => {
    smokeConfigRef.current = smokeConfig;
  }, [smokeConfig]);

  useEffect(() => {
    previewTimeRef.current = previewTimeS;
  }, [previewTimeS]);

  useEffect(() => {
    activeTrimTargetRef.current = activeTrimTarget;
  }, [activeTrimTarget]);

  useEffect(() => {
    modalOpenRef.current = modalOpen;
    if (modalOpen) setDragActive(false);
    if (elevatedUpdateRun && aboutOpen) setAboutOpen(false);
  }, [aboutOpen, elevatedUpdateRun, modalOpen]);

  useEffect(() => {
    if (activeTrimTargetRef.current === "preview" || previewPlayingRef.current) {
      previewSelectionTimeRef.current = previewTimeS;
    }
  }, [activeTrimTarget, previewPlaying, previewTimeS]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    let stop = false;

    void (async () => {
      try {
        const startup = await invoke<UpdateStartupResponse>("finalize_update_startup", {
          onEvent: createUpdateProgressChannel(),
        });
        if (startup.status === "recoveryRequired") {
          if (!stop) setUpdateStartupNotice({ kind: "error", message: startup.message });
          return;
        }
        if (startup.status === "recovering" || startup.status === "elevatingRecovery") {
          if (!stop) {
            updateBusyRef.current = true;
            setUpdateBusy(true);
            setUpdateStatus(startup.message);
            setUpdateStartupNotice({ kind: "status", message: startup.message });
          }
          return;
        }
        if (!stop && (startup.status === "completed" || startup.status === "recovered")) {
          setUpdateStartupNotice({ kind: "status", message: startup.message });
        }
      } catch (error) {
        const publicError = coerceUpdatePublicError(error, "The updater could not verify its saved state. Restart the app before trying another update.");
        if (!stop) setUpdateStartupNotice({ kind: "error", message: publicError.message });
        return;
      }

      try {
        if (await invoke<boolean>("elevated_update_pending")) {
          if (!stop) {
            setElevatedUpdateRun(true);
            void applyUpdate();
          }
          return;
        }
      } catch (error) {
        console.warn("Failed to check elevated update state:", error);
      }

      try {
        const result = await invoke<UpdateCheckResponse>("check_for_update", {
          force: false,
          onEvent: createUpdateProgressChannel(),
        });
        if (!stop && result.status === "available") {
          setUpdateNotice(result);
          setUpdateStatus(null);
        }
      } catch (error) {
        console.warn("Failed to check for updates:", error);
      }
    })();

    return () => {
      stop = true;
    };
  }, []);

  useEffect(() => () => {
    if (trimDragCleanupRef.current) {
      trimDragCleanupRef.current();
      trimDragCleanupRef.current = null;
    }
  }, []);

  useEffect(() => {
    let currentWindow: ReturnType<typeof getCurrentWindow>;
    try {
      currentWindow = getCurrentWindow();
    } catch {
      return;
    }
    let disposed = false;
    let resizeGuard = false;
    let unlistenResize: (() => void) | null = null;

    const enforceMinWindowSize = async (size?: { width: number; height: number } | null) => {
      if (disposed || resizeGuard) return;

      resizeGuard = true;
      try {
        const scaleFactor = await currentWindow.scaleFactor();
        const width = size?.width ?? (await currentWindow.innerSize()).width;
        const height = size?.height ?? (await currentWindow.innerSize()).height;
        const logicalWidth = width / scaleFactor;
        const logicalHeight = height / scaleFactor;

        if (logicalWidth >= MIN_WINDOW_WIDTH && logicalHeight >= MIN_WINDOW_HEIGHT) {
          return;
        }

        await currentWindow.setSize(
          new LogicalSize(
            Math.max(MIN_WINDOW_WIDTH, logicalWidth),
            Math.max(MIN_WINDOW_HEIGHT, logicalHeight),
          ),
        );
      } finally {
        resizeGuard = false;
      }
    };

    void (async () => {
      try {
        const minSize = new LogicalSize(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT);
        await currentWindow.setMinSize(minSize);
        await currentWindow.setSizeConstraints({
          minWidth: MIN_WINDOW_WIDTH,
          minHeight: MIN_WINDOW_HEIGHT,
        });
        await enforceMinWindowSize();
        unlistenResize = await currentWindow.onResized(({ payload }) => {
          void enforceMinWindowSize({
            width: payload.width,
            height: payload.height,
          });
        });
      } catch {
        // ignore
      }
    })();

    return () => {
      disposed = true;
      if (unlistenResize) {
        unlistenResize();
      }
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let cancelled = false;
    void (async () => {
      try {
        const handle = await getCurrentWindow().onCloseRequested(async (event) => {
          const queuedCount = summarizeExportQueue(exportQueueStateRef.current).queued;
          const encoding = jobIdRef.current !== null || pendingEncodeRef.current !== null;
          const preparing = queuePreparationCountRef.current > 0;
          const updating = updateBusyRef.current;
          if (!encoding && queuedCount === 0 && !preparing && !updating) return;

          const unfinished = [
            encoding ? "an export is still running" : null,
            queuedCount > 0
              ? `${queuedCount} queued export${queuedCount === 1 ? " has" : "s have"} not started`
              : null,
            preparing ? "selected queue files are still being prepared" : null,
            updating ? "an update is still being prepared" : null,
          ].filter((part): part is string => Boolean(part));
          const summary = `${unfinished.join(", and ")}.`;
          const ok = await confirmDialog(`${summary} Close anyway?`, {
            title: "Video For Lazies",
            kind: "warning",
          });
          if (!ok) event.preventDefault();
        });
        if (cancelled) handle();
        else unlisten = handle;
      } catch {
        // Not running inside a Tauri window.
      }
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    previewPlayingRef.current = previewPlaying;
  }, [previewPlaying]);

  useEffect(() => {
    inputPathRef.current = inputPath;
  }, [inputPath]);

  useEffect(() => {
    audioEnabledRef.current = audioEnabled;
  }, [audioEnabled]);

  useEffect(() => {
    setPreviewTimeS(0);
    previewPlayingRef.current = false;
    setPreviewPlaying(false);
    setPreviewMediaReady(false);
    setLastExport(null);
    if (jobIdRef.current === null && pendingEncodeRef.current === null) {
      updateLatestAttempt(createIdleEncodeAttempt());
    }
    activeTrimTargetRef.current = "preview";
    setActiveTrimTarget("preview");
    previewSelectionTimeRef.current = 0;
    smokeMetricsRef.current = null;
  }, [inputPath]);

  useEffect(() => {
    try {
      const parsed = parsePersistedSettings(localStorage.getItem(SETTINGS_KEY));
      if (parsed.format) setFormat(parsed.format);
      if (parsed.normalizeAudio) setNormalizeAudio(true);
      if (parsed.stripMetadata === false) setStripMetadata(false);
      if (parsed.advanced?.videoCodec) setAdvancedVideoCodec(parsed.advanced.videoCodec);
      if (parsed.advanced?.audioBitrateKbps) setAdvancedAudioBitrateKbps(String(parsed.advanced.audioBitrateKbps));
      if (parsed.advanced?.videoQuality) setAdvancedVideoQuality(parsed.advanced.videoQuality);
      if (parsed.advanced?.encodeSpeed) setAdvancedEncodeSpeed(parsed.advanced.encodeSpeed);
      if (parsed.advanced?.frameRateCapFps) setAdvancedFrameRateCapFps(String(parsed.advanced.frameRateCapFps));
      if (parsed.advanced?.audioChannels) setAdvancedAudioChannels(parsed.advanced.audioChannels);
    } catch {
      // ignore
    } finally {
      setSettingsReady(true);
    }
  }, []);

  useEffect(() => {
    const loaded = loadUserRecipeStore(localStorage);
    setUserRecipeStore(loaded);
    if (loaded.warnings.length) {
      showPersistentRecipeStatus(loaded.warnings.join(" "));
    }
  }, []);

  useEffect(() => {
    if (!settingsReady) return;
    try {
      localStorage.setItem(
        SETTINGS_KEY,
        serializePersistedSettings({
          format,
          normalizeAudio,
          stripMetadata,
          advanced: {
            videoCodec: advancedVideoCodec,
            audioBitrateKbps: advancedAudioBitrateKbps === "auto" ? null : Number(advancedAudioBitrateKbps),
            videoQuality: advancedVideoQuality,
            encodeSpeed: advancedEncodeSpeed,
            frameRateCapFps: advancedFrameRateCapFps === "auto" ? null : Number(advancedFrameRateCapFps),
            audioChannels: advancedAudioChannels,
          },
        }),
      );
    } catch {
      // ignore
    }
  }, [
    settingsReady,
    format,
    normalizeAudio,
    stripMetadata,
    advancedVideoCodec,
    advancedAudioBitrateKbps,
    advancedVideoQuality,
    advancedEncodeSpeed,
    advancedFrameRateCapFps,
    advancedAudioChannels,
  ]);

  useEffect(() => {
    if (!settingsReady) return;
    if (!hasTauriRuntime()) return;

    let stop = false;
    (async () => {
      try {
        const config = await invoke<AppSmokeConfig | null>("read_smoke_config");
        if (stop || !config) return;

        smokeStageRef.current = null;
        smokeStageHistoryRef.current = [];
        smokeStatusWriteRef.current = Promise.resolve();
        smokeAppliedRef.current = false;
        smokeStartRef.current = false;
        smokeAttemptIdRef.current = null;
        smokeWorkflowRunningRef.current = false;
        smokeWorkflowDoneRef.current = false;
        smokeG7UiRunningRef.current = false;
        smokeG7UiDoneRef.current = false;
        smokeG7ActiveChecksRunningRef.current = false;
        smokeG7ActiveChecksDoneRef.current = false;
        smokeG7EvidenceRef.current = config.g7Operation
          ? createSmokeG7Evidence(config.g7Operation)
          : null;
        smokeG7ExportButtonRef.current = null;
        smokeG7ExportRectRef.current = null;
        smokeG7BeforeCancelInvokeRef.current = null;
        smokeG7CancelStagePromiseRef.current = null;
        smokeConfigRef.current = config;
        setSmokeConfig(config);
        await reportSmokeStatus("detected", { message: "Packaged app smoke mode detected." });
      } catch (error) {
        console.warn("Failed to read smoke config:", error);
      }
    })();

    return () => {
      stop = true;
    };
  }, [settingsReady]);

  function safeSubtitleError(error: unknown, selectedPath?: string) {
    let message = coerceErrorMessage(error, "The selected subtitle file could not be validated.");
    if (selectedPath) {
      message = message.split(selectedPath).join("the selected subtitle file");
      const selectedName = basename(selectedPath);
      if (selectedName) message = message.split(selectedName).join("the selected subtitle file");
    }
    return message;
  }

  function clearExternalSubtitle(message = "No external subtitles selected.", focusBrowse = false) {
    subtitleInspectionTokenRef.current += 1;
    setSubtitlePath("");
    setSubtitleInspection(null);
    setSubtitleInspecting(false);
    setSubtitleError(null);
    setSubtitleStatus(message);
    if (focusBrowse) {
      window.setTimeout(() => focusButtonWhenAvailable(subtitleBrowseButtonRef), 0);
    }
  }

  function applyNewInput(path: string, nextFormat: OutputFormat) {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current ||
      updateBusyRef.current
    ) return;
    supersedeQueueSnapshotApply();
    setOutputAuto(true);
    setOutputPath("");
    setColorPolicy("auto");
    if (queuePathIdentity(path) !== queuePathIdentity(inputPathRef.current)) {
      clearExternalSubtitle("External subtitles cleared for the new source.");
    }

    if (nextFormat !== formatRef.current) setFormat(nextFormat);
    setInputPath(path);
    setStatus("Probing…");
  }

  function performResetAllSettings() {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current ||
      updateBusyRef.current
    ) {
      setResetConfirmationOpen(false);
      return;
    }
    setResetConfirmationOpen(false);
    supersedeQueueSnapshotApply();
    cropDetectionRevisionRef.current += 1;
    setCropDetecting(false);
    setFormat(DEFAULT_OUTPUT_FORMAT);
    setTitle("");
    setSizeLimitMb(DEFAULT_SIZE_LIMIT_MB);
    setResizeMode("source");
    setMaxEdgePx("");
    setCustomWidthPx("");
    setCustomHeightPx("");
    setOutputAspectLocked(true);
    setAudioEnabled(true);
    setStrictFit(false);
    clearExternalSubtitle("External subtitles cleared by Reset.");
    setNormalizeAudio(false);
    setPerturbFirstFrame(false);
    setColorPolicy("auto");
    setStripMetadata(true);
    setSampleDurationS(10);
    setSampleEstimate(null);
    autoMutedRef.current = false;
    setAdvancedVideoCodec("auto");
    setAdvancedAudioBitrateKbps("auto");
    setAdvancedVideoQuality("auto");
    setAdvancedEncodeSpeed("auto");
    setAdvancedFrameRateCapFps("auto");
    setAdvancedAudioChannels("auto");

    setTrimStart("0");
    setTrimEnd("");
    setTrimDragSnapS("0");
    setReverse(false);
    setLoopVideo(false);
    setSpeed("1.0");
    setRotateDeg(0);

    setCropEnabled(false);
    setCropRect({ x: 0, y: 0, w: 1, h: 1 });
    setAspectLocked(false);
    setAspectPreset("free");
    setCropDetectHint(null);

    setBrightness("0");
    setContrast("1");
    setSaturation("1");
  }

  function applyFullRecipeSettings(
    recipeSettings: ExportRecipeSettings,
    label: string,
    options: { resetOutput: boolean },
  ) {
    if (queueSnapshotApplyingRef.current) return;
    preservePendingSnapshotCropWithoutAnnouncement();
    const recipeAdvanced = recipeSettings.advanced;
    const recipeResize = normalizeRecipeResizeSettings(recipeSettings);
    const recipeWantsAudio = recipeSettings.format === "mp3" || recipeSettings.audioEnabled;
    const sourceHasAudio = probe ? probe.hasAudio : true;

    setFormat(recipeSettings.format);
    setSizeLimitMb(recipeSettings.sizeLimitMb);
    setResizeMode(recipeResize.mode);
    setMaxEdgePx(recipeResize.maxEdgePx);
    setCustomWidthPx(recipeResize.widthPx);
    setCustomHeightPx(recipeResize.heightPx);
    setOutputAspectLocked(recipeResize.lockAspect);
    autoMutedRef.current = recipeWantsAudio && !sourceHasAudio;
    setAudioEnabled(recipeWantsAudio && sourceHasAudio);
    setNormalizeAudio(Boolean(recipeSettings.normalizeAudio));
    setPerturbFirstFrame(Boolean(recipeSettings.perturbFirstFrame));
    setStrictFit(Boolean(recipeSettings.strictFit));
    setAdvancedVideoCodec(recipeAdvanced.videoCodec);
    setAdvancedAudioBitrateKbps(recipeAdvanced.audioBitrateKbps === null ? "auto" : String(recipeAdvanced.audioBitrateKbps));
    setAdvancedVideoQuality(recipeAdvanced.videoQuality);
    setAdvancedEncodeSpeed(recipeAdvanced.encodeSpeed);
    setAdvancedFrameRateCapFps(recipeAdvanced.frameRateCapFps === null ? "auto" : String(recipeAdvanced.frameRateCapFps));
    setAdvancedAudioChannels(recipeAdvanced.audioChannels);
    if (options.resetOutput) {
      setOutputAuto(true);
      setOutputPath("");
      setOutputSuggestNonce((nonce) => nonce + 1);
    }
    showRecipeNotification(
      options.resetOutput
        ? `Applied ${label}. A fresh output path will be suggested; clip edits, color consent, and metadata privacy were unchanged.`
        : `Applied ${label}. Clip edits, color consent, and metadata privacy were unchanged. The output extension may follow the recipe format.`,
    );
  }

  function applyExportRecipe(recipe: ExportRecipe) {
    if (queueSnapshotApplyingRef.current) return;
    if (recipe.partial) {
      if (format === "mp3") {
        setStatus(`${recipe.label} is available only for MP4 or WebM video exports.`);
        return;
      }
      preservePendingSnapshotCropWithoutAnnouncement();
      // Partial recipes change only the settings they list and leave the rest
      // of the current configuration (including the output path) untouched.
      const partialSettings = recipe.settings;
      const nextFormat = partialSettings.format ?? format;
      if (partialSettings.format !== undefined) setFormat(partialSettings.format);
      if (partialSettings.sizeLimitMb !== undefined) setSizeLimitMb(partialSettings.sizeLimitMb);
      if (partialSettings.audioEnabled !== undefined) {
        const requestedAudio = nextFormat === "mp3" || partialSettings.audioEnabled;
        const sourceHasAudio = probe ? probe.hasAudio : true;
        autoMutedRef.current = requestedAudio && !sourceHasAudio;
        setAudioEnabled(requestedAudio && sourceHasAudio);
      }
      if (partialSettings.normalizeAudio !== undefined) setNormalizeAudio(partialSettings.normalizeAudio);
      if (partialSettings.perturbFirstFrame !== undefined) setPerturbFirstFrame(partialSettings.perturbFirstFrame);
      if (partialSettings.strictFit !== undefined) setStrictFit(partialSettings.strictFit);
      showRecipeNotification(`Applied ${recipe.label}.`);
      return;
    }
    applyFullRecipeSettings(recipe.settings, recipe.label, { resetOutput: true });
  }

  function applyUserRecipe(recipe: UserRecipe) {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current
    ) return;
    applyFullRecipeSettings(recipe.settings, recipe.name, { resetOutput: false });
  }

  function openCreateRecipeDialog() {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current
    ) return;
    if (userRecipeStore.readOnly) {
      showPersistentRecipeStatus("Saved recipes were created by a newer app version and cannot be changed here.");
      return;
    }
    setAboutOpen(false);
    setRecipeNameDraft(`Custom recipe ${userRecipeStore.recipes.length + 1}`);
    setRecipeDescriptionDraft("");
    setRecipeResetToCurrentSettings(false);
    setRecipeDialogError(null);
    setRecipeDialog({ kind: "create" });
  }

  function openEditRecipeDialog(recipe: UserRecipe) {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current ||
      userRecipeStore.readOnly
    ) return;
    setAboutOpen(false);
    setRecipeNameDraft(recipe.name);
    setRecipeDescriptionDraft(recipe.description);
    setRecipeResetToCurrentSettings(false);
    setRecipeDialogError(null);
    setRecipeDialog({ kind: "edit", recipeId: recipe.id, recipeName: recipe.name });
  }

  function requestDeleteRecipe() {
    if (!recipeDialog || recipeDialog.kind !== "edit") return;
    setRecipeDialogError(null);
    setRecipeDialog({
      kind: "delete",
      recipeId: recipeDialog.recipeId,
      recipeName: recipeNameDraft.trim() || recipeDialog.recipeName,
    });
  }

  function cancelRecipeDialog() {
    if (recipeDialog?.kind === "delete") {
      setRecipeDialogError(null);
      setRecipeDialog({
        kind: "edit",
        recipeId: recipeDialog.recipeId,
        recipeName: recipeDialog.recipeName,
      });
      return;
    }
    closeRecipeDialog();
  }

  function closeRecipeDialog() {
    setRecipeDialog(null);
    setRecipeNameDraft("");
    setRecipeDescriptionDraft("");
    setRecipeResetToCurrentSettings(false);
    setRecipeDialogError(null);
  }

  function persistRecipeMutation(
    nextRecipes: UserRecipe[],
    successMessage: string,
    options: { focusSaveButton?: boolean } = {},
  ) {
    const persisted = persistUserRecipeStore(localStorage, userRecipeStore, nextRecipes);
    if (!persisted.ok) {
      setRecipeDialogError(persisted.error);
      showPersistentRecipeStatus(persisted.error);
      return false;
    }
    setUserRecipeStore(persisted.store);
    showRecipeNotification(successMessage);
    closeRecipeDialog();
    if (options.focusSaveButton) {
      window.setTimeout(() => focusButtonWhenAvailable(recipeSaveButtonRef), 0);
    }
    return true;
  }

  function confirmRecipeDialog() {
    if (!recipeDialog) return;

    if (recipeDialog.kind === "create") {
      const created = createUserRecipe(
        userRecipeStore.recipes,
        recipeNameDraft,
        recipeDescriptionDraft,
        currentRecipeSettings,
      );
      if (!created.ok) {
        setRecipeDialogError(created.error);
        return;
      }
      persistRecipeMutation(created.recipes, `Saved ${created.recipe.name}.`);
      return;
    }

    if (recipeDialog.kind === "edit") {
      const updated = updateUserRecipe(userRecipeStore.recipes, recipeDialog.recipeId, {
        name: recipeNameDraft,
        description: recipeDescriptionDraft,
        ...(recipeResetToCurrentSettings ? { settings: currentRecipeSettings } : {}),
      });
      if (!updated.ok) {
        setRecipeDialogError(updated.error);
        return;
      }
      persistRecipeMutation(updated.recipes, `Updated ${updated.recipe.name}.`);
      return;
    }

    const removed = deleteUserRecipe(userRecipeStore.recipes, recipeDialog.recipeId);
    if (!removed.ok) {
      setRecipeDialogError(removed.error);
      return;
    }
    persistRecipeMutation(removed.recipes, `Deleted ${removed.recipe.name}.`, { focusSaveButton: true });
  }

  useEffect(() => {
    if (format !== "mp3") return;
    setAudioEnabled(true);
  }, [format]);

  useEffect(() => {
    if (isVideoCodecCompatible(format, advancedVideoCodec)) return;
    setAdvancedVideoCodec("auto");
  }, [format, advancedVideoCodec]);

  useEffect(() => {
    if (encodeCapabilities || encodeCapabilitiesError) return;

    let stop = false;
    (async () => {
      try {
        const capabilities = await invoke<EncodeCapabilities>("encode_capabilities");
        if (stop) return;
        setEncodeCapabilities(capabilities);
        setEncodeCapabilitiesError(null);
      } catch (error) {
        if (stop) return;
        setEncodeCapabilitiesError(coerceErrorMessage(error, "Failed to read encoder capabilities."));
      }
    })();

    return () => {
      stop = true;
    };
  }, [encodeCapabilities, encodeCapabilitiesError]);

  useEffect(() => {
    if (!smokeConfig || smokeAppliedRef.current) return;

    smokeAppliedRef.current = true;
    smokeStartRef.current = false;
    smokeAttemptIdRef.current = null;
    smokeMetricsRef.current = null;
    smokeInteractionRunningRef.current = false;
    smokeInteractionDoneRef.current = false;
    smokeG7UiRunningRef.current = false;
    smokeG7UiDoneRef.current = false;
    smokeG7ActiveChecksRunningRef.current = false;
    smokeG7ActiveChecksDoneRef.current = false;
    smokeG7EvidenceRef.current = smokeConfig.g7Operation
      ? createSmokeG7Evidence(smokeConfig.g7Operation)
      : null;
    smokeG7ExportButtonRef.current = null;
    smokeG7ExportRectRef.current = null;
    smokeG7BeforeCancelInvokeRef.current = null;
    smokeG7CancelStagePromiseRef.current = null;

    handleDroppedPaths([smokeConfig.inputPath]);
    setOutputAuto(false);
    setOutputPath(smokeConfig.outputPath);
    setFormat(smokeConfig.format);
    setTitle(smokeConfig.title ?? "");
    setSizeLimitMb(formatNumberInput(smokeConfig.sizeLimitMb));
    setResizeMode(smokeConfig.resizeMode ?? "source");
    setMaxEdgePx(smokeConfig.resizeMaxEdgePx === null || smokeConfig.resizeMaxEdgePx === undefined ? "" : formatNumberInput(smokeConfig.resizeMaxEdgePx));
    setCustomWidthPx(smokeConfig.resizeWidthPx === null || smokeConfig.resizeWidthPx === undefined ? "" : formatNumberInput(smokeConfig.resizeWidthPx));
    setCustomHeightPx(smokeConfig.resizeHeightPx === null || smokeConfig.resizeHeightPx === undefined ? "" : formatNumberInput(smokeConfig.resizeHeightPx));
    setOutputAspectLocked(true);
    setAudioEnabled(smokeConfig.audioEnabled !== false);
    setStripMetadata(smokeConfig.stripMetadata !== false);
    const smokeStrictFit = smokeConfig.format !== "mp3" && smokeConfig.sizeLimitMb > 0 && smokeConfig.strictFit === true;
    setStrictFit(smokeStrictFit);
    clearExternalSubtitle("No external subtitles selected.");
    if (smokeConfig.subtitlePath) {
      const configuredSubtitlePath = smokeConfig.subtitlePath;
      const token = subtitleInspectionTokenRef.current + 1;
      subtitleInspectionTokenRef.current = token;
      setSubtitleInspecting(true);
      void invoke<SubtitleInspection>("inspect_srt", { path: configuredSubtitlePath })
        .then((inspection) => {
          if (subtitleInspectionTokenRef.current !== token) return;
          setSubtitlePath(configuredSubtitlePath);
          setSubtitleInspection(inspection);
          setSubtitleError(null);
          setSubtitleStatus(
            `${basename(configuredSubtitlePath)} selected, ${inspection.cueCount} cue${inspection.cueCount === 1 ? "" : "s"} validated.`,
          );
        })
        .catch((error) => {
          if (subtitleInspectionTokenRef.current !== token) return;
          const message = safeSubtitleError(error, configuredSubtitlePath);
          setSubtitleError(message);
          void reportSmokeFailure(`Packaged app smoke subtitle validation failed: ${message}`);
        })
        .finally(() => {
          if (subtitleInspectionTokenRef.current === token) setSubtitleInspecting(false);
        });
    }
    setNormalizeAudio(false);
    setPerturbFirstFrame(smokeConfig.perturbFirstFrame ?? false);
    setColorPolicy(smokeConfig.colorPolicy ?? "auto");
    autoMutedRef.current = false;
    setAdvancedVideoCodec("auto");
    setAdvancedAudioBitrateKbps("auto");
    setAdvancedVideoQuality("auto");
    setAdvancedEncodeSpeed("auto");
    setAdvancedFrameRateCapFps("auto");
    setAdvancedAudioChannels("auto");
    setTrimStart(formatNumberInput(smokeConfig.trimStartS));
    setTrimEnd(smokeConfig.trimEndS === null || smokeConfig.trimEndS === undefined ? "" : formatNumberInput(smokeConfig.trimEndS));
    setTrimDragSnapS("0");
    setReverse(smokeConfig.reverse ?? false);
    setLoopVideo(smokeConfig.loopVideo ?? false);
    setSpeed("1.0");
    setRotateDeg(0);
    setCropEnabled(false);
    setCropRect({ x: 0, y: 0, w: 1, h: 1 });
    setAspectLocked(false);
    setAspectPreset("free");
    setCropDetectHint(null);
    setBrightness("0");
    setContrast("1");
    setSaturation("1");
    setStatus("Smoke: preparing packaged interaction check…");
    void reportSmokeStatus("input-applied", {
      message: "Smoke input staged through the shared drop path.",
      outputPath: smokeConfig.outputPath,
    });
  }, [smokeConfig]);

  useEffect(() => {
    setTrimDragSnapS((current) => normalizeTrimDragSnapInput(current, probe?.durationS ?? null));
  }, [probe?.durationS]);

  const parsedSizeLimitMb = Number(sizeLimitMb);
  const exactSizeTargetBytes = exactTargetBytesFromMegabytes(parsedSizeLimitMb);
  const sizeLimitEnabled = sizeLimitMb.trim() !== "" && parsedSizeLimitMb > 0;
  const sizeTargetExactnessBlockingReason =
    sizeLimitEnabled && exactSizeTargetBytes === null
      ? SIZE_TARGET_EXACTNESS_ERROR
      : null;
  useEffect(() => {
    if (format !== "mp3" && sizeLimitEnabled) return;
    setStrictFit(false);
  }, [format, sizeLimitEnabled]);
  const shapedVideoDimensions = useMemo(() => {
    if (!probe) return null;
    return dimensionsAfterShape(probe, cropEnabled, cropRect, rotateDeg);
  }, [probe, cropEnabled, cropRect, rotateDeg]);
  const squarePixelVideoDimensions = useMemo(() => {
    if (!probe || !shapedVideoDimensions) return null;
    if (!hasNonSquarePixels(probe)) return shapedVideoDimensions;
    return squarePixelDimensions({
      probe,
      width: shapedVideoDimensions.width,
      height: shapedVideoDimensions.height,
      manualRotateDeg: rotateDeg,
    });
  }, [probe, rotateDeg, shapedVideoDimensions]);

  const plannedSummary = useMemo(() => {
    if (!probe) return null;

    const sizeMb = Number(sizeLimitMb);
    if (!Number.isFinite(sizeMb) || sizeMb < 0) return null;
    if (sizeMb > 0 && exactTargetBytesFromMegabytes(sizeMb) === null) return null;

    const speedNum = Number(speed);
    if (!Number.isFinite(speedNum) || speedNum <= 0) return null;

    const startRaw = trimStart.trim() === "" ? 0 : Number(trimStart);
    if (!Number.isFinite(startRaw) || startRaw < 0) return null;

    const endRaw = trimEnd.trim() === "" ? null : Number(trimEnd);
    if (endRaw !== null && (!Number.isFinite(endRaw) || endRaw < 0)) return null;

    // Mirror the request clamping so size planning never budgets beyond the clip.
    const startS = Math.min(startRaw, probe.durationS);
    const endS = Math.min(endRaw ?? probe.durationS, probe.durationS);
    if (endS <= startS) return null;

    const baseDurationS = Math.max(0.001, (endS - startS) / speedNum);
    const durationS = format !== "mp3" && loopVideo ? baseDurationS * 2 : baseDurationS;
    const sizeLimitEnabled = sizeMb > 0;
    const totalKbps = sizeLimitEnabled
      ? Math.max(1, Math.floor((sizeMb * 1_000_000 * 8 * 0.95) / durationS / 1000))
      : null;

    if (format === "mp3") {
      return {
        durationS,
        totalKbps,
        sizeLimitEnabled,
        audioIncluded: Boolean(probe.hasAudio),
        w: null as number | null,
        h: null as number | null,
        videoCodec: null as string | null,
      };
    }

    if (!squarePixelVideoDimensions) return null;

    let w = squarePixelVideoDimensions.width;
    let h = squarePixelVideoDimensions.height;

    if (resizeMode === "maxEdge") {
      const maxEdge = maxEdgePx.trim() === "" ? null : Number(maxEdgePx);
      if (maxEdge === null || !Number.isFinite(maxEdge) || maxEdge < OUTPUT_DIMENSION_MIN_PX || maxEdge > OUTPUT_DIMENSION_MAX_PX) {
        return null;
      }
      const resized = hasNonSquarePixels(probe) && shapedVideoDimensions
        ? fitMaxEdgeDisplayDimensions({
            probe,
            width: shapedVideoDimensions.width,
            height: shapedVideoDimensions.height,
            maxEdge,
            manualRotateDeg: rotateDeg,
          })
        : fitMaxEdgeDimensions(w, h, maxEdge);
      w = resized.width;
      h = resized.height;
    } else if (resizeMode === "custom") {
      const customWidth = customWidthPx.trim() === "" ? null : Number(customWidthPx);
      const customHeight = customHeightPx.trim() === "" ? null : Number(customHeightPx);
      if (
        customWidth === null ||
        customHeight === null ||
        !Number.isFinite(customWidth) ||
        !Number.isFinite(customHeight) ||
        customWidth < OUTPUT_DIMENSION_MIN_PX ||
        customHeight < OUTPUT_DIMENSION_MIN_PX ||
        customWidth > OUTPUT_DIMENSION_MAX_PX ||
        customHeight > OUTPUT_DIMENSION_MAX_PX
      ) {
        return null;
      }
      w = evenPixel(customWidth);
      h = evenPixel(customHeight);
    }

    const sourceVideoCopyCompatibleForPlanning =
      format === "mp4"
        ? probe.videoCodec === "h264" || probe.videoCodec === "mpeg4"
        : probe.videoCodec === "vp8" || probe.videoCodec === "vp9";
    const planningCrop = cropEnabled ? cropRectToPixels(cropRect, probe.width, probe.height) : null;
    const planningCropIsActive = Boolean(
      planningCrop && !isFullFramePixelCrop(planningCrop, probe.width, probe.height),
    );
    const planningFrameRateCap = Number(advancedFrameRateCapFps);
    const planningFrameRateCapApplies = advancedFrameRateCapFps !== "auto" &&
      Number.isFinite(planningFrameRateCap) &&
      (probe.frameRate === null || probe.frameRate === undefined ||
        probe.frameRate * speedNum > planningFrameRateCap + 0.01);
    const videoEncodeAlreadyDefinite =
      !sourceVideoCopyCompatibleForPlanning ||
      trimRequestIsActive({ startS: startRaw, endS: endRaw, durationS: probe.durationS }) ||
      planningCropIsActive ||
      reverse ||
      loopVideo ||
      rotateDeg !== 0 ||
      timelineSpeedChanges(speedNum) ||
      resizeMode !== "source" ||
      !colorIsDefault(brightness, contrast, saturation) ||
      perturbFirstFrame ||
      Boolean(subtitlePath) ||
      colorPolicy === "standardSdr" ||
      hasNonSquarePixels(probe) ||
      advancedVideoCodec !== "auto" ||
      (!sizeLimitEnabled && advancedVideoQuality !== "auto") ||
      advancedEncodeSpeed !== "auto" ||
      planningFrameRateCapApplies;
    const encodedDimensions = encodedOutputDimensions(w, h, videoEncodeAlreadyDefinite);
    w = encodedDimensions.width;
    h = encodedDimensions.height;

    const capabilityDefaultCodec = encodeCapabilities?.videoCodecs.find(
      (codec) => codec.format === format && codec.available && codec.isDefault,
    )?.value;
    const compatibleSourcePlanningCodec = sourceVideoCopyCompatibleForPlanning
      ? probe.videoCodec
      : null;
    const planningVideoCodec = advancedVideoCodec === "auto"
      ? capabilityDefaultCodec ?? compatibleSourcePlanningCodec ?? (format === "mp4" ? "h264" : "vp9")
      : advancedVideoCodec;
    const audioIncluded = audioEnabled && probe.hasAudio;
    const sourceAudioCopyCompatibleForPlanning =
      format === "mp4"
        ? probe.audioCodec === "aac"
        : probe.audioCodec === "opus" || probe.audioCodec === "vorbis";
    const retainedAudioForcesEncode = sizeLimitEnabled && audioIncluded &&
      (!sourceAudioCopyCompatibleForPlanning || normalizeAudio || advancedAudioChannels !== "auto");
    if (!videoEncodeAlreadyDefinite && retainedAudioForcesEncode) {
      const audioFallbackDimensions = encodedOutputDimensions(w, h, true);
      w = audioFallbackDimensions.width;
      h = audioFallbackDimensions.height;
    }

    return { durationS, totalKbps, sizeLimitEnabled, audioIncluded, w, h, videoCodec: planningVideoCodec };
  }, [
    probe,
    sizeLimitMb,
    speed,
    trimStart,
    trimEnd,
    format,
    loopVideo,
    squarePixelVideoDimensions,
    shapedVideoDimensions,
    resizeMode,
    maxEdgePx,
    customWidthPx,
    customHeightPx,
    cropEnabled,
    cropRect,
    reverse,
    rotateDeg,
    brightness,
    contrast,
    saturation,
    perturbFirstFrame,
    colorPolicy,
    advancedVideoCodec,
    advancedVideoQuality,
    advancedEncodeSpeed,
    advancedFrameRateCapFps,
    advancedAudioChannels,
    normalizeAudio,
    audioEnabled,
    subtitlePath,
    encodeCapabilities,
  ]);

  const previewDurationS = probe?.durationS ?? 0;
  const clampedPreviewTimeS = clamp(previewTimeS, 0, previewDurationS || 0);
  const trimTimeline = useMemo<TrimTimeline | null>(() => {
    if (!probe) return null;

    const rawStart = trimStart.trim() === "" ? 0 : Number(trimStart);
    const safeStart = Number.isFinite(rawStart) ? clamp(rawStart, 0, probe.durationS) : 0;

    const rawEnd = trimEnd.trim() === "" ? null : Number(trimEnd);
    const safeEnd =
      rawEnd === null || !Number.isFinite(rawEnd)
        ? probe.durationS
        : clamp(rawEnd, safeStart, probe.durationS);

    const minGap = Math.min(TRIM_MIN_GAP_S, probe.durationS);
    const end = Math.max(safeEnd, Math.min(probe.durationS, safeStart + minGap));
    const start = clamp(safeStart, 0, Math.max(0, end - minGap));

    return {
      start,
      end,
      hasCustomEnd: trimEnd.trim() !== "",
      minGap,
    };
  }, [probe, trimStart, trimEnd]);

  useEffect(() => {
    trimTimelineRef.current = trimTimeline;
  }, [trimTimeline]);

  const trimSummary = useMemo(() => {
    if (!probe) return null;

    const startRaw = trimStart.trim() === "" ? 0 : Number(trimStart);
    if (!Number.isFinite(startRaw) || startRaw < 0) return null;

    const endRaw = trimEnd.trim() === "" ? null : Number(trimEnd);
    if (endRaw !== null && (!Number.isFinite(endRaw) || endRaw < 0 || endRaw <= startRaw)) return null;

    const start = clamp(startRaw, 0, probe.durationS);
    const end = endRaw === null ? null : clamp(endRaw, 0, probe.durationS);

    if (end === null) {
      if (start <= 0) return `Full clip, ${formatClock(probe.durationS)} total.`;
      return `From ${formatClock(start)} to end (${formatClock(Math.max(0, probe.durationS - start))}).`;
    }

    return `From ${formatClock(start)} to ${formatClock(end)} (${formatClock(Math.max(0, end - start))}).`;
  }, [probe, trimStart, trimEnd]);

  const trimDragSnapInputMaxS = probe ? Math.min(TRIM_DRAG_SNAP_MAX_S, Math.max(0, Math.floor(probe.durationS))) : TRIM_DRAG_SNAP_MAX_S;
  const trimDragSnapIntervalS = Number(normalizeTrimDragSnapInput(trimDragSnapS, probe?.durationS ?? null));

  const cropPixelRect = useMemo(
    () => (probe ? cropRectToPixels(cropRect, probe.width, probe.height) : null),
    [cropRect, probe],
  );
  const encodedCropPixelRect = useMemo(
    () => (cropPixelRect ? alignCropRectForEncoding(cropPixelRect) : null),
    [cropPixelRect],
  );

  const cropSummary = useMemo(() => {
    if (!probe || !cropEnabled || !cropPixelRect || !encodedCropPixelRect) return null;

    const isFull = isFullFramePixelCrop(cropPixelRect, probe.width, probe.height);

    return isFull
      ? "Full frame selected."
      : encodedCropPixelRect.width === cropPixelRect.width && encodedCropPixelRect.height === cropPixelRect.height
        ? `${encodedCropPixelRect.width}x${encodedCropPixelRect.height} at ${encodedCropPixelRect.x},${encodedCropPixelRect.y}.`
        : `${encodedCropPixelRect.width}x${encodedCropPixelRect.height} encoded crop at ${encodedCropPixelRect.x},${encodedCropPixelRect.y} (requested ${cropPixelRect.width}x${cropPixelRect.height}; output dimensions require even values).`;
  }, [probe, cropEnabled, cropPixelRect, encodedCropPixelRect]);

  const inputSummary = useMemo(() => {
    if (!inputPath) return "Drop a video anywhere in the window or browse for one below.";
    if (!probe) return "Probing input…";
    return `${probe.width}x${probe.height} • ${formatClock(probe.durationS)}${probe.hasAudio ? " • audio" : " • no audio"}`;
  }, [inputPath, probe]);

  const outputModeSummary = outputAuto
    ? "Output name follows the input and selected format automatically."
    : "Using a custom output destination.";

  const sourceBadgeText = probe
    ? `${probe.width}x${probe.height} • ${formatClock(probe.durationS)}`
    : inputPath
      ? "Preparing source…"
      : "No source loaded";

  const advancedAudioBitrateValue = advancedAudioBitrateKbps === "auto" ? null : Number(advancedAudioBitrateKbps);
  const advancedAudioBitrateValid =
    advancedAudioBitrateValue !== null &&
    AUDIO_BITRATE_PRESETS_KBPS.includes(advancedAudioBitrateValue as (typeof AUDIO_BITRATE_PRESETS_KBPS)[number]);
  const advancedAudioBitrateRequest = advancedAudioBitrateValid ? advancedAudioBitrateValue : null;
  const advancedFrameRateCapValue = advancedFrameRateCapFps === "auto" ? null : Number(advancedFrameRateCapFps);
  const advancedFrameRateCapValid =
    advancedFrameRateCapValue !== null &&
    FRAME_RATE_CAP_PRESETS_FPS.includes(advancedFrameRateCapValue as (typeof FRAME_RATE_CAP_PRESETS_FPS)[number]);
  const advancedFrameRateCapRequest = advancedFrameRateCapValid ? advancedFrameRateCapValue : null;
  const advancedAudioApplies = !sizeLimitEnabled && advancedAudioBitrateRequest !== null;
  const sourceFrameRate = probe?.frameRate ?? null;
  const frameRatePlan = effectiveFrameRatePlan({
    format,
    sourceFrameRate,
    speed: Number(speed),
    frameRateCapFps: advancedFrameRateCapRequest,
  });
  const frameRateCapApplies = frameRatePlan.capApplies;
  const audioOverrideCanApply =
    format === "mp3" ? Boolean(probe?.hasAudio) : audioEnabled && Boolean(probe?.hasAudio);
  const advancedAudioChannelsApplies = advancedAudioChannels !== "auto" && audioOverrideCanApply;
  const normalizeAudioApplies = normalizeAudio && audioOverrideCanApply;
  const advancedVideoQualityApplies = format !== "mp3" && !sizeLimitEnabled && advancedVideoQuality !== "auto";
  const advancedEncodeSpeedApplies = format !== "mp3" && advancedEncodeSpeed !== "auto";
  const advancedCodecSummary =
    format === "mp3"
      ? "No video codec"
      : advancedVideoCodec === "auto"
        ? "Automatic safe codec plan, verified after export"
        : formatVideoCodecLabel(advancedVideoCodec);
  const sourceStreamSummary = probe
    ? [
        `${probe.videoCodec ?? "unknown"} video (stream ${probe.videoStreamIndex})`,
        probe.audioStreamIndex === null || probe.audioStreamIndex === undefined
          ? "no audio"
          : `${probe.audioCodec ?? "unknown"} audio (stream ${probe.audioStreamIndex})`,
        probe.sourceFormat ? `container ${probe.sourceFormat}` : null,
      ]
        .filter(Boolean)
        .join(" • ")
    : "Probe the source to inspect its selected streams.";
  const sourceColorSummary = probe
    ? [
        probe.dynamicRange === "hdr10"
          ? "HDR10"
          : probe.dynamicRange === "hlg"
            ? "HLG"
            : probe.dynamicRange === "dolbyVision"
              ? "Dolby Vision"
              : probe.dynamicRange === "unknown"
                ? probe.colorTransfer === "smpte2084"
                  ? "Ambiguous PQ/HDR metadata"
                  : "SDR metadata not declared"
                : "SDR",
        probe.bitDepth ? `${probe.bitDepth}-bit` : null,
        probe.pixelFormat ?? null,
      ]
        .filter(Boolean)
        .join(" • ")
    : "Color metadata pending";
  const sourceSarSummary = probe?.sampleAspectRatio
    ? `${probe.sampleAspectRatio.numerator}:${probe.sampleAspectRatio.denominator} SAR${
        probe.displayAspectRatio
          ? ` • ${(probe.displayAspectRatio.numerator / probe.displayAspectRatio.denominator).toFixed(3)} display aspect`
          : ""
      }`
    : "Square pixels assumed";
  const advancedQualitySummary =
    format === "mp3"
      ? "No video quality"
      : advancedVideoQuality === "auto"
        ? sizeLimitEnabled
          ? "Size target controls quality"
          : "Auto quality"
        : sizeLimitEnabled
          ? `${VIDEO_QUALITY_LABELS[advancedVideoQuality]} when size target is off`
          : VIDEO_QUALITY_LABELS[advancedVideoQuality];
  const advancedSpeedSummary =
    format === "mp3"
      ? "No video speed"
      : advancedEncodeSpeed === "auto"
        ? "Auto encode speed"
        : `${ENCODE_SPEED_LABELS[advancedEncodeSpeed]} encode`;
  const advancedAudioSummary =
    advancedAudioBitrateRequest === null
      ? "Auto audio bitrate"
      : sizeLimitEnabled
        ? `${advancedAudioBitrateRequest} kbps when size target is off`
        : `${advancedAudioBitrateRequest} kbps audio`;
  const advancedFrameRateSummary =
    format === "mp3"
      ? "No video frame rate"
      : advancedFrameRateCapRequest === null
        ? "Source frame rate"
        : frameRatePlan.postSpeedFps === null
          ? `Cap at ${advancedFrameRateCapRequest} fps, source rate unavailable`
          : frameRatePlan.capApplies
            ? `Cap at ${advancedFrameRateCapRequest} fps from ${frameRatePlan.postSpeedFps.toFixed(1)} fps after speed`
            : `Cap ${advancedFrameRateCapRequest} fps, effective rate is already ${frameRatePlan.postSpeedFps.toFixed(1)} fps`;
  const advancedAudioChannelsSummary =
    advancedAudioChannels === "auto"
      ? "Auto audio channels"
      : !audioOverrideCanApply
        ? `${AUDIO_CHANNEL_LABELS[advancedAudioChannels]} when audio is included`
        : `${AUDIO_CHANNEL_LABELS[advancedAudioChannels]} audio`;
  const advancedOverrideCount =
    (advancedVideoCodec === "auto" ? 0 : 1) +
    (advancedAudioBitrateRequest === null ? 0 : 1) +
    (advancedVideoQuality === "auto" ? 0 : 1) +
    (advancedEncodeSpeed === "auto" ? 0 : 1) +
    (advancedFrameRateCapRequest === null ? 0 : 1) +
    (advancedAudioChannels === "auto" ? 0 : 1) +
    (normalizeAudio ? 1 : 0);
  const trimStartValue = trimStart.trim() === "" ? 0 : Number(trimStart);
  const trimEndValue = trimEnd.trim() === "" ? null : Number(trimEnd);
  const trimIsActive = trimRequestIsActive({
    startS: trimStartValue,
    endS: trimEndValue,
    durationS: probe?.durationS ?? null,
  });
  const trimForcesReencode = trimIsActive;
  const cropFilterPlanned = Boolean(
    cropEnabled && cropPixelRect && probe && !isFullFramePixelCrop(cropPixelRect, probe.width, probe.height),
  );
  const externalSubtitleActive = Boolean(subtitlePath);
  const hasVideoEditTransforms =
    trimForcesReencode ||
    cropFilterPlanned ||
    reverse ||
    loopVideo ||
    rotateDeg !== 0 ||
    timelineSpeedChanges(speed) ||
    (format !== "mp3" && resizeMode !== "source") ||
    !colorIsDefault(brightness, contrast, saturation) ||
    (format !== "mp3" && perturbFirstFrame) ||
    (format !== "mp3" && colorPolicy === "standardSdr") ||
    (format !== "mp3" && hasNonSquarePixels(probe)) ||
    (format !== "mp3" && externalSubtitleActive);
  const advancedForcesReencode =
    format !== "mp3" &&
    (advancedVideoCodec !== "auto" ||
      advancedVideoQualityApplies ||
      advancedEncodeSpeedApplies ||
      frameRateCapApplies ||
      advancedAudioApplies ||
      advancedAudioChannelsApplies ||
      normalizeAudioApplies);
  const reusableAudioEnabled = audioEnabled || autoMutedRef.current;
  const currentRecipeSettings = useMemo(
    () => ({
      format,
      sizeLimitMb,
      resize: {
        mode: resizeMode,
        maxEdgePx,
        widthPx: customWidthPx,
        heightPx: customHeightPx,
        lockAspect: outputAspectLocked,
      },
      audioEnabled: reusableAudioEnabled,
      normalizeAudio,
      perturbFirstFrame: format === "mp3" ? false : perturbFirstFrame,
      strictFit,
      advanced: {
        videoCodec: advancedVideoCodec,
        audioBitrateKbps: advancedAudioBitrateRequest,
        videoQuality: advancedVideoQuality,
        encodeSpeed: advancedEncodeSpeed,
        frameRateCapFps: advancedFrameRateCapRequest,
        audioChannels: advancedAudioChannels,
      },
    }),
    [
      format,
      sizeLimitMb,
      resizeMode,
      maxEdgePx,
      customWidthPx,
      customHeightPx,
      outputAspectLocked,
      reusableAudioEnabled,
      normalizeAudio,
      perturbFirstFrame,
      strictFit,
      advancedVideoCodec,
      advancedAudioBitrateRequest,
      advancedVideoQuality,
      advancedEncodeSpeed,
      advancedFrameRateCapRequest,
      advancedAudioChannels,
    ],
  );
  const matchingFullBuiltInRecipe = useMemo(
    () => EXPORT_RECIPES.find((recipe) => !recipe.partial && recipeMatchesSettings(recipe, currentRecipeSettings)) ?? null,
    [currentRecipeSettings],
  );
  const matchingUserRecipe = useMemo(
    () => findMatchingUserRecipe(userRecipeStore.recipes, currentRecipeSettings),
    [currentRecipeSettings, userRecipeStore.recipes],
  );
  const matchingPartialBuiltInRecipe = useMemo(
    () => EXPORT_RECIPES.find((recipe) => recipe.partial && recipeMatchesSettings(recipe, currentRecipeSettings)) ?? null,
    [currentRecipeSettings],
  );
  const matchingRecipeLabel = matchingUserRecipe?.name ?? matchingFullBuiltInRecipe?.label ?? matchingPartialBuiltInRecipe?.label ?? null;
  const videoCodecCapabilitiesForFormat = useMemo(
    () => encodeCapabilities?.videoCodecs.filter((codec) => codec.format === format) ?? [],
    [encodeCapabilities, format],
  );
  const selectedVideoCodecCapability =
    advancedVideoCodec === "auto"
      ? null
      : videoCodecCapabilitiesForFormat.find((codec) => codec.value === advancedVideoCodec) ?? null;
  const selectedVideoCodecUnavailable =
    selectedVideoCodecCapability !== null && selectedVideoCodecCapability.available === false;
  const fallbackVideoCodecOptions: VideoCodecPreference[] =
    format === "mp4" ? ["h264", "mpeg4"] : format === "webm" ? ["vp9", "vp8"] : [];
  const videoCodecOptions = videoCodecCapabilitiesForFormat.length
    ? videoCodecCapabilitiesForFormat
    : fallbackVideoCodecOptions.map((value) => ({
        format,
        value,
        label: VIDEO_CODEC_LABELS[value],
        ffmpegName: VIDEO_CODEC_FFMPEG_NAMES[value] ?? "",
        available: false,
        isDefault: value === fallbackVideoCodecOptions[0],
      }));
  const colorSource = useMemo(() => classifyColorSource(probe), [probe]);
  const featureCapability = (feature: string) =>
    encodeCapabilities?.features?.find((capability) => capability.name === feature) ?? null;
  const describeMissingFeature = (
    feature: string,
    label: string,
    required?: { encoders?: string[]; filters?: string[] },
  ) => {
    const capability = featureCapability(feature);
    const requiredEncoders = required?.encoders ? new Set(required.encoders) : null;
    const requiredFilters = required?.filters ? new Set(required.filters) : null;
    const missing = [
      ...(capability?.missingEncoders ?? [])
        .filter((name) => requiredEncoders === null || requiredEncoders.has(name))
        .map((name) => `encoder ${name}`),
      ...(capability?.missingFilters ?? [])
        .filter((name) => requiredFilters === null || requiredFilters.has(name))
        .map((name) => `filter ${name}`),
    ];
    if (capability && missing.length === 0) return null;
    return missing.length
      ? `${label} is unavailable because FFmpeg is missing ${missing.join(", ")}.`
      : `${label} is unavailable in the active FFmpeg build.`;
  };
  const externalSubtitleCapability = featureCapability("externalSubtitles");
  const subtitlePickerBlockingReason = (() => {
    if (format === "mp3") return "External subtitles can be burned into MP4 or WebM video only.";
    if (encodeCapabilitiesError) return `Subtitle capability inspection failed: ${encodeCapabilitiesError}`;
    if (!encodeCapabilities) return "Checking whether the active FFmpeg build can burn external subtitles.";
    if (externalSubtitleCapability?.available === true) return null;
    return describeMissingFeature("externalSubtitles", "External subtitles", { filters: ["subtitles"] });
  })();
  const externalSubtitleBlockingReason = externalSubtitleActive ? subtitlePickerBlockingReason : null;
  const colorBlockingReason = (() => {
    if (format === "mp3" || colorSource.kind === "standard") return null;
    if (colorSource.kind === "unsupported") return colorSource.reason;
    if (colorPolicy !== "standardSdr") {
      return `${colorSource.label} source detected. Enable standard SDR conversion before exporting video.`;
    }
    if (format !== "mp4") return "Standard SDR conversion is available for MP4 output only.";
    if (advancedVideoCodec !== "auto" && advancedVideoCodec !== "h264") {
      return "Standard SDR conversion requires Auto or H.264 (libx264) video encoding.";
    }
    return describeMissingFeature("hdrToSdr", "Standard SDR conversion", {
      encoders: ["libx264"],
      filters: probe?.dynamicRange === "hdr10"
        ? ["format", "tonemap", "zscale"]
        : ["format", "zscale"],
    });
  })();
  const sarNormalizationRequired = format !== "mp3" && hasNonSquarePixels(probe);
  const sourceRotationReason = sourceRotationBlockingReason(probe);
  const rotationBlockingReason = format === "mp3" ? null : sourceRotationReason;
  const frameCapabilityBlockingReason = probe
    ? describeMissingFeature("coreExport", "Frame export", { encoders: ["png"] })
    : null;
  const cropDetectCapabilityBlockingReason = probe
    ? describeMissingFeature("coreExport", "Crop detection", { filters: ["cropdetect"] })
    : null;
  const frameExportBlockingReason = frameCapabilityBlockingReason ?? sourceRotationReason ??
    (colorSource.kind === "standard"
      ? hasNonSquarePixels(probe)
        ? "Frame export is unavailable for non-square-pixel sources because PNG output would not preserve the visible display shape."
        : null
      : "Frame export is available only for standard 8-bit SDR sources because it does not apply the video export color conversion policy.");
  const minimumSourceDimensionBlockingReason = format !== "mp3" && probe && (probe.width < 2 || probe.height < 2)
    ? `Video export requires a source at least 2x2 pixels; this source is ${probe.width}x${probe.height}. Audio-only MP3 export remains available.`
    : null;
  const sarBlockingReason = sarNormalizationRequired
    ? describeMissingFeature("sarNormalize", "Square-pixel normalization")
    : null;
  const capabilityInspectionBlockingReason = (() => {
    if (encodeCapabilitiesError) return `FFmpeg capability inspection failed: ${encodeCapabilitiesError}`;
    if (!encodeCapabilities) return "Checking the active FFmpeg capability contract.";
    return null;
  })();
  const transformRequiredFilters = (() => {
    const filters = new Set<string>();
    const retainedAudio = Boolean(probe?.hasAudio) && Boolean(plannedSummary?.audioIncluded);
    if (reverse) {
      if (format !== "mp3") filters.add("reverse");
      if (retainedAudio) {
        filters.add("asetnsamples");
        filters.add("areverse");
      }
    }
    if (format !== "mp3" && loopVideo) {
      filters.add("split");
      filters.add("reverse");
      filters.add("concat");
      if (retainedAudio) {
        filters.add("asetnsamples");
        filters.add("asplit");
        filters.add("areverse");
      }
    }
    return [...filters];
  })();
  const transformCapabilityBlockingReason = transformRequiredFilters.length
    ? describeMissingFeature("reverseLoop", "The selected Reverse and Loop plan", {
        filters: transformRequiredFilters,
      })
    : null;
  const sourceVideoCopyCompatible =
    format === "mp4"
      ? probe?.videoCodec === "h264" || probe?.videoCodec === "mpeg4"
      : format === "webm"
        ? probe?.videoCodec === "vp8" || probe?.videoCodec === "vp9"
      : false;
  const sizeTargetSourceAudioCopyCompatible =
    format === "mp4"
      ? probe?.audioCodec === "aac"
      : format === "webm"
        ? probe?.audioCodec === "opus" || probe?.audioCodec === "vorbis"
        : false;
  const sizeTargetSpeedChangesTimeline = timelineSpeedChanges(speed);
  const sizeTargetRetainedAudioNeedsEncode = Boolean(plannedSummary?.audioIncluded) &&
    (trimIsActive ||
      reverse ||
      loopVideo ||
      sizeTargetSpeedChangesTimeline ||
      normalizeAudio ||
      advancedAudioChannels !== "auto" ||
      !sizeTargetSourceAudioCopyCompatible);
  const sizeTargetRequiresVideoEncoder = sizeLimitEnabled &&
    (!sourceVideoCopyCompatible || sizeTargetRetainedAudioNeedsEncode);
  const currentPlanRequiresVideoEncoder =
    format !== "mp3" &&
    (hasVideoEditTransforms ||
      !sourceVideoCopyCompatible ||
      sizeTargetRequiresVideoEncoder ||
      advancedVideoCodec !== "auto" ||
      advancedVideoQualityApplies ||
      advancedEncodeSpeedApplies ||
      frameRateCapApplies);
  const plannedEncodeSummary = useMemo(() => {
    if (!plannedSummary || format === "mp3" || plannedSummary.w === null || plannedSummary.h === null) {
      return plannedSummary;
    }
    const dimensions = encodedOutputDimensions(
      plannedSummary.w,
      plannedSummary.h,
      currentPlanRequiresVideoEncoder,
    );
    return { ...plannedSummary, w: dimensions.width, h: dimensions.height };
  }, [plannedSummary, format, currentPlanRequiresVideoEncoder]);
  // Sample export always replaces the current trim with a short exact trim.
  const exactSampleRequiresVideoEncoder = format !== "mp3";
  const exactSamplePlannedEncodeSummary = useMemo(() => {
    if (!plannedSummary || format === "mp3" || plannedSummary.w === null || plannedSummary.h === null) {
      return plannedSummary;
    }
    const dimensions = encodedOutputDimensions(plannedSummary.w, plannedSummary.h, true);
    return { ...plannedSummary, w: dimensions.width, h: dimensions.height };
  }, [plannedSummary, format]);
  const sourceDimensionBlockingReason = minimumSourceDimensionBlockingReason ??
    (format !== "mp3" && currentPlanRequiresVideoEncoder &&
      plannedEncodeSummary && plannedEncodeSummary.w !== null && plannedEncodeSummary.h !== null
      ? codecOutputDimensionBlockingReason(
          plannedEncodeSummary.videoCodec,
          plannedEncodeSummary.w,
          plannedEncodeSummary.h,
        )
      : null);
  const exactSampleSourceDimensionBlockingReason = minimumSourceDimensionBlockingReason ??
    (format !== "mp3" && exactSamplePlannedEncodeSummary &&
      exactSamplePlannedEncodeSummary.w !== null && exactSamplePlannedEncodeSummary.h !== null
      ? codecOutputDimensionBlockingReason(
          exactSamplePlannedEncodeSummary.videoCodec,
          exactSamplePlannedEncodeSummary.w,
          exactSamplePlannedEncodeSummary.h,
        )
      : null);
  const outputDimensionsSummary = useMemo(() => {
    if (format === "mp3") return "No video dimensions for MP3 output";
    if (resizeMode === "source") {
      if (!plannedEncodeSummary || plannedEncodeSummary.w === null || plannedEncodeSummary.h === null) {
        return "Original source dimensions";
      }
      if (hasNonSquarePixels(probe)) {
        return `Square pixels ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} (source ${shapedVideoDimensions?.width ?? probe?.width}x${shapedVideoDimensions?.height ?? probe?.height})`;
      }
      if (
        currentPlanRequiresVideoEncoder &&
        squarePixelVideoDimensions &&
        (plannedEncodeSummary.w !== squarePixelVideoDimensions.width || plannedEncodeSummary.h !== squarePixelVideoDimensions.height)
      ) {
        return `Encoded ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} (source ${squarePixelVideoDimensions.width}x${squarePixelVideoDimensions.height}; encoded output requires even dimensions)`;
      }
      return `Original ${plannedEncodeSummary.w}x${plannedEncodeSummary.h}`;
    }
    if (resizeMode === "maxEdge") {
      const maxEdge = maxEdgePx.trim();
      return maxEdge ? `Max edge ${maxEdge} px` : "Set a max edge";
    }
    const width = customWidthPx.trim();
    const height = customHeightPx.trim();
    const widthNum = Number(width);
    const heightNum = Number(height);
    return width && height && Number.isFinite(widthNum) && Number.isFinite(heightNum)
      ? `Custom ${evenPixel(widthNum)}x${evenPixel(heightNum)}`
      : "Set custom width and height";
  }, [
    format,
    resizeMode,
    probe,
    shapedVideoDimensions,
    squarePixelVideoDimensions,
    maxEdgePx,
    customWidthPx,
    customHeightPx,
    plannedEncodeSummary,
    currentPlanRequiresVideoEncoder,
  ]);
  const strictFitPolicySummary = summarizeStrictFitPolicy({
    strictFit: format !== "mp3" && sizeLimitEnabled && strictFit,
    audioEnabled: format !== "mp3" && audioEnabled && probe?.hasAudio !== false,
  });
  const subtitlePlanSummary = subtitlePath && subtitleInspection
    ? `${basename(subtitlePath)}, ${subtitleInspection.cueCount} cue${subtitleInspection.cueCount === 1 ? "" : "s"}, source time ${formatClock(subtitleInspection.firstCueStartS)} to ${formatClock(subtitleInspection.lastCueEndS)}`
    : subtitlePath
      ? "External subtitle validation is incomplete."
      : "No external subtitles";
  const autoVideoCodecBlockingReason =
    currentPlanRequiresVideoEncoder &&
    advancedVideoCodec === "auto" &&
    encodeCapabilities &&
    !videoCodecOptions.some((codec) => codec.available)
      ? `No compatible video encoder is available for ${format.toUpperCase()} export.`
      : null;
  const exactSampleAutoVideoCodecBlockingReason =
    exactSampleRequiresVideoEncoder &&
    advancedVideoCodec === "auto" &&
    encodeCapabilities &&
    !videoCodecOptions.some((codec) => codec.available)
      ? `No compatible video encoder is available for ${format.toUpperCase()} exact sample export.`
      : null;
  const retainedAudioForPlan = Boolean(probe?.hasAudio && plannedSummary?.audioIncluded);
  const speedChangesTimeline = timelineSpeedChanges(speed);
  const coreRequiredFiltersFor = (exactTrimRequired: boolean, videoEncoderRequired: boolean) => {
    const filters = new Set<string>();
    if (format !== "mp3") {
      if (exactTrimRequired) {
        filters.add("trim");
        filters.add("setpts");
      }
      if (cropFilterPlanned) filters.add("crop");
      if (rotateDeg !== 0) filters.add("transpose");
      if (resizeMode !== "source") filters.add("scale");
      if (!colorIsDefault(brightness, contrast, saturation)) filters.add("eq");
      if (speedChangesTimeline) filters.add("setpts");
      if (frameRateCapApplies) filters.add("fps");
      if (perturbFirstFrame) filters.add("noise");
      const evenOutputFilterPlanned = cropFilterPlanned || resizeMode !== "source" || sarNormalizationRequired;
      if (
        videoEncoderRequired &&
        !evenOutputFilterPlanned &&
        probe &&
        (probe.width % 2 !== 0 || probe.height % 2 !== 0)
      ) {
        filters.add("crop");
      }
    }
    if (retainedAudioForPlan) {
      if (exactTrimRequired) {
        filters.add("atrim");
        filters.add("asetpts");
      }
      if (normalizeAudio) {
        filters.add("loudnorm");
        filters.add("aresample");
      }
      if (speedChangesTimeline) filters.add("atempo");
    }
    return [...filters];
  };
  const coreRequiredFilters = coreRequiredFiltersFor(trimForcesReencode, currentPlanRequiresVideoEncoder);
  const exactSampleCoreRequiredFilters = coreRequiredFiltersFor(true, exactSampleRequiresVideoEncoder);
  const sourceAudioCopyCompatible =
    format === "mp4"
      ? probe?.audioCodec === "aac"
      : format === "webm"
        ? probe?.audioCodec === "opus" || probe?.audioCodec === "vorbis"
        : false;
  const currentPlanRequiresAudioEncoder =
    format === "mp3" ||
    (retainedAudioForPlan &&
      (trimForcesReencode ||
        reverse ||
        loopVideo ||
        speedChangesTimeline ||
        normalizeAudio ||
        advancedAudioApplies ||
        advancedAudioChannels !== "auto" ||
        !sourceAudioCopyCompatible));
  const exactSampleRequiresAudioEncoder = format === "mp3" || retainedAudioForPlan;
  const coreFeature = featureCapability("coreExport");
  const coreMissingEncoders = new Set(coreFeature?.missingEncoders ?? []);
  const requiredAudioEncodersFor = (audioEncoderRequired: boolean) => !audioEncoderRequired
    ? []
    : format === "mp3"
      ? ["libmp3lame"]
      : format === "mp4"
        ? ["aac"]
        : !coreMissingEncoders.has("libopus")
          ? ["libopus"]
          : ["libvorbis"];
  const requiredAudioEncoders = requiredAudioEncodersFor(currentPlanRequiresAudioEncoder);
  const exactSampleRequiredAudioEncoders = requiredAudioEncodersFor(exactSampleRequiresAudioEncoder);
  const coreCapabilityBlockingReason =
    coreRequiredFilters.length || requiredAudioEncoders.length
      ? describeMissingFeature("coreExport", "The selected export plan", {
          encoders: requiredAudioEncoders,
          filters: coreRequiredFilters,
        })
      : null;
  const exactSampleCoreCapabilityBlockingReason =
    exactSampleCoreRequiredFilters.length || exactSampleRequiredAudioEncoders.length
      ? describeMissingFeature("coreExport", "The exact sample export plan", {
          encoders: exactSampleRequiredAudioEncoders,
          filters: exactSampleCoreRequiredFilters,
        })
      : null;
  const transformMemoryEstimate = useMemo(() => {
    if (!probe || !plannedEncodeSummary || (format !== "mp3" && (plannedEncodeSummary.w === null || plannedEncodeSummary.h === null))) {
      return reverse || (format !== "mp3" && loopVideo)
        ? {
            bytes: null,
            severity: "blocked" as const,
            reason: "Reverse and Loop need a valid trim, speed, and output size before export can start.",
            videoBytes: null,
            audioBytes: null,
          }
        : { bytes: 0, severity: "ok" as const, reason: null, videoBytes: 0, audioBytes: 0 };
    }
    return estimateTransformMemory({
      probe,
      reverse,
      loopVideo: format !== "mp3" && loopVideo,
      trimStartS: trimTimeline?.start ?? 0,
      trimEndS: trimTimeline?.end ?? probe.durationS,
      speed: Number(speed),
      frameRateCapFps: advancedFrameRateCapRequest,
      width: plannedEncodeSummary.w ?? 1,
      height: plannedEncodeSummary.h ?? 1,
      decodedVideoBytesPerPixel:
        format === "mp4" && colorSource.kind === "convertible" && colorPolicy === "standardSdr"
          ? 1.5
          : null,
      normalizeAudio,
      audioEnabled: Boolean(plannedEncodeSummary.audioIncluded),
      videoEnabled: format !== "mp3",
    });
  }, [
    probe,
    plannedEncodeSummary,
    reverse,
    loopVideo,
    format,
    trimTimeline,
    speed,
    advancedFrameRateCapRequest,
    audioEnabled,
    normalizeAudio,
    colorSource,
    colorPolicy,
  ]);
  const transformMemoryBlockingReason = transformMemoryEstimate.severity === "blocked"
    ? transformMemoryEstimate.reason ?? "Reverse or Loop exceeds the decoded-memory safety limit."
    : null;
  const exactSampleSourceDurationS = probe && trimTimeline
    ? Math.min(sampleDurationS, Math.max(0, trimTimeline.end - trimTimeline.start))
    : null;
  const exactSampleTransformMemoryEstimate = useMemo(() => {
    if (
      !probe ||
      exactSampleSourceDurationS === null ||
      !exactSamplePlannedEncodeSummary ||
      (format !== "mp3" && (
        exactSamplePlannedEncodeSummary.w === null ||
        exactSamplePlannedEncodeSummary.h === null
      ))
    ) {
      return reverse || (format !== "mp3" && loopVideo)
        ? {
            bytes: null,
            severity: "blocked" as const,
            reason: "Reverse and Loop need a valid sample duration, speed, and output size before export can start.",
            videoBytes: null,
            audioBytes: null,
          }
        : { bytes: 0, severity: "ok" as const, reason: null, videoBytes: 0, audioBytes: 0 };
    }
    return estimateTransformMemory({
      probe,
      reverse,
      loopVideo: format !== "mp3" && loopVideo,
      trimStartS: 0,
      trimEndS: exactSampleSourceDurationS,
      speed: Number(speed),
      frameRateCapFps: advancedFrameRateCapRequest,
      width: exactSamplePlannedEncodeSummary.w ?? 1,
      height: exactSamplePlannedEncodeSummary.h ?? 1,
      decodedVideoBytesPerPixel:
        format === "mp4" && colorSource.kind === "convertible" && colorPolicy === "standardSdr"
          ? 1.5
          : null,
      normalizeAudio,
      audioEnabled: Boolean(exactSamplePlannedEncodeSummary.audioIncluded),
      videoEnabled: format !== "mp3",
    });
  }, [
    probe,
    exactSampleSourceDurationS,
    exactSamplePlannedEncodeSummary,
    reverse,
    loopVideo,
    format,
    speed,
    advancedFrameRateCapRequest,
    normalizeAudio,
    colorSource,
    colorPolicy,
  ]);
  const exactSampleTransformMemoryBlockingReason = exactSampleTransformMemoryEstimate.severity === "blocked"
    ? exactSampleTransformMemoryEstimate.reason ?? "The exact sample exceeds the decoded-memory safety limit."
    : null;
  const queueOutputBlockingReason =
    outputPath && exportQueueClaimsOutputPath(exportQueueState, outputPath)
      ? "That output path is already reserved by an item in the export queue. Choose another destination or remove the queued item."
      : null;
  const currentPlannedDurationS = plannedEncodeSummary?.durationS ?? null;
  const planSummaryText = useMemo(() => {
    if (!plannedEncodeSummary) return null;
    const shape =
      plannedEncodeSummary.w !== null && plannedEncodeSummary.h !== null
        ? `${plannedEncodeSummary.w}x${plannedEncodeSummary.h}`
        : "audio only";
    const bitrate = plannedEncodeSummary.totalKbps !== null ? `~${plannedEncodeSummary.totalKbps} kbps` : "no size limit";
    return `${shape} • ${formatClock(currentPlannedDurationS ?? plannedEncodeSummary.durationS)} • ${bitrate}`;
  }, [plannedEncodeSummary, currentPlannedDurationS]);
  const trimAccuracySummary = !trimIsActive
    ? "No active trim"
    : "Frame/sample accurate boundaries";
  const encodeModeSummary =
    format === "mp3"
        ? "Audio-only re-encode"
        : externalSubtitleActive
          ? "Force re-encode to burn external subtitles"
          : advancedForcesReencode
            ? "Force re-encode because overrides are active"
            : sizeLimitEnabled
              ? "Re-encode for size target unless copy already fits"
              : hasVideoEditTransforms
                ? "Re-encode for edits"
                : "Auto stream copy when safe";
  const advancedPlanSummary = `${encodeModeSummary} • ${advancedCodecSummary}`;
  const encodeEventBlockingReason = encodeEventsError
    ? encodeEventsError
    : !encodeEventsReady
      ? "Preparing export event handling."
      : null;
  const exactExportPlanBlockingReason =
    encodeEventBlockingReason ??
    sizeTargetExactnessBlockingReason ??
    capabilityInspectionBlockingReason ??
    rotationBlockingReason ??
    sourceDimensionBlockingReason ??
    colorBlockingReason ??
    sarBlockingReason ??
    coreCapabilityBlockingReason ??
    transformCapabilityBlockingReason ??
    transformMemoryBlockingReason ??
    (subtitleInspecting ? "Wait for external subtitle validation to finish." : null) ??
    externalSubtitleBlockingReason ??
    autoVideoCodecBlockingReason ??
    queueOutputBlockingReason ??
    (selectedVideoCodecUnavailable ? "Choose an available codec before exporting." : null);
  const exactSamplePlanBlockingReason =
    encodeEventBlockingReason ??
    sizeTargetExactnessBlockingReason ??
    capabilityInspectionBlockingReason ??
    rotationBlockingReason ??
    exactSampleSourceDimensionBlockingReason ??
    colorBlockingReason ??
    sarBlockingReason ??
    exactSampleCoreCapabilityBlockingReason ??
    transformCapabilityBlockingReason ??
    exactSampleTransformMemoryBlockingReason ??
    (subtitleInspecting ? "Wait for external subtitle validation to finish." : null) ??
    externalSubtitleBlockingReason ??
    exactSampleAutoVideoCodecBlockingReason ??
    (selectedVideoCodecUnavailable ? "Choose an available codec before exporting an exact sample." : null);
  const exportBlockingReason = exactExportPlanBlockingReason;
  const colorHandlingSummary = format === "mp3"
    ? "Video color is not included in MP3 output"
    : colorSource.kind === "standard"
      ? "Standard SDR path"
      : colorSource.kind === "unsupported"
        ? colorSource.reason
        : colorPolicy === "standardSdr"
          ? `${colorSource.label} -> 8-bit BT.709 SDR MP4`
          : `${colorSource.label} needs an explicit SDR conversion choice`;
  const displayHandlingSummary = format === "mp3"
    ? "No video display geometry"
    : rotationBlockingReason
      ? rotationBlockingReason
      : sourceDimensionBlockingReason
        ? sourceDimensionBlockingReason
    : sarNormalizationRequired && plannedEncodeSummary && plannedEncodeSummary.w !== null && plannedEncodeSummary.h !== null
      ? resizeMode === "custom"
        ? `${sourceSarSummary} -> custom ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} square pixels`
        : `${sourceSarSummary} -> ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} square pixels with display shape preserved`
      : resizeMode === "custom" && plannedEncodeSummary && plannedEncodeSummary.w !== null && plannedEncodeSummary.h !== null
        ? `Custom ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} square-pixel dimensions`
        : "Square-pixel display shape preserved";
  const transformMemorySummary = reverse || (format !== "mp3" && loopVideo)
    ? transformMemoryEstimate.bytes === null
      ? transformMemoryEstimate.reason ?? "Decoded-memory estimate unavailable"
      : `${formatByteSize(transformMemoryEstimate.bytes)} decoded buffer (${transformMemoryEstimate.severity})`
    : "No whole-clip Reverse/Loop buffer";

  const activeEditChips = useMemo(() => {
    const chips: string[] = [];

    if (trimIsActive) {
      chips.push("Trim");
    }

    if (cropFilterPlanned) chips.push("Crop");

    const speedNum = Number(speed);
    if (timelineSpeedChanges(speedNum)) {
      chips.push(`${speedNum.toFixed(2).replace(/\.?0+$/, "")}x speed`);
    }

    if (rotateDeg !== 0) chips.push(`Rotate ${rotateDeg}°`);
    if (reverse) chips.push("Reverse");
    if (format !== "mp3" && loopVideo) chips.push("Loop");

    if (!colorIsDefault(brightness, contrast, saturation)) {
      chips.push("Color");
    }

    if (format !== "mp3" && resizeMode === "maxEdge" && maxEdgePx.trim() !== "") chips.push(`Max ${maxEdgePx.trim()} px`);
    if (format !== "mp3" && resizeMode === "custom" && customWidthPx.trim() !== "" && customHeightPx.trim() !== "") {
      chips.push(`${customWidthPx.trim()}x${customHeightPx.trim()}`);
    }
    if (title.trim()) chips.push("Custom title");
    if (format !== "mp3" && probe?.hasAudio && !audioEnabled) chips.push("Muted");
    if (advancedVideoCodec !== "auto" && format !== "mp3") chips.push(VIDEO_CODEC_LABELS[advancedVideoCodec]);
    if (advancedVideoQuality !== "auto" && format !== "mp3") chips.push(VIDEO_QUALITY_LABELS[advancedVideoQuality]);
    if (advancedEncodeSpeed !== "auto" && format !== "mp3") chips.push(`${ENCODE_SPEED_LABELS[advancedEncodeSpeed]} encode`);
    if (advancedFrameRateCapRequest !== null && format !== "mp3") chips.push(`${advancedFrameRateCapRequest} fps cap`);
    if (advancedAudioBitrateRequest !== null) chips.push(`Audio ${advancedAudioBitrateRequest}k`);
    if (advancedAudioChannels !== "auto") chips.push(AUDIO_CHANNEL_LABELS[advancedAudioChannels]);
    if (normalizeAudioApplies) chips.push("Normalized audio");
    if (format !== "mp3" && colorPolicy === "standardSdr") chips.push("Standard SDR");
    if (format !== "mp3" && hasNonSquarePixels(probe)) chips.push("Square pixels");
    if (format !== "mp3" && externalSubtitleActive) chips.push("External subtitles");
    if (format !== "mp3" && sizeLimitEnabled && strictFit) chips.push("Strict Fit");

    return chips;
  }, [
    trimIsActive,
    cropFilterPlanned,
    speed,
    rotateDeg,
    reverse,
    loopVideo,
    brightness,
    contrast,
    saturation,
    resizeMode,
    maxEdgePx,
    customWidthPx,
    customHeightPx,
    title,
    format,
    probe,
    audioEnabled,
    advancedVideoCodec,
    advancedVideoQuality,
    advancedEncodeSpeed,
    advancedFrameRateCapRequest,
    advancedAudioBitrateRequest,
    advancedAudioChannels,
    normalizeAudioApplies,
    colorPolicy,
    externalSubtitleActive,
    sizeLimitEnabled,
    strictFit,
  ]);

  const lastExportSizeText = lastExport?.outputSizeBytes ? `${(lastExport.outputSizeBytes / 1_000_000).toFixed(2)} MB` : null;
  const lastExportDurationText = lastExport?.durationS ? formatClock(lastExport.durationS) : null;
  const lastTargetData = lastExport?.targetResult
    ? targetResultFormatData(lastExport.targetResult)
    : null;
  const lastCorrectiveActions = useMemo(
    () => lastExport?.targetResult?.status === "missed"
      ? strictFitCorrectiveActions({
          targetResult: lastExport.targetResult,
          ...lastExport.correctiveContext,
        })
      : [],
    [lastExport],
  );
  const currentTargetBytesCandidate = exactTargetBytesFromMegabytes(Number(sizeLimitMb));
  const currentExactTargetBytes = currentTargetBytesCandidate !== null && currentTargetBytesCandidate > 0
    ? currentTargetBytesCandidate
    : null;
  const correctiveActionsMatchCurrentPlan = Boolean(
    lastExport?.targetResult &&
    lastExport.correctiveContext.sourcePathIdentity === queuePathIdentity(inputPath) &&
    lastExport.correctiveContext.format === format &&
    lastExport.targetResult.targetBytes === currentExactTargetBytes,
  );
  const progressUi = getActiveProgressUi(encodeProgress, jobId !== null);
  const updateProgressUi = getUpdateProgressUi(updateProgress);
  const attemptUi = deriveEncodeAttemptPresentation(latestAttempt);
  const encodeBusy =
    attemptUi.isActive ||
    queueRunning ||
    queuePreparationBusy ||
    queueSnapshotApplying ||
    subtitleInspecting ||
    cropDetecting ||
    frameSaving ||
    updateBusy;
  const lastExportIsCurrentOutcome =
    latestAttempt.kind === "succeeded" || latestAttempt.kind === "target-missed";
  const latestAttemptOutputPath =
    "outputPath" in latestAttempt ? latestAttempt.outputPath ?? null : null;
  const displayedStatus = latestAttempt.kind === "running" && jobId !== null
    ? progressUi.phase === "finalizing"
      ? "Finalizing output..."
      : progressUi.phase === "copying"
        ? "Copying media..."
        : "Encoding..."
    : status;

  const footerKicker =
    latestAttempt.kind === "running"
      ? progressUi.label
      : attemptUi.kicker
        ? attemptUi.kicker
        : inputPath
        ? probe
          ? "Ready to export"
          : "Preparing input"
        : "Waiting for input";

  const footerMetaText = useMemo(() => {
    if (attemptUi.isActive) return planSummaryText ?? inputSummary;
    if (attemptUi.isFailure || attemptUi.isCancelled) {
      return latestAttemptOutputPath || outputPath || planSummaryText || outputModeSummary;
    }
    if (lastExport) {
      const parts = [
        lastExportDurationText ? `${lastExportDurationText} export` : null,
        lastExportSizeText,
        lastExport.outputPath,
      ].filter(Boolean);
      return parts.join(" • ");
    }
    return outputPath || planSummaryText || outputModeSummary;
  }, [
    attemptUi.isActive,
    attemptUi.isCancelled,
    attemptUi.isFailure,
    lastExport,
    lastExportDurationText,
    lastExportSizeText,
    latestAttemptOutputPath,
    planSummaryText,
    inputSummary,
    outputPath,
    outputModeSummary,
  ]);

  const exportReady = Boolean(inputPath && outputPath && probe && !exportBlockingReason);
  const sampleExportReady = Boolean(inputPath && outputPath && probe && !exactSamplePlanBlockingReason);
  const planHeroReady =
    exportReady && !attemptUi.isActive && !attemptUi.isFailure && !attemptUi.isCancelled && !attemptUi.isTargetMissed;
  const queueCounts = useMemo(() => summarizeExportQueue(exportQueueState), [exportQueueState]);
  const activeQueuePosition = queueActiveItemId === null
    ? null
    : exportQueue.findIndex((item) => item.id === queueActiveItemId) + 1;
  const planStatusText =
    attemptUi.isActive
      ? displayedStatus
      : attemptUi.isTargetMissed
        ? attemptUi.message ?? "The measured artifact is over the requested size target and remains available to open."
      : attemptUi.isFailure || attemptUi.isCancelled
        ? attemptUi.message ?? displayedStatus
      : !inputPath
        ? "Load a source video to unlock the export plan and composing tools."
        : !probe
          ? "Analyzing the source video so the export plan can be calculated."
          : !outputPath
            ? "Pick an output path to enable export."
            : exportBlockingReason
              ? exportBlockingReason
            : lastExportIsCurrentOutcome
              ? "Last export completed. Review the output below or adjust the settings and export another variation."
              : "Source, output path, and current settings are valid. Export is ready.";

  const planWarnings = useMemo(() => {
    const warnings: string[] = [];
    if (!probe || !plannedEncodeSummary) return warnings;

    if (plannedEncodeSummary.sizeLimitEnabled) {
      warnings.push(
        strictFit
          ? strictFitPolicySummary
          : "Strict Fit is off. The requested dimensions and audio are preserved; if the measured output is over target, the artifact is published as a target miss with exact bytes.",
      );
      if (plannedEncodeSummary.totalKbps !== null && plannedEncodeSummary.totalKbps < 160 && format !== "mp3") {
        warnings.push("This target leaves a very low bitrate for video. Lower output dimensions, trim shorter, or raise the size limit.");
      }
      if (
        plannedEncodeSummary.totalKbps !== null &&
        plannedEncodeSummary.w !== null &&
        plannedEncodeSummary.h !== null &&
        plannedEncodeSummary.w * plannedEncodeSummary.h >= 1280 * 720 &&
        plannedEncodeSummary.totalKbps < 700
      ) {
        warnings.push("The current resolution is high for this bitrate. Smaller output dimensions will usually behave better.");
      }
    }

    if (format === "mp3" && !probe.hasAudio) {
      warnings.push("MP3 export needs an input with an audio stream.");
    }

    if (externalSubtitleActive) {
      warnings.push(
        format === "mp3"
          ? "External subtitles cannot be burned into MP3. Choose MP4 or WebM, or remove the subtitle file."
          : "External subtitles will be burned into the video after final geometry and color handling, using source-timeline timing. This forces video re-encoding.",
      );
    }

    if (selectedVideoCodecUnavailable) {
      warnings.push(`${VIDEO_CODEC_LABELS[advancedVideoCodec]} is not available in this FFmpeg build. Use Auto or another available codec.`);
    }

    if (colorSource.kind === "convertible" && format !== "mp3") {
      warnings.push(
        colorPolicy === "standardSdr"
          ? `${colorSource.label} will be converted to 8-bit BT.709 SDR for broad MP4 sharing.`
          : `${colorSource.label} needs an explicit standard SDR conversion choice before video export.`,
      );
    } else if (colorSource.kind === "unsupported" && format !== "mp3") {
      warnings.push(colorSource.reason);
    }

    if (sarNormalizationRequired && plannedEncodeSummary.w !== null && plannedEncodeSummary.h !== null) {
      warnings.push(
        resizeMode === "custom"
          ? `Non-square source pixels will be normalized to the requested ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} square-pixel dimensions. Custom dimensions are authoritative and may change the visible shape.`
          : `Non-square source pixels will be normalized to ${plannedEncodeSummary.w}x${plannedEncodeSummary.h} square pixels while preserving the visible shape.`,
      );
    }

    if (rotationBlockingReason) warnings.push(rotationBlockingReason);
    if (sourceDimensionBlockingReason) warnings.push(sourceDimensionBlockingReason);

    if (transformMemoryEstimate.severity === "warning" && transformMemoryEstimate.bytes !== null) {
      warnings.push(
        `Reverse/Loop may buffer about ${formatByteSize(transformMemoryEstimate.bytes)} of decoded media. Trim shorter or resize smaller if memory is tight.`,
      );
    } else if (transformMemoryEstimate.severity === "blocked") {
      warnings.push(transformMemoryEstimate.reason ?? "Reverse/Loop exceeds the decoded-memory safety limit.");
    }

    if (capabilityInspectionBlockingReason && encodeCapabilitiesError) {
      warnings.push(capabilityInspectionBlockingReason);
    }

    if (sizeLimitEnabled && advancedAudioBitrateRequest !== null) {
      warnings.push("Audio bitrate override is held until no-limit exports; size-targeted exports plan audio bitrate automatically.");
    }

    if (sizeLimitEnabled && advancedVideoQuality !== "auto") {
      warnings.push("Quality override is held until no-limit exports; size-targeted exports use the bitrate planner.");
    }

    if (advancedEncodeSpeed !== "auto" && advancedVideoCodec === "mpeg4") {
      warnings.push("The MPEG-4 fallback codec has no meaningful speed preset; this setting may not change that encoder.");
    }

    if (
      advancedFrameRateCapRequest !== null &&
      frameRatePlan.postSpeedFps !== null &&
      !frameRatePlan.capApplies
    ) {
      warnings.push(
        `Frame-rate cap is set to ${advancedFrameRateCapRequest} fps, but the post-speed rate is already about ${frameRatePlan.postSpeedFps.toFixed(1)} fps.`,
      );
    }

    if (advancedAudioChannels !== "auto" && !audioOverrideCanApply) {
      warnings.push("Audio channel override is waiting for an export with an included audio stream.");
    }

    return warnings;
  }, [
    probe,
    plannedEncodeSummary,
    format,
    audioEnabled,
    selectedVideoCodecUnavailable,
    advancedVideoCodec,
    sizeLimitEnabled,
    advancedAudioBitrateRequest,
    advancedVideoQuality,
    advancedEncodeSpeed,
    advancedFrameRateCapRequest,
    sourceFrameRate,
    frameRatePlan,
    advancedAudioChannels,
    audioOverrideCanApply,
    colorSource,
    colorPolicy,
    sarNormalizationRequired,
    rotationBlockingReason,
    sourceDimensionBlockingReason,
    transformMemoryEstimate,
    capabilityInspectionBlockingReason,
    encodeCapabilitiesError,
    strictFit,
    strictFitPolicySummary,
    externalSubtitleActive,
    trimIsActive,
  ]);

  const handleDroppedPaths = useEffectEvent(async (paths: string[]) => {
    if (updateBusyRef.current) {
      setDragActive(false);
      const message = "Finish the current update before adding media files.";
      setStatus(message);
      return { kind: "status" as const, message, clearDragActive: true };
    }
    const action = resolveDroppedVideoAction({
      paths,
      currentFormat: formatRef.current,
      busy:
        jobIdRef.current !== null ||
        pendingEncodeRef.current !== null ||
        exportQueueStateRef.current.autoRun ||
        queuePreparationCountRef.current > 0 ||
        queueSnapshotApplyingRef.current ||
        subtitleInspecting,
      queueCapacity: exportQueueRemainingCapacity(exportQueueStateRef.current),
    });

    if (action.clearDragActive) {
      setDragActive(false);
    }

    if (action.kind === "status") {
      setStatus(action.message);
      return action;
    }

    if (action.kind === "applyInput") {
      applyNewInput(action.path, action.nextFormat);
      const ignored = action.unsupportedCount + action.duplicateCount;
      if (ignored > 0) {
        setStatus(`Probing ${basename(action.path)}. ${ignored} other dropped file${ignored === 1 ? " was" : "s were"} ignored.`);
      }
    }

    if (action.kind === "queueInputs") {
      try {
        await enqueueInputPaths(action.paths, action);
      } catch (error) {
        setStatus(coerceErrorMessage(error, "Could not add the dropped files to the queue."));
      }
    }

    return action;
  });

  useEffect(() => {
    let stop = false;
    async function run() {
      if (!inputPath) {
        setOutputPath("");
        return;
      }
      if (!outputAuto) return;

      const takenPaths = exportQueueOutputPaths(exportQueueStateRef.current);
      try {
        const suggested = await invoke<string>("suggest_output_path", { inputPath, format, takenPaths });
        if (stop) return;
        setOutputPath(suggested);
      } catch {
        if (stop) return;
        setOutputPath(ensureUniqueOutputPath(suggestOutputPath(inputPath, format), takenPaths));
      }
    }
    run();
    return () => {
      stop = true;
    };
  }, [inputPath, format, outputAuto, outputSuggestNonce, exportQueueState.pathRevision]);

  useEffect(() => {
    if (!inputPath) return;
    if (outputAuto) return;
    setOutputPath((prev) => {
      if (!prev) return suggestOutputPath(inputPath, format);
      return replaceExtension(prev, format);
    });
  }, [format, outputAuto, inputPath]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    const cleanupWindowDrop = bindWindowFileDrop(window, {
      isDropAllowed: () => !modalOpenRef.current,
      onDragActiveChange: setDragActive,
      onPathsDropped: (paths) => {
        if (!modalOpenRef.current) void handleDroppedPaths(paths);
      },
    });

    let unlisten: (() => void) | null = null;
    (async () => {
      try {
        // Drag-drop events are emitted to webview targets only (tauri 2.11
        // manager/webview.rs emit_to_webview), so a Window-scoped listener
        // never receives them; the WebviewWindow surface is required.
        unlisten = await getCurrentWebviewWindow().onDragDropEvent((event) => {
          const p = event.payload;
          if (p.type === "enter" || p.type === "over") {
            if (!modalOpenRef.current) {
              setDragActive(true);
            }
            return;
          }
          if (p.type === "leave") {
            setDragActive(false);
            return;
          }
          if (p.type !== "drop") return;
          if (!modalOpenRef.current) void handleDroppedPaths(p.paths);
        });
      } catch (e) {
        console.warn("Failed to register drag-drop handler:", e);
      }
    })();

    return () => {
      unlisten?.();
      cleanupWindowDrop();
    };
  }, []);

  const videoSrc = useMemo(() => {
    if (!inputPath || !previewReady) return "";
    return convertFileSrc(inputPath);
  }, [inputPath, previewReady]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    let stop = false;
    async function run() {
      if (!inputPath) {
        setPreviewReady(false);
        setPreviewError(null);
        return;
      }

      setPreviewReady(false);
      setPreviewError(null);

      try {
        await invoke("allow_preview_path", { path: inputPath });
        if (stop) return;
        setPreviewReady(true);
      } catch (e) {
        if (stop) return;
        const msg =
          typeof e === "string"
            ? e
            : e instanceof Error
              ? e.message
              : "Failed to enable preview access for the selected file.";
        console.error("allow_preview_path failed:", e);
        setPreviewError(msg);
      }
    }
    run();
    return () => {
      stop = true;
    };
  }, [inputPath]);

  const aspect = useMemo(() => {
    if (!aspectLocked || aspectPreset === "free") return { locked: false, ratio: null };
    const ratio =
      aspectPreset === "1:1"
        ? 1
        : aspectPreset === "16:9"
          ? 16 / 9
          : aspectPreset === "9:16"
            ? 9 / 16
            : aspectPreset === "4:3"
              ? 4 / 3
              : 3 / 4;
    return { locked: true, ratio };
  }, [aspectLocked, aspectPreset]);

  useEffect(() => {
    if (aspectLocked && aspectPreset === "free") {
      setAspectPreset("1:1");
    }
  }, [aspectLocked, aspectPreset]);

  useEffect(() => {
    let stop = false;
    async function run() {
      if (!inputPath) {
        setProbe(null);
        setProbeError(null);
        setCropDetectHint(null);
        return;
      }
      setProbeError(null);
      try {
        const p = await invoke<VideoProbe>("probe_video", { path: inputPath });
        if (stop) return;
        setProbe(p);
        const pendingSnapshot = pendingQueueSnapshotRef.current;
        const preservesAppliedSnapshot =
          pendingSnapshot?.inputPath === inputPath &&
          pendingSnapshot.token === queueSnapshotApplyTokenRef.current;
        if (preservesAppliedSnapshot) {
          pendingQueueSnapshotRef.current = null;
          if (pendingSnapshot?.announce) {
            setStatus(pendingSnapshot.message ?? `Applied the full queue snapshot for ${basename(inputPath)} with a fresh output path.`);
          }
        } else if (!p.hasAudio) {
          // Remember that the mute was automatic so the next input with audio
          // does not silently inherit it.
          if (audioEnabledRef.current) autoMutedRef.current = true;
          setAudioEnabled(false);
        } else if (autoMutedRef.current) {
          autoMutedRef.current = false;
          setAudioEnabled(true);
        }
        if (!p.hasAudio && formatRef.current === "mp3") {
          setFormat("mp4");
        }
        if (!preservesAppliedSnapshot) {
          setCropRect({ x: 0, y: 0, w: 1, h: 1 });
        }
        setCropDetectHint(null);
        if (!preservesAppliedSnapshot) {
          setStatus("Ready.");
        }
      } catch (e) {
        if (stop) return;
        if (pendingQueueSnapshotRef.current?.inputPath === inputPath) {
          pendingQueueSnapshotRef.current = null;
        }
        const msg = typeof e === "string" ? e : e instanceof Error ? e.message : "Failed to probe video.";
        setProbe(null);
        setProbeError(msg);
        setStatus("Probe failed.");
      }
    }
    run();
    return () => {
      stop = true;
    };
  }, [inputPath]);

  useEffect(() => {
    if (!smokeConfig) return;
    if (smokeStageRef.current === SMOKE_SUCCESS_STAGE || smokeStageRef.current === SMOKE_ERROR_STAGE) return;

    if (encodeEventsError) {
      void reportSmokeFailure(ENCODE_EVENT_SETUP_SMOKE_ERROR);
      return;
    }
    if (!encodeEventsReady) return;

    if (probeError) {
      void reportSmokeFailure(`Packaged app smoke probe failed: ${probeError}`);
      return;
    }

    if (!smokeConfig.skipPreviewInteractions && previewError) {
      void reportSmokeFailure(`Packaged app smoke preview failed: ${previewError}`);
      return;
    }

    if (smokeConfig.subtitlePath) {
      if (smokeConfig.format === "mp3") {
        void reportSmokeFailure("Packaged app smoke cannot combine external subtitles with MP3 output.");
        return;
      }
      if (subtitleError) {
        void reportSmokeFailure(`Packaged app smoke subtitle validation failed: ${subtitleError}`);
        return;
      }
      if (subtitleInspecting || subtitlePath !== smokeConfig.subtitlePath || !subtitleInspection) return;
      if (!encodeCapabilities && !encodeCapabilitiesError) return;
      if (externalSubtitleCapability?.available !== true) {
        void reportSmokeFailure(subtitlePickerBlockingReason ?? "Packaged app smoke requires external subtitle support.");
        return;
      }
    }

    if (inputPath !== smokeConfig.inputPath || !probe) return;

    void reportSmokeStatus("probe-ready", {
      message: `Source probed: ${probe.width}x${probe.height}, ${formatClock(probe.durationS)}`,
    });

    if (!smokeWorkflowDoneRef.current) {
      if (smokeWorkflowRunningRef.current) return;
      smokeWorkflowRunningRef.current = true;
      void (async () => {
        const result = await runSmokeWorkflowChecks();
        smokeWorkflowRunningRef.current = false;
        if (!result.ok) {
          await reportSmokeFailure(result.message);
          return;
        }
        smokeWorkflowDoneRef.current = true;
        await reportSmokeStatus("workflow-ready", { message: result.message });
        setStatus("Smoke: queue and recipe workflow checks passed.");
      })();
      return;
    }

    if (smokeConfig.g7Operation && !smokeG7UiDoneRef.current) {
      if (smokeG7UiRunningRef.current) return;
      smokeG7UiRunningRef.current = true;
      void (async () => {
        try {
          const result = await runSmokeG7UiChecks();
          if (!result.ok) {
            await reportSmokeFailure(result.message, {
              g7Evidence: smokeG7EvidenceRef.current,
            });
            return;
          }
          smokeG7UiDoneRef.current = true;
          setStatus("Smoke: G7 operational UI checks passed.");
        } finally {
          smokeG7UiRunningRef.current = false;
        }
      })();
      return;
    }

    if (smokeConfig.skipPreviewInteractions) {
      if (!smokeInteractionDoneRef.current) {
        const trimStartS = Math.max(0, smokeConfig.trimStartS);
        const trimEndS = Math.max(trimStartS, Math.min(probe.durationS, smokeConfig.trimEndS ?? probe.durationS));
        const smokeSpeed = Number(speed);
        const effectiveSmokeSpeed = Number.isFinite(smokeSpeed) && smokeSpeed > 0 ? smokeSpeed : 1;
        const expectedDurationS = (Math.max(0, trimEndS - trimStartS) / effectiveSmokeSpeed) *
          (smokeConfig.loopVideo && smokeConfig.format !== "mp3" ? 2 : 1);
        smokeMetricsRef.current = {
          trimStartS,
          trimEndS,
          expectedDurationS,
        };
        smokeInteractionDoneRef.current = true;
        void reportSmokeStatus("interaction-ready", {
          message: "Headless packaged export smoke skipped preview playback checks after the source probe completed.",
          trimStartS,
          trimEndS,
          expectedDurationS,
        });
        setStatus("Smoke: headless export checks ready.");
      }
    } else {
      if (!previewReady || !previewMediaReady) return;

      void reportSmokeStatus("preview-ready", {
        message: "Preview loaded and ready for packaged export smoke.",
      });

      if (!smokeInteractionDoneRef.current) {
        if (smokeInteractionRunningRef.current) return;

        smokeInteractionRunningRef.current = true;
        void (async () => {
          const result = await runSmokeInteractionChecks();
          smokeInteractionRunningRef.current = false;

          if (!result.ok) {
            await reportSmokeFailure(result.message);
            return;
          }

          smokeInteractionDoneRef.current = true;
          const reportedTrimStartS = result.trimStartS;
          const reportedTrimEndS = result.trimEndS;
          const reportedExpectedDurationS = result.expectedDurationS;
          smokeMetricsRef.current = {
            trimStartS: reportedTrimStartS,
            trimEndS: reportedTrimEndS,
            expectedDurationS: reportedExpectedDurationS,
          };
          await reportSmokeStatus("interaction-ready", {
            message: result.message,
            trimStartS: reportedTrimStartS ?? null,
            trimEndS: reportedTrimEndS ?? null,
            expectedDurationS: reportedExpectedDurationS ?? null,
          });
          setStatus("Smoke: interaction checks passed.");
        })();
        return;
      }
    }

    if (!smokeInteractionDoneRef.current) return;

    if (jobId !== null || smokeStartRef.current) return;

    smokeStartRef.current = true;
    void (async () => {
      const smokeMetrics = smokeMetricsRef.current;
      await reportSmokeStatus("encoding", {
        message:
          smokeMetrics && smokeMetrics.trimStartS !== null && smokeMetrics.trimEndS !== null
            ? `Trim shortcuts kept start at ${smokeMetrics.trimStartS.toFixed(2)}s, end at ${smokeMetrics.trimEndS.toFixed(2)}s before export.`
            : "Starting packaged export smoke job.",
        outputPath: smokeConfig.outputPath,
        trimStartS: smokeMetrics?.trimStartS ?? null,
        trimEndS: smokeMetrics?.trimEndS ?? null,
        expectedDurationS: smokeMetrics?.expectedDurationS ?? null,
      });
      const result = await startEncode({ reportAsSmokeResult: true });
      if (!result.ok) {
        await reportSmokeFailure(`Packaged app smoke encode failed to start: ${result.message}`);
        return;
      }
    })();
  }, [
    smokeConfig,
    inputPath,
    probe,
    probeError,
    previewReady,
    previewMediaReady,
    previewError,
    jobId,
    trimTimeline,
    speed,
    status,
    subtitlePath,
    subtitleInspection,
    subtitleInspecting,
    subtitleError,
    encodeCapabilities,
    encodeCapabilitiesError,
    encodeEventsReady,
    encodeEventsError,
  ]);

  useEffect(() => {
    if (!smokeConfig?.g7Operation || jobId === null) return;
    if (
      encodeProgress.attemptId === null ||
      encodeProgress.jobId !== jobId ||
      smokeAttemptIdRef.current !== encodeProgress.attemptId
    ) return;

    const current = smokeG7EvidenceRef.current;
    if (!current) return;
    const progressElement = document.querySelector<HTMLElement>(
      '[data-smoke-id="encode-progress"][role="progressbar"]',
    );
    if (!progressElement) {
      void reportSmokeFailure("Packaged G7 could not find the mounted encode progressbar.", {
        g7Evidence: current,
      });
      return;
    }

    const numberAttribute = (name: string) => {
      const raw = progressElement.getAttribute(name);
      if (raw === null || !/^-?\d+$/.test(raw)) return null;
      const value = Number(raw);
      return Number.isSafeInteger(value) ? value : null;
    };
    const metaValues = progressElement.querySelectorAll<HTMLElement>(".vfl-progress-meta > span");
    const phaseLabel = metaValues.item(0)?.textContent?.trim() ?? "";
    const percentText = metaValues.item(1)?.textContent?.trim() ?? "";
    const percentMatch = /^(\d+)%$/.exec(percentText);
    const fillElement = progressElement.querySelector<HTMLElement>(".vfl-progress-fill");
    const valueNow = numberAttribute("aria-valuenow");
    const sample: AppSmokeMountedProgressSample = {
      attemptId: encodeProgress.attemptId,
      jobId: encodeProgress.jobId,
      phase: progressUi.phase,
      sourceOverallPct: encodeProgress.overallPct,
      isFinalizing: Boolean(fillElement?.classList.contains("is-finalizing")),
      role: progressElement.getAttribute("role"),
      ariaLabel: progressElement.getAttribute("aria-label"),
      valueMin: numberAttribute("aria-valuemin"),
      valueMax: numberAttribute("aria-valuemax"),
      valueNow,
      valueText: progressElement.getAttribute("aria-valuetext"),
      phaseLabel,
      visiblePercent: percentMatch ? Number(percentMatch[1]) : null,
      fillWidth: fillElement?.style.width ?? "",
    };
    const expectedFillWidth = `${progressUi.percent}%`;
    if (
      sample.role !== "progressbar" ||
      sample.ariaLabel !== "Encoding progress" ||
      sample.valueMin !== 0 ||
      sample.valueMax !== 100 ||
      sample.valueNow !== progressUi.percent ||
      sample.valueNow > 99 ||
      sample.valueText !== progressUi.valueText ||
      sample.phaseLabel !== progressUi.label ||
      sample.visiblePercent !== progressUi.percent ||
      sample.fillWidth !== expectedFillWidth ||
      sample.isFinalizing !== progressUi.isFinalizing ||
      progressElement.getAttribute("aria-hidden") !== null
    ) {
      void reportSmokeFailure("Packaged G7 mounted progressbar accessibility or visible-value contract failed.", {
        g7Evidence: current,
      });
      return;
    }

    const previous = current.mountedProgressHistory[current.mountedProgressHistory.length - 1];
    if (
      previous?.valueNow !== null &&
      previous?.valueNow !== undefined &&
      valueNow !== null &&
      valueNow < previous.valueNow
    ) {
      void reportSmokeFailure("Packaged G7 mounted progressbar visibly regressed.", {
        g7Evidence: current,
      });
      return;
    }
    if (previous && JSON.stringify(previous) === JSON.stringify(sample)) return;
    if (current.mountedProgressHistory.length >= 512) {
      void reportSmokeFailure("Packaged G7 mounted progress evidence exceeded its bounded 512-sample history.", {
        g7Evidence: current,
      });
      return;
    }
    smokeG7EvidenceRef.current = {
      ...current,
      mountedProgressHistory: [...current.mountedProgressHistory, sample],
    };
  }, [
    smokeConfig?.g7Operation,
    jobId,
    encodeProgress.attemptId,
    encodeProgress.jobId,
    encodeProgress.overallPct,
    progressUi.phase,
    progressUi.isFinalizing,
    progressUi.label,
    progressUi.percent,
    progressUi.valueText,
  ]);

  useEffect(() => {
    if (
      !smokeConfig?.g7Operation ||
      jobId === null ||
      smokeG7ActiveChecksDoneRef.current ||
      smokeG7ActiveChecksRunningRef.current
    ) return;

    smokeG7ActiveChecksRunningRef.current = true;
    void (async () => {
      try {
        const result = await runSmokeG7ActiveEncodeChecks();
        if (!result.ok) {
          await reportSmokeFailure(result.message, {
            g7Evidence: smokeG7EvidenceRef.current,
          });
          return;
        }
        smokeG7ActiveChecksDoneRef.current = true;
      } finally {
        smokeG7ActiveChecksRunningRef.current = false;
      }
    })();
  }, [smokeConfig, jobId]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    encodeEventsReadyRef.current = false;
    encodeEventsErrorRef.current = null;
    setEncodeEventsReady(false);
    setEncodeEventsError(null);

    const subscription = installEncodeEventListeners<EncodeFinishedPayload, EncodeProgressPayload>({
      subscribeFinished: (handler) => listen<EncodeFinishedPayload>("encode-finished", (event) => {
        handler(event.payload);
      }),
      subscribeProgress: (handler) => listen<EncodeProgressPayload>("encode-progress", (event) => {
        handler(event.payload);
      }),
      onProgress: (payload) => {
        const pendingEncode = pendingEncodeRef.current;
        if (!acceptsEncodeEvent(pendingEncode, payload)) return;
        appendSmokeG7Progress(payload);
        setEncodeProgress((current) => reduceEncodeProgress(current, payload));
      },
      onFinished: (p) => {
        const pendingEncode = pendingEncodeRef.current;
        if (!acceptsEncodeEvent(pendingEncode, p) || !pendingEncode) return;

        const completedAtMs = Date.now();
        const settlement = settleEncodeFinished(
          pendingEncode,
          latestAttemptRef.current,
          p,
          completedAtMs,
        );
        if (!settlement.accepted || !settlement.context) return;
        const completedContext = settlement.context;
        const completedQueueItemId = completedContext.queueItemId;
        const completedQueueRunId = completedContext.queueRunId;
        updateLatestAttempt(settlement.state);
        pendingEncodeRef.current = settlement.pending;
        setJobId(null);
        jobIdRef.current = null;
        setEncodeProgress(createEncodeProgressState());

        if (smokeAttemptIdRef.current === p.attemptId) {
          smokeAttemptIdRef.current = null;
          const g7Operation = smokeConfigRef.current?.g7Operation ?? null;
          const g7Evidence = smokeG7EvidenceRef.current;
          const expectedMountedActivePhase =
            g7Operation === "copy-progress" ? "copying" : "encoding";
          const mountedProgressReady =
            !g7Operation ||
            (
              g7Evidence?.mountedProgressHistory.some(
                (sample) => sample.phase === expectedMountedActivePhase,
              ) === true &&
              (
                g7Operation === "cancel-drop" ||
                g7Evidence.mountedProgressHistory.some(
                  (sample) => sample.phase === "finalizing",
                )
              )
            );
          const expectedG7Cancellation =
            g7Operation === "cancel-drop" && settlement.state.kind === "cancelled";
          if (!mountedProgressReady) {
            void reportSmokeFailure(
              "Packaged G7 export settled before its mounted progressbar evidence completed.",
              { g7Evidence },
            );
          } else if (expectedG7Cancellation) {
            if (
              !smokeG7ActiveChecksDoneRef.current ||
              !g7Evidence?.dropPreservedInput ||
              g7Evidence.cancelInvokeCount !== 1
            ) {
              void reportSmokeFailure(
                "Packaged G7 cancellation settled before its drop and idempotence evidence completed.",
                { g7Evidence },
              );
            } else {
              void reportSmokeStatus(SMOKE_SUCCESS_STAGE, {
                ok: true,
                message: "The expected packaged export cancellation completed safely.",
                outputPath: null,
                outputSizeBytes: null,
                diagnostics: p.diagnostics ?? null,
                g7Evidence,
              });
            }
          } else if (!p.ok) {
            void reportSmokeFailure(
              `Packaged app smoke encode failed: ${p.message || "Encode failed."}`,
              {
                diagnostics: p.diagnostics ?? null,
                g7Evidence,
              },
            );
          } else if (
            g7Operation &&
            g7Operation !== "copy-progress" &&
            !smokeG7ActiveChecksDoneRef.current
          ) {
            void reportSmokeFailure(
              "Packaged G7 export completed before its mounted active-control evidence finished.",
              { g7Evidence },
            );
          } else {
            const smokeMetrics = smokeMetricsRef.current;
            void reportSmokeStatus(SMOKE_SUCCESS_STAGE, {
              ok: true,
              message: null,
              outputPath: p.outputPath ?? null,
              outputSizeBytes: p.outputSizeBytes ?? null,
              trimStartS: smokeMetrics?.trimStartS ?? null,
              trimEndS: smokeMetrics?.trimEndS ?? null,
              expectedDurationS: smokeMetrics?.expectedDurationS ?? completedContext.durationS,
              targetResult: p.targetResult ?? null,
              diagnostics: p.diagnostics ?? null,
              queueOutcomeKind: p.targetResult?.status === "missed" ? "target-missed" : "done",
              g7Evidence,
            });
          }
        }

        if (!p.ok) {
          const failureMessage =
            settlement.state.kind === "cancelled" ? "Export canceled." : p.message || "Encode failed.";
          let shouldContinueQueue = false;
          if (completedQueueItemId !== null && completedQueueRunId !== null) {
            const nextQueue = dispatchExportQueue({
              type: "settled",
              itemId: completedQueueItemId,
              runId: completedQueueRunId,
              outcome: {
                kind: settlement.state.kind === "cancelled" ? "cancelled" : "failed",
                message: failureMessage,
                outputPath: p.outputPath ?? completedContext.outputPath,
                outputSizeBytes: p.outputSizeBytes ?? null,
                diagnostics: p.diagnostics ?? null,
                completedAtMs,
              },
            });
            shouldContinueQueue = nextQueue.autoRun && nextQueue.active === null;
          }
          setStatus(failureMessage);
          if (shouldContinueQueue) {
            window.setTimeout(() => void startNextQueuedItem(), 0);
          } else if (completedQueueItemId !== null) {
            window.setTimeout(() => focusQueueAfterMutation(queueFallbackButtonRef), 0);
          }
          return;
        }

        if (completedContext.sample && p.outputSizeBytes) {
          const { outputDurationS, fullDurationS } = completedContext.sample;
          setSampleEstimate({
            sampleBytes: p.outputSizeBytes,
            estimateBytes:
              fullDurationS && outputDurationS > 0
                ? Math.round((p.outputSizeBytes * fullDurationS) / outputDurationS)
                : null,
          });
        }

        const targetData = p.targetResult ? targetResultFormatData(p.targetResult) : null;
        const targetMissed = p.targetResult?.status === "missed";
        const exactEvidenceConsistent = !p.targetResult || Boolean(
          targetData &&
          p.outputSizeBytes === targetData.actualBytes &&
          p.diagnostics?.actualSizeBytes === targetData.actualBytes &&
          p.diagnostics?.requestedSizeBytes === targetData.targetBytes,
        );
        const sizeMb = p.outputSizeBytes ? p.outputSizeBytes / 1_000_000 : null;
        const suffix = sizeMb !== null ? ` (${sizeMb.toFixed(2)} MB)` : "";
        const exactTargetSummary = p.targetResult ? summarizeTargetResult(p.targetResult) : null;
        if (!exactEvidenceConsistent) {
          setStatus("Export completed with inconsistent exact-byte target evidence. The artifact was not reclassified in the app.");
        } else if (targetMissed) {
          setStatus(exactTargetSummary ?? "Target missed. The measured artifact is available to open.");
        } else {
          setStatus(`Done${suffix}.${exactTargetSummary ? ` ${exactTargetSummary}` : p.message ? ` ${p.message}` : ""}`);
        }
        const selectedPlan = p.targetResult?.plans.find((plan) => plan.selected) ?? null;
        setLastExport({
          outputPath: p.outputPath ?? completedContext.outputPath,
          outputSizeBytes: p.outputSizeBytes ?? null,
          durationS: completedContext.durationS,
          format: completedContext.format,
          message: p.message ?? null,
          diagnostics: p.diagnostics ?? null,
          targetResult: p.targetResult ?? null,
          correctiveContext: {
            ...completedContext.correctiveContext,
            width: selectedPlan?.width ?? completedContext.correctiveContext.width,
            height: selectedPlan?.height ?? completedContext.correctiveContext.height,
            hasAudio:
              selectedPlan
                ? selectedPlan.audioAction !== null &&
                  selectedPlan.audioAction !== undefined &&
                  selectedPlan.audioAction !== "drop"
                : completedContext.correctiveContext.hasAudio,
          },
          completedAtMs,
        });
        setAppliedCorrectiveKinds([]);
        let shouldContinueQueue = false;
        if (completedQueueItemId !== null && completedQueueRunId !== null) {
          const nextQueue = dispatchExportQueue({
            type: "settled",
            itemId: completedQueueItemId,
            runId: completedQueueRunId,
            outcome: {
              kind: targetMissed ? "target-missed" : "done",
              message: targetMissed ? exactTargetSummary ?? p.message ?? "Target missed." : p.message ?? null,
              outputPath: p.outputPath ?? completedContext.outputPath,
              outputSizeBytes: p.outputSizeBytes ?? null,
              targetResult: p.targetResult ?? null,
              diagnostics: p.diagnostics ?? null,
              completedAtMs,
            },
          });
          shouldContinueQueue = nextQueue.autoRun && nextQueue.active === null;
        }
        if (outputAutoRef.current) {
          setOutputSuggestNonce((n) => n + 1);
        }
        if (shouldContinueQueue) {
          window.setTimeout(() => void startNextQueuedItem(), 0);
        }
      },
      onReady: () => {
        encodeEventsReadyRef.current = true;
        encodeEventsErrorRef.current = null;
        setEncodeEventsError(null);
        setEncodeEventsReady(true);
      },
      onError: () => {
        const message = ENCODE_EVENT_SETUP_ERROR;
        encodeEventsReadyRef.current = false;
        encodeEventsErrorRef.current = message;
        setEncodeEventsReady(false);
        setEncodeEventsError(message);
      },
    });

    return () => {
      encodeEventsReadyRef.current = false;
      subscription.dispose();
    };
  }, []);

  async function pickInput() {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queuePreparationCountRef.current > 0 ||
      queueSnapshotApplyingRef.current
    ) return;
    try {
      const selected = await openDialog({
        multiple: false,
        directory: false,
        title: "Select a video",
        filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "avi", "webm", "m4v"] }],
      });

      if (typeof selected === "string") {
        if (
          jobIdRef.current !== null ||
          pendingEncodeRef.current !== null ||
          exportQueueStateRef.current.autoRun ||
          queuePreparationCountRef.current > 0 ||
          queueSnapshotApplyingRef.current
        ) return;
        const pickedExt = extname(selected).toLowerCase();
        const nextFormat: OutputFormat =
          pickedExt === "mp4" || pickedExt === "webm" ? (pickedExt as OutputFormat) : formatRef.current;

        applyNewInput(selected, nextFormat);
      }
    } catch (e) {
      const msg =
        typeof e === "string"
          ? e
          : e instanceof Error
            ? e.message
            : "Failed to open file picker.";
      console.error("Dialog open failed:", e);
      setStatus(msg);
    }
  }

  async function pickExternalSubtitle() {
    if (
      encodeBusy ||
      !inputPath ||
      subtitlePickerBlockingReason
    ) return;

    try {
      const selected = await openDialog({
        multiple: false,
        directory: false,
        title: "Select external subtitles",
        filters: [{ name: "SubRip subtitles", extensions: ["srt"] }],
      });
      if (typeof selected !== "string") return;
      if (formatRef.current === "mp3") {
        setSubtitleError("External subtitles require MP4 or WebM video output.");
        return;
      }

      const token = subtitleInspectionTokenRef.current + 1;
      subtitleInspectionTokenRef.current = token;
      const sourceAtSelection = inputPathRef.current;
      const formatAtSelection = formatRef.current;
      setSubtitleInspecting(true);
      setSubtitleError(null);
      try {
        const inspection = await invoke<SubtitleInspection>("inspect_srt", { path: selected });
        if (
          subtitleInspectionTokenRef.current !== token ||
          inputPathRef.current !== sourceAtSelection ||
          formatRef.current !== formatAtSelection
        ) return;
        setSubtitlePath(selected);
        setSubtitleInspection(inspection);
        setSubtitleStatus(
          `${basename(selected)} selected, ${inspection.cueCount} cue${inspection.cueCount === 1 ? "" : "s"} validated.`,
        );
      } catch (error) {
        if (subtitleInspectionTokenRef.current !== token) return;
        setSubtitleError(safeSubtitleError(error, selected));
      } finally {
        if (subtitleInspectionTokenRef.current === token) setSubtitleInspecting(false);
      }
    } catch (error) {
      setSubtitleError(safeSubtitleError(error));
    }
  }

  function removeExternalSubtitle() {
    if (!subtitlePath || encodeBusy) return;
    clearExternalSubtitle("External subtitles removed. No export started.", true);
  }

  async function pickOutput() {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queuePreparationCountRef.current > 0 ||
      queueSnapshotApplyingRef.current
    ) return;
    const ext = format;
    const defaultPath = outputPath || (inputPath ? suggestOutputPath(inputPath, ext) : `output.${ext}`);

    try {
      const selected = await saveDialog({
        title: "Save output as…",
        defaultPath,
        filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
      });

      if (typeof selected === "string") {
        if (
          jobIdRef.current !== null ||
          pendingEncodeRef.current !== null ||
          exportQueueStateRef.current.autoRun ||
          queuePreparationCountRef.current > 0 ||
          queueSnapshotApplyingRef.current
        ) return;
        setOutputAuto(false);
        setOutputPath(replaceExtension(selected, format));
      }
    } catch (e) {
      const msg =
        typeof e === "string"
          ? e
          : e instanceof Error
            ? e.message
            : "Failed to open save dialog.";
      console.error("Dialog save failed:", e);
      setStatus(msg);
    }
  }

  function parseNum(label: string, raw: string, opts?: { min?: number; max?: number }) {
    const v = Number(raw);
    if (!Number.isFinite(v)) throw new Error(`${label} must be a number.`);
    if (opts?.min !== undefined && v < opts.min) throw new Error(`${label} must be >= ${opts.min}.`);
    if (opts?.max !== undefined && v > opts.max) throw new Error(`${label} must be <= ${opts.max}.`);
    return v;
  }

  function parseIntNum(label: string, raw: string, opts?: { min?: number; max?: number }) {
    const v = parseNum(label, raw, opts);
    if (!Number.isInteger(v)) throw new Error(`${label} must be an integer.`);
    return v;
  }

  function currentOutputAspectRatio() {
    if (shapedVideoDimensions && shapedVideoDimensions.height > 0) {
      return shapedVideoDimensions.width / shapedVideoDimensions.height;
    }

    const width = parseDimensionDraft(customWidthPx);
    const height = parseDimensionDraft(customHeightPx);
    if (width !== null && height !== null && height > 0) return width / height;

    return 16 / 9;
  }

  function seedCustomDimensions() {
    if (!shapedVideoDimensions) return;
    setCustomWidthPx((current) => current || String(evenPixel(shapedVideoDimensions.width)));
    setCustomHeightPx((current) => current || String(evenPixel(shapedVideoDimensions.height)));
  }

  function handleResizeModeChange(nextMode: ResizeMode) {
    setResizeMode(nextMode);
    if (nextMode === "maxEdge") {
      setMaxEdgePx((current) => current || "720");
    } else if (nextMode === "custom") {
      seedCustomDimensions();
    }
  }

  function handleCustomWidthChange(raw: string) {
    setCustomWidthPx(raw);
    if (!outputAspectLocked) return;

    const width = parseDimensionDraft(raw);
    if (width === null) return;

    const ratio = currentOutputAspectRatio();
    setCustomHeightPx(formatDimensionDraft(width / ratio));
  }

  function handleCustomHeightChange(raw: string) {
    setCustomHeightPx(raw);
    if (!outputAspectLocked) return;

    const height = parseDimensionDraft(raw);
    if (height === null) return;

    const ratio = currentOutputAspectRatio();
    setCustomWidthPx(formatDimensionDraft(height * ratio));
  }

  function handleOutputAspectLockChange(nextLocked: boolean) {
    setOutputAspectLocked(nextLocked);
    if (!nextLocked || resizeMode !== "custom") return;

    const width = parseDimensionDraft(customWidthPx);
    const ratio = currentOutputAspectRatio();
    if (width !== null) {
      setCustomHeightPx(formatDimensionDraft(width / ratio));
      return;
    }

    const height = parseDimensionDraft(customHeightPx);
    if (height !== null) {
      setCustomWidthPx(formatDimensionDraft(height * ratio));
    }
  }

  function buildResizeRequest() {
    if (format === "mp3" || resizeMode === "source") {
      return {
        resize: {
          mode: "source" as ResizeMode,
          maxEdgePx: null,
          widthPx: null,
          heightPx: null,
        },
        legacyMaxEdgePx: null as number | null,
      };
    }

    if (resizeMode === "maxEdge") {
      const maxEdge = parseIntNum("Max edge (px)", maxEdgePx, {
        min: OUTPUT_DIMENSION_MIN_PX,
        max: OUTPUT_DIMENSION_MAX_PX,
      });
      return {
        resize: {
          mode: "maxEdge" as ResizeMode,
          maxEdgePx: maxEdge,
          widthPx: null,
          heightPx: null,
        },
        legacyMaxEdgePx: maxEdge,
      };
    }

    const widthPx = parseIntNum("Output width (px)", customWidthPx, {
      min: OUTPUT_DIMENSION_MIN_PX,
      max: OUTPUT_DIMENSION_MAX_PX,
    });
    const heightPx = parseIntNum("Output height (px)", customHeightPx, {
      min: OUTPUT_DIMENSION_MIN_PX,
      max: OUTPUT_DIMENSION_MAX_PX,
    });

    return {
      resize: {
        mode: "custom" as ResizeMode,
        maxEdgePx: null,
        widthPx,
        heightPx,
      },
      legacyMaxEdgePx: null as number | null,
    };
  }

  function buildAdvancedSettings() {
    return {
      videoCodec: advancedVideoCodec,
      audioBitrateKbps: advancedAudioBitrateRequest,
      videoQuality: advancedVideoQuality,
      encodeSpeed: advancedEncodeSpeed,
      frameRateCapFps: advancedFrameRateCapRequest,
      audioChannels: advancedAudioChannels,
    };
  }

  function buildSettingsOnlyRequest(nextInputPath: string, nextOutputPath: string): EncodeRequest {
    const size = parseNum("Size limit (MB)", sizeLimitMb, { min: 0 });
    if (size !== 0 && size < 0.1) throw new Error("Size limit must be >= 0.1 MB (or 0/empty to disable).");
    if (exactTargetBytesFromMegabytes(size) === null) throw new Error(SIZE_TARGET_EXACTNESS_ERROR);
    const resizeRequest = buildResizeRequest();
    const requestStrictFit = format !== "mp3" && size > 0 && strictFit;

    return {
      inputPath: nextInputPath,
      outputPath: nextOutputPath,
      format,
      title: null,
      sizeLimitMb: size,
      audioEnabled: format === "mp3" ? true : audioEnabled || autoMutedRef.current,
      normalizeAudio,
      stripMetadata,
      // Consent to reinterpret a source's color is source-specific. Files
      // added without probing must make their own explicit choice later.
      colorPolicy: "auto",
      advanced: buildAdvancedSettings(),
      trim: null,
      crop: null,
      reverse: false,
      speed: 1,
      rotateDeg: 0,
      resize: resizeRequest.resize,
      maxEdgePx: resizeRequest.legacyMaxEdgePx,
      color: null,
      perturbFirstFrame: format === "mp3" ? false : perturbFirstFrame,
      loopVideo: false,
      strictFit: requestStrictFit,
      // Same-settings batches intentionally exclude clip-scoped subtitles.
      subtitlePath: null,
    };
  }

  function buildRequest(): EncodeRequest {
    if (!inputPath) throw new Error("Pick an input file first.");
    if (!outputPath) throw new Error("Pick an output path first.");
    if (!probe) throw new Error("Probe not ready yet.");
    if (subtitleInspecting) throw new Error("Wait for external subtitle validation to finish.");
    if (format === "mp3" && !probe.hasAudio) throw new Error("Input has no audio stream (can't export MP3).");
    if (format === "mp3" && subtitlePath) throw new Error("External subtitles require MP4 or WebM video output.");
    if (subtitlePath && (!subtitleInspection || subtitlePickerBlockingReason)) {
      throw new Error(subtitlePickerBlockingReason ?? "Validate the selected subtitle file before exporting.");
    }

    const size = parseNum("Size limit (MB)", sizeLimitMb, { min: 0 });
    if (size !== 0 && size < 0.1) throw new Error("Size limit must be >= 0.1 MB (or 0/empty to disable).");
    if (exactTargetBytesFromMegabytes(size) === null) throw new Error(SIZE_TARGET_EXACTNESS_ERROR);
    const s = parseNum("Speed", speed, { min: 0.05, max: 16 });
    const resizeRequest = buildResizeRequest();
    const requestStrictFit = format !== "mp3" && size > 0 && strictFit;

    const b = brightness.trim() === "" ? 0 : parseNum("Brightness", brightness, { min: -1, max: 1 });
    const c = contrast.trim() === "" ? 1 : parseNum("Contrast", contrast, { min: 0, max: 2 });
    const sat = saturation.trim() === "" ? 1 : parseNum("Saturation", saturation, { min: 0, max: 3 });
    const color: ColorAdjust | null = b !== 0 || c !== 1 || sat !== 1 ? { brightness: b, contrast: c, saturation: sat } : null;

    const startRaw = trimStart.trim() === "" ? 0 : parseNum("Trim start (s)", trimStart, { min: 0 });
    const endRaw = trimEnd.trim() === "" ? null : parseNum("Trim end (s)", trimEnd, { min: 0 });
    // Clamp to the probed clip so typed values past the end cannot skew the
    // backend's size planning.
    const startS = Math.min(startRaw, probe.durationS);
    if (startS >= probe.durationS) throw new Error("Trim start (s) is past the end of the clip.");
    const endS = endRaw === null || endRaw >= probe.durationS ? null : endRaw;
    if (endS !== null && endS <= startS) throw new Error("Trim end (s) must be greater than trim start.");
    const trim = startS > 0 || endS !== null
      ? { startS, endS }
      : null;

    const cropPx = cropEnabled ? cropRectToPixels(cropRect, probe.width, probe.height) : null;
    const cropIsActive = cropPx && !isFullFramePixelCrop(cropPx, probe.width, probe.height);
    const encodedCrop = cropIsActive ? alignCropRectForEncoding(cropPx) : null;
    if (encodedCrop && (encodedCrop.width < 2 || encodedCrop.height < 2)) {
      throw new Error("Crop width and height must each be at least 2 pixels.");
    }
    const crop = encodedCrop;

    const request: EncodeRequest = {
      inputPath,
      outputPath,
      format,
      title: title.trim() ? title.trim() : null,
      sizeLimitMb: size,
      audioEnabled: audioEnabled,
      normalizeAudio,
      stripMetadata,
      colorPolicy,
      advanced: buildAdvancedSettings(),
      trim,
      crop,
      reverse,
      speed: s,
      rotateDeg,
      resize: resizeRequest.resize,
      maxEdgePx: resizeRequest.legacyMaxEdgePx,
      color,
      perturbFirstFrame: format === "mp3" ? false : perturbFirstFrame,
      loopVideo: format === "mp3" ? false : loopVideo,
      strictFit: requestStrictFit,
      subtitlePath: subtitlePath || null,
    };
    return request;
  }

  function claimedOutputPathsForPreparation(): string[] {
    const paths = exportQueueOutputPaths(exportQueueStateRef.current);
    const pendingOutputPath = pendingEncodeRef.current?.outputPath ?? null;
    const pendingIdentity = pendingOutputPath ? queuePathIdentity(pendingOutputPath) : null;
    if (
      pendingOutputPath &&
      pendingIdentity &&
      !paths.some((path) => queuePathIdentity(path) === pendingIdentity)
    ) {
      paths.push(pendingOutputPath);
    }
    return paths;
  }

  async function suggestedOutputForInput(nextInputPath: string, nextFormat: OutputFormat, takenPaths: string[]) {
    try {
      return await invoke<string>("suggest_output_path", {
        inputPath: nextInputPath,
        format: nextFormat,
        takenPaths,
      });
    } catch {
      return ensureUniqueOutputPath(suggestOutputPath(nextInputPath, nextFormat), takenPaths);
    }
  }

  function serializeQueuePreparation<T>(task: () => Promise<T>): Promise<T> {
    queuePreparationCountRef.current += 1;
    setQueuePreparationBusy(true);
    const run = queuePreparationRef.current.then(task, task);
    queuePreparationRef.current = run.then(() => undefined, () => undefined);
    return run.finally(() => {
      queuePreparationCountRef.current = Math.max(0, queuePreparationCountRef.current - 1);
      if (queuePreparationCountRef.current === 0) {
        setQueuePreparationBusy(false);
        const state = exportQueueStateRef.current;
        if (
          state.autoRun &&
          state.active === null &&
          !queueSnapshotApplyingRef.current &&
          state.items.some((item) => item.status === "queued")
        ) {
          window.setTimeout(() => void startNextQueuedItem(), 0);
        }
      }
    });
  }

  function captureQueueResumeIntent() {
    return {
      resume: exportQueueStateRef.current.autoRun,
      stopRevision: queueStopRevisionRef.current,
    };
  }

  function resumeQueueAfterPreparation(
    state: ExportQueueState,
    intent: { resume: boolean; stopRevision: number },
  ) {
    let next = state;
    const hasQueuedItems = () => next.items.some((item) => item.status === "queued");
    if (
      intent.resume &&
      intent.stopRevision === queueStopRevisionRef.current &&
      !next.autoRun &&
      next.active === null &&
      !queueSnapshotApplyingRef.current &&
      hasQueuedItems()
    ) {
      next = dispatchExportQueue({ type: "start-auto-run" });
    }
    if (next.autoRun && next.active === null && hasQueuedItems()) {
      window.setTimeout(() => void startNextQueuedItem(), 0);
    }
    return next;
  }

  function describeQueueIngestion(
    acceptedCount: number,
    counts: { unsupportedCount?: number; duplicateCount?: number; overflowCount?: number },
  ) {
    const parts = [
      counts.unsupportedCount ? `${counts.unsupportedCount} unsupported ignored` : null,
      counts.duplicateCount ? `${counts.duplicateCount} duplicate ignored` : null,
      counts.overflowCount ? `${counts.overflowCount} skipped because the queue is full` : null,
    ].filter(Boolean);
    const accepted = acceptedCount === 1 ? "Queued 1 file." : `Queued ${acceptedCount} files.`;
    return parts.length ? `${accepted} ${parts.join("; ")}.` : accepted;
  }

  async function enqueueInputPaths(
    paths: string[],
    counts: { unsupportedCount?: number; duplicateCount?: number; overflowCount?: number } = {},
  ) {
    if (!paths.length) return 0;
    const requestTemplate = buildSettingsOnlyRequest(
      paths[0],
      `${dirname(paths[0])}${stem(paths[0])}-batch.${format}`,
    );
    const batchFormat = requestTemplate.format;
    const resumeIntent = captureQueueResumeIntent();
    return serializeQueuePreparation(async () => {
      const capacity = exportQueueRemainingCapacity(exportQueueStateRef.current);
      const acceptedPaths = paths.slice(0, capacity);
      const overflowCount = (counts.overflowCount ?? 0) + Math.max(0, paths.length - acceptedPaths.length);
      if (!acceptedPaths.length) {
        setStatus(describeQueueIngestion(0, { ...counts, overflowCount }));
        return 0;
      }

      const takenPaths = claimedOutputPathsForPreparation();
      const prepared: { request: EncodeRequest; durationS: null }[] = [];
      for (const nextInputPath of acceptedPaths) {
        const nextOutputPath = await suggestedOutputForInput(nextInputPath, batchFormat, takenPaths);
        takenPaths.push(nextOutputPath);
        prepared.push({
          request: {
            ...cloneEncodeRequest(requestTemplate),
            inputPath: nextInputPath,
            outputPath: nextOutputPath,
          },
          durationS: null,
        });
      }

      const beforeCount = exportQueueStateRef.current.items.length;
      const next = dispatchExportQueue({ type: "enqueue-prepared", items: prepared });
      const acceptedCount = next.items.length - beforeCount;
      resumeQueueAfterPreparation(next, resumeIntent);
      setStatus(describeQueueIngestion(acceptedCount, { ...counts, overflowCount }));
      return acceptedCount;
    });
  }

  function applyQueueRequestToWorkbench(
    request: EncodeRequest,
    sourceProbe: VideoProbe,
    nextOutputPath: string,
    inspectedSubtitle: SubtitleInspection | null,
  ) {
    const resize = request.resize ?? {
      mode: request.maxEdgePx ? "maxEdge" as const : "source" as const,
      maxEdgePx: request.maxEdgePx ?? null,
      widthPx: null,
      heightPx: null,
    };
    const advanced = request.advanced ?? {};
    const crop = request.crop ?? null;
    const color = request.color ?? null;

    setFormat(request.format);
    setTitle(request.title ?? "");
    setSizeLimitMb(request.sizeLimitMb > 0 ? String(request.sizeLimitMb) : "");
    setResizeMode(request.format === "mp3" ? "source" : resize.mode);
    setMaxEdgePx(resize.mode === "maxEdge" && resize.maxEdgePx ? String(resize.maxEdgePx) : "");
    setCustomWidthPx(resize.mode === "custom" && resize.widthPx ? String(resize.widthPx) : "");
    setCustomHeightPx(resize.mode === "custom" && resize.heightPx ? String(resize.heightPx) : "");
    setOutputAspectLocked(true);
    const requestedAudio = request.format === "mp3" || request.audioEnabled;
    autoMutedRef.current = requestedAudio && !sourceProbe.hasAudio;
    setAudioEnabled(requestedAudio && sourceProbe.hasAudio);
    setNormalizeAudio(request.normalizeAudio);
    setPerturbFirstFrame(request.format === "mp3" ? false : request.perturbFirstFrame);
    setStrictFit(request.format !== "mp3" && request.sizeLimitMb > 0 && request.strictFit === true);
    subtitleInspectionTokenRef.current += 1;
    setSubtitlePath(request.subtitlePath ?? "");
    setSubtitleInspection(inspectedSubtitle);
    setSubtitleInspecting(false);
    setSubtitleError(null);
    setSubtitleStatus(
      request.subtitlePath && inspectedSubtitle
        ? `${basename(request.subtitlePath)} selected, ${inspectedSubtitle.cueCount} cue${inspectedSubtitle.cueCount === 1 ? "" : "s"} validated.`
        : "No external subtitles selected.",
    );
    setColorPolicy(request.format === "mp3" ? "auto" : request.colorPolicy);
    setStripMetadata(request.stripMetadata);
    setAdvancedVideoCodec((advanced.videoCodec ?? "auto") as VideoCodecPreference);
    setAdvancedAudioBitrateKbps(advanced.audioBitrateKbps == null ? "auto" : String(advanced.audioBitrateKbps));
    setAdvancedVideoQuality((advanced.videoQuality ?? "auto") as VideoQualityPreference);
    setAdvancedEncodeSpeed((advanced.encodeSpeed ?? "auto") as EncodeSpeedPreference);
    setAdvancedFrameRateCapFps(advanced.frameRateCapFps == null ? "auto" : String(advanced.frameRateCapFps));
    setAdvancedAudioChannels((advanced.audioChannels ?? "auto") as AudioChannelPreference);

    setTrimStart(request.trim?.startS ? String(request.trim.startS) : "0");
    setTrimEnd(request.trim?.endS == null ? "" : String(request.trim.endS));
    setTrimDragSnapS("0");
    setReverse(request.reverse);
    setLoopVideo(request.format === "mp3" ? false : request.loopVideo);
    setSpeed(String(request.speed));
    setRotateDeg(request.rotateDeg);

    setCropEnabled(Boolean(crop));
    setCropRect(crop
      ? {
          x: clamp(crop.x / sourceProbe.width, 0, 1),
          y: clamp(crop.y / sourceProbe.height, 0, 1),
          w: clamp(crop.width / sourceProbe.width, 0, 1),
          h: clamp(crop.height / sourceProbe.height, 0, 1),
        }
      : { x: 0, y: 0, w: 1, h: 1 });
    setAspectLocked(false);
    setAspectPreset("free");
    setCropDetectHint(null);

    setBrightness(String(color?.brightness ?? 0));
    setContrast(String(color?.contrast ?? 1));
    setSaturation(String(color?.saturation ?? 1));
    setSampleEstimate(null);
    // The snapshot preparer already allocated this collision-free path. Keep
    // it fixed so a later automatic suggestion cannot replace it while the
    // newly applied source is probing.
    setOutputAuto(false);
    setOutputPath(nextOutputPath);
  }

  async function retryQueueItem(itemId: number) {
    const resumeIntent = captureQueueResumeIntent();
    await serializeQueuePreparation(async () => {
      const item = exportQueueStateRef.current.items.find((candidate) => candidate.id === itemId);
      if (!item || (item.status !== "target-missed" && item.status !== "failed" && item.status !== "cancelled")) return;
      const previousOutputPath = item.outputPath;
      const takenPaths = claimedOutputPathsForPreparation();
      const nextOutputPath = await suggestedOutputForInput(item.inputPath, item.format, takenPaths);
      const currentItem = exportQueueStateRef.current.items.find((candidate) => candidate.id === itemId);
      if (
        !currentItem ||
        (currentItem.status !== "target-missed" && currentItem.status !== "failed" && currentItem.status !== "cancelled") ||
        currentItem.outputPath !== previousOutputPath
      ) return;
      const nextRequest = { ...cloneEncodeRequest(item.request), outputPath: nextOutputPath };
      const next = dispatchExportQueue({
        type: "retry-prepared",
        itemId,
        request: nextRequest,
        durationS: item.durationS,
      });
      const retried = next.items.find((candidate) => candidate.id === itemId);
      if (!retried || retried.status !== "queued" || retried.outputPath !== nextOutputPath) return;
      setStatus(`Queued a retry for ${basename(item.inputPath)} with a fresh output path.`);
      const resumed = resumeQueueAfterPreparation(next, resumeIntent);
      const preferredFocus = resumed.autoRun ? queueStopButtonRef : queueRunButtonRef;
      window.setTimeout(() => focusQueueAfterMutation(preferredFocus), 0);
    });
  }

  async function duplicateQueueItem(itemId: number) {
    const resumeIntent = captureQueueResumeIntent();
    await serializeQueuePreparation(async () => {
      const state = exportQueueStateRef.current;
      const item = state.items.find((candidate) => candidate.id === itemId);
      if (!item || item.status === "running") return;
      if (exportQueueRemainingCapacity(state) === 0) {
        setStatus("The export queue is full. Clear finished items before duplicating another plan.");
        return;
      }
      const sourceOutputPath = item.outputPath;
      const takenPaths = claimedOutputPathsForPreparation();
      const nextOutputPath = await suggestedOutputForInput(item.inputPath, item.format, takenPaths);
      const currentItem = exportQueueStateRef.current.items.find((candidate) => candidate.id === itemId);
      if (!currentItem || currentItem.status === "running" || currentItem.outputPath !== sourceOutputPath) return;
      const beforeCount = exportQueueStateRef.current.items.length;
      const next = dispatchExportQueue({
        type: "duplicate-prepared",
        sourceItemId: itemId,
        request: { ...cloneEncodeRequest(item.request), outputPath: nextOutputPath },
        durationS: item.durationS,
      });
      if (next.items.length !== beforeCount + 1) return;
      setStatus(`Duplicated ${basename(item.inputPath)} with a fresh output path.`);
      const resumed = resumeQueueAfterPreparation(next, resumeIntent);
      if (exportQueueRemainingCapacity(resumed) === 0) {
        const preferredFocus = resumed.autoRun ? queueStopButtonRef : queueRunButtonRef;
        window.setTimeout(() => focusQueueAfterMutation(preferredFocus), 0);
      }
    });
  }

  async function applyQueueItemSnapshot(itemId: number) {
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queueSnapshotApplyingRef.current ||
      queuePreparationCountRef.current > 0
    ) return;

    const token = queueSnapshotApplyTokenRef.current + 1;
    queueSnapshotApplyTokenRef.current = token;
    queueSnapshotApplyingRef.current = true;
    setQueueSnapshotApplying(true);
    window.setTimeout(focusQueueAfterMutation, 0);
    let snapshotSubtitlePath: string | null = null;
    try {
      await serializeQueuePreparation(async () => {
        const item = exportQueueStateRef.current.items.find((candidate) => candidate.id === itemId);
        if (!item || item.status === "running") return;
        snapshotSubtitlePath = item.request.subtitlePath ?? null;
        if (item.format === "mp3" && item.request.subtitlePath) {
          throw new Error("That queue snapshot combines MP3 with external subtitles. Choose MP4 or WebM first.");
        }
        if (item.request.subtitlePath && externalSubtitleCapability?.available !== true) {
          throw new Error(subtitlePickerBlockingReason ?? "External subtitles are unavailable in the active FFmpeg build.");
        }
        const requestFingerprint = JSON.stringify(item.request);
        const [sourceProbe, nextOutputPath, inspectedSubtitle] = await Promise.all([
          invoke<VideoProbe>("probe_video", { path: item.inputPath }),
          suggestedOutputForInput(item.inputPath, item.format, claimedOutputPathsForPreparation()),
          item.request.subtitlePath
            ? invoke<SubtitleInspection>("inspect_srt", { path: item.request.subtitlePath })
            : Promise.resolve(null),
        ]);
        if (queueSnapshotApplyTokenRef.current !== token) return;
        if (item.format === "mp3" && !sourceProbe.hasAudio) {
          throw new Error("That queue snapshot requests MP3, but the source has no audio stream.");
        }
        if (
          jobIdRef.current !== null ||
          pendingEncodeRef.current !== null ||
          exportQueueStateRef.current.autoRun ||
          !queueSnapshotApplyingRef.current
        ) return;
        const currentItem = exportQueueStateRef.current.items.find((candidate) => candidate.id === itemId);
        if (!currentItem || currentItem.status === "running" || JSON.stringify(currentItem.request) !== requestFingerprint) return;

        const request = { ...cloneEncodeRequest(item.request), outputPath: nextOutputPath };
        const appliedSnapshotStatus =
          `Applied the full queue snapshot for ${basename(item.inputPath)} with a fresh output path.`;
        const inputChanged = inputPathRef.current !== item.inputPath;
        pendingQueueSnapshotRef.current = inputChanged
          ? {
              token,
              inputPath: item.inputPath,
              announce: true,
              message: appliedSnapshotStatus,
            }
          : null;
        applyQueueRequestToWorkbench(
          request,
          sourceProbe,
          nextOutputPath,
          inspectedSubtitle,
        );
        if (inputChanged) setInputPath(item.inputPath);
        setProbe(sourceProbe);
        setProbeError(null);
        setStatus(appliedSnapshotStatus);
      });
    } catch (error) {
      if (queueSnapshotApplyTokenRef.current === token) {
        pendingQueueSnapshotRef.current = null;
        const message = snapshotSubtitlePath
          ? safeSubtitleError(error, snapshotSubtitlePath)
          : coerceErrorMessage(error, "Could not apply the queue snapshot.");
        if (snapshotSubtitlePath) {
          setOpenCards((cards) => ({ ...cards, output: true }));
          setSubtitleError(message);
        } else {
          setStatus(message);
        }
      }
    } finally {
      if (queueSnapshotApplyTokenRef.current === token) {
        queueSnapshotApplyingRef.current = false;
        setQueueSnapshotApplying(false);
      }
    }
  }

  async function addCurrentPlanToQueue() {
    if (updateBusyRef.current) {
      setStatus("Finish the current update before changing the export queue.");
      return;
    }
    if (!exportReady) return;
    let capturedRequest: EncodeRequest;
    try {
      capturedRequest = buildRequest();
    } catch (error) {
      setStatus(coerceErrorMessage(error, "Failed to capture the current export plan."));
      return;
    }
    const capturedDurationS = currentPlannedDurationS;
    await serializeQueuePreparation(async () => {
      try {
        if (exportQueueRemainingCapacity(exportQueueStateRef.current) === 0) {
          setStatus("The export queue is full. Clear finished items before adding another plan.");
          return;
        }
        // Earlier queue snapshots may already claim this suggested path even
        // though nothing exists on disk yet.
        const outputPath = ensureUniqueOutputPath(capturedRequest.outputPath, claimedOutputPathsForPreparation());
        dispatchExportQueue({
          type: "enqueue-prepared",
          items: [{
            request: outputPath === capturedRequest.outputPath
              ? cloneEncodeRequest(capturedRequest)
              : { ...cloneEncodeRequest(capturedRequest), outputPath },
            durationS: capturedDurationS,
          }],
        });
        setOutputSuggestNonce((nonce) => nonce + 1);
        setStatus(`Queued ${basename(capturedRequest.inputPath)}.`);
      } catch (e) {
        setStatus(coerceErrorMessage(e, "Failed to queue current export."));
      }
    });
  }

  async function addFilesToQueue() {
    if (updateBusyRef.current) {
      setStatus("Finish the current update before changing the export queue.");
      return;
    }
    try {
      const selected = await openDialog({
        multiple: true,
        directory: false,
        title: "Add files to export queue",
        filters: [{ name: "Video", extensions: SUPPORTED_INPUT_EXTENSIONS }],
      });
      const picked = Array.isArray(selected) ? selected : typeof selected === "string" ? [selected] : [];
      const action = resolveDroppedVideoAction({
        paths: picked,
        currentFormat: formatRef.current,
        busy: true,
        queueCapacity: exportQueueRemainingCapacity(exportQueueStateRef.current),
      });
      if (action.kind === "queueInputs") {
        const acceptedCount = await enqueueInputPaths(action.paths, action);
        const state = exportQueueStateRef.current;
        if (acceptedCount > 0 && exportQueueRemainingCapacity(state) === 0) {
          const preferredFocus = state.autoRun ? queueStopButtonRef : queueRunButtonRef;
          window.setTimeout(() => focusQueueAfterMutation(preferredFocus), 0);
        }
      } else if (action.kind === "status") {
        setStatus(action.message);
      }
    } catch (e) {
      setStatus(coerceErrorMessage(e, "Failed to add files to the queue."));
    }
  }

  function removeQueueItem(id: number) {
    const before = exportQueueStateRef.current;
    const next = dispatchExportQueue({ type: "remove", itemId: id });
    if (next !== before && !next.items.some((item) => item.id === id)) {
      window.setTimeout(() => focusQueueAfterMutation(queueFallbackButtonRef), 0);
    }
  }

  function clearCompletedQueueItems() {
    dispatchExportQueue({ type: "clear-terminal" });
    window.setTimeout(() => focusQueueAfterMutation(queueFallbackButtonRef), 0);
  }

  function recordQueueStopIntent() {
    queueStopRevisionRef.current += 1;
    dispatchExportQueue({ type: "stop-auto-run" });
  }

  function stopQueueAfterCurrent() {
    const hasActiveQueueItem =
      exportQueueStateRef.current.active !== null ||
      pendingEncodeRef.current?.queueItemId != null;
    recordQueueStopIntent();
    setStatus(hasActiveQueueItem ? "Queue will stop after the current export." : "Queue stopped.");
    window.setTimeout(() => focusQueueAfterMutation(queueFallbackButtonRef), 0);
  }

  async function startNextQueuedItem() {
    if (!encodeEventsReadyRef.current) {
      dispatchExportQueue({ type: "stop-auto-run" });
      setStatus(
        encodeEventsErrorRef.current
          ? encodeEventsErrorRef.current
          : "Preparing export event handling. Try Run queue again in a moment.",
      );
      return;
    }
    const current = exportQueueStateRef.current;
    if (
      !current.autoRun ||
      current.active !== null ||
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      queuePreparationCountRef.current > 0 ||
      updateBusyRef.current
    ) return;

    const claimed = dispatchExportQueue({ type: "claim-next" });
    const nextItem = getActiveExportQueueItem(claimed);
    const active = claimed.active;
    if (!nextItem || !active) {
      const counts = summarizeExportQueue(claimed);
      const issueParts = [
        counts.missed ? `${counts.missed} target miss${counts.missed === 1 ? "" : "es"}` : null,
        counts.failed ? `${counts.failed} failed` : null,
        counts.cancelled ? `${counts.cancelled} canceled` : null,
      ].filter((part): part is string => Boolean(part));
      setStatus(issueParts.length
        ? `Queue finished with ${issueParts.join(", ")}. ${counts.missed ? "Measured target-miss artifacts remain available to open or adjust." : "Review the item details or retry."}`
        : "Queue complete.");
      window.setTimeout(() => focusQueueAfterMutation(queueFallbackButtonRef), 0);
      return;
    }

    const queuedCount = summarizeExportQueue(claimed).queued + 1;
    const result = await startEncode({
      request: cloneEncodeRequest(nextItem.request),
      durationS: nextItem.durationS,
      startingStatus: `Starting queued export (${queuedCount} remaining)…`,
      queueItemId: nextItem.id,
      queueRunId: active.runId,
    });

    if (!result.ok) {
      const next = dispatchExportQueue({
        type: "start-failed",
        itemId: nextItem.id,
        runId: active.runId,
        message: result.message,
        outputPath: nextItem.request.outputPath,
        completedAtMs: Date.now(),
      });
      if (next.autoRun && next.active === null) {
        window.setTimeout(() => void startNextQueuedItem(), 0);
      }
    }
  }

  function runQueue() {
    if (updateBusyRef.current) {
      setStatus("Finish the current update before running the export queue.");
      return;
    }
    if (!encodeEventsReadyRef.current) {
      setStatus(
        encodeEventsErrorRef.current
          ? encodeEventsErrorRef.current
          : "Preparing export event handling. Try Run queue again in a moment.",
      );
      return;
    }
    const state = exportQueueStateRef.current;
    if (
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      queuePreparationCountRef.current > 0 ||
      state.autoRun ||
      state.active !== null
    ) return;
    const hasQueuedItems = state.items.some((item) => item.status === "queued");
    if (!hasQueuedItems) {
      setStatus("Queue is empty.");
      return;
    }
    dispatchExportQueue({ type: "start-auto-run" });
    window.setTimeout(() => focusQueueAfterMutation(queueStopButtonRef), 0);
    void startNextQueuedItem();
  }

  async function autoDetectCrop() {
    if (!inputPath || !probe) return;
    const requestedPath = inputPath;
    const requestRevision = ++cropDetectionRevisionRef.current;
    setCropDetectHint(null);
    setCropDetecting(true);
    try {
      const crop = await invoke<Crop | null>("detect_crop", { path: inputPath });
      // A slow detection must not apply the old video's crop to a newly
      // loaded input or overwrite a confirmed settings reset.
      if (
        cropDetectionRevisionRef.current !== requestRevision ||
        inputPathRef.current !== requestedPath
      ) return;
      if (!crop) {
        setCropDetectHint("No crop detected.");
        return;
      }
      setCropRect({
        x: crop.x / probe.width,
        y: crop.y / probe.height,
        w: crop.width / probe.width,
        h: crop.height / probe.height,
      });
      setCropEnabled(true);
      setCropDetectHint(`Detected ${crop.width}x${crop.height} @ ${crop.x},${crop.y}.`);
    } catch {
      if (
        cropDetectionRevisionRef.current !== requestRevision ||
        inputPathRef.current !== requestedPath
      ) return;
      setCropDetectHint(CROP_DETECTION_PUBLIC_ERROR);
    } finally {
      if (cropDetectionRevisionRef.current === requestRevision) {
        setCropDetecting(false);
      }
    }
  }

  async function startEncode(options?: {
    request?: EncodeRequest;
    durationS?: number | null;
    startingStatus?: string;
    queueItemId?: number | null;
    queueRunId?: number | null;
    sample?: { outputDurationS: number; fullDurationS: number | null } | null;
    reportAsSmokeResult?: boolean;
  }): Promise<StartEncodeResult> {
    if (updateBusyRef.current) {
      return { ok: false, message: "Finish the current update before starting an export." };
    }
    if (!encodeEventsReadyRef.current) {
      return {
        ok: false,
        message: encodeEventsErrorRef.current
          ? encodeEventsErrorRef.current
          : "Preparing export event handling. Try again in a moment.",
      };
    }
    if (subtitleInspecting) {
      return { ok: false, message: "Wait for external subtitle validation to finish." };
    }
    if (pendingEncodeRef.current !== null) {
      return { ok: false, message: "An export is already starting or running." };
    }
    if (queuePreparationCountRef.current > 0) {
      return { ok: false, message: "Wait for queue preparation to finish before starting an export." };
    }
    if ((options?.queueItemId ?? null) === null && exportQueueStateRef.current.autoRun) {
      return { ok: false, message: "Stop the queue before starting a separate export." };
    }

    attemptIdRef.current += 1;
    const attemptId = attemptIdRef.current;
    updateLatestAttempt(beginEncodeAttempt(attemptId));

    try {
      const request = options?.request ?? buildRequest();
      const requestUsesLoadedSource = request.inputPath === inputPath && probe !== null;
      const sourceRate = requestUsesLoadedSource ? probe.frameRate ?? null : null;
      const speedAdjustedRate = sourceRate === null ? null : sourceRate * request.speed;
      const requestedRateCap = request.advanced?.frameRateCapFps ?? null;
      const plannedFrameRateFps = speedAdjustedRate === null
        ? null
        : requestedRateCap && requestedRateCap > 0
          ? Math.min(speedAdjustedRate, requestedRateCap)
          : speedAdjustedRate;
      pendingEncodeRef.current = {
        attemptId,
        jobId: null,
        cancelRequested: false,
        outputPath: request.outputPath,
        durationS: options?.durationS ?? currentPlannedDurationS,
        format: request.format,
        queueItemId: options?.queueItemId ?? null,
        queueRunId: options?.queueRunId ?? null,
        sample: options?.sample ?? null,
        correctiveContext: {
          format: request.format,
          sourcePathIdentity: queuePathIdentity(request.inputPath),
          width: requestUsesLoadedSource ? plannedEncodeSummary?.w ?? null : null,
          height: requestUsesLoadedSource ? plannedEncodeSummary?.h ?? null : null,
          currentMaxEdgePx:
            request.resize?.mode === "maxEdge" ? request.resize.maxEdgePx ?? null : null,
          plannedFrameRateFps,
          hasAudio: requestUsesLoadedSource ? probe.hasAudio : request.audioEnabled,
          audioEnabled: request.audioEnabled,
          strictFit: request.strictFit,
        },
      };
      if (smokeConfigRef.current && options?.reportAsSmokeResult) {
        smokeAttemptIdRef.current = attemptId;
      }
      setStatus(options?.startingStatus ?? "Starting…");
      setEncodeProgress(createEncodeProgressState());
      const id = await invoke<number>("start_encode", { request, attemptId });
      const binding = bindStartedEncode(
        pendingEncodeRef.current,
        latestAttemptRef.current,
        attemptId,
        id,
      );
      if (binding.accepted && binding.pending) {
        pendingEncodeRef.current = binding.pending;
        setJobId(id);
        jobIdRef.current = id;
        setStatus("Encoding…");
        updateLatestAttempt(binding.state);
      }
      return { ok: true, jobId: id };
    } catch (e) {
      const msg = coerceErrorMessage(e, "Failed to start encode.");
      const attemptedOutputPath =
        pendingEncodeRef.current?.attemptId === attemptId
          ? pendingEncodeRef.current.outputPath
          : options?.request?.outputPath ?? outputPath;
      if (pendingEncodeRef.current?.attemptId === attemptId) {
        pendingEncodeRef.current = null;
      }
      if (smokeAttemptIdRef.current === attemptId) {
        smokeAttemptIdRef.current = null;
      }
      updateLatestAttempt(
        failEncodeAttemptStart(
          latestAttemptRef.current,
          attemptId,
          msg,
          Date.now(),
          attemptedOutputPath,
        ),
      );
      setStatus(msg);
      return { ok: false, message: msg };
    }
  }

  async function exportSample() {
    if (
      !sampleExportReady ||
      !inputPath ||
      !probe ||
      jobId !== null ||
      pendingEncodeRef.current !== null
      || updateBusyRef.current
    ) return;

    try {
      const request = buildRequest();
      const extension = request.format;
      const defaultPath = `${dirname(request.outputPath || inputPath)}${stem(request.outputPath || inputPath)}-sample.${extension}`;
      const selected = await saveDialog({
        title: "Save sample export as…",
        defaultPath,
        filters: [{ name: extension.toUpperCase(), extensions: [extension] }],
      });
      if (typeof selected !== "string") return;
      if (exportQueueClaimsOutputPath(exportQueueStateRef.current, selected)) {
        setStatus("That sample output path is already reserved by an item in the export queue. Choose another destination or remove the queued item.");
        return;
      }

      const sourceStart = request.trim?.startS ?? 0;
      const sourceEnd = request.trim?.endS ?? probe.durationS;
      const availableDuration = Math.max(0.1, sourceEnd - sourceStart);
      const sampleSourceDuration = Math.min(sampleDurationS, availableDuration);
      const anchorTime = clamp(previewTimeRef.current || sourceStart, sourceStart, sourceEnd);
      const latestStart = Math.max(sourceStart, sourceEnd - sampleSourceDuration);
      const sampleStart = clamp(anchorTime - sampleSourceDuration / 2, sourceStart, latestStart);
      const sampleEnd = Math.min(sourceEnd, sampleStart + sampleSourceDuration);
      const outputSampleDuration = Math.max(0.1, (sampleEnd - sampleStart) / request.speed) * (request.loopVideo ? 2 : 1);
      const fullOutputDuration = plannedSummary?.durationS ?? null;

      if (request.sizeLimitMb > 0 && fullOutputDuration && fullOutputDuration > 0) {
        request.sizeLimitMb = Math.max(0.1, request.sizeLimitMb * (outputSampleDuration / fullOutputDuration));
      }

      request.outputPath = selected;
      request.trim = { startS: sampleStart, endS: sampleEnd };

      setSampleEstimate(null);
      await startEncode({
        request,
        durationS: outputSampleDuration,
        startingStatus: "Starting sample export…",
        sample: { outputDurationS: outputSampleDuration, fullDurationS: fullOutputDuration },
      });
    } catch (e) {
      setStatus(coerceErrorMessage(e, "Failed to start sample export."));
    }
  }

  async function cancelEncode() {
    if (jobId === null || latestAttemptRef.current.kind === "cancelling") return;
    const pendingEncode = pendingEncodeRef.current;
    if (pendingEncode?.jobId === jobId) {
      if (pendingEncode.cancelRequested) return;
      const cancellingAttempt = requestEncodeCancellation(
        latestAttemptRef.current,
        pendingEncode.attemptId,
        jobId,
      );
      if (cancellingAttempt === latestAttemptRef.current) return;
      pendingEncode.cancelRequested = true;
      updateLatestAttempt(cancellingAttempt);
    }
    if (exportQueueStateRef.current.active !== null) {
      recordQueueStopIntent();
      setStatus("Canceling queued export…");
    } else {
      setStatus("Canceling…");
    }
    try {
      if (smokeG7EvidenceRef.current?.operation === "cancel-drop") {
        updateSmokeG7Evidence({
          cancelInvokeCount: smokeG7EvidenceRef.current.cancelInvokeCount + 1,
        });
        const beforeBackendInvoke = smokeG7BeforeCancelInvokeRef.current;
        smokeG7BeforeCancelInvokeRef.current = null;
        if (beforeBackendInvoke) await beforeBackendInvoke();
      }
      await invoke("cancel_encode", { jobId });
    } catch {
      // ignore
    }
  }

  async function openFolderFor(path: string) {
    if (!path) return;
    try {
      const dir = dirname(path);
      await openPath(dir || path);
    } catch {
      // ignore
    }
  }

  async function openOutputFolder() {
    const latestAttemptUsesCurrentPlan =
      latestAttempt.kind === "failed" || latestAttempt.kind === "cancelled";
    const p = latestAttemptUsesCurrentPlan
      ? latestAttempt.outputPath || outputPath || inputPath
      : lastExport?.outputPath || outputPath || inputPath;
    if (!p) return;
    await openFolderFor(p);
  }

  async function openOutputFile(pathToOpen?: string) {
    const p = pathToOpen || lastExport?.outputPath || outputPath;
    if (!p) return;
    try {
      await openPath(p);
    } catch {
      // ignore
    }
  }

  async function openExternalUrl(url: string) {
    try {
      await openUrl(url);
    } catch (error) {
      console.warn("Failed to open external URL:", error);
    }
  }

  function applyStrictFitCorrectiveAction(action: StrictFitCorrectiveAction) {
    if (
      encodeBusy ||
      !correctiveActionsMatchCurrentPlan ||
      appliedCorrectiveKinds.includes(action.kind)
    ) return;
    if (action.kind === "reduceMaxEdge") {
      setResizeMode("maxEdge");
      setMaxEdgePx(String(action.maxEdgePx));
    } else if (action.kind === "capFrameRate") {
      setAdvancedFrameRateCapFps(String(action.frameRateCapFps));
    } else if (action.kind === "removeAudio") {
      autoMutedRef.current = false;
      setAudioEnabled(false);
    } else if (action.kind === "enableStrictFit") {
      setStrictFit(true);
    }
    setAppliedCorrectiveKinds((current) => current.includes(action.kind) ? current : [...current, action.kind]);
    setStatus(action.confirmation);
  }

  async function saveCurrentFrame() {
    if (!inputPath || !previewReady || !probe) return;
    if (frameSaving || jobId !== null || pendingEncodeRef.current !== null) return;

    const defaultPath = `${dirname(inputPath)}${stem(inputPath)}-frame.png`;
    const prevStatus = status;

    setFrameSaving(true);
    setStatus("Saving frame…");
    try {
      const selected = await saveDialog({
        title: "Save frame as…",
        defaultPath,
        filters: [{ name: "PNG", extensions: ["png"] }],
      });
      if (typeof selected !== "string") {
        setStatus(prevStatus);
        return;
      }

      await invoke("extract_frame", { inputPath, timeS: previewTimeS, outputPath: selected });
      setStatus("Frame saved.");
    } catch (e) {
      const msg = typeof e === "string" ? e : e instanceof Error ? e.message : "Failed to save frame.";
      setStatus(msg);
    } finally {
      setFrameSaving(false);
    }
  }

  function seekPreview(nextTimeS: number) {
    if (!probe || !previewReady) return;
    cropperRef.current?.seekTo(clamp(nextTimeS, 0, probe.durationS));
  }

  function pausePreviewPlayback() {
    cropperRef.current?.pause();
    previewPlayingRef.current = false;
    setPreviewPlaying(false);
  }

  function syncPreviewToTime(nextTimeS: number, target: TrimFocusTarget, opts?: { pause?: boolean }) {
    activeTrimTargetRef.current = target;
    setActiveTrimTarget(target);
    if (opts?.pause) {
      pausePreviewPlayback();
    }
    const safeTimeS = clamp(nextTimeS, 0, probe?.durationS ?? 0);
    if (target === "preview") {
      previewSelectionTimeRef.current = safeTimeS;
    }
    seekPreview(safeTimeS);
  }

  function updateTrimTarget(target: Exclude<TrimFocusTarget, "preview">, nextTimeS: number, opts?: { pause?: boolean }) {
    const nextValue =
      target === "start"
        ? setTrimStartValue(nextTimeS)
        : setTrimEndValue(nextTimeS, { preferEmptyAtEnd: true });
    if (typeof nextValue !== "number") return;
    syncPreviewToTime(nextValue, target, opts);
  }

  function updateTrimTargetFromClientX(target: Exclude<TrimFocusTarget, "preview">, clientX: number) {
    const track = trimTimelineTrackRef.current;
    const timeline = trimTimelineRef.current;
    if (!track || !timeline || previewDurationS <= 0) return;

    const rect = track.getBoundingClientRect();
    if (rect.width <= 0) return;

    const ratio = clamp((clientX - rect.left) / rect.width, 0, 1);
    const rawTimeS = ratio * previewDurationS;
    if (trimDragSnapIntervalS <= 0) {
      updateTrimTarget(target, rawTimeS, { pause: true });
      return;
    }

    const snapStepS = trimDragSnapIntervalS;
    const candidates: number[] = [];
    if (target === "start") {
      const maxStart = Math.max(0, timeline.end - timeline.minGap);
      for (let timeS = 0; timeS <= maxStart + 0.0001; timeS += snapStepS) {
        candidates.push(Number(timeS.toFixed(6)));
      }
      if (candidates.length === 0) {
        candidates.push(0);
      }
    } else {
      const minEnd = Math.min(previewDurationS, timeline.start + timeline.minGap);
      const firstSnap = Math.ceil(minEnd / snapStepS) * snapStepS;
      for (let timeS = firstSnap; timeS <= previewDurationS + 0.0001; timeS += snapStepS) {
        candidates.push(Number(timeS.toFixed(6)));
      }
      if (candidates.length === 0 || Math.abs(candidates[candidates.length - 1] - previewDurationS) > 0.0001) {
        candidates.push(previewDurationS);
      }
    }

    const snappedTimeS = candidates.reduce((best, candidate) =>
      Math.abs(candidate - rawTimeS) < Math.abs(best - rawTimeS) ? candidate : best,
    );
    updateTrimTarget(target, snappedTimeS, { pause: true });
  }

  function beginTrimHandleDrag(target: Exclude<TrimFocusTarget, "preview">, event: ReactPointerEvent<HTMLButtonElement>) {
    event.preventDefault();

    if (trimDragCleanupRef.current) {
      trimDragCleanupRef.current();
      trimDragCleanupRef.current = null;
    }

    focusTrimTarget(target);
    updateTrimTargetFromClientX(target, event.clientX);

    const handlePointerMove = (moveEvent: PointerEvent) => {
      updateTrimTargetFromClientX(target, moveEvent.clientX);
    };

    const stopDragging = () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", stopDragging);
      window.removeEventListener("pointercancel", stopDragging);
      if (trimDragCleanupRef.current === stopDragging) {
        trimDragCleanupRef.current = null;
      }
    };

    trimDragCleanupRef.current = stopDragging;
    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", stopDragging);
    window.addEventListener("pointercancel", stopDragging);
  }

  function stepPreview(deltaS: number) {
    syncPreviewToTime(clampedPreviewTimeS + deltaS, "preview");
  }

  function nudgePreview(deltaS: number, opts?: { pause?: boolean }) {
    syncPreviewToTime(previewTimeRef.current + deltaS, "preview", opts);
  }

  function setTrimStartValue(nextTimeS: number) {
    const timeline = trimTimelineRef.current;
    if (!probe || !timeline) return;
    const maxStart = Math.max(0, timeline.end - timeline.minGap);
    const next = clamp(nextTimeS, 0, maxStart);
    setTrimStart(formatNumberInput(next));
    return next;
  }

  function setTrimEndValue(nextTimeS: number, opts?: { preferEmptyAtEnd?: boolean }) {
    const timeline = trimTimelineRef.current;
    if (!probe || !timeline) return;
    const minEnd = Math.min(probe.durationS, timeline.start + timeline.minGap);
    const next = clamp(nextTimeS, minEnd, probe.durationS);

    if (opts?.preferEmptyAtEnd && next >= probe.durationS - 0.001) {
      setTrimEnd("");
      return probe.durationS;
    }

    setTrimEnd(formatNumberInput(next));
    return next;
  }

  function getTrimTargetTime(target: Exclude<TrimFocusTarget, "preview">) {
    const timeline = trimTimelineRef.current;
    if (!timeline) return null;
    return target === "start" ? timeline.start : timeline.end;
  }

  function nudgeTrimTarget(target: Exclude<TrimFocusTarget, "preview">, deltaS: number) {
    const currentTimeS = getTrimTargetTime(target);
    if (currentTimeS === null) return false;
    updateTrimTarget(target, currentTimeS + deltaS, { pause: true });
    return true;
  }

  function applyTrimStartFromCurrent() {
    updateTrimTarget("start", previewSelectionTimeRef.current, { pause: true });
  }

  function applyTrimEndFromCurrent() {
    updateTrimTarget("end", previewSelectionTimeRef.current, { pause: true });
  }

  function focusTrimTarget(target: Exclude<TrimFocusTarget, "preview">) {
    const timeline = trimTimelineRef.current;
    if (!timeline) return;
    const nextTimeS = target === "start" ? timeline.start : timeline.end;
    syncPreviewToTime(nextTimeS, target, { pause: true });
  }

  function runComposeShortcutAction(action: ComposeShortcutAction) {
    if (!previewReady || !probe || jobId !== null || pendingEncodeRef.current !== null) return false;

    switch (action.kind) {
      case "toggle-playback":
        void togglePreviewPlayback();
        return true;
      case "apply-trim-start":
        applyTrimStartFromCurrent();
        return true;
      case "apply-trim-end":
        applyTrimEndFromCurrent();
        return true;
      case "nudge-timeline": {
        const activeTarget = activeTrimTargetRef.current;
        if (activeTarget === "preview") {
          nudgePreview(action.deltaS, { pause: true });
          return true;
        }
        return nudgeTrimTarget(activeTarget, action.deltaS);
      }
    }
  }

  const handleComposeShortcutKeydown = useEffectEvent((event: KeyboardEvent) => {
    if (!inputPath || modalOpen) return;
    if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
    if (blocksComposeShortcutTarget(event.target)) return;

    let action: ComposeShortcutAction | null = null;

    switch (event.code) {
      case "Space":
        if (event.repeat) return;
        action = { kind: "toggle-playback" };
        break;
      case "BracketLeft":
        action = { kind: "apply-trim-start" };
        break;
      case "BracketRight":
        action = { kind: "apply-trim-end" };
        break;
      case "ArrowLeft":
        action = { kind: "nudge-timeline", deltaS: event.shiftKey ? -TRIM_COARSE_NUDGE_S : -TRIM_FINE_NUDGE_S };
        break;
      case "ArrowRight":
        action = { kind: "nudge-timeline", deltaS: event.shiftKey ? TRIM_COARSE_NUDGE_S : TRIM_FINE_NUDGE_S };
        break;
      default:
        return;
    }

    if (!runComposeShortcutAction(action)) return;
    event.preventDefault();
  });

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      handleComposeShortcutKeydown(event);
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [handleComposeShortcutKeydown]);

  async function runSmokeWorkflowChecks(): Promise<SmokeWorkflowResult> {
    if (!smokeConfig || !probe || inputPath !== smokeConfig.inputPath) {
      return { ok: false, message: "Workflow smoke ran before its source was ready." };
    }
    if (exportQueueStateRef.current.items.length !== 0 || exportQueueStateRef.current.active !== null) {
      return { ok: false, message: "Workflow smoke expected a fresh empty queue." };
    }

    let continuation: {
      recipeId: string;
      recipeName: string;
      originalRecipeRaw: string | null;
    } | null = null;
    try {
      const raw = sessionStorage.getItem(SMOKE_WORKFLOW_SESSION_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Record<string, unknown>;
        if (
          typeof parsed.recipeId !== "string" ||
          typeof parsed.recipeName !== "string" ||
          (parsed.originalRecipeRaw !== null && typeof parsed.originalRecipeRaw !== "string")
        ) {
          throw new Error("Workflow smoke continuation data was invalid.");
        }
        continuation = {
          recipeId: parsed.recipeId,
          recipeName: parsed.recipeName,
          originalRecipeRaw: parsed.originalRecipeRaw,
        };
      }
    } catch (error) {
      return { ok: false, message: coerceErrorMessage(error, "Workflow smoke continuation data could not be read.") };
    }

    const originalRecipeRaw = continuation
      ? continuation.originalRecipeRaw
      : localStorage.getItem(USER_RECIPE_STORAGE_KEY);
    const originalRecipeStore = parseUserRecipeStore(originalRecipeRaw);
    const originalRecipeStatus = recipeStatus;
    const smokeRecipeName = continuation?.recipeName ?? `Workflow smoke ${Date.now()}`;
    const renamedSmokeRecipeName = `${smokeRecipeName} renamed`;
    let smokeRecipeId = continuation?.recipeId ?? null;
    let workflowSucceeded = false;
    let workflowReloading = false;
    const expectQueueTargetMiss = smokeConfig.workflowQueueExport && smokeConfig.g5QueueTargetMiss === true;

    function setMountedInputValue(input: HTMLInputElement, value: string) {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      if (!setter) throw new Error("Workflow smoke could not access the mounted input value setter.");
      setter.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
      input.dispatchEvent(new Event("change", { bubbles: true }));
    }

    function isMountedEnabledButton(
      button: HTMLButtonElement | null | undefined,
    ): button is HTMLButtonElement {
      return Boolean(button?.isConnected && !button.disabled);
    }

    function queueRuntimeSummary(itemId: number) {
      const state = exportQueueStateRef.current;
      const item = state.items.find((candidate) => candidate.id === itemId);
      const pending = pendingEncodeRef.current;
      return [
        `item=${item?.status ?? "missing"}`,
        `outcome=${item?.lastOutcome?.kind ?? "none"}`,
        `diagnostics=${item?.lastOutcome?.diagnostics ? "present" : "missing"}`,
        `active=${state.active?.itemId === itemId ? "matching" : state.active ? "other" : "none"}`,
        `autoRun=${state.autoRun ? "on" : "off"}`,
        `jobRef=${jobIdRef.current ?? "none"}`,
        `pending=${pending ? (pending.jobId === null ? "starting" : "bound") : "none"}`,
        `preparing=${queuePreparationCountRef.current}`,
        `updateBusy=${updateBusyRef.current ? "on" : "off"}`,
        `listeners=${encodeEventsReadyRef.current ? "ready" : "not-ready"}`,
        `listenerError=${encodeEventsErrorRef.current ? "present" : "none"}`,
      ].join(", ");
    }

    function getRecipeNameDialogControls() {
      const dialog = document.querySelector<HTMLElement>('.vfl-recipe-modal[role="dialog"]');
      const input = document.getElementById("vfl-user-recipe-name");
      const description = document.getElementById("vfl-user-recipe-description");
      const confirm = document.querySelector<HTMLButtonElement>('[data-smoke-id="user-recipe-confirm"]');
      return {
        dialog: dialog?.isConnected ? dialog : null,
        input: input instanceof HTMLInputElement && input.isConnected ? input : null,
        description: description instanceof HTMLTextAreaElement && description.isConnected ? description : null,
        confirm: isMountedEnabledButton(confirm) ? confirm : null,
      };
    }

    try {
      setOpenCards((cards) => ({ ...cards, recipes: true, queue: true }));

      if (!continuation) {
        const getSaveRecipeButton = () => {
          const button = document.querySelector<HTMLButtonElement>('[data-smoke-id="save-current-recipe"]');
          return isMountedEnabledButton(button) ? button : null;
        };
        const saveRecipeMounted = await waitForSmokeCondition(() => getSaveRecipeButton() !== null);
        const saveRecipeButton = getSaveRecipeButton();
        if (!saveRecipeMounted || !saveRecipeButton) {
          return { ok: false, message: "Workflow smoke could not find the enabled create-recipe tile." };
        }
        if (
          smokeConfig.workflowQueueExport &&
          !smokeConfig.skipPreviewInteractions &&
          !(await focusSmokeWebviewTarget(saveRecipeButton))
        ) {
          return { ok: false, message: "Workflow smoke could not focus the WebView and create-recipe tile." };
        }
        if (!smokeConfig.workflowQueueExport || smokeConfig.skipPreviewInteractions) saveRecipeButton.focus();
        if (smokeConfig.workflowQueueExport) {
          await reportSmokeStatus("workflow-recipe-ready", {
            message: "Waiting for packaged accessible activation on the create-recipe tile.",
          });
        }
        if (!smokeConfig.workflowQueueExport || smokeConfig.skipPreviewInteractions) {
          saveRecipeButton.click();
        }

        const saveDialogMounted = await waitForSmokeCondition(() => {
          const { dialog, input, description, confirm } = getRecipeNameDialogControls();
          return Boolean(
            dialog &&
            input &&
            description &&
            confirm?.textContent?.trim() === "Create recipe" &&
            document.activeElement === input &&
            dialog.textContent?.includes("The new recipe will use your current settings."),
          );
        });
        const {
          dialog: saveDialog,
          input: saveNameInput,
          description: saveDescriptionInput,
        } = getRecipeNameDialogControls();
        if (!saveDialog) {
          return {
            ok: false,
            message: smokeConfig.workflowQueueExport && !smokeConfig.skipPreviewInteractions
              ? "Smoke accessible activation did not open the create-recipe dialog."
              : "Workflow smoke did not open the create-recipe dialog.",
          };
        }
        if (
          !saveDialogMounted ||
          !saveNameInput ||
          !saveDescriptionInput ||
          !saveDialog.textContent?.includes("The new recipe will use your current settings.")
        ) {
          return { ok: false, message: "Workflow smoke found incomplete create-recipe controls or dialog semantics." };
        }
        setMountedInputValue(saveNameInput, smokeRecipeName);
        const saveReady = await waitForSmokeCondition(() => {
          const { input, confirm } = getRecipeNameDialogControls();
          return Boolean(
            input?.value === smokeRecipeName &&
            document.activeElement === input &&
            confirm?.textContent?.trim() === "Create recipe",
          );
        });
        const saveConfirm = getRecipeNameDialogControls().confirm;
        if (!saveReady || !saveConfirm) {
          return { ok: false, message: "Workflow smoke could not find the committed Create recipe action." };
        }
        saveConfirm.click();
        const savePassed = await waitForSmokeCondition(
          () => loadUserRecipeStore(localStorage).recipes.some((recipe) => recipe.name === smokeRecipeName),
        );
        if (!savePassed) return { ok: false, message: "Workflow smoke did not persist through the mounted Create recipe action." };
        const savedRecipe = loadUserRecipeStore(localStorage).recipes.find((recipe) => recipe.name === smokeRecipeName);
        if (!savedRecipe) return { ok: false, message: "Workflow smoke could not recover the saved recipe identity." };
        smokeRecipeId = savedRecipe.id;

        if (smokeConfig.workflowQueueExport) {
          sessionStorage.setItem(SMOKE_WORKFLOW_SESSION_KEY, JSON.stringify({
            recipeId: savedRecipe.id,
            recipeName: savedRecipe.name,
            originalRecipeRaw,
          }));
          await reportSmokeStatus("workflow-recipe-saved", {
            message: "Mounted Create persisted the recipe; reloading the packaged WebView to verify restoration.",
          });
          workflowReloading = true;
          window.location.reload();
          await waitMs(60_000);
          return { ok: false, message: "Workflow smoke reload did not replace the current document." };
        }
      }

      if (!smokeRecipeId) return { ok: false, message: "Workflow smoke had no saved recipe identity." };
      const restoredRecipe = loadUserRecipeStore(localStorage).recipes.find(
        (recipe) => recipe.id === smokeRecipeId && recipe.name === smokeRecipeName,
      );
      const getRecipeActions = (recipeName: string) => {
        const row = document.querySelector<HTMLElement>(`[data-user-recipe-id="${smokeRecipeId}"]`);
        const buttons = row?.isConnected
          ? Array.from(row.querySelectorAll<HTMLButtonElement>("button"))
          : [];
        const action = (name: "Apply" | "Edit") => {
          const button = buttons.find(
            (candidate) => name === "Apply"
              ? candidate.dataset.recipeAction === "apply"
              : candidate.getAttribute("aria-label") === `Edit ${recipeName}`,
          );
          return isMountedEnabledButton(button) ? button : null;
        };
        return {
          row: row?.isConnected ? row : null,
          apply: action("Apply"),
          edit: action("Edit"),
        };
      };
      const restoredMounted = await waitForSmokeCondition(() => {
        const { row, apply, edit } = getRecipeActions(smokeRecipeName);
        return Boolean(row && apply && edit);
      });
      if (!restoredRecipe || !restoredMounted) {
        return { ok: false, message: "Workflow smoke did not restore the saved recipe after packaged startup." };
      }

      const persistedRaw = localStorage.getItem(USER_RECIPE_STORAGE_KEY) ?? "";
      const escapedInputPath = JSON.stringify(smokeConfig.inputPath).slice(1, -1);
      const escapedOutputPath = JSON.stringify(smokeConfig.outputPath).slice(1, -1);
      const subtitlePathToken = smokeConfig.subtitlePath ?? "";
      const escapedSubtitlePath = subtitlePathToken ? JSON.stringify(subtitlePathToken).slice(1, -1) : "";
      const subtitleBasename = subtitlePathToken ? basename(subtitlePathToken) : "";
      const forbiddenRecipeTokens = [
        smokeConfig.inputPath,
        smokeConfig.outputPath,
        escapedInputPath,
        escapedOutputPath,
        subtitlePathToken,
        escapedSubtitlePath,
        subtitleBasename,
        '"inputPath"',
        '"outputPath"',
        '"subtitlePath"',
        '"subtitleInspection"',
        '"subtitleCueCount"',
        '"cueCount"',
        '"title"',
        '"trim"',
        '"crop"',
        '"colorPolicy"',
        '"stripMetadata"',
        '"diagnostics"',
        '"jobId"',
      ];
      if (forbiddenRecipeTokens.some((token) => token && persistedRaw.includes(token))) {
        return { ok: false, message: "Workflow smoke found forbidden clip, path, metadata, diagnostic, or identity data in a saved recipe." };
      }

      const applyRecipeButton = getRecipeActions(smokeRecipeName).apply;
      if (!applyRecipeButton) {
        return { ok: false, message: "Workflow smoke could not find the mounted enabled Apply recipe action." };
      }
      applyRecipeButton.click();
      const applyCommitted = await waitForSmokeCondition(() => {
        const status = document.querySelector<HTMLElement>('[data-smoke-id="user-recipe-status"]');
        const edit = getRecipeActions(smokeRecipeName).edit;
        return Boolean(
          status?.isConnected &&
          status.textContent?.startsWith(`Applied ${smokeRecipeName}.`) &&
          edit,
        );
      });
      const editRecipeButton = getRecipeActions(smokeRecipeName).edit;
      if (!applyCommitted || !editRecipeButton) {
        return { ok: false, message: "Workflow smoke did not commit Apply before mounting a fresh Edit action." };
      }
      editRecipeButton.click();

      const editDialogMounted = await waitForSmokeCondition(() => {
        const { dialog, input, confirm } = getRecipeNameDialogControls();
        return Boolean(
          dialog &&
          input &&
          document.activeElement === input &&
          confirm?.textContent?.trim() === "Save changes",
        );
      });
      const editInput = getRecipeNameDialogControls().input;
      if (!editDialogMounted || !editInput) {
        return { ok: false, message: "Workflow smoke could not find the mounted edit field." };
      }
      setMountedInputValue(editInput, renamedSmokeRecipeName);
      const editReady = await waitForSmokeCondition(() => {
        const { input, confirm } = getRecipeNameDialogControls();
        return Boolean(
          input?.value === renamedSmokeRecipeName &&
          document.activeElement === input &&
          confirm?.textContent?.trim() === "Save changes",
        );
      });
      const editConfirm = getRecipeNameDialogControls().confirm;
      if (!editReady || !editConfirm) {
        return { ok: false, message: "Workflow smoke could not find the committed Save changes action." };
      }
      editConfirm.click();
      const renamePassed = await waitForSmokeCondition(
        () => loadUserRecipeStore(localStorage).recipes.some((recipe) => recipe.name === renamedSmokeRecipeName),
      );
      if (!renamePassed) return { ok: false, message: "Workflow smoke did not persist the renamed recipe." };

      const getEditRecipeButton = () => getRecipeActions(renamedSmokeRecipeName).edit;
      const editMounted = await waitForSmokeCondition(() => getEditRecipeButton() !== null);
      const renamedEditButton = getEditRecipeButton();
      if (!editMounted || !renamedEditButton) {
        return { ok: false, message: "Workflow smoke could not find the renamed recipe Edit action." };
      }
      renamedEditButton.click();
      const deleteActionMounted = await waitForSmokeCondition(() => {
        const dialog = document.querySelector<HTMLElement>('.vfl-recipe-modal[role="dialog"]');
        return Array.from(dialog?.querySelectorAll<HTMLButtonElement>("button") ?? [])
          .some((button) => button.textContent?.trim() === "Delete recipe" && isMountedEnabledButton(button));
      });
      const editDialog = document.querySelector<HTMLElement>('.vfl-recipe-modal[role="dialog"]');
      const deleteRecipeButton = Array.from(editDialog?.querySelectorAll<HTMLButtonElement>("button") ?? [])
        .find((button) => button.textContent?.trim() === "Delete recipe");
      if (!deleteActionMounted || !isMountedEnabledButton(deleteRecipeButton)) {
        return { ok: false, message: "Workflow smoke could not find Delete recipe inside the edit dialog." };
      }
      deleteRecipeButton.click();
      const getDeleteDialogControls = () => {
        const dialog = document.querySelector<HTMLElement>('.vfl-recipe-modal[role="alertdialog"]');
        const buttons = dialog?.isConnected
          ? Array.from(dialog.querySelectorAll<HTMLButtonElement>("button"))
          : [];
        const cancel = buttons.find((button) => button.textContent?.trim() === "Cancel");
        const confirm = buttons.find((button) => button.textContent?.trim() === "Delete recipe");
        return {
          dialog: dialog?.isConnected ? dialog : null,
          cancel: isMountedEnabledButton(cancel) ? cancel : null,
          confirm: isMountedEnabledButton(confirm) ? confirm : null,
        };
      };
      const deleteDialogMounted = await waitForSmokeCondition(() => {
        const { dialog, cancel, confirm } = getDeleteDialogControls();
        return Boolean(dialog && cancel && confirm && document.activeElement === cancel);
      });
      const {
        dialog: deleteDialog,
        cancel: deleteCancel,
        confirm: deleteConfirm,
      } = getDeleteDialogControls();
      if (
        !deleteDialogMounted ||
        !deleteDialog ||
        !deleteCancel ||
        !deleteConfirm ||
        document.activeElement !== deleteCancel
      ) {
        return { ok: false, message: "Workflow smoke found incomplete delete-recipe alert dialog focus semantics." };
      }
      deleteConfirm.click();
      const deletePassed = await waitForSmokeCondition(
        () =>
          !loadUserRecipeStore(localStorage).recipes.some((recipe) => recipe.id === smokeRecipeId) &&
          document.activeElement === recipeSaveButtonRef.current,
      );
      if (!deletePassed) return { ok: false, message: "Workflow smoke did not delete the recipe and restore focus to the create-recipe tile." };

      const sourceExtension = extname(smokeConfig.inputPath);
      const secondInputPath = `${dirname(smokeConfig.inputPath)}${stem(smokeConfig.inputPath)}-workflow-smoke.${sourceExtension}`;
      const dropAction = await handleDroppedPaths([smokeConfig.inputPath, secondInputPath]);
      if (dropAction.kind !== "queueInputs") {
        return { ok: false, message: "Workflow smoke did not route a multi-file drop into the queue." };
      }
      const multiDropPassed = await waitForSmokeCondition(() => exportQueueStateRef.current.items.length === 2);
      if (!multiDropPassed) return { ok: false, message: "Workflow smoke did not enqueue both dropped paths." };
      const multiDropItems = exportQueueStateRef.current.items;
      if (
        multiDropItems[0].inputPath !== smokeConfig.inputPath ||
        multiDropItems[1].inputPath !== secondInputPath ||
        multiDropItems[0].outputPath === multiDropItems[1].outputPath
      ) {
        return { ok: false, message: "Workflow smoke found unstable drop ordering or colliding output previews." };
      }

      const getDuplicateButton = () => {
        const firstRow = document.querySelector<HTMLElement>(`[data-queue-item-id="${multiDropItems[0].id}"]`);
        const button = firstRow?.isConnected
          ? firstRow.querySelector<HTMLButtonElement>('[data-queue-action="duplicate"]')
          : null;
        return isMountedEnabledButton(button) ? button : null;
      };
      const duplicateMounted = await waitForSmokeCondition(() => getDuplicateButton() !== null);
      const duplicateButton = getDuplicateButton();
      if (!duplicateMounted || !duplicateButton) {
        return { ok: false, message: "Workflow smoke could not find the mounted Duplicate action." };
      }
      duplicateButton.click();
      const duplicatePassed = await waitForSmokeCondition(() => exportQueueStateRef.current.items.length === 3);
      if (!duplicatePassed) return { ok: false, message: "Workflow smoke did not duplicate the queue snapshot." };
      const duplicatedItems = exportQueueStateRef.current.items;
      const duplicatedItem = duplicatedItems[duplicatedItems.length - 1];
      const getRemoveDuplicateButton = () => {
        const duplicatedRow = duplicatedItem
          ? document.querySelector<HTMLElement>(`[data-queue-item-id="${duplicatedItem.id}"]`)
          : null;
        const button = duplicatedRow?.isConnected
          ? duplicatedRow.querySelector<HTMLButtonElement>('[data-queue-action="remove"]')
          : null;
        return isMountedEnabledButton(button) ? button : null;
      };
      const removeDuplicateMounted = await waitForSmokeCondition(() => getRemoveDuplicateButton() !== null);
      const removeDuplicateButton = getRemoveDuplicateButton();
      if (!duplicatedItem || !removeDuplicateMounted || !removeDuplicateButton) {
        return { ok: false, message: "Workflow smoke could not find the duplicated item's Remove action." };
      }
      removeDuplicateButton.focus();
      removeDuplicateButton.click();
      const removeFocused = await waitForSmokeCondition(
        () =>
          !exportQueueStateRef.current.items.some((item) => item.id === duplicatedItem.id) &&
          document.activeElement === queueFallbackButtonRef.current,
      );
      if (!removeFocused) return { ok: false, message: "Workflow smoke did not restore focus after queue removal." };

      const currentRequest = buildRequest();
      const snapshotOutputPath = await suggestedOutputForInput(
        currentRequest.inputPath,
        currentRequest.format,
        claimedOutputPathsForPreparation(),
      );
      const beforeSnapshotCount = exportQueueStateRef.current.items.length;
      const snapshotState = dispatchExportQueue({
        type: "enqueue-prepared",
        items: [{
          request: { ...currentRequest, outputPath: snapshotOutputPath },
          durationS: plannedSummary?.durationS ?? null,
        }],
      });
      const snapshotItem = snapshotState.items[beforeSnapshotCount];
      const getApplySnapshotButton = () => {
        const snapshotRow = document.querySelector<HTMLElement>(`[data-queue-item-id="${snapshotItem.id}"]`);
        const button = snapshotRow?.isConnected
          ? snapshotRow.querySelector<HTMLButtonElement>('[data-queue-action="apply-snapshot"]')
          : null;
        return isMountedEnabledButton(button) ? button : null;
      };
      const snapshotMounted = await waitForSmokeCondition(() => getApplySnapshotButton() !== null);
      const applySnapshotButton = getApplySnapshotButton();
      if (!snapshotMounted || !applySnapshotButton) {
        return { ok: false, message: "Workflow smoke could not find the mounted Apply snapshot action." };
      }
      applySnapshotButton.click();
      const snapshotApplied = await waitForSmokeCondition(
        () =>
          document.querySelector(".vfl-footer-status")?.textContent?.includes("Applied the full queue snapshot") === true &&
          !queueSnapshotApplyingRef.current,
      );
      if (!snapshotApplied) return { ok: false, message: "Workflow smoke did not apply the full queue snapshot." };
      setOutputAuto(false);
      setOutputPath(smokeConfig.outputPath);

      const smokeDiagnostics: ExportDiagnostics = {
        mode: "workflow-smoke",
        videoAction: "encode",
        audioAction: "copy",
        videoCodec: "h264",
        audioCodec: "aac",
        passes: 1,
        attempts: 1,
        subtitleBurnedIn: false,
        subtitleCueCount: null,
        commandPreview: "ffmpeg workflow smoke",
      };
      dispatchExportQueue({ type: "reset" });
      const failureRequest = {
        ...buildRequest(),
        outputPath: smokeConfig.inputPath,
      };
      const failureQueue = dispatchExportQueue({
        type: "enqueue-prepared",
        items: [{ request: failureRequest, durationS: plannedSummary?.durationS ?? null }],
      });
      const failedItem = failureQueue.items[0];
      if (!failedItem) return { ok: false, message: "Workflow smoke could not enqueue the deterministic failure." };

      if (smokeConfig.workflowQueueExport) {
        runQueue();
        const realFailurePassed = await waitForSmokeCondition(() => {
          const item = exportQueueStateRef.current.items.find((candidate) => candidate.id === failedItem.id);
          return Boolean(item?.status === "failed" && item.lastOutcome?.diagnostics && !exportQueueStateRef.current.autoRun);
        }, 60_000);
        if (!realFailurePassed) {
          return {
            ok: false,
            message: `Workflow smoke did not retain diagnostics from the real queued backend failure (${queueRuntimeSummary(failedItem.id)}).`,
          };
        }
      } else {
        let failureState = dispatchExportQueue({ type: "start-auto-run" });
        failureState = dispatchExportQueue({ type: "claim-next" });
        const failedActive = failureState.active;
        if (!failedActive) return { ok: false, message: "Workflow smoke could not claim a queue item." };
        dispatchExportQueue({ type: "stop-auto-run" });
        dispatchExportQueue({
          type: "settled",
          itemId: failedActive.itemId,
          runId: failedActive.runId,
          outcome: {
            kind: "failed",
            message: "Workflow smoke retained failure.",
            outputPath: failedItem.outputPath,
            diagnostics: smokeDiagnostics,
            completedAtMs: Date.now(),
          },
        });
      }
      const getFailureControls = () => {
        const failedRow = document.querySelector<HTMLElement>(`[data-queue-item-id="${failedItem.id}"]`);
        const retryButton = failedRow?.isConnected
          ? failedRow.querySelector<HTMLButtonElement>('[data-queue-action="retry"]')
          : null;
        const diagnostics = failedRow?.isConnected
          ? failedRow.querySelector<HTMLElement>(".vfl-queue-diagnostics")
          : null;
        return {
          retryButton: isMountedEnabledButton(retryButton) ? retryButton : null,
          diagnostics: diagnostics?.isConnected ? diagnostics : null,
        };
      };
      const failureMounted = await waitForSmokeCondition(() => {
        const controls = getFailureControls();
        return controls.retryButton !== null && controls.diagnostics !== null;
      });
      const { retryButton, diagnostics } = getFailureControls();
      if (!failureMounted || !retryButton || !diagnostics) {
        return { ok: false, message: "Workflow smoke did not retain mounted failure diagnostics and Retry." };
      }
      const failedOutputPath = failedItem.outputPath;
      retryButton.focus();
      retryButton.click();
      const retryPassed = await waitForSmokeCondition(() => {
        const retried = exportQueueStateRef.current.items.find((item) => item.id === failedItem.id);
        const runQueueButton = queueRunButtonRef.current;
        return Boolean(
          retried &&
          retried.status === "queued" &&
          retried.outputPath !== failedOutputPath &&
          retried.lastOutcome?.diagnostics !== null &&
          runQueueButton?.isConnected &&
          !runQueueButton.disabled &&
          document.activeElement === runQueueButton,
        );
      });
      if (!retryPassed) return { ok: false, message: "Workflow smoke did not retry with a fresh path and retained diagnostics." };

      if (smokeConfig.workflowQueueExport) {
        const runQueueButton = queueRunButtonRef.current;
        if (!isMountedEnabledButton(runQueueButton)) {
          return { ok: false, message: "Workflow smoke could not find enabled Run queue after retry." };
        }
        if (!smokeConfig.skipPreviewInteractions && !(await focusSmokeWebviewTarget(runQueueButton))) {
          return { ok: false, message: "Workflow smoke could not focus the WebView and Run queue control." };
        }
        if (smokeConfig.skipPreviewInteractions) runQueueButton.focus();
        await reportSmokeStatus("workflow-queue-ready", {
          message: "Waiting for packaged accessible activation on Run queue after a real failure and retry.",
        });
        if (smokeConfig.skipPreviewInteractions) runQueueButton.click();

        const realRetryPassed = await waitForSmokeCondition(() => {
          const item = exportQueueStateRef.current.items.find((candidate) => candidate.id === failedItem.id);
          if (!item || exportQueueStateRef.current.autoRun) return false;
          const retainedFailure = item.history.some((attempt) => attempt.kind === "failed" && attempt.diagnostics);
          if (expectQueueTargetMiss) {
            return Boolean(
              item.status === "target-missed" &&
              item.lastOutcome?.kind === "target-missed" &&
              item.lastOutcome.targetResult?.status === "missed" &&
              item.lastOutcome.diagnostics &&
              item.history.length >= 2 &&
              retainedFailure &&
              item.history.some(
                (attempt) => attempt.kind === "target-missed" && attempt.targetResult?.status === "missed",
              )
            );
          }
          return Boolean(
            item?.status === "done" &&
            item.lastOutcome?.kind === "done" &&
            item.lastOutcome.diagnostics &&
            item.history.length >= 2 &&
            retainedFailure
          );
        }, 60_000);
        if (!realRetryPassed) {
          return {
            ok: false,
            message: expectQueueTargetMiss
              ? `Workflow smoke did not retain the real queued target miss and its prior failure history (${queueRuntimeSummary(failedItem.id)}).`
              : `Workflow smoke did not complete the real queued retry with retained failure history (${queueRuntimeSummary(failedItem.id)}).`,
          };
        }
        const recoveryEvidenceMounted = await waitForSmokeCondition(() => {
          const row = document.querySelector<HTMLElement>(`[data-queue-item-id="${failedItem.id}"]`);
          const historyText = row?.querySelector(".vfl-queue-history")?.textContent ?? "";
          if (!historyText.includes("Attempted output")) return false;
          if (!expectQueueTargetMiss) return true;
          const retryControl = row?.querySelector<HTMLButtonElement>('[data-queue-action="retry"]') ?? null;
          const duplicateControl = row?.querySelector<HTMLButtonElement>('[data-queue-action="duplicate"]') ?? null;
          return Boolean(
            historyText.includes("Target missed") &&
            historyText.includes("Result") &&
            retryControl &&
            !retryControl.disabled &&
            duplicateControl &&
            !duplicateControl.disabled
          );
        });
        if (!recoveryEvidenceMounted) {
          return {
            ok: false,
            message: expectQueueTargetMiss
              ? "Workflow smoke could not inspect retained failure and target-miss history with mounted Retry and Duplicate actions."
              : "Workflow smoke could not inspect retained prior-attempt history after success.",
          };
        }
        await reportSmokeStatus("workflow-queue-complete", {
          message: expectQueueTargetMiss
            ? "Real queued failure, retry, keyboard run, target miss, diagnostics, history, Retry, and Duplicate checks passed."
            : "Real queued failure, retry, keyboard run, backend success, diagnostics, and history checks passed.",
        });
      }

      const reusable = reusableSettingsFromEncodeRequest(failedItem.request);
      const reusableRaw = JSON.stringify(reusable);
      if (!reusable || reusableRaw.includes(smokeConfig.inputPath) || reusableRaw.includes("inputPath")) {
        return { ok: false, message: "Workflow smoke found a media path in the reusable queue settings boundary." };
      }

      dispatchExportQueue({ type: "reset" });
      if (exportQueueStateRef.current.items.length !== 0) {
        return { ok: false, message: "Workflow smoke could not restore an empty queue." };
      }
      workflowSucceeded = true;
      return {
        ok: true,
        message: "Packaged recipe restoration, queue recovery, multi-file routing, snapshot, diagnostics, focus, and retry checks passed.",
      };
    } catch (error) {
      return { ok: false, message: coerceErrorMessage(error, "Packaged workflow checks failed.") };
    } finally {
      const shouldRestore = workflowSucceeded || (!smokeConfig.workflowQueueExport && !workflowReloading);
      if (shouldRestore) {
        closeRecipeDialog();
        dispatchExportQueue({ type: "reset" });
        try {
          if (originalRecipeRaw === null) localStorage.removeItem(USER_RECIPE_STORAGE_KEY);
          else localStorage.setItem(USER_RECIPE_STORAGE_KEY, originalRecipeRaw);
          sessionStorage.removeItem(SMOKE_WORKFLOW_SESSION_KEY);
        } catch {
          // The returned failure will identify persistence problems in the main path.
        }
        setUserRecipeStore(originalRecipeStore);
        showPersistentRecipeStatus(originalRecipeStatus);
      }
    }
  }

  async function runSmokeG7UiChecks(): Promise<SmokeWorkflowResult> {
    const config = smokeConfigRef.current;
    const operation = config?.g7Operation;
    if (!config || !operation || !probe) {
      return { ok: false, message: "Packaged G7 UI checks ran without their source probe and operation." };
    }

    setOpenCards((cards) => ({ ...cards, transform: true, advanced: true }));
    setSpeed("1.25");
    setRotateDeg(180);
    setAdvancedFrameRateCapFps("24");

    const seededSettingsMounted = await waitForSmokeCondition(() => {
      const speedInput = document.getElementById("vfl-speed") as HTMLInputElement | null;
      const rotateInput = document.getElementById("vfl-rotate") as HTMLSelectElement | null;
      const capInput = document.getElementById("vfl-frame-rate-cap") as HTMLSelectElement | null;
      return speedInput?.value === "1.25" && rotateInput?.value === "180" && capInput?.value === "24";
    });
    if (!seededSettingsMounted) {
      return { ok: false, message: "Packaged G7 reset check could not mount its non-default settings." };
    }

    const resetButton = document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-settings"]');
    if (!resetButton || resetButton.disabled) {
      return { ok: false, message: "Packaged G7 reset check could not find the enabled mounted Reset all settings action." };
    }

    resetButton.focus();
    resetButton.click();
    const cancelFocused = await waitForSmokeCondition(() => {
      const dialog = document.querySelector<HTMLElement>('[role="alertdialog"]');
      const cancel = document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-cancel"]');
      return Boolean(dialog && cancel && document.activeElement === cancel);
    });
    const firstDialog = document.querySelector<HTMLElement>('[role="alertdialog"]');
    if (!cancelFocused || !firstDialog) {
      return { ok: false, message: "Packaged G7 reset confirmation did not open as an alert dialog with safe Cancel focus." };
    }
    updateSmokeG7Evidence({
      resetDialogRole: firstDialog.getAttribute("role"),
      resetCancelFocused: true,
    });

    const cancelReset = document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-cancel"]');
    cancelReset?.click();
    const cancelPreserved = await waitForSmokeCondition(() => {
      const speedInput = document.getElementById("vfl-speed") as HTMLInputElement | null;
      const rotateInput = document.getElementById("vfl-rotate") as HTMLSelectElement | null;
      const capInput = document.getElementById("vfl-frame-rate-cap") as HTMLSelectElement | null;
      return (
        !document.querySelector('[role="alertdialog"]') &&
        document.activeElement === resetButton &&
        speedInput?.value === "1.25" &&
        rotateInput?.value === "180" &&
        capInput?.value === "24"
      );
    });
    if (!cancelPreserved) {
      return { ok: false, message: "Packaged G7 reset Cancel did not preserve settings and restore trigger focus." };
    }
    updateSmokeG7Evidence({
      resetCancelPreservedSettings: true,
      resetCancelRestoredFocus: true,
    });

    resetButton.click();
    const secondDialogReady = await waitForSmokeCondition(() => {
      const cancel = document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-cancel"]');
      const confirm = document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-confirm"]');
      return Boolean(cancel && confirm && document.activeElement === cancel && !confirm.disabled);
    });
    if (!secondDialogReady) {
      return { ok: false, message: "Packaged G7 reset confirmation could not be reopened for the destructive action." };
    }
    document.querySelector<HTMLButtonElement>('[data-smoke-id="reset-all-confirm"]')?.click();
    const resetConfirmed = await waitForSmokeCondition(() => {
      const speedInput = document.getElementById("vfl-speed") as HTMLInputElement | null;
      const rotateInput = document.getElementById("vfl-rotate") as HTMLSelectElement | null;
      const capInput = document.getElementById("vfl-frame-rate-cap") as HTMLSelectElement | null;
      return (
        !document.querySelector('[role="alertdialog"]') &&
        document.activeElement === resetButton &&
        speedInput?.value === "1.0" &&
        rotateInput?.value === "0" &&
        capInput?.value === "auto"
      );
    });
    if (!resetConfirmed) {
      return { ok: false, message: "Packaged G7 confirmed reset did not restore defaults and trigger focus." };
    }
    updateSmokeG7Evidence({
      resetConfirmed: true,
      resetConfirmRestoredFocus: true,
    });

    const speedValue = operation === "rotate-speed-cap" ? "2" : operation === "cancel-drop" ? "0.5" : "1.0";
    const rotationValue = operation === "copy-progress" ? 0 : 90;
    const capValue = operation === "copy-progress" ? "auto" : "24";
    setSpeed(speedValue);
    setRotateDeg(rotationValue);
    setAdvancedFrameRateCapFps(capValue);
    setAdvancedEncodeSpeed(operation === "cancel-drop" ? "smaller" : "auto");
    setCropEnabled(false);
    setCropRect({ x: 0, y: 0, w: 1, h: 1 });

    const desiredSettingsMounted = await waitForSmokeCondition(() => {
      const speedInput = document.getElementById("vfl-speed") as HTMLInputElement | null;
      const rotateInput = document.getElementById("vfl-rotate") as HTMLSelectElement | null;
      const capInput = document.getElementById("vfl-frame-rate-cap") as HTMLSelectElement | null;
      return speedInput?.value === speedValue && rotateInput?.value === String(rotationValue) && capInput?.value === capValue;
    });
    if (!desiredSettingsMounted) {
      return { ok: false, message: "Packaged G7 operation settings did not reach their mounted controls." };
    }

    let previewTransform: string | null = null;
    if (rotationValue !== 0) {
      const rotatedPreviewReady = await waitForSmokeCondition(() => {
        const surface = document.querySelector<HTMLElement>(".vfl-video-surface");
        previewTransform = surface?.style.transform ?? null;
        return Boolean(previewTransform?.includes(`rotate(${rotationValue}deg)`));
      });
      if (!rotatedPreviewReady) {
        return { ok: false, message: "Packaged G7 preview did not render the selected manual quarter-turn." };
      }
    }

    const speedNumber = Number(speedValue);
    const postSpeedFrameRateFps = probe.frameRate == null ? null : probe.frameRate * speedNumber;
    const frameRateCapFps = capValue === "auto" ? null : Number(capValue);
    const frameRateCapApplies = postSpeedFrameRateFps === null || frameRateCapFps === null
      ? null
      : postSpeedFrameRateFps > frameRateCapFps + 0.01;
    if (frameRateCapFps !== null && frameRateCapApplies !== true) {
      return { ok: false, message: "Packaged G7 speed-aware frame cap did not apply to the post-speed source rate." };
    }
    let frameRateMountedCopyVerified = frameRateCapFps === null;
    if (frameRateCapFps !== null && postSpeedFrameRateFps !== null) {
      frameRateMountedCopyVerified = await waitForSmokeCondition(() => {
        const capInput = document.getElementById("vfl-frame-rate-cap");
        const hint = capInput?.closest(".vfl-field")?.querySelector<HTMLElement>(".vfl-inline-hint");
        const copy = hint?.textContent ?? "";
        return (
          copy.includes(`${postSpeedFrameRateFps.toFixed(1)} fps`) &&
          copy.includes(`${frameRateCapFps} fps`)
        );
      });
      if (!frameRateMountedCopyVerified) {
        return { ok: false, message: "Packaged G7 mounted frame-rate guidance did not name the post-speed rate and selected cap." };
      }
    }

    const exportButton = document.querySelector<HTMLButtonElement>('[data-smoke-id="export"]');
    if (!exportButton || exportButton.disabled || exportButton.textContent?.trim() !== "Export") {
      return { ok: false, message: "Packaged G7 checks could not find the ready mounted Export control." };
    }
    exportButton.focus();
    if (document.activeElement !== exportButton) {
      return { ok: false, message: "Packaged G7 ready Export control did not take focus before activation." };
    }
    smokeG7ExportButtonRef.current = exportButton;
    const exportRect = exportButton.getBoundingClientRect();
    smokeG7ExportRectRef.current = {
      x: exportRect.x,
      y: exportRect.y,
      width: exportRect.width,
      height: exportRect.height,
    };
    updateSmokeG7Evidence({
      previewRotationDeg: rotationValue,
      previewTransform,
      postSpeedFrameRateFps,
      frameRateCapFps,
      frameRateCapApplies,
      frameRateMountedCopyVerified,
      exportControlStable: true,
      exportControlInitiallyFocused: true,
    });
    await reportSmokeStatus("g7-ui-ready", {
      message: "Mounted G7 reset, preview rotation, speed-aware frame cap, and stable Export preflight checks passed.",
      g7Evidence: smokeG7EvidenceRef.current,
    });
    return { ok: true, message: "Mounted G7 operational UI checks passed." };
  }

  async function runSmokeG7ActiveEncodeChecks(): Promise<SmokeWorkflowResult> {
    const config = smokeConfigRef.current;
    const evidence = smokeG7EvidenceRef.current;
    if (!config?.g7Operation || !evidence || jobIdRef.current === null) {
      return { ok: false, message: "Packaged G7 active-export checks ran without an active operation." };
    }

    const activeControlsMounted = await waitForSmokeCondition(() => {
      const exportButton = document.querySelector<HTMLButtonElement>('[data-smoke-id="export"]');
      const cancelButton = document.querySelector<HTMLButtonElement>('[data-smoke-id="cancel-export"]');
      return Boolean(
        exportButton &&
        exportButton === smokeG7ExportButtonRef.current &&
        exportButton.disabled &&
        exportButton.textContent?.trim() === "Exporting…" &&
        cancelButton &&
        cancelButton !== exportButton &&
        !cancelButton.disabled
      );
    });
    if (!activeControlsMounted) {
      return { ok: false, message: "Packaged G7 Export did not retain identity beside a separate active Cancel control." };
    }
    const before = smokeG7ExportRectRef.current;
    const activeExport = document.querySelector<HTMLButtonElement>('[data-smoke-id="export"]');
    const after = activeExport?.getBoundingClientRect();
    const geometryStable = Boolean(
      before &&
      after &&
      Math.abs(before.x - after.x) <= 1 &&
      Math.abs(before.y - after.y) <= 1 &&
      Math.abs(before.width - after.width) <= 1 &&
      Math.abs(before.height - after.height) <= 1
    );
    if (!geometryStable && before && after) {
      return {
        ok: false,
        message: `Packaged G7 Export moved from ${before.x.toFixed(1)},${before.y.toFixed(1)} ${before.width.toFixed(1)}x${before.height.toFixed(1)} to ${after.x.toFixed(1)},${after.y.toFixed(1)} ${after.width.toFixed(1)}x${after.height.toFixed(1)} CSS pixels.`,
      };
    }
    if (!geometryStable) {
      return { ok: false, message: "Packaged G7 Export geometry was unavailable across the active-state transition." };
    }
    updateSmokeG7Evidence({
      exportControlPreservedIdentity: true,
      exportControlPreservedGeometry: true,
      cancelControlSeparate: true,
    });
    await reportSmokeStatus("g7-controls-ready", {
      message: "The mounted Export control retained identity and became disabled beside a separate enabled Cancel action.",
      g7Evidence: smokeG7EvidenceRef.current,
    });

    if (config.g7Operation !== "cancel-drop") {
      smokeG7ActiveChecksDoneRef.current = true;
      return { ok: true, message: "Mounted active-export controls passed." };
    }
    if (!config.g7DropPath) {
      return { ok: false, message: "Packaged G7 cancel-drop operation has no configured drop input." };
    }

    const sourceBeforeDrop = inputPathRef.current;
    const queueCountBeforeDrop = exportQueueStateRef.current.items.length;
    const action = await handleDroppedPaths([config.g7DropPath]);
    const droppedInputQueued = await waitForSmokeCondition(() =>
      exportQueueStateRef.current.items.length === queueCountBeforeDrop + 1 &&
      exportQueueStateRef.current.items.some(
        (item) => queuePathIdentity(item.inputPath) === queuePathIdentity(config.g7DropPath ?? ""),
      ),
    );
    const dropPreservedInput = queuePathIdentity(inputPathRef.current) === queuePathIdentity(sourceBeforeDrop);
    if (action.kind !== "queueInputs" || !droppedInputQueued || !dropPreservedInput) {
      return { ok: false, message: "Packaged G7 active-export drop did not queue the file while preserving the current source." };
    }
    updateSmokeG7Evidence({
      dropActionKind: action.kind,
      dropPreservedInput: true,
      queuedDropCount: exportQueueStateRef.current.items.length - queueCountBeforeDrop,
    });
    await reportSmokeStatus("g7-drop-queued", {
      message: "A drop during export was queued with a clear status and did not replace the active source.",
      g7Evidence: smokeG7EvidenceRef.current,
    });

    const backendEncodeStarted = await waitForSmokeCondition(
      () => smokeG7EvidenceRef.current?.progressHistory.some((sample) => sample.phase === "encoding") === true,
      15_000,
    );
    if (!backendEncodeStarted) {
      return { ok: false, message: "Packaged G7 cancellation never observed a real backend encode progress event." };
    }

    const cancelButton = document.querySelector<HTMLButtonElement>('[data-smoke-id="cancel-export"]');
    if (!cancelButton || cancelButton.disabled) {
      return { ok: false, message: "Packaged G7 cancel-drop operation lost its mounted Cancel action." };
    }
    smokeG7BeforeCancelInvokeRef.current = () => {
      const stagePromise = reportSmokeStatus("g7-cancel-requested", {
        message: "Mounted cancellation was requested once; a repeated handler call was ignored.",
        g7Evidence: smokeG7EvidenceRef.current,
      });
      smokeG7CancelStagePromiseRef.current = stagePromise;
      return stagePromise;
    };
    cancelButton.click();
    const repeatedCancellation = cancelEncode();
    const cancelStagePromise = smokeG7CancelStagePromiseRef.current;
    if (
      latestAttemptRef.current.kind !== "cancelling" ||
      smokeG7EvidenceRef.current?.cancelInvokeCount !== 1 ||
      !cancelStagePromise
    ) {
      return { ok: false, message: "Packaged G7 Cancel did not synchronously latch one idempotent backend request." };
    }
    await repeatedCancellation;
    await cancelStagePromise;
    const cancellationRendered = await waitForSmokeCondition(() =>
      document.querySelector<HTMLButtonElement>('[data-smoke-id="cancel-export"]')?.disabled === true,
    );
    if (!cancellationRendered || smokeG7EvidenceRef.current?.cancelInvokeCount !== 1) {
      return { ok: false, message: "Packaged G7 repeated cancellation reached the backend more than once." };
    }
    smokeG7ActiveChecksDoneRef.current = true;
    return { ok: true, message: "Mounted active-export drop and idempotent cancellation checks passed." };
  }

  async function runSmokeAccessibilityChecks(): Promise<SmokeAccessibilityResult> {
    if (!probe || !previewReady || !previewMediaReady) {
      return { ok: false, message: "Accessibility smoke ran before the preview was ready." };
    }

    setOpenCards((cards) => ({ ...cards, crop: true }));
    await waitMs(220);

    const startSlider = document.getElementById("vfl-trim-start-slider");
    const endSlider = document.getElementById("vfl-trim-end-slider");
    if (!(startSlider instanceof HTMLButtonElement) || !(endSlider instanceof HTMLButtonElement)) {
      return { ok: false, message: "Accessibility smoke could not find both mounted trim sliders." };
    }

    for (const [slider, label] of [[startSlider, "Trim start"], [endSlider, "Trim end"]] as const) {
      if (slider.getAttribute("role") !== "slider" || slider.getAttribute("aria-label") !== label) {
        return { ok: false, message: `Accessibility smoke found incomplete role or name semantics for ${label}.` };
      }
      for (const attribute of ["aria-valuemin", "aria-valuemax", "aria-valuenow", "aria-valuetext"]) {
        if (!slider.hasAttribute(attribute)) {
          return { ok: false, message: `Accessibility smoke found ${label} without ${attribute}.` };
        }
      }
      const bounds = slider.getBoundingClientRect();
      if (bounds.width < 24 || bounds.height < 24) {
        return {
          ok: false,
          message: `Accessibility smoke found ${label} target ${bounds.width.toFixed(1)}x${bounds.height.toFixed(1)} instead of at least 24x24 CSS pixels.`,
        };
      }
    }

    function dispatchKey(target: HTMLElement | Document | Window, key: string, options: { shiftKey?: boolean; code?: string } = {}) {
      const event = new KeyboardEvent("keydown", {
        key,
        code: options.code ?? key,
        shiftKey: options.shiftKey ?? false,
        bubbles: true,
        cancelable: true,
      });
      target.dispatchEvent(event);
      return event;
    }

    function captureTrustedSmokeKey(target: HTMLElement, expectedKey: string) {
      let received = false;
      const handler = (event: KeyboardEvent) => {
        if (
          event.isTrusted &&
          event.key === expectedKey &&
          document.hasFocus() &&
          document.activeElement === target
        ) {
          received = true;
        }
      };
      target.addEventListener("keydown", handler);
      return {
        received: () => received,
        stop: () => target.removeEventListener("keydown", handler),
      };
    }

    if (!(await focusSmokeWebviewTarget(startSlider))) {
      return { ok: false, message: "Accessibility smoke could not focus the WebView and Trim start slider." };
    }
    const beforeStart = Number(startSlider.getAttribute("aria-valuenow"));
    const realTrimRight = captureTrustedSmokeKey(startSlider, "ArrowRight");
    await reportSmokeStatus("keyboard-trim-ready", {
      message: "Waiting for real packaged keyboard input: Right on Trim start.",
    });
    const realTrimIncrementPassed = await waitForSmokeCondition(() => {
      const currentStart = Number(document.getElementById("vfl-trim-start-slider")?.getAttribute("aria-valuenow"));
      return realTrimRight.received() && Math.abs(currentStart - (beforeStart + TRIM_FINE_NUDGE_S)) <= 0.001;
    });
    realTrimRight.stop();
    if (!realTrimIncrementPassed) {
      return { ok: false, message: "Accessibility smoke did not receive one trusted real Right step on Trim start." };
    }
    const realTrimLeft = captureTrustedSmokeKey(startSlider, "ArrowLeft");
    const realTrimTab = captureTrustedSmokeKey(startSlider, "Tab");
    await reportSmokeStatus("keyboard-trim-incremented", {
      message: "Real packaged Right moved Trim start by exactly 0.1s; waiting for trusted real Left then Tab.",
    });
    const realTrimKeysPassed = await waitForSmokeCondition(() => {
      const currentStart = Number(document.getElementById("vfl-trim-start-slider")?.getAttribute("aria-valuenow"));
      return realTrimLeft.received() &&
        realTrimTab.received() &&
        Math.abs(currentStart - beforeStart) <= 0.001 &&
        document.activeElement?.id === "vfl-trim-end-slider";
    });
    realTrimLeft.stop();
    realTrimTab.stop();
    if (!realTrimKeysPassed) {
      return { ok: false, message: "Accessibility smoke did not receive the trusted real Right, Left, and Tab trim sequence." };
    }
    await reportSmokeStatus("keyboard-trim-complete", {
      message: "Real packaged trim keyboard sequence completed with the original value restored and focus on Trim end.",
    });

    startSlider.focus();
    const startMaximum = Number(startSlider.getAttribute("aria-valuemax"));
    const arrowEvent = dispatchKey(startSlider, "ArrowRight");
    await waitMs(160);
    const afterArrow = Number(document.getElementById("vfl-trim-start-slider")?.getAttribute("aria-valuenow"));
    const expectedAfterArrow = Math.min(startMaximum, beforeStart + TRIM_FINE_NUDGE_S);
    if (!arrowEvent.defaultPrevented || Math.abs(afterArrow - expectedAfterArrow) > 0.001) {
      return {
        ok: false,
        message: `Accessibility smoke expected one Trim start ArrowRight step (${beforeStart} -> ${expectedAfterArrow}) but saw ${afterArrow}.`,
      };
    }

    const arrowLeftEvent = dispatchKey(startSlider, "ArrowLeft");
    await waitMs(160);
    const afterArrowLeft = Number(document.getElementById("vfl-trim-start-slider")?.getAttribute("aria-valuenow"));
    if (!arrowLeftEvent.defaultPrevented || Math.abs(afterArrowLeft - beforeStart) > 0.001) {
      return {
        ok: false,
        message: `Accessibility smoke expected one Trim start ArrowLeft step back to ${beforeStart} but saw ${afterArrowLeft}.`,
      };
    }

    for (const key of ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "End", "Home"]) {
      const event = dispatchKey(document.getElementById("vfl-trim-start-slider") as HTMLElement, key);
      if (!event.defaultPrevented) {
        return { ok: false, message: `Accessibility smoke found unhandled Trim start key ${key}.` };
      }
      await waitMs(80);
    }

    const homeValue = Number(document.getElementById("vfl-trim-start-slider")?.getAttribute("aria-valuenow"));
    if (Math.abs(homeValue) > 0.001) {
      return { ok: false, message: `Accessibility smoke expected Home to move Trim start to 0s, saw ${homeValue}.` };
    }

    setCropEnabled(true);
    setCropRect({ x: 0.08, y: 0.08, w: 0.84, h: 0.84 });
    await waitMs(220);
    const cropInputs = Array.from(document.querySelectorAll<HTMLInputElement>("[data-crop-field]"));
    if (cropInputs.length !== 4 || cropInputs.some((input) => !input.labels?.length || input.type !== "number")) {
      return { ok: false, message: "Accessibility smoke did not find four labelled numeric crop fields." };
    }
    const cropX = document.getElementById("vfl-crop-x");
    if (!(cropX instanceof HTMLInputElement)) {
      return { ok: false, message: "Accessibility smoke could not find the Crop X input." };
    }
    if (!(await focusSmokeWebviewTarget(cropX))) {
      return { ok: false, message: "Accessibility smoke could not focus the WebView and Crop X input." };
    }
    const cropXBefore = Number(cropX.value);
    const realCropUp = captureTrustedSmokeKey(cropX, "ArrowUp");
    await reportSmokeStatus("keyboard-crop-ready", {
      message: "Waiting for real packaged keyboard input: Arrow Up on Crop X.",
    });
    const realCropKeyPassed = await waitForSmokeCondition(() => {
      const current = Number((document.getElementById("vfl-crop-x") as HTMLInputElement | null)?.value);
      return realCropUp.received() && current === cropXBefore + 1;
    });
    realCropUp.stop();
    if (!realCropKeyPassed) {
      return { ok: false, message: `Accessibility smoke did not receive the trusted real Crop X Arrow Up step from ${cropXBefore}.` };
    }
    await reportSmokeStatus("keyboard-crop-complete", {
      message: `Real packaged crop keyboard sequence moved Crop X from ${cropXBefore} to ${cropXBefore + 1}.`,
    });

    const currentCropX = document.getElementById("vfl-crop-x");
    if (!(currentCropX instanceof HTMLInputElement)) {
      return { ok: false, message: "Accessibility smoke lost the Crop X input after real keyboard input." };
    }
    const cropKeyEvent = dispatchKey(currentCropX, "ArrowDown");
    await waitMs(160);
    const cropXAfter = Number((document.getElementById("vfl-crop-x") as HTMLInputElement | null)?.value);
    if (!cropKeyEvent.defaultPrevented || cropXAfter !== cropXBefore) {
      return { ok: false, message: `Accessibility smoke could not restore Crop X by its mounted ArrowDown handler (${cropXBefore + 1} -> ${cropXAfter}).` };
    }

    setCropDetectHint("Accessibility smoke crop status.");
    await waitMs(100);
    const cropStatus = document.getElementById("vfl-crop-detect-status");
    if (
      cropStatus?.getAttribute("role") !== "status" ||
      cropStatus.getAttribute("aria-live") !== "polite" ||
      cropStatus.getAttribute("aria-atomic") !== "true" ||
      !cropStatus.textContent?.includes("Accessibility smoke crop status.")
    ) {
      return { ok: false, message: "Accessibility smoke found incomplete crop live-status semantics." };
    }

    const aboutTrigger = document.querySelector<HTMLButtonElement>('button[aria-label="About & updates"]');
    if (!aboutTrigger) {
      return { ok: false, message: "Accessibility smoke could not find the About trigger." };
    }
    if (!(await focusSmokeWebviewTarget(aboutTrigger))) {
      return { ok: false, message: "Accessibility smoke could not focus the WebView and About trigger." };
    }
    await reportSmokeStatus("keyboard-modal-ready", {
      message: "Waiting for packaged accessible activation on About & updates.",
    });
    const realModalOpenPassed = await waitForSmokeCondition(() => {
      const dialog = document.querySelector<HTMLElement>('.vfl-about-modal[role="dialog"]');
      const close = dialog?.querySelector<HTMLButtonElement>('button[aria-label="Close about dialog"]');
      return Boolean(dialog && close && document.activeElement === close);
    });
    if (!realModalOpenPassed) {
      return { ok: false, message: "Accessibility smoke did not receive accessible activation for About." };
    }
    await reportSmokeStatus("keyboard-modal-open", {
      message: "Packaged accessible activation opened About and moved focus to Close; waiting for accessible close activation.",
    });
    const realModalClosePassed = await waitForSmokeCondition(
      () => !document.querySelector('.vfl-about-modal[role="dialog"]') && document.activeElement === aboutTrigger,
    );
    if (!realModalClosePassed) {
      return { ok: false, message: "Accessibility smoke did not receive accessible About close with trigger restoration." };
    }
    await reportSmokeStatus("keyboard-complete", {
      message: "Packaged keyboard and accessible activation flow passed for trim, crop, and About focus restoration.",
    });

    aboutTrigger.click();
    await waitMs(180);

    const aboutDialog = document.querySelector<HTMLElement>('.vfl-about-modal[role="dialog"]');
    const root = document.getElementById("root");
    const closeButton = aboutDialog?.querySelector<HTMLButtonElement>('button[aria-label="Close about dialog"]');
    const lastButton = aboutDialog?.querySelectorAll<HTMLButtonElement>("button:not([disabled])").item(
      Math.max(0, (aboutDialog?.querySelectorAll<HTMLButtonElement>("button:not([disabled])").length ?? 1) - 1),
    );
    if (
      !aboutDialog ||
      aboutDialog.getAttribute("aria-modal") !== "true" ||
      !root?.inert ||
      root.getAttribute("aria-hidden") !== "true" ||
      !closeButton ||
      document.activeElement !== closeButton ||
      !lastButton
    ) {
      return { ok: false, message: "Accessibility smoke found incomplete About modal focus or background isolation." };
    }

    const updateCheckButton = Array.from(aboutDialog.querySelectorAll<HTMLButtonElement>("button")).find((button) =>
      button.textContent?.includes("Check for updates"),
    );
    if (!updateCheckButton) {
      return { ok: false, message: "Accessibility smoke could not find the About update-check control." };
    }
    updateCheckButton.focus();
    setManualUpdateBusy(true);
    await waitMs(100);
    if (!aboutDialog.contains(document.activeElement)) {
      return { ok: false, message: "Accessibility smoke lost modal focus when the focused update-check control became disabled." };
    }
    setManualUpdateBusy(false);
    await waitMs(100);

    lastButton.focus();
    const tabEvent = dispatchKey(lastButton, "Tab");
    await waitMs(80);
    if (!tabEvent.defaultPrevented || document.activeElement !== closeButton) {
      return { ok: false, message: "Accessibility smoke could not wrap Tab from the last About control to Close." };
    }
    closeButton.focus();
    const shiftTabEvent = dispatchKey(closeButton, "Tab", { shiftKey: true });
    await waitMs(80);
    if (!shiftTabEvent.defaultPrevented || document.activeElement !== lastButton) {
      return { ok: false, message: "Accessibility smoke could not wrap Shift+Tab from Close to the last About control." };
    }

    const previewBeforeBlockedShortcut = previewTimeRef.current;
    dispatchKey(window, "ArrowRight");
    await waitMs(80);
    if (Math.abs(previewTimeRef.current - previewBeforeBlockedShortcut) > 0.001) {
      return { ok: false, message: "Accessibility smoke found a compose shortcut changing the preview behind About." };
    }

    const escapeEvent = dispatchKey(document, "Escape");
    await waitMs(180);
    if (
      !escapeEvent.defaultPrevented ||
      document.querySelector('.vfl-about-modal[role="dialog"]') ||
      root?.inert ||
      root?.getAttribute("aria-hidden") === "true" ||
      document.activeElement !== aboutTrigger
    ) {
      return { ok: false, message: "Accessibility smoke found incomplete About Escape close or trigger-focus restoration." };
    }

    setCropDetectHint(null);
    setTrimStart("0");
    setTrimEnd("");
    const trimResetCommitted = await waitForSmokeCondition(() => {
      const timeline = trimTimelineRef.current;
      return Boolean(
        timeline &&
        Math.abs(timeline.start) <= 0.001 &&
        Math.abs(timeline.end - probe.durationS) <= 0.001 &&
        !timeline.hasCustomEnd
      );
    });
    if (!trimResetCommitted) {
      return { ok: false, message: "Accessibility smoke trim reset did not reach the current timeline before interaction checks." };
    }
    await reportSmokeStatus("accessibility-ready", {
      message: "Mounted accessibility checks passed for crop fields, trim sliders, live status, modal focus containment, background isolation, and focus restoration.",
    });

    return {
      ok: true,
      message: "Mounted accessibility checks passed.",
    };
  }

  async function runSmokeInteractionChecks(): Promise<SmokeInteractionResult> {
    if (!probe || !previewReady || !previewMediaReady) {
      return { ok: false, message: "Packaged app smoke tried interaction checks before the preview was ready." };
    }

    const accessibilityResult = await runSmokeAccessibilityChecks();
    if (!accessibilityResult.ok) return accessibilityResult;

    syncPreviewToTime(0.5, "preview", { pause: true });
    const previewStartReady = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "preview" &&
      Math.abs(previewTimeRef.current - 0.5) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - 0.5) <= 0.06,
    );
    if (!previewStartReady) {
      return { ok: false, message: "Packaged app smoke preview selection did not reach 0.50s before the start nudge." };
    }
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not nudge the free preview selection with the keyboard shortcut path." };
    }
    const previewStartNudged = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "preview" &&
      Math.abs(previewTimeRef.current - 0.6) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - 0.6) <= 0.06,
    );
    if (!previewStartNudged) {
      return { ok: false, message: "Packaged app smoke preview selection did not commit the 0.10s start nudge." };
    }

    const rememberedStartS = previewSelectionTimeRef.current;
    const endBeforeStartShortcut = trimTimelineRef.current?.end ?? probe.durationS;
    focusTrimTarget("end");
    const endFocusReady = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "end" &&
      Math.abs(previewTimeRef.current - endBeforeStartShortcut) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - rememberedStartS) <= 0.001,
    );
    if (!endFocusReady) {
      return { ok: false, message: "Packaged app smoke did not focus the current trim end while preserving the remembered start selection." };
    }
    if (!runComposeShortcutAction({ kind: "apply-trim-start" })) {
      return { ok: false, message: "Packaged app smoke could not apply trim start through the keyboard shortcut path." };
    }
    const startShortcutCommitted = await waitForSmokeCondition(() => {
      const timeline = trimTimelineRef.current;
      return Boolean(timeline && Math.abs(timeline.start - rememberedStartS) <= 0.06);
    });
    if (!startShortcutCommitted) {
      return { ok: false, message: "Packaged app smoke Set start action did not reach the current trim timeline." };
    }

    const afterStartShortcut = trimTimelineRef.current;
    if (!afterStartShortcut || Math.abs(afterStartShortcut.start - rememberedStartS) > 0.06) {
      return {
        ok: false,
        message: `Packaged app smoke expected Set start to capture the remembered preview selection (${rememberedStartS.toFixed(2)}s) but saw ${(afterStartShortcut?.start ?? NaN).toFixed(2)}s.`,
      };
    }

    syncPreviewToTime(1.35, "preview", { pause: true });
    const previewEndReady = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "preview" &&
      Math.abs(previewTimeRef.current - 1.35) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - 1.35) <= 0.06,
    );
    if (!previewEndReady) {
      return { ok: false, message: "Packaged app smoke preview selection did not reach 1.35s before the end nudge." };
    }
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not fine-nudge the preview selection for trim end." };
    }
    const previewEndNudged = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "preview" &&
      Math.abs(previewTimeRef.current - 1.45) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - 1.45) <= 0.06,
    );
    if (!previewEndNudged) {
      return { ok: false, message: "Packaged app smoke preview selection did not commit the 0.10s end nudge." };
    }

    const rememberedEndS = previewSelectionTimeRef.current;
    const startBeforeEndShortcut = trimTimelineRef.current?.start ?? rememberedStartS;
    focusTrimTarget("start");
    const startFocusReady = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "start" &&
      Math.abs(previewTimeRef.current - startBeforeEndShortcut) <= 0.06 &&
      Math.abs(previewSelectionTimeRef.current - rememberedEndS) <= 0.001,
    );
    if (!startFocusReady) {
      return { ok: false, message: "Packaged app smoke did not focus the current trim start while preserving the remembered end selection." };
    }
    if (!runComposeShortcutAction({ kind: "apply-trim-end" })) {
      return { ok: false, message: "Packaged app smoke could not apply trim end through the keyboard shortcut path." };
    }
    const endShortcutCommitted = await waitForSmokeCondition(() => {
      const timeline = trimTimelineRef.current;
      return Boolean(timeline && Math.abs(timeline.end - rememberedEndS) <= 0.06);
    });
    if (!endShortcutCommitted) {
      return { ok: false, message: "Packaged app smoke Set end action did not reach the current trim timeline." };
    }

    const afterEndShortcut = trimTimelineRef.current;
    if (!afterEndShortcut || Math.abs(afterEndShortcut.end - rememberedEndS) > 0.06) {
      return {
        ok: false,
        message: `Packaged app smoke expected Set end to capture the remembered preview selection (${rememberedEndS.toFixed(2)}s) but saw ${(afterEndShortcut?.end ?? NaN).toFixed(2)}s.`,
      };
    }

    focusTrimTarget("end");
    const beforeSelectedEndNudge = trimTimelineRef.current?.end ?? rememberedEndS;
    const selectedEndFocusReady = await waitForSmokeCondition(() =>
      activeTrimTargetRef.current === "end" &&
      Math.abs(previewTimeRef.current - beforeSelectedEndNudge) <= 0.06,
    );
    if (!selectedEndFocusReady) {
      return { ok: false, message: "Packaged app smoke did not focus the current trim end before its selected-boundary nudge." };
    }
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: -TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not fine-nudge the selected trim boundary." };
    }
    const expectedEndAfterNudge = beforeSelectedEndNudge - TRIM_FINE_NUDGE_S;
    const selectedEndNudgeCommitted = await waitForSmokeCondition(() => {
      const timeline = trimTimelineRef.current;
      return Boolean(timeline && Math.abs(timeline.end - expectedEndAfterNudge) <= 0.06);
    });
    if (!selectedEndNudgeCommitted) {
      return { ok: false, message: "Packaged app smoke selected trim end nudge did not reach the current timeline." };
    }
    const afterSelectedEndNudge = trimTimelineRef.current;
    if (!afterSelectedEndNudge || Math.abs(afterSelectedEndNudge.end - expectedEndAfterNudge) > 0.06) {
      return {
        ok: false,
        message: `Packaged app smoke expected the selected trim end to nudge to ${expectedEndAfterNudge.toFixed(2)}s but saw ${(afterSelectedEndNudge?.end ?? NaN).toFixed(2)}s.`,
      };
    }

    setCropEnabled(true);
    setCropRect({ x: 0.08, y: 0.08, w: 0.84, h: 0.84 });
    await waitMs(220);

    const smokePlaybackWindowMs = Math.min(
      420,
      Math.max(260, Math.round(Math.max(0.3, afterSelectedEndNudge.end - afterStartShortcut.start) * 450)),
    );
    const startTime = previewTimeRef.current;
    await togglePreviewPlayback();
    await waitMs(220);
    const startedPlaying = previewPlayingRef.current;
    await waitMs(smokePlaybackWindowMs);
    const advancedTime = previewTimeRef.current;
    await togglePreviewPlayback();
    await waitMs(220);
    const pausedAgain = !previewPlayingRef.current;

    if (!startedPlaying) {
      return { ok: false, message: "Packaged app smoke could not start playback while crop was enabled." };
    }
    if (advancedTime <= startTime + 0.2) {
      return {
        ok: false,
        message: `Packaged app smoke playback did not advance under crop mode (${startTime.toFixed(2)}s -> ${advancedTime.toFixed(2)}s).`,
      };
    }
    if (!pausedAgain) {
      return { ok: false, message: "Packaged app smoke could not pause playback after the crop-enabled preview check." };
    }

    smokeMetricsRef.current = {
      trimStartS: afterStartShortcut.start,
      trimEndS: afterSelectedEndNudge.end,
      expectedDurationS: Math.max(0, afterSelectedEndNudge.end - afterStartShortcut.start),
    };

    return {
      ok: true,
      message: `Accessibility checks passed; trim shortcuts kept start at ${afterStartShortcut.start.toFixed(2)}s, end at ${afterSelectedEndNudge.end.toFixed(2)}s, and crop-enabled playback advanced from ${startTime.toFixed(2)}s to ${advancedTime.toFixed(2)}s.`,
      trimStartS: afterStartShortcut.start,
      trimEndS: afterSelectedEndNudge.end,
      expectedDurationS: Math.max(0, afterSelectedEndNudge.end - afterStartShortcut.start),
    };
  }

  async function togglePreviewPlayback() {
    if (!previewReady || jobId !== null || pendingEncodeRef.current !== null) return;
    const nextPlaying = await cropperRef.current?.togglePlayback();
    if (typeof nextPlaying === "boolean") {
      previewPlayingRef.current = nextPlaying;
      if (nextPlaying) {
        activeTrimTargetRef.current = "preview";
        setActiveTrimTarget("preview");
      }
      setPreviewPlaying(nextPlaying);
    }
  }

  function resetTrim() {
    setTrimStart("0");
    setTrimEnd("");
  }

  function createUpdateProgressChannel(onPhaseMessage?: (message: string) => void) {
    setUpdateProgress(createUpdateProgressState());
    updatePhaseRef.current = null;
    return new Channel<UpdateProgressEvent>((event) => {
      setUpdateProgress((current) => reduceUpdateProgress(current, event));
      if (updatePhaseRef.current !== event.phase) {
        updatePhaseRef.current = event.phase;
        onPhaseMessage?.(event.message);
      }
    });
  }

  async function dismissUpdate(choice: "remindLater" | "skip7days" | "dismiss") {
    if (!updateNotice?.latestVersion) {
      setUpdateNotice(null);
      return;
    }

    try {
      await invoke("record_update_prompt_choice", {
        choice,
        version: updateNotice.latestVersion,
      });
      setUpdateNotice(null);
      setUpdateStatus(null);
    } catch (error) {
      setUpdateStatus(
        coerceUpdatePublicError(error, "The update preference could not be saved. Try again.").message,
      );
    }
  }

  async function checkForUpdatesNow() {
    if (manualUpdateBusy || updateBusy) return;

    setManualUpdateBusy(true);
    setManualUpdateStatus("Checking for updates...");
    try {
      const result = await invoke<UpdateCheckResponse>("check_for_update", {
        force: true,
        onEvent: createUpdateProgressChannel(setManualUpdateStatus),
      });
      if (result.status === "available") {
        setUpdateNotice(result);
        setUpdateStatus(null);
        setManualUpdateStatus(`Video For Lazies ${result.latestVersion} is ready.`);
      } else {
        setUpdateNotice(null);
        setUpdateStatus(null);
        setManualUpdateStatus(result.reason ?? "Video For Lazies is up to date.");
      }
    } catch (error) {
      setManualUpdateStatus(
        coerceUpdatePublicError(error, "Video For Lazies could not check for updates. Check your connection and try again.").message,
      );
    } finally {
      setManualUpdateBusy(false);
    }
  }

  async function applyUpdate() {
    if (
      updateBusyRef.current ||
      jobIdRef.current !== null ||
      pendingEncodeRef.current !== null ||
      exportQueueStateRef.current.autoRun ||
      queuePreparationCountRef.current > 0 ||
      cropDetecting ||
      frameSaving
    ) return;
    updateBusyRef.current = true;
    setUpdateBusy(true);
    setUpdatePublicError(null);
    setUpdateStatus("Preparing update...");
    try {
      const result = await invoke<UpdateApplyResponse>("prepare_and_apply_update", {
        onEvent: createUpdateProgressChannel(setUpdateStatus),
      });
      setUpdateStatus(result.message);
    } catch (error) {
      const publicError = coerceUpdatePublicError(
        error,
        "The update could not be prepared safely. Your current app was not changed. Try again, or download the portable release manually.",
      );
      setUpdatePublicError(publicError);
      setUpdateStatus(publicError.message);
      updateBusyRef.current = false;
      setUpdateBusy(false);
    }
  }

  const previewColorFilter = useMemo(
    () => buildPreviewColorFilter(brightness, contrast, saturation),
    [brightness, contrast, saturation],
  );

  const trimSelectionStyle =
    trimTimeline && previewDurationS > 0
      ? {
          left: `${(trimTimeline.start / previewDurationS) * 100}%`,
          width: `${Math.max(0, ((trimTimeline.end - trimTimeline.start) / previewDurationS) * 100)}%`,
        }
      : null;
  const trimPlayheadStyle =
    previewDurationS > 0
      ? {
          left: `${(clampedPreviewTimeS / previewDurationS) * 100}%`,
        }
      : null;
  const selectedTrimBoundary = activeTrimTarget === "preview" ? null : activeTrimTarget;
  const trimShortcutHint = selectedTrimBoundary
    ? `Arrow keys nudge ${formatTrimTargetLabel(selectedTrimBoundary)} by 0.1s. Shift or Page Up/Down uses 1s; Home/End moves to the limit. [ sets Start, ] sets End, and Space plays.`
    : "Select Start or End to edit that boundary. Arrow keys nudge by 0.1s, Shift uses 1s, [ sets Start, ] sets End, and Space plays.";
  const visibleRecipeStatus = recipeStatus ?? recipeNotification?.message ?? null;
  const recipeStatusIsError = recipeStatus !== null && userRecipeStore.readOnly;
  const recipeStatusIsFading = recipeStatus === null && recipeNotification?.isFading === true;

  return (
    <div className="vfl-app">
      {dragActive && !modalOpen ? (
        <div className="vfl-drop-overlay" aria-hidden="true">
          <div className="vfl-drop-overlay-card">
            {updateBusy
              ? "Finish the current update before adding files"
              : encodeBusy
                ? "Drop files to add them after the current export"
                : "Drop one file to open, or several to queue"}
          </div>
        </div>
      ) : null}
      {elevatedUpdateRun ? (
        <ModalDialog
          role="alertdialog"
          className="vfl-elevated-update-card"
          labelledBy="vfl-elevated-update-title"
          describedBy="vfl-elevated-update-status"
          initialFocus={updateBusy ? "container" : "first"}
          onRequestClose={updateBusy ? undefined : () => setElevatedUpdateRun(false)}
        >
          <div className="vfl-about-title" id="vfl-elevated-update-title">Installing update</div>
          <div className="vfl-muted" id="vfl-elevated-update-status" role="status" aria-live="assertive" aria-atomic="true">
            {updateBusy
              ? updateStatus ?? "Installing the update with administrator permission. The app restarts automatically."
              : updateStatus ?? "Installing the update with administrator permission."}
          </div>
          {updateBusy ? (
            <div
              className="vfl-progress"
              role="progressbar"
              aria-label="Elevated update progress"
              aria-valuemin={updateProgressUi.determinate ? 0 : undefined}
              aria-valuemax={updateProgressUi.determinate ? 100 : undefined}
              aria-valuenow={updateProgressUi.percent ?? undefined}
              aria-valuetext={updateProgressUi.valueText}
            >
              <div className="vfl-progress-meta">
                <span>{updateProgressUi.label}</span>
                {updateProgressUi.percent !== null ? <span>{updateProgressUi.percent}%</span> : null}
              </div>
            </div>
          ) : null}
          {!updateBusy ? (
            <div className="vfl-actions">
              <button onClick={() => void applyUpdate()}>Try again</button>
              <button onClick={() => setElevatedUpdateRun(false)}>Continue without updating</button>
            </div>
          ) : null}
        </ModalDialog>
      ) : aboutOpen ? (
        <ModalDialog
          className="vfl-about-modal"
          labelledBy="vfl-about-title"
          initialFocus="first"
          onRequestClose={() => setAboutOpen(false)}
          closeOnBackdrop
        >
            <div className="vfl-about-head">
              <div>
                <div className="vfl-about-kicker">About & Legal</div>
                <div className="vfl-about-title" id="vfl-about-title">Video For Lazies</div>
              </div>
              <button className="vfl-modal-close" type="button" aria-label="Close about dialog" onClick={() => setAboutOpen(false)}>
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M7 7l10 10M17 7 7 17" />
                </svg>
              </button>
            </div>
            <div className="vfl-legal-list">
              <div className="vfl-legal-row">
                <div className="vfl-summary-label">App</div>
                <div className="vfl-summary-value">Version {APP_VERSION}</div>
              </div>
              <div className="vfl-legal-row">
                <div className="vfl-summary-label">Runtime</div>
                <div className="vfl-summary-value">Portable builds include pinned GPL FFmpeg sidecars with libx264.</div>
              </div>
              <div className="vfl-legal-row">
                <div className="vfl-summary-label">Updates</div>
                <div className="vfl-update-row-content">
                  <button onClick={() => void checkForUpdatesNow()} disabled={manualUpdateBusy || updateBusy}>
                    {manualUpdateBusy ? "Checking..." : "Check for updates"}
                  </button>
                  {manualUpdateStatus ? (
                    <div className="vfl-update-inline-status" role="status" aria-live="polite" aria-atomic="true">
                      {manualUpdateStatus}
                    </div>
                  ) : null}
                </div>
              </div>
            </div>
            <div className="vfl-about-links" aria-label="Project links">
              <button type="button" onClick={() => void openExternalUrl(APP_LINKS.github)}>
                GitHub
              </button>
              <button type="button" onClick={() => void openExternalUrl(APP_LINKS.releases)}>
                Releases
              </button>
              <button type="button" onClick={() => void openExternalUrl(APP_LINKS.security)}>
                Security
              </button>
            </div>
        </ModalDialog>
      ) : resetConfirmationOpen ? (
        <ModalDialog
          role="alertdialog"
          className="vfl-reset-confirmation"
          labelledBy="vfl-reset-confirmation-title"
          describedBy="vfl-reset-confirmation-description"
          initialFocus="first"
          onRequestClose={() => setResetConfirmationOpen(false)}
          closeOnBackdrop
        >
          <div className="vfl-about-title" id="vfl-reset-confirmation-title">Reset all settings?</div>
          <p className="vfl-muted" id="vfl-reset-confirmation-description">
            This clears the current export settings, trim, crop, color edits, and subtitle selection. It does not remove the source, output file, queue, or saved recipes.
          </p>
          <div className="vfl-actions">
            <button
              type="button"
              data-smoke-id="reset-all-cancel"
              onClick={() => setResetConfirmationOpen(false)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="danger"
              data-smoke-id="reset-all-confirm"
              onClick={performResetAllSettings}
            >
              Reset settings
            </button>
          </div>
        </ModalDialog>
      ) : recipeDialog ? (
        <UserRecipeDialog
          state={recipeDialog}
          nameDraft={recipeNameDraft}
          descriptionDraft={recipeDescriptionDraft}
          resetToCurrentSettings={recipeResetToCurrentSettings}
          error={recipeDialogError}
          onNameDraftChange={(name) => {
            setRecipeNameDraft(name);
            setRecipeDialogError(null);
          }}
          onDescriptionDraftChange={(description) => {
            setRecipeDescriptionDraft(description);
            setRecipeDialogError(null);
          }}
          onResetToCurrentSettingsChange={(enabled) => {
            setRecipeResetToCurrentSettings(enabled);
            setRecipeDialogError(null);
          }}
          onConfirm={confirmRecipeDialog}
          onRequestDelete={requestDeleteRecipe}
          onCancel={cancelRecipeDialog}
        />
      ) : null}
      <header className="vfl-header">
        {updateStartupNotice ? (
          <div className={`vfl-update-banner ${updateStartupNotice.kind === "error" ? "is-error" : ""}`}>
            <div className="vfl-update-copy">
              <div className="vfl-update-kicker">Update recovery</div>
              <div
                className="vfl-update-summary"
                role={updateStartupNotice.kind === "error" ? "alert" : "status"}
                aria-live={updateStartupNotice.kind === "error" ? "assertive" : "polite"}
                aria-atomic="true"
              >
                {updateStartupNotice.message}
              </div>
            </div>
            <div className="vfl-update-actions">
              <button type="button" onClick={() => void openExternalUrl(APP_LINKS.releases)}>
                Open portable releases
              </button>
              {updateStartupNotice.kind === "status" ? (
                <button type="button" onClick={() => setUpdateStartupNotice(null)}>Dismiss</button>
              ) : null}
            </div>
          </div>
        ) : null}
        {updateNotice ? (
          <div className="vfl-update-banner" aria-busy={updateBusy}>
            <div className="vfl-update-copy">
              <div className="vfl-update-kicker">Update available</div>
              <div className="vfl-update-title">
                Video For Lazies {updateNotice.latestVersion}
                {updateNotice.artifact?.sizeBytes ? ` (${(updateNotice.artifact.sizeBytes / (1024 * 1024)).toFixed(1)} MB)` : ""}
              </div>
              <div
                className="vfl-update-summary"
                role={updatePublicError ? "alert" : "status"}
                aria-live={updatePublicError ? "assertive" : "polite"}
                aria-atomic="true"
              >
                {updateStatus ?? updateNotice.notes?.summary ?? "A new portable release is ready."}
              </div>
              {updateBusy ? (
                <div
                  className="vfl-progress"
                  role="progressbar"
                  aria-label="Update progress"
                  aria-valuemin={updateProgressUi.determinate ? 0 : undefined}
                  aria-valuemax={updateProgressUi.determinate ? 100 : undefined}
                  aria-valuenow={updateProgressUi.percent ?? undefined}
                  aria-valuetext={updateProgressUi.valueText}
                >
                  <div className="vfl-progress-meta">
                    <span>{updateProgressUi.label}</span>
                    {updateProgressUi.percent !== null ? <span>{updateProgressUi.percent}%</span> : null}
                  </div>
                  {updateProgressUi.percent !== null ? (
                    <div className="vfl-progress-bar">
                      <div className="vfl-progress-fill" style={{ width: `${updateProgressUi.percent}%` }} />
                    </div>
                  ) : null}
                </div>
              ) : null}
            </div>
            <div className="vfl-update-actions">
              <button className="primary" onClick={() => void applyUpdate()} disabled={updateBusy || encodeBusy}>
                {updateBusy ? "Updating..." : updatePublicError ? "Retry update" : "Update now"}
              </button>
              {updatePublicError ? (
                <button type="button" onClick={() => void openExternalUrl(APP_LINKS.releases)} disabled={updateBusy}>
                  Download portable release
                </button>
              ) : null}
              <button onClick={() => void dismissUpdate("remindLater")} disabled={updateBusy}>
                Remind me later
              </button>
              <button onClick={() => void dismissUpdate("skip7days")} disabled={updateBusy}>
                Skip
              </button>
            </div>
          </div>
        ) : null}
        <div className="vfl-header-top">
          <div className="vfl-title">
            <div className="vfl-title-main">Video For Lazies</div>
          </div>
          <div className="vfl-header-utilities">
            <div className="vfl-header-badges">
              <div className="vfl-header-badge">{sourceBadgeText}</div>
              <div className="vfl-header-badge">{format.toUpperCase()} export</div>
              {sizeLimitEnabled ? <div className="vfl-header-badge">{sizeLimitMb} MB target</div> : null}
              {advancedOverrideCount > 0 ? <div className="vfl-header-badge">Advanced {advancedOverrideCount}</div> : null}
              {activeEditChips.length ? (
                <div className="vfl-header-badge subtle">
                  {activeEditChips.length} active edit{activeEditChips.length === 1 ? "" : "s"}
                </div>
              ) : null}
            </div>
            <button
              className="vfl-about-button"
              type="button"
              aria-label="About & updates"
              title="About & updates"
              onClick={() => setAboutOpen(true)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <circle cx="12" cy="12" r="9" />
                <path d="M12 11v6" />
                <path d="M12 7h.01" />
              </svg>
            </button>
          </div>
        </div>

      </header>

      <div className="vfl-split">
        <section className="vfl-pane-left">
              <div className="vfl-preview-section">
                {probe && inputPath ? (
                  previewError ? (
                    <div className="vfl-error" role="alert">{previewError}</div>
                  ) : !previewReady ? (
                    <div className="vfl-muted" role="status" aria-live="polite" aria-atomic="true">Loading preview…</div>
                  ) : (
                    <div className="vfl-preview-layout">
                      <div className="vfl-cropper-wrap">
                        <VideoCropper
                          ref={cropperRef}
                          src={videoSrc}
                          rect={cropRect}
                          onChange={setCropRect}
                          aspect={aspect}
                          frameAspectRatio={probe.width / probe.height}
                          rotationDeg={rotateDeg}
                          cropEnabled={cropEnabled}
                          disabled={encodeBusy}
                          colorFilter={previewColorFilter}
                          onTimeUpdate={setPreviewTimeS}
                          onPlaybackChange={setPreviewPlaying}
                          onSourceReadyChange={setPreviewMediaReady}
                        />
                      </div>

                      <input
                        className="vfl-scrubber"
                        type="range"
                        min={0}
                        max={previewDurationS || 0}
                        step={0.01}
                        value={clampedPreviewTimeS}
                        style={rangeFillStyle(clampedPreviewTimeS, 0, previewDurationS || 0)}
                        onChange={(e) => syncPreviewToTime(Number(e.currentTarget.value), "preview")}
                        disabled={encodeBusy || !previewReady || !probe}
                        aria-label="Preview timeline"
                      />

                      <div className="vfl-transport-row">
                        <button className="primary" onClick={togglePreviewPlayback} disabled={encodeBusy || !previewReady || !probe}>
                          {previewPlaying ? "Pause" : "Play"}
                        </button>
                        <button onClick={() => stepPreview(-5)} disabled={encodeBusy || !previewReady || !probe}>
                          Back 5s
                        </button>
                        <button onClick={() => stepPreview(5)} disabled={encodeBusy || !previewReady || !probe}>
                          Forward 5s
                        </button>
                        <button
                          type="button"
                          className="vfl-icon-button"
                          onClick={saveCurrentFrame}
                          disabled={encodeBusy || !previewReady || !probe || frameSaving || Boolean(frameExportBlockingReason)}
                          title={frameExportBlockingReason ?? "Save Frame"}
                          aria-label="Save the current preview frame as a PNG"
                          aria-busy={frameSaving}
                        >
                          <svg viewBox="0 0 24 24" aria-hidden="true">
                            <path d="M4.5 7.5h3.2l1.6-2.3h5.4l1.6 2.3h3.2a1 1 0 0 1 1 1v9.3a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1V8.5a1 1 0 0 1 1-1Z" />
                            <circle cx="12" cy="13" r="3.4" />
                          </svg>
                        </button>
                        <div className="vfl-transport-time">
                          {formatClock(clampedPreviewTimeS)} / {formatClock(previewDurationS)}
                        </div>
                      </div>
                      {frameExportBlockingReason ? (
                        <div className="vfl-inline-hint" role="status">
                          Save Frame unavailable: {frameExportBlockingReason}
                        </div>
                      ) : null}

                      {trimTimeline ? (
                        <div className="vfl-trim-block">
                          <div className="vfl-trim-head">
                            <span className="vfl-trim-label">Trim</span>
                            <span className="vfl-trim-range-summary">
                              {formatClock(trimTimeline.start)} to {trimTimeline.hasCustomEnd ? formatClock(trimTimeline.end) : "end"}
                            </span>
                            <span className="vfl-trim-kept-chip" title={trimSummary ?? undefined}>
                              {(trimTimeline.end - trimTimeline.start).toFixed(2)} s kept
                            </span>
                          </div>
                          <div
                            className="vfl-trim-timeline-track"
                            ref={trimTimelineTrackRef}
                            title="Drag either trim handle or click Start/End above to make the preview jump to that boundary."
                          >
                            <div className="vfl-trim-timeline-rail" />
                            {trimSelectionStyle ? <div className="vfl-trim-timeline-selection" style={trimSelectionStyle} /> : null}
                            {trimPlayheadStyle ? <div className="vfl-trim-timeline-playhead" style={trimPlayheadStyle} /> : null}
                            <TrimSliderHandle
                              id="vfl-trim-start-slider"
                              label="Trim start"
                              value={trimTimeline.start}
                              valueText={`${formatClock(trimTimeline.start)}, ${trimTimeline.start.toFixed(2)} seconds`}
                              min={0}
                              max={Math.max(0, trimTimeline.end - trimTimeline.minGap)}
                              leftPercent={(trimTimeline.start / previewDurationS) * 100}
                              active={activeTrimTarget === "start"}
                              onPointerDown={(event) => beginTrimHandleDrag("start", event)}
                              onFocus={() => focusTrimTarget("start")}
                              onChange={(value) => updateTrimTarget("start", value, { pause: true })}
                              disabled={encodeBusy || !previewReady || !probe || previewDurationS <= 0}
                            />
                            <TrimSliderHandle
                              id="vfl-trim-end-slider"
                              label="Trim end"
                              value={trimTimeline.end}
                              valueText={`${formatClock(trimTimeline.end)}, ${trimTimeline.end.toFixed(2)} seconds`}
                              min={Math.min(previewDurationS, trimTimeline.start + trimTimeline.minGap)}
                              max={previewDurationS}
                              leftPercent={(trimTimeline.end / previewDurationS) * 100}
                              active={activeTrimTarget === "end"}
                              onPointerDown={(event) => beginTrimHandleDrag("end", event)}
                              onFocus={() => focusTrimTarget("end")}
                              onChange={(value) => updateTrimTarget("end", value, { pause: true })}
                              disabled={encodeBusy || !previewReady || !probe || previewDurationS <= 0}
                            />
                          </div>
                          <div className="vfl-trim-row">
                            <div className="vfl-trim-field">
                              <label htmlFor="vfl-trim-start">Start (s)</label>
                              <input
                                id="vfl-trim-start"
                                value={trimStart}
                                onFocus={() => focusTrimTarget("start")}
                                onChange={(e) => setTrimStart(e.currentTarget.value)}
                                disabled={encodeBusy}
                                inputMode="decimal"
                              />
                              <button
                                type="button"
                                onClick={applyTrimStartFromCurrent}
                                disabled={encodeBusy || !previewReady || !probe}
                                title="Use the current preview time"
                              >
                                Set
                              </button>
                            </div>
                            <div className="vfl-trim-field">
                              <label htmlFor="vfl-trim-end">End (s)</label>
                              <input
                                id="vfl-trim-end"
                                value={trimEnd}
                                onFocus={() => focusTrimTarget("end")}
                                onChange={(e) => setTrimEnd(e.currentTarget.value)}
                                disabled={encodeBusy}
                                placeholder="(end)"
                                inputMode="decimal"
                              />
                              <button
                                type="button"
                                onClick={applyTrimEndFromCurrent}
                                disabled={encodeBusy || !previewReady || !probe}
                                title="Use the current preview time"
                              >
                                Set
                              </button>
                              <button type="button" onClick={() => setTrimEnd("")} disabled={encodeBusy || trimEnd.trim() === ""}>
                                Clear
                              </button>
                            </div>
                            <div
                              className="vfl-trim-field"
                              title={
                                trimDragSnapIntervalS > 0
                                  ? `While dragging Start or End, the handle snaps in ${trimDragSnapIntervalS}s steps and never allows a value above the current clip length.`
                                  : "0 disables snapping."
                              }
                            >
                              <label htmlFor="trim-drag-snap">Drag snap (s)</label>
                              <input
                                id="trim-drag-snap"
                                type="number"
                                min={0}
                                max={trimDragSnapInputMaxS}
                                step={1}
                                value={trimDragSnapS}
                                onChange={(e) => setTrimDragSnapS(normalizeTrimDragSnapInput(e.currentTarget.value, probe?.durationS ?? null))}
                                disabled={encodeBusy || !probe}
                                inputMode="numeric"
                              />
                            </div>
                            <button
                              type="button"
                              className="vfl-trim-reset"
                              data-smoke-id="reset-trim"
                              onClick={resetTrim}
                              disabled={encodeBusy}
                            >
                              Reset trim
                            </button>
                          </div>
                          <div className="vfl-kbd-hint">{trimShortcutHint}</div>
                        </div>
                      ) : null}
                    </div>
                  )
                ) : (
                  <div className="vfl-empty-state">
                    <div className="vfl-empty-copy">
                      <div className="vfl-section-title">Start with a source video</div>
                      <div className="vfl-muted vfl-section-caption">
                        Drop a file anywhere in the window or browse for one. Once a source is loaded, the preview, trim,
                        and crop tools live here.
                      </div>
                      <div className="vfl-actions">
                        <button className="primary" onClick={pickInput} disabled={encodeBusy}>
                          Browse…
                        </button>
                      </div>
                    </div>
                    <div className="vfl-empty-steps" aria-label="Getting started">
                      <div className="vfl-empty-step">
                        <div className="vfl-empty-step-index">1</div>
                        <div>
                          <div className="vfl-empty-step-title">Load a source</div>
                          <div className="vfl-empty-step-copy">Point the app at a clip, then let it probe the resolution, duration, and audio track.</div>
                        </div>
                      </div>
                      <div className="vfl-empty-step">
                        <div className="vfl-empty-step-index">2</div>
                        <div>
                          <div className="vfl-empty-step-title">Shape the export</div>
                          <div className="vfl-empty-step-copy">Pick a recipe or adjust format, size, and encoder settings in the panel on the right.</div>
                        </div>
                      </div>
                      <div className="vfl-empty-step">
                        <div className="vfl-empty-step-index">3</div>
                        <div>
                          <div className="vfl-empty-step-title">Export from the plan</div>
                          <div className="vfl-empty-step-copy">Check the Current plan card, then use the bottom bar to export once the output path is ready.</div>
                        </div>
                      </div>
                    </div>
                  </div>
                )}
              </div>
        </section>

        <aside className="vfl-rail">
          <div
            id="vfl-subtitle-live-status"
            className="vfl-sr-only"
            role="status"
            aria-live="polite"
            aria-atomic="true"
          >
            {subtitleStatus}
          </div>
          {probeError ? <div className="vfl-error" role="alert">{probeError}</div> : null}
          <RailCard
            title="Source & destination"
            summary={inputPath ? basename(inputPath) : "No source"}
            open={openCards.source}
            onToggle={() => toggleCard("source")}
          >
            <div className="vfl-field">
              <label htmlFor="vfl-input-path">Input</label>
              <input id="vfl-input-path" value={inputPath} placeholder="Pick a video…" readOnly />
              <div className="vfl-file-actions">
                <button className={!inputPath ? "primary" : ""} onClick={pickInput} disabled={encodeBusy}>
                  Browse…
                </button>
              </div>
              <div className="vfl-inline-hint">Drag and drop works anywhere in the window.</div>
              {probe ? (
                <div className="vfl-source-facts" aria-label="Source media facts">
                  <span>{sourceColorSummary}</span>
                  <span>{sourceSarSummary}</span>
                  {probe.attachedPictureCount ? (
                    <span>{probe.attachedPictureCount} attached picture{probe.attachedPictureCount === 1 ? "" : "s"} ignored</span>
                  ) : null}
                  {probe.unsupportedRotationDeg !== null && probe.unsupportedRotationDeg !== undefined ? (
                    <span>{probe.unsupportedRotationDeg.toFixed(3)}° source rotation is unsafe for video geometry</span>
                  ) : null}
                </div>
              ) : null}
            </div>
            <div className="vfl-field">
              <label htmlFor="vfl-output-path">Output</label>
              <input id="vfl-output-path" value={outputPath} placeholder="Pick an output path…" readOnly />
              <div className="vfl-file-actions">
                <button className={!outputPath ? "primary" : ""} onClick={pickOutput} disabled={!inputPath || encodeBusy}>
                  Save as…
                </button>
                <button onClick={openOutputFolder} disabled={(!outputPath && !inputPath) || encodeBusy}>
                  Open folder
                </button>
              </div>
              <div className="vfl-inline-hint">
                {outputAuto ? "Auto-suggested beside the input file. Change it any time." : "Custom output path selected."}
              </div>
            </div>
          </RailCard>

          <RailCard
            title="Recipes"
            summary={matchingRecipeLabel ?? "Custom settings"}
            open={openCards.recipes}
            onToggle={() => toggleCard("recipes")}
          >
            <div className="vfl-muted vfl-section-caption">
              Apply a starting point, then adjust any setting before exporting.
            </div>
            <div className="vfl-recipe-grid">
              {EXPORT_RECIPES.map((recipe) => {
                const isActive = matchingUserRecipe === null && (
                  matchingFullBuiltInRecipe?.id === recipe.id ||
                  (matchingFullBuiltInRecipe === null && matchingPartialBuiltInRecipe?.id === recipe.id)
                );
                const isDisabled =
                  encodeBusy ||
                  (recipe.partial && format === "mp3") ||
                  (recipe.settings.format === "mp3" && probe !== null && !probe.hasAudio);
                return (
                  <button
                    key={recipe.id}
                    type="button"
                    className={`vfl-recipe-option ${isActive ? "active" : ""}`}
                    onClick={() => applyExportRecipe(recipe)}
                    disabled={isDisabled}
                    aria-pressed={isActive}
                  >
                    <span className="vfl-recipe-option-title">{recipe.label}</span>
                    <span className="vfl-recipe-option-copy">{recipe.description}</span>
                  </button>
                );
              })}
              {userRecipeStore.recipes.map((recipe) => {
                const isActive = matchingUserRecipe?.id === recipe.id;
                const unavailable = encodeBusy || (recipe.settings.format === "mp3" && probe !== null && !probe.hasAudio);
                const recipeTitleId = `vfl-user-recipe-title-${recipe.id}`;
                const recipeDescriptionId = `vfl-user-recipe-description-${recipe.id}`;
                return (
                  <div
                    key={recipe.id}
                    className={`vfl-user-recipe-tile ${isActive ? "active" : ""}`}
                    data-user-recipe-id={recipe.id}
                  >
                    <button
                      type="button"
                      className={`vfl-recipe-option vfl-user-recipe-apply ${isActive ? "active" : ""}`}
                      data-recipe-action="apply"
                      aria-labelledby={recipeTitleId}
                      aria-describedby={recipeDescriptionId}
                      aria-pressed={isActive}
                      onClick={() => applyUserRecipe(recipe)}
                      disabled={unavailable}
                    >
                      <span id={recipeTitleId} className="vfl-recipe-option-title">{recipe.name}</span>
                      <span id={recipeDescriptionId} className="vfl-recipe-option-copy">{recipe.description || "Saved settings"}</span>
                    </button>
                    <button
                      type="button"
                      className="vfl-user-recipe-edit"
                      aria-label={`Edit ${recipe.name}`}
                      title={`Edit ${recipe.name}`}
                      onClick={() => openEditRecipeDialog(recipe)}
                      disabled={encodeBusy || userRecipeStore.readOnly}
                    >
                      <svg viewBox="0 0 16 16" aria-hidden="true">
                        <path d="M11.9 1.7a1.4 1.4 0 0 1 2 2L6 11.6 2.7 12.3l.7-3.3 8.5-7.3Zm-7.7 7.8-.3 1.3 1.3-.3 6.4-6.4-1-1-6.4 6.4Z" />
                      </svg>
                    </button>
                  </div>
                );
              })}
              <button
                ref={recipeSaveButtonRef}
                type="button"
                className="vfl-recipe-add"
                data-smoke-id="save-current-recipe"
                aria-label="Create recipe from current settings"
                title="Create recipe from current settings"
                onClick={openCreateRecipeDialog}
                disabled={
                  encodeBusy ||
                  userRecipeStore.readOnly ||
                  userRecipeStore.recipes.length >= 50
                }
              >
                <span aria-hidden="true">+</span>
              </button>
            </div>
            {visibleRecipeStatus ? (
              <div
                className={`${recipeStatusIsError ? "vfl-error" : "vfl-inline-hint vfl-recipe-status"}${recipeStatusIsFading ? " is-fading" : ""}`}
                role={recipeStatusIsError ? "alert" : "status"}
                aria-live={recipeStatusIsError ? "assertive" : "polite"}
                aria-atomic="true"
                data-smoke-id="user-recipe-status"
              >
                {visibleRecipeStatus}
              </div>
            ) : null}
          </RailCard>

          <RailCard
            title="Export settings"
            summary={`${format.toUpperCase()}${sizeLimitEnabled ? ` • ${sizeLimitMb} MB` : ""}`}
            open={openCards.output}
            onToggle={() => toggleCard("output")}
          >
            <div className="vfl-stack-md">
              <div className="vfl-field">
                <label htmlFor="vfl-format">Format</label>
                <select id="vfl-format" value={format} onChange={(e) => setFormat(e.currentTarget.value as OutputFormat)} disabled={encodeBusy}>
                  <option value="mp4">mp4</option>
                  <option value="webm">webm</option>
                  <option value="mp3" disabled={probe !== null && !probe.hasAudio}>
                    mp3
                  </option>
                </select>
              </div>
              <div className="vfl-field">
                <div className="vfl-field-label">Audio</div>
                <label className="vfl-check vfl-check-card">
                  <input
                    type="checkbox"
                    checked={audioEnabled}
                    onChange={(e) => {
                      autoMutedRef.current = false;
                      setAudioEnabled(e.currentTarget.checked);
                    }}
                    disabled={encodeBusy || format === "mp3" || !probe?.hasAudio}
                  />
                  <span>{format === "mp3" ? "Always enabled for mp3 export" : "Include audio in the export"}</span>
                </label>
              </div>
              <div className="vfl-field">
                <label htmlFor="vfl-size-limit">Size limit (MB)</label>
                <input
                  id="vfl-size-limit"
                  value={sizeLimitMb}
                  onChange={(e) => setSizeLimitMb(e.currentTarget.value)}
                  disabled={encodeBusy}
                  placeholder="0 or empty = no limit"
                  aria-describedby="vfl-size-limit-hint"
                  aria-invalid={sizeTargetExactnessBlockingReason ? true : undefined}
                />
                <div id="vfl-size-limit-hint" className="vfl-inline-hint">
                  Target checks use exact bytes; values too large for exact byte tracking are rejected.
                </div>
                {sizeTargetExactnessBlockingReason ? (
                  <div className="vfl-error" role="alert">{sizeTargetExactnessBlockingReason}</div>
                ) : null}
                <div className="vfl-chips" aria-label="Size presets">
                  <button
                    type="button"
                    className={`vfl-preset-chip ${Number(sizeLimitMb) === 0 ? "active" : ""}`}
                    onClick={() => setSizeLimitMb("")}
                    disabled={encodeBusy}
                  >
                    No limit
                  </button>
                  {SIZE_PRESETS_MB.map((v) => {
                    const active = Number(sizeLimitMb) === v;
                    return (
                      <button
                        key={v}
                        type="button"
                        className={`vfl-preset-chip ${active ? "active" : ""}`}
                        onClick={() => setSizeLimitMb(String(v))}
                        disabled={encodeBusy}
                        title={SIZE_PRESET_HINTS[v]}
                      >
                        {v} MB
                      </button>
                    );
                  })}
                </div>
              </div>
              <label className="vfl-check vfl-strict-fit-toggle">
                <input
                  id="vfl-strict-fit"
                  type="checkbox"
                  checked={strictFit}
                  onChange={(event) => setStrictFit(event.currentTarget.checked)}
                  disabled={encodeBusy || format === "mp3" || !sizeLimitEnabled}
                />
                <span>Strict Fit</span>
              </label>
              <div className="vfl-field vfl-output-dimensions-field">
                <div className="vfl-field-label">Output dimensions</div>
                <div className="vfl-segmented" role="group" aria-label="Output dimensions">
                  <button
                    type="button"
                    className={resizeMode === "source" ? "active" : ""}
                    onClick={() => handleResizeModeChange("source")}
                    disabled={encodeBusy || format === "mp3"}
                    aria-pressed={resizeMode === "source"}
                  >
                    Original
                  </button>
                  <button
                    type="button"
                    className={resizeMode === "maxEdge" ? "active" : ""}
                    onClick={() => handleResizeModeChange("maxEdge")}
                    disabled={encodeBusy || format === "mp3"}
                    aria-pressed={resizeMode === "maxEdge"}
                  >
                    Max edge
                  </button>
                  <button
                    type="button"
                    className={resizeMode === "custom" ? "active" : ""}
                    onClick={() => handleResizeModeChange("custom")}
                    disabled={encodeBusy || format === "mp3"}
                    aria-pressed={resizeMode === "custom"}
                  >
                    Custom
                  </button>
                </div>

                <div className="vfl-output-dimensions-panel">
                  {format === "mp3" ? (
                    <div className="vfl-inline-hint">MP3 exports audio only.</div>
                  ) : resizeMode === "source" ? (
                    <div className="vfl-inline-hint">Keeps the shaped source size.</div>
                  ) : resizeMode === "maxEdge" ? (
                    <>
                      <label htmlFor="vfl-max-edge">Max edge (px)</label>
                      <input
                        id="vfl-max-edge"
                        value={maxEdgePx}
                        onChange={(e) => setMaxEdgePx(e.currentTarget.value)}
                        disabled={encodeBusy}
                        placeholder="720"
                        inputMode="numeric"
                      />
                      <div className="vfl-inline-hint">Scales down the long edge, keeps aspect ratio, never upscales.</div>
                    </>
                  ) : (
                    <>
                      <div className="vfl-dimensions-grid">
                        <div>
                          <label htmlFor="vfl-output-width">Width (px)</label>
                          <input
                            id="vfl-output-width"
                            value={customWidthPx}
                            onChange={(e) => handleCustomWidthChange(e.currentTarget.value)}
                            disabled={encodeBusy}
                            placeholder={shapedVideoDimensions ? String(shapedVideoDimensions.width) : "1280"}
                            inputMode="numeric"
                          />
                        </div>
                        <div>
                          <label htmlFor="vfl-output-height">Height (px)</label>
                          <input
                            id="vfl-output-height"
                            value={customHeightPx}
                            onChange={(e) => handleCustomHeightChange(e.currentTarget.value)}
                            disabled={encodeBusy}
                            placeholder={shapedVideoDimensions ? String(shapedVideoDimensions.height) : "720"}
                            inputMode="numeric"
                          />
                        </div>
                      </div>
                      <label className="vfl-check vfl-check-card vfl-dimension-lock">
                        <input
                          type="checkbox"
                          checked={outputAspectLocked}
                          onChange={(e) => handleOutputAspectLockChange(e.currentTarget.checked)}
                          disabled={encodeBusy}
                        />
                        <span>Lock aspect ratio</span>
                      </label>
                      <div className="vfl-inline-hint">Odd values are snapped down to even pixels for encoder compatibility.</div>
                    </>
                  )}
                </div>
                <div className="vfl-inline-hint">{outputDimensionsSummary}</div>
              </div>
              <div className="vfl-field vfl-subtitle-field">
                <div className="vfl-field-label">External subtitles</div>
                <div className="vfl-control-row">
                  <button
                    ref={subtitleBrowseButtonRef}
                    type="button"
                    onClick={() => void pickExternalSubtitle()}
                    disabled={encodeBusy || !inputPath || subtitleInspecting || Boolean(subtitlePickerBlockingReason)}
                    aria-busy={subtitleInspecting}
                    aria-describedby="vfl-subtitle-status"
                    title={subtitlePickerBlockingReason ?? "Select one validated SubRip subtitle file"}
                  >
                    {subtitleInspecting ? "Validating…" : subtitlePath ? "Replace SRT…" : "Choose SRT…"}
                  </button>
                  {subtitlePath ? (
                    <button type="button" onClick={removeExternalSubtitle} disabled={encodeBusy || subtitleInspecting}>
                      Remove subtitles
                    </button>
                  ) : null}
                </div>
                <div
                  id="vfl-subtitle-status"
                  className="vfl-inline-hint vfl-live-line"
                >
                  {subtitleStatus}
                </div>
                {subtitleError || externalSubtitleBlockingReason ? (
                  <div className="vfl-error" role="alert">
                    {subtitleError ?? externalSubtitleBlockingReason}
                  </div>
                ) : null}
                <div className="vfl-inline-hint">
                  One UTF-8 .srt file can be burned into MP4 or WebM. Cues use source timing and fixed bottom-centered white text with a black outline. Inline HTML or ASS styling tags are rejected. Selecting subtitles forces video re-encoding.
                </div>
              </div>
              <div className="vfl-field">
                <label htmlFor="vfl-title-metadata">Title metadata</label>
                <input id="vfl-title-metadata" value={title} onChange={(e) => setTitle(e.currentTarget.value)} disabled={encodeBusy} />
              </div>
              <div className="vfl-field">
                <div className="vfl-field-label">Privacy</div>
                <label className="vfl-check vfl-check-card">
                  <input
                    type="checkbox"
                    checked={stripMetadata}
                    onChange={(e) => setStripMetadata(e.currentTarget.checked)}
                    disabled={encodeBusy}
                  />
                  <span>Strip location metadata</span>
                </label>
                <div className="vfl-inline-hint">
                  {stripMetadata
                    ? "GPS and capture metadata from the source are removed from exports."
                    : "Source metadata, including GPS location when present, is copied into exports."}
                </div>
              </div>
              <div className="vfl-field">
                <div className="vfl-field-label">Sharing</div>
                <label className="vfl-check vfl-check-card">
                  <input
                    type="checkbox"
                    checked={format === "mp3" ? false : perturbFirstFrame}
                    onChange={(e) => setPerturbFirstFrame(e.currentTarget.checked)}
                    disabled={encodeBusy || format === "mp3"}
                  />
                  <span>Make each export unique</span>
                </label>
                <div className="vfl-inline-hint">
                  {format === "mp3"
                    ? "Only applies to video exports."
                    : "Imperceptibly alters the first frame so each export has a different hash. Helps when a forum blocks re-uploaded duplicates. Does not defeat content fingerprinting."}
                </div>
              </div>
            </div>
          </RailCard>

          <RailCard
            title="Crop"
            summary={cropEnabled && cropSummary ? cropSummary : "Off"}
            open={openCards.crop}
            onToggle={() => toggleCard("crop")}
          >
            <label className="vfl-check">
              <input
                type="checkbox"
                checked={cropEnabled}
                onChange={(e) => setCropEnabled(e.currentTarget.checked)}
                disabled={encodeBusy || !probe}
              />
              Enable crop
            </label>
            <div className="vfl-row2 vfl-row2-compact">
              <label className="vfl-check">
                <input
                  type="checkbox"
                  checked={aspectLocked}
                  onChange={(e) => setAspectLocked(e.currentTarget.checked)}
                  disabled={encodeBusy || !cropEnabled}
                />
                Lock aspect
              </label>
              <div className="vfl-field">
                <label htmlFor="vfl-aspect-preset">Aspect preset</label>
                <select
                  id="vfl-aspect-preset"
                  value={aspectPreset}
                  onChange={(e) => setAspectPreset(e.currentTarget.value as typeof aspectPreset)}
                  disabled={encodeBusy || !cropEnabled || !aspectLocked}
                >
                  <option value="free">Free</option>
                  <option value="1:1">1:1</option>
                  <option value="16:9">16:9</option>
                  <option value="9:16">9:16</option>
                  <option value="4:3">4:3</option>
                  <option value="3:4">3:4</option>
                </select>
              </div>
            </div>
            {probe ? (
              <CropPixelFields
                frameWidth={probe.width}
                frameHeight={probe.height}
                rect={cropRect}
                onChange={(next) => {
                  setCropRect(next);
                  setCropDetectHint(null);
                }}
                disabled={encodeBusy || !cropEnabled}
                aspectRatio={aspect.locked ? aspect.ratio : null}
              />
            ) : null}
            <div className="vfl-actions vfl-actions-wrap">
              <button
                onClick={autoDetectCrop}
                disabled={encodeBusy || !probe || !inputPath || cropDetecting || Boolean(sourceRotationReason ?? cropDetectCapabilityBlockingReason)}
                title={sourceRotationReason ?? cropDetectCapabilityBlockingReason ?? "Detect black bars and suggest a crop"}
                aria-busy={cropDetecting}
                aria-describedby="vfl-crop-detect-status"
              >
                {cropDetecting ? "Detecting…" : "Auto detect crop"}
              </button>
              <button
                onClick={() => {
                  setCropRect({ x: 0, y: 0, w: 1, h: 1 });
                  setCropDetectHint(null);
                }}
                disabled={encodeBusy || !probe}
                title="Reset crop selection"
              >
                Reset crop
              </button>
            </div>
            <div className="vfl-inline-hint">
              {cropEnabled ? "Drag directly on the preview to adjust the crop box." : "Enable crop to draw directly on the preview."}
            </div>
            <div
              className="vfl-inline-hint vfl-live-line"
              id="vfl-crop-detect-status"
              role="status"
              aria-live="polite"
              aria-atomic="true"
            >
              {cropDetecting
                ? "Detecting crop…"
                : cropDetectHint ?? sourceRotationReason ?? cropDetectCapabilityBlockingReason ?? ""}
            </div>
            {cropEnabled && cropSummary ? <div className="vfl-inline-hint">Current crop: {cropSummary}</div> : null}
          </RailCard>

          <RailCard
            title="Transform & color"
            summary={activeEditChips.filter((chip) => chip !== "Trim" && chip !== "Crop").length ? "Edits active" : "Defaults"}
            open={openCards.transform}
            onToggle={() => toggleCard("transform")}
          >
                  <div className="vfl-subsection-title">Transform</div>
                  <div className="vfl-stack-md">
                    <div className="vfl-row2">
                      <div className="vfl-field">
                        <label htmlFor="vfl-speed">Speed</label>
                        <input id="vfl-speed" value={speed} onChange={(e) => setSpeed(e.currentTarget.value)} disabled={encodeBusy} />
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-rotate">Rotate</label>
                        <select
                          id="vfl-rotate"
                          value={rotateDeg}
                          onChange={(e) => setRotateDeg(Number(e.currentTarget.value))}
                          disabled={encodeBusy}
                        >
                          <option value={0}>0°</option>
                          <option value={90}>90° clockwise</option>
                          <option value={180}>180°</option>
                          <option value={270}>270° clockwise</option>
                        </select>
                      </div>
                    </div>
                    <label className="vfl-check">
                      <input
                        type="checkbox"
                        checked={reverse}
                        onChange={(e) => setReverse(e.currentTarget.checked)}
                        disabled={encodeBusy}
                      />
                      Reverse (video + audio)
                    </label>
                    <label className="vfl-check">
                      <input
                        type="checkbox"
                        checked={format === "mp3" ? false : loopVideo}
                        onChange={(e) => setLoopVideo(e.currentTarget.checked)}
                        disabled={encodeBusy || format === "mp3"}
                      />
                      Loop (boomerang)
                    </label>
                    <div className="vfl-inline-hint">
                      {format === "mp3"
                        ? "Loop only applies to video exports."
                        : "Plays the clip forward then in reverse so it loops seamlessly. Doubles the length."}
                    </div>
                    {reverse || (format !== "mp3" && loopVideo) ? (
                      <div
                        className={`vfl-memory-status ${transformMemoryEstimate.severity}`}
                        role="status"
                        aria-live="polite"
                        aria-atomic="true"
                      >
                        <strong>Decoded buffer</strong>
                        <span>
                          {transformMemoryEstimate.bytes === null
                            ? transformMemoryEstimate.reason
                            : `${formatByteSize(transformMemoryEstimate.bytes)} estimated${
                                transformMemoryEstimate.severity === "blocked"
                                  ? " • above the 2 GiB safety limit"
                                  : transformMemoryEstimate.severity === "warning"
                                    ? " • memory warning"
                                    : " • within the safety limit"
                              }`}
                        </span>
                      </div>
                    ) : null}
                  </div>

                  <div className="vfl-subsection-title">Color</div>
                  <div className="vfl-stack-md">
                    {probe && format !== "mp3" && colorSource.kind !== "standard" ? (
                      <div className={`vfl-color-policy ${colorSource.kind === "unsupported" ? "blocked" : ""}`}>
                        <div className="vfl-field-label">Source color handling</div>
                        {colorSource.kind === "convertible" ? (
                          <>
                            <label className="vfl-check vfl-check-card">
                              <input
                                type="checkbox"
                                checked={colorPolicy === "standardSdr"}
                                onChange={(event) => setColorPolicy(event.currentTarget.checked ? "standardSdr" : "auto")}
                                disabled={encodeBusy}
                              />
                              <span>Convert {colorSource.label} to standard SDR for sharing</span>
                            </label>
                            <div className="vfl-inline-hint">
                              Creates an 8-bit BT.709 MP4 for broad playback. HDR preservation is not supported in this version.
                            </div>
                          </>
                        ) : (
                          <div className="vfl-error" role="alert">{colorSource.reason} Audio-only MP3 export remains available.</div>
                        )}
                      </div>
                    ) : null}
                    <div className="vfl-row2">
                      <div className="vfl-field vfl-slider-field">
                        <div className="vfl-slider-top">
                          <label htmlFor="vfl-brightness">Brightness</label>
                          <div className="vfl-slider-value">{Number(brightness).toFixed(2)}</div>
                        </div>
                        <input
                          id="vfl-brightness"
                          type="range"
                          min={-1}
                          max={1}
                          step={0.01}
                          value={brightness}
                          style={rangeFillStyle(Number(brightness), -1, 1)}
                          onChange={(e) => setBrightness(e.currentTarget.value)}
                          disabled={encodeBusy}
                        />
                        <div className="vfl-inline-hint">Lift shadows or darken the whole frame.</div>
                      </div>
                      <div className="vfl-field vfl-slider-field">
                        <div className="vfl-slider-top">
                          <label htmlFor="vfl-contrast">Contrast</label>
                          <div className="vfl-slider-value">{Number(contrast).toFixed(2)}</div>
                        </div>
                        <input
                          id="vfl-contrast"
                          type="range"
                          min={0}
                          max={2}
                          step={0.01}
                          value={contrast}
                          style={rangeFillStyle(Number(contrast), 0, 2)}
                          onChange={(e) => setContrast(e.currentTarget.value)}
                          disabled={encodeBusy}
                        />
                        <div className="vfl-inline-hint">Increase separation between dark and bright areas.</div>
                      </div>
                    </div>
                    <div className="vfl-field vfl-slider-field">
                      <div className="vfl-slider-top">
                        <label htmlFor="vfl-saturation">Saturation</label>
                        <div className="vfl-slider-value">{Number(saturation).toFixed(2)}</div>
                      </div>
                      <input
                        id="vfl-saturation"
                        type="range"
                        min={0}
                        max={3}
                        step={0.01}
                        value={saturation}
                        style={rangeFillStyle(Number(saturation), 0, 3)}
                        onChange={(e) => setSaturation(e.currentTarget.value)}
                        disabled={encodeBusy}
                      />
                      <div className="vfl-inline-hint">Boost or soften overall color intensity.</div>
                    </div>
                    <div className="vfl-actions">
                      <button
                        onClick={() => {
                          setBrightness("0");
                          setContrast("1");
                          setSaturation("1");
                          setColorPolicy("auto");
                        }}
                        disabled={encodeBusy}
                      >
                        Reset color
                      </button>
                    </div>
                  </div>
          </RailCard>

          <RailCard
            title="Advanced"
            summary={advancedOverrideCount > 0 ? `${advancedOverrideCount} override${advancedOverrideCount === 1 ? "" : "s"}` : "Auto"}
            open={openCards.advanced}
            onToggle={() => toggleCard("advanced")}
          >
                  <div className="vfl-muted vfl-section-caption">
                    Auto keeps the current export planner in charge; overrides apply only when a matching encoder is available.
                  </div>
                  <div className="vfl-stack-md">
                    <div className="vfl-row2">
                      <div className="vfl-field">
                        <label htmlFor="vfl-video-codec">Video codec</label>
                        <select
                          id="vfl-video-codec"
                          value={advancedVideoCodec}
                          onChange={(e) => setAdvancedVideoCodec(e.currentTarget.value as VideoCodecPreference)}
                          disabled={encodeBusy || format === "mp3"}
                        >
                          <option value="auto">
                            Auto
                            {videoCodecOptions.find((codec) => codec.isDefault)?.label
                              ? ` (${videoCodecOptions.find((codec) => codec.isDefault)?.label})`
                              : ""}
                          </option>
                          {videoCodecOptions.map((codec) => (
                            <option key={`${codec.format}-${codec.value}`} value={codec.value} disabled={!codec.available}>
                              {codec.label} ({codec.ffmpegName}){codec.available ? "" : " unavailable"}
                            </option>
                          ))}
                        </select>
                        <div className="vfl-inline-hint">
                          {format === "mp3"
                            ? "MP3 exports are audio-only."
                            : encodeCapabilities
                              ? `${videoCodecOptions.filter((codec) => codec.available).length} available for ${format.toUpperCase()}.`
                              : encodeCapabilitiesError
                                ? "Capability check failed."
                                : "Capability check runs when this tab opens."}
                        </div>
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-audio-bitrate">Audio bitrate</label>
                        <select
                          id="vfl-audio-bitrate"
                          value={advancedAudioBitrateKbps}
                          onChange={(e) => setAdvancedAudioBitrateKbps(e.currentTarget.value)}
                          disabled={encodeBusy || sizeLimitEnabled || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
                        >
                          <option value="auto">Auto</option>
                          {AUDIO_BITRATE_PRESETS_KBPS.map((kbps) => (
                            <option key={kbps} value={kbps}>
                              {kbps} kbps
                            </option>
                          ))}
                        </select>
                        <div className="vfl-inline-hint">
                          {sizeLimitEnabled
                            ? "Size-targeted exports plan audio bitrate automatically."
                            : advancedAudioApplies
                              ? `${advancedAudioBitrateRequest} kbps will be requested.`
                              : "Auto uses the current format default."}
                        </div>
                      </div>
                    </div>

                    <div className="vfl-row2">
                      <div className="vfl-field">
                        <label htmlFor="vfl-video-quality">Quality</label>
                        <select
                          id="vfl-video-quality"
                          value={advancedVideoQuality}
                          onChange={(e) => setAdvancedVideoQuality(e.currentTarget.value as VideoQualityPreference)}
                          disabled={encodeBusy || format === "mp3"}
                        >
                          <option value="auto">Auto</option>
                          <option value="smaller">Smaller file</option>
                          <option value="balanced">Balanced</option>
                          <option value="higher">Higher quality</option>
                        </select>
                        <div className="vfl-inline-hint">
                          {format === "mp3"
                            ? "MP3 exports are audio-only."
                            : sizeLimitEnabled
                              ? "Held while a size target is active."
                              : advancedVideoQualityApplies
                                ? `${VIDEO_QUALITY_LABELS[advancedVideoQuality]} will be requested.`
                                : "Auto uses the current codec default."}
                        </div>
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-encode-speed">Encode speed</label>
                        <select
                          id="vfl-encode-speed"
                          value={advancedEncodeSpeed}
                          onChange={(e) => setAdvancedEncodeSpeed(e.currentTarget.value as EncodeSpeedPreference)}
                          disabled={encodeBusy || format === "mp3"}
                        >
                          <option value="auto">Auto</option>
                          <option value="faster">Faster</option>
                          <option value="balanced">Balanced</option>
                          <option value="smaller">Smaller file</option>
                        </select>
                        <div className="vfl-inline-hint">
                          {format === "mp3"
                            ? "MP3 exports are audio-only."
                            : advancedEncodeSpeedApplies
                              ? `${ENCODE_SPEED_LABELS[advancedEncodeSpeed]} preset for re-encode paths.`
                              : "Auto keeps the codec default speed."}
                        </div>
                      </div>
                    </div>

                    <div className="vfl-row2">
                      <div className="vfl-field">
                        <label htmlFor="vfl-frame-rate-cap">Frame-rate cap</label>
                        <select
                          id="vfl-frame-rate-cap"
                          value={advancedFrameRateCapFps}
                          onChange={(e) => setAdvancedFrameRateCapFps(e.currentTarget.value)}
                          disabled={encodeBusy || format === "mp3"}
                        >
                          <option value="auto">Auto</option>
                          {FRAME_RATE_CAP_PRESETS_FPS.map((fps) => (
                            <option key={fps} value={fps}>
                              {fps} fps
                            </option>
                          ))}
                        </select>
                        <div className="vfl-inline-hint">
                          {format === "mp3"
                            ? "MP3 exports are audio-only."
                            : advancedFrameRateCapRequest === null
                              ? "Auto keeps the source frame rate."
                              : frameRateCapApplies
                                ? frameRatePlan.postSpeedFps !== null
                                  ? `Effective post-speed rate is about ${frameRatePlan.postSpeedFps.toFixed(1)} fps; frames above ${advancedFrameRateCapRequest} fps will be reduced.`
                                  : `Frames above ${advancedFrameRateCapRequest} fps will be reduced.`
                                : "Source is already at or below the cap."}
                        </div>
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-audio-channels">Audio channels</label>
                        <select
                          id="vfl-audio-channels"
                          value={advancedAudioChannels}
                          onChange={(e) => setAdvancedAudioChannels(e.currentTarget.value as AudioChannelPreference)}
                          disabled={encodeBusy || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
                        >
                          <option value="auto">Auto</option>
                          <option value="stereo">Stereo</option>
                          <option value="mono">Mono</option>
                        </select>
                        <div className="vfl-inline-hint">
                          {advancedAudioChannels === "auto"
                            ? "Auto keeps the encoder default channel layout."
                            : advancedAudioChannelsApplies
                              ? `${AUDIO_CHANNEL_LABELS[advancedAudioChannels]} output will be requested.`
                              : "Waiting for an export with included audio."}
                        </div>
                      </div>
                    </div>

                    <div className="vfl-row2">
                      <div className="vfl-field">
                        <div className="vfl-field-label">Audio cleanup</div>
                        <label className="vfl-check vfl-check-card">
                          <input
                            type="checkbox"
                            checked={normalizeAudio}
                            onChange={(e) => setNormalizeAudio(e.currentTarget.checked)}
                            disabled={encodeBusy || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
                          />
                          <span>Normalize speech</span>
                        </label>
                        <div className="vfl-inline-hint">
                          {normalizeAudio
                            ? normalizeAudioApplies
                              ? "Evens out quiet or uneven voice loudness. Forces a re-encode."
                              : "Waiting for an export with included audio."
                            : "Evens out quiet or uneven voice loudness in screen recordings."}
                        </div>
                      </div>
                    </div>

                    <div className="vfl-field">
                      <div className="vfl-field-label">Sample preview</div>
                      <div className="vfl-control-row">
                        <div className="vfl-segmented" role="group" aria-label="Sample duration">
                          {SAMPLE_DURATION_CHOICES_S.map((seconds) => (
                            <button
                              key={seconds}
                              type="button"
                              className={sampleDurationS === seconds ? "active" : ""}
                              onClick={() => setSampleDurationS(seconds)}
                              disabled={encodeBusy}
                              aria-pressed={sampleDurationS === seconds}
                            >
                              {seconds} s
                            </button>
                          ))}
                        </div>
                        <button
                          type="button"
                          onClick={() => void exportSample()}
                          disabled={!sampleExportReady || encodeBusy}
                        >
                          Export exact sample
                        </button>
                      </div>
                      <div className="vfl-inline-hint">
                        {sampleEstimate
                          ? `Sample ${formatByteSize(sampleEstimate.sampleBytes)}${
                              sampleEstimate.estimateBytes ? ` -> full export ~ ${formatByteSize(sampleEstimate.estimateBytes)}` : ""
                            }`
                          : "Encodes a short slice around the preview position with exact trim boundaries."}
                      </div>
                    </div>

                    <div className="vfl-actions vfl-actions-secondary">
                      <button
                        type="button"
                        onClick={() => {
                          setAdvancedVideoCodec("auto");
                          setAdvancedAudioBitrateKbps("auto");
                          setAdvancedVideoQuality("auto");
                          setAdvancedEncodeSpeed("auto");
                          setAdvancedFrameRateCapFps("auto");
                          setAdvancedAudioChannels("auto");
                          setNormalizeAudio(false);
                        }}
                        disabled={encodeBusy || advancedOverrideCount === 0}
                      >
                        Reset advanced
                      </button>
                      {encodeCapabilitiesError ? (
                        <button
                          type="button"
                          onClick={() => {
                            setEncodeCapabilities(null);
                            setEncodeCapabilitiesError(null);
                          }}
                          disabled={encodeBusy}
                        >
                          Retry capability check
                        </button>
                      ) : null}
                    </div>

                    {encodeCapabilitiesError ? <div className="vfl-error" role="alert">{encodeCapabilitiesError}</div> : null}
                  </div>

                  <div className="vfl-subsection-title">Advanced plan</div>
                  <div className={`vfl-plan-hero ${advancedOverrideCount > 0 ? "is-ready" : ""}`}>
                    <div className="vfl-plan-hero-kicker">{advancedOverrideCount > 0 ? "Overrides active" : "Auto mode"}</div>
                    <div className="vfl-plan-hero-copy">{advancedPlanSummary}</div>
                  </div>
                  <div className="vfl-summary-list">
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Encode mode</div>
                      <div className="vfl-summary-value">{encodeModeSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Format</div>
                      <div className="vfl-summary-value">{format.toUpperCase()}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Video codec</div>
                      <div className="vfl-summary-value">{advancedCodecSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Quality</div>
                      <div className="vfl-summary-value">{advancedQualitySummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Encode speed</div>
                      <div className="vfl-summary-value">{advancedSpeedSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Frame rate</div>
                      <div className="vfl-summary-value">{advancedFrameRateSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Audio bitrate</div>
                      <div className="vfl-summary-value">{advancedAudioSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Audio channels</div>
                      <div className="vfl-summary-value">{advancedAudioChannelsSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Size target</div>
                      <div className="vfl-summary-value">{sizeLimitEnabled ? "Bitrate planned from target size" : "Quality mode with no size cap"}</div>
                    </div>
                  </div>
                  {planWarnings.length ? (
                    <div className="vfl-plan-warnings">
                      {planWarnings.map((warning) => (
                        <div key={warning} className="vfl-plan-warning">
                          {warning}
                        </div>
                      ))}
                    </div>
                  ) : null}
          </RailCard>

          <RailCard
            title="Current plan"
            summary={attemptUi.summary ?? (exportReady ? "Ready" : "Waiting")}
            open={openCards.plan}
            onToggle={() => toggleCard("plan")}
          >
                  <div className={`vfl-plan-hero ${planHeroReady ? "is-ready" : ""} ${attemptUi.isActive ? "is-busy" : ""} ${attemptUi.isSuccess ? "is-done" : ""} ${attemptUi.isTargetMissed ? "is-target-missed" : ""} ${attemptUi.isFailure ? "is-failed" : ""} ${attemptUi.isCancelled ? "is-cancelled" : ""}`}>
                    <div className="vfl-plan-hero-kicker">{footerKicker}</div>
                    <div className="vfl-plan-hero-copy">{planStatusText}</div>
                  </div>
                  <div className="vfl-summary-list">
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Source</div>
                      <div className="vfl-summary-value">{inputSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Source streams</div>
                      <div className="vfl-summary-value">{sourceStreamSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Output</div>
                      <div className="vfl-summary-value">{outputPath || outputModeSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Dimensions</div>
                      <div className="vfl-summary-value">{outputDimensionsSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Color handling</div>
                      <div className="vfl-summary-value">{colorHandlingSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Display handling</div>
                      <div className="vfl-summary-value">{displayHandlingSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Transform memory</div>
                      <div className="vfl-summary-value">{transformMemorySummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Planned</div>
                      <div className="vfl-summary-value">{planSummaryText ?? "Pick a valid input and settings to calculate the plan."}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Trim</div>
                      <div className="vfl-summary-value">{trimAccuracySummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Recipe</div>
                      <div className="vfl-summary-value">{matchingRecipeLabel ?? "Custom settings"}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Advanced</div>
                      <div className="vfl-summary-value">{advancedPlanSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Strict Fit</div>
                      <div className="vfl-summary-value">{strictFitPolicySummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Subtitles</div>
                      <div className="vfl-summary-value">{subtitlePlanSummary}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Active edits</div>
                      {activeEditChips.length ? (
                        <div className="vfl-chips vfl-chips-compact">
                          {activeEditChips.map((chip) => (
                            <div key={chip} className="vfl-chip">
                              {chip}
                            </div>
                          ))}
                        </div>
                      ) : (
                        <div className="vfl-summary-value">No trim, crop, transform, or color adjustments yet.</div>
                      )}
                    </div>
                  </div>
                  {planWarnings.length ? (
                    <div className="vfl-plan-warnings" role="status" aria-live="polite">
                      {planWarnings.map((warning) => (
                        <div key={warning} className="vfl-plan-warning">
                          {warning}
                        </div>
                      ))}
                    </div>
                  ) : null}
                  {lastExport ? (
                    <div className={`vfl-export-result ${lastTargetData?.missed ? "target-missed" : ""}`}>
                      <div className="vfl-export-result-head">
                        <div>
                          <div className="vfl-export-result-kicker">
                            {lastExportIsCurrentOutcome
                              ? "Last measured export"
                              : lastExport.targetResult?.status === "missed"
                                ? "Previous measured target-miss artifact"
                                : "Previous successful export"}
                          </div>
                          <div className="vfl-export-result-title">
                            {lastTargetData?.missed ? "Target missed" : lastTargetData?.met ? "Target met" : "Done"}
                            {lastExportSizeText ? ` (${lastExportSizeText})` : ""}
                          </div>
                        </div>
                        <div className="vfl-chip active">{lastExport.format.toUpperCase()}</div>
                      </div>
                      <div className="vfl-export-result-meta">
                        {lastExportDurationText ? `${lastExportDurationText} • ` : ""}
                        {new Date(lastExport.completedAtMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                      </div>
                      {lastExport.message ? <div className="vfl-export-result-note">{lastExport.message}</div> : null}
                      <div className="vfl-export-result-path">{formatPathForDisplay(lastExport.outputPath)}</div>
                      {lastExport.targetResult ? <TargetResultDetails targetResult={lastExport.targetResult} /> : null}
                      {lastExport.diagnostics ? (
                        <details className="vfl-export-diagnostics">
                          <summary>Export details</summary>
                          <div className="vfl-diagnostic-grid">
                            <div>
                              <span>Mode</span>
                              <strong>{lastExport.diagnostics.mode}</strong>
                            </div>
                            <div>
                              <span>Video result</span>
                              <strong>
                                {lastExport.diagnostics.videoCodec ?? "none"} ({formatStreamAction(lastExport.diagnostics.videoAction)})
                              </strong>
                            </div>
                            <div>
                              <span>Audio result</span>
                              <strong>
                                {lastExport.diagnostics.audioCodec ?? "none"} ({formatStreamAction(lastExport.diagnostics.audioAction)})
                              </strong>
                            </div>
                            <div>
                              <span>Source media</span>
                              <strong>
                                {[lastExport.diagnostics.sourceFormat, lastExport.diagnostics.sourceVideoCodec, lastExport.diagnostics.sourceAudioCodec]
                                  .filter(Boolean)
                                  .join(" • ") || "unknown"}
                              </strong>
                            </div>
                            <div>
                              <span>Passes</span>
                              <strong>
                                {lastExport.diagnostics.passes}
                                {lastExport.diagnostics.attempts > 1 ? `, ${lastExport.diagnostics.attempts} attempts` : ""}
                              </strong>
                            </div>
                            <div>
                              <span>Video bitrate</span>
                              <strong>{lastExport.diagnostics.videoBitrateKbps ? `${lastExport.diagnostics.videoBitrateKbps} kbps` : "auto"}</strong>
                            </div>
                            <div>
                              <span>Audio bitrate</span>
                              <strong>{lastExport.diagnostics.audioBitrateKbps ? `${lastExport.diagnostics.audioBitrateKbps} kbps` : "auto"}</strong>
                            </div>
                            <div>
                              <span>Target</span>
                              <strong>{formatByteSize(lastExport.diagnostics.requestedSizeBytes)}</strong>
                            </div>
                            <div>
                              <span>Actual</span>
                              <strong>{formatByteSize(lastExport.diagnostics.actualSizeBytes)}</strong>
                            </div>
                            {lastExport.diagnostics.colorAction ? (
                              <div>
                                <span>Color</span>
                                <strong>{lastExport.diagnostics.colorAction}</strong>
                              </div>
                            ) : null}
                            {lastExport.diagnostics.sarAction ? (
                              <div>
                                <span>Display pixels</span>
                                <strong>{lastExport.diagnostics.sarAction}</strong>
                              </div>
                            ) : null}
                            {lastExport.diagnostics.reverseBufferEstimateBytes ? (
                              <div>
                                <span>Transform buffer</span>
                                <strong>
                                  {formatByteSize(lastExport.diagnostics.reverseBufferEstimateBytes)}
                                  {lastExport.diagnostics.reverseBufferAction
                                    ? ` (${lastExport.diagnostics.reverseBufferAction})`
                                    : ""}
                                </strong>
                              </div>
                            ) : null}
                            {lastExport.diagnostics.subtitleBurnedIn ? (
                              <div>
                                <span>External subtitles</span>
                                <strong>
                                  Burned in{lastExport.diagnostics.subtitleCueCount ? `, ${lastExport.diagnostics.subtitleCueCount} cues` : ""}
                                </strong>
                              </div>
                            ) : null}
                          </div>
                          {lastExport.diagnostics.copyFallbackReason ? (
                            <div className="vfl-export-result-note">
                              Copy fallback: {lastExport.diagnostics.copyFallbackReason}
                            </div>
                          ) : null}
                          <pre className="vfl-command-preview">{lastExport.diagnostics.commandPreview}</pre>
                        </details>
                      ) : null}
                      {lastTargetData?.missed ? (
                        <section className="vfl-corrective-actions" aria-labelledby="vfl-corrective-actions-title">
                          <div id="vfl-corrective-actions-title" className="vfl-field-label">Adjust settings</div>
                          <div className="vfl-inline-hint">
                            {correctiveActionsMatchCurrentPlan
                              ? "These actions update controls only. No export starts until you choose Export."
                              : "Apply the matching queue snapshot or restore this source, format, and exact target before using these controls. No export starts automatically."}
                          </div>
                          <div className="vfl-actions vfl-actions-wrap" role="group" aria-label="Target miss corrective actions">
                            {lastCorrectiveActions.length ? lastCorrectiveActions.map((action) => {
                              const applied = appliedCorrectiveKinds.includes(action.kind);
                              return (
                                <button
                                  key={action.kind}
                                  type="button"
                                  onClick={() => applyStrictFitCorrectiveAction(action)}
                                  disabled={encodeBusy || applied || !correctiveActionsMatchCurrentPlan}
                                >
                                  {applied ? `${action.label} applied` : action.label}
                                </button>
                              );
                            }) : <span className="vfl-inline-hint">No additional automatic corrections apply to this plan.</span>}
                          </div>
                        </section>
                      ) : null}
                      <div className="vfl-actions vfl-actions-secondary">
                        <button onClick={() => void openOutputFile(lastExport.outputPath)}>Open file</button>
                        <button onClick={() => void openFolderFor(lastExport.outputPath)}>Open folder</button>
                      </div>
                    </div>
                  ) : null}
                  <div className="vfl-plan-actions">
                    <div className="vfl-actions vfl-actions-secondary">
                      <button
                        data-smoke-id="reset-all-settings"
                        onClick={() => setResetConfirmationOpen(true)}
                        disabled={encodeBusy || updateBusy}
                      >
                        Reset all settings
                      </button>
                    </div>
                  </div>
          </RailCard>

          <RailCard
            title="Queue"
            summary={
              exportQueue.length
                ? activeQueuePosition
                  ? `Item ${activeQueuePosition} of ${exportQueue.length}`
                  : `${queueCounts.queued} queued`
                : "Empty"
            }
            open={openCards.queue}
            onToggle={() => toggleCard("queue")}
          >
            <div className="vfl-muted vfl-section-caption">
              Add exports as snapshots and run them one after another with no parallel FFmpeg jobs.
            </div>
            <div
              ref={queueRegionRef}
              className="vfl-actions vfl-actions-wrap vfl-queue-actions"
              role="group"
              tabIndex={-1}
              aria-label="Export queue controls"
            >
              <button type="button" onClick={() => void addCurrentPlanToQueue()} disabled={!exportReady || encodeBusy}>
                Add current plan
              </button>
              <button ref={queueFallbackButtonRef} type="button" onClick={() => void addFilesToQueue()} disabled={exportQueueRemainingCapacity(exportQueueState) === 0}>
                Add files
              </button>
              <button
                ref={queueRunButtonRef}
                data-smoke-id="run-export-queue"
                type="button"
                className="primary"
                onClick={runQueue}
                disabled={!encodeEventsReady || encodeBusy || queueRunning || queueCounts.queued === 0}
                title={encodeEventBlockingReason ?? undefined}
                aria-describedby={encodeEventsError ? "vfl-encode-event-setup-error" : undefined}
              >
                Run queue
              </button>
              <button ref={queueStopButtonRef} type="button" onClick={stopQueueAfterCurrent} disabled={!queueRunning}>
                Stop after current
              </button>
              <button type="button" onClick={clearCompletedQueueItems} disabled={queueCounts.done + queueCounts.missed + queueCounts.failed + queueCounts.cancelled === 0}>
                Clear finished
              </button>
            </div>
            <div className="vfl-queue-counts" aria-live="polite">
              <span>{queueCounts.queued} queued</span>
              <span>{queueCounts.running} running</span>
              <span>{queueCounts.done} done</span>
              <span>{queueCounts.missed} target missed</span>
              <span>{queueCounts.failed} failed</span>
              <span>{queueCounts.cancelled} canceled</span>
            </div>
            {exportQueue.length ? (
              <div className="vfl-queue-list" role="list" aria-label="Export queue items">
                {exportQueue.map((item) => {
                  const outcome = item.lastOutcome;
                  const outcomeHasArtifact = outcome?.kind === "done" || outcome?.kind === "target-missed";
                  const doneOutputPath = outcome?.kind === "done" ? outcome.outputPath : null;
                  const actualOutputPath = doneOutputPath ??
                    (outcome?.kind === "target-missed" ? outcome.outputPath : null);
                  const attemptedOutputPath = outcome && !outcomeHasArtifact ? outcome.outputPath : null;
                  const outcomeIsFailure = outcome?.kind === "failed" || outcome?.kind === "cancelled";
                  const queuedTrimSummary = item.request.trim
                    ? `Trim • requested ${formatClock(item.request.trim.startS)} to ${item.request.trim.endS == null ? "end" : formatClock(item.request.trim.endS)}`
                    : "No trim";
                  return (
                  <div
                    key={item.id}
                    role="listitem"
                    aria-label={`${basename(item.inputPath)}, ${QUEUE_STATUS_LABELS[item.status]}`}
                    className={`vfl-queue-item ${item.status} ${queueActiveItemId === item.id ? "active" : ""}`}
                    data-queue-item-id={item.id}
                  >
                    <div className="vfl-queue-item-main">
                      <div className="vfl-queue-item-title">{basename(item.inputPath)}</div>
                      <div className="vfl-queue-item-meta">
                        {item.format.toUpperCase()} {"->"} {basename(item.outputPath)}
                        {outcome?.outputSizeBytes ? ` • ${formatByteSize(outcome.outputSizeBytes)}` : ""}
                      </div>
                      <div className="vfl-queue-item-meta">{queuedTrimSummary}</div>
                      {actualOutputPath && actualOutputPath !== item.outputPath ? (
                        <div className="vfl-queue-item-meta">Actual result: {actualOutputPath}</div>
                      ) : null}
                      {attemptedOutputPath ? (
                        <div className="vfl-queue-item-meta">
                          {item.status === "queued" ? "Previous attempted output" : "Attempted output"}: {attemptedOutputPath}
                        </div>
                      ) : null}
                      {outcome?.message ? (
                        <div
                          className="vfl-queue-item-message"
                          role={outcomeIsFailure ? "alert" : undefined}
                          aria-live={outcomeIsFailure ? "assertive" : undefined}
                          aria-atomic={outcomeIsFailure ? "true" : undefined}
                        >
                          {item.status === "queued" ? "Previous attempt: " : ""}{outcome.message}
                        </div>
                      ) : null}
                      {outcome?.targetResult ? <TargetResultDetails targetResult={outcome.targetResult} /> : null}
                      {outcome ? <QueueDiagnosticsDetails outcome={outcome} summary="Latest diagnostics" /> : null}
                      {item.history.length > 1 ? (
                        <details className="vfl-queue-history">
                          <summary>Recent attempt history ({item.history.length} retained)</summary>
                          <ol>
                            {item.history.map((attempt) => (
                              <li key={attempt.runId} data-queue-run-id={attempt.runId}>
                                <div className="vfl-queue-history-title">
                                  Run {attempt.runId}: {QUEUE_STATUS_LABELS[attempt.kind]}
                                </div>
                                {attempt.outputPath ? (
                                  <div className="vfl-queue-item-meta">
                                    {attempt.kind === "done" || attempt.kind === "target-missed" ? "Result" : "Attempted output"}: {attempt.outputPath}
                                  </div>
                                ) : null}
                                {attempt.message ? <div className="vfl-queue-item-message">{attempt.message}</div> : null}
                                {attempt.targetResult ? <TargetResultDetails targetResult={attempt.targetResult} /> : null}
                                <QueueDiagnosticsDetails outcome={attempt} summary={`Run ${attempt.runId} diagnostics`} />
                              </li>
                            ))}
                          </ol>
                        </details>
                      ) : null}
                    </div>
                    <div className="vfl-queue-item-actions">
                      <span className={`vfl-chip ${item.status === "running" || item.status === "done" ? "active" : ""} ${item.status === "target-missed" ? "missed" : ""}`}>
                        {QUEUE_STATUS_LABELS[item.status]}
                      </span>
                      {outcomeHasArtifact && actualOutputPath ? (
                        <button type="button" onClick={() => void openOutputFile(actualOutputPath)}>
                          {item.status === "queued" ? "Open previous file" : "Open file"}
                        </button>
                      ) : null}
                      {outcomeHasArtifact && actualOutputPath ? (
                        <button type="button" onClick={() => void openFolderFor(actualOutputPath)}>
                          Open folder
                        </button>
                      ) : null}
                      {item.status === "target-missed" || item.status === "failed" || item.status === "cancelled" ? (
                        <button type="button" data-queue-action="retry" onClick={() => void retryQueueItem(item.id)}>
                          Retry
                        </button>
                      ) : null}
                      <button
                        type="button"
                        data-queue-action="duplicate"
                        onClick={() => void duplicateQueueItem(item.id)}
                        disabled={item.status === "running" || exportQueueRemainingCapacity(exportQueueState) === 0}
                      >
                        Duplicate
                      </button>
                      <button
                        type="button"
                        data-queue-action="apply-snapshot"
                        onClick={() => void applyQueueItemSnapshot(item.id)}
                        disabled={encodeBusy || item.status === "running"}
                      >
                        Apply snapshot
                      </button>
                      <button data-queue-action="remove" type="button" onClick={() => removeQueueItem(item.id)} disabled={item.status === "running" || queueSnapshotApplying}>
                        Remove
                      </button>
                    </div>
                  </div>
                  );
                })}
              </div>
            ) : (
              <div className="vfl-inline-hint">
                Use Add files for a same-settings batch, or Add current plan to queue the loaded file with its current edits.
              </div>
            )}
          </RailCard>
        </aside>
      </div>

      <footer className={`vfl-footer ${attemptUi.isActive || queueRunning ? "is-active" : "is-idle"}`}>
        <div className="vfl-footer-main">
          <div className="vfl-footer-copy">
            <div className="vfl-footer-kicker">{footerKicker}</div>
            <div className="vfl-footer-status" role="status" aria-live="polite" aria-atomic="true">{displayedStatus}</div>
            {encodeEventsError ? (
              <div
                id="vfl-encode-event-setup-error"
                className="vfl-error"
                role="alert"
                aria-live="assertive"
                aria-atomic="true"
              >
                {encodeEventsError}
              </div>
            ) : null}
            <div className="vfl-footer-meta">{footerMetaText}</div>
          </div>
          <div className="vfl-footer-actions">
            {lastExport ? (
              <button key="open-export" onClick={() => void openOutputFile(lastExport.outputPath)} disabled={encodeBusy}>
                {lastExportIsCurrentOutcome ? "Open file" : "Open previous file"}
              </button>
            ) : null}
            <button key="open-output-folder" onClick={openOutputFolder} disabled={(!outputPath && !inputPath) || encodeBusy}>
              Open folder
            </button>
            {jobId !== null ? (
              <button
                key="cancel-export"
                className="danger vfl-cancel-export-button"
                data-smoke-id="cancel-export"
                onClick={() => void cancelEncode()}
                disabled={latestAttempt.kind === "cancelling"}
              >
                {latestAttempt.kind === "cancelling"
                  ? "Cancellation requested"
                  : exportQueueState.active !== null
                    ? "Cancel current and stop queue"
                    : "Cancel export"}
              </button>
            ) : null}
            <button
              key="export"
              className="primary vfl-export-button"
              data-smoke-id="export"
              onClick={() => void startEncode()}
              disabled={!exportReady || encodeBusy}
            >
              {latestAttempt.kind === "starting"
                ? "Starting…"
                : jobId !== null
                  ? latestAttempt.kind === "cancelling" ? "Cancelling…" : "Exporting…"
                  : "Export"}
            </button>
          </div>
        </div>
        {activeQueuePosition && exportQueue.length ? (
          <div className="vfl-footer-queue" aria-live="polite">
            Queue: item {activeQueuePosition} of {exportQueue.length}
          </div>
        ) : null}
        <div
          data-smoke-id="encode-progress"
          className={`vfl-progress ${jobId === null ? "is-placeholder" : ""}`}
          role={jobId !== null ? "progressbar" : undefined}
          aria-label={jobId !== null ? "Encoding progress" : undefined}
          aria-valuemin={jobId !== null ? 0 : undefined}
          aria-valuemax={jobId !== null ? 100 : undefined}
          aria-valuenow={jobId !== null ? progressUi.percent : undefined}
          aria-valuetext={jobId !== null ? progressUi.valueText : undefined}
          aria-hidden={jobId === null ? "true" : undefined}
        >
          <div className="vfl-progress-meta">
            <span>{progressUi.label}</span>
            <span>{progressUi.percent}%</span>
          </div>
          <div className="vfl-progress-bar">
            <div
              className={`vfl-progress-fill ${progressUi.isFinalizing ? "is-finalizing" : ""}`}
              style={{ width: `${progressUi.percent}%` }}
            />
          </div>
        </div>
      </footer>
    </div>
  );
}

export default App;
