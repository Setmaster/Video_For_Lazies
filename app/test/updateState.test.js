import test from "node:test";
import assert from "node:assert/strict";

import {
  createUpdateProgressState,
  finalizeStartupOnce,
  getUpdateProgressUi,
  reduceUpdateProgress,
} from "../src/lib/updateState.mjs";

test("startup effect replay shares the native lease and only its live subscriber proceeds", async () => {
  const holder = { current: null };
  let starts = 0;
  let finish;
  const start = () => {
    assert.equal(++starts, 1, "overlapping startup commands would be rejected by native admission");
    return new Promise((resolve) => { finish = resolve; });
  };
  let firstStopped = false;
  const proceeded = [];
  const first = finalizeStartupOnce(holder, start);
  const discardedEffect = first.then(() => { if (!firstStopped) proceeded.push("discarded"); });
  firstStopped = true;
  const replay = finalizeStartupOnce(holder, start);
  const liveEffect = replay.then(() => proceeded.push("live"));
  assert.equal(first, replay);
  await Promise.resolve();
  assert.equal(starts, 1);
  finish({ status: "none", message: "" });
  await Promise.all([discardedEffect, liveEffect]);
  assert.deepEqual(proceeded, ["live"]);
  assert.equal(finalizeStartupOnce(holder, start), first);
  assert.equal(starts, 1);
});

test("startup failure is shared without swallowing it or retrying native finalization", async () => {
  const holder = { current: null };
  const failure = new Error("Saved update state needs recovery");
  let starts = 0;
  const start = () => { starts += 1; throw failure; };
  const first = finalizeStartupOnce(holder, start);
  const replay = finalizeStartupOnce(holder, start);
  const outcomes = await Promise.allSettled([first, replay]);
  for (const outcome of outcomes) {
    assert.equal(outcome.status, "rejected");
    assert.equal(outcome.reason, failure);
  }
  await assert.rejects(finalizeStartupOnce(holder, start), (error) => error === failure);
  assert.equal(starts, 1);
});

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
