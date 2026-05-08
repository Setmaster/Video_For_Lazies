import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  ACTIVE_PROGRESS_DISPLAY_CAP,
  FINALIZING_PROGRESS_THRESHOLD,
  clampProgress,
  getActiveProgressUi,
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

test("progress shimmer moves left to right and stops while finalizing", async () => {
  const css = await fs.readFile(path.resolve(repoRoot, "app/src/App.css"), "utf8");

  assert.match(css, /\.vfl-progress-fill\.is-finalizing\s*{[\s\S]*animation:\s*none;[\s\S]*}/);
  assert.match(css, /@keyframes progress-shimmer\s*{[\s\S]*0%\s*{\s*background-position:\s*200%\s+0;\s*}[\s\S]*100%\s*{\s*background-position:\s*0%\s+0;\s*}[\s\S]*}/);
});
