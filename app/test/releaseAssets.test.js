import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { prepareReleaseAssets } from "../scripts/prepare-release-assets.mjs";
import {
  APP_ID,
  PAYLOAD_MANIFEST_SCHEMA,
  UPDATE_MANIFEST_FILE_NAME,
} from "../scripts/updateManifests.mjs";

function payloadManifest(version, target) {
  return `${JSON.stringify({
    schema: PAYLOAD_MANIFEST_SCHEMA,
    appId: APP_ID,
    version,
    target,
    rootDir: "Video_For_Lazies",
    files: [
      {
        path: target === "windows-x64" ? "Video_For_Lazies.exe" : "video_for_lazies",
        sha256: "0".repeat(64),
        sizeBytes: 1,
        mode: target === "linux-x64" ? 0o755 : null,
        kind: "file",
      },
    ],
  }, null, 2)}\n`;
}

test("prepareReleaseAssets stages release zips with a combined checksum file", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const linuxDir = path.resolve(inputDir, "linux");
    const winDir = path.resolve(inputDir, "win");
    const outputDir = path.resolve(tempRoot, "release-assets");

    await fs.mkdir(linuxDir, { recursive: true });
    await fs.mkdir(winDir, { recursive: true });
    await fs.writeFile(path.resolve(linuxDir, "Video_For_Lazies-v0.1.0-linux-x64.zip"), "linux");
    await fs.writeFile(path.resolve(winDir, "Video_For_Lazies-v0.1.0-win-x64.zip"), "windows");
    await fs.writeFile(path.resolve(linuxDir, "Video_For_Lazies-v0.1.0-linux-x64.payload-manifest.json"), payloadManifest("0.1.0", "linux-x64"));
    await fs.writeFile(path.resolve(winDir, "Video_For_Lazies-v0.1.0-win-x64.payload-manifest.json"), payloadManifest("0.1.0", "windows-x64"));
    await fs.writeFile(path.resolve(inputDir, "SHA256SUMS.txt"), "stale");

    const result = await prepareReleaseAssets({ inputDir, outputDir });
    const fileNames = result.files.map((filePath) => path.basename(filePath));

    assert.deepEqual(fileNames, [
      "Video_For_Lazies-v0.1.0-linux-x64.zip",
      "Video_For_Lazies-v0.1.0-win-x64.zip",
      "SHA256SUMS.txt",
      UPDATE_MANIFEST_FILE_NAME,
    ]);
    assert.equal(await fs.readFile(path.resolve(outputDir, "Video_For_Lazies-v0.1.0-linux-x64.zip"), "utf8"), "linux");
    assert.equal(await fs.readFile(path.resolve(outputDir, "Video_For_Lazies-v0.1.0-win-x64.zip"), "utf8"), "windows");

    const checksum = await fs.readFile(path.resolve(outputDir, "SHA256SUMS.txt"), "utf8");
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-linux-x64\.zip/);
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-win-x64\.zip/);

    const updateManifest = JSON.parse(await fs.readFile(path.resolve(outputDir, UPDATE_MANIFEST_FILE_NAME), "utf8"));
    assert.equal(updateManifest.version, "0.1.0");
    assert.equal(updateManifest.artifacts["linux-x64"].fileName, "Video_For_Lazies-v0.1.0-linux-x64.zip");
    assert.equal(updateManifest.artifacts["windows-x64"].fileName, "Video_For_Lazies-v0.1.0-win-x64.zip");
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets requires both Linux and Windows zips", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-missing-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");

    await fs.mkdir(inputDir, { recursive: true });
    await fs.writeFile(path.resolve(inputDir, "Video_For_Lazies-v0.1.0-linux-x64.zip"), "linux");

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir }),
      /release zip files/,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
