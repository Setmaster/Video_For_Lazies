export const UPDATE_PHASES = Object.freeze([
  "checking",
  "downloading",
  "verifyingArchive",
  "extracting",
  "verifyingPayload",
  "staging",
  "launchingHelper",
  "waitingForExit",
  "backingUp",
  "replacing",
  "rollingBack",
  "restarting",
  "recovering",
  "completed",
  "failed",
]);

const PHASE_LABELS = Object.freeze({
  checking: "Checking signed update",
  downloading: "Downloading update",
  verifyingArchive: "Verifying archive",
  extracting: "Extracting update",
  verifyingPayload: "Verifying payload",
  staging: "Staging update",
  launchingHelper: "Starting update helper",
  waitingForExit: "Waiting for the app to close",
  backingUp: "Backing up current files",
  replacing: "Installing verified files",
  rollingBack: "Restoring previous files",
  restarting: "Restarting Video For Lazies",
  recovering: "Checking update recovery",
  completed: "Update complete",
  failed: "Update failed",
});

function safeBytes(value) {
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) && numeric >= 0 ? numeric : null;
}

function formatByteCount(value) {
  return new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 }).format(value);
}

export function createUpdateProgressState() {
  return {
    operationId: null,
    phase: null,
    completedBytes: null,
    totalBytes: null,
    message: null,
  };
}

export function reduceUpdateProgress(previous, event) {
  const state = previous && typeof previous === "object" ? previous : createUpdateProgressState();
  const operationId = typeof event?.operationId === "string" && event.operationId ? event.operationId : null;
  const phase = UPDATE_PHASES.includes(event?.phase) ? event.phase : null;
  if (!operationId || !phase) return state;

  const terminal = state.phase === "completed" || state.phase === "failed";
  if (state.operationId && state.operationId !== operationId && !terminal) return state;

  const sameOperationAndPhase = state.operationId === operationId && state.phase === phase;
  const nextCompleted = safeBytes(event.completedBytes);
  const nextTotal = safeBytes(event.totalBytes);
  const completedBytes = sameOperationAndPhase && state.completedBytes !== null && nextCompleted !== null
    ? Math.max(state.completedBytes, nextCompleted)
    : nextCompleted;
  const totalBytes = nextTotal !== null && completedBytes !== null
    ? Math.max(nextTotal, completedBytes)
    : nextTotal;

  return {
    operationId,
    phase,
    completedBytes,
    totalBytes,
    message: typeof event.message === "string" && event.message ? event.message : PHASE_LABELS[phase],
  };
}

export function getUpdateProgressUi(state) {
  const phase = UPDATE_PHASES.includes(state?.phase) ? state.phase : null;
  const completedBytes = safeBytes(state?.completedBytes);
  const totalBytes = safeBytes(state?.totalBytes);
  const determinate = phase === "downloading" && completedBytes !== null && totalBytes !== null && totalBytes > 0;
  const percent = determinate ? Math.round(Math.min(1, completedBytes / totalBytes) * 100) : null;
  return {
    phase,
    label: phase ? PHASE_LABELS[phase] : "Preparing update",
    determinate,
    percent,
    valueText: determinate
      ? `${PHASE_LABELS[phase]}, ${formatByteCount(completedBytes)} of ${formatByteCount(totalBytes)} bytes, ${percent} percent`
      : phase ? PHASE_LABELS[phase] : "Preparing update",
  };
}
