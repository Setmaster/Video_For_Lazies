export const ACTIVE_PROGRESS_DISPLAY_CAP = 0.99;
export const FINALIZING_PROGRESS_THRESHOLD = 0.999;
export const ENCODE_PROGRESS_PHASES = Object.freeze(["copying", "encoding", "finalizing"]);

export function clampProgress(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return 0;
  return Math.max(0, Math.min(1, numeric));
}

function safePositiveInteger(value, fallback = 1) {
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) && numeric > 0 ? numeric : fallback;
}

function normalizePhase(value) {
  return ENCODE_PROGRESS_PHASES.includes(value) ? value : "encoding";
}

export function createEncodeProgressState() {
  return {
    attemptId: null,
    jobId: null,
    phase: "encoding",
    stepIndex: 1,
    stepCount: 1,
    pass: 1,
    totalPasses: 1,
    passPct: 0,
    overallPct: 0,
  };
}

export function reduceEncodeProgress(previous, payload) {
  const state = previous && typeof previous === "object" ? previous : createEncodeProgressState();
  const attemptId = safePositiveInteger(payload?.attemptId, 0);
  const jobId = safePositiveInteger(payload?.jobId, 0);
  if (!attemptId || !jobId) return state;

  if (state.attemptId !== null) {
    if (attemptId < state.attemptId) return state;
    if (attemptId === state.attemptId && state.jobId !== null && jobId !== state.jobId) return state;
  }

  const sameAttempt = state.attemptId === attemptId && state.jobId === jobId;
  const rawOverallPct = clampProgress(payload?.overallPct);
  return {
    attemptId,
    jobId,
    phase: normalizePhase(payload?.phase),
    stepIndex: safePositiveInteger(payload?.stepIndex),
    stepCount: safePositiveInteger(payload?.stepCount),
    pass: safePositiveInteger(payload?.pass),
    totalPasses: safePositiveInteger(payload?.totalPasses),
    passPct: clampProgress(payload?.passPct),
    overallPct: sameAttempt ? Math.max(clampProgress(state.overallPct), rawOverallPct) : rawOverallPct,
  };
}

export function getActiveProgressUi(rawProgress, isActive) {
  const progressState = rawProgress && typeof rawProgress === "object" ? rawProgress : null;
  const rawValue = clampProgress(progressState?.overallPct ?? rawProgress);
  const phase = normalizePhase(progressState?.phase);
  const isFinalizing = Boolean(isActive) && (
    progressState ? phase === "finalizing" : rawValue >= FINALIZING_PROGRESS_THRESHOLD
  );
  const value = isActive ? Math.min(rawValue, ACTIVE_PROGRESS_DISPLAY_CAP) : rawValue;
  const percent = Math.round(value * 100);
  const stepIndex = safePositiveInteger(progressState?.stepIndex);
  const stepCount = Math.max(stepIndex, safePositiveInteger(progressState?.stepCount));
  const pass = safePositiveInteger(progressState?.pass);
  const totalPasses = Math.max(pass, safePositiveInteger(progressState?.totalPasses));
  const phaseLabel = isFinalizing
    ? "Finalizing output"
    : phase === "copying"
      ? "Copying media"
      : totalPasses > 1
        ? `Encoding pass ${pass} of ${totalPasses}`
        : "Encoding";
  const stepText = stepCount > 1 ? `, step ${stepIndex} of ${stepCount}` : "";

  return {
    value,
    percent,
    isFinalizing,
    phase,
    label: progressState ? phaseLabel : isFinalizing ? "Finalizing output" : "Progress",
    valueText: `${phaseLabel}${stepText}, ${percent} percent`,
  };
}
