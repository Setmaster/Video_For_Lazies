function safeUnlisten(unlisten) {
  if (typeof unlisten !== "function") return;
  try {
    const result = unlisten();
    if (result && typeof result.then === "function") {
      void Promise.resolve(result).catch(() => {
        // Listener teardown is best-effort and must not become an unhandled rejection.
      });
    }
  } catch {
    // Listener teardown is best-effort and must not mask the original state.
  }
}

export const ENCODE_EVENT_REGISTRATION_TIMEOUT_MS = 10_000;

const defaultScheduler = Object.freeze({
  setTimeout(callback, delayMs) {
    return globalThis.setTimeout(callback, delayMs);
  },
  clearTimeout(handle) {
    globalThis.clearTimeout(handle);
  },
});

export function installEncodeEventListeners({
  subscribeFinished,
  subscribeProgress,
  onFinished,
  onProgress,
  onReady,
  onError,
  registrationTimeoutMs = ENCODE_EVENT_REGISTRATION_TIMEOUT_MS,
  scheduler = defaultScheduler,
}) {
  let state = "installing";
  let unlistenFinished = null;
  let unlistenProgress = null;
  let deadlineActive = false;
  let deadlineHandle = null;
  let readySettled = false;
  let resolveReady;

  const ready = new Promise((resolve) => {
    resolveReady = resolve;
  });

  const settleReady = (value) => {
    if (readySettled) return;
    readySettled = true;
    resolveReady(value);
  };

  const clearDeadline = () => {
    if (!deadlineActive) return;
    deadlineActive = false;
    try {
      scheduler.clearTimeout(deadlineHandle);
    } catch {
      // Registration state must remain deterministic if scheduler cleanup fails.
    }
    deadlineHandle = null;
  };

  const cleanup = () => {
    const finished = unlistenFinished;
    const progress = unlistenProgress;
    unlistenFinished = null;
    unlistenProgress = null;
    safeUnlisten(progress);
    safeUnlisten(finished);
  };

  const failRegistration = (error) => {
    if (state !== "installing") return;
    state = "failed";
    clearDeadline();
    cleanup();
    settleReady(false);
    try {
      onError(error);
    } catch {
      // Readiness has already failed closed; callback errors cannot reopen it.
    }
  };

  const requestedTimeoutMs = Number(registrationTimeoutMs);
  const timeoutMs = Number.isFinite(requestedTimeoutMs) && requestedTimeoutMs > 0
    ? requestedTimeoutMs
    : ENCODE_EVENT_REGISTRATION_TIMEOUT_MS;

  try {
    deadlineHandle = scheduler.setTimeout(() => {
      failRegistration(new Error("Export event listeners did not become ready in time."));
    }, timeoutMs);
    deadlineActive = true;
  } catch (error) {
    failRegistration(error);
  }

  if (state === "installing") void (async () => {
    try {
      const finished = await subscribeFinished(onFinished);
      if (state !== "installing") {
        safeUnlisten(finished);
        return;
      }
      unlistenFinished = finished;

      const progress = await subscribeProgress(onProgress);
      if (state !== "installing") {
        safeUnlisten(progress);
        return;
      }
      unlistenProgress = progress;
      onReady();
      if (state !== "installing") return;
      state = "ready";
      clearDeadline();
      settleReady(true);
    } catch (error) {
      failRegistration(error);
    }
  })();

  return {
    ready,
    dispose() {
      if (state === "disposed") return;
      state = "disposed";
      clearDeadline();
      cleanup();
      settleReady(false);
    },
  };
}
