import test from "node:test";
import assert from "node:assert/strict";
import { planTargetDuration, prepareTargetSample, resolveTargetSpeed, targetDurationSummary, validTargetDuration, targetSampleBlockingReason, TARGET_NUT_SAMPLE_REASON, TARGET_REMOVE_SAMPLE_REASON } from "../src/lib/targetDuration.mjs";
import { parsePersistedSettings, serializePersistedSettings } from "../src/lib/settings.mjs";
import { EXPORT_RECIPES } from "../src/lib/exportRecipes.mjs";
import { reusableSettingsFromEncodeRequest } from "../src/lib/userRecipes.mjs";
import { createExportQueueState, reduceExportQueue } from "../src/lib/exportQueue.mjs";

const plan = options => planTargetDuration({ enabled: true, sourceDurationS: 8, targetS: 4, manualSpeed: 3, ...options });

test("target derives shorter/longer speed without replacing manual intent", () => {
  assert.equal(plan().effectiveSpeed, 2);
  assert.equal(plan({ targetS: 16 }).effectiveSpeed, .5);
  assert.equal(plan({ enabled: false }).effectiveSpeed, 3);
  assert.equal(plan({ manualSpeed: "not a speed" }).effectiveSpeed, 2);
  assert.equal(plan({ enabled: false, manualSpeed: "not a speed" }).effectiveSpeed, null);
  assert.equal(plan({ enabled: false }).targetDurationS, null);
});

test("trim/source/boomerang recalculate speed; MP3 ignores boomerang", () => {
  const selected = { sourceDurationS: 10, trimStartS: 2, trimEndS: 6, targetS: 2 };
  assert.equal(plan(selected).effectiveSpeed, 2);
  assert.equal(plan({ ...selected, loopVideo: true }).effectiveSpeed, 4);
  assert.equal(plan({ ...selected, loopVideo: true, format: "mp3" }).effectiveSpeed, 2);
  assert.equal(plan({ sourceDurationS: 20 }).effectiveSpeed, 5);
  assert.equal(plan({ trimEndS: 100 }).retainedDurationS, 8);
  assert.match(plan({ trimStartS: 8 }).error, /trim interval/);
});

test("invalid targets fail visibly rather than selecting arbitrary speed", () => {
  for (const targetS of ["", " ", 0, -1, NaN, Infinity, "bad", false, true, {}, []]) {
    assert.equal(validTargetDuration(targetS), false);
    const result = plan({ targetS });
    assert.equal(result.effectiveSpeed, null); assert.match(result.error, /greater than zero/);
  }
  for (const targetS of [.001, 4, 1e100]) assert.equal(validTargetDuration(targetS), true);
});

test("supported limits choose closest bounded speed and explain the unreachable target", () => {
  const fast = plan({ targetS: .4 }); assert.equal(fast.effectiveSpeed, 16); assert.equal(fast.estimatedOutputDurationS, .5); assert.equal(fast.speedLimited, true);
  const slow = plan({ targetS: 800 }); assert.equal(slow.effectiveSpeed, .05); assert.equal(slow.estimatedOutputDurationS, 160); assert.equal(slow.speedLimited, true);
  assert.match(targetDurationSummary(fast), /Closest supported speed/);
  assert.equal(plan({ targetS: 1e-308 }).effectiveSpeed, 16);
  assert.equal(plan({ targetS: 1e308 }).effectiveSpeed, .05);
});

test("cleanup/no-source pending state never substitutes stale manual speed or duration", () => {
  for (const options of [{ cleanupPending: true }, { sourceDurationS: null }]) {
    const result = plan({ ...options, manualSpeed: 12 });
    assert.equal(result.pending, true); assert.equal(result.effectiveSpeed, null); assert.equal(result.estimatedOutputDurationS, null); assert.equal(result.error, null);
    assert.match(targetDurationSummary(result), options.cleanupPending ? /calculated after cleanup/ : /Waiting for the source/);
  }
});

test("automatic interpolation grid is bounded and minimizes final frame-clock error", () => {
  for (const fps of [60, 120]) for (const multiplier of [1, 2]) {
    const result = resolveTargetSpeed(1.01, .503, multiplier, fps);
    assert.ok(result.effectiveSpeed >= .05 && result.effectiveSpeed <= 16);
    assert.equal(Number.isInteger(Math.round(result.effectiveSpeed * 1e6)), true);
    assert.ok(Math.abs(result.estimatedOutputDurationS - .503) <= multiplier / fps / 2 + 1e-12);
    assert.equal(plan({ sourceDurationS: 1.01, targetS: .503, loopVideo: multiplier === 2, interpolationTargetFps: fps }).effectiveSpeed, result.effectiveSpeed);
  }
});

test("sample inherits full trim/boomerang target speed and clears target before short trim", () => {
  const request = { speed: 3, targetDurationS: 4, trim: { startS: 2, endS: 6 }, loopVideo: true };
  const full = plan({ sourceDurationS: 10, trimStartS: 2, trimEndS: 6, targetS: 4, loopVideo: true });
  const sample = prepareTargetSample(request, full);
  assert.equal(sample.speed, 2); assert.equal(sample.targetDurationS, undefined); assert.equal(sample.isSample, true);
  assert.equal(sample.targetDurationSample, true);
  sample.trim = { startS: 3, endS: 4 };
  assert.equal(sample.speed, 2); assert.equal(request.speed, 3); assert.equal(request.targetDurationS, 4);
  assert.deepEqual(prepareTargetSample({ speed: 1.25 }, plan({ enabled: false })), { speed: 1.25, isSample: true });
  assert.deepEqual(prepareTargetSample({ speed: 1.25, targetDurationSample: true }, plan({ enabled: false })), { speed: 1.25, isSample: true });
  assert.equal(request.targetDurationSample, undefined);
});

test("target plus removal cannot create a sample before cleanup resolution", () => {
  const request = { speed: 1, targetDurationS: 4, interpolation: { antiStutter: true, antiStutterMode: "remove" } };
  assert.throws(() => prepareTargetSample(request, plan({ cleanupPending: true })), { message: TARGET_REMOVE_SAMPLE_REASON });
  assert.equal(request.isSample, undefined);
});

test("queue freezes target/manual/trim intent but strips transient sample marker", () => {
  const request = { inputPath: "/in.mp4", outputPath: "/out.mp4", format: "mp4", speed: 3, targetDurationS: 4, isSample: true, targetDurationSample: true, trim: { startS: 1, endS: 9 }, loopVideo: true };
  let queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request, durationS: 4 }] });
  request.targetDurationS = 2; request.trim.startS = 0;
  assert.equal(queue.items[0].request.targetDurationS, 4); assert.equal(queue.items[0].request.speed, 3); assert.equal(queue.items[0].request.trim.startS, 1); assert.equal(queue.items[0].request.isSample, undefined);
  assert.equal(queue.items[0].request.targetDurationSample, undefined);
  queue = reduceExportQueue(queue, { type: "duplicate-prepared", sourceItemId: queue.items[0].id, request: { ...queue.items[0].request, outputPath: "/other.mp4" } });
  queue.items[0].request.targetDurationS = 7;
  assert.equal(queue.items[1].request.targetDurationS, 4);
  assert.equal(queue.items[1].request.targetDurationSample, undefined);
});

test("target follows existing clip-scoped manual speed and stays outside settings/recipes", () => {
  assert.deepEqual(parsePersistedSettings(serializePersistedSettings({ targetDurationS: 4, speed: 3, isSample: true, targetDurationSample: true })), {});
  const base = EXPORT_RECIPES.find(recipe => recipe.id === "archive-quality").settings;
  const reusable = reusableSettingsFromEncodeRequest({ ...base, speed: 3, targetDurationS: 4, isSample: true, targetDurationSample: true });
  assert.ok(reusable); assert.equal("targetDurationS" in reusable, false); assert.equal("speed" in reusable, false); assert.equal("isSample" in reusable, false);
  assert.equal("targetDurationSample" in reusable, false);
});


test("native auto-speed golden cases match frontend frame-grid planning", () => {
  for (const [duration, target, fps, multiplier, speed, output] of [
    [1.01, .5, 60, 1, 2.02, .5], [1.01, .5, 60, 2, 4.04, .5],
    [1, 1.01, 60, 2, 2, 1], [.75, 1, 60, 2, 1.5, 1], [.6, 1, 60, 2, 1.2, 1],
    [2, .001, null, 1, 16, .125], [2, 1000, null, 1, .05, 40],
  ]) {
    assert.deepEqual(resolveTargetSpeed(duration, target, multiplier, fps), { effectiveSpeed: speed, estimatedOutputDurationS: output });
  }
});

test("only target samples needing full cleanup/NUT timeline analysis are blocked", () => {
  const remove = { targetFps: 60, modelId: "rife-v4.25", antiStutter: true, antiStutterMode: "remove" };
  assert.equal(targetSampleBlockingReason({ enabled: true, interpolation: remove }), TARGET_REMOVE_SAMPLE_REASON);
  assert.equal(targetSampleBlockingReason({ enabled: true, sourceFormat: "nut" }), TARGET_NUT_SAMPLE_REASON);
  assert.equal(targetSampleBlockingReason({ enabled: false, sourceFormat: "nut", interpolation: remove }), null);
  assert.equal(targetSampleBlockingReason({ enabled: true, sourceFormat: "mp4" }), null);
  assert.equal(targetSampleBlockingReason({ enabled: true, format: "mp3", interpolation: remove }), null);
});


test("full timeline analysis remains pending without stale source estimates", () => {
  const result = plan({ timelinePending: true, manualSpeed: .1 });
  assert.equal(result.effectiveSpeed, null); assert.equal(result.estimatedOutputDurationS, null);
  assert.equal(result.waitingForTimeline, true); assert.equal(result.waitingForCleanup, false);
  assert.match(targetDurationSummary(result), /full timeline analysis/);
  assert.equal(plan({ enabled: false, timelinePending: true, manualSpeed: .1 }).effectiveSpeed, .1);
});
