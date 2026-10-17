export const INTERPOLATION_MODELS = Object.freeze(["rife-v4.25", "rife-v4.26"]);
export const INTERPOLATION_TARGETS = Object.freeze([60, 120]);

/** Rebuild only reusable intent; never persist paths, device names or install state. */
export function normalizeInterpolation(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  if (!INTERPOLATION_TARGETS.includes(value.targetFps) || !INTERPOLATION_MODELS.includes(value.modelId)) return null;
  return { targetFps: value.targetFps, modelId: value.modelId };
}

export function interpolationConflict(value, format, cap) {
  const intent = normalizeInterpolation(value);
  if (!intent) return null;
  if (format === "mp3") return "Smooth motion needs MP4 or WebM video output. Turn it off to export MP3.";
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
