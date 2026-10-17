import test from "node:test";
import assert from "node:assert/strict";
import { beginInterpolationInstall, bindInterpolationInstall, createInterpolationInstall, interpolationConflict, interpolationInstallActive, interpolationPublicMessage, normalizeInterpolation, reduceInterpolationInstall } from "../src/lib/interpolation.mjs";
import { parsePersistedSettings, serializePersistedSettings } from "../src/lib/settings.mjs";
import { EXPORT_RECIPES, recipeMatchesSettings } from "../src/lib/exportRecipes.mjs";
import { createExportQueueState, reduceExportQueue } from "../src/lib/exportQueue.mjs";
import { createEmptyUserRecipeStore, createUserRecipe, parseUserRecipeStore, persistUserRecipeStore, reusableSettingsFromEncodeRequest, serializeUserRecipeStore, userRecipeMatchesSettings, USER_RECIPE_SCHEMA_VERSION } from "../src/lib/userRecipes.mjs";
import { createEncodeProgressState, getActiveProgressUi, reduceEncodeProgress } from "../src/lib/progress.mjs";

const intent = { targetFps: 120, modelId: "rife-v4.26" };
const model = intent.modelId;
const event = (state, operationId = "current", completedBytes = 0) => ({ modelId: model, operationId, state, completedBytes, totalBytes: 100, message: "Public status" });
const settings = () => ({ ...EXPORT_RECIPES.find(({ id }) => id === "archive-quality").settings, interpolation: intent });

test("saved interpolation intent is an allowlist and old settings default Off", () => {
  assert.deepEqual(parsePersistedSettings('{}'), {});
  assert.equal(normalizeInterpolation({ targetFps: 60, modelId: "untrusted" }), null);
  assert.equal(normalizeInterpolation({ targetFps: "60", modelId: "rife-v4.25" }), null);
  const raw = serializePersistedSettings({ interpolation: { ...intent, modelPath: "/private/model", deviceName: "private" } });
  assert.deepEqual(JSON.parse(raw), { interpolation: intent });
  assert.deepEqual(parsePersistedSettings(raw).interpolation, intent);
  assert.doesNotMatch(raw, /private|Path|device/);
});

test("cap conflicts and MP3 cannot silently discard requested interpolation", () => {
  assert.match(interpolationConflict(intent, "mp4", 60), /below.*120.*Remove the cap/);
  assert.match(interpolationConflict(intent, "mp3", null), /MP4 or WebM/);
  assert.equal(interpolationConflict(intent, "webm", 120), null);
  assert.equal(interpolationConflict(null, "mp3", 24), null);
});

test("early events bind only to returned operation ID and terminal outcomes settle once", () => {
  let state = beginInterpolationInstall(model);
  state = reduceInterpolationInstall(state, event("ready", "old", 100));
  state = reduceInterpolationInstall(state, event("verifying", "current", 100));
  state = reduceInterpolationInstall(state, event("testing", "current", 100));
  state = bindInterpolationInstall(state, "current");
  assert.equal(state.state, "testing");
  assert.equal(interpolationInstallActive(state), true);
  assert.equal(reduceInterpolationInstall(state, { ...event("ready"), modelId: "rife-v4.25" }), state);
  assert.equal(reduceInterpolationInstall(state, event("failed", "old")), state);
  assert.equal(reduceInterpolationInstall(state, event("downloading")), state);
  state = reduceInterpolationInstall(state, event("ready", "current", 100));
  assert.equal(interpolationInstallActive(state), false);
  assert.equal(reduceInterpolationInstall(state, event("failed")), state);
  assert.equal(reduceInterpolationInstall(createInterpolationInstall(model), event("ready" )).state, "idle");
});

test("retry ignores previous-operation completion and reports actual monotonic bytes", () => {
  let state = bindInterpolationInstall(beginInterpolationInstall(model), "retry");
  state = reduceInterpolationInstall(state, event("downloading", "retry", 70));
  state = reduceInterpolationInstall(state, event("downloading", "retry", 20));
  assert.equal(state.completedBytes, 70);
  assert.equal(reduceInterpolationInstall(state, event("ready", "current", 100)), state);
  state = reduceInterpolationInstall(state, event("canceled", "retry", 75));
  assert.equal(state.state, "canceled");
  assert.equal(reduceInterpolationInstall(state, event("ready", "retry", 100)), state);
});

test("all pre-interpolation recipe schemas migrate Off and preserve current target/model", () => {
  for (const schemaVersion of [0, 1, 2, 3]) {
    const stored = { id: "old", name: "Old", label: "Old", description: "Old description", settings: settings() };
    const migrated = parseUserRecipeStore(JSON.stringify({ schemaVersion, recipes: [stored] }));
    assert.equal(migrated.migrated, true);
    assert.equal(migrated.recipes[0].settings.interpolation, null);
  }
  const created = createUserRecipe([], "Smooth", "120 fps", settings(), { id: "smooth" });
  assert.equal(created.ok, true);
  const restored = parseUserRecipeStore(serializeUserRecipeStore(created.recipes));
  assert.equal(restored.migrated, false);
  assert.deepEqual(restored.recipes[0].settings.interpolation, intent);
  assert.equal(userRecipeMatchesSettings(restored.recipes[0], settings()), true);
  assert.equal(userRecipeMatchesSettings(restored.recipes[0], { ...settings(), interpolation: { ...intent, targetFps: 60 } }), false);
  assert.equal(userRecipeMatchesSettings(restored.recipes[0], { ...settings(), interpolation: { ...intent, modelId: "rife-v4.25" } }), false);
});

test("future recipes remain read-only and built-in exact matching includes interpolation", () => {
  const future = parseUserRecipeStore(JSON.stringify({ schemaVersion: USER_RECIPE_SCHEMA_VERSION + 1, recipes: [] }));
  let writes = 0;
  assert.equal(persistUserRecipeStore({ setItem() { writes++; } }, future, []).ok, false);
  assert.equal(writes, 0);
  const builtIn = EXPORT_RECIPES.find(({ id }) => id === "archive-quality");
  assert.equal(recipeMatchesSettings(builtIn, builtIn.settings), true);
  assert.equal(recipeMatchesSettings(builtIn, settings()), false);
  assert.equal(createEmptyUserRecipeStore().readOnly, false);
});

test("queue snapshots, duplication and reusable Apply settings retain private-path-free intent", () => {
  const request = { ...settings(), inputPath: "/private/input.mp4", outputPath: "/private/output.mp4", sizeLimitMb: 0, subtitlePath: "/private/subtitle.srt" };
  let queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request, durationS: 3 }] });
  request.interpolation = null;
  assert.deepEqual(queue.items[0].request.interpolation, intent);
  const restored = reusableSettingsFromEncodeRequest(queue.items[0].request);
  assert.deepEqual(restored.interpolation, intent);
  assert.doesNotMatch(JSON.stringify(restored), /private|Path/);
  queue = reduceExportQueue(queue, { type: "duplicate-prepared", sourceItemId: queue.items[0].id, request: { ...queue.items[0].request, outputPath: "/private/output-2.mp4" } });
  assert.equal(queue.items.length, 2);
  assert.deepEqual(queue.items[1].request.interpolation, intent);
  queue.items[0].request.interpolation.targetFps = 60;
  assert.equal(queue.items[1].request.interpolation.targetFps, 120);
});

test("inference progress preserves phase, attempt ownership, monotonicity and publication cap", () => {
  let state = reduceEncodeProgress(createEncodeProgressState(), { attemptId: 12, jobId: 8, phase: "preparing", overallPct: 0.02 });
  assert.equal(getActiveProgressUi(state, true).label, "Preparing smooth motion");
  state = reduceEncodeProgress(state, { attemptId: 12, jobId: 8, phase: "interpolating", overallPct: 0.4 });
  assert.equal(getActiveProgressUi(state, true).label, "Creating smooth motion");
  state = reduceEncodeProgress(state, { attemptId: 12, jobId: 8, phase: "encoding", overallPct: 0.2 });
  assert.equal(state.overallPct, 0.4);
  assert.equal(reduceEncodeProgress(state, { attemptId: 11, jobId: 8, overallPct: 1 }), state);
  state = reduceEncodeProgress(state, { attemptId: 12, jobId: 8, phase: "finalizing", overallPct: 1 });
  assert.equal(getActiveProgressUi(state, true).percent, 99);
});

test("numeric native operation IDs bind safely without string coercion", () => {
  let state = beginInterpolationInstall(model);
  state = reduceInterpolationInstall(state, event("testing", 17, 100));
  state = bindInterpolationInstall(state, 17);
  assert.equal(state.state, "testing");
  assert.equal(state.operationId, 17);
  assert.equal(reduceInterpolationInstall(state, event("ready", "17", 100)), state);
  assert.equal(reduceInterpolationInstall(state, event("ready", Number.MAX_SAFE_INTEGER + 1, 100)), state);
  assert.equal(reduceInterpolationInstall(state, event("ready", 17, 100)).state, "ready");
});

test("reverse and loop memory use generated source-epoch frames and RGB storage", async () => {
  const { estimateTransformMemory } = await import("../src/lib/mediaDepth.mjs");
  const base = { probe: { durationS: 10, frameRate: 24, decodedVideoBytesPerPixel: 1.5, hasAudio: false }, reverse: true, loopVideo: true, width: 100, height: 100, audioEnabled: false, speed: 2 };
  const actual = estimateTransformMemory({ ...base, interpolationTargetFps: 120 });
  const expected = estimateTransformMemory({ ...base, probe: { ...base.probe, frameRate: 60, decodedVideoBytesPerPixel: 4 } });
  assert.equal(actual.bytes, expected.bytes);
  assert.ok(actual.bytes > estimateTransformMemory(base).bytes);
  assert.equal(estimateTransformMemory({ ...base, reverse: false, loopVideo: false, interpolationTargetFps: 120 }).bytes, 0);
});

test("public command failure messages retain recovery guidance without exposing Error internals", () => {
  assert.equal(interpolationPublicMessage(" A model check is already running. ", "Try again."), "A model check is already running.");
  assert.equal(interpolationPublicMessage(new Error("/private/model/file"), "Try again."), "Try again.");
  assert.equal(interpolationPublicMessage("x".repeat(2000), "Try again.").length, 1000);
});
