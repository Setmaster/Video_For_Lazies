function validId(value) {
  return Number.isSafeInteger(value) && value > 0;
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
    return {
      kind: "succeeded",
      attemptId: payload.attemptId,
      jobId: payload.jobId,
      completedAtMs,
    };
  }

  const message = String(payload.message || "Encode failed.");
  if (state.kind === "cancelling") {
    return {
      kind: "cancelled",
      attemptId: payload.attemptId,
      jobId: payload.jobId,
      message,
      completedAtMs,
    };
  }
  return {
    kind: "failed",
    attemptId: payload.attemptId,
    jobId: payload.jobId,
    message,
    completedAtMs,
  };
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
    typeof pending?.outputPath === "string" && pending.outputPath
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
        kicker: "Export complete",
        summary: "Done",
        message: "Last export completed.",
      };
    case "failed":
      return {
        isActive: false,
        isSuccess: false,
        isFailure: true,
        isCancelled: false,
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
        kicker: null,
        summary: null,
        message: null,
      };
  }
}
