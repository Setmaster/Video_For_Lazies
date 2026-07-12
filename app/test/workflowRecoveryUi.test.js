import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

async function read(relativePath) {
  return fs.readFile(path.resolve(__dirname, relativePath), "utf8");
}

function between(source, startMarker, endMarker) {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start + startMarker.length);
  assert.ok(start >= 0, `Missing start marker: ${startMarker}`);
  assert.ok(end > start, `Missing end marker after ${startMarker}: ${endMarker}`);
  return source.slice(start, end);
}

test("App uses one reducer-backed queue identity and closes queue continuation races", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /useState<ExportQueueState>\(\(\) => createExportQueueState\(\)\)/);
  assert.match(app, /const exportQueueStateRef = useRef<ExportQueueState>\(exportQueueState\)/);
  assert.match(app, /reduceExportQueue\(exportQueueStateRef\.current, action\)/);
  assert.doesNotMatch(app, /exportQueueRef|queueRunningRef|queueActiveItemIdRef|queueIdRef/);
  assert.match(app, /queueRunId: number \| null/);
  assert.match(app, /completedQueueRunId = completedContext\.queueRunId/);
  assert.match(app, /type: "settled",\s+itemId: completedQueueItemId,\s+runId: completedQueueRunId/);
  assert.match(app, /pendingEncodeRef\.current !== null\) return;/);
  assert.match(app, /Stop the queue before starting a separate export/);
  assert.match(app, /disabled=\{!exportReady \|\| encodeBusy\}/);
  assert.match(app, /activeQueuePosition/);
  assert.match(app, /queueStopRevisionRef/);
  assert.match(app, /function recordQueueStopIntent\(\)/);
  assert.match(app, /if \(exportQueueStateRef\.current\.active !== null\) \{\s+recordQueueStopIntent\(\)/);
  assert.match(app, /resumeQueueAfterPreparation/);
  assert.match(app, /intent\.stopRevision === queueStopRevisionRef\.current/);
  assert.match(app, /const queuePreparationCountRef = useRef\(0\)/);
  assert.match(app, /queuePreparationCountRef\.current \+= 1/);
  assert.match(app, /queuePreparationCountRef\.current === 0[\s\S]*?startNextQueuedItem/);
  assert.match(app, /const encodeBusy = attemptUi\.isActive \|\| queueRunning \|\| queuePreparationBusy \|\| queueSnapshotApplying/);
});

test("drop, output claims, retry, snapshot, and diagnostics are wired through production paths", async () => {
  const app = await read("../src/App.tsx");

  assert.match(app, /const handleDroppedPaths = useEffectEvent\(async \(paths: string\[\]\)/);
  assert.match(app, /busy:\s+jobIdRef\.current !== null \|\|\s+pendingEncodeRef\.current !== null \|\|\s+exportQueueStateRef\.current\.autoRun \|\|\s+queuePreparationCountRef\.current > 0 \|\|\s+queueSnapshotApplyingRef\.current/);
  assert.match(app, /queueCapacity: exportQueueRemainingCapacity\(exportQueueStateRef\.current\)/);
  assert.match(app, /isDropAllowed: \(\) => !modalOpenRef\.current/);
  assert.match(app, /exportQueueState\.pathRevision/);
  assert.match(app, /exportQueueClaimsOutputPath\(exportQueueState, outputPath\)/);
  assert.match(app, /That output path is already reserved by an item in the export queue/);
  assert.match(app, /pendingEncodeRef\.current\?\.outputPath/);
  assert.match(app, /claimedOutputPathsForPreparation/);
  assert.match(app, /const requestTemplate = buildSettingsOnlyRequest/);
  assert.match(app, /const batchFormat = requestTemplate\.format/);
  assert.match(app, /async function retryQueueItem/);
  assert.match(app, /suggestedOutputForInput\(item\.inputPath, item\.format, takenPaths\)/);
  assert.match(app, /async function applyQueueItemSnapshot/);
  assert.match(app, /invoke<VideoProbe>\("probe_video", \{ path: item\.inputPath \}\)/);
  assert.match(app, /queueSnapshotApplyTokenRef\.current !== token/);
  assert.match(app, /JSON\.stringify\(currentItem\.request\) !== requestFingerprint/);
  assert.match(app, /p\.outputPath \?\? completedContext\.outputPath/);
  assert.match(app, /className="vfl-export-diagnostics vfl-queue-diagnostics"/);
  assert.match(app, /data-queue-action="retry"/);
  assert.match(app, /data-queue-action="duplicate"/);
  assert.match(app, /data-queue-action="apply-snapshot"/);
  assert.match(app, /data-queue-action="remove"/);
  assert.match(app, /Recent attempt history \(\{item\.history\.length\} retained\)/);
  assert.match(app, /outcome\?\.kind === "done" \? outcome\.outputPath : null/);
  assert.match(app, /function focusQueueAfterMutation/);
  assert.match(app, /remainingAttempts = 25/);
  assert.match(app, /focusQueueAfterMutation\(preferred, remainingAttempts - 1\)/);
  assert.match(app, /queueRegionRef\.current\?\.isConnected\) queueRegionRef\.current\.focus\(\)/);
  assert.match(app, /removeQueueItem[\s\S]*?focusQueueAfterMutation\(queueFallbackButtonRef\)/);
  assert.match(
    app,
    /const getRemoveDuplicateButton = \(\) =>[\s\S]*?waitForSmokeCondition\(\(\) => getRemoveDuplicateButton\(\) !== null\)[\s\S]*?const removeDuplicateButton = getRemoveDuplicateButton\(\)/,
  );
  assert.match(app, /const resumed = resumeQueueAfterPreparation\(next, resumeIntent\);[\s\S]*?resumed\.autoRun \? queueStopButtonRef : queueRunButtonRef/);
  assert.match(app, /ref=\{queueRegionRef\}[\s\S]*?role="group"[\s\S]*?aria-label="Export queue controls"/);
});

test("workflow smoke waits for committed mounted controls instead of fixed render sleeps", async () => {
  const app = await read("../src/App.tsx");
  const workflow = between(
    app,
    "async function runSmokeWorkflowChecks",
    "async function runSmokeAccessibilityChecks",
  );

  assert.doesNotMatch(workflow, /waitMs\((?:180|100|160|120)\)/);
  assert.match(
    workflow,
    /function isMountedEnabledButton[\s\S]*?button\?\.isConnected && !button\.disabled/,
  );

  const applyClick = workflow.indexOf("applyRecipeButton.click();");
  const applyWait = workflow.indexOf("const applyCommitted = await waitForSmokeCondition", applyClick);
  const committedStatus = workflow.indexOf("status.textContent?.startsWith(`Applied ${smokeRecipeName}.`)", applyWait);
  const freshRename = workflow.indexOf("const renameRecipeButton = getRecipeActions(smokeRecipeName).rename", committedStatus);
  assert.ok(applyClick >= 0 && applyWait > applyClick && committedStatus > applyWait && freshRename > committedStatus);
  assert.doesNotMatch(workflow.slice(0, applyClick), /const renameRecipeButton\s*=/);
  assert.match(
    workflow,
    /const renameDialogMounted = await waitForSmokeCondition[\s\S]*?document\.activeElement === input[\s\S]*?"Rename recipe"/,
  );

  for (const [startMarker, endMarker] of [
    ["function getRecipeNameDialogControls", "try {"],
    ["const getSaveRecipeButton", "const saveRecipeMounted"],
    ["const getRecipeActions", "const restoredMounted"],
    ["const getDeleteDialogControls", "const deleteDialogMounted"],
    ["const getDuplicateButton", "const duplicateMounted"],
    ["const getRemoveDuplicateButton", "const removeDuplicateMounted"],
    ["const getApplySnapshotButton", "const snapshotMounted"],
    ["const getFailureControls", "const failureMounted"],
  ]) {
    const getter = between(workflow, startMarker, endMarker);
    assert.match(getter, /isMountedEnabledButton/);
  }
  assert.match(between(workflow, "const getRecipeActions", "const restoredMounted"), /row\?\.isConnected/);
  assert.match(between(workflow, "const getDeleteDialogControls", "const deleteDialogMounted"), /dialog\?\.isConnected/);
  assert.match(between(workflow, "const getFailureControls", "const failureMounted"), /diagnostics\?\.isConnected/);

  assert.match(
    workflow,
    /const retryPassed = await waitForSmokeCondition[\s\S]*?runQueueButton\?\.isConnected[\s\S]*?!runQueueButton\.disabled[\s\S]*?document\.activeElement === runQueueButton/,
  );
  assert.match(workflow, /if \(!isMountedEnabledButton\(runQueueButton\)\)/);
});

test("user recipe management is accessible, persist-first, and explicit about privacy", async () => {
  const [app, dialog, recipes] = await Promise.all([
    read("../src/App.tsx"),
    read("../src/components/UserRecipeDialog.tsx"),
    read("../src/lib/userRecipes.mjs"),
  ]);

  assert.match(app, /persistUserRecipeStore\(localStorage, userRecipeStore, nextRecipes\)/);
  assert.match(app, /if \(!persisted\.ok\)/);
  assert.match(app, /setUserRecipeStore\(persisted\.store\)/);
  assert.match(app, /Save current settings/);
  assert.match(app, /currentSettingsSummary=\{currentRecipeSettingsSummary\}/);
  assert.match(app, /audioEnabled: reusableAudioEnabled/);
  assert.match(app, /cloneUserRecipeSettings\(currentRecipeSettings\) \?\? currentRecipeSettings/);
  assert.match(app, /canonicalCurrentRecipeSettings\.sizeLimitMb/);
  assert.match(app, /recipe\.partial && format === "mp3"/);
  assert.match(app, /if \(format === "mp3"\) \{[\s\S]*?available only for MP4 or WebM video exports/);
  assert.match(app, /aria-label=\{`Apply \$\{recipe\.name\}`\}/);
  assert.match(app, /aria-label=\{`Rename \$\{recipe\.name\}`\}/);
  assert.match(app, /aria-label=\{`Delete \$\{recipe\.name\}`\}/);
  assert.match(dialog, /role="alertdialog"/);
  assert.match(dialog, /label htmlFor="vfl-user-recipe-name"/);
  assert.match(dialog, /currentSettingsSummary: string/);
  assert.match(dialog, /saving[\s\S]*?"vfl-recipe-dialog-description vfl-recipe-values-summary vfl-recipe-privacy-summary"[\s\S]*?: "vfl-recipe-dialog-description"/);
  assert.match(dialog, /id="vfl-recipe-values-summary"/);
  assert.match(dialog, /<strong>Current values:<\/strong> \{currentSettingsSummary\}/);
  assert.match(dialog, /Saved fields:/);
  assert.match(dialog, /Never saved:/);
  assert.match(dialog, /Metadata privacy remains a separate global setting/);
  assert.match(recipes, /USER_RECIPE_STORAGE_KEY = "vfl:user-recipes"/);
  assert.doesNotMatch(app, /vfl:queue|localStorage\.(?:setItem|getItem)\([^\n]*queue/i);
});

test("packaged export proof exercises restart, real queue failure, retry, success, and failure diagnostics", async () => {
  const [app, runner, windows, rust] = await Promise.all([
    read("../src/App.tsx"),
    read("../scripts/run-portable-export-smoke.mjs"),
    read("../scripts/windows-portable-export-smoke.ps1"),
    read("../src-tauri/src/lib.rs"),
  ]);

  for (const stage of [
    "workflow-recipe-ready",
    "workflow-recipe-saved",
    "workflow-queue-ready",
    "workflow-queue-complete",
    "workflow-ready",
  ]) {
    assert.match(app, new RegExp(`"${stage}"`));
    assert.match(runner, new RegExp(`"${stage}"`));
    assert.match(windows, new RegExp(`"${stage}"`));
  }
  assert.match(app, /runSmokeWorkflowChecks/);
  assert.match(app, /sessionStorage\.setItem\(SMOKE_WORKFLOW_SESSION_KEY/);
  assert.match(app, /window\.location\.reload\(\)/);
  assert.match(app, /JSON\.stringify\(smokeConfig\.inputPath\)\.slice\(1, -1\)/);
  assert.match(app, /JSON\.stringify\(smokeConfig\.outputPath\)\.slice\(1, -1\)/);
  assert.match(app, /saveConfirm\.click\(\)/);
  assert.match(app, /workflowQueueExport/);
  assert.match(app, /runQueue\(\)/);
  assert.match(app, /item\?\.status === "failed" && item\.lastOutcome\?\.diagnostics/);
  assert.match(app, /function queueRuntimeSummary\(itemId: number\)/);
  assert.match(app, /listeners=\$\{encodeEventsReadyRef\.current \? "ready" : "not-ready"\}/);
  assert.match(app, /item\?\.status === "done"/);
  assert.match(app, /item\.history\.some\(\(attempt\) => attempt\.kind === "failed" && attempt\.diagnostics\)/);
  assert.match(app, /reportAsSmokeResult: true/);
  assert.match(app, /Packaged recipe restoration, queue recovery, multi-file routing, snapshot, diagnostics, focus, and retry checks passed/);
  assert.match(runner, /XDG_DATA_HOME/);
  assert.match(runner, /app\.stdout\.log/);
  assert.match(windows, /WEBVIEW2_USER_DATA_FOLDER/);
  assert.match(windows, /taskkill\.exe" \/PID \$process\.Id \/T \/F/);
  assert.match(rust, /failed_encode_diagnostics/);
  assert.match(rust, /diagnostics: Some\(diagnostics\)/);
});
