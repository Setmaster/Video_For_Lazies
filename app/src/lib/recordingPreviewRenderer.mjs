// Poll ahead of the 30 Hz producer so scheduler jitter cannot turn equal-rate
// producer/consumer sampling into skipped frames. Empty responses carry no BMP.
export function createPreviewPollCadence(interval = 1000 / 60) {
  let next = null;
  return (started, finished) => {
    const scheduled = next ?? started;
    next = started - scheduled >= interval / 2 ? started + interval : scheduled + interval;
    return Math.max(0, next - finished);
  };
}

// A single decoded frame can wait for presentation. Neither IPC nor React owns
// a queue of images, and every bitmap is released after drawing or invalidation.
export function createRecordingPreviewRenderer({
  isCurrent, onFirstFrame, onError,
  decode = (bytes) => createImageBitmap(new Blob([bytes], { type: "image/bmp" })),
  requestFrame = (callback) => requestAnimationFrame(callback),
  cancelFrame = (handle) => cancelAnimationFrame(handle),
  now = () => performance.now(),
}) {
  let canvas = null, pending = null, scheduled = null, decoding = false;
  let generation = 0, sequence = 0, count = 0, dropped = 0, identity = null;
  const current = (job) => job.generation === generation && isCurrent(job.frame);
  const telemetry = (frame, decodeMs, drawMs, presentedAt) => {
    const data = canvas.dataset;
    data.previewId = frame.previewId;
    data.previewEpoch = String(frame.configEpoch);
    data.previewSequence = String(frame.sequence);
    data.previewCapturedAtQpc100ns = frame.capturedAtQpc100ns;
    data.previewPresentedAtMs = String(presentedAt);
    data.previewPresentedCount = String(count);
    data.previewDroppedCount = String(dropped);
    data.previewDecodeMs = String(decodeMs);
    data.previewDrawMs = String(drawMs);
    const container = canvas.closest(".vfl-record-preview");
    if (container) container.dataset.previewSequence = String(frame.sequence);
  };
  const paint = () => {
    scheduled = null;
    const job = pending;
    pending = null;
    if (!job) return;
    try {
      if (!canvas || !current(job) || job.frame.sequence <= sequence) return;
      const context = canvas.getContext("2d", { alpha: false });
      if (!context) throw new Error("The recording preview canvas is unavailable.");
      if (canvas.width !== job.frame.width) canvas.width = job.frame.width;
      if (canvas.height !== job.frame.height) canvas.height = job.frame.height;
      const started = now();
      context.drawImage(job.bitmap, 0, 0);
      const presentedAt = now();
      sequence = job.frame.sequence;
      count += 1;
      telemetry(job.frame, job.decodeMs, presentedAt - started, presentedAt);
      const nextIdentity = `${job.frame.previewId}:${job.frame.configEpoch}`;
      if (identity !== nextIdentity) {
        identity = nextIdentity;
        onFirstFrame(job.frame);
      }
    } catch (failure) {
      if (current(job)) onError(failure);
    } finally {
      job.bitmap.close();
    }
  };
  const clear = () => {
    generation += 1;
    if (scheduled !== null) cancelFrame(scheduled);
    scheduled = null;
    if (pending) pending.bitmap.close();
    pending = null;
    sequence = 0; count = 0; dropped = 0; identity = null;
    if (canvas) {
      // Clearing must happen synchronously, before a React visibility commit.
      canvas.width = 1; canvas.height = 1;
      for (const key of Object.keys(canvas.dataset)) {
        if (key.startsWith("preview")) delete canvas.dataset[key];
      }
      canvas.dataset.previewSequence = "0";
      canvas.dataset.previewPresentedCount = "0";
      const container = canvas.closest(".vfl-record-preview");
      if (container) container.dataset.previewSequence = "0";
    }
  };
  return {
    clear,
    setCanvas(value) {
      if (canvas === value) return;
      clear();
      canvas = value;
    },
    afterSequence() { return Math.max(sequence, pending?.frame.sequence ?? 0); },
    async offer(frame) {
      if (decoding || !canvas || !isCurrent(frame) || frame.sequence <= Math.max(sequence, pending?.frame.sequence ?? 0)) return false;
      decoding = true;
      const token = generation, started = now();
      let bitmap = null;
      try {
        bitmap = await decode(frame.bmp);
        const job = { frame, bitmap, generation: token, decodeMs: now() - started };
        if (!current(job) || !canvas) return false;
        if (pending) { pending.bitmap.close(); dropped += 1; }
        pending = job;
        bitmap = null;
        if (scheduled === null) scheduled = requestFrame(paint);
        return true;
      } catch (failure) {
        if (token === generation && isCurrent(frame)) onError(failure);
        return false;
      } finally {
        bitmap?.close();
        decoding = false;
      }
    },
  };
}
