import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const appRoot = path.resolve(__dirname, "..");

async function readAppFile(relativePath) {
  return fs.readFile(path.resolve(appRoot, relativePath), "utf8");
}

test("Windows update helper has a helper-only asInvoker manifest", async () => {
  const [buildScript, helperManifest, mainManifest] = await Promise.all([
    readAppFile("src-tauri/build.rs"),
    readAppFile("src-tauri/vfl-update-helper.manifest"),
    readAppFile("src-tauri/video-for-lazies.manifest"),
  ]);

  assert.match(helperManifest, /requestedExecutionLevel\s+level="asInvoker"\s+uiAccess="false"/);
  assert.match(helperManifest, /com\.setmaster\.VideoForLazies\.UpdateHelper/);
  assert.doesNotMatch(mainManifest, /requestedExecutionLevel/);
  assert.doesNotMatch(mainManifest, /VideoForLazies\.UpdateHelper/);
  assert.match(mainManifest, /Microsoft\.Windows\.Common-Controls/);

  assert.match(buildScript, /WindowsAttributes::new_without_app_manifest/);
  assert.match(buildScript, /\("video_for_lazies", "video-for-lazies\.manifest"\)/);
  assert.match(buildScript, /\("vfl-update-helper", "vfl-update-helper\.manifest"\)/);
  assert.match(buildScript, /rustc-link-arg-bin=\{binary\}=\/MANIFESTINPUT:/);
  assert.doesNotMatch(buildScript, /rustc-link-arg-bins=/);
});

test("Windows artifact verifier checks the embedded helper and main manifests", async () => {
  const [verifier, releaseRunner, windowsSmoke] = await Promise.all([
    readAppFile("scripts/verify-windows-update-helper-manifest.ps1"),
    readAppFile("scripts/run-release-portable.mjs"),
    readAppFile("scripts/windows-portable-smoke.ps1"),
  ]);

  assert.match(verifier, /inputresource:\$resolvedExecutable;#1/);
  assert.match(verifier, /helperLevels\.Count -ne 1/);
  assert.match(verifier, /level -ne "asInvoker"/);
  assert.match(verifier, /uiAccess -ne "false"/);
  assert.match(verifier, /mainLevels\.Count -ne 0/);
  assert.match(verifier, /commonControls\.Count -ne 1/);
  assert.match(verifier, /--self-test/);
  assert.match(verifier, /vfl-update-helper ok/);
  assert.match(verifier, /Preserving updater manifest diagnostics/);

  assert.match(releaseRunner, /async function verifyWindowsUpdateHelperManifest/);
  assert.match(releaseRunner, /verifyWindowsUpdateHelperManifest\(portableDir, \{ platform \}\);/);
  assert.match(releaseRunner, /"-RunSelfTest"/);
  assert.match(windowsSmoke, /verify-windows-update-helper-manifest\.ps1/);
  assert.match(windowsSmoke, /-HelperPath \(Join-Path \$portableRoot "vfl-update-helper\.exe"\)/);
  assert.match(windowsSmoke, /-MainAppPath \$exePath/);
  assert.match(windowsSmoke, /-RunSelfTest/);
});

test("Windows error 740 returns a typed elevation outcome to the established app relaunch", async () => {
  const updater = await readAppFile("src-tauri/src/updater.rs");

  assert.match(updater, /enum UpdateHelperLaunchOutcome[\s\S]*ElevationRequired/);
  assert.match(
    updater,
    /if is_elevation_required_error\(&error\) \{\s*return Ok\(UpdateHelperLaunchOutcome::ElevationRequired\);/,
  );
  assert.match(
    updater,
    /if _launch_outcome == UpdateHelperLaunchOutcome::ElevationRequired[\s\S]*return elevate_for_update\(&app, &check\);/,
  );
});
