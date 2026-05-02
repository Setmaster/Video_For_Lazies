import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { prepareReleaseAssets } from "../scripts/prepare-release-assets.mjs";

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
    await fs.writeFile(path.resolve(inputDir, "SHA256SUMS.txt"), "stale");

    const result = await prepareReleaseAssets({ inputDir, outputDir });
    const fileNames = result.files.map((filePath) => path.basename(filePath));

    assert.deepEqual(fileNames, [
      "Video_For_Lazies-v0.1.0-linux-x64.zip",
      "Video_For_Lazies-v0.1.0-win-x64.zip",
      "SHA256SUMS.txt",
    ]);
    assert.equal(await fs.readFile(path.resolve(outputDir, "Video_For_Lazies-v0.1.0-linux-x64.zip"), "utf8"), "linux");
    assert.equal(await fs.readFile(path.resolve(outputDir, "Video_For_Lazies-v0.1.0-win-x64.zip"), "utf8"), "windows");

    const checksum = await fs.readFile(path.resolve(outputDir, "SHA256SUMS.txt"), "utf8");
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-linux-x64\.zip/);
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-win-x64\.zip/);
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
