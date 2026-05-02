import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  tauriWindowsSidecarResourceSource,
  tauriWindowsSidecarResourceTarget,
} from "../scripts/ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("tauri build hooks prepare the bundled ffmpeg sidecar", async () => {
  const confPath = path.resolve(__dirname, "../src-tauri/tauri.conf.json");
  const bundleConfigPath = path.resolve(__dirname, "../scripts/ffmpegBundle.mjs");
  const syncScriptPath = path.resolve(__dirname, "../scripts/sync-ffmpeg-sidecar.mjs");
  const raw = await fs.readFile(confPath, "utf8");
  const bundleRaw = await fs.readFile(bundleConfigPath, "utf8");
  const syncRaw = await fs.readFile(syncScriptPath, "utf8");
  const json = JSON.parse(raw);

  assert.match(json?.build?.beforeBuildCommand ?? "", /prepare:ffmpeg-sidecar/);
  assert.match(json?.build?.beforeDevCommand ?? "", /prepare:ffmpeg-sidecar/);
  assert.match(bundleRaw, /win64-gpl-shared/);
  assert.match(bundleRaw, /autobuild-2026-05-02-13-12/);
  assert.match(bundleRaw, /ffmpeg-n8\.1-11-g75d37c499d-win64-gpl-shared-8\.1\.zip/);
  assert.match(bundleRaw, /pinned GPL shared build/i);
  assert.match(syncRaw, /missing libx264/i);
});

test("tauri config maps the staged ffmpeg sidecar into app resources", async () => {
  const confPath = path.resolve(__dirname, "../src-tauri/tauri.conf.json");
  const raw = await fs.readFile(confPath, "utf8");
  const json = JSON.parse(raw);
  const resources = json?.bundle?.resources;

  assert.equal(typeof resources, "object");
  assert.equal(resources?.[tauriWindowsSidecarResourceSource], tauriWindowsSidecarResourceTarget);
});

test("tauri window config keeps a supported minimum app size", async () => {
  const confPath = path.resolve(__dirname, "../src-tauri/tauri.conf.json");
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const confRaw = await fs.readFile(confPath, "utf8");
  const appRaw = await fs.readFile(appPath, "utf8");
  const json = JSON.parse(confRaw);
  const mainWindow = json?.app?.windows?.[0] ?? {};

  assert.equal(mainWindow.minWidth, 960);
  assert.equal(mainWindow.minHeight, 720);
  assert.match(appRaw, /setMinSize/);
  assert.match(appRaw, /setSizeConstraints/);
});

test("portable build uses tauri build instead of raw cargo build", async () => {
  const packageJsonPath = path.resolve(__dirname, "../package.json");
  const smokeWrapperPath = path.resolve(__dirname, "../scripts/run-portable-smoke.mjs");
  const exportSmokeWrapperPath = path.resolve(__dirname, "../scripts/run-portable-export-smoke.mjs");
  const makePortablePath = path.resolve(__dirname, "../scripts/make-portable.mjs");
  const releaseRunnerPath = path.resolve(__dirname, "../scripts/run-release-portable.mjs");
  const smokePowerShellPath = path.resolve(__dirname, "../scripts/windows-portable-smoke.ps1");
  const exportSmokePowerShellPath = path.resolve(__dirname, "../scripts/windows-portable-export-smoke.ps1");
  const raw = await fs.readFile(packageJsonPath, "utf8");
  const json = JSON.parse(raw);
  const portableScript = json?.scripts?.portable ?? "";
  const smokeScript = json?.scripts?.["smoke:portable"] ?? "";
  const exportSmokeScript = json?.scripts?.["smoke:portable:export"] ?? "";
  const releaseScript = json?.scripts?.["release:portable"] ?? "";
  const makePortableRaw = await fs.readFile(makePortablePath, "utf8");

  await fs.access(smokeWrapperPath);
  await fs.access(exportSmokeWrapperPath);
  await fs.access(makePortablePath);
  await fs.access(releaseRunnerPath);
  await fs.access(smokePowerShellPath);
  await fs.access(exportSmokePowerShellPath);
  assert.match(portableScript, /\btauri build\b/);
  assert.match(portableScript, /--no-bundle/);
  assert.match(portableScript, /smoke:portable/);
  assert.equal(smokeScript, "node scripts/run-portable-smoke.mjs");
  assert.equal(exportSmokeScript, "node scripts/run-portable-export-smoke.mjs");
  assert.equal(releaseScript, "node scripts/run-release-portable.mjs");
  assert.match(makePortableRaw, /cleanupPaths:\s*listPortableLegacyPaths\(\)/);
  assert.doesNotMatch(portableScript, /\bcargo build\b/);
});

test("portable export smoke enforces the interaction-ready stage history", async () => {
  const bundleConfigPath = path.resolve(__dirname, "../scripts/ffmpegBundle.mjs");
  const exportSmokeWrapperPath = path.resolve(__dirname, "../scripts/run-portable-export-smoke.mjs");
  const releaseRunnerPath = path.resolve(__dirname, "../scripts/run-release-portable.mjs");
  const exportSmokePowerShellPath = path.resolve(__dirname, "../scripts/windows-portable-export-smoke.ps1");
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const bundleRaw = await fs.readFile(bundleConfigPath, "utf8");
  const wrapperRaw = await fs.readFile(exportSmokeWrapperPath, "utf8");
  const releaseRaw = await fs.readFile(releaseRunnerPath, "utf8");
  const raw = await fs.readFile(exportSmokePowerShellPath, "utf8");
  const appRaw = await fs.readFile(appPath, "utf8");

  assert.match(raw, /interaction-ready/);
  assert.match(raw, /stageHistory/);
  assert.match(raw, /missed required app stages/);
  assert.match(raw, /expectedDurationS/);
  assert.match(raw, /trimStartS/);
  assert.match(raw, /trimEndS/);
  assert.match(raw, /output duration mismatch/);
  assert.match(raw, /\[double\]\$SizeLimitMb = 0/);
  assert.match(raw, /\[int\]\$InputWidth = 640/);
  assert.match(bundleRaw, /variant: "win64-gpl-shared"/);
  assert.match(wrapperRaw, /-SizeLimitMb/);
  assert.match(wrapperRaw, /-InputWidth/);
  assert.match(releaseRaw, /assertBundledLibx264/);
  assert.match(releaseRaw, /missing libx264/);
  assert.match(appRaw, /smokeStatusWriteRef/);
  assert.match(appRaw, /trimStartS: extra\.trimStartS \?\? null/);
  assert.match(appRaw, /expectedDurationS: extra\.expectedDurationS \?\? null/);
  assert.match(releaseRaw, /sizeLimitMb:\s*0\.3/);
  assert.match(releaseRaw, /inputWidth:\s*1920/);
  assert.match(releaseRaw, /inputHeight:\s*1080/);
});

test("general tab side panel keeps current plan in normal flow", async () => {
  const cssPath = path.resolve(__dirname, "../src/App.css");
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const raw = await fs.readFile(cssPath, "utf8");
  const appRaw = await fs.readFile(appPath, "utf8");

  assert.match(raw, /@media \(min-width: 1180px\) and \(min-height: 820px\)\s*\{\s*\.vfl-grid-general/s);
  assert.doesNotMatch(raw, /\.vfl-general-side\s+\.vfl-sticky-card/);
  assert.doesNotMatch(raw, /position:\s*sticky/);
  assert.doesNotMatch(appRaw, /vfl-sticky-card/);
});
