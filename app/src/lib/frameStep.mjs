// One native request and one verified media seek at a time. Logical PTS stays
// separate from the small interior display-seek bias used by the preview.
export function createFrameStepper({ read, fetchStep, pause, seek, apply, status, cancelSeek }) {
  let generation = 0;
  let running = false;
  let queue = [];
  let logical = null;
  const valid = (entry) => {
    const state = read();
    return entry.generation === generation && state?.enabled && !state.blocked &&
      state.key === entry.key && state.activeTarget === entry.target;
  };
  async function drain() {
    if (running) return;
    running = true;
    try {
      while (queue.length) {
        const entry = queue.shift();
        if (!valid(entry)) continue;
        const state = read();
        const timeS = logical?.key === entry.key && logical.target === entry.target
          ? logical.timeS : state.values[entry.target];
        status({ pending: true, error: false, message: "Reading source frame…" });
        try {
          const result = await fetchStep({
            path: state.path, sourceIdentity: state.sourceIdentity,
            videoStreamIndex: state.videoStreamIndex, durationS: state.durationS,
            timeS, step: entry.step,
            minS: state.limits[entry.target].min, maxS: state.limits[entry.target].max,
          });
          if (!valid(entry)) continue;
          if (result.sourceIdentity !== state.sourceIdentity || !Number.isFinite(result.timeS) || !Number.isFinite(result.frameEndS)) {
            throw new Error("Source frame timing changed. Reload the video and try again.");
          }
          if (!result.didMove) {
            status({ pending: false, error: false, message: entry.target === "preview" ? "At the source frame limit." : "At the trim limit." });
            continue;
          }
          if (entry.step > 0 ? result.timeS <= timeS : result.timeS >= timeS) {
            throw new Error("The source frame did not advance. Reload the video and try again.");
          }
          const limits = read().limits[entry.target];
          if (result.timeS < limits.min || result.timeS > limits.max || result.timeS >= state.durationS || result.frameEndS <= result.timeS) {
            throw new Error("The next source frame is outside the current trim range.");
          }
          await seek(result.timeS, result.frameEndS);
          if (!valid(entry)) continue;
          apply(entry.target, result.timeS);
          logical = { key: entry.key, target: entry.target, timeS: result.timeS };
          status({ pending: false, error: false, message: result.atLimit ? "Reached the frame limit." : "Source frame selected." });
        } catch (error) {
          if (!valid(entry)) continue;
          queue = [];
          logical = null;
          status({ pending: false, error: true, message: `Frame stepping unavailable: ${error instanceof Error ? error.message : String(error)}` });
        }
      }
    } finally { running = false; }
  }
  return {
    enqueue(target, step) {
      const state = read();
      if (!state?.enabled || state.blocked || ![-10, -1, 1, 10].includes(step)) return false;
      pause();
      if (queue.length >= 16) {
        status({ pending: true, error: false, message: "Frame stepping is busy. Release the arrow key." });
        return true;
      }
      queue.push({ target, step, key: state.key, generation });
      void drain();
      return true;
    },
    reset() {
      generation += 1;
      queue = [];
      logical = null;
      cancelSeek?.();
      status({ pending: false, error: false, message: null });
    },
  };
}

export function frameDisplaySeek(timeS, endS) {
  const span = endS - timeS;
  if (![timeS, endS].every(Number.isFinite) || span < 0.00002) {
    throw new Error("This preview cannot distinguish these source frame times.");
  }
  return { seekS: timeS + Math.min(span / 100, 0.001), toleranceS: Math.min(0.000002, span / 10) };
}
