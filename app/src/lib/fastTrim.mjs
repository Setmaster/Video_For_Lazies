const FAST_TRIM_PHASES = new Set(["idle", "checking", "ready", "blocked", "stale", "error"]);

function finiteInteger(value) {
  return Number.isSafeInteger(value) ? value : null;
}

function normalizedAdvanced(advanced) {
  const value = advanced && typeof advanced === "object" ? advanced : {};
  return {
    videoCodec: value.videoCodec ?? "auto",
    audioBitrateKbps: value.audioBitrateKbps ?? null,
    videoQuality: value.videoQuality ?? "auto",
    encodeSpeed: value.encodeSpeed ?? "auto",
    frameRateCapFps: value.frameRateCapFps ?? null,
    audioChannels: value.audioChannels ?? "auto",
  };
}

/**
 * Fingerprint every request fact bound by the backend Fast Trim consent token
 * or capable of changing compatibility/boundaries. The output destination and
 * accepted consent remain excluded because they are destination-only or
 * derived state.
 */
export function fastTrimRequestFingerprint(request) {
  if (!request || typeof request !== "object") return null;
  const trim = request.trim && typeof request.trim === "object"
    ? {
        startS: request.trim.startS,
        endS: request.trim.endS ?? null,
        mode: request.trim.mode ?? "exact",
      }
    : null;
  const resize = request.resize && typeof request.resize === "object"
    ? {
        mode: request.resize.mode ?? (request.maxEdgePx ? "maxEdge" : "source"),
        maxEdgePx: request.resize.maxEdgePx ?? request.maxEdgePx ?? null,
        widthPx: request.resize.widthPx ?? null,
        heightPx: request.resize.heightPx ?? null,
      }
    : {
        mode: request.maxEdgePx ? "maxEdge" : "source",
        maxEdgePx: request.maxEdgePx ?? null,
        widthPx: null,
        heightPx: null,
      };
  const crop = request.crop && typeof request.crop === "object"
    ? {
        x: request.crop.x,
        y: request.crop.y,
        width: request.crop.width,
        height: request.crop.height,
      }
    : null;
  const color = request.color && typeof request.color === "object"
    ? {
        brightness: request.color.brightness,
        contrast: request.color.contrast,
        saturation: request.color.saturation,
      }
    : null;

  return JSON.stringify({
    inputPath: request.inputPath ?? "",
    format: request.format ?? null,
    title: typeof request.title === "string" ? request.title.trim() : "",
    sizeLimitMb: request.sizeLimitMb ?? 0,
    audioEnabled: request.audioEnabled === true,
    normalizeAudio: request.normalizeAudio === true,
    stripMetadata: request.stripMetadata === true,
    colorPolicy: request.colorPolicy ?? "auto",
    advanced: normalizedAdvanced(request.advanced),
    trim,
    crop,
    reverse: request.reverse === true,
    speed: request.speed ?? 1,
    rotateDeg: request.rotateDeg ?? 0,
    resize,
    color,
    perturbFirstFrame: request.perturbFirstFrame === true,
    loopVideo: request.loopVideo === true,
    strictFit: request.strictFit === true,
    strictFitAllowAudioRemoval: request.strictFitAllowAudioRemoval === true,
    subtitlePath: request.subtitlePath ?? null,
  });
}

export function createFastTrimState() {
  return {
    phase: "idle",
    requestId: 0,
    fingerprint: null,
    inspection: null,
    error: null,
    acceptedConfirmationToken: null,
  };
}

export function beginFastTrimCheck(state, fingerprint, requestId) {
  if (!fingerprint || !Number.isSafeInteger(requestId) || requestId <= 0) return state;
  return {
    phase: "checking",
    requestId,
    fingerprint,
    inspection: null,
    error: null,
    acceptedConfirmationToken: null,
  };
}

export function settleFastTrimCheck(state, fingerprint, requestId, inspection) {
  if (
    state?.phase !== "checking" ||
    state.requestId !== requestId ||
    state.fingerprint !== fingerprint ||
    !inspection ||
    (inspection.status !== "ready" && inspection.status !== "blocked")
  ) {
    return state;
  }
  return {
    ...state,
    phase: inspection.status,
    inspection,
    error: null,
    acceptedConfirmationToken: null,
  };
}

export function failFastTrimCheck(state, fingerprint, requestId, message) {
  if (state?.phase !== "checking" || state.requestId !== requestId || state.fingerprint !== fingerprint) {
    return state;
  }
  return {
    ...state,
    phase: "error",
    inspection: null,
    error: typeof message === "string" && message.trim() ? message.trim() : "Fast Trim inspection failed.",
    acceptedConfirmationToken: null,
  };
}

export function invalidateFastTrimState(state, message = "Settings changed. Check Fast Trim again.") {
  if (!state || !FAST_TRIM_PHASES.has(state.phase)) return createFastTrimState();
  return {
    ...state,
    phase: "stale",
    fingerprint: null,
    inspection: null,
    error: message,
    acceptedConfirmationToken: null,
  };
}

export function acceptFastTrimBounds(state, accepted) {
  const inspection = state?.phase === "ready" ? state.inspection : null;
  const confirmationToken = inspection?.consent?.confirmationToken;
  if (!inspection || inspection.status !== "ready" || !confirmationToken) return state;
  return {
    ...state,
    acceptedConfirmationToken: accepted ? confirmationToken : null,
  };
}

export function fastTrimStateMatches(state, fingerprint) {
  return Boolean(
    state?.phase === "ready" &&
    fingerprint &&
    state.fingerprint === fingerprint &&
    state.inspection?.status === "ready" &&
    state.inspection.consent,
  );
}

export function fastTrimStateForPresentation(state, fingerprint) {
  if (!state || state.phase === "idle" || state.phase === "stale") return state;
  const fingerprintMatches = Boolean(fingerprint && state.fingerprint === fingerprint);
  const current = state.phase === "ready"
    ? fastTrimStateMatches(state, fingerprint)
    : fingerprintMatches;
  if (current) return state;
  return {
    ...state,
    phase: "stale",
    inspection: null,
    error: "Settings changed. Check Fast Trim again.",
    acceptedConfirmationToken: null,
  };
}

export function fastTrimStateIsAccepted(state, fingerprint) {
  if (!fastTrimStateMatches(state, fingerprint)) return false;
  const inspection = state.inspection;
  if (!inspection.requiresAcceptance) return true;
  return state.acceptedConfirmationToken === inspection.consent.confirmationToken;
}

export function fastTrimConsentForRequest(state, fingerprint) {
  return fastTrimStateIsAccepted(state, fingerprint) ? state.inspection.consent : null;
}

export function fastTrimConsentsMatch(left, right) {
  if (!left || !right) return false;
  return left.planSchema === right.planSchema &&
    left.confirmationToken === right.confirmationToken &&
    left.requestedStartUs === right.requestedStartUs &&
    left.requestedEndUs === right.requestedEndUs &&
    left.effectiveStartUs === right.effectiveStartUs &&
    left.effectiveEndUs === right.effectiveEndUs &&
    left.videoPacketCount === right.videoPacketCount;
}

export function fastTrimDurationFromRequest(request) {
  const consent = request?.trim?.mode === "fastCopy" ? request.trim.fastCopyConsent : null;
  const startUs = finiteInteger(consent?.effectiveStartUs);
  const endUs = finiteInteger(consent?.effectiveEndUs);
  if (startUs === null || endUs === null || endUs <= startUs) return null;
  return (endUs - startUs) / 1_000_000;
}

export function fastTrimEffectiveDurationS(inspection) {
  const startUs = finiteInteger(inspection?.effectiveStartUs);
  const endUs = finiteInteger(inspection?.effectiveEndUs);
  if (startUs === null || endUs === null || endUs <= startUs) return null;
  return (endUs - startUs) / 1_000_000;
}

export function formatFastTrimTimeUs(value) {
  const safeUs = finiteInteger(value);
  if (safeUs === null) return "Unavailable";
  const totalMilliseconds = Math.max(0, Math.round(safeUs / 1000));
  const hours = Math.floor(totalMilliseconds / 3_600_000);
  const minutes = Math.floor((totalMilliseconds % 3_600_000) / 60_000);
  const seconds = Math.floor((totalMilliseconds % 60_000) / 1000);
  const milliseconds = totalMilliseconds % 1000;
  const clock = `${String(minutes).padStart(hours ? 2 : 1, "0")}:${String(seconds).padStart(2, "0")}.${String(milliseconds).padStart(3, "0")}`;
  return hours ? `${hours}:${clock}` : clock;
}

export function formatFastTrimDeltaUs(value) {
  const safeUs = finiteInteger(value);
  if (safeUs === null) return "Unavailable";
  const sign = safeUs > 0 ? "+" : safeUs < 0 ? "-" : "";
  return `${sign}${(Math.abs(safeUs) / 1_000_000).toFixed(3)} s`;
}

export function fastTrimModeLabel(mode) {
  return mode === "fastCopy" ? "Fast trim, no re-encode" : "Exact trim";
}

export function summarizeFastTrimInspection(inspection) {
  if (!inspection) return "Fast Trim has not been checked.";
  if (inspection.status === "blocked") {
    return inspection.reasons?.length === 1
      ? inspection.reasons[0].message
      : `${inspection.reasons?.length ?? 0} settings block Fast Trim.`;
  }
  if (!Number.isSafeInteger(inspection.effectiveStartUs) || !Number.isSafeInteger(inspection.effectiveEndUs)) {
    return "Fast Trim returned incomplete boundary evidence.";
  }
  const interval = `${formatFastTrimTimeUs(inspection.effectiveStartUs)} to ${formatFastTrimTimeUs(inspection.effectiveEndUs)}`;
  return inspection.requiresAcceptance
    ? `Expected Fast Trim interval: ${interval}. Accept these expanded boundaries before export.`
    : `Fast Trim is ready for ${interval}; the requested boundaries are already aligned.`;
}

export function summarizeTrimResult(result) {
  if (!result) return null;
  const mode = fastTrimModeLabel(result.mode);
  const effective = `${formatFastTrimTimeUs(result.effectiveStartUs)} to ${formatFastTrimTimeUs(result.effectiveEndUs)}`;
  const actual = `${formatFastTrimTimeUs(result.actualStartUs)} to ${formatFastTrimTimeUs(result.actualEndUs)}`;
  return `${mode}: expected retained source ${effective}; measured retained source ${actual}.`;
}

export function pathFreeFastTrimMessage(error, paths = []) {
  let message = typeof error === "string"
    ? error
    : error instanceof Error
      ? error.message
      : "Fast Trim inspection failed.";
  for (const path of paths) {
    if (typeof path !== "string" || !path) continue;
    message = message.split(path).join("the selected file");
    const basename = path.replaceAll("\\", "/").split("/").pop();
    if (basename) message = message.split(basename).join("the selected file");
  }
  return message.trim() || "Fast Trim inspection failed.";
}
