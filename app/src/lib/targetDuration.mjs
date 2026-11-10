import { antiStutterRemovesTime } from "./interpolation.mjs";

export const MIN_TARGET_SPEED = .05;
export const MAX_TARGET_SPEED = 16;

export function validTargetDuration(value) {
  return typeof value === "number" && Number.isFinite(value) && value > 0;
}

export function resolveTargetSpeed(retained, target, multiplier = 1, fps = null) {
  const scale = 1_000_000;
  const raw = retained * multiplier / target;
  const clamped = Math.min(MAX_TARGET_SPEED, Math.max(MIN_TARGET_SPEED, raw));
  if (fps !== 60 && fps !== 120) {
    const speed = Math.round(clamped * scale) / scale;
    return { effectiveSpeed: speed, estimatedOutputDurationS: retained / speed * multiplier };
  }
  const durationNs = Math.round(retained * 1e9);
  if (!Number.isFinite(durationNs) || durationNs < 1) return null;
  const units = new Set([MIN_TARGET_SPEED * scale, MAX_TARGET_SPEED * scale]);
  const add = value => {
    for (const candidate of [Math.floor(value), Math.ceil(value)]) {
      if (Number.isFinite(candidate)) units.add(Math.min(MAX_TARGET_SPEED * scale, Math.max(MIN_TARGET_SPEED * scale, candidate)));
    }
  };
  add(clamped * scale);
  const frames = target * fps / multiplier;
  for (const count of [Math.floor(frames), Math.ceil(frames)]) {
    if (Number.isFinite(count) && count >= 1) add(retained * fps / count * scale);
  }
  const numerator = (2n * BigInt(durationNs) - 1n) * BigInt(fps) * 1_000_000n;
  let best = null;
  for (const candidate of units) {
    const denominator = 2_000_000_000n * BigInt(candidate);
    const count = (numerator + denominator - 1n) / denominator;
    const speed = candidate / scale;
    const duration = Number(count) * multiplier / fps;
    const error = Math.abs(duration - target), distance = Math.abs(speed - clamped);
    if (!best || error < best.error || error === best.error && (distance < best.distance || distance === best.distance && candidate < best.units)) {
      best = { effectiveSpeed: speed, estimatedOutputDurationS: duration, error, distance, units: candidate };
    }
  }
  return best && { effectiveSpeed: best.effectiveSpeed, estimatedOutputDurationS: best.estimatedOutputDurationS };
}

// A clip-scoped estimate. Native export resolves against authoritative retained
// time and frame rounding; never replace manual intent with this derived value.
export function planTargetDuration({ enabled = false, targetS, sourceDurationS,
  trimStartS = 0, trimEndS = null, loopVideo = false, format = "mp4",
  manualSpeed = 1, cleanupPending = false, timelinePending = false, interpolationTargetFps = null } = {}) {
  const manual = Number(manualSpeed);
  const base = { targetDurationS: null, calculatedSpeed: null, effectiveSpeed: Number.isFinite(manual) && manual > 0 ? manual : null,
    estimatedOutputDurationS: null, retainedDurationS: null, speedLimited: false, pending: false, waitingForCleanup: false, waitingForTimeline: false, error: null };
  if (!enabled) return base;
  const target = typeof targetS === "number" ? targetS : typeof targetS === "string" && targetS.trim() ? Number(targetS) : NaN;
  if (!validTargetDuration(target)) return { ...base, effectiveSpeed: null, error: "Enter a target duration greater than zero seconds." };
  const selected = { ...base, targetDurationS: target, effectiveSpeed: null };
  if (!Number.isFinite(sourceDurationS) || sourceDurationS <= 0) return { ...selected, pending: true };
  const start = trimStartS === "" ? 0 : Number(trimStartS);
  const end = trimEndS === "" || trimEndS === null ? sourceDurationS : Number(trimEndS);
  if (!Number.isFinite(start) || !Number.isFinite(end) || start < 0 || end < 0 || Math.min(end, sourceDurationS) <= Math.min(start, sourceDurationS)) {
    return { ...selected, error: "Choose a valid trim interval before calculating target speed." };
  }
  if (cleanupPending) return { ...selected, pending: true, waitingForCleanup: true };
  if (timelinePending) return { ...selected, pending: true, waitingForTimeline: true };
  const retained = Math.min(end, sourceDurationS) - Math.min(start, sourceDurationS);
  const multiplier = format !== "mp3" && loopVideo ? 2 : 1;
  const calculatedSpeed = retained * multiplier / target;
  const resolved = resolveTargetSpeed(retained, target, multiplier, interpolationTargetFps);
  if (!resolved) return { ...selected, error: "The selected interval cannot be represented for target-speed planning." };
  return { ...selected, calculatedSpeed: Number.isFinite(calculatedSpeed) ? calculatedSpeed : null, ...resolved, retainedDurationS: retained,
    speedLimited: calculatedSpeed < MIN_TARGET_SPEED || calculatedSpeed > MAX_TARGET_SPEED };
}

export const TARGET_REMOVE_SAMPLE_REASON = "Target speed is calculated after cleanup. Export the full video, or turn off Target duration or Remove repeated time to sample.";
export const TARGET_NUT_SAMPLE_REASON = "The full timeline needs analysis before target speed is known. Export the full video, or turn off Target duration to sample.";

export function targetSampleBlockingReason({ enabled, format = "mp4", interpolation, sourceFormat } = {}) {
  if (!enabled) return null;
  if (format !== "mp3" && antiStutterRemovesTime(interpolation)) return TARGET_REMOVE_SAMPLE_REASON;
  return sourceFormat === "nut" ? TARGET_NUT_SAMPLE_REASON : null;
}

export function prepareTargetSample(request, fullPlan) {
  if (request.targetDurationS != null && request.format !== "mp3" && antiStutterRemovesTime(request.interpolation)) throw new Error(TARGET_REMOVE_SAMPLE_REASON);
  const sample = { ...request, isSample: true };
  delete sample.targetDurationSample;
  if (request.targetDurationS != null) {
    if (fullPlan.error || fullPlan.pending || fullPlan.effectiveSpeed === null) throw new Error(fullPlan.error ?? "Target speed is not ready for a sample.");
    sample.speed = fullPlan.effectiveSpeed;
    sample.targetDurationSample = true;
    delete sample.targetDurationS;
  }
  return sample;
}

export function targetDurationSummary(plan) {
  if (plan.error) return plan.error;
  if (plan.pending) return plan.waitingForCleanup ? "Speed and final length are calculated after cleanup."
    : plan.waitingForTimeline ? "Speed and final length are calculated after full timeline analysis."
    : "Waiting for the source to calculate speed and length.";
  if (plan.effectiveSpeed === null || plan.estimatedOutputDurationS === null) return null;
  return `${plan.effectiveSpeed.toFixed(3)}× speed · about ${plan.estimatedOutputDurationS.toFixed(3)} s output.${plan.speedLimited ? " Closest supported speed reached (0.05–16×)." : " Actual length can differ by frame rounding."}`;
}
