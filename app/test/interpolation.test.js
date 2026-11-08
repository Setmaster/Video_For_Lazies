import test from "node:test";
import assert from "node:assert/strict";
import { INTERPOLATION_MODELS, antiStutterMode, antiStutterRemovesTime, antiStutterPlanSummary, resolvedOutputDuration, setAntiStutterEnabled, antiStutterSummary, interpolationModelLabel, beginInterpolationInstall, bindInterpolationInstall, createInterpolationInstall, interpolationConflict, interpolationInstallActive, interpolationPublicMessage, normalizeInterpolation, reduceInterpolationInstall } from "../src/lib/interpolation.mjs";
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

test("each optional model survives settings, recipes and queue snapshots", () => {
  for (const modelId of INTERPOLATION_MODELS) {
    const interpolation = { targetFps: 60, modelId };
    assert.deepEqual(parsePersistedSettings(serializePersistedSettings({ interpolation })).interpolation, interpolation);
    const created = createUserRecipe([], modelId, "Smooth motion", { ...settings(), interpolation }, { id: modelId });
    assert.equal(created.ok, true);
    assert.deepEqual(parseUserRecipeStore(serializeUserRecipeStore(created.recipes)).recipes[0].settings.interpolation, interpolation);
    const queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request: { ...settings(), interpolation, inputPath: "/input.mp4", outputPath: "/output.mp4" }, durationS: 1 }] });
    assert.deepEqual(queue.items[0].request.interpolation, interpolation);
    interpolation.modelId = "untrusted";
    assert.equal(queue.items[0].request.interpolation.modelId, modelId);
  }
});

test("plan summaries identify every model by its actual name", () => {
  assert.deepEqual(INTERPOLATION_MODELS.map(interpolationModelLabel), ["RIFE 4.25", "RIFE 4.26", "GMFSS Fortuna (Union)", "GIMM-VFI-R-P", "LDF-VFI"]);
  assert.equal(interpolationModelLabel("toString"), "toString");
});

test("cap conflicts and MP3 cannot silently discard requested interpolation", () => {
  assert.match(interpolationConflict(intent, "mp4", 60), /below.*120.*Remove the cap/);
  assert.match(interpolationConflict(intent, "mp3", null), /MP4 or WebM/);
  assert.equal(interpolationConflict(intent, "webm", 120), null);
  assert.equal(interpolationConflict(null, "mp3", 24), null);
});

test("Anti-stutter is opt-in, survives reusable intent and remains distinct in recipes", () => {
  for (const antiStutter of [undefined, false, "true", 1, {}]) {
    assert.deepEqual(normalizeInterpolation({ ...intent, antiStutter }), intent);
  }
  const repair = { ...intent, antiStutter: true };
  assert.deepEqual(normalizeInterpolation(repair), repair);
  assert.deepEqual(parsePersistedSettings(serializePersistedSettings({ interpolation: repair })).interpolation, repair);
  const plan = { ...settings(), interpolation: repair };
  const created = createUserRecipe([], "Repair freezes", "Anti-stutter", plan, { id: "repair" });
  assert.equal(created.ok, true);
  const restored = parseUserRecipeStore(serializeUserRecipeStore(created.recipes)).recipes[0];
  assert.deepEqual(restored.settings.interpolation, repair);
  assert.equal(userRecipeMatchesSettings(restored, plan), true);
  assert.equal(userRecipeMatchesSettings(restored, settings()), false);
  let queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request: { ...plan, inputPath: "/input.mp4", outputPath: "/output.mp4" }, durationS: 1 }] });
  repair.antiStutter = false;
  assert.equal(queue.items[0].request.interpolation.antiStutter, true);
  const applied = reusableSettingsFromEncodeRequest(queue.items[0].request);
  assert.equal(applied.interpolation.antiStutter, true);
  queue = reduceExportQueue(queue, { type: "duplicate-prepared", sourceItemId: queue.items[0].id, request: { ...queue.items[0].request, outputPath: "/output-2.mp4" } });
  queue.items[0].request.interpolation.antiStutter = false;
  assert.equal(queue.items[1].request.interpolation.antiStutter, true);
});

test("Anti-stutter preserves unsupported model intent and blocks export instead of ignoring it", () => {
  const unsupported = { targetFps: 60, modelId: "ldf-vfi", antiStutter: true };
  assert.deepEqual(normalizeInterpolation(unsupported), unsupported);
  assert.match(interpolationConflict(unsupported, "mp4", null), /Anti-stutter needs RIFE, GMFSS or GIMM/);
  assert.equal(interpolationConflict({ ...unsupported, antiStutter: false }, "mp4", null), null);
  for (const modelId of INTERPOLATION_MODELS.filter((id) => id !== "ldf-vfi")) {
    assert.equal(interpolationConflict({ ...unsupported, modelId }, "webm", null), null);
  }
  assert.match(interpolationConflict(unsupported, "mp3", null), /MP4 or WebM/);
});

test("repair strength defaults to 75 and preserves explicit conservative intent", () => {
  for (const strength of [undefined, null, false, "50", -1, 101, 0.5, NaN, Infinity, {}]) {
    assert.deepEqual(normalizeInterpolation({ ...intent, antiStutterStrength: strength }), intent);
  }
  assert.deepEqual(normalizeInterpolation({ ...intent, antiStutterStrength: 75 }), intent);
  for (const strength of [0, 1, 50, 99, 100]) {
    const chosen = { ...intent, antiStutterStrength: strength };
    assert.deepEqual(normalizeInterpolation(chosen), chosen);
    assert.deepEqual(normalizeInterpolation({ ...chosen, antiStutter: false }), chosen);
    assert.deepEqual(normalizeInterpolation({ ...chosen, antiStutter: true }), { ...chosen, antiStutter: true });
  }
});

for (const strength of [0, 73]) test(`repair strength ${strength} survives settings, recipes and isolated queue snapshots`, () => {
  const interpolation = { ...intent, antiStutter: true, antiStutterStrength: strength };
  const plan = { ...settings(), interpolation };
  assert.deepEqual(parsePersistedSettings(serializePersistedSettings(plan)).interpolation, interpolation);
  const created = createUserRecipe([], "Stronger repair", "Anti-stutter", plan, { id: "strength" });
  assert.equal(created.ok, true);
  const restored = parseUserRecipeStore(serializeUserRecipeStore(created.recipes)).recipes[0];
  assert.deepEqual(restored.settings.interpolation, interpolation);
  assert.equal(userRecipeMatchesSettings(restored, plan), true);
  assert.equal(userRecipeMatchesSettings(restored, { ...plan, interpolation: { ...interpolation, antiStutterStrength: 100 } }), false);
  let queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request: { ...plan, inputPath: "/input.mp4", outputPath: "/output.mp4" }, durationS: 1 }] });
  interpolation.antiStutterStrength = 12;
  assert.equal(queue.items[0].request.interpolation.antiStutterStrength, strength);
  assert.equal(reusableSettingsFromEncodeRequest(queue.items[0].request).interpolation.antiStutterStrength, strength);
  queue = reduceExportQueue(queue, { type: "duplicate-prepared", sourceItemId: queue.items[0].id, request: { ...queue.items[0].request, outputPath: "/output-2.mp4" } });
  queue.items[0].request.interpolation.antiStutterStrength = 25;
  assert.equal(queue.items[1].request.interpolation.antiStutterStrength, strength);
});

test("Anti-stutter results distinguish no repairs from uncertain candidates and completed repairs", () => {
  assert.equal(antiStutterSummary(undefined), null);
  assert.equal(antiStutterSummary({ repairedRuns: 0, unchangedRuns: 0 }), "Anti-stutter: no short freezes repaired.");
  assert.equal(antiStutterSummary({ repairedRuns: 0, unchangedRuns: 4 }), "Anti-stutter: no short freezes repaired. 4 candidates left unchanged.");
  assert.equal(antiStutterSummary({ repairedRuns: 1, unchangedRuns: 1 }), "Anti-stutter: 1 suspected freeze repaired. 1 candidate left unchanged.");
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


test("legacy enabled Anti-stutter stays repair while a new enable selects removal", () => {
  const legacy = { ...intent, antiStutter: true };
  assert.deepEqual(normalizeInterpolation(legacy), legacy);
  assert.equal(antiStutterMode(legacy), "repair");
  assert.equal(antiStutterRemovesTime(legacy), false);
  assert.equal(setAntiStutterEnabled(intent, true).antiStutterMode, "remove");
  const disabledLegacy = normalizeInterpolation(setAntiStutterEnabled(legacy, false));
  assert.equal(disabledLegacy.antiStutterMode, "repair");
  assert.equal(setAntiStutterEnabled(disabledLegacy, true).antiStutterMode, "repair");
  for (const mode of ["remove", "repair"]) {
    const chosen = { ...intent, antiStutter: true, antiStutterMode: mode };
    assert.equal(setAntiStutterEnabled(setAntiStutterEnabled(chosen, false), true).antiStutterMode, mode);
    assert.deepEqual(normalizeInterpolation(chosen), chosen);
  }
});

test("mode allowlist preserves explicit repair and drops malformed mode without enabling removal", () => {
  for (const mode of [undefined, null, false, 1, "Remove", "drop", {}, []]) {
    const normalized = normalizeInterpolation({ ...intent, antiStutter: true, antiStutterMode: mode });
    assert.deepEqual(normalized, { ...intent, antiStutter: true });
    assert.equal(antiStutterRemovesTime(normalized), false);
  }
  assert.deepEqual(normalizeInterpolation({ ...intent, antiStutterMode: "repair" }), { ...intent, antiStutterMode: "repair" });
  assert.equal(antiStutterRemovesTime({ ...intent, antiStutterMode: "remove" }), false);
});

for (const mode of ["remove", "repair"]) test(`${mode} mode survives settings recipes and isolated queue snapshots`, () => {
  const interpolation = { ...intent, antiStutter: true, antiStutterStrength: 0, antiStutterMode: mode };
  const plan = { ...settings(), interpolation };
  assert.deepEqual(parsePersistedSettings(serializePersistedSettings(plan)).interpolation, interpolation);
  const created = createUserRecipe([], `Mode ${mode}`, "Anti-stutter", plan, { id: mode });
  assert.equal(created.ok, true);
  const restored = parseUserRecipeStore(serializeUserRecipeStore(created.recipes)).recipes[0];
  assert.deepEqual(restored.settings.interpolation, interpolation);
  assert.equal(userRecipeMatchesSettings(restored, plan), true);
  assert.equal(userRecipeMatchesSettings(restored, { ...plan, interpolation: { ...interpolation, antiStutterMode: mode === "remove" ? "repair" : "remove" } }), false);
  let queue = reduceExportQueue(createExportQueueState(), { type: "enqueue-prepared", items: [{ request: { ...plan, inputPath: "/input.mp4", outputPath: "/output.mp4" }, durationS: 3 }] });
  interpolation.antiStutterMode = "changed";
  assert.equal(queue.items[0].request.interpolation.antiStutterMode, mode);
  assert.equal(queue.items[0].durationS, mode === "remove" ? null : 3);
  assert.equal(reusableSettingsFromEncodeRequest(queue.items[0].request).interpolation.antiStutterMode, mode);
  queue = reduceExportQueue(queue, { type: "duplicate-prepared", sourceItemId: queue.items[0].id, request: { ...queue.items[0].request, outputPath: "/output-2.mp4" } });
  queue.items[0].request.interpolation.antiStutterMode = "changed";
  assert.equal(queue.items[1].request.interpolation.antiStutterMode, mode);
});

test("mode summaries do not promise preserved timing for removal or removal for legacy repair", () => {
  const legacy = { ...intent, antiStutter: true };
  assert.match(antiStutterPlanSummary(legacy), /Fill pauses.*preserve duration/);
  assert.match(antiStutterPlanSummary({ ...legacy, antiStutterMode: "remove" }), /shorten output.*audio cuts/);
  assert.equal(antiStutterPlanSummary(intent), null);
  assert.equal(antiStutterSummary({ mode: "remove", removedRuns: 0, removedDurationS: 0, unchangedRuns: 1 }), "Anti-stutter: no repeated time removed. 1 candidate left unchanged.");
  assert.equal(antiStutterSummary({ mode: "remove", removedRuns: 2, removedDurationS: .4, unchangedRuns: 0 }), "Anti-stutter: 0.400 s of repeated source time removed from 2 holds.");
  assert.equal(antiStutterSummary({ mode: "remove", removedRuns: 1, removedDurationS: .4, repairedRuns: 2, unchangedRuns: 1 }), "Anti-stutter: 0.400 s of repeated source time removed from 1 hold. 2 pauses filled without removing time. 1 candidate left unchanged.");
  assert.equal(antiStutterSummary({ mode: "remove", removedRuns: 0, removedDurationS: 0, repairedRuns: 1, unchangedRuns: 0 }), "Anti-stutter: no repeated time removed. 1 pause filled without removing time.");
  assert.match(antiStutterSummary({ mode: "remove", removedRuns: 1, removedDurationS: .0001 }), /<0.001 s/);
});


test("completed output uses native final duration without source-time speed or loop rescaling", () => {
  const diagnostics = { outputDurationS: 7.25, interpolation: { antiStutter: { mode: "remove", removedDurationS: 2 } } };
  assert.equal(resolvedOutputDuration(diagnostics, 10), 7.25);
  for (const outputDurationS of [undefined, null, 0, -1, NaN, Infinity]) {
    assert.equal(resolvedOutputDuration({ ...diagnostics, outputDurationS }, 10), null);
  }
  assert.equal(resolvedOutputDuration(null, 10), 10);
  assert.equal(resolvedOutputDuration({ interpolation: { antiStutter: { mode: "repair" } } }, 10), 10);
  assert.equal(resolvedOutputDuration(undefined), null);
});
