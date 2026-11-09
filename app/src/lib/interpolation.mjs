export const INTERPOLATION_MODELS = Object.freeze(["rife-v4.25", "rife-v4.26", "gmfss-fortuna-union", "gimm-vfi-r-p", "ldf-vfi"]);
export const INTERPOLATION_TARGETS = Object.freeze([60, 120]);
export const ANTI_STUTTER_DEFAULT_STRENGTH = 75;

const MODEL_LABELS = Object.freeze({
  "rife-v4.25": "RIFE 4.25", "rife-v4.26": "RIFE 4.26",
  "gmfss-fortuna-union": "GMFSS Fortuna (Union)", "gimm-vfi-r-p": "GIMM-VFI-R-P", "ldf-vfi": "LDF-VFI",
});
export function interpolationModelLabel(modelId) {
  return Object.hasOwn(MODEL_LABELS, modelId) ? MODEL_LABELS[modelId] : modelId;
}

/** Rebuild only reusable intent; never persist paths, device names or install state. */
export function normalizeInterpolation(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  if (!INTERPOLATION_TARGETS.includes(value.targetFps) || !INTERPOLATION_MODELS.includes(value.modelId)) return null;
  const strength = value.antiStutterStrength;
  return {
    targetFps: value.targetFps,
    modelId: value.modelId,
    ...(value.antiStutter === true ? { antiStutter: true } : {}),
    ...(["remove", "repair"].includes(value.antiStutterMode) ? { antiStutterMode: value.antiStutterMode } : {}),
    ...(Number.isInteger(strength) && strength >= 0 && strength <= 100 && strength !== ANTI_STUTTER_DEFAULT_STRENGTH ? { antiStutterStrength: strength } : {}),
  };
}

// Missing mode belongs to older duration-preserving settings. Only a new enable
// chooses removal; reading saved intent must never silently shorten a clip.
export function antiStutterMode(value) {
  return value?.antiStutterMode === "remove" ? "remove" : "repair";
}
export function antiStutterRemovesTime(value) {
  return value?.antiStutter === true && antiStutterMode(value) === "remove";
}
export function setAntiStutterEnabled(value, enabled) {
  return { ...value, antiStutter: enabled,
    antiStutterMode: value.antiStutterMode ?? (value.antiStutter === true ? "repair" : "remove") };
}
export function antiStutterPlanSummary(value) {
  if (!value?.antiStutter) return null;
  return antiStutterRemovesTime(value)
    ? "Remove repeated time; bridge motion where reliable and shorten output with matching audio cuts"
    : "Fill pauses; preserve duration and audio timing";
}
export function resolvedOutputDuration(diagnostics, fallback = null) {
  const duration = diagnostics?.outputDurationS;
  if (Number.isFinite(duration) && duration > 0) return duration;
  return diagnostics?.interpolation?.antiStutter?.mode === "remove" ? null : fallback;
}

export function antiStutterConflict(value) {
  return value?.antiStutter === true && value.modelId === "ldf-vfi"
    ? "Anti-stutter needs RIFE, GMFSS or GIMM. Choose one of those models or turn Anti-stutter off."
    : null;
}

export function antiStutterSummary(value) {
  if (!value) return null;
  const repaired = value.repairedRuns;
  const unchanged = value.unchangedRuns;
  if (value.mode === "remove") {
    const runs = value.removedRuns ?? 0;
    const seconds = value.removedDurationS ?? 0;
    const duration = seconds > 0 && seconds < .001 ? "<0.001" : seconds.toFixed(3);
    const bridged = value.bridgedRuns ?? 0;
    const retained = value.retainedBridgeDurationS ?? 0;
    const bridge = bridged > 0 && Number.isFinite(retained) && retained > 0
      ? ` ${retained < .001 ? "<0.001" : retained.toFixed(3)} s extra retained for smoother motion across ${bridged} transition${bridged === 1 ? "" : "s"}.`
      : "";
    return `Anti-stutter: ${runs === 0 ? "no repeated time removed" : `${duration} s of repeated source time removed from ${runs} hold${runs === 1 ? "" : "s"}`}.${bridge}${repaired > 0 ? ` ${repaired} pause${repaired === 1 ? "" : "s"} filled without removing time.` : ""}${unchanged > 0 ? ` ${unchanged} candidate${unchanged === 1 ? "" : "s"} left unchanged.` : ""}`;
  }
  return `Anti-stutter: ${repaired === 0 ? "no short freezes repaired" : `${repaired} suspected freeze${repaired === 1 ? "" : "s"} repaired`}.${unchanged > 0 ? ` ${unchanged} candidate${unchanged === 1 ? "" : "s"} left unchanged.` : ""}`;
}

export function interpolationConflict(value, format, cap) {
  const intent = normalizeInterpolation(value);
  if (!intent) return null;
  if (format === "mp3") return "Smooth motion needs MP4 or WebM video output. Turn it off to export MP3.";
  const repairConflict = antiStutterConflict(intent);
  if (repairConflict) return repairConflict;
  if (cap !== null && cap !== undefined && Number(cap) > 0 && Number(cap) < intent.targetFps) {
    return `The ${cap} fps cap is below Smooth motion's ${intent.targetFps} fps target. Remove the cap or turn Smooth motion off.`;
  }
  return null;
}

function validOperationId(value) {
  return (typeof value === "string" && value.length > 0) || (Number.isSafeInteger(value) && value > 0);
}

const TERMINAL = new Set(["ready", "canceled", "failed"]);
const PHASE_RANK = { downloading: 1, verifying: 2, testing: 3, ready: 4, canceled: 4, failed: 4 };
export function createInterpolationInstall(modelId) {
  return { modelId, operationId: null, state: "idle", completedBytes: 0, totalBytes: 0, message: null, pendingEvents: [] };
}
export function beginInterpolationInstall(modelId) {
  return { ...createInterpolationInstall(modelId), state: "starting" };
}
export function reduceInterpolationInstall(previous, event) {
  if (!previous || !event || event.modelId !== previous.modelId || !validOperationId(event.operationId) || !PHASE_RANK[event.state]) return previous;
  if (previous.state === "idle" || TERMINAL.has(previous.state)) return previous;
  // The native thread can emit before invoke returns its operation ID. Never
  // infer ownership from model ID alone; replay only the returned operation.
  if (previous.operationId === null) {
    return { ...previous, pendingEvents: [...previous.pendingEvents, event].slice(-32) };
  }
  if (event.operationId !== previous.operationId || PHASE_RANK[event.state] < (PHASE_RANK[previous.state] ?? 0)) return previous;
  const totalBytes = Number.isSafeInteger(event.totalBytes) && event.totalBytes >= 0 ? event.totalBytes : previous.totalBytes;
  const completed = Number.isSafeInteger(event.completedBytes) && event.completedBytes >= 0 ? event.completedBytes : previous.completedBytes;
  return { ...previous, state: event.state, message: typeof event.message === "string" ? event.message : previous.message, totalBytes, completedBytes: Math.min(totalBytes || completed, Math.max(previous.completedBytes, completed)) };
}
export function bindInterpolationInstall(previous, operationId) {
  if (previous?.state !== "starting" || !validOperationId(operationId)) return previous;
  let next = { ...previous, operationId, state: "downloading", pendingEvents: [] };
  for (const event of previous.pendingEvents) next = reduceInterpolationInstall(next, event);
  return next;
}
export function interpolationInstallActive(state) {
  return ["starting", "downloading", "verifying", "testing"].includes(state?.state);
}

/** IPC command errors are redacted public strings. Never expose arbitrary Error objects. */
export function interpolationPublicMessage(value, fallback) {
  return typeof value === "string" && value.trim() ? value.trim().slice(0, 1000) : fallback;
}
