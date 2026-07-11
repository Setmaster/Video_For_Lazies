import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("packaged G5 queue smoke proves a real target miss after failure recovery", async () => {
  const [app, types] = await Promise.all([
    fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8"),
    fs.readFile(path.resolve(__dirname, "../src/lib/types.ts"), "utf8"),
  ]);
  const start = app.indexOf("async function runSmokeWorkflowChecks");
  const end = app.indexOf("async function runSmokeAccessibilityChecks", start);
  assert.ok(start >= 0 && end > start, "workflow smoke implementation must remain inspectable");
  const workflow = app.slice(start, end);

  assert.match(types, /g5QueueTargetMiss\?: boolean \| null/);
  assert.match(workflow, /const expectQueueTargetMiss = smokeConfig\.workflowQueueExport && smokeConfig\.g5QueueTargetMiss === true/);
  assert.match(workflow, /item\.status === "target-missed"/);
  assert.match(workflow, /item\.lastOutcome\?\.kind === "target-missed"/);
  assert.match(workflow, /item\.lastOutcome\.targetResult\?\.status === "missed"/);
  assert.match(workflow, /attempt\.kind === "failed" && attempt\.diagnostics/);
  assert.match(workflow, /attempt\.kind === "target-missed" && attempt\.targetResult\?\.status === "missed"/);
  assert.match(workflow, /\[data-queue-action="retry"\]/);
  assert.match(workflow, /\[data-queue-action="duplicate"\]/);
  assert.match(workflow, /!retryControl\.disabled/);
  assert.match(workflow, /!duplicateControl\.disabled/);

  const mountedEvidence = workflow.indexOf("if (!recoveryEvidenceMounted)");
  const completionStage = workflow.indexOf('reportSmokeStatus("workflow-queue-complete"');
  assert.ok(mountedEvidence >= 0 && completionStage > mountedEvidence, "completion must follow mounted recovery evidence");
  assert.match(workflow, /if \(!expectQueueTargetMiss\) return true/);
  assert.match(workflow, /backend success, diagnostics, and history checks passed/);
});
