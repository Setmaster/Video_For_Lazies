function validId(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function cloneJson(value) {
  return JSON.parse(JSON.stringify(value));
}

function withFinishedOutcome(base, payload) {
  let outcome = base;
  if (typeof payload.outputPath === "string" && payload.outputPath.length > 0) {
    outcome = { ...outcome, outputPath: payload.outputPath };
  }
  const outputSizeBytes = Number.isFinite(payload.outputSizeBytes) && payload.outputSizeBytes >= 0
    ? payload.outputSizeBytes
    : Number.isFinite(payload.targetResult?.actualBytes) && payload.targetResult.actualBytes >= 0
      ? payload.targetResult.actualBytes
      : null;
  if (outputSizeBytes !== null) {
    outcome = { ...outcome, outputSizeBytes };
  }
  if (payload.targetResult !== null && payload.targetResult !== undefined) {
    outcome = { ...outcome, targetResult: cloneJson(payload.targetResult) };
  }
  if (payload.diagnostics !== null && payload.diagnostics !== undefined) {
    outcome = { ...outcome, diagnostics: cloneJson(payload.diagnostics) };
  }
  if (
    !("message" in outcome) &&
    typeof payload.message === "string" &&
    payload.message.length > 0
  ) {
    outcome = { ...outcome, message: payload.message };
  }
  return outcome;
}

export function createIdleEncodeAttempt() {
  return { kind: "idle" };
}

export function beginEncodeAttempt(attemptId) {
  if (!validId(attemptId)) {
    throw new Error("Encode attempt ID must be a positive safe integer.");
  }
  return { kind: "starting", attemptId };
}

export function bindEncodeAttempt(state, attemptId, jobId) {
  if (
    state?.kind !== "starting" ||
    state.attemptId !== attemptId ||
    !validId(jobId)
  ) {
    return state;
  }
  return { kind: "running", attemptId, jobId };
}

export function bindStartedEncode(pending, state, attemptId, jobId) {
  if (!pending || pending.attemptId !== attemptId || pending.jobId !== null) {
    return { accepted: false, pending, state, context: null };
  }

  const nextState = bindEncodeAttempt(state, attemptId, jobId);
  if (nextState === state) {
    return { accepted: false, pending, state, context: null };
  }

  const nextPending = { ...pending, jobId };
  return {
    accepted: true,
    pending: nextPending,
    state: nextState,
    context: nextPending,
  };
}

export function failEncodeAttemptStart(
  state,
  attemptId,
  message,
  completedAtMs,
  outputPath,
) {
  if (state?.kind !== "starting" || state.attemptId !== attemptId) {
    return state;
  }
  const failed = {
    kind: "failed",
    attemptId,
    jobId: null,
    message: String(message || "Failed to start encode."),
    completedAtMs,
  };
  return typeof outputPath === "string" && outputPath
    ? { ...failed, outputPath }
    : failed;
}

export function requestEncodeCancellation(state, attemptId, jobId) {
  if (
    state?.kind !== "running" ||
    state.attemptId !== attemptId ||
    state.jobId !== jobId
  ) {
    return state;
  }
  return { kind: "cancelling", attemptId, jobId };
}

export function acceptsEncodeEvent(pending, payload) {
  if (!pending || !payload) return false;
  if (pending.attemptId !== payload.attemptId) return false;
  return pending.jobId === null || pending.jobId === payload.jobId;
}

export function finishEncodeAttempt(state, payload, completedAtMs) {
  if (!payload || state?.kind === "idle") return state;
  if (state.attemptId !== payload.attemptId) return state;

  if (
    (state.kind === "running" || state.kind === "cancelling") &&
    state.jobId !== payload.jobId
  ) {
    return state;
  }
  if (!["starting", "running", "cancelling"].includes(state.kind)) {
    return state;
  }

  if (payload.ok) {
    const targetMissed = payload.targetResult?.status === "missed";
    return withFinishedOutcome({
      kind: targetMissed ? "target-missed" : "succeeded",
      attemptId: payload.attemptId,
      jobId: payload.jobId,
      ...(targetMissed
        ? { message: String(payload.message || "Export completed, but missed the requested size target.") }
        : {}),
      completedAtMs,
    }, payload);
  }

  const message = String(payload.message || "Encode failed.");
  if (state.kind === "cancelling") {
    return withFinishedOutcome({
      kind: "cancelled",
      attemptId: payload.attemptId,
      jobId: payload.jobId,
      message,
      completedAtMs,
    }, payload);
  }
  return withFinishedOutcome({
    kind: "failed",
    attemptId: payload.attemptId,
    jobId: payload.jobId,
    message,
    completedAtMs,
  }, payload);
}

export function settleEncodeFinished(pending, state, payload, completedAtMs) {
  if (!acceptsEncodeEvent(pending, payload)) {
    return { accepted: false, pending, state, context: null };
  }

  const nextState = finishEncodeAttempt(state, payload, completedAtMs);
  if (nextState === state) {
    return { accepted: false, pending, state, context: null };
  }

  const settledState =
    !("outputPath" in nextState) && typeof pending?.outputPath === "string" && pending.outputPath
      ? { ...nextState, outputPath: pending.outputPath }
      : nextState;
  return {
    accepted: true,
    pending: null,
    state: settledState,
    context: pending,
  };
}

export function deriveEncodeAttemptPresentation(state) {
  switch (state?.kind) {
    case "starting":
      return {
        isActive: true,
        isSuccess: false,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: false,
        kicker: "Starting export",
        summary: "Starting",
        message: "Starting export…",
      };
    case "running":
      return {
        isActive: true,
        isSuccess: false,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: false,
        kicker: "Encoding now",
        summary: "Encoding",
        message: "Encoding…",
      };
    case "cancelling":
      return {
        isActive: true,
        isSuccess: false,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: false,
        kicker: "Canceling export",
        summary: "Canceling",
        message: "Canceling…",
      };
    case "succeeded":
      return {
        isActive: false,
        isSuccess: true,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: false,
        kicker: "Export complete",
        summary: "Done",
        message: "Last export completed.",
      };
    case "target-missed":
      return {
        isActive: false,
        isSuccess: false,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: true,
        kicker: "Size target missed",
        summary: "Target missed",
        message: state.message,
      };
    case "failed":
      return {
        isActive: false,
        isSuccess: false,
        isFailure: true,
        isCancelled: false,
        isTargetMissed: false,
        kicker: "Export failed",
        summary: "Failed",
        message: state.message,
      };
    case "cancelled":
      return {
        isActive: false,
        isSuccess: false,
        isFailure: false,
        isCancelled: true,
        isTargetMissed: false,
        kicker: "Export canceled",
        summary: "Canceled",
        message: state.message,
      };
    default:
      return {
        isActive: false,
        isSuccess: false,
        isFailure: false,
        isCancelled: false,
        isTargetMissed: false,
        kicker: null,
        summary: null,
        message: null,
      };
  }
}
