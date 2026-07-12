import test from "node:test";
import assert from "node:assert/strict";
import {
  STRICT_FIT_FRAME_RATE_CAP_FPS,
  STRICT_FIT_MAX_EDGE_TIERS,
  STRICT_FIT_MAX_PLANS,
  STRICT_FIT_REDUCED_AUDIO_KBPS,
  canonicalizeStrictFitOptions,
  canonicalizeTargetResult,
  exactTargetBytesFromMegabytes,
  nextLowerStrictFitMaxEdge,
  strictFitCorrectiveActions,
  summarizeStrictFitPlan,
  summarizeStrictFitPolicy,
  summarizeTargetResult,
  targetResultFormatData,
} from "../src/lib/strictFit.mjs";

const met = Object.freeze({
  status: "met",
  targetBytes: 10_000_000,
  actualBytes: 10_000_000,
  overshootBytes: 0,
});

const missed = Object.freeze({
  status: "missed",
  targetBytes: 10_000_000,
  actualBytes: 10_250_000,
  overshootBytes: 250_000,
});

test("Strict Fit policy constants are fixed and immutable", () => {
  assert.equal(STRICT_FIT_MAX_PLANS, 4);
  assert.equal(STRICT_FIT_REDUCED_AUDIO_KBPS, 32);
  assert.equal(STRICT_FIT_FRAME_RATE_CAP_FPS, 30);
  assert.deepEqual(STRICT_FIT_MAX_EDGE_TIERS, [1280, 960, 720, 540, 360]);
  assert.equal(Object.isFrozen(STRICT_FIT_MAX_EDGE_TIERS), true);
  assert.throws(() => STRICT_FIT_MAX_EDGE_TIERS.push(240), TypeError);
});

test("strict options require literal booleans and expose only Strict Fit", () => {
  assert.deepEqual(canonicalizeStrictFitOptions(), {
    strictFit: false,
  });
  assert.deepEqual(canonicalizeStrictFitOptions({ strictFit: false }), {
    strictFit: false,
  });
  assert.deepEqual(canonicalizeStrictFitOptions({ strictFit: "true" }), {
    strictFit: false,
  });
  const enabled = canonicalizeStrictFitOptions({ strictFit: true });
  assert.deepEqual(enabled, { strictFit: true });
  assert.equal(Object.isFrozen(enabled), true);
  assert.throws(() => { enabled.strictFit = false; }, TypeError);
});

test("decimal MB targets fail closed outside the exact safe-integer byte range", () => {
  assert.equal(exactTargetBytesFromMegabytes(0), 0);
  assert.equal(exactTargetBytesFromMegabytes(0.1), 100_000);
  assert.equal(exactTargetBytesFromMegabytes(9_007_199_254.74099), 9_007_199_254_740_990);
  assert.equal(exactTargetBytesFromMegabytes(Number.MAX_SAFE_INTEGER / 1_000_000), null);
  assert.equal(exactTargetBytesFromMegabytes(9_007_199_254.740993), null);
  assert.equal(exactTargetBytesFromMegabytes(Number.POSITIVE_INFINITY), null);
  assert.equal(exactTargetBytesFromMegabytes(-1), null);
  assert.equal(exactTargetBytesFromMegabytes("10"), null);
});

test("backend target status stays authoritative while exact-byte invariants are validated", () => {
  assert.deepEqual(canonicalizeTargetResult(met), met);
  assert.deepEqual(canonicalizeTargetResult(missed), missed);
  assert.equal(Object.isFrozen(canonicalizeTargetResult(missed)), true);

  for (const invalid of [
    null,
    {},
    { ...met, status: "done" },
    { ...met, targetBytes: 0 },
    { ...met, targetBytes: 10.5 },
    { ...met, actualBytes: Number.MAX_SAFE_INTEGER + 1 },
    { ...met, overshootBytes: -1 },
    { ...met, actualBytes: 10_000_001, overshootBytes: 1 },
    { ...missed, status: "met" },
    { ...missed, status: "missed", actualBytes: 10_000_000, overshootBytes: 0 },
    { ...missed, overshootBytes: 249_999 },
  ]) {
    assert.equal(canonicalizeTargetResult(invalid), null);
  }
});

test("format data uses exact decimal bytes for boundaries, overshoot, and percentage", () => {
  const exact = targetResultFormatData({
    ...missed,
    targetMb: 999,
    actualMb: 0,
  });
  assert.deepEqual({
    status: exact.status,
    met: exact.met,
    missed: exact.missed,
    targetBytes: exact.targetBytes,
    actualBytes: exact.actualBytes,
    overshootBytes: exact.overshootBytes,
    remainingBytes: exact.remainingBytes,
    overshootPercent: exact.overshootPercent,
  }, {
    status: "missed",
    met: false,
    missed: true,
    targetBytes: 10_000_000,
    actualBytes: 10_250_000,
    overshootBytes: 250_000,
    remainingBytes: 0,
    overshootPercent: 2.5,
  });
  assert.equal(exact.targetBytesText, "10,000,000 bytes");
  assert.equal(exact.actualBytesText, "10,250,000 bytes");
  assert.equal(exact.overshootPercentText, "2.50%");
  assert.equal(exact.targetMegabytesText, "10.00 MB");

  const boundary = targetResultFormatData(met);
  assert.equal(boundary.status, "met");
  assert.equal(boundary.remainingBytes, 0);
  assert.equal(boundary.overshootPercent, 0);

  const tiny = targetResultFormatData({
    status: "missed",
    targetBytes: Number.MAX_SAFE_INTEGER - 1,
    actualBytes: Number.MAX_SAFE_INTEGER,
    overshootBytes: 1,
  });
  assert.equal(tiny.overshootPercentText, "<0.01%");
});

test("next max-edge tier observes every boundary without upscaling or no-op plans", () => {
  assert.equal(nextLowerStrictFitMaxEdge(1920, 1080), 1280);
  assert.equal(nextLowerStrictFitMaxEdge(1080, 1920), 1280);
  assert.equal(nextLowerStrictFitMaxEdge(1281, 720), 1280);
  assert.equal(nextLowerStrictFitMaxEdge(1280, 720), 960);
  assert.equal(nextLowerStrictFitMaxEdge(961, 777), 960);
  assert.equal(nextLowerStrictFitMaxEdge(960, 540), 720);
  assert.equal(nextLowerStrictFitMaxEdge(721, 719), 720);
  assert.equal(nextLowerStrictFitMaxEdge(720, 720), 540);
  assert.equal(nextLowerStrictFitMaxEdge(541, 333), 540);
  assert.equal(nextLowerStrictFitMaxEdge(540, 333), 360);
  assert.equal(nextLowerStrictFitMaxEdge(361, 359), 360);
  assert.equal(nextLowerStrictFitMaxEdge(360, 359), null);
  assert.equal(nextLowerStrictFitMaxEdge(320, 180), null);

  // Source geometry can be larger than the already-planned max edge.
  assert.equal(nextLowerStrictFitMaxEdge(3840, 2160, 720), 540);
  // A custom output below the selected control still never upscales.
  assert.equal(nextLowerStrictFitMaxEdge(854, 480, 1280), 720);

  for (const dimensions of [
    [0, 720],
    [720.5, 480],
    [Number.MAX_SAFE_INTEGER + 1, 480],
    [720, 480, "720"],
  ]) {
    assert.equal(nextLowerStrictFitMaxEdge(...dimensions), null);
  }
});

test("corrective actions expose only applicable control changes in stable order", () => {
  const actions = strictFitCorrectiveActions({
    targetResult: missed,
    format: "mp4",
    width: 1920,
    height: 1080,
    plannedFrameRateFps: 60,
    hasAudio: true,
    audioEnabled: true,
    strictFit: false,
  });
  assert.deepEqual(actions.map(({ kind }) => kind), [
    "reduceMaxEdge",
    "capFrameRate",
    "removeAudio",
    "enableStrictFit",
  ]);
  assert.deepEqual(actions.map(({ changes }) => changes), [
    { resizeMode: "maxEdge", maxEdgePx: 1280 },
    { frameRateCapFps: 30 },
    { audioEnabled: false },
    { strictFit: true },
  ]);
  assert.ok(actions.every(({ confirmation }) => confirmation.endsWith("No export started.")));
  assert.equal(Object.isFrozen(actions), true);
  assert.ok(actions.every((action) => Object.isFrozen(action) && Object.isFrozen(action.changes)));
  assert.throws(() => { actions[0].changes.maxEdgePx = 960; }, TypeError);
  assert.equal(missed.overshootBytes, 250_000);
});

test("met, already-corrected, absent-stream, and MP3 contexts omit inapplicable actions", () => {
  assert.deepEqual(strictFitCorrectiveActions({
    targetResult: met,
    format: "mp4",
    width: 1920,
    height: 1080,
    plannedFrameRateFps: 60,
    hasAudio: true,
    audioEnabled: true,
  }), []);
  assert.deepEqual(strictFitCorrectiveActions({
    targetResult: missed,
    format: "mp4",
    width: 360,
    height: 240,
    plannedFrameRateFps: 30,
    hasAudio: false,
    audioEnabled: true,
    strictFit: true,
  }), []);
  assert.deepEqual(strictFitCorrectiveActions({
    targetResult: missed,
    format: "mp3",
    width: 1920,
    height: 1080,
    plannedFrameRateFps: 60,
    hasAudio: true,
    audioEnabled: true,
    strictFit: false,
  }), []);
  assert.deepEqual(strictFitCorrectiveActions({
    targetResult: missed,
    format: null,
    width: 1920,
    height: 1080,
    plannedFrameRateFps: 60,
    hasAudio: true,
    audioEnabled: true,
    strictFit: false,
  }), []);
  assert.deepEqual(strictFitCorrectiveActions({
    targetResult: { ...missed, overshootBytes: 1 },
    format: "mp4",
    width: 1920,
    height: 1080,
  }), []);
});

test("policy and exact result summaries disclose limits without rounded classification", () => {
  assert.match(summarizeStrictFitPolicy({ strictFit: false }), /Strict Fit is off/);
  assert.match(summarizeStrictFitPolicy({ strictFit: true }), /at most 4 plans/);
  assert.match(summarizeStrictFitPolicy({ strictFit: true }), /bitrate correction/);
  assert.match(summarizeStrictFitPolicy({ strictFit: true }), /32 kbps/);
  const retainedAudioPolicy = summarizeStrictFitPolicy({ strictFit: true });
  assert.match(retainedAudioPolicy, /keeps audio at 32 kbps/);
  assert.doesNotMatch(retainedAudioPolicy, /remove audio/);
  assert.match(summarizeStrictFitPolicy({
    strictFit: true,
    audioEnabled: false,
  }), /audio plan is skipped/);

  assert.equal(
    summarizeTargetResult(met),
    "Target met: 10,000,000 bytes used of the 10,000,000 bytes limit (0 bytes remaining).",
  );
  assert.equal(
    summarizeTargetResult(missed),
    "Target missed: 10,250,000 bytes is 250,000 bytes (2.50%) over the 10,000,000 bytes limit.",
  );
  assert.equal(summarizeTargetResult({ ...missed, status: "met" }), null);
});

test("plan summaries are bounded, readable, and strip control characters", () => {
  const summary = summarizeStrictFitPlan({
    planNumber: 2,
    label: "  Lower\nedge\u202e  ",
    mutations: ["Max edge 960 px\n", "\u0000Audio 32 kbps"],
    width: 960,
    height: 540,
    videoBitrateKbps: 850,
    audioAction: "encode",
    audioBitrateKbps: 32,
    actualSizeBytes: 9_900_000,
    status: "met",
    selected: true,
  });
  assert.equal(
    summary,
    "Plan 2 (Lower edge) met the target at 9,900,000 bytes with 960x540, 850 kbps video, 32 kbps audio. Changes: Max edge 960 px; Audio 32 kbps. This was the published plan.",
  );
  assert.equal(summarizeStrictFitPlan({
    planNumber: 5,
    actualSizeBytes: 10,
    status: "missed",
  }), null);
  assert.equal(summarizeStrictFitPlan({
    planNumber: 1,
    actualSizeBytes: Number.MAX_SAFE_INTEGER + 1,
    status: "missed",
  }), null);
  assert.match(summarizeStrictFitPlan({
    planNumber: 4,
    audioAction: "drop",
    actualSizeBytes: 9_000_000,
    status: "met",
  }), /audio removed/);
});
