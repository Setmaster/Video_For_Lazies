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

export function installEncodeEventListeners({
  subscribeFinished,
  subscribeProgress,
  onFinished,
  onProgress,
  onReady,
  onError,
}) {
  let disposed = false;
  let unlistenFinished = null;
  let unlistenProgress = null;

  const cleanup = () => {
    const finished = unlistenFinished;
    const progress = unlistenProgress;
    unlistenFinished = null;
    unlistenProgress = null;
    safeUnlisten(progress);
    safeUnlisten(finished);
  };

  const ready = (async () => {
    try {
      const finished = await subscribeFinished(onFinished);
      if (disposed) {
        safeUnlisten(finished);
        return false;
      }
      unlistenFinished = finished;

      const progress = await subscribeProgress(onProgress);
      if (disposed) {
        safeUnlisten(progress);
        cleanup();
        return false;
      }
      unlistenProgress = progress;
      onReady();
      return true;
    } catch (error) {
      cleanup();
      if (!disposed) onError(error);
      return false;
    }
  })();

  return {
    ready,
    dispose() {
      disposed = true;
      cleanup();
    },
  };
}
