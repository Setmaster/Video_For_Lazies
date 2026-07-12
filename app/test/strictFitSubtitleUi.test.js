import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function read(relativePath) {
  const source = await fs.readFile(path.resolve(__dirname, relativePath), "utf8");
  return source.replace(/\r\n?/g, "\n");
}

function between(source, start, end) {
  const startIndex = source.indexOf(start);
  const endIndex = source.indexOf(end, startIndex + start.length);
  assert.notEqual(startIndex, -1, `missing start marker: ${start}`);
  assert.notEqual(endIndex, -1, `missing end marker: ${end}`);
  return source.slice(startIndex, endIndex);
}

test("frontend types and request builders carry the typed G5 contract", async () => {
  const [app, types, recipes] = await Promise.all([
    read("../src/App.tsx"),
    read("../src/lib/types.ts"),
    read("../src/lib/exportRecipes.d.ts"),
  ]);

  for (const field of [
    "strictFit: boolean",
    "subtitlePath?: string | null",
    "targetResult?: TargetResult | null",
    "subtitleBurnedIn: boolean",
    "subtitleCueCount?: number | null",
  ]) {
    assert.match(types, new RegExp(field.replace(/[?+]/g, "\\$&")));
  }
  assert.match(types, /export interface TargetResult[\s\S]*?status: SizeTargetStatus[\s\S]*?targetBytes: number[\s\S]*?actualBytes: number[\s\S]*?overshootBytes: number[\s\S]*?selectedPlanNumber: number[\s\S]*?plans: FitPlanResult\[\]/);
  assert.match(types, /export interface SubtitleInspection[\s\S]*?cueCount: number[\s\S]*?firstCueStartS: number[\s\S]*?lastCueEndS: number/);
  assert.match(recipes, /strictFit: boolean/);
  assert.doesNotMatch(types, /strictFitAllowAudioRemoval/);
  assert.doesNotMatch(recipes, /strictFitAllowAudioRemoval/);

  const batchBuilder = between(app, "function buildSettingsOnlyRequest", "function buildRequest");
  assert.match(batchBuilder, /strictFit: requestStrictFit/);
  assert.doesNotMatch(batchBuilder, /strictFitAllowAudioRemoval/);
  assert.match(batchBuilder, /subtitlePath: null/);

  const fullBuilder = between(app, "function buildRequest", "function claimedOutputPathsForPreparation");
  assert.match(fullBuilder, /format === "mp3" && subtitlePath/);
  assert.match(fullBuilder, /strictFit: requestStrictFit/);
  assert.match(fullBuilder, /subtitlePath: subtitlePath \|\| null/);
});

test("external SRT stays clip-scoped, revalidated, capability-gated, and path-private", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /featureCapability\("externalSubtitles"\)/);
  assert.match(app, /invoke<SubtitleInspection>\("inspect_srt", \{ path: selected \}\)/);
  assert.match(app, /item\.request\.subtitlePath[\s\S]*?invoke<SubtitleInspection>\("inspect_srt", \{ path: item\.request\.subtitlePath \}\)/);
  assert.match(app, /clearExternalSubtitle\("External subtitles cleared for the new source\."\)/);
  assert.match(app, /clearExternalSubtitle\("External subtitles cleared by Reset\."\)/);
  assert.match(app, /setSubtitlePath\(request\.subtitlePath \?\? ""\)/);
  assert.match(app, /basename\(subtitlePath\)/);
  assert.doesNotMatch(app, /value=\{subtitlePath\}/);

  const recipeApply = between(app, "function applyFullRecipeSettings", "function applyExportRecipe");
  assert.doesNotMatch(recipeApply, /setSubtitlePath|clearExternalSubtitle/);
  const smokeWriter = between(app, "async function reportSmokeStatus", "async function reportSmokeFailure");
  assert.doesNotMatch(smokeWriter, /subtitlePath|subtitleName|subtitleBasename/);
  assert.match(app, /subtitlePathToken[\s\S]*?escapedSubtitlePath[\s\S]*?subtitleBasename[\s\S]*?'"subtitlePath"'[\s\S]*?'"cueCount"'/);
  const settingsPersistence = between(app, "localStorage.setItem(\n        SETTINGS_KEY", "  }, [\n    settingsReady");
  assert.doesNotMatch(settingsPersistence, /subtitlePath/);
});

test("exact target misses remain openable artifacts with bounded evidence and control-only corrections", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /const targetMissed = p\.targetResult\?\.status === "missed"/);
  assert.match(app, /p\.outputSizeBytes === targetData\.actualBytes/);
  assert.match(app, /p\.diagnostics\?\.actualSizeBytes === targetData\.actualBytes/);
  assert.match(app, /p\.diagnostics\?\.requestedSizeBytes === targetData\.targetBytes/);
  assert.match(app, /kind: targetMissed \? "target-missed" : "done"/);
  assert.match(app, /targetResult: p\.targetResult \?\? null/);
  assert.match(app, /queueCounts\.missed/);
  assert.match(app, /item\.status === "target-missed"/);
  assert.match(app, /TargetResultDetails targetResult=\{outcome\.targetResult\}/);
  assert.match(app, /targetResult\.plans\.slice\(0, 4\)/);
  assert.equal((app.match(/exactTargetBytesFromMegabytes\(size\) === null/g) ?? []).length, 2);
  assert.match(app, /Size limit is too large to track exactly in bytes/);
  assert.match(app, /aria-invalid=\{sizeTargetExactnessBlockingReason \? true : undefined\}/);
  assert.match(app, /aria-label="Target miss corrective actions"/);
  assert.match(app, /These actions update controls only\. No export starts until you choose Export\./);
  assert.match(app, /disabled=\{encodeBusy \|\| applied \|\| !correctiveActionsMatchCurrentPlan\}/);
  assert.match(app, /lastExport\.correctiveContext\.sourcePathIdentity === queuePathIdentity\(inputPath\)/);
  assert.match(app, /Open file/);
  assert.match(app, /else if \(targetMissed\) \{[\s\S]*?setStatus\(exactTargetSummary[\s\S]*?\} else \{[\s\S]*?setStatus\(`Done/);
});

test("Strict Fit and subtitle controls preserve accessible status, focus, and exact max-edge language", async () => {
  const [app, css] = await Promise.all([
    read("../src/App.tsx"),
    read("../src/App.css"),
  ]);

  assert.equal((app.match(/id="vfl-subtitle-live-status"/g) ?? []).length, 1);
  assert.match(app, /id="vfl-subtitle-live-status"[\s\S]*?role="status"[\s\S]*?aria-live="polite"[\s\S]*?aria-atomic="true"/);
  assert.equal((app.match(/id="vfl-subtitle-status"/g) ?? []).length, 1);
  assert.match(css, /\.vfl-sr-only/);
  assert.match(app, /clearExternalSubtitle\("External subtitles removed\. No export started\.", true\)/);
  assert.match(app, /focusButtonWhenAvailable\(subtitleBrowseButtonRef\)/);
  assert.match(app, /role="alert"[\s\S]*?subtitleError \?\? externalSubtitleBlockingReason/);
  assert.match(app, /Inline HTML or ASS styling tags are rejected\./);
  assert.match(app, /fixed bottom-centered white text with a black outline/);
  assert.doesNotMatch(app, /Allow the final applicable plan to remove audio/);
  assert.match(app, /<label className="vfl-check vfl-strict-fit-toggle">[\s\S]*?id="vfl-strict-fit"[\s\S]*?<span>Strict Fit<\/span>[\s\S]*?<\/label>/);
  assert.match(app, /subtitleInspecting \? "Wait for external subtitle validation to finish\." : null/);
  assert.match(app, /const encodeBusy =[^;]*subtitleInspecting/);
  assert.match(app, /if \(subtitleInspecting\) throw new Error\("Wait for external subtitle validation to finish\."\)/);
  assert.match(app, /if \(subtitleInspecting\) \{[\s\S]*?return \{ ok: false, message: "Wait for external subtitle validation to finish\." \}/);
  assert.match(app, /Max edge \(px\)/);
  assert.doesNotMatch(app, /\b(?:720p|540p)\b/);

  assert.match(css, /\.vfl-plan-hero\.is-target-missed/);
  assert.match(css, /\.vfl-export-result\.target-missed/);
  assert.match(css, /\.vfl-queue-item\.target-missed/);
  assert.match(css, /\.vfl-corrective-actions/);
});
