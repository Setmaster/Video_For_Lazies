import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

async function readAppSource() {
  return fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
}

async function readTypesSource() {
  return fs.readFile(path.resolve(__dirname, "../src/lib/types.ts"), "utf8");
}

test("manual update check forces the updater without changing startup checks", async () => {
  const raw = await readAppSource();

  assert.match(raw, /check_for_update", \{\s*force: false,\s*onEvent: createUpdateProgressChannel\(\),/s);
  assert.match(raw, /async function checkForUpdatesNow\(\)/);
  assert.match(raw, /check_for_update", \{\s*force: true,\s*onEvent: createUpdateProgressChannel\(setManualUpdateStatus\),/s);
  assert.match(raw, /setManualUpdateStatus\(result\.reason \?\? "Video For Lazies is up to date\."\)/);
});

test("update application streams safe phase progress and blocks conflicting media work", async () => {
  const raw = await readAppSource();

  assert.match(raw, /new Channel<UpdateProgressEvent>/);
  assert.match(raw, /prepare_and_apply_update", \{\s*onEvent: createUpdateProgressChannel\(setUpdateStatus\),/s);
  assert.match(raw, /finalize_update_startup", \{\s*onEvent: createUpdateProgressChannel\(\),/s);
  assert.match(raw, /aria-label="Update progress"/);
  assert.match(raw, /aria-valuetext=\{updateProgressUi\.valueText\}/);
  assert.match(raw, /role=\{updatePublicError \? "alert" : "status"\}/);
  assert.match(raw, /if \(updateBusyRef\.current\) \{\s*return \{ ok: false, message: "Finish the current update before starting an export\." \};/s);
  assert.match(raw, /updating \? "an update is still being prepared" : null/);
  assert.match(raw, /status === "recoveryRequired"/);
  assert.doesNotMatch(raw, /setUpdateStatus\(coerceErrorMessage\(error/);

  const busyStart = raw.indexOf("const encodeBusy =");
  const busyEnd = raw.indexOf("const lastExportIsCurrentOutcome", busyStart);
  const encodeBusy = raw.slice(busyStart, busyEnd);
  assert.match(encodeBusy, /cropDetecting/);
  assert.match(encodeBusy, /frameSaving/);
  assert.match(encodeBusy, /updateBusy/);

  const applyStart = raw.indexOf("async function applyUpdate()");
  const applyEnd = raw.indexOf("const previewColorFilter", applyStart);
  const applyUpdate = raw.slice(applyStart, applyEnd);
  assert.match(applyUpdate, /cropDetecting/);
  assert.match(applyUpdate, /frameSaving/);

  const dropStart = raw.indexOf("const handleDroppedPaths =");
  const dropEnd = raw.indexOf("const videoSrc =", dropStart);
  assert.match(raw.slice(dropStart, dropEnd), /if \(updateBusyRef\.current\)/);
});

test("startup recovery responses stay visible, busy, and suppress ordinary update checks", async () => {
  const [raw, types] = await Promise.all([readAppSource(), readTypesSource()]);

  assert.match(types, /\| "recovering"\s*\| "elevatingRecovery"\s*\| "recoveryRequired"/s);
  const startupStart = raw.indexOf('invoke<UpdateStartupResponse>("finalize_update_startup"');
  const startupEnd = raw.indexOf('invoke<boolean>("elevated_update_pending")', startupStart);
  assert.ok(startupStart >= 0 && startupEnd > startupStart);
  const startup = raw.slice(startupStart, startupEnd);
  assert.match(startup, /startup\.status === "recovering" \|\| startup\.status === "elevatingRecovery"/);
  assert.match(startup, /updateBusyRef\.current = true/);
  assert.match(startup, /setUpdateBusy\(true\)/);
  assert.match(startup, /setUpdateStatus\(startup\.message\)/);
  assert.match(startup, /setUpdateStartupNotice\(\{ kind: "status", message: startup\.message \}\)/);
  assert.match(startup, /return;/);
});

test("about modal exposes a compact manual update command", async () => {
  const raw = await readAppSource();

  assert.match(raw, /aria-label="About & updates"/);
  assert.match(raw, /className="vfl-about-modal"/);
  assert.doesNotMatch(raw, /<div className="vfl-section-title">About & Legal<\/div>/);
  assert.match(raw, /<div className="vfl-summary-label">Updates<\/div>/);
  assert.match(raw, /manualUpdateBusy \? "Checking\.\.\." : "Check for updates"/);
  assert.match(raw, /className="vfl-update-inline-status" role="status" aria-live="polite"/);
});
