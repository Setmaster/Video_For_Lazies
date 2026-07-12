export const STRICT_FIT_MAX_PLANS = 4;
export const STRICT_FIT_REDUCED_AUDIO_KBPS = 32;
export const STRICT_FIT_FRAME_RATE_CAP_FPS = 30;
export const STRICT_FIT_MAX_EDGE_TIERS = Object.freeze([1280, 960, 720, 540, 360]);

function isPositiveSafeInteger(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function isNonNegativeSafeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function finitePositive(value) {
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? value : null;
}

function formatInteger(value) {
  return String(value).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

function formatPercent(value) {
  if (value > 0 && value < 0.01) return "<0.01%";
  return `${value.toFixed(2)}%`;
}

function cleanDisplayText(value, maxLength = 160) {
  if (typeof value !== "string") return null;
  const cleaned = value
    .replace(/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return cleaned ? cleaned.slice(0, maxLength) : null;
}

function freezeAction(action) {
  return Object.freeze({
    ...action,
    changes: Object.freeze(action.changes),
  });
}

/**
 * Converts decimal megabytes to the exact integer byte target shared by the
 * frontend and backend. Zero means no target; null means the value cannot be
 * represented as a positive safe integer byte count.
 */
export function exactTargetBytesFromMegabytes(value) {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return null;
  if (value === 0) return 0;
  const bytes = Math.trunc(value * 1_000_000);
  return isPositiveSafeInteger(bytes) ? bytes : null;
}

/** Canonicalizes Strict Fit without accepting truthy strings. */
export function canonicalizeStrictFitOptions(value) {
  return Object.freeze({
    strictFit: value?.strictFit === true,
  });
}

/**
 * Validates the backend's authoritative exact-byte classification. A
 * contradictory payload is rejected rather than silently reclassified in JS.
 */
export function canonicalizeTargetResult(value) {
  if (!value || (value.status !== "met" && value.status !== "missed")) return null;
  if (!isPositiveSafeInteger(value.targetBytes) || !isPositiveSafeInteger(value.actualBytes)) {
    return null;
  }
  if (!isNonNegativeSafeInteger(value.overshootBytes)) return null;

  const expectedOvershoot = Math.max(0, value.actualBytes - value.targetBytes);
  if (value.overshootBytes !== expectedOvershoot) return null;
  if (value.status === "met" && value.actualBytes > value.targetBytes) return null;
  if (value.status === "missed" && value.actualBytes <= value.targetBytes) return null;

  return Object.freeze({
    status: value.status,
    targetBytes: value.targetBytes,
    actualBytes: value.actualBytes,
    overshootBytes: value.overshootBytes,
  });
}

/**
 * Produces deterministic display data from a validated typed result. Rounded
 * decimal-MB strings are display-only and never participate in classification.
 */
export function targetResultFormatData(value) {
  const result = canonicalizeTargetResult(value);
  if (!result) return null;

  const remainingBytes = Math.max(0, result.targetBytes - result.actualBytes);
  const overshootPercent = result.status === "missed"
    ? (result.overshootBytes / result.targetBytes) * 100
    : 0;

  return Object.freeze({
    ...result,
    met: result.status === "met",
    missed: result.status === "missed",
    remainingBytes,
    overshootPercent,
    targetBytesText: `${formatInteger(result.targetBytes)} bytes`,
    actualBytesText: `${formatInteger(result.actualBytes)} bytes`,
    overshootBytesText: `${formatInteger(result.overshootBytes)} bytes`,
    remainingBytesText: `${formatInteger(remainingBytes)} bytes`,
    targetMegabytesText: `${(result.targetBytes / 1_000_000).toFixed(2)} MB`,
    actualMegabytesText: `${(result.actualBytes / 1_000_000).toFixed(2)} MB`,
    overshootPercentText: formatPercent(overshootPercent),
  });
}

/**
 * Picks the next tier below the effective planned long edge. currentMaxEdgePx
 * is optional, but lets callers avoid a no-op when source dimensions are
 * larger than an already-selected max-edge control.
 */
export function nextLowerStrictFitMaxEdge(width, height, currentMaxEdgePx = null) {
  if (!isPositiveSafeInteger(width) || !isPositiveSafeInteger(height)) return null;
  if (currentMaxEdgePx !== null && currentMaxEdgePx !== undefined &&
      !isPositiveSafeInteger(currentMaxEdgePx)) {
    return null;
  }

  const longEdge = Math.max(width, height);
  const effectiveLongEdge = currentMaxEdgePx == null
    ? longEdge
    : Math.min(longEdge, currentMaxEdgePx);
  return STRICT_FIT_MAX_EDGE_TIERS.find((tier) => tier < effectiveLongEdge) ?? null;
}

/**
 * Lists only control changes that can help a measured miss. Applying one of
 * these actions must remain a separate UI event; this helper never starts work.
 */
export function strictFitCorrectiveActions({
  targetResult,
  format,
  width,
  height,
  currentMaxEdgePx = null,
  plannedFrameRateFps = null,
  hasAudio = false,
  audioEnabled = false,
  strictFit = false,
} = {}) {
  const result = canonicalizeTargetResult(targetResult);
  if (!result || result.status !== "missed") return Object.freeze([]);

  const actions = [];
  const normalizedFormat = String(format ?? "").trim().toLowerCase();
  const isVideo = normalizedFormat === "mp4" || normalizedFormat === "webm";
  if (!isVideo) return Object.freeze(actions);

  const nextMaxEdgePx = nextLowerStrictFitMaxEdge(width, height, currentMaxEdgePx);
  if (nextMaxEdgePx !== null) {
    actions.push(freezeAction({
      kind: "reduceMaxEdge",
      label: `Use ${nextMaxEdgePx} px max edge`,
      confirmation: `Max edge set to ${nextMaxEdgePx} px. No export started.`,
      maxEdgePx: nextMaxEdgePx,
      changes: { resizeMode: "maxEdge", maxEdgePx: nextMaxEdgePx },
    }));
  }

  const effectiveFrameRate = finitePositive(plannedFrameRateFps);
  if (effectiveFrameRate !== null && effectiveFrameRate > STRICT_FIT_FRAME_RATE_CAP_FPS) {
    actions.push(freezeAction({
      kind: "capFrameRate",
      label: `Cap at ${STRICT_FIT_FRAME_RATE_CAP_FPS} fps`,
      confirmation: `Frame-rate cap set to ${STRICT_FIT_FRAME_RATE_CAP_FPS} fps. No export started.`,
      frameRateCapFps: STRICT_FIT_FRAME_RATE_CAP_FPS,
      changes: { frameRateCapFps: STRICT_FIT_FRAME_RATE_CAP_FPS },
    }));
  }

  if (hasAudio === true && audioEnabled === true) {
    actions.push(freezeAction({
      kind: "removeAudio",
      label: "Remove audio",
      confirmation: "Audio disabled. No export started.",
      changes: { audioEnabled: false },
    }));
  }

  if (strictFit !== true) {
    actions.push(freezeAction({
      kind: "enableStrictFit",
      label: "Enable Strict Fit",
      confirmation: "Strict Fit enabled. No export started.",
      changes: { strictFit: true },
    }));
  }

  return Object.freeze(actions);
}

export function summarizeStrictFitPolicy(value) {
  const options = canonicalizeStrictFitOptions(value);
  if (!options.strictFit) {
    return "Strict Fit is off. The requested settings will run without structural fallback plans.";
  }

  const audioPolicy = value?.audioEnabled === false
    ? "The final audio plan is skipped because audio is not included."
    : `The final applicable plan keeps audio at ${STRICT_FIT_REDUCED_AUDIO_KBPS} kbps.`;
  return `Strict Fit may run at most ${STRICT_FIT_MAX_PLANS} plans in order: the requested plan, a bitrate correction, one lower max-edge tier, then the applicable final audio plan. No-op plans are skipped. ${audioPolicy}`;
}

export function summarizeStrictFitPlan(value) {
  if (!value || !isPositiveSafeInteger(value.planNumber) ||
      value.planNumber > STRICT_FIT_MAX_PLANS ||
      (value.status !== "met" && value.status !== "missed") ||
      !isPositiveSafeInteger(value.actualSizeBytes)) {
    return null;
  }

  const label = cleanDisplayText(value.label);
  const dimensions = isPositiveSafeInteger(value.width) && isPositiveSafeInteger(value.height)
    ? `${value.width}x${value.height}`
    : null;
  const settings = [dimensions];
  if (isPositiveSafeInteger(value.videoBitrateKbps)) {
    settings.push(`${value.videoBitrateKbps} kbps video`);
  }
  if (value.audioAction === "drop") {
    settings.push("audio removed");
  } else if (isPositiveSafeInteger(value.audioBitrateKbps)) {
    settings.push(`${value.audioBitrateKbps} kbps audio`);
  }

  const mutations = Array.isArray(value.mutations)
    ? value.mutations
      .map((mutation) => cleanDisplayText(mutation))
      .filter(Boolean)
      .slice(0, 16)
    : [];
  const planName = label ? `Plan ${value.planNumber} (${label})` : `Plan ${value.planNumber}`;
  const settingText = settings.filter(Boolean).length > 0
    ? ` with ${settings.filter(Boolean).join(", ")}`
    : "";
  const mutationText = mutations.length > 0
    ? ` Changes: ${mutations.join("; ")}.`
    : " No structural changes.";
  const selectedText = value.selected === true ? " This was the published plan." : "";
  return `${planName} ${value.status} the target at ${formatInteger(value.actualSizeBytes)} bytes${settingText}.${mutationText}${selectedText}`;
}

export function summarizeTargetResult(value) {
  const data = targetResultFormatData(value);
  if (!data) return null;
  if (data.met) {
    return `Target met: ${data.actualBytesText} used of the ${data.targetBytesText} limit (${data.remainingBytesText} remaining).`;
  }
  return `Target missed: ${data.actualBytesText} is ${data.overshootBytesText} (${data.overshootPercentText}) over the ${data.targetBytesText} limit.`;
}
