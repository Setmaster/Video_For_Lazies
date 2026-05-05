import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

async function readAppSource() {
  return fs.readFile(path.resolve(__dirname, "../src/App.tsx"), "utf8");
}

test("manual update check forces the updater without changing startup checks", async () => {
  const raw = await readAppSource();

  assert.match(raw, /check_for_update", \{ force: false \}/);
  assert.match(raw, /async function checkForUpdatesNow\(\)/);
  assert.match(raw, /check_for_update", \{ force: true \}/);
  assert.match(raw, /setManualUpdateStatus\(result\.reason \?\? "Video For Lazies is up to date\."\)/);
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
