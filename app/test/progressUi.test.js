import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  ACTIVE_PROGRESS_DISPLAY_CAP,
  FINALIZING_PROGRESS_THRESHOLD,
  clampProgress,
  createEncodeProgressState,
  getActiveProgressUi,
  reduceEncodeProgress,
} from "../src/lib/progress.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const repoRoot = path.resolve(__dirname, "../..");

test("active progress stays below 100 percent until the finished event", () => {
  const state = getActiveProgressUi(1, true);

  assert.equal(ACTIVE_PROGRESS_DISPLAY_CAP, 0.99);
  assert.equal(FINALIZING_PROGRESS_THRESHOLD, 0.999);
  assert.equal(state.value, 0.99);
  assert.equal(state.percent, 99);
  assert.equal(state.isFinalizing, true);
  assert.equal(state.label, "Finalizing output");
});

test("inactive completed progress can render as 100 percent", () => {
  const state = getActiveProgressUi(1, false);

  assert.equal(state.value, 1);
  assert.equal(state.percent, 100);
  assert.equal(state.isFinalizing, false);
  assert.equal(state.label, "Progress");
});

test("progress inputs clamp to the supported display range", () => {
  assert.equal(clampProgress(-0.25), 0);
  assert.equal(clampProgress(1.25), 1);
  assert.equal(clampProgress(Number.NaN), 0);
});

test("progress reducer stays monotonic across copying, fallback encoding, and finalization", () => {
  let state = createEncodeProgressState();
  state = reduceEncodeProgress(state, {
    attemptId: 10, jobId: 20, phase: "copying", stepIndex: 1, stepCount: 4,
    pass: 1, totalPasses: 1, passPct: 1, overallPct: 0.25,
  });
  state = reduceEncodeProgress(state, {
    attemptId: 10, jobId: 20, phase: "encoding", stepIndex: 2, stepCount: 4,
    pass: 1, totalPasses: 2, passPct: 0.1, overallPct: 0.2,
  });
  assert.equal(state.overallPct, 0.25);
  assert.equal(state.phase, "encoding");

  state = reduceEncodeProgress(state, {
    attemptId: 10, jobId: 20, phase: "finalizing", stepIndex: 2, stepCount: 4,
    pass: 1, totalPasses: 2, passPct: 1, overallPct: 0.5,
  });
  const ui = getActiveProgressUi(state, true);
  assert.equal(ui.isFinalizing, true);
  assert.equal(ui.label, "Finalizing output");
  assert.match(ui.valueText, /step 2 of 4, 50 percent/);
});

test("progress reducer rejects stale attempt and mismatched job events", () => {
  const current = reduceEncodeProgress(createEncodeProgressState(), {
    attemptId: 10, jobId: 20, phase: "encoding", stepIndex: 1, stepCount: 2,
    pass: 1, totalPasses: 1, passPct: 0.5, overallPct: 0.25,
  });
  assert.equal(reduceEncodeProgress(current, { ...current, attemptId: 9, overallPct: 1 }), current);
  assert.equal(reduceEncodeProgress(current, { ...current, jobId: 21, overallPct: 1 }), current);
});

test("finalizing progress is visually distinct without a looping animation", async () => {
  const css = await fs.readFile(path.resolve(repoRoot, "app/src/App.css"), "utf8");

  // Workbench theme: the fill is a static gradient (no infinite shimmer), and
  // the finalizing phase switches to its own color so 99% reads differently.
  assert.match(css, /\.vfl-progress-fill\s*{[\s\S]*?background:[\s\S]*?}/);
  assert.match(css, /\.vfl-progress-fill\.is-finalizing\s*{[\s\S]*?background:[\s\S]*?}/);
  assert.doesNotMatch(css, /animation:[^;]*infinite/);
});
