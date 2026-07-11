import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  TRANSFORM_MEMORY_BLOCK_BYTES,
  estimateTransformMemory,
} from "../src/lib/mediaDepth.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("Fast Trim uses native accessible controls and concrete containing-boundary disclosure", async () => {
  const [component, app, css] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/components/TrimModeControls.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/App.css"), "utf8"),
  ]);

  assert.match(component, /<fieldset className="vfl-fast-trim"/);
  assert.match(component, /<legend>Trim method<\/legend>/);
  assert.match(component, /type="radio"[\s\S]*value="exact"/);
  assert.match(component, /type="radio"[\s\S]*value="fastCopy"/);
  assert.match(component, /Fast Trim keeps a containing closed-GOP interval/);
  assert.match(component, /material before the requested start and after the requested end/);
  assert.match(component, /role="status"[\s\S]*aria-live="polite"[\s\S]*aria-atomic="true"/);
  assert.match(component, /role="alert"/);
  assert.match(component, /<caption>Fast Trim expected source boundaries<\/caption>/);
  assert.match(component, /type="checkbox"/);
  assert.match(component, /I accept the expected retained interval from/);
  assert.match(component, /!accepted \? \(/);
  assert.doesNotMatch(component, /\.focus\(/);

  assert.match(app, /<TrimModeControls/);
  assert.match(app, /disabled=\{!exportReady \|\| encodeBusy\}/);
  assert.match(app, /fastTrimBlockingReason/);
  assert.match(css, /\.vfl-fast-trim-option\.selected/);
  assert.match(css, /\.vfl-fast-trim-evidence table/);
});

test("Fast Trim request, queue, result, reset, recipe, and smoke paths stay explicitly guarded", async () => {
  const [app, types, queue, recipeDialog] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/types.ts"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/exportQueue.mjs"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/components/UserRecipeDialog.tsx"), "utf8"),
  ]);

  assert.match(types, /export type TrimMode = "exact" \| "fastCopy";/);
  assert.match(types, /fastCopyConsent\?: FastTrimConsent \| null;/);
  assert.match(types, /export interface FastTrimInspection/);
  assert.match(types, /export interface TrimResult/);
  assert.match(types, /fastTrimInspection\?: FastTrimInspection \| null;/);
  assert.match(types, /trimResult\?: TrimResult \| null;/);
  for (const optionalDisposition of [
    "dub",
    "lyrics",
    "karaoke",
    "cleanEffects",
    "nonDiegetic",
    "captions",
    "descriptions",
    "metadata",
    "dependent",
    "multilayer",
  ]) {
    assert.match(types, new RegExp(`${optionalDisposition}\\?: boolean;`));
  }

  assert.match(app, /invoke<FastTrimInspection>\("inspect_fast_trim", \{ request: inspectionRequest \}\)/);
  assert.match(app, /request\.trim\.fastCopyConsent = fastTrimConsentForRequest/);
  assert.match(app, /fastTrimStateIsAccepted\(fastTrimState, fastTrimCurrentFingerprint\)/);
  assert.match(app, /fastTrimDurationFromRequest\(capturedRequest\)/);
  assert.match(app, /fastTrimConsentsMatch\(request\.trim\.fastCopyConsent, inspectedFastTrim\.consent\)/);
  assert.match(app, /Applied the full queue snapshot\. Fast Trim boundaries changed/);
  assert.match(app, /request\.trim = \{ startS: sampleStart, endS: sampleEnd, mode: "exact", fastCopyConsent: null \};/);
  assert.match(app, /Export exact sample/);
  assert.match(app, /fastTrimModeRef\.current = "exact";[\s\S]*setTrimMode\("exact"\);[\s\S]*clearFastTrimState\(\);/);
  assert.match(app, /fastTrimInspection: extra\.fastTrimInspection \?\? null/);
  assert.match(app, /trimResult: p\.trimResult \?\? null/);
  assert.match(app, /"fast-trim-ready"/);
  assert.match(app, /"fast-trim-reset-trim-complete"/);
  assert.match(app, /"fast-trim-reset-all-complete"/);
  assert.match(app, /data-smoke-id="reset-trim"/);
  assert.match(app, /data-smoke-id="reset-all-settings"/);
  assert.match(app, /resetTrimButton\.click\(\)/);
  assert.match(app, /resetAllButton\.click\(\)/);
  assert.match(app, /state\.phase === "idle"[\s\S]*state\.inspection === null[\s\S]*state\.acceptedConfirmationToken === null/);
  assert.match(app, /"keyboard-fast-trim-ready"/);
  assert.match(app, /"keyboard-fast-trim-accept-ready"/);
  assert.match(app, /"keyboard-fast-trim-complete"/);
  assert.match(
    app,
    /async function reportSmokeFailure\([\s\S]*extra: Omit<AppSmokeStatus, "stage" \| "ok" \| "message"> = \{\},[\s\S]*reportSmokeStatus\(SMOKE_ERROR_STAGE, \{ \.\.\.extra, ok: false, message \}\)/,
  );
  assert.match(
    app,
    /if \(inspection\.status === "blocked"\)[\s\S]*reportSmokeFailure\([\s\S]*\{ fastTrimInspection: inspection \},[\s\S]*\);/,
  );
  assert.match(
    app,
    /if \(!p\.ok\) \{[\s\S]*reportSmokeFailure\([\s\S]*diagnostics: p\.diagnostics \?\? null,[\s\S]*trimResult: p\.trimResult \?\? null,[\s\S]*\);/,
  );

  assert.match(queue, /trimResult: outcome\.trimResult/);
  assert.match(recipeDialog, /trim method, expected or accepted Fast Trim boundaries/);
  assert.match(app, /Fast Trim boundaries or consent/);
});

test("same-settings batches and schema-v2 recipes do not inherit clip-scoped Fast Trim", async () => {
  const [app, userRecipes] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/userRecipes.mjs"), "utf8"),
  ]);

  assert.match(app, /function buildSettingsOnlyRequest[\s\S]*trim: null,[\s\S]*crop: null,/);
  assert.match(app, /Same-settings batches intentionally exclude clip-scoped subtitles/);
  assert.match(userRecipes, /export const USER_RECIPE_SCHEMA_VERSION = 2;/);
  assert.match(userRecipes, /Convert a full backend request into the same privacy-safe recipe allowlist/);
  assert.doesNotMatch(userRecipes, /fastCopyConsent|confirmationToken|effectiveStartUs|videoPacketCount/);
});

test("Fast export and forced-Exact sample capability gates remain separate", async () => {
  const app = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  const exactStart = app.indexOf("const exactExportPlanBlockingReason");
  const sampleStart = app.indexOf("const exactSamplePlanBlockingReason");
  const fastStart = app.indexOf("const fastExportBasicBlockingReason");
  const selectionStart = app.indexOf("const exportBlockingReason", fastStart);
  assert.ok(exactStart >= 0 && sampleStart > exactStart && fastStart > sampleStart && selectionStart > fastStart);

  const exactGate = app.slice(exactStart, sampleStart);
  const sampleGate = app.slice(sampleStart, fastStart);
  const fastGate = app.slice(fastStart, selectionStart);

  assert.match(exactGate, /capabilityInspectionBlockingReason/);
  assert.match(exactGate, /coreCapabilityBlockingReason/);
  assert.match(exactGate, /autoVideoCodecBlockingReason/);
  assert.match(sampleGate, /capabilityInspectionBlockingReason/);
  assert.match(sampleGate, /exactSampleCoreCapabilityBlockingReason/);
  assert.match(sampleGate, /exactSampleAutoVideoCodecBlockingReason/);
  assert.match(sampleGate, /exactSampleTransformMemoryBlockingReason/);
  assert.doesNotMatch(sampleGate, /transformMemoryBlockingReason|queueOutputBlockingReason/);
  assert.match(app, /coreRequiredFiltersFor\(true, exactSampleRequiresVideoEncoder\)/);
  assert.match(app, /const exactSampleRequiresAudioEncoder = format === "mp3" \|\| retainedAudioForPlan;/);
  assert.match(app, /const sampleExportReady = Boolean\([\s\S]*!exactSamplePlanBlockingReason\);/);

  assert.match(fastGate, /sizeTargetExactnessBlockingReason/);
  assert.match(fastGate, /subtitleInspecting/);
  assert.match(fastGate, /queueOutputBlockingReason/);
  assert.doesNotMatch(
    fastGate,
    /capabilityInspectionBlockingReason|coreCapabilityBlockingReason|transformCapabilityBlockingReason|autoVideoCodecBlockingReason|selectedVideoCodecUnavailable/,
  );
  assert.match(
    app.slice(selectionStart, selectionStart + 260),
    /trimMode === "fastCopy"[\s\S]*fastExportBasicBlockingReason \?\? fastTrimBlockingReason[\s\S]*exactExportPlanBlockingReason/,
  );
});

test("exact samples use bounded transform memory and reserve only the chosen sample destination", async () => {
  const app = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  assert.match(
    app,
    /const exactSampleSourceDurationS = probe && trimTimeline[\s\S]*Math\.min\(sampleDurationS, Math\.max\(0, trimTimeline\.end - trimTimeline\.start\)\)/,
  );
  assert.match(
    app,
    /const exactSampleTransformMemoryEstimate = useMemo[\s\S]*trimStartS: 0,[\s\S]*trimEndS: exactSampleSourceDurationS/,
  );
  assert.match(
    app,
    /const selected = await saveDialog[\s\S]*if \(exportQueueClaimsOutputPath\(exportQueueStateRef\.current, selected\)\)[\s\S]*return;/,
  );

  const probe = {
    durationS: 7_200,
    frameRate: 30,
    decodedVideoBytesPerPixel: 1.5,
    hasAudio: false,
  };
  const fullClip = estimateTransformMemory({
    probe,
    reverse: true,
    loopVideo: false,
    width: 640,
    height: 360,
    audioEnabled: false,
  });
  const safeSample = estimateTransformMemory({
    probe,
    reverse: true,
    loopVideo: false,
    trimEndS: 5,
    width: 640,
    height: 360,
    audioEnabled: false,
  });
  const unsafeSample = estimateTransformMemory({
    probe: { ...probe, frameRate: 120, decodedVideoBytesPerPixel: 3 },
    reverse: true,
    loopVideo: true,
    trimEndS: 30,
    width: 3_840,
    height: 2_160,
    audioEnabled: false,
  });
  assert.equal(fullClip.severity, "blocked");
  assert.ok(fullClip.bytes >= TRANSFORM_MEMORY_BLOCK_BYTES);
  assert.equal(safeSample.severity, "ok");
  assert.equal(unsafeSample.severity, "blocked");
});

test("Fast Trim planning preserves odd copy dimensions and packaged smoke proves reset and source mutation gates", async () => {
  const [app, types] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/types.ts"), "utf8"),
  ]);
  const plannedSummary = app.slice(
    app.indexOf("const plannedSummary = useMemo"),
    app.indexOf("const previewDurationS ="),
  );
  assert.match(
    plannedSummary,
    /trimMode === "exact" && trimRequestIsActive\(\{ startS: startRaw, endS: endRaw, durationS: probe\.durationS \}\)/,
  );
  const trimModeDependencyPattern = /\r?\n\s+trimMode,\r?\n/;
  assert.match(plannedSummary, trimModeDependencyPattern);
  assert.match(plannedSummary.replace(/\r?\n/g, "\r\n"), trimModeDependencyPattern);
  assert.match(types, /selectedAudioDispositions\?: StreamDispositions \| null;/);
  assert.match(types, /audioEnabled\?: boolean \| null;/);
  assert.match(types, /stripMetadata\?: boolean \| null;/);
  assert.match(types, /sourceMutation\?: boolean \| null;/);
  assert.match(app, /setAudioEnabled\(smokeConfig\.audioEnabled !== false\)/);
  assert.match(app, /setStripMetadata\(smokeConfig\.stripMetadata !== false\)/);
  assert.match(app, /"fast-trim-source-mutation-ready"/);
  assert.match(app, /"fast-trim-source-mutation-complete"/);
  assert.match(
    app,
    /candidate\.consent\.confirmationToken !== acceptedConsent\.confirmationToken/,
  );
  assert.match(
    app,
    /candidate = await invoke<FastTrimInspection>\("inspect_fast_trim", \{ request: inspectionRequest \}\)/,
  );
  assert.doesNotMatch(
    app.slice(
      app.indexOf("if (smokeConfig.sourceMutation"),
      app.indexOf("if (jobId !== null || smokeStartRef.current)"),
    ),
    /settleFastTrimCheck|setFastTrimState|fastCopyConsent = candidate/,
  );
});

test("Fast Trim presentation is current-fingerprint guarded and reports backend blocks", async () => {
  const app = await fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
  assert.match(app, /const fastTrimPresentationState = fastTrimStateForPresentation/);
  assert.match(app, /fastTrimPresentationState\.phase === "ready"[\s\S]*fastTrimEffectiveDurationS\(fastTrimPresentationState\.inspection\)/);
  assert.match(app, /Fast Trim copy-only plan, blocked by the backend check/);
  assert.match(app, /state=\{fastTrimPresentationState\}/);
});
