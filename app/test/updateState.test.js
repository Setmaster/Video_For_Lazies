import test from "node:test";
import assert from "node:assert/strict";

import {
  createUpdateProgressState,
  getUpdateProgressUi,
  reduceUpdateProgress,
} from "../src/lib/updateState.mjs";

test("update progress rejects stale operation events and keeps download bytes monotonic", () => {
  let state = reduceUpdateProgress(createUpdateProgressState(), {
    operationId: "apply-1",
    phase: "downloading",
    completedBytes: 500,
    totalBytes: 1_000,
    message: "Downloading update...",
  });
  state = reduceUpdateProgress(state, {
    operationId: "apply-1",
    phase: "downloading",
    completedBytes: 400,
    totalBytes: 1_000,
  });
  assert.equal(state.completedBytes, 500);
  assert.equal(reduceUpdateProgress(state, {
    operationId: "stale-2",
    phase: "extracting",
  }), state);
  assert.deepEqual(getUpdateProgressUi(state), {
    phase: "downloading",
    label: "Downloading update",
    determinate: true,
    percent: 50,
    valueText: "Downloading update, 500 of 1,000 bytes, 50 percent",
  });
});

test("only download bytes create determinate progress and live text stays path-free", () => {
  const state = reduceUpdateProgress(createUpdateProgressState(), {
    operationId: "apply-1",
    phase: "verifyingArchive",
    completedBytes: 500,
    totalBytes: 1_000,
    message: "Verifying C:\\Users\\private\\download.zip",
  });

  assert.deepEqual(getUpdateProgressUi(state), {
    phase: "verifyingArchive",
    label: "Verifying archive",
    determinate: false,
    percent: null,
    valueText: "Verifying archive",
  });
});

test("a completed operation permits the next update operation", () => {
  const completed = reduceUpdateProgress(createUpdateProgressState(), {
    operationId: "check-1",
    phase: "completed",
  });
  const next = reduceUpdateProgress(completed, {
    operationId: "apply-2",
    phase: "checking",
  });
  assert.equal(next.operationId, "apply-2");
  assert.equal(next.phase, "checking");
  assert.equal(getUpdateProgressUi(next).determinate, false);
});

test("invalid update progress is ignored without inventing percentages", () => {
  const initial = createUpdateProgressState();
  assert.equal(reduceUpdateProgress(initial, { operationId: "x", phase: "unknown" }), initial);
  assert.equal(getUpdateProgressUi(initial).percent, null);
});
