export const ACTIVE_PROGRESS_DISPLAY_CAP = 0.99;
export const FINALIZING_PROGRESS_THRESHOLD = 0.999;

export function clampProgress(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) return 0;
  return Math.max(0, Math.min(1, numeric));
}

export function getActiveProgressUi(rawProgress, isActive) {
  const rawValue = clampProgress(rawProgress);
  const isFinalizing = Boolean(isActive) && rawValue >= FINALIZING_PROGRESS_THRESHOLD;
  const value = isActive ? Math.min(rawValue, ACTIVE_PROGRESS_DISPLAY_CAP) : rawValue;
  const percent = Math.round(value * 100);

  return {
    value,
    percent,
    isFinalizing,
    label: isFinalizing ? "Finalizing output" : "Progress",
  };
}
