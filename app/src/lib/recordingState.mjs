const activePhases = new Set(["preparing", "countdown", "starting", "recording", "stopping", "finalizing"]);
const phases = new Set(["idle", ...activePhases, "completed", "recoverable", "failed"]);

export function isRecordingActive(phase) {
  return activePhases.has(phase);
}

export function isCaptureSource(source) {
  if (!source || typeof source !== "object") return false;
  const identifier = (value) => typeof value === "string" && value.length > 0 && value.length <= 256;
  if (source.kind === "window") return identifier(source.windowId);
  return source.kind === "region" && identifier(source.displayId) &&
    [source.x, source.y, source.width, source.height].every(Number.isSafeInteger) &&
    source.width >= 16 && source.height >= 16 && source.width <= 7680 && source.height <= 4320;
}

export function captureSourceKey(source) {
  if (!isCaptureSource(source)) return "";
  return source.kind === "window" ? JSON.stringify(["window", source.windowId])
    : JSON.stringify(["region", source.displayId, source.x, source.y, source.width, source.height]);
}

export function recordingWindowProblem(windowId, windows) {
  if (!windowId) return "Choose a window or game to continue.";
  const target = windows.find((window) => window.id === windowId);
  if (!target) return "The selected window is unavailable. Refresh and select it again.";
  if (target.protection === "excluded") return "This window excludes screen capture. Choose another window.";
  if (!target.available) return target.reason || "This window cannot be captured. Choose another window.";
  if (target.method !== "wgc-window") return "This window's capture method is unsupported.";
  return null;
}

export function acceptRecordingSnapshot(current, incoming) {
  if (!incoming || !Number.isSafeInteger(incoming.sequence) || incoming.sequence < 0 ||
      !phases.has(incoming.phase) || !Array.isArray(incoming.recoveries) ||
      !Number.isFinite(incoming.elapsedMs) || incoming.elapsedMs < 0 ||
      !Number.isFinite(incoming.bytesWritten) || incoming.bytesWritten < 0 ||
      (incoming.sessionId !== null && typeof incoming.sessionId !== "string") ||
      (incoming.outputPath !== null && typeof incoming.outputPath !== "string") ||
      (incoming.message !== null && typeof incoming.message !== "string") ||
      (incoming.resolvedGeometry !== null && (!incoming.resolvedGeometry ||
        ![incoming.resolvedGeometry.width, incoming.resolvedGeometry.height].every(Number.isSafeInteger) ||
        incoming.resolvedGeometry.width < 16 || incoming.resolvedGeometry.height < 16 ||
        incoming.resolvedGeometry.width > 7680 || incoming.resolvedGeometry.height > 4320)) ||
      (incoming.selectionRequestId !== null && (typeof incoming.selectionRequestId !== "string" || !incoming.selectionRequestId)) ||
      (incoming.config !== null && (typeof incoming.config !== "object" || !incoming.config ||
        typeof incoming.config.requestId !== "string" || !isCaptureSource(incoming.config.source) ||
        ![24, 25, 30, 50, 60].includes(incoming.config.fps) || !["standard", "high"].includes(incoming.config.quality) ||
        ![0, 3, 5].includes(incoming.config.countdownSeconds) || typeof incoming.config.showCursor !== "boolean" ||
        (incoming.config.systemAudioDeviceId !== null && typeof incoming.config.systemAudioDeviceId !== "string") ||
        (incoming.config.microphoneDeviceId !== null && typeof incoming.config.microphoneDeviceId !== "string") ||
        typeof incoming.config.outputDirectory !== "string")) ||
      (incoming.countdownRemaining !== null && (!Number.isSafeInteger(incoming.countdownRemaining) || incoming.countdownRemaining < 0)) ||
      incoming.recoveries.some((recovery) => !recovery || typeof recovery.sessionId !== "string" || !recovery.sessionId || typeof recovery.createdAt !== "string" || typeof recovery.message !== "string") ||
      (incoming.phase === "completed" && !incoming.outputPath) ||
      (isRecordingActive(incoming.phase) && (typeof incoming.sessionId !== "string" || !incoming.sessionId))) {
    throw new Error("Recording returned an invalid status. Refresh status before starting another recording.");
  }
  return incoming.sequence > current.sequence ? incoming : current;
}

export function isRecordingRegionWithinDisplay(region, display) {
  return recordingSourceProblem(region, display) === null;
}

export function recordingSourceProblem(region, display) {
  if (!region) return "Choose your capture source to continue.";
  if (!display || region.displayId !== display.id) return "The selected display is unavailable. Choose a display again.";
  const within =
    [region.x, region.y, region.width, region.height, display.x, display.y, display.width, display.height].every(Number.isSafeInteger) &&
    region.width > 0 && region.height > 0 && display.width > 0 && display.height > 0 &&
    region.x >= display.x && region.y >= display.y &&
    region.x + region.width <= display.x + display.width &&
    region.y + region.height <= display.y + display.height;
  if (!within) return "The selected area is outside the display. Select it again.";
  if (region.width < 16 || region.height < 16) return "Choose an area at least 16 × 16 pixels.";
  if (region.width > 7680 || region.height > 4320) return "Recording supports up to 7680 × 4320 pixels. Select a smaller area.";
  return null;
}

export function formatRecordingDuration(milliseconds) {
  const seconds = Math.floor(Math.max(0, milliseconds) / 1000);
  return `${Math.floor(seconds / 3600).toString().padStart(2, "0")}:${Math.floor(seconds / 60 % 60).toString().padStart(2, "0")}:${(seconds % 60).toString().padStart(2, "0")}`;
}
