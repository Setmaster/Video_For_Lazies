import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import url from "node:url";

import {
  listPortableCompanionFiles,
  portableLinuxDesktopFileName,
  portableLinuxIconFileName,
  tauriSidecarResourceSource,
  tauriSidecarResourceTarget,
} from "../scripts/ffmpegBundle.mjs";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));

test("FFmpeg capability contract keeps canonical LF bytes on every platform", async () => {
  const attributesPath = path.resolve(__dirname, "../../.gitattributes");
  const attributes = await fs.readFile(attributesPath, "utf8");

  assert.ok(
    attributes.split(/\r?\n/).includes("app/ffmpeg-capabilities.json text eol=lf"),
    "the packaged capability contract must not inherit Windows CRLF checkout conversion",
  );
});

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
  assert.match(bundleRaw, /linux64-gpl-shared/);
  assert.match(bundleRaw, /autobuild-2026-06-30-13-34/);
  assert.match(bundleRaw, /ffmpeg-n8\.1\.2-21-gce3c09c101-win64-gpl-shared-8\.1\.zip/);
  assert.match(bundleRaw, /ffmpeg-n8\.1\.2-21-gce3c09c101-linux64-gpl-shared-8\.1\.tar\.xz/);
  assert.match(bundleRaw, /month-end builds/);
  assert.match(bundleRaw, /pinned GPL shared build/i);
  assert.match(bundleRaw, /buildScriptsCommit/);
  assert.match(bundleRaw, /x264Commit/);
  assert.match(bundleRaw, /windowsSourceArchiveNames/);
  assert.match(bundleRaw, /linuxSourceArchiveNames/);
  assert.match(syncRaw, /windowsBuildScriptsArchivePath/);
  assert.match(syncRaw, /windowsX264SourceArchivePath/);
  assert.match(syncRaw, /linuxBuildScriptsArchivePath/);
  assert.match(syncRaw, /linuxX264SourceArchivePath/);
  assert.match(syncRaw, /linuxWrapperScript/);
  assert.match(syncRaw, /LD_LIBRARY_PATH/);
  assert.match(syncRaw, /downloadFileWithCurl/);
  assert.match(syncRaw, /verifyFfmpegCapabilityContract/);
  assert.match(syncRaw, /FFMPEG_CAPABILITY_CONTRACT_FILE_NAME/);
  assert.match(syncRaw, /assertCapabilityContractCopy/);
  assert.match(syncRaw, /DOWNLOAD_TIMEOUT_MS/);
  assert.match(syncRaw, /ARCHIVE_PROCESS_TIMEOUT_MS/);
  assert.match(syncRaw, /child\.on\("close"/);
  assert.doesNotMatch(syncRaw, /currentNotice === expectedNotice/);
  assert.match(syncRaw, /rm\(windowsSidecarDir, \{ recursive: true, force: true \}\)/);
  assert.match(syncRaw, /rm\(linuxSidecarDir, \{ recursive: true, force: true \}\)/);
});

test("tauri config maps the generated current ffmpeg sidecar into app resources", async () => {
  const confPath = path.resolve(__dirname, "../src-tauri/tauri.conf.json");
  const buildScriptPath = path.resolve(__dirname, "../src-tauri/build.rs");
  const makePortablePath = path.resolve(__dirname, "../scripts/make-portable.mjs");
  const raw = await fs.readFile(confPath, "utf8");
  const buildScriptRaw = await fs.readFile(buildScriptPath, "utf8");
  const makePortableRaw = await fs.readFile(makePortablePath, "utf8");
  const json = JSON.parse(raw);
  const resources = json?.bundle?.resources;

  assert.equal(typeof resources, "object");
  assert.equal(resources?.[tauriSidecarResourceSource], tauriSidecarResourceTarget);
  assert.match(buildScriptRaw, /\.ffmpeg-bundle/);
  assert.match(buildScriptRaw, /current/);
  assert.match(makePortableRaw, /listPortableCompanionDirs\(\)/);
});

test("linux portable payload includes launcher icon files", () => {
  const linuxFiles = new Set(listPortableCompanionFiles({ platform: "linux" }).map((file) => file.name));
  const windowsFiles = new Set(listPortableCompanionFiles({ platform: "win32" }).map((file) => file.name));

  assert.ok(linuxFiles.has(portableLinuxIconFileName));
  assert.ok(linuxFiles.has(portableLinuxDesktopFileName));
  assert.equal(windowsFiles.has(portableLinuxIconFileName), false);
  assert.equal(windowsFiles.has(portableLinuxDesktopFileName), false);
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
  const codecSmokeWrapperPath = path.resolve(__dirname, "../scripts/run-portable-codec-plan-smoke.mjs");
  const makePortablePath = path.resolve(__dirname, "../scripts/make-portable.mjs");
  const releaseRunnerPath = path.resolve(__dirname, "../scripts/run-release-portable.mjs");
  const smokePowerShellPath = path.resolve(__dirname, "../scripts/windows-portable-smoke.ps1");
  const exportSmokePowerShellPath = path.resolve(__dirname, "../scripts/windows-portable-export-smoke.ps1");
  const raw = await fs.readFile(packageJsonPath, "utf8");
  const json = JSON.parse(raw);
  const portableScript = json?.scripts?.portable ?? "";
  const updateHelperScript = json?.scripts?.["build:update-helper"] ?? "";
  const smokeScript = json?.scripts?.["smoke:portable"] ?? "";
  const exportSmokeScript = json?.scripts?.["smoke:portable:export"] ?? "";
  const codecSmokeScript = json?.scripts?.["smoke:portable:codecs"] ?? "";
  const releaseScript = json?.scripts?.["release:portable"] ?? "";
  const makePortableRaw = await fs.readFile(makePortablePath, "utf8");
  const smokePowerShellRaw = await fs.readFile(smokePowerShellPath, "utf8");

  await fs.access(smokeWrapperPath);
  await fs.access(exportSmokeWrapperPath);
  await fs.access(codecSmokeWrapperPath);
  await fs.access(makePortablePath);
  await fs.access(releaseRunnerPath);
  await fs.access(smokePowerShellPath);
  await fs.access(exportSmokePowerShellPath);
  assert.match(portableScript, /\btauri build\b/);
  assert.match(portableScript, /--no-bundle/);
  assert.match(portableScript, /build:update-helper/);
  assert.match(portableScript, /smoke:portable/);
  assert.match(updateHelperScript, /--bin vfl-update-helper/);
  assert.equal(smokeScript, "node scripts/run-portable-smoke.mjs");
  assert.equal(exportSmokeScript, "node scripts/run-portable-export-smoke.mjs");
  assert.equal(codecSmokeScript, "node scripts/run-portable-codec-plan-smoke.mjs");
  assert.equal(releaseScript, "node scripts/run-release-portable.mjs");
  assert.match(smokePowerShellRaw, /\[int\]\$StartupTimeoutSeconds = 30/);
  assert.match(smokePowerShellRaw, /\[int\]\$MaxAttempts = 8/);
  assert.match(smokePowerShellRaw, /vfl-portable-startup-webview-/);
  assert.match(smokePowerShellRaw, /WEBVIEW2_USER_DATA_FOLDER = \$webViewDataRoot/);
  assert.match(smokePowerShellRaw, /taskkill\.exe \/PID \$process\.Id \/T \/F/);
  assert.match(smokePowerShellRaw, /\$cleanupAttempt -le 6/);
  assert.match(smokePowerShellRaw, /Start-Sleep -Milliseconds 250/);
  assert.match(makePortableRaw, /cleanupPaths:\s*listPortableLegacyPaths\(\)/);
  assert.match(makePortableRaw, /generatePortableDocs/);
  assert.doesNotMatch(portableScript, /\bcargo build\b/);
});

test("portable export smoke enforces ordered workflow and interaction stage history", async () => {
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
  const requiredWorkflowStages = [
    "workflow-recipe-ready",
    "workflow-recipe-saved",
    "workflow-queue-ready",
    "workflow-queue-complete",
    "workflow-ready",
  ];

  for (const [label, source] of [["Linux wrapper", wrapperRaw], ["Windows runner", raw]]) {
    let previousIndex = -1;
    for (const stage of requiredWorkflowStages) {
      const stageIndex = source.indexOf(`"${stage}"`);
      assert.ok(stageIndex > previousIndex, `${label} must require ${stage} after the preceding workflow stage.`);
      previousIndex = stageIndex;
    }
  }

  assert.match(raw, /interaction-ready/);
  assert.match(raw, /workflow-ready/);
  assert.match(raw, /VFL_SMOKE_WORKFLOW_QUEUE"\] = "1"/);
  assert.match(raw, /WEBVIEW2_USER_DATA_FOLDER"\] = \$webViewDataRoot/);
  assert.match(raw, /webview2-user-data/);
  assert.match(raw, /taskkill\.exe" \/PID \$process\.Id \/T \/F/);
  assert.match(raw, /"workflow-recipe-ready"\s*\{\s*\$sentKeyboardStages\[\$status\.stage\] = \$true\s*Send-SmokeKeySequence -Process \$process -Keys @\("\{ENTER\}"\)/);
  assert.match(raw, /"workflow-queue-ready"\s*\{\s*\$sentKeyboardStages\[\$status\.stage\] = \$true\s*Send-SmokeKeySequence -Process \$process -Keys @\("\{ENTER\}"\)/);
  assert.match(raw, /stageHistory/);
  assert.match(raw, /missed required app stages/);
  assert.match(raw, /required app stages were out of order/);
  assert.match(raw, /expectedDurationS/);
  assert.match(raw, /trimStartS/);
  assert.match(raw, /trimEndS/);
  assert.match(raw, /output duration mismatch/);
  assert.match(raw, /\[double\]\$SizeLimitMb = 0/);
  assert.match(raw, /\[int\]\$InputWidth = 640/);
  assert.match(raw, /\[ValidateSet\("mp4", "webm", "mp3"\)\]\[string\]\$OutputFormat = "mp4"/);
  assert.match(raw, /VFL_SMOKE_FORMAT"\] = \$OutputFormat/);
  assert.match(bundleRaw, /variant: "win64-gpl-shared"/);
  assert.match(wrapperRaw, /-SizeLimitMb/);
  assert.match(wrapperRaw, /"workflow-ready"/);
  assert.match(wrapperRaw, /VFL_SMOKE_WORKFLOW_QUEUE:\s*"1"/);
  assert.match(wrapperRaw, /XDG_DATA_HOME:\s*xdgDataHome/);
  assert.match(wrapperRaw, /XDG_CONFIG_HOME:\s*xdgConfigHome/);
  assert.match(wrapperRaw, /XDG_CACHE_HOME:\s*xdgCacheHome/);
  assert.match(wrapperRaw, /app\.stdout\.log/);
  assert.match(wrapperRaw, /app\.stderr\.log/);
  assert.match(wrapperRaw, /stdio:\s*\["ignore", stdoutFileHandle\.fd, stderrFileHandle\.fd\]/);
  assert.match(wrapperRaw, /required app stages were out of order/);
  assert.match(wrapperRaw, /Portable export smoke evidence retained at/);
  assert.match(wrapperRaw, /if \(succeeded\)[\s\S]*?fs\.rm\(smokeRoot/);
  assert.match(wrapperRaw, /-InputWidth/);
  assert.match(wrapperRaw, /-OutputFormat/);
  assert.match(wrapperRaw, /outputFormat = "mp4"/);
  assert.match(releaseRaw, /assertBundledFfmpegCapabilities/);
  assert.match(releaseRaw, /verifyFfmpegCapabilityContract/);
  assert.match(releaseRaw, /assertCapabilityContractCopy/);
  assert.match(releaseRaw, /FFMPEG_CAPABILITY_CONTRACT_FILE_NAME/);
  assert.match(releaseRaw, /PORTABLE_BUILD_TIMEOUT_MS/);
  assert.match(releaseRaw, /DEFAULT_COMMAND_TIMEOUT_MS/);
  assert.match(releaseRaw, /runBundledEncodeSmoke/);
  assert.match(releaseRaw, /runPortableMediaDepthSmoke\(\{ portableDir, platform \}\)/);
  assert.match(releaseRaw, /runPortableExportSmoke\(\{ portableDir, outputFormat: "webm" \}\)/);
  assert.match(releaseRaw, /runPortableCodecPlanSmoke\(\{ portableDir \}\)/);
  assert.match(releaseRaw, /getFfmpegSourceArchiveNames/);
  assert.match(appRaw, /smokeStatusWriteRef/);
  assert.match(appRaw, /runSmokeWorkflowChecks/);
  assert.match(appRaw, /smokeStageHistoryRef/);
  assert.match(appRaw, /stageHistory: smokeStageHistoryRef\.current/);
  assert.match(appRaw, /trimStartS: extra\.trimStartS \?\? null/);
  assert.match(appRaw, /expectedDurationS: extra\.expectedDurationS \?\? null/);
  assert.match(releaseRaw, /timeoutSeconds:\s*300/);
  assert.match(releaseRaw, /sizeLimitMb:\s*0\.3/);
  assert.match(releaseRaw, /inputWidth:\s*1920/);
  assert.match(releaseRaw, /inputHeight:\s*1080/);
});

test("portable codec-plan smoke covers remux, both partial directions, and incompatible MP4/WebM paths", async () => {
  const matrixPath = path.resolve(__dirname, "../scripts/run-portable-codec-plan-smoke.mjs");
  const wrapperPath = path.resolve(__dirname, "../scripts/run-portable-export-smoke.mjs");
  const matrixRaw = await fs.readFile(matrixPath, "utf8");
  const wrapperRaw = await fs.readFile(wrapperPath, "utf8");

  assert.match(matrixRaw, /MP4 compatible remux/);
  assert.match(matrixRaw, /MP4 video copy and audio re-encode/);
  assert.match(matrixRaw, /MP4 video re-encode and audio copy/);
  assert.match(matrixRaw, /MP4 incompatible full re-encode/);
  assert.match(matrixRaw, /WebM compatible remux/);
  assert.match(matrixRaw, /WebM incompatible full re-encode/);
  assert.match(matrixRaw, /expectedVideoAction: "copy"/);
  assert.match(matrixRaw, /expectedAudioAction: "encode"/);
  assert.match(wrapperRaw, /show_data_hash/);
  assert.match(wrapperRaw, /output permissions mismatch/);
  assert.match(wrapperRaw, /expected stream action/);
  assert.match(wrapperRaw, /useFullDuration/);
});

test("settings rail scrolls while plan card stays in normal flow", async () => {
  const cssPath = path.resolve(__dirname, "../src/App.css");
  const appPath = path.resolve(__dirname, "../src/App.tsx");
  const raw = await fs.readFile(cssPath, "utf8");
  const appRaw = await fs.readFile(appPath, "utf8");

  // The rail is the single scroll container; no card may float over siblings.
  assert.match(raw, /\.vfl-rail\s*{[\s\S]*?overflow-y:\s*auto;[\s\S]*?}/);
  assert.doesNotMatch(raw, /position:\s*sticky/);
  assert.doesNotMatch(appRaw, /vfl-sticky-card/);
});
