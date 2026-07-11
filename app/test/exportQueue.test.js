import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_EXPORT_QUEUE_ITEMS,
  MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM,
  createExportQueueState,
  exportQueueClaimsOutputPath,
  exportQueueOutputPaths,
  exportQueueRemainingCapacity,
  getActiveExportQueueItem,
  getNextQueuedExportItem,
  queuePathIdentity,
  reduceExportQueue,
  stableUniqueQueuePaths,
  summarizeExportQueue,
} from "../src/lib/exportQueue.mjs";

function request(name, overrides = {}) {
  return {
    inputPath: `/inputs/${name}.mp4`,
    outputPath: `/outputs/${name}-2.mp4`,
    format: "mp4",
    title: null,
    sizeLimitMb: 0,
    audioEnabled: true,
    normalizeAudio: false,
    stripMetadata: true,
    colorPolicy: "auto",
    advanced: {
      videoCodec: "auto",
      audioBitrateKbps: null,
      videoQuality: "auto",
      encodeSpeed: "auto",
      frameRateCapFps: null,
      audioChannels: "auto",
    },
    trim: null,
    crop: null,
    reverse: false,
    speed: 1,
    rotateDeg: 0,
    resize: { mode: "source", maxEdgePx: null, widthPx: null, heightPx: null },
    maxEdgePx: null,
    color: null,
    perturbFirstFrame: false,
    loopVideo: false,
    ...overrides,
  };
}

function enqueue(state, ...requests) {
  return reduceExportQueue(state, {
    type: "enqueue-prepared",
    items: requests.map((nextRequest, index) => ({
      request: nextRequest,
      durationS: index + 1,
    })),
  });
}

function beginNext(state) {
  return reduceExportQueue(
    reduceExportQueue(state, { type: "start-auto-run" }),
    { type: "claim-next" },
  );
}

test("queue state defaults to a bounded empty state", () => {
  const state = createExportQueueState();

  assert.deepEqual(state, {
    items: [],
    nextItemId: 1,
    nextRunId: 1,
    autoRun: false,
    active: null,
    pathRevision: 0,
    maxItems: MAX_EXPORT_QUEUE_ITEMS,
  });
  assert.equal(exportQueueRemainingCapacity(state), 100);
  assert.equal(createExportQueueState({ maxItems: 1000 }).maxItems, 100);
  assert.equal(createExportQueueState({ maxItems: 2 }).maxItems, 2);
});

test("path identity is deterministic for Windows and case-sensitive for POSIX", () => {
  assert.equal(
    queuePathIdentity("C:\\Videos\\Clip.MP4"),
    queuePathIdentity("c:/videos/clip.mp4"),
  );
  assert.equal(
    queuePathIdentity("\\\\Server\\Share\\Clip.mp4", "windows"),
    queuePathIdentity("//server/share/clip.mp4", "windows"),
  );
  assert.notEqual(
    queuePathIdentity("/videos/Clip.mp4", "posix"),
    queuePathIdentity("/videos/clip.mp4", "posix"),
  );
  assert.equal(queuePathIdentity("", "posix"), null);
});

test("auto path identity recognizes forward-slash Windows UNC paths", () => {
  const backslashUnc = "\\\\Server\\Share\\Clip.mp4";
  const fileUriUnc = "//server/share/clip.mp4";

  assert.equal(
    queuePathIdentity(backslashUnc),
    queuePathIdentity(fileUriUnc),
  );
  assert.deepEqual(
    stableUniqueQueuePaths([backslashUnc, fileUriUnc]),
    {
      paths: [backslashUnc],
      duplicateCount: 1,
      invalidCount: 0,
    },
  );
});

test("explicit POSIX identity preserves double-slash spelling and case", () => {
  assert.equal(
    queuePathIdentity("//Server/Share/Clip.mp4", "posix"),
    "posix://Server/Share/Clip.mp4",
  );
  assert.notEqual(
    queuePathIdentity("//Server/Share/Clip.mp4", "posix"),
    queuePathIdentity("//server/share/clip.mp4", "posix"),
  );
  assert.deepEqual(
    stableUniqueQueuePaths([
      "//Server/Share/Clip.mp4",
      "//server/share/clip.mp4",
    ], "posix"),
    {
      paths: ["//Server/Share/Clip.mp4", "//server/share/clip.mp4"],
      duplicateCount: 0,
      invalidCount: 0,
    },
  );
});

test("stable path de-duplication preserves first occurrence order", () => {
  assert.deepEqual(
    stableUniqueQueuePaths([
      "C:\\Videos\\First.mp4",
      "c:/videos/first.mp4",
      "C:\\Videos\\Second.mp4",
      "",
      null,
    ], "windows"),
    {
      paths: ["C:\\Videos\\First.mp4", "C:\\Videos\\Second.mp4"],
      duplicateCount: 1,
      invalidCount: 2,
    },
  );
});

test("enqueue assigns monotonic IDs, clones requests, and enforces capacity", () => {
  const first = request("first", { advanced: { videoCodec: "h264" } });
  const second = request("second");
  const third = request("third");
  const state = enqueue(createExportQueueState({ maxItems: 2 }), first, second, third);

  first.advanced.videoCodec = "vp9";
  first.outputPath = "/mutated.mp4";

  assert.equal(state.items.length, 2);
  assert.deepEqual(state.items.map((item) => item.id), [1, 2]);
  assert.deepEqual(state.items.map((item) => item.status), ["queued", "queued"]);
  assert.equal(state.items[0].request.advanced.videoCodec, "h264");
  assert.equal(state.items[0].outputPath, "/outputs/first-2.mp4");
  assert.equal(state.nextItemId, 3);
  assert.equal(state.pathRevision, 1);
  assert.equal(exportQueueRemainingCapacity(state), 0);
  assert.equal(getNextQueuedExportItem(state).id, 1);
});

test("the default queue accepts at most one hundred visible items", () => {
  const requests = Array.from({ length: 101 }, (_, index) => request(`item-${index}`));
  const state = enqueue(createExportQueueState(), ...requests);

  assert.equal(state.items.length, 100);
  assert.equal(state.nextItemId, 101);
  assert.equal(exportQueueRemainingCapacity(state), 0);
});

test("output path helpers include all visible item claims", () => {
  const state = enqueue(
    createExportQueueState(),
    request("one", { outputPath: "C:\\Videos\\Result.MP4" }),
    request("two", { outputPath: "/outputs/two.webm", format: "webm" }),
  );

  assert.deepEqual(exportQueueOutputPaths(state), [
    "C:\\Videos\\Result.MP4",
    "/outputs/two.webm",
  ]);
  assert.equal(exportQueueClaimsOutputPath(state, "c:/videos/result.mp4"), true);
  assert.equal(exportQueueClaimsOutputPath(state, "/outputs/Two.webm", "posix"), false);
  assert.equal(exportQueueClaimsOutputPath(state, "c:/videos/result.mp4", "windows", 1), false);
});

test("output claims match equivalent auto-detected Windows UNC spellings", () => {
  const state = enqueue(
    createExportQueueState(),
    request("unc", { outputPath: "\\\\Server\\Share\\Result.MP4" }),
  );

  assert.equal(exportQueueClaimsOutputPath(state, "//server/share/result.mp4"), true);
  assert.equal(exportQueueClaimsOutputPath(state, "//server/share/result.mp4", "posix"), false);
});

test("claim and settle use exact item and run identity", () => {
  let state = enqueue(createExportQueueState(), request("one"), request("two"));
  state = beginNext(state);

  assert.deepEqual(state.active, { itemId: 1, runId: 1 });
  assert.equal(getActiveExportQueueItem(state).status, "running");
  assert.equal(summarizeExportQueue(state).running, 1);

  const alreadyClaimed = reduceExportQueue(state, { type: "claim-next" });
  assert.equal(alreadyClaimed, state);
  assert.equal(
    reduceExportQueue(state, {
      type: "settled",
      itemId: 1,
      runId: 99,
      outcome: { kind: "done" },
    }),
    state,
  );
  assert.equal(
    reduceExportQueue(state, {
      type: "settled",
      itemId: 2,
      runId: 1,
      outcome: { kind: "done" },
    }),
    state,
  );

  const diagnostics = { mode: "Stream copy", commandPreview: "ffmpeg <input> <output>" };
  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: {
      kind: "done",
      outputSizeBytes: 1234,
      diagnostics,
      completedAtMs: 500,
    },
  });
  diagnostics.mode = "mutated";

  assert.equal(state.active, null);
  assert.equal(state.items[0].status, "done");
  assert.equal(state.items[0].lastOutcome.outputPath, "/outputs/one-2.mp4");
  assert.equal(state.items[0].lastOutcome.outputSizeBytes, 1234);
  assert.equal(state.items[0].lastOutcome.diagnostics.mode, "Stream copy");
  assert.equal(state.items[0].history.length, 1);

  const duplicateFinish = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: { kind: "failed", message: "late" },
  });
  assert.equal(duplicateFinish, state);

  state = reduceExportQueue(state, { type: "claim-next" });
  assert.deepEqual(state.active, { itemId: 2, runId: 2 });
  assert.equal(state.items[1].status, "running");
});

test("target-missed settlement retains the artifact and cloned target diagnostics", () => {
  let state = beginNext(enqueue(createExportQueueState(), request("missed")));
  const targetResult = {
    status: "missed",
    targetBytes: 1_000,
    actualBytes: 1_125,
    overshootBytes: 125,
    strictFit: true,
    selectedPlanNumber: 4,
    plans: [{ planNumber: 4, status: "missed", actualSizeBytes: 1_125 }],
  };
  const diagnostics = {
    mode: "Strict Fit",
    commandPreview: "ffmpeg <input> <output>",
    attempts: 4,
  };

  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: {
      kind: "target-missed",
      message: "The smallest successful artifact is still 125 bytes over target.",
      outputSizeBytes: 1_125,
      targetResult,
      diagnostics,
      completedAtMs: 700,
    },
  });
  targetResult.plans[0].actualSizeBytes = 9_999;
  diagnostics.mode = "mutated";

  assert.equal(state.active, null);
  assert.equal(state.items[0].status, "target-missed");
  assert.equal(state.items[0].lastOutcome.kind, "target-missed");
  assert.equal(state.items[0].lastOutcome.outputPath, "/outputs/missed-2.mp4");
  assert.equal(state.items[0].lastOutcome.outputSizeBytes, 1_125);
  assert.equal(state.items[0].lastOutcome.targetResult.plans[0].actualSizeBytes, 1_125);
  assert.equal(state.items[0].lastOutcome.diagnostics.mode, "Strict Fit");
  assert.deepEqual(summarizeExportQueue(state), {
    total: 1,
    queued: 0,
    running: 0,
    done: 0,
    missed: 1,
    failed: 0,
    cancelled: 0,
  });

  const cleared = reduceExportQueue(state, { type: "clear-terminal" });
  assert.equal(cleared.items.length, 0);
  assert.equal(cleared.pathRevision, state.pathRevision + 1);
});

test("target-missed items can retry or duplicate without accepting stale run settlement", () => {
  let state = beginNext(enqueue(createExportQueueState(), request("missed-retry")));
  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: {
      // Compatibility guard: a legacy caller cannot misclassify an exact miss as done.
      kind: "done",
      targetResult: {
        status: "missed",
        targetBytes: 1_000,
        actualBytes: 1_125,
        overshootBytes: 125,
      },
    },
  });

  state = reduceExportQueue(state, {
    type: "duplicate-prepared",
    sourceItemId: 1,
    request: request("missed-copy", { outputPath: "/outputs/missed-copy.mp4" }),
  });
  assert.equal(state.items[1].status, "queued");
  assert.equal(state.items[1].history.length, 0);
  assert.equal(state.items[1].lastOutcome, null);

  state = reduceExportQueue(state, {
    type: "retry-prepared",
    itemId: 1,
    request: request("missed-retry", { outputPath: "/outputs/missed-retry-3.mp4" }),
  });
  assert.equal(state.items[0].status, "queued");
  assert.equal(state.items[0].history.length, 1);
  assert.equal(state.items[0].lastOutcome.kind, "target-missed");
  assert.equal(state.items[0].lastOutcome.outputSizeBytes, 1_125);

  state = reduceExportQueue(state, { type: "claim-next" });
  assert.deepEqual(state.active, { itemId: 1, runId: 2 });
  const currentRun = state;
  assert.equal(
    reduceExportQueue(state, {
      type: "settled",
      itemId: 1,
      runId: 1,
      outcome: { kind: "done" },
    }),
    currentRun,
  );
});

test("stop after current preserves the active item and prevents another claim", () => {
  let state = beginNext(enqueue(createExportQueueState(), request("one"), request("two")));
  state = reduceExportQueue(state, { type: "stop-auto-run" });

  assert.equal(state.autoRun, false);
  assert.deepEqual(state.active, { itemId: 1, runId: 1 });

  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: { kind: "cancelled", message: "Canceled.", completedAtMs: 20 },
  });
  assert.equal(state.items[0].status, "cancelled");
  assert.equal(state.items[0].lastOutcome.message, "Canceled.");
  assert.equal(reduceExportQueue(state, { type: "claim-next" }), state);
  assert.equal(state.items[1].status, "queued");
});

test("start failure is retained and retry rejects a stale prior run", () => {
  let state = beginNext(enqueue(createExportQueueState(), request("retry")));
  state = reduceExportQueue(state, {
    type: "start-failed",
    itemId: 1,
    runId: 1,
    message: "Could not start.",
    completedAtMs: 100,
  });

  assert.equal(state.items[0].status, "failed");
  assert.equal(state.items[0].history.length, 1);
  assert.equal(state.items[0].lastOutcome.message, "Could not start.");

  const retryRequest = request("retry", { outputPath: "/outputs/retry-3.mp4" });
  state = reduceExportQueue(state, {
    type: "retry-prepared",
    itemId: 1,
    request: retryRequest,
    durationS: 8,
  });
  retryRequest.outputPath = "/mutated.mp4";

  assert.equal(state.items[0].id, 1);
  assert.equal(state.items[0].status, "queued");
  assert.equal(state.items[0].outputPath, "/outputs/retry-3.mp4");
  assert.equal(state.items[0].history.length, 1);
  assert.equal(state.items[0].lastOutcome.message, "Could not start.");
  assert.equal(state.items[0].durationS, 8);

  state = reduceExportQueue(state, { type: "claim-next" });
  assert.deepEqual(state.active, { itemId: 1, runId: 2 });
  const beforeStale = state;
  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: { kind: "failed", message: "old event" },
  });
  assert.equal(state, beforeStale);

  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 2,
    outcome: { kind: "done", outputSizeBytes: 2000, completedAtMs: 200 },
  });
  assert.equal(state.items[0].status, "done");
  assert.deepEqual(state.items[0].history.map((outcome) => outcome.kind), ["failed", "done"]);
});

test("duplicate appends a fresh immutable item but never duplicates the active item", () => {
  let state = enqueue(createExportQueueState(), request("source"));
  const duplicateRequest = request("source", { outputPath: "/outputs/source-3.mp4" });
  state = reduceExportQueue(state, {
    type: "duplicate-prepared",
    sourceItemId: 1,
    request: duplicateRequest,
  });
  duplicateRequest.outputPath = "/mutated.mp4";

  assert.deepEqual(state.items.map((item) => item.id), [1, 2]);
  assert.equal(state.items[1].outputPath, "/outputs/source-3.mp4");
  assert.equal(state.items[1].history.length, 0);
  assert.equal(state.items[1].lastOutcome, null);

  state = beginNext(state);
  assert.equal(
    reduceExportQueue(state, {
      type: "duplicate-prepared",
      sourceItemId: 1,
      request: request("source", { outputPath: "/outputs/source-4.mp4" }),
    }),
    state,
  );
});

test("retry history remains bounded while retaining the latest diagnostics", () => {
  let state = enqueue(createExportQueueState(), request("bounded-history"));

  for (let attempt = 1; attempt <= 15; attempt += 1) {
    state = beginNext(state);
    const active = state.active;
    state = reduceExportQueue(state, { type: "stop-auto-run" });
    state = reduceExportQueue(state, {
      type: "settled",
      itemId: active.itemId,
      runId: active.runId,
      outcome: {
        kind: "failed",
        message: `failure-${attempt}`,
        diagnostics: { mode: `attempt-${attempt}` },
      },
    });
    if (attempt < 15) {
      state = reduceExportQueue(state, {
        type: "retry-prepared",
        itemId: active.itemId,
        request: request("bounded-history", { outputPath: `/outputs/bounded-${attempt + 2}.mp4` }),
      });
    }
  }

  assert.equal(state.items[0].history.length, MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM);
  assert.equal(state.items[0].history[0].message, "failure-6");
  assert.equal(state.items[0].lastOutcome.message, "failure-15");
  assert.equal(state.items[0].lastOutcome.diagnostics.mode, "attempt-15");
});

test("target-missed retry history uses the same per-item bound", () => {
  let state = enqueue(createExportQueueState(), request("bounded-misses"));

  for (let attempt = 1; attempt <= 12; attempt += 1) {
    state = beginNext(state);
    const active = state.active;
    state = reduceExportQueue(state, { type: "stop-auto-run" });
    state = reduceExportQueue(state, {
      type: "settled",
      itemId: active.itemId,
      runId: active.runId,
      outcome: {
        kind: "target-missed",
        outputSizeBytes: 1_000 + attempt,
        targetResult: {
          status: "missed",
          targetBytes: 1_000,
          actualBytes: 1_000 + attempt,
          overshootBytes: attempt,
        },
      },
    });
    if (attempt < 12) {
      state = reduceExportQueue(state, {
        type: "retry-prepared",
        itemId: active.itemId,
        request: request("bounded-misses", { outputPath: `/outputs/miss-${attempt + 1}.mp4` }),
      });
    }
  }

  assert.equal(state.items[0].history.length, MAX_EXPORT_QUEUE_OUTCOMES_PER_ITEM);
  assert.equal(state.items[0].history[0].targetResult.overshootBytes, 3);
  assert.equal(state.items[0].lastOutcome.targetResult.overshootBytes, 12);
});

test("remove, clear, and reset preserve monotonic IDs and active safety", () => {
  let state = enqueue(createExportQueueState(), request("one"), request("two"), request("three"));
  state = beginNext(state);
  assert.equal(reduceExportQueue(state, { type: "remove", itemId: 1 }), state);
  assert.equal(reduceExportQueue(state, { type: "reset" }), state);

  state = reduceExportQueue(state, {
    type: "settled",
    itemId: 1,
    runId: 1,
    outcome: { kind: "done" },
  });
  state = reduceExportQueue(state, { type: "stop-auto-run" });
  state = reduceExportQueue(state, { type: "remove", itemId: 2 });
  assert.deepEqual(state.items.map((item) => item.id), [1, 3]);

  state = reduceExportQueue(state, { type: "clear-terminal" });
  assert.deepEqual(state.items.map((item) => item.id), [3]);
  const nextItemId = state.nextItemId;
  const nextRunId = state.nextRunId;

  state = reduceExportQueue(state, { type: "reset" });
  assert.equal(state.items.length, 0);
  assert.equal(state.nextItemId, nextItemId);
  assert.equal(state.nextRunId, nextRunId);

  state = enqueue(state, request("after-reset"));
  assert.equal(state.items[0].id, nextItemId);
});

test("invalid transitions and invalid prepared snapshots are no-ops", () => {
  const initial = createExportQueueState();
  assert.equal(reduceExportQueue(initial, { type: "unknown" }), initial);
  assert.equal(
    reduceExportQueue(initial, {
      type: "enqueue-prepared",
      items: [{ request: { inputPath: "", outputPath: "" } }],
    }),
    initial,
  );
  assert.equal(reduceExportQueue(initial, { type: "retry-prepared", itemId: 1 }), initial);

  const running = beginNext(enqueue(initial, request("invalid-target-miss")));
  assert.equal(
    reduceExportQueue(running, {
      type: "settled",
      itemId: 1,
      runId: 1,
      outcome: { kind: "target-missed" },
    }),
    running,
  );
});
