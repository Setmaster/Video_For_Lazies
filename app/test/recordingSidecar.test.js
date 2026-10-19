import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { prepareRecordingSidecar, recordingSidecarDir, recordingExecutableName } from "../scripts/build-recording-sidecar.mjs";

const appRoot = path.resolve(import.meta.dirname, "..");
const execFileAsync = promisify(execFile);

test("non-Windows recording preparation skips without creating resource state", async () => {
  const before = await fs.stat(recordingSidecarDir).then((value) => value.mtimeMs, (error) => {
    if (error.code !== "ENOENT") throw error;
    return null;
  });
  assert.equal(await prepareRecordingSidecar({ platform: "linux" }), null);
  const after = await fs.stat(recordingSidecarDir).then((value) => value.mtimeMs, (error) => {
    if (error.code !== "ENOENT") throw error;
    return null;
  });
  assert.equal(after, before);
});

test("recording CLI rejects unsupported options before any build", async () => {
  await assert.rejects(execFileAsync(process.execPath, [path.join(appRoot, "scripts/build-recording-sidecar.mjs"), "--unsupported"]), (error) => {
    assert.equal(error.code, 1);
    assert.match(error.stderr, /Usage:/);
    return true;
  });
});

test("recording resource is confined to Windows and both Tauri hooks prepare it", async () => {
  const common = JSON.parse(await fs.readFile(path.join(appRoot, "src-tauri/tauri.conf.json"), "utf8"));
  const windows = JSON.parse(await fs.readFile(path.join(appRoot, "src-tauri/tauri.windows.conf.json"), "utf8"));
  const pkg = JSON.parse(await fs.readFile(path.join(appRoot, "package.json"), "utf8"));
  assert.equal(recordingExecutableName, "vfl_recording_host.exe");
  assert.equal(windows.bundle.resources["target/recording-sidecar"], "recording-sidecar");
  assert.ok(!Object.values(common.bundle.resources).includes("recording-sidecar"));
  assert.match(common.build.beforeDevCommand, /prepare:recording-sidecar/);
  assert.match(common.build.beforeBuildCommand, /prepare:recording-sidecar/);
  assert.equal(pkg.scripts["prepare:recording-sidecar"], "node scripts/build-recording-sidecar.mjs");
});
