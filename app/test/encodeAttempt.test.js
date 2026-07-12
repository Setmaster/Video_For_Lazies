import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  acceptsEncodeEvent,
  beginEncodeAttempt,
  bindEncodeAttempt,
  bindStartedEncode,
  createIdleEncodeAttempt,
  deriveEncodeAttemptPresentation,
  failEncodeAttemptStart,
  finishEncodeAttempt,
  requestEncodeCancellation,
  settleEncodeFinished,
} from "../src/lib/encodeAttempt.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

test("encode attempt follows starting, running, and succeeded states", () => {
  const idle = createIdleEncodeAttempt();
  const starting = beginEncodeAttempt(101);
  const running = bindEncodeAttempt(starting, 101, 7);
  const succeeded = finishEncodeAttempt(
    running,
    { attemptId: 101, jobId: 7, ok: true },
    500,
  );

  assert.deepEqual(idle, { kind: "idle" });
  assert.deepEqual(starting, { kind: "starting", attemptId: 101 });
  assert.deepEqual(running, { kind: "running", attemptId: 101, jobId: 7 });
  assert.deepEqual(succeeded, {
    kind: "succeeded",
    attemptId: 101,
    jobId: 7,
    completedAtMs: 500,
  });
});

test("successful settlement retains its artifact, target result, message, and diagnostics", () => {
  const running = bindEncodeAttempt(beginEncodeAttempt(201), 201, 21);
  const targetResult = {
    status: "met",
    targetBytes: 1_000,
    actualBytes: 990,
    overshootBytes: 0,
    strictFit: true,
    selectedPlanNumber: 2,
    plans: [{ planNumber: 2, status: "met", actualSizeBytes: 990 }],
  };
  const diagnostics = {
    mode: "Strict Fit",
    attempts: 2,
    commandPreview: "ffmpeg <input> <output>",
  };
  const succeeded = finishEncodeAttempt(
    running,
    {
      attemptId: 201,
      jobId: 21,
      ok: true,
      outputPath: "/exports/met.mp4",
      outputSizeBytes: 990,
      targetResult,
      message: "Fit confirmed by exact output bytes.",
      diagnostics,
    },
    501,
  );
  targetResult.plans[0].actualSizeBytes = 5_000;
  diagnostics.mode = "mutated";

  assert.equal(succeeded.kind, "succeeded");
  assert.equal(succeeded.outputPath, "/exports/met.mp4");
  assert.equal(succeeded.outputSizeBytes, 990);
  assert.equal(succeeded.message, "Fit confirmed by exact output bytes.");
  assert.equal(succeeded.targetResult.plans[0].actualSizeBytes, 990);
  assert.equal(succeeded.diagnostics.mode, "Strict Fit");
});

test("exact target miss is artifact-bearing but neither success nor execution failure", () => {
  const pending = {
    attemptId: 202,
    jobId: 22,
    outputPath: "/exports/requested-fallback.mp4",
    queueItemId: 8,
  };
  const running = bindEncodeAttempt(beginEncodeAttempt(202), 202, 22);
  const settlement = settleEncodeFinished(
    pending,
    running,
    {
      attemptId: 202,
      jobId: 22,
      ok: true,
      outputPath: "/exports/smallest-successful.mp4",
      targetResult: {
        status: "missed",
        targetBytes: 1_000,
        actualBytes: 1_125,
        overshootBytes: 125,
      },
      message: "The smallest successful artifact is still over target.",
      diagnostics: {
        mode: "Strict Fit",
        attempts: 4,
        commandPreview: "ffmpeg <input> <output>",
      },
    },
    502,
  );

  assert.equal(settlement.accepted, true);
  assert.equal(settlement.pending, null);
  assert.equal(settlement.context.queueItemId, 8);
  assert.equal(settlement.state.kind, "target-missed");
  assert.equal(settlement.state.outputPath, "/exports/smallest-successful.mp4");
  assert.equal(settlement.state.outputSizeBytes, 1_125);
  assert.equal(settlement.state.targetResult.overshootBytes, 125);
  assert.equal(settlement.state.diagnostics.attempts, 4);

  assert.deepEqual(deriveEncodeAttemptPresentation(settlement.state), {
    isActive: false,
    isSuccess: false,
    isFailure: false,
    isCancelled: false,
    isTargetMissed: true,
    kicker: "Size target missed",
    summary: "Target missed",
    message: "The smallest successful artifact is still over target.",
  });
});

test("start failure belongs only to the matching attempt", () => {
  const starting = beginEncodeAttempt(102);
  const stale = failEncodeAttemptStart(starting, 99, "stale", 1);
  const failed = failEncodeAttemptStart(starting, 102, "Could not start.", 2);

  assert.equal(stale, starting);
  assert.deepEqual(failed, {
    kind: "failed",
    attemptId: 102,
    jobId: null,
    message: "Could not start.",
    completedAtMs: 2,
  });
});

test("cancel request classifies a failed finish as cancelled", () => {
  const running = bindEncodeAttempt(beginEncodeAttempt(103), 103, 8);
  const cancelling = requestEncodeCancellation(running, 103, 8);
  const cancelled = finishEncodeAttempt(
    cancelling,
    { attemptId: 103, jobId: 8, ok: false, message: "Canceled." },
    600,
  );

  assert.equal(cancelling.kind, "cancelling");
  assert.deepEqual(cancelled, {
    kind: "cancelled",
    attemptId: 103,
    jobId: 8,
    message: "Canceled.",
    completedAtMs: 600,
  });
});

test("successful finish wins after a cancel request", () => {
  const running = bindEncodeAttempt(beginEncodeAttempt(104), 104, 9);
  const cancelling = requestEncodeCancellation(running, 104, 9);
  const succeeded = finishEncodeAttempt(
    cancelling,
    { attemptId: 104, jobId: 9, ok: true },
    700,
  );

  assert.equal(succeeded.kind, "succeeded");
});

test("event gate accepts early matching attempts and rejects stale identities", () => {
  assert.equal(acceptsEncodeEvent(null, { attemptId: 1, jobId: 1 }), false);
  assert.equal(
    acceptsEncodeEvent({ attemptId: 105, jobId: null }, { attemptId: 105, jobId: 10 }),
    true,
  );
  assert.equal(
    acceptsEncodeEvent({ attemptId: 105, jobId: null }, { attemptId: 104, jobId: 10 }),
    false,
  );
  assert.equal(
    acceptsEncodeEvent({ attemptId: 105, jobId: 10 }, { attemptId: 105, jobId: 11 }),
    false,
  );
  assert.equal(
    acceptsEncodeEvent({ attemptId: 105, jobId: 10 }, { attemptId: 105, jobId: 10 }),
    true,
  );
});

test("early finish can settle a starting attempt by attempt identity", () => {
  const starting = beginEncodeAttempt(106);
  const finished = finishEncodeAttempt(
    starting,
    { attemptId: 106, jobId: 12, ok: true },
    800,
  );
  assert.equal(finished.kind, "succeeded");
  assert.equal(bindEncodeAttempt(finished, 106, 12), finished);
});

test("early finish clears the coordinator gate and a later start reply cannot resurrect it", () => {
  const pending = {
    attemptId: 206,
    jobId: null,
    queueItemId: 44,
    sample: { outputDurationS: 2, fullDurationS: 20 },
  };
  const starting = beginEncodeAttempt(206);
  const settled = settleEncodeFinished(
    pending,
    starting,
    { attemptId: 206, jobId: 22, ok: true },
    900,
  );
  const lateBinding = bindStartedEncode(
    settled.pending,
    settled.state,
    206,
    22,
  );

  assert.equal(settled.accepted, true);
  assert.equal(settled.pending, null);
  assert.equal(settled.context.queueItemId, 44);
  assert.equal(settled.context.sample.fullDurationS, 20);
  assert.equal(settled.state.kind, "succeeded");
  assert.equal(lateBinding.accepted, false);
  assert.equal(lateBinding.pending, null);
  assert.equal(lateBinding.state, settled.state);
});

test("finished coordinator exposes side-effect context once and rejects duplicates", () => {
  const pending = {
    attemptId: 207,
    jobId: 23,
    queueItemId: 45,
    outputPath: "/exports/queued-output.mp4",
  };
  const running = bindEncodeAttempt(beginEncodeAttempt(207), 207, 23);
  const first = settleEncodeFinished(
    pending,
    running,
    { attemptId: 207, jobId: 23, ok: false, message: "failed" },
    901,
  );
  const duplicate = settleEncodeFinished(
    first.pending,
    first.state,
    { attemptId: 207, jobId: 23, ok: true },
    902,
  );

  assert.equal(first.accepted, true);
  assert.equal(first.context.queueItemId, 45);
  assert.equal(first.state.outputPath, "/exports/queued-output.mp4");
  assert.equal(duplicate.accepted, false);
  assert.equal(duplicate.context, null);
  assert.equal(duplicate.state, first.state);
});

test("stale and duplicate finishes leave lifecycle unchanged", () => {
  const running = bindEncodeAttempt(beginEncodeAttempt(107), 107, 13);
  const staleAttempt = finishEncodeAttempt(
    running,
    { attemptId: 106, jobId: 13, ok: false, message: "stale" },
    1,
  );
  const staleJob = finishEncodeAttempt(
    running,
    {
      attemptId: 107,
      jobId: 14,
      ok: true,
      targetResult: {
        status: "missed",
        targetBytes: 1_000,
        actualBytes: 1_100,
        overshootBytes: 100,
      },
      message: "stale target miss",
    },
    1,
  );
  const succeeded = finishEncodeAttempt(
    running,
    { attemptId: 107, jobId: 13, ok: true },
    2,
  );
  const duplicate = finishEncodeAttempt(
    succeeded,
    { attemptId: 107, jobId: 13, ok: false, message: "late" },
    3,
  );

  assert.equal(staleAttempt, running);
  assert.equal(staleJob, running);
  assert.equal(duplicate, succeeded);
});

test("presentation gives failure and cancellation precedence over prior success UI", () => {
  const failed = failEncodeAttemptStart(
    beginEncodeAttempt(108),
    108,
    "Failed to start.",
    4,
  );
  const cancelled = finishEncodeAttempt(
    requestEncodeCancellation(
      bindEncodeAttempt(beginEncodeAttempt(109), 109, 15),
      109,
      15,
    ),
    { attemptId: 109, jobId: 15, ok: false, message: "Canceled." },
    5,
  );

  assert.deepEqual(deriveEncodeAttemptPresentation(failed), {
    isActive: false,
    isSuccess: false,
    isFailure: true,
    isCancelled: false,
    isTargetMissed: false,
    kicker: "Export failed",
    summary: "Failed",
    message: "Failed to start.",
  });
  const cancelledUi = deriveEncodeAttemptPresentation(cancelled);
  assert.equal(cancelledUi.kicker, "Export canceled");
  assert.equal(cancelledUi.isFailure, false);
  assert.equal(cancelledUi.isCancelled, true);
});

test("App routes both backend event types through attempt identity before effects", async () => {
  const app = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  const listenerStart = app.indexOf("const subscription = installEncodeEventListeners");
  const listenerEnd = app.indexOf("  }, []);", listenerStart);
  const listenerInstall = app.slice(listenerStart, listenerEnd);
  const finishedRegistration = listenerInstall.indexOf('listen<EncodeFinishedPayload>("encode-finished"');
  const progressRegistration = listenerInstall.indexOf('listen<EncodeProgressPayload>("encode-progress"');
  const progressListener = listenerInstall.slice(
    listenerInstall.indexOf("onProgress:"),
    listenerInstall.indexOf("onFinished:"),
  );
  const finishedListener = listenerInstall.slice(
    listenerInstall.indexOf("onFinished:"),
    listenerInstall.indexOf("onReady:"),
  );

  assert.ok(listenerStart >= 0 && listenerEnd > listenerStart);
  assert.ok(finishedRegistration >= 0 && progressRegistration > finishedRegistration);
  assert.match(progressListener, /if \(!acceptsEncodeEvent\(pendingEncode, payload\)\) return;/);
  assert.match(finishedListener, /if \(!acceptsEncodeEvent\(pendingEncode, p\) \|\| !pendingEncode\) return;/);
  assert.match(listenerInstall, /encodeEventsReadyRef\.current = true;/);
  assert.match(listenerInstall, /encodeEventsErrorRef\.current = message;/);
  assert.doesNotMatch(listenerInstall, /coerceErrorMessage\(error/);
  assert.match(listenerInstall, /subscription\.dispose\(\)/);
  assert.match(app, /invoke<number>\("start_encode", \{ request, attemptId \}\)/);
  assert.match(app, /bindStartedEncode\(/);
  assert.match(app, /settleEncodeFinished\(/);
  assert.match(app, /async function startEncode[\s\S]*?if \(!encodeEventsReadyRef\.current\)/);
  assert.match(app, /function runQueue\(\) \{\s*if \(!encodeEventsReadyRef\.current\)/s);
  assert.match(app, /if \(encodeEventsError\)[\s\S]*?if \(!encodeEventsReady\) return;/);
  assert.match(app, /ENCODE_EVENT_SETUP_SMOKE_ERROR/);
  assert.match(app, /id="vfl-encode-event-setup-error"[\s\S]*?role="alert"[\s\S]*?aria-live="assertive"/);
  assert.match(app, /const encodeBusy = attemptUi\.isActive \|\| queueRunning \|\| queuePreparationBusy \|\| queueSnapshotApplying \|\| subtitleInspecting;/);
  assert.match(app, /latestAttempt\.kind === "starting" \? \(/);
  assert.match(app, /disabled=\{!exportReady \|\| encodeBusy\}/);
  assert.match(app, /Previous successful export/);
  assert.match(app, /openFolderFor\(lastExport\.outputPath\)/);
  assert.match(app, /latestAttempt\.outputPath \|\| outputPath \|\| inputPath/);
  assert.match(app, /Copy fallback:/);
  assert.match(app, /attemptUi\.isFailure/);
  assert.match(app, /attemptUi\.isCancelled/);
});
