import { useEffect, useEffectEvent, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm as confirmDialog, open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { openPath, openUrl } from "@tauri-apps/plugin-opener";
import { VideoCropper, type NormalizedRect, type VideoCropperHandle } from "./components/VideoCropper";
import type {
  AppSmokeConfig,
  AppSmokeStatus,
  AudioChannelPreference,
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
  UpdateApplyResponse,
  UpdateCheckResponse,
  VideoCodecPreference,
  VideoQualityPreference,
  VideoProbe,
} from "./lib/types";
import {
  bindWindowFileDrop,
  resolveDroppedVideoAction,
  SUPPORTED_INPUT_EXTENSIONS,
} from "./lib/dropInput";
import { DEFAULT_OUTPUT_FORMAT, DEFAULT_SIZE_LIMIT_MB } from "./lib/defaults";
import { basename, dirname, ensureUniqueOutputPath, extname, replaceExtension, stem, suggestOutputPath } from "./lib/outputPath";
import { getActiveProgressUi } from "./lib/progress";
import { parsePersistedSettings, serializePersistedSettings } from "./lib/settings";
import { EXPORT_RECIPES, findMatchingExportRecipe, normalizeRecipeResizeSettings, type ExportRecipe } from "./lib/exportRecipes";
import { buildPreviewColorFilter } from "./lib/previewFilter";
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
const SMOKE_SUCCESS_STAGE = "success";
const SMOKE_ERROR_STAGE = "error";
const SMOKE_STAGE_ORDER = ["detected", "input-applied", "probe-ready", "preview-ready", "interaction-ready", "encoding"] as const;
const APP_VERSION = "1.9.0";
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

type LastExportResult = {
  outputPath: string;
  outputSizeBytes: number | null;
  durationS: number | null;
  format: OutputFormat;
  message: string | null;
  diagnostics: ExportDiagnostics | null;
  completedAtMs: number;
};
type ExportQueueItemStatus = "queued" | "running" | "done" | "failed";
type ExportQueueItem = {
  id: number;
  inputPath: string;
  outputPath: string;
  format: OutputFormat;
  durationS: number | null;
  request: EncodeRequest;
  status: ExportQueueItemStatus;
  message: string | null;
  outputSizeBytes: number | null;
};

const QUEUE_STATUS_LABELS: Record<ExportQueueItemStatus, string> = {
  queued: "Queued",
  running: "Running",
  done: "Done",
  failed: "Failed",
};

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

function formatClock(totalSeconds: number) {
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return "0:00";

  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const secondsText = seconds.toFixed(2).padStart(5, "0");

  if (hours > 0) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${secondsText}`;
  }

  return `${minutes}:${secondsText}`;
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

function waitMs(ms: number) {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
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

function colorIsDefault(brightness: string, contrast: string, saturation: string) {
  const brightnessNum = Number(brightness);
  const contrastNum = Number(contrast);
  const saturationNum = Number(saturation);
  return !(
    (Number.isFinite(brightnessNum) && Math.abs(brightnessNum) > 0.001) ||
    (Number.isFinite(contrastNum) && Math.abs(contrastNum - 1) > 0.001) ||
    (Number.isFinite(saturationNum) && Math.abs(saturationNum - 1) > 0.001)
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
    const cropPx = {
      x: Math.round(cropRect.x * probe.width),
      y: Math.round(cropRect.y * probe.height),
      width: Math.round(cropRect.w * probe.width),
      height: Math.round(cropRect.h * probe.height),
    };
    const isFull =
      cropPx.x <= 1 && cropPx.y <= 1 && cropPx.width >= probe.width - 2 && cropPx.height >= probe.height - 2;
    if (!isFull) {
      width = evenPixel(cropPx.width);
      height = evenPixel(cropPx.height);
    }
  }

  if (rotateDeg === 90 || rotateDeg === 270) {
    const tmp = width;
    width = height;
    height = tmp;
  }

  return { width, height };
}

function fitMaxEdgeDimensions(width: number, height: number, maxEdge: number) {
  const longEdge = Math.max(width, height);
  if (!Number.isFinite(maxEdge) || maxEdge < OUTPUT_DIMENSION_MIN_PX || longEdge <= maxEdge) {
    return { width, height };
  }

  const scale = maxEdge / longEdge;
  return {
    width: evenPixel(Math.floor(width * scale)),
    height: evenPixel(Math.floor(height * scale)),
  };
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
  const [normalizeAudio, setNormalizeAudio] = useState(false);
  const [perturbFirstFrame, setPerturbFirstFrame] = useState(false);
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
  const [progress, setProgress] = useState(0);
  const [status, setStatus] = useState<string>("Pick a video to begin.");
  const [dragActive, setDragActive] = useState(false);
  const [settingsReady, setSettingsReady] = useState(false);
  const [smokeConfig, setSmokeConfig] = useState<AppSmokeConfig | null>(null);
  const [previewMediaReady, setPreviewMediaReady] = useState(false);
  const [lastExport, setLastExport] = useState<LastExportResult | null>(null);
  const [exportQueue, setExportQueue] = useState<ExportQueueItem[]>([]);
  const [queueRunning, setQueueRunning] = useState(false);
  const [queueActiveItemId, setQueueActiveItemId] = useState<number | null>(null);
  const [updateNotice, setUpdateNotice] = useState<UpdateCheckResponse | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [updateStatus, setUpdateStatus] = useState<string | null>(null);
  const [manualUpdateBusy, setManualUpdateBusy] = useState(false);
  const [manualUpdateStatus, setManualUpdateStatus] = useState<string | null>(null);
  // True when this instance was relaunched elevated to finish installing an
  // update; the app applies it immediately and restarts on its own.
  const [elevatedUpdateRun, setElevatedUpdateRun] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);

  const jobIdRef = useRef<number | null>(null);
  const inputPathRef = useRef("");
  const audioEnabledRef = useRef(true);
  const autoMutedRef = useRef(false);
  const formatRef = useRef<OutputFormat>(format);
  const outputAutoRef = useRef<boolean>(outputAuto);
  const cropperRef = useRef<VideoCropperHandle | null>(null);
  const trimTimelineTrackRef = useRef<HTMLDivElement | null>(null);
  const trimDragCleanupRef = useRef<(() => void) | null>(null);
  const exportQueueRef = useRef<ExportQueueItem[]>([]);
  const queueRunningRef = useRef(false);
  const queueActiveItemIdRef = useRef<number | null>(null);
  const queueIdRef = useRef(1);
  const smokeConfigRef = useRef<AppSmokeConfig | null>(null);
  const smokeStageRef = useRef<string | null>(null);
  const smokeStageHistoryRef = useRef<string[]>([]);
  const smokeStatusWriteRef = useRef<Promise<void>>(Promise.resolve());
  const smokeAppliedRef = useRef(false);
  const smokeStartRef = useRef(false);
  const smokeJobIdRef = useRef<number | null>(null);
  const smokeMetricsRef = useRef<{ trimStartS: number | null; trimEndS: number | null; expectedDurationS: number | null } | null>(null);
  const smokeInteractionRunningRef = useRef(false);
  const smokeInteractionDoneRef = useRef(false);
  const previewTimeRef = useRef(0);
  const previewSelectionTimeRef = useRef(0);
  const previewPlayingRef = useRef(false);
  const activeTrimTargetRef = useRef<TrimFocusTarget>(activeTrimTarget);
  const trimTimelineRef = useRef<TrimTimeline | null>(null);
  const pendingEncodeRef = useRef<{
    jobId: number | null;
    outputPath: string;
    durationS: number | null;
    format: OutputFormat;
    queueItemId: number | null;
    sample: { outputDurationS: number; fullDurationS: number | null } | null;
  } | null>(null);

  async function reportSmokeStatus(stage: string, extra: Omit<AppSmokeStatus, "stage"> = {}) {
    if (!smokeConfigRef.current) return;
    if (smokeStageRef.current === SMOKE_SUCCESS_STAGE || smokeStageRef.current === SMOKE_ERROR_STAGE) return;

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
            message: extra.message ?? null,
            outputPath: extra.outputPath ?? null,
            outputSizeBytes: extra.outputSizeBytes ?? null,
            trimStartS: extra.trimStartS ?? null,
            trimEndS: extra.trimEndS ?? null,
            expectedDurationS: extra.expectedDurationS ?? null,
            stageHistory: smokeStageHistoryRef.current,
          },
        });
      } catch (error) {
        console.warn("Failed to write smoke status:", error);
      }
    });
    await smokeStatusWriteRef.current;
  }

  async function reportSmokeFailure(message: string) {
    if (!smokeConfigRef.current) return;
    if (smokeStageRef.current === SMOKE_SUCCESS_STAGE || smokeStageRef.current === SMOKE_ERROR_STAGE) return;
    await reportSmokeStatus(SMOKE_ERROR_STAGE, { ok: false, message });
  }

  useEffect(() => {
    jobIdRef.current = jobId;
  }, [jobId]);

  useEffect(() => {
    formatRef.current = format;
  }, [format]);

  useEffect(() => {
    outputAutoRef.current = outputAuto;
  }, [outputAuto]);

  useEffect(() => {
    exportQueueRef.current = exportQueue;
  }, [exportQueue]);

  useEffect(() => {
    queueRunningRef.current = queueRunning;
  }, [queueRunning]);

  useEffect(() => {
    queueActiveItemIdRef.current = queueActiveItemId;
  }, [queueActiveItemId]);

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
    if (!aboutOpen) return;

    function handleKeydown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setAboutOpen(false);
      }
    }

    window.addEventListener("keydown", handleKeydown);
    return () => window.removeEventListener("keydown", handleKeydown);
  }, [aboutOpen]);

  useEffect(() => {
    if (activeTrimTarget === "preview" || previewPlaying) {
      previewSelectionTimeRef.current = previewTimeS;
    }
  }, [activeTrimTarget, previewPlaying, previewTimeS]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    let stop = false;

    void (async () => {
      try {
        await invoke("finalize_update_startup");
      } catch (error) {
        console.warn("Failed to finalize pending update state:", error);
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
        const result = await invoke<UpdateCheckResponse>("check_for_update", { force: false });
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
          const queuedCount = exportQueueRef.current.filter((item) => item.status === "queued").length;
          const encoding = jobIdRef.current !== null;
          if (!encoding && queuedCount === 0) return;

          const queuedText = `${queuedCount} queued export${queuedCount === 1 ? "" : "s"}`;
          const summary = encoding
            ? queuedCount > 0
              ? `An export is still running and ${queuedText} have not started.`
              : "An export is still running."
            : `${queuedText} have not started.`;
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
    setPreviewPlaying(false);
    setPreviewMediaReady(false);
    setLastExport(null);
    setActiveTrimTarget("preview");
    previewSelectionTimeRef.current = 0;
    smokeMetricsRef.current = null;
    pendingEncodeRef.current = null;
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
        smokeJobIdRef.current = null;
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

  function applyNewInput(path: string, nextFormat: OutputFormat) {
    setOutputAuto(true);
    setOutputPath("");

    if (nextFormat !== formatRef.current) setFormat(nextFormat);
    setInputPath(path);
    setStatus("Probing…");
  }

  function resetAllSettings() {
    setFormat(DEFAULT_OUTPUT_FORMAT);
    setTitle("");
    setSizeLimitMb(DEFAULT_SIZE_LIMIT_MB);
    setResizeMode("source");
    setMaxEdgePx("");
    setCustomWidthPx("");
    setCustomHeightPx("");
    setOutputAspectLocked(true);
    setAudioEnabled(true);
    setNormalizeAudio(false);
    setPerturbFirstFrame(false);
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

  function applyExportRecipe(recipe: ExportRecipe) {
    if (recipe.partial) {
      // Partial recipes change only the settings they list and leave the rest
      // of the current configuration (including the output path) untouched.
      const partialSettings = recipe.settings;
      if (partialSettings.format !== undefined) setFormat(partialSettings.format);
      if (partialSettings.sizeLimitMb !== undefined) setSizeLimitMb(partialSettings.sizeLimitMb);
      if (partialSettings.audioEnabled !== undefined) {
        setAudioEnabled(format === "mp3" ? true : partialSettings.audioEnabled);
      }
      if (partialSettings.normalizeAudio !== undefined) setNormalizeAudio(partialSettings.normalizeAudio);
      if (partialSettings.perturbFirstFrame !== undefined) setPerturbFirstFrame(partialSettings.perturbFirstFrame);
      setStatus(`Applied ${recipe.label}.`);
      return;
    }

    const recipeSettings = recipe.settings;
    const recipeAdvanced = recipeSettings.advanced;
    const recipeResize = normalizeRecipeResizeSettings(recipeSettings);

    setFormat(recipeSettings.format);
    setSizeLimitMb(recipeSettings.sizeLimitMb);
    setResizeMode(recipeResize.mode);
    setMaxEdgePx(recipeResize.maxEdgePx);
    setCustomWidthPx(recipeResize.widthPx);
    setCustomHeightPx(recipeResize.heightPx);
    setOutputAspectLocked(recipeResize.lockAspect);
    setAudioEnabled(
      recipeSettings.format === "mp3"
        ? true
        : recipeSettings.audioEnabled && (probe ? probe.hasAudio : true),
    );
    setNormalizeAudio(Boolean(recipeSettings.normalizeAudio));
    setPerturbFirstFrame(Boolean(recipeSettings.perturbFirstFrame));
    setAdvancedVideoCodec(recipeAdvanced.videoCodec);
    setAdvancedAudioBitrateKbps(recipeAdvanced.audioBitrateKbps === null ? "auto" : String(recipeAdvanced.audioBitrateKbps));
    setAdvancedVideoQuality(recipeAdvanced.videoQuality);
    setAdvancedEncodeSpeed(recipeAdvanced.encodeSpeed);
    setAdvancedFrameRateCapFps(recipeAdvanced.frameRateCapFps === null ? "auto" : String(recipeAdvanced.frameRateCapFps));
    setAdvancedAudioChannels(recipeAdvanced.audioChannels);
    setOutputAuto(true);
    setOutputPath("");
    setOutputSuggestNonce((n) => n + 1);
    setStatus(`Applied ${recipe.label}.`);
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
    if (!openCards.advanced && advancedVideoCodec === "auto") return;

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
  }, [openCards.advanced, advancedVideoCodec, encodeCapabilities, encodeCapabilitiesError]);

  useEffect(() => {
    if (!smokeConfig || smokeAppliedRef.current) return;

    smokeAppliedRef.current = true;
    smokeStartRef.current = false;
    smokeJobIdRef.current = null;
    smokeMetricsRef.current = null;
    smokeInteractionRunningRef.current = false;
    smokeInteractionDoneRef.current = false;

    handleDroppedPaths([smokeConfig.inputPath]);
    setOutputAuto(false);
    setOutputPath(smokeConfig.outputPath);
    setFormat(smokeConfig.format);
    setTitle("");
    setSizeLimitMb(formatNumberInput(smokeConfig.sizeLimitMb));
    setResizeMode(smokeConfig.resizeMode ?? "source");
    setMaxEdgePx(smokeConfig.resizeMaxEdgePx === null || smokeConfig.resizeMaxEdgePx === undefined ? "" : formatNumberInput(smokeConfig.resizeMaxEdgePx));
    setCustomWidthPx(smokeConfig.resizeWidthPx === null || smokeConfig.resizeWidthPx === undefined ? "" : formatNumberInput(smokeConfig.resizeWidthPx));
    setCustomHeightPx(smokeConfig.resizeHeightPx === null || smokeConfig.resizeHeightPx === undefined ? "" : formatNumberInput(smokeConfig.resizeHeightPx));
    setOutputAspectLocked(true);
    setAudioEnabled(true);
    setNormalizeAudio(false);
    setPerturbFirstFrame(smokeConfig.perturbFirstFrame ?? false);
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
    setReverse(false);
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
      message: `Smoke input staged through the shared drop path: ${smokeConfig.inputPath}`,
      outputPath: smokeConfig.outputPath,
    });
  }, [smokeConfig]);

  useEffect(() => {
    setTrimDragSnapS((current) => normalizeTrimDragSnapInput(current, probe?.durationS ?? null));
  }, [probe?.durationS]);

  const sizeLimitEnabled = sizeLimitMb.trim() !== "" && Number(sizeLimitMb) > 0;
  const shapedVideoDimensions = useMemo(() => {
    if (!probe) return null;
    return dimensionsAfterShape(probe, cropEnabled, cropRect, rotateDeg);
  }, [probe, cropEnabled, cropRect, rotateDeg]);

  const plannedSummary = useMemo(() => {
    if (!probe) return null;

    const sizeMb = Number(sizeLimitMb);
    if (!Number.isFinite(sizeMb) || sizeMb < 0) return null;

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
      return { durationS, totalKbps, sizeLimitEnabled, w: null as number | null, h: null as number | null };
    }

    if (!shapedVideoDimensions) return null;

    let w = shapedVideoDimensions.width;
    let h = shapedVideoDimensions.height;

    if (resizeMode === "maxEdge") {
      const maxEdge = maxEdgePx.trim() === "" ? null : Number(maxEdgePx);
      if (maxEdge === null || !Number.isFinite(maxEdge) || maxEdge < OUTPUT_DIMENSION_MIN_PX || maxEdge > OUTPUT_DIMENSION_MAX_PX) {
        return null;
      }
      const resized = fitMaxEdgeDimensions(w, h, maxEdge);
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

    return { durationS, totalKbps, sizeLimitEnabled, w, h };
  }, [
    probe,
    sizeLimitMb,
    speed,
    trimStart,
    trimEnd,
    format,
    loopVideo,
    shapedVideoDimensions,
    resizeMode,
    maxEdgePx,
    customWidthPx,
    customHeightPx,
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

  const cropSummary = useMemo(() => {
    if (!probe || !cropEnabled) return null;

    const cropPx = {
      x: Math.round(cropRect.x * probe.width),
      y: Math.round(cropRect.y * probe.height),
      width: Math.round(cropRect.w * probe.width),
      height: Math.round(cropRect.h * probe.height),
    };

    const isFull =
      cropPx.x <= 1 && cropPx.y <= 1 && cropPx.width >= probe.width - 2 && cropPx.height >= probe.height - 2;

    return isFull ? "Full frame selected." : `${cropPx.width}x${cropPx.height} at ${cropPx.x},${cropPx.y}.`;
  }, [probe, cropEnabled, cropRect]);

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

  const planSummaryText = useMemo(() => {
    if (!plannedSummary) return null;
    const shape =
      plannedSummary.w !== null && plannedSummary.h !== null
        ? `${plannedSummary.w}x${plannedSummary.h}`
        : "audio only";
    const bitrate = plannedSummary.totalKbps !== null ? `~${plannedSummary.totalKbps} kbps` : "no size limit";
    return `${shape} • ${formatClock(plannedSummary.durationS)} • ${bitrate}`;
  }, [plannedSummary]);
  const outputDimensionsSummary = useMemo(() => {
    if (format === "mp3") return "No video dimensions for MP3 output";
    if (resizeMode === "source") {
      return shapedVideoDimensions
        ? `Original ${shapedVideoDimensions.width}x${shapedVideoDimensions.height}`
        : "Original source dimensions";
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
  }, [format, resizeMode, shapedVideoDimensions, maxEdgePx, customWidthPx, customHeightPx]);

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
  const frameRateCapApplies =
    format !== "mp3" &&
    advancedFrameRateCapRequest !== null &&
    (sourceFrameRate === null || sourceFrameRate > advancedFrameRateCapRequest + 0.01);
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
        ? "Auto codec"
        : formatVideoCodecLabel(advancedVideoCodec);
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
        : sourceFrameRate !== null && sourceFrameRate <= advancedFrameRateCapRequest + 0.01
          ? `Cap ${advancedFrameRateCapRequest} fps, source is already lower`
          : `Cap at ${advancedFrameRateCapRequest} fps`;
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
  const hasVideoEditTransforms =
    Boolean(trimSummary) ||
    Boolean(cropEnabled && cropSummary && cropSummary !== "Full frame selected.") ||
    reverse ||
    loopVideo ||
    rotateDeg !== 0 ||
    (Number.isFinite(Number(speed)) && Math.abs(Number(speed) - 1) > 0.001) ||
    (format !== "mp3" && resizeMode !== "source") ||
    !colorIsDefault(brightness, contrast, saturation);
  const advancedForcesReencode =
    format !== "mp3" &&
    (advancedVideoCodec !== "auto" ||
      advancedVideoQualityApplies ||
      advancedEncodeSpeedApplies ||
      frameRateCapApplies ||
      advancedAudioApplies ||
      advancedAudioChannelsApplies ||
      normalizeAudioApplies);
  const encodeModeSummary =
    format === "mp3"
      ? "Audio-only re-encode"
      : advancedForcesReencode
        ? "Force re-encode because overrides are active"
        : sizeLimitEnabled
          ? "Re-encode for size target unless copy already fits"
          : hasVideoEditTransforms
            ? "Re-encode for edits"
            : "Auto stream copy when safe";
  const advancedPlanSummary = `${encodeModeSummary} • ${advancedCodecSummary}`;
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
      audioEnabled,
      normalizeAudio,
      perturbFirstFrame: format === "mp3" ? false : perturbFirstFrame,
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
      audioEnabled,
      normalizeAudio,
      perturbFirstFrame,
      advancedVideoCodec,
      advancedAudioBitrateRequest,
      advancedVideoQuality,
      advancedEncodeSpeed,
      advancedFrameRateCapRequest,
      advancedAudioChannels,
    ],
  );
  const matchingRecipe = useMemo(() => findMatchingExportRecipe(currentRecipeSettings), [currentRecipeSettings]);
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
        available: true,
        isDefault: value === fallbackVideoCodecOptions[0],
      }));

  const activeEditChips = useMemo(() => {
    const chips: string[] = [];

    const trimStartNum = trimStart.trim() === "" ? 0 : Number(trimStart);
    const trimEndNum = trimEnd.trim() === "" ? null : Number(trimEnd);
    if ((Number.isFinite(trimStartNum) && trimStartNum > 0) || (trimEndNum !== null && Number.isFinite(trimEndNum) && trimEndNum > 0)) {
      chips.push("Trim");
    }

    if (cropEnabled && cropSummary && cropSummary !== "Full frame selected.") chips.push("Crop");

    const speedNum = Number(speed);
    if (Number.isFinite(speedNum) && Math.abs(speedNum - 1) > 0.001) {
      chips.push(`${speedNum.toFixed(2).replace(/\.?0+$/, "")}x speed`);
    }

    if (rotateDeg !== 0) chips.push(`Rotate ${rotateDeg}°`);
    if (reverse) chips.push("Reverse");
    if (format !== "mp3" && loopVideo) chips.push("Loop");

    const brightnessNum = Number(brightness);
    const contrastNum = Number(contrast);
    const saturationNum = Number(saturation);
    if (
      (Number.isFinite(brightnessNum) && Math.abs(brightnessNum) > 0.001) ||
      (Number.isFinite(contrastNum) && Math.abs(contrastNum - 1) > 0.001) ||
      (Number.isFinite(saturationNum) && Math.abs(saturationNum - 1) > 0.001)
    ) {
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

    return chips;
  }, [
    trimStart,
    trimEnd,
    cropEnabled,
    cropSummary,
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
  ]);

  const lastExportSizeText = lastExport?.outputSizeBytes ? `${(lastExport.outputSizeBytes / 1_000_000).toFixed(2)} MB` : null;
  const lastExportDurationText = lastExport?.durationS ? formatClock(lastExport.durationS) : null;
  const progressUi = getActiveProgressUi(progress, jobId !== null);
  const displayedStatus = jobId !== null && progressUi.isFinalizing ? "Finalizing output..." : status;

  const footerKicker =
    jobId !== null
      ? progressUi.isFinalizing
        ? "Finalizing output"
        : "Encoding now"
      : lastExport
        ? "Export complete"
        : inputPath
        ? probe
          ? "Ready to export"
          : "Preparing input"
        : "Waiting for input";

  const footerMetaText = useMemo(() => {
    if (jobId !== null) return planSummaryText ?? inputSummary;
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
    jobId,
    lastExport,
    lastExportDurationText,
    lastExportSizeText,
    planSummaryText,
    inputSummary,
    outputPath,
    outputModeSummary,
  ]);

  const exportReady = Boolean(inputPath && outputPath && probe && !selectedVideoCodecUnavailable);
  const queueCounts = useMemo(
    () => ({
      queued: exportQueue.filter((item) => item.status === "queued").length,
      running: exportQueue.filter((item) => item.status === "running").length,
      done: exportQueue.filter((item) => item.status === "done").length,
      failed: exportQueue.filter((item) => item.status === "failed").length,
    }),
    [exportQueue],
  );
  const planStatusText =
    jobId !== null
      ? displayedStatus
      : !inputPath
        ? "Load a source video to unlock the export plan and composing tools."
        : !probe
          ? "Analyzing the source video so the export plan can be calculated."
          : !outputPath
            ? "Pick an output path to enable export."
            : selectedVideoCodecUnavailable
              ? "Choose an available codec before exporting."
            : lastExport
              ? "Last export completed. Review the output below or adjust the settings and export another variation."
              : "Source, output path, and current settings are valid. Export is ready.";

  const planWarnings = useMemo(() => {
    const warnings: string[] = [];
    if (!probe || !plannedSummary) return warnings;

    if (plannedSummary.sizeLimitEnabled) {
      warnings.push("Size targets are best effort; tight budgets can reduce quality, remove audio, or finish above target.");
      if (plannedSummary.totalKbps !== null && plannedSummary.totalKbps < 160 && format !== "mp3") {
        warnings.push("This target leaves a very low bitrate for video. Lower output dimensions, trim shorter, or raise the size limit.");
      }
      if (format !== "mp3" && probe.hasAudio && audioEnabled && plannedSummary.totalKbps !== null && plannedSummary.totalKbps < 82) {
        warnings.push("This target is too tight to keep audio; export may remove it to prioritize a playable video.");
      }
      if (
        plannedSummary.totalKbps !== null &&
        plannedSummary.w !== null &&
        plannedSummary.h !== null &&
        plannedSummary.w * plannedSummary.h >= 1280 * 720 &&
        plannedSummary.totalKbps < 700
      ) {
        warnings.push("The current resolution is high for this bitrate. Smaller output dimensions will usually behave better.");
      }
    }

    if (format === "mp3" && !probe.hasAudio) {
      warnings.push("MP3 export needs an input with an audio stream.");
    }

    if (selectedVideoCodecUnavailable) {
      warnings.push(`${VIDEO_CODEC_LABELS[advancedVideoCodec]} is not available in this FFmpeg build. Use Auto or another available codec.`);
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

    if (advancedFrameRateCapRequest !== null && sourceFrameRate !== null && sourceFrameRate <= advancedFrameRateCapRequest + 0.01) {
      warnings.push(`Frame-rate cap is set to ${advancedFrameRateCapRequest} fps, but the source is already about ${sourceFrameRate.toFixed(1)} fps.`);
    }

    if (advancedAudioChannels !== "auto" && !audioOverrideCanApply) {
      warnings.push("Audio channel override is waiting for an export with an included audio stream.");
    }

    return warnings;
  }, [
    probe,
    plannedSummary,
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
    advancedAudioChannels,
    audioOverrideCanApply,
  ]);

  function handleDroppedPaths(paths: string[]) {
    const action = resolveDroppedVideoAction({
      paths,
      currentFormat: formatRef.current,
      jobId: jobIdRef.current,
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
    }

    return action;
  }

  useEffect(() => {
    let stop = false;
    async function run() {
      if (!inputPath) {
        setOutputPath("");
        return;
      }
      if (!outputAuto) return;

      const takenPaths = exportQueueRef.current.map((item) => item.request.outputPath);
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
  }, [inputPath, format, outputAuto, outputSuggestNonce]);

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
      isDropAllowed: () => jobIdRef.current === null,
      onDragActiveChange: setDragActive,
      onPathsDropped: handleDroppedPaths,
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
            if (jobIdRef.current === null) {
              setDragActive(true);
            }
            return;
          }
          if (p.type === "leave") {
            setDragActive(false);
            return;
          }
          if (p.type !== "drop") return;
          handleDroppedPaths(p.paths);
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
        if (!p.hasAudio) {
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
        setCropRect({ x: 0, y: 0, w: 1, h: 1 });
        setCropDetectHint(null);
        setStatus("Ready.");
      } catch (e) {
        if (stop) return;
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

    if (probeError) {
      void reportSmokeFailure(`Packaged app smoke probe failed: ${probeError}`);
      return;
    }

    if (!smokeConfig.skipPreviewInteractions && previewError) {
      void reportSmokeFailure(`Packaged app smoke preview failed: ${previewError}`);
      return;
    }

    if (inputPath !== smokeConfig.inputPath || !probe) return;

    void reportSmokeStatus("probe-ready", {
      message: `Source probed: ${probe.width}x${probe.height}, ${formatClock(probe.durationS)}`,
    });

    if (smokeConfig.skipPreviewInteractions) {
      if (!smokeInteractionDoneRef.current) {
        const trimStartS = Math.max(0, smokeConfig.trimStartS);
        const trimEndS = Math.max(trimStartS, Math.min(probe.durationS, smokeConfig.trimEndS ?? probe.durationS));
        smokeMetricsRef.current = {
          trimStartS,
          trimEndS,
          expectedDurationS: Math.max(0, trimEndS - trimStartS),
        };
        smokeInteractionDoneRef.current = true;
        void reportSmokeStatus("interaction-ready", {
          message: "Headless packaged export smoke skipped preview playback checks after the source probe completed.",
          trimStartS,
          trimEndS,
          expectedDurationS: Math.max(0, trimEndS - trimStartS),
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
          await reportSmokeStatus("interaction-ready", {
            message: result.message,
            trimStartS: result.trimStartS ?? null,
            trimEndS: result.trimEndS ?? null,
            expectedDurationS: result.expectedDurationS ?? null,
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
      const result = await startEncode();
      if (!result.ok) {
        await reportSmokeFailure(`Packaged app smoke encode failed to start: ${result.message}`);
        return;
      }
      smokeJobIdRef.current = result.jobId;
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
    status,
  ]);

  useEffect(() => {
    if (!hasTauriRuntime()) return;

    let unlistenProgress: (() => void) | null = null;
    let unlistenFinished: (() => void) | null = null;

    (async () => {
      unlistenProgress = await listen<EncodeProgressPayload>("encode-progress", (event) => {
        setProgress(Math.max(0, Math.min(1, event.payload.overallPct)));
      });
      unlistenFinished = await listen<EncodeFinishedPayload>("encode-finished", (event) => {
        const p = event.payload;
        const pendingEncode = pendingEncodeRef.current;
        const completedQueueItemId = pendingEncode?.queueItemId ?? null;
        setJobId(null);
        jobIdRef.current = null;
        setProgress(0);

        if (smokeJobIdRef.current === p.jobId) {
          smokeJobIdRef.current = null;
          if (!p.ok) {
            void reportSmokeFailure(`Packaged app smoke encode failed: ${p.message || "Encode failed."}`);
          } else {
            const smokeMetrics = smokeMetricsRef.current;
            void reportSmokeStatus(SMOKE_SUCCESS_STAGE, {
              ok: true,
              message: null,
              outputPath: p.outputPath ?? null,
              outputSizeBytes: p.outputSizeBytes ?? null,
              trimStartS: smokeMetrics?.trimStartS ?? null,
              trimEndS: smokeMetrics?.trimEndS ?? null,
              expectedDurationS: smokeMetrics?.expectedDurationS ?? pendingEncode?.durationS ?? null,
            });
          }
        }

        if (!p.ok) {
          if (completedQueueItemId !== null) {
            updateExportQueue((items) =>
              items.map((item) =>
                item.id === completedQueueItemId
                  ? { ...item, status: "failed", message: p.message || "Encode failed.", outputSizeBytes: null }
                  : item,
              ),
            );
            queueActiveItemIdRef.current = null;
            setQueueActiveItemId(null);
          }
          pendingEncodeRef.current = null;
          setStatus(p.message || "Encode failed.");
          if (completedQueueItemId !== null && queueRunningRef.current) {
            window.setTimeout(() => void startNextQueuedItem(), 0);
          }
          return;
        }

        if (pendingEncode?.sample && p.outputSizeBytes) {
          const { outputDurationS, fullDurationS } = pendingEncode.sample;
          setSampleEstimate({
            sampleBytes: p.outputSizeBytes,
            estimateBytes:
              fullDurationS && outputDurationS > 0
                ? Math.round((p.outputSizeBytes * fullDurationS) / outputDurationS)
                : null,
          });
        }

        const sizeMb = p.outputSizeBytes ? p.outputSizeBytes / 1_000_000 : null;
        const suffix = sizeMb !== null ? ` (${sizeMb.toFixed(2)} MB)` : "";
        setStatus(`Done${suffix}.`);
        if (p.message) {
          setStatus(`Done${suffix}. ${p.message}`);
        }
        setLastExport({
          outputPath: p.outputPath ?? pendingEncode?.outputPath ?? outputPath,
          outputSizeBytes: p.outputSizeBytes ?? null,
          durationS: pendingEncode?.durationS ?? plannedSummary?.durationS ?? null,
          format: pendingEncode?.format ?? formatRef.current,
          message: p.message ?? null,
          diagnostics: p.diagnostics ?? null,
          completedAtMs: Date.now(),
        });
        if (completedQueueItemId !== null) {
          updateExportQueue((items) =>
            items.map((item) =>
              item.id === completedQueueItemId
                ? {
                    ...item,
                    status: "done",
                    message: p.message ?? null,
                    outputSizeBytes: p.outputSizeBytes ?? null,
                  }
                : item,
            ),
          );
          queueActiveItemIdRef.current = null;
          setQueueActiveItemId(null);
        }
        pendingEncodeRef.current = null;
        if (outputAutoRef.current) {
          setOutputSuggestNonce((n) => n + 1);
        }
        if (completedQueueItemId !== null && queueRunningRef.current) {
          window.setTimeout(() => void startNextQueuedItem(), 0);
        }
      });
    })();

    return () => {
      unlistenProgress?.();
      unlistenFinished?.();
    };
  }, []);

  async function pickInput() {
    try {
      const selected = await openDialog({
        multiple: false,
        directory: false,
        title: "Select a video",
        filters: [{ name: "Video", extensions: ["mp4", "mov", "mkv", "avi", "webm", "m4v"] }],
      });

      if (typeof selected === "string") {
        const pickedExt = extname(selected).toLowerCase();
        const nextFormat: OutputFormat =
          pickedExt === "mp4" || pickedExt === "webm" ? (pickedExt as OutputFormat) : format;

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

  async function pickOutput() {
    const ext = format;
    const defaultPath = outputPath || (inputPath ? suggestOutputPath(inputPath, ext) : `output.${ext}`);

    try {
      const selected = await saveDialog({
        title: "Save output as…",
        defaultPath,
        filters: [{ name: ext.toUpperCase(), extensions: [ext] }],
      });

      if (typeof selected === "string") {
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
    const resizeRequest = buildResizeRequest();

    return {
      inputPath: nextInputPath,
      outputPath: nextOutputPath,
      format,
      title: null,
      sizeLimitMb: size,
      audioEnabled: format === "mp3" ? true : audioEnabled,
      normalizeAudio,
      stripMetadata,
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
    };
  }

  function buildRequest(): EncodeRequest {
    if (!inputPath) throw new Error("Pick an input file first.");
    if (!outputPath) throw new Error("Pick an output path first.");
    if (!probe) throw new Error("Probe not ready yet.");
    if (format === "mp3" && !probe.hasAudio) throw new Error("Input has no audio stream (can't export MP3).");

    const size = parseNum("Size limit (MB)", sizeLimitMb, { min: 0 });
    if (size !== 0 && size < 0.1) throw new Error("Size limit must be >= 0.1 MB (or 0/empty to disable).");
    const s = parseNum("Speed", speed, { min: 0.05, max: 16 });
    const resizeRequest = buildResizeRequest();

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
    const trim = startS > 0 || endS !== null ? { startS, endS } : null;

    const cropPx = cropEnabled
      ? {
          x: Math.round(cropRect.x * probe.width),
          y: Math.round(cropRect.y * probe.height),
          width: Math.round(cropRect.w * probe.width),
          height: Math.round(cropRect.h * probe.height),
        }
      : null;
    const crop =
      cropPx &&
      !(cropPx.x <= 1 && cropPx.y <= 1 && cropPx.width >= probe.width - 2 && cropPx.height >= probe.height - 2)
        ? cropPx
        : null;

    return {
      inputPath,
      outputPath,
      format,
      title: title.trim() ? title.trim() : null,
      sizeLimitMb: size,
      audioEnabled: audioEnabled,
      normalizeAudio,
      stripMetadata,
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
    };
  }

  function updateExportQueue(updater: (items: ExportQueueItem[]) => ExportQueueItem[]) {
    const next = updater(exportQueueRef.current);
    exportQueueRef.current = next;
    setExportQueue(next);
    return next;
  }

  function createQueueItem(request: EncodeRequest, durationS: number | null): ExportQueueItem {
    const id = queueIdRef.current;
    queueIdRef.current += 1;
    return {
      id,
      inputPath: request.inputPath,
      outputPath: request.outputPath,
      format: request.format,
      durationS,
      request: cloneEncodeRequest(request),
      status: "queued",
      message: null,
      outputSizeBytes: null,
    };
  }

  function enqueueItems(items: ExportQueueItem[]) {
    if (!items.length) return;
    updateExportQueue((current) => [...current, ...items]);
    const label = items.length === 1 ? basename(items[0].inputPath) : `${items.length} files`;
    setStatus(`Queued ${label}.`);
  }

  function queuedOutputPaths(): string[] {
    return exportQueueRef.current.map((item) => item.request.outputPath);
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

  async function addCurrentPlanToQueue() {
    if (!exportReady) return;
    try {
      const request = buildRequest();
      // Earlier queue snapshots may already claim this suggested path even
      // though nothing exists on disk yet.
      const outputPath = ensureUniqueOutputPath(request.outputPath, queuedOutputPaths());
      enqueueItems([
        createQueueItem(
          outputPath === request.outputPath ? request : { ...request, outputPath },
          plannedSummary?.durationS ?? null,
        ),
      ]);
    } catch (e) {
      setStatus(coerceErrorMessage(e, "Failed to queue current export."));
    }
  }

  async function addFilesToQueue() {
    if (jobId !== null) return;
    try {
      const selected = await openDialog({
        multiple: true,
        directory: false,
        title: "Add files to export queue",
        filters: [{ name: "Video", extensions: SUPPORTED_INPUT_EXTENSIONS }],
      });
      const picked = Array.isArray(selected) ? selected : typeof selected === "string" ? [selected] : [];
      const paths = [...new Set(picked)].filter(Boolean);
      if (!paths.length) return;

      const items: ExportQueueItem[] = [];
      const takenPaths = queuedOutputPaths();
      for (const nextInputPath of paths) {
        const nextOutputPath = await suggestedOutputForInput(nextInputPath, format, takenPaths);
        takenPaths.push(nextOutputPath);
        const request = buildSettingsOnlyRequest(nextInputPath, nextOutputPath);
        items.push(createQueueItem(request, null));
      }
      enqueueItems(items);
    } catch (e) {
      setStatus(coerceErrorMessage(e, "Failed to add files to the queue."));
    }
  }

  function removeQueueItem(id: number) {
    updateExportQueue((items) => items.filter((item) => item.id !== id || item.status === "running"));
  }

  function clearCompletedQueueItems() {
    updateExportQueue((items) => items.filter((item) => item.status === "queued" || item.status === "running"));
  }

  function stopQueueAfterCurrent() {
    queueRunningRef.current = false;
    setQueueRunning(false);
    setStatus(jobIdRef.current === null ? "Queue stopped." : "Queue will stop after the current export.");
  }

  async function startNextQueuedItem() {
    if (!queueRunningRef.current || jobIdRef.current !== null) return;

    const nextItem = exportQueueRef.current.find((item) => item.status === "queued");
    if (!nextItem) {
      queueRunningRef.current = false;
      setQueueRunning(false);
      queueActiveItemIdRef.current = null;
      setQueueActiveItemId(null);
      setStatus("Queue complete.");
      return;
    }

    queueActiveItemIdRef.current = nextItem.id;
    setQueueActiveItemId(nextItem.id);
    updateExportQueue((items) =>
      items.map((item) =>
        item.id === nextItem.id
          ? { ...item, status: "running", message: null, outputSizeBytes: null }
          : item,
      ),
    );

    const queuedCount = exportQueueRef.current.filter((item) => item.status === "queued").length + 1;
    const result = await startEncode({
      request: cloneEncodeRequest(nextItem.request),
      durationS: nextItem.durationS,
      startingStatus: `Starting queued export (${queuedCount} remaining)…`,
      queueItemId: nextItem.id,
    });

    if (!result.ok) {
      updateExportQueue((items) =>
        items.map((item) =>
          item.id === nextItem.id
            ? { ...item, status: "failed", message: result.message, outputSizeBytes: null }
            : item,
        ),
      );
      queueActiveItemIdRef.current = null;
      setQueueActiveItemId(null);
      window.setTimeout(() => void startNextQueuedItem(), 0);
    }
  }

  function runQueue() {
    if (jobId !== null || queueRunningRef.current) return;
    const hasQueuedItems = exportQueueRef.current.some((item) => item.status === "queued");
    if (!hasQueuedItems) {
      setStatus("Queue is empty.");
      return;
    }
    queueRunningRef.current = true;
    setQueueRunning(true);
    void startNextQueuedItem();
  }

  async function autoDetectCrop() {
    if (!inputPath || !probe) return;
    const requestedPath = inputPath;
    setCropDetectHint(null);
    setCropDetecting(true);
    try {
      const crop = await invoke<Crop | null>("detect_crop", { path: inputPath });
      // A slow detection must not apply the old video's crop to a newly
      // loaded input.
      if (inputPathRef.current !== requestedPath) return;
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
    } catch (e) {
      if (inputPathRef.current !== requestedPath) return;
      const msg = typeof e === "string" ? e : e instanceof Error ? e.message : "Crop detection failed.";
      setCropDetectHint(msg);
    } finally {
      setCropDetecting(false);
    }
  }

  async function startEncode(options?: {
    request?: EncodeRequest;
    durationS?: number | null;
    startingStatus?: string;
    queueItemId?: number | null;
    sample?: { outputDurationS: number; fullDurationS: number | null } | null;
  }): Promise<StartEncodeResult> {
    try {
      const request = options?.request ?? buildRequest();
      pendingEncodeRef.current = {
        jobId: null,
        outputPath: request.outputPath,
        durationS: options?.durationS ?? plannedSummary?.durationS ?? null,
        format: request.format,
        queueItemId: options?.queueItemId ?? null,
        sample: options?.sample ?? null,
      };
      setStatus(options?.startingStatus ?? "Starting…");
      setProgress(0);
      const id = await invoke<number>("start_encode", { request });
      setJobId(id);
      jobIdRef.current = id;
      setStatus("Encoding…");
      smokeJobIdRef.current = id;
      if (pendingEncodeRef.current) {
        pendingEncodeRef.current.jobId = id;
      }
      return { ok: true, jobId: id };
    } catch (e) {
      pendingEncodeRef.current = null;
      const msg = coerceErrorMessage(e, "Failed to start encode.");
      setStatus(msg);
      return { ok: false, message: msg };
    }
  }

  async function exportSample() {
    if (!inputPath || !probe || jobId !== null || selectedVideoCodecUnavailable) return;

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
    if (jobId === null) return;
    if (queueActiveItemIdRef.current !== null) {
      queueRunningRef.current = false;
      setQueueRunning(false);
      setStatus("Canceling queued export…");
    } else {
      setStatus("Canceling…");
    }
    try {
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
    const p = lastExport?.outputPath || outputPath || inputPath;
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

  async function saveCurrentFrame() {
    if (!inputPath || !previewReady || !probe) return;
    if (frameSaving || jobId !== null) return;

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
    setPreviewPlaying(false);
  }

  function syncPreviewToTime(nextTimeS: number, target: TrimFocusTarget, opts?: { pause?: boolean }) {
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
    if (!probe || !trimTimeline) return;
    const maxStart = Math.max(0, trimTimeline.end - trimTimeline.minGap);
    const next = clamp(nextTimeS, 0, maxStart);
    setTrimStart(formatNumberInput(next));
    return next;
  }

  function setTrimEndValue(nextTimeS: number, opts?: { preferEmptyAtEnd?: boolean }) {
    if (!probe || !trimTimeline) return;
    const minEnd = Math.min(probe.durationS, trimTimeline.start + trimTimeline.minGap);
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
    if (!trimTimeline) return;
    const nextTimeS = target === "start" ? trimTimeline.start : trimTimeline.end;
    syncPreviewToTime(nextTimeS, target, { pause: true });
  }

  function runComposeShortcutAction(action: ComposeShortcutAction) {
    if (!previewReady || !probe || jobId !== null) return false;

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
    if (!inputPath || aboutOpen) return;
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

  async function runSmokeInteractionChecks() {
    if (!probe || !previewReady || !previewMediaReady) {
      return { ok: false, message: "Packaged app smoke tried interaction checks before the preview was ready." };
    }

    syncPreviewToTime(0.5, "preview", { pause: true });
    await waitMs(160);
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not nudge the free preview selection with the keyboard shortcut path." };
    }
    await waitMs(160);

    const rememberedStartS = previewSelectionTimeRef.current;
    focusTrimTarget("end");
    await waitMs(160);
    if (!runComposeShortcutAction({ kind: "apply-trim-start" })) {
      return { ok: false, message: "Packaged app smoke could not apply trim start through the keyboard shortcut path." };
    }
    await waitMs(160);

    const afterStartShortcut = trimTimelineRef.current;
    if (!afterStartShortcut || Math.abs(afterStartShortcut.start - rememberedStartS) > 0.06) {
      return {
        ok: false,
        message: `Packaged app smoke expected Set start to capture the remembered preview selection (${rememberedStartS.toFixed(2)}s) but saw ${(afterStartShortcut?.start ?? NaN).toFixed(2)}s.`,
      };
    }

    syncPreviewToTime(1.35, "preview", { pause: true });
    await waitMs(160);
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not fine-nudge the preview selection for trim end." };
    }
    await waitMs(160);

    const rememberedEndS = previewSelectionTimeRef.current;
    focusTrimTarget("start");
    await waitMs(160);
    if (!runComposeShortcutAction({ kind: "apply-trim-end" })) {
      return { ok: false, message: "Packaged app smoke could not apply trim end through the keyboard shortcut path." };
    }
    await waitMs(160);

    const afterEndShortcut = trimTimelineRef.current;
    if (!afterEndShortcut || Math.abs(afterEndShortcut.end - rememberedEndS) > 0.06) {
      return {
        ok: false,
        message: `Packaged app smoke expected Set end to capture the remembered preview selection (${rememberedEndS.toFixed(2)}s) but saw ${(afterEndShortcut?.end ?? NaN).toFixed(2)}s.`,
      };
    }

    focusTrimTarget("end");
    await waitMs(160);
    const beforeSelectedEndNudge = trimTimelineRef.current?.end ?? rememberedEndS;
    if (!runComposeShortcutAction({ kind: "nudge-timeline", deltaS: -TRIM_FINE_NUDGE_S })) {
      return { ok: false, message: "Packaged app smoke could not fine-nudge the selected trim boundary." };
    }
    await waitMs(160);

    const afterSelectedEndNudge = trimTimelineRef.current;
    const expectedEndAfterNudge = beforeSelectedEndNudge - TRIM_FINE_NUDGE_S;
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
      message: `Trim shortcuts kept start at ${afterStartShortcut.start.toFixed(2)}s, end at ${afterSelectedEndNudge.end.toFixed(2)}s, and crop-enabled playback advanced from ${startTime.toFixed(2)}s to ${advancedTime.toFixed(2)}s.`,
      trimStartS: afterStartShortcut.start,
      trimEndS: afterSelectedEndNudge.end,
      expectedDurationS: Math.max(0, afterSelectedEndNudge.end - afterStartShortcut.start),
    };
  }

  async function togglePreviewPlayback() {
    if (!previewReady || jobId !== null) return;
    const nextPlaying = await cropperRef.current?.togglePlayback();
    if (typeof nextPlaying === "boolean") {
      if (nextPlaying) {
        setActiveTrimTarget("preview");
      }
      setPreviewPlaying(nextPlaying);
    }
  }

  function resetTrim() {
    setTrimStart("0");
    setTrimEnd("");
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
    } catch (error) {
      console.warn("Failed to store update prompt choice:", error);
    } finally {
      setUpdateNotice(null);
      setUpdateStatus(null);
    }
  }

  async function checkForUpdatesNow() {
    if (manualUpdateBusy || updateBusy) return;

    setManualUpdateBusy(true);
    setManualUpdateStatus("Checking for updates...");
    try {
      const result = await invoke<UpdateCheckResponse>("check_for_update", { force: true });
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
      setManualUpdateStatus(coerceErrorMessage(error, "Update check failed."));
    } finally {
      setManualUpdateBusy(false);
    }
  }

  async function applyUpdate() {
    if (updateBusy) return;
    setUpdateBusy(true);
    setUpdateStatus("Preparing update...");
    try {
      const result = await invoke<UpdateApplyResponse>("prepare_and_apply_update");
      setUpdateStatus(result.message);
    } catch (error) {
      setUpdateStatus(coerceErrorMessage(error, "Update failed."));
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
    ? `Left/Right nudges ${formatTrimTargetLabel(selectedTrimBoundary)} by 0.1s. Shift uses 1s, [ sets Start, ] sets End, and Space plays.`
    : "Click Start or End above to select that boundary. Left/Right nudges the preview by 0.1s, Shift uses 1s, [ sets Start, ] sets End, and Space plays.";

  return (
    <div className="vfl-app">
      {dragActive && jobId === null ? (
        <div className="vfl-drop-overlay" aria-hidden="true">
          <div className="vfl-drop-overlay-card">Drop a video to open</div>
        </div>
      ) : null}
      {elevatedUpdateRun ? (
        <div className="vfl-modal-backdrop" role="presentation">
          <div className="vfl-elevated-update-card" role="alertdialog" aria-live="assertive" aria-label="Installing update">
            <div className="vfl-about-title">Installing update</div>
            <div className="vfl-muted">
              {updateBusy
                ? updateStatus ?? "Installing the update with administrator permission. The app restarts automatically."
                : updateStatus ?? "Installing the update with administrator permission."}
            </div>
            {!updateBusy ? (
              <div className="vfl-actions">
                <button onClick={() => void applyUpdate()}>Try again</button>
                <button onClick={() => setElevatedUpdateRun(false)}>Continue without updating</button>
              </div>
            ) : null}
          </div>
        </div>
      ) : null}
      {aboutOpen ? (
        <div className="vfl-modal-backdrop" role="presentation" onClick={() => setAboutOpen(false)}>
          <div
            className="vfl-about-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="vfl-about-title"
            onClick={(event) => event.stopPropagation()}
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
                    <div className="vfl-update-inline-status" role="status" aria-live="polite">
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
          </div>
        </div>
      ) : null}
      <header className="vfl-header">
        {updateNotice ? (
          <div className="vfl-update-banner" role="status" aria-live="polite">
            <div className="vfl-update-copy">
              <div className="vfl-update-kicker">Update available</div>
              <div className="vfl-update-title">
                Video For Lazies {updateNotice.latestVersion}
                {updateNotice.artifact?.sizeBytes ? ` (${(updateNotice.artifact.sizeBytes / (1024 * 1024)).toFixed(1)} MB)` : ""}
              </div>
              <div className="vfl-update-summary">{updateStatus ?? updateNotice.notes?.summary ?? "A new portable release is ready."}</div>
            </div>
            <div className="vfl-update-actions">
              <button className="primary" onClick={() => void applyUpdate()} disabled={updateBusy || jobId !== null}>
                {updateBusy ? "Updating..." : "Update now"}
              </button>
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
                    <div className="vfl-muted">Loading preview…</div>
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
                          cropEnabled={cropEnabled}
                          disabled={jobId !== null}
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
                        disabled={jobId !== null || !previewReady || !probe}
                        aria-label="Preview timeline"
                      />

                      <div className="vfl-transport-row">
                        <button className="primary" onClick={togglePreviewPlayback} disabled={jobId !== null || !previewReady || !probe}>
                          {previewPlaying ? "Pause" : "Play"}
                        </button>
                        <button onClick={() => stepPreview(-5)} disabled={jobId !== null || !previewReady || !probe}>
                          Back 5s
                        </button>
                        <button onClick={() => stepPreview(5)} disabled={jobId !== null || !previewReady || !probe}>
                          Forward 5s
                        </button>
                        <button
                          type="button"
                          className="vfl-icon-button"
                          onClick={saveCurrentFrame}
                          disabled={jobId !== null || !previewReady || !probe || frameSaving}
                          title="Save Frame"
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
                            <button
                              type="button"
                              className={`vfl-trim-timeline-grab vfl-trim-timeline-grab-start ${activeTrimTarget === "start" ? "active" : ""}`}
                              style={{ left: `${(trimTimeline.start / previewDurationS) * 100}%` }}
                              onPointerDown={(e) => beginTrimHandleDrag("start", e)}
                              onFocus={() => focusTrimTarget("start")}
                              disabled={jobId !== null || !previewReady || !probe || previewDurationS <= 0}
                              aria-label="Trim start"
                            />
                            <button
                              type="button"
                              className={`vfl-trim-timeline-grab vfl-trim-timeline-grab-end ${activeTrimTarget === "end" ? "active" : ""}`}
                              style={{ left: `${(trimTimeline.end / previewDurationS) * 100}%` }}
                              onPointerDown={(e) => beginTrimHandleDrag("end", e)}
                              onFocus={() => focusTrimTarget("end")}
                              disabled={jobId !== null || !previewReady || !probe || previewDurationS <= 0}
                              aria-label="Trim end"
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
                                disabled={jobId !== null}
                                inputMode="decimal"
                              />
                              <button
                                type="button"
                                onClick={applyTrimStartFromCurrent}
                                disabled={jobId !== null || !previewReady || !probe}
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
                                disabled={jobId !== null}
                                placeholder="(end)"
                                inputMode="decimal"
                              />
                              <button
                                type="button"
                                onClick={applyTrimEndFromCurrent}
                                disabled={jobId !== null || !previewReady || !probe}
                                title="Use the current preview time"
                              >
                                Set
                              </button>
                              <button type="button" onClick={() => setTrimEnd("")} disabled={jobId !== null || trimEnd.trim() === ""}>
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
                                disabled={jobId !== null || !probe}
                                inputMode="numeric"
                              />
                            </div>
                            <button type="button" className="vfl-trim-reset" onClick={resetTrim} disabled={jobId !== null}>
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
                        <button className="primary" onClick={pickInput} disabled={jobId !== null}>
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
                <button className={!inputPath ? "primary" : ""} onClick={pickInput} disabled={jobId !== null}>
                  Browse…
                </button>
              </div>
              <div className="vfl-inline-hint">Drag and drop works anywhere in the window.</div>
            </div>
            <div className="vfl-field">
              <label htmlFor="vfl-output-path">Output</label>
              <input id="vfl-output-path" value={outputPath} placeholder="Pick an output path…" readOnly />
              <div className="vfl-file-actions">
                <button className={!outputPath ? "primary" : ""} onClick={pickOutput} disabled={!inputPath || jobId !== null}>
                  Save as…
                </button>
                <button onClick={openOutputFolder} disabled={(!outputPath && !inputPath) || jobId !== null}>
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
            summary={matchingRecipe ? matchingRecipe.label : "Custom settings"}
            open={openCards.recipes}
            onToggle={() => toggleCard("recipes")}
          >
            <div className="vfl-muted vfl-section-caption">
              Apply a starting point, then adjust any setting before exporting.
            </div>
            <div className="vfl-recipe-grid">
              {EXPORT_RECIPES.map((recipe) => {
                const isActive = matchingRecipe?.id === recipe.id;
                const isDisabled = jobId !== null || (recipe.settings.format === "mp3" && probe !== null && !probe.hasAudio);
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
            </div>
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
                <select id="vfl-format" value={format} onChange={(e) => setFormat(e.currentTarget.value as OutputFormat)} disabled={jobId !== null}>
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
                    disabled={jobId !== null || format === "mp3" || !probe?.hasAudio}
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
                  disabled={jobId !== null}
                  placeholder="0 or empty = no limit"
                />
                <div className="vfl-chips" aria-label="Size presets">
                  <button
                    type="button"
                    className={`vfl-preset-chip ${Number(sizeLimitMb) === 0 ? "active" : ""}`}
                    onClick={() => setSizeLimitMb("")}
                    disabled={jobId !== null}
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
                        disabled={jobId !== null}
                        title={SIZE_PRESET_HINTS[v]}
                      >
                        {v} MB
                      </button>
                    );
                  })}
                </div>
              </div>
              <div className="vfl-field vfl-output-dimensions-field">
                <div className="vfl-field-label">Output dimensions</div>
                <div className="vfl-segmented" role="group" aria-label="Output dimensions">
                  <button
                    type="button"
                    className={resizeMode === "source" ? "active" : ""}
                    onClick={() => handleResizeModeChange("source")}
                    disabled={jobId !== null || format === "mp3"}
                    aria-pressed={resizeMode === "source"}
                  >
                    Original
                  </button>
                  <button
                    type="button"
                    className={resizeMode === "maxEdge" ? "active" : ""}
                    onClick={() => handleResizeModeChange("maxEdge")}
                    disabled={jobId !== null || format === "mp3"}
                    aria-pressed={resizeMode === "maxEdge"}
                  >
                    Max edge
                  </button>
                  <button
                    type="button"
                    className={resizeMode === "custom" ? "active" : ""}
                    onClick={() => handleResizeModeChange("custom")}
                    disabled={jobId !== null || format === "mp3"}
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
                        disabled={jobId !== null}
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
                            disabled={jobId !== null}
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
                            disabled={jobId !== null}
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
                          disabled={jobId !== null}
                        />
                        <span>Lock aspect ratio</span>
                      </label>
                      <div className="vfl-inline-hint">Odd values are snapped down to even pixels for encoder compatibility.</div>
                    </>
                  )}
                </div>
                <div className="vfl-inline-hint">{outputDimensionsSummary}</div>
              </div>
              <div className="vfl-field">
                <label htmlFor="vfl-title-metadata">Title metadata</label>
                <input id="vfl-title-metadata" value={title} onChange={(e) => setTitle(e.currentTarget.value)} disabled={jobId !== null} />
              </div>
              <div className="vfl-field">
                <div className="vfl-field-label">Privacy</div>
                <label className="vfl-check vfl-check-card">
                  <input
                    type="checkbox"
                    checked={stripMetadata}
                    onChange={(e) => setStripMetadata(e.currentTarget.checked)}
                    disabled={jobId !== null}
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
                    disabled={jobId !== null || format === "mp3"}
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
                disabled={jobId !== null || !probe}
              />
              Enable crop
            </label>
            <div className="vfl-row2 vfl-row2-compact">
              <label className="vfl-check">
                <input
                  type="checkbox"
                  checked={aspectLocked}
                  onChange={(e) => setAspectLocked(e.currentTarget.checked)}
                  disabled={jobId !== null || !cropEnabled}
                />
                Lock aspect
              </label>
              <div className="vfl-field">
                <label htmlFor="vfl-aspect-preset">Aspect preset</label>
                <select
                  id="vfl-aspect-preset"
                  value={aspectPreset}
                  onChange={(e) => setAspectPreset(e.currentTarget.value as typeof aspectPreset)}
                  disabled={jobId !== null || !cropEnabled || !aspectLocked}
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
            <div className="vfl-actions vfl-actions-wrap">
              <button
                onClick={autoDetectCrop}
                disabled={jobId !== null || !probe || !inputPath || cropDetecting}
                title="Detect black bars and suggest a crop"
              >
                {cropDetecting ? "Detecting…" : "Auto detect crop"}
              </button>
              <button
                onClick={() => {
                  setCropRect({ x: 0, y: 0, w: 1, h: 1 });
                  setCropDetectHint(null);
                }}
                disabled={jobId !== null || !probe}
                title="Reset crop selection"
              >
                Reset crop
              </button>
            </div>
            <div className="vfl-inline-hint">
              {cropEnabled ? "Drag directly on the preview to adjust the crop box." : "Enable crop to draw directly on the preview."}
            </div>
            {cropDetectHint ? <div className="vfl-inline-hint">{cropDetectHint}</div> : null}
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
                        <input id="vfl-speed" value={speed} onChange={(e) => setSpeed(e.currentTarget.value)} disabled={jobId !== null} />
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-rotate">Rotate</label>
                        <select
                          id="vfl-rotate"
                          value={rotateDeg}
                          onChange={(e) => setRotateDeg(Number(e.currentTarget.value))}
                          disabled={jobId !== null}
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
                        disabled={jobId !== null}
                      />
                      Reverse (video + audio)
                    </label>
                    <label className="vfl-check">
                      <input
                        type="checkbox"
                        checked={format === "mp3" ? false : loopVideo}
                        onChange={(e) => setLoopVideo(e.currentTarget.checked)}
                        disabled={jobId !== null || format === "mp3"}
                      />
                      Loop (boomerang)
                    </label>
                    <div className="vfl-inline-hint">
                      {format === "mp3"
                        ? "Loop only applies to video exports."
                        : "Plays the clip forward then in reverse so it loops seamlessly. Doubles the length."}
                    </div>
                  </div>

                  <div className="vfl-subsection-title">Color</div>
                  <div className="vfl-stack-md">
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
                          disabled={jobId !== null}
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
                          disabled={jobId !== null}
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
                        disabled={jobId !== null}
                      />
                      <div className="vfl-inline-hint">Boost or soften overall color intensity.</div>
                    </div>
                    <div className="vfl-actions">
                      <button
                        onClick={() => {
                          setBrightness("0");
                          setContrast("1");
                          setSaturation("1");
                        }}
                        disabled={jobId !== null}
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
                          disabled={jobId !== null || format === "mp3"}
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
                          disabled={jobId !== null || sizeLimitEnabled || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
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
                          disabled={jobId !== null || format === "mp3"}
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
                          disabled={jobId !== null || format === "mp3"}
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
                          disabled={jobId !== null || format === "mp3"}
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
                                ? `Frames above ${advancedFrameRateCapRequest} fps will be reduced.`
                                : "Source is already at or below the cap."}
                        </div>
                      </div>
                      <div className="vfl-field">
                        <label htmlFor="vfl-audio-channels">Audio channels</label>
                        <select
                          id="vfl-audio-channels"
                          value={advancedAudioChannels}
                          onChange={(e) => setAdvancedAudioChannels(e.currentTarget.value as AudioChannelPreference)}
                          disabled={jobId !== null || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
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
                            disabled={jobId !== null || (format !== "mp3" && (!audioEnabled || (probe !== null && !probe.hasAudio)))}
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
                              disabled={jobId !== null}
                              aria-pressed={sampleDurationS === seconds}
                            >
                              {seconds} s
                            </button>
                          ))}
                        </div>
                        <button
                          type="button"
                          onClick={() => void exportSample()}
                          disabled={!exportReady || jobId !== null}
                        >
                          Export sample
                        </button>
                      </div>
                      <div className="vfl-inline-hint">
                        {sampleEstimate
                          ? `Sample ${formatByteSize(sampleEstimate.sampleBytes)}${
                              sampleEstimate.estimateBytes ? ` -> full export ~ ${formatByteSize(sampleEstimate.estimateBytes)}` : ""
                            }`
                          : "Encodes a short slice around the preview position with the current settings."}
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
                        disabled={jobId !== null || advancedOverrideCount === 0}
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
                          disabled={jobId !== null}
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
                    <div className="vfl-plan-warnings" role="status" aria-live="polite">
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
            summary={jobId !== null ? "Encoding now" : exportReady ? "Ready" : "Waiting"}
            open={openCards.plan}
            onToggle={() => toggleCard("plan")}
          >
                  <div className={`vfl-plan-hero ${exportReady ? "is-ready" : ""} ${jobId !== null ? "is-busy" : ""} ${lastExport ? "is-done" : ""}`}>
                    <div className="vfl-plan-hero-kicker">{jobId !== null ? "Encoding now" : footerKicker}</div>
                    <div className="vfl-plan-hero-copy">{planStatusText}</div>
                  </div>
                  <div className="vfl-summary-list">
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Source</div>
                      <div className="vfl-summary-value">{inputSummary}</div>
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
                      <div className="vfl-summary-label">Planned</div>
                      <div className="vfl-summary-value">{planSummaryText ?? "Pick a valid input and settings to calculate the plan."}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Recipe</div>
                      <div className="vfl-summary-value">{matchingRecipe ? matchingRecipe.label : "Custom settings"}</div>
                    </div>
                    <div className="vfl-summary-row">
                      <div className="vfl-summary-label">Advanced</div>
                      <div className="vfl-summary-value">{advancedPlanSummary}</div>
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
                    <div className="vfl-export-result">
                      <div className="vfl-export-result-head">
                        <div>
                          <div className="vfl-export-result-kicker">Last export</div>
                          <div className="vfl-export-result-title">Done{lastExportSizeText ? ` (${lastExportSizeText})` : ""}</div>
                        </div>
                        <div className="vfl-chip active">{lastExport.format.toUpperCase()}</div>
                      </div>
                      <div className="vfl-export-result-meta">
                        {lastExportDurationText ? `${lastExportDurationText} • ` : ""}
                        {new Date(lastExport.completedAtMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                      </div>
                      {lastExport.message ? <div className="vfl-export-result-note">{lastExport.message}</div> : null}
                      <div className="vfl-export-result-path">{lastExport.outputPath}</div>
                      {lastExport.diagnostics ? (
                        <details className="vfl-export-diagnostics">
                          <summary>Export details</summary>
                          <div className="vfl-diagnostic-grid">
                            <div>
                              <span>Mode</span>
                              <strong>{lastExport.diagnostics.mode}</strong>
                            </div>
                            <div>
                              <span>Video</span>
                              <strong>{lastExport.diagnostics.videoCodec ?? "none"}</strong>
                            </div>
                            <div>
                              <span>Audio</span>
                              <strong>{lastExport.diagnostics.audioCodec ?? "none"}</strong>
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
                          </div>
                          {lastExport.diagnostics.audioRemovedForSizeTarget ? (
                            <div className="vfl-export-result-note">Audio was removed to fit the requested size target.</div>
                          ) : null}
                          <pre className="vfl-command-preview">{lastExport.diagnostics.commandPreview}</pre>
                        </details>
                      ) : null}
                      <div className="vfl-actions vfl-actions-secondary">
                        <button onClick={() => void openOutputFile(lastExport.outputPath)}>Open file</button>
                        <button onClick={() => void openOutputFolder()}>Open folder</button>
                      </div>
                    </div>
                  ) : null}
                  <div className="vfl-plan-actions">
                    <div className="vfl-actions vfl-actions-secondary">
                      <button onClick={resetAllSettings} disabled={jobId !== null}>
                        Reset all settings
                      </button>
                    </div>
                  </div>
          </RailCard>

          <RailCard
            title="Queue"
            summary={
              exportQueue.length
                ? queueActiveItemId !== null
                  ? `Item ${queueCounts.done + queueCounts.failed + 1} of ${exportQueue.length}`
                  : `${queueCounts.queued} queued`
                : "Empty"
            }
            open={openCards.queue}
            onToggle={() => toggleCard("queue")}
          >
            <div className="vfl-muted vfl-section-caption">
              Add exports as snapshots and run them one after another with no parallel FFmpeg jobs.
            </div>
            <div className="vfl-actions vfl-actions-wrap vfl-queue-actions">
              <button type="button" onClick={() => void addCurrentPlanToQueue()} disabled={!exportReady || jobId !== null}>
                Add current plan
              </button>
              <button type="button" onClick={() => void addFilesToQueue()} disabled={jobId !== null}>
                Add files
              </button>
              <button type="button" className="primary" onClick={runQueue} disabled={jobId !== null || queueRunning || queueCounts.queued === 0}>
                Run queue
              </button>
              <button type="button" onClick={stopQueueAfterCurrent} disabled={!queueRunning}>
                Stop after current
              </button>
              <button type="button" onClick={clearCompletedQueueItems} disabled={queueCounts.done + queueCounts.failed === 0}>
                Clear finished
              </button>
            </div>
            <div className="vfl-queue-counts" aria-live="polite">
              <span>{queueCounts.queued} queued</span>
              <span>{queueCounts.running} running</span>
              <span>{queueCounts.done} done</span>
              <span>{queueCounts.failed} failed</span>
            </div>
            {exportQueue.length ? (
              <div className="vfl-queue-list">
                {exportQueue.map((item) => (
                  <div key={item.id} className={`vfl-queue-item ${item.status} ${queueActiveItemId === item.id ? "active" : ""}`}>
                    <div className="vfl-queue-item-main">
                      <div className="vfl-queue-item-title">{basename(item.inputPath)}</div>
                      <div className="vfl-queue-item-meta">
                        {item.format.toUpperCase()} {"->"} {basename(item.outputPath)}
                        {item.outputSizeBytes ? ` • ${formatByteSize(item.outputSizeBytes)}` : ""}
                      </div>
                      {item.message ? <div className="vfl-queue-item-message">{item.message}</div> : null}
                    </div>
                    <div className="vfl-queue-item-actions">
                      <span className={`vfl-chip ${item.status === "running" || item.status === "done" ? "active" : ""}`}>
                        {QUEUE_STATUS_LABELS[item.status]}
                      </span>
                      {item.status === "done" ? (
                        <button type="button" onClick={() => void openFolderFor(item.outputPath)}>
                          Open folder
                        </button>
                      ) : null}
                      <button type="button" onClick={() => removeQueueItem(item.id)} disabled={item.status === "running"}>
                        Remove
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            ) : (
              <div className="vfl-inline-hint">
                Use Add files for a same-settings batch, or Add current plan to queue the loaded file with its current edits.
              </div>
            )}
          </RailCard>
        </aside>
      </div>

      <footer className={`vfl-footer ${jobId !== null ? "is-active" : "is-idle"}`}>
        <div className="vfl-footer-main">
          <div className="vfl-footer-copy">
            <div className="vfl-footer-kicker">{footerKicker}</div>
            <div className="vfl-footer-status" role="status" aria-live="polite">{displayedStatus}</div>
            <div className="vfl-footer-meta">{footerMetaText}</div>
          </div>
          <div className="vfl-footer-actions">
            {lastExport ? (
              <button onClick={() => void openOutputFile(lastExport.outputPath)} disabled={jobId !== null}>
                Open file
              </button>
            ) : null}
            <button onClick={openOutputFolder} disabled={(!outputPath && !inputPath) || jobId !== null}>
              Open folder
            </button>
            {jobId !== null ? (
              <button className="danger vfl-export-button" onClick={cancelEncode}>
                Cancel
              </button>
            ) : (
              <button className="primary vfl-export-button" onClick={() => void startEncode()} disabled={!exportReady}>
                Export
              </button>
            )}
          </div>
        </div>
        {queueActiveItemId !== null && exportQueue.length ? (
          <div className="vfl-footer-queue" aria-live="polite">
            Queue: item {queueCounts.done + queueCounts.failed + 1} of {exportQueue.length}
          </div>
        ) : null}
        {jobId !== null ? (
          <div
            className="vfl-progress"
            role="progressbar"
            aria-label="Encoding progress"
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={progressUi.percent}
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
        ) : null}
      </footer>
    </div>
  );
}

export default App;
