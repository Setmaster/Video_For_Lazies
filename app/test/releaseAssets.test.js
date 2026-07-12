import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { prepareReleaseAssets } from "../scripts/prepare-release-assets.mjs";
import {
  UPDATE_MANIFEST_FILE_NAME,
  generatePayloadManifest,
} from "../scripts/updateManifests.mjs";
import { createReleaseZipFixture } from "../test-support/releaseZipFixture.js";

async function writePortableFixture(root, { platform }) {
  const files = [
    platform === "win32" ? "Video_For_Lazies.exe" : "video_for_lazies",
    platform === "win32" ? "vfl-update-helper.exe" : "vfl-update-helper",
    "README.md",
    "LICENSE.txt",
    "THIRD_PARTY_NOTICES.md",
    "SOURCE.md",
    "FFMPEG_BUNDLING.md",
    "ffmpeg-sidecar/LICENSE.txt",
    "ffmpeg-sidecar/FFMPEG_BUNDLE_NOTICES.txt",
  ];
  if (platform === "linux") files.push("Video_For_Lazies.png", "Video_For_Lazies.desktop");
  for (const relativePath of files) {
    const filePath = path.resolve(root, relativePath);
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    await fs.writeFile(filePath, relativePath);
    if (
      platform === "linux"
      && ["video_for_lazies", "vfl-update-helper", "Video_For_Lazies.desktop"].includes(relativePath)
    ) {
      await fs.chmod(filePath, 0o755);
    }
  }
}

async function writeReleasePair(inputDir, { version = "0.1.0" } = {}) {
  const artifacts = {};
  for (const [folder, portableTarget, platform] of [
    ["linux", "linux", "linux"],
    ["win", "win", "win32"],
  ]) {
    const targetDir = path.resolve(inputDir, folder);
    const portableDir = path.resolve(targetDir, "fixture", "Video_For_Lazies");
    const baseName = `Video_For_Lazies-v${version}-${portableTarget}-x64`;
    const zipPath = path.resolve(targetDir, `${baseName}.zip`);
    const sidecarPath = path.resolve(targetDir, `${baseName}.payload-manifest.json`);
    await writePortableFixture(portableDir, { platform });
    const { manifestPath } = await generatePayloadManifest({ portableDir, version, platform });
    const manifestBytes = await fs.readFile(manifestPath);
    await createReleaseZipFixture(portableDir, zipPath);
    await fs.writeFile(sidecarPath, manifestBytes);
    artifacts[platform === "linux" ? "linux-x64" : "windows-x64"] = { zipPath, sidecarPath };
  }
  return artifacts;
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function corruptZipEntryData(zipBytes, expectedName) {
  const corrupted = Buffer.from(zipBytes);
  for (let offset = 0; offset + 46 <= corrupted.length; offset += 1) {
    if (corrupted.readUInt32LE(offset) !== 0x02014b50) continue;
    const fileNameLength = corrupted.readUInt16LE(offset + 28);
    const extraLength = corrupted.readUInt16LE(offset + 30);
    const commentLength = corrupted.readUInt16LE(offset + 32);
    const entryEnd = offset + 46 + fileNameLength + extraLength + commentLength;
    if (entryEnd > corrupted.length) continue;
    const entryName = corrupted
      .subarray(offset + 46, offset + 46 + fileNameLength)
      .toString("utf8")
      .replaceAll("\\", "/");
    if (entryName !== expectedName) continue;
    const compressedSize = corrupted.readUInt32LE(offset + 20);
    const localHeaderOffset = corrupted.readUInt32LE(offset + 42);
    const localNameLength = corrupted.readUInt16LE(localHeaderOffset + 26);
    const localExtraLength = corrupted.readUInt16LE(localHeaderOffset + 28);
    const dataOffset = localHeaderOffset + 30 + localNameLength + localExtraLength;
    assert.ok(compressedSize > 0, `ZIP fixture entry ${expectedName} must contain data`);
    corrupted[dataOffset + Math.floor(compressedSize / 2)] ^= 0xff;
    return corrupted;
  }
  throw new Error(`Could not locate ZIP fixture entry ${expectedName}.`);
}

test("prepareReleaseAssets stages release zips with a combined checksum file", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");

    const sourceArtifacts = await writeReleasePair(inputDir);
    await fs.writeFile(path.resolve(inputDir, "SHA256SUMS.txt"), "stale");

    const result = await prepareReleaseAssets({ inputDir, outputDir });
    const fileNames = result.files.map((filePath) => path.basename(filePath));

    assert.deepEqual(fileNames, [
      "Video_For_Lazies-v0.1.0-linux-x64.zip",
      "Video_For_Lazies-v0.1.0-win-x64.zip",
      "SHA256SUMS.txt",
      UPDATE_MANIFEST_FILE_NAME,
    ]);
    const stagedLinuxZip = path.resolve(outputDir, "Video_For_Lazies-v0.1.0-linux-x64.zip");
    const stagedWinZip = path.resolve(outputDir, "Video_For_Lazies-v0.1.0-win-x64.zip");
    assert.deepEqual(await fs.readFile(stagedLinuxZip), await fs.readFile(sourceArtifacts["linux-x64"].zipPath));
    assert.deepEqual(await fs.readFile(stagedWinZip), await fs.readFile(sourceArtifacts["windows-x64"].zipPath));

    const checksum = await fs.readFile(path.resolve(outputDir, "SHA256SUMS.txt"), "utf8");
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-linux-x64\.zip/);
    assert.match(checksum, /Video_For_Lazies-v0\.1\.0-win-x64\.zip/);

    const updateManifest = JSON.parse(await fs.readFile(path.resolve(outputDir, UPDATE_MANIFEST_FILE_NAME), "utf8"));
    assert.equal(updateManifest.version, "0.1.0");
    assert.equal(updateManifest.artifacts["linux-x64"].fileName, "Video_For_Lazies-v0.1.0-linux-x64.zip");
    assert.equal(updateManifest.artifacts["windows-x64"].fileName, "Video_For_Lazies-v0.1.0-win-x64.zip");
    const finalLinuxBytes = await fs.readFile(stagedLinuxZip);
    assert.equal(updateManifest.artifacts["linux-x64"].sha256, sha256(finalLinuxBytes));
    assert.equal(updateManifest.artifacts["linux-x64"].sizeBytes, finalLinuxBytes.length);
    await assert.rejects(
      fs.access(path.resolve(outputDir, "Video_For_Lazies-v0.1.0-linux-x64.payload-manifest.json")),
      { code: "ENOENT" },
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets requires both Linux and Windows zips", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-missing-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");

    const artifacts = await writeReleasePair(inputDir);
    await fs.rm(artifacts["windows-x64"].zipPath);

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir }),
      /missing release zip.*windows-x64/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets rejects an output ancestor before deleting input or siblings", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-ancestor-test-"));
  const inputDir = path.resolve(tempRoot, "downloads");
  const markerPath = path.resolve(tempRoot, "unrelated-marker.txt");
  try {
    const artifacts = await writeReleasePair(inputDir);
    await fs.writeFile(markerPath, "preserve me");

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir: tempRoot }),
      /output directory must not contain the input directory/i,
    );
    assert.equal(await fs.readFile(markerPath, "utf8"), "preserve me");
    await fs.access(artifacts["linux-x64"].zipPath);
    await fs.access(artifacts["windows-x64"].zipPath);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets rejects a symlink-alias output before deleting canonical input", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-alias-test-"));
  const inputDir = path.resolve(tempRoot, "downloads");
  const aliasDir = path.resolve(tempRoot, "alias");
  const markerPath = path.resolve(inputDir, "preserve-marker.txt");
  try {
    const artifacts = await writeReleasePair(inputDir);
    await fs.writeFile(markerPath, "preserve me");
    await fs.symlink(tempRoot, aliasDir, process.platform === "win32" ? "junction" : "dir");

    await assert.rejects(
      prepareReleaseAssets({
        inputDir,
        outputDir: path.resolve(aliasDir, "downloads"),
      }),
      /output directory must not contain the input directory/i,
    );
    assert.equal(await fs.readFile(markerPath, "utf8"), "preserve me");
    await fs.access(artifacts["linux-x64"].zipPath);
    await fs.access(artifacts["windows-x64"].zipPath);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets rejects nested duplicate release candidates before staging", async (t) => {
  await t.test("duplicate ZIP basename", async () => {
    const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-duplicate-zip-test-"));
    try {
      const inputDir = path.resolve(tempRoot, "downloads");
      const outputDir = path.resolve(tempRoot, "release-assets");
      const artifacts = await writeReleasePair(inputDir);
      const duplicateDir = path.resolve(inputDir, "nested", "duplicate");
      await fs.mkdir(duplicateDir, { recursive: true });
      await fs.copyFile(
        artifacts["linux-x64"].zipPath,
        path.resolve(duplicateDir, path.basename(artifacts["linux-x64"].zipPath)),
      );

      await assert.rejects(
        prepareReleaseAssets({ inputDir, outputDir }),
        /duplicate release zip candidates.*linux-x64/i,
      );
    } finally {
      await fs.rm(tempRoot, { recursive: true, force: true });
    }
  });

  await t.test("duplicate payload-sidecar basename", async () => {
    const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-duplicate-sidecar-test-"));
    try {
      const inputDir = path.resolve(tempRoot, "downloads");
      const outputDir = path.resolve(tempRoot, "release-assets");
      const artifacts = await writeReleasePair(inputDir);
      const duplicateDir = path.resolve(inputDir, "nested", "duplicate");
      await fs.mkdir(duplicateDir, { recursive: true });
      await fs.copyFile(
        artifacts["windows-x64"].sidecarPath,
        path.resolve(duplicateDir, path.basename(artifacts["windows-x64"].sidecarPath)),
      );

      await assert.rejects(
        prepareReleaseAssets({ inputDir, outputDir }),
        /duplicate payload manifest sidecar candidates.*windows-x64/i,
      );
    } finally {
      await fs.rm(tempRoot, { recursive: true, force: true });
    }
  });
});

test("prepareReleaseAssets refuses sidecar and embedded-manifest byte drift", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-pairing-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");
    const artifacts = await writeReleasePair(inputDir);
    await fs.appendFile(artifacts["windows-x64"].sidecarPath, "\n");

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir }),
      /byte-for-byte.*embedded/i,
    );
    await assert.rejects(
      fs.access(path.resolve(outputDir, UPDATE_MANIFEST_FILE_NAME)),
      { code: "ENOENT" },
    );
    await assert.rejects(
      fs.access(path.resolve(outputDir, "SHA256SUMS.txt")),
      { code: "ENOENT" },
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets rejects corrupt downloaded ZIP payload bytes before manifest publication", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-corrupt-zip-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");
    const artifacts = await writeReleasePair(inputDir);
    const linuxZip = artifacts["linux-x64"].zipPath;
    await fs.writeFile(
      linuxZip,
      corruptZipEntryData(await fs.readFile(linuxZip), "Video_For_Lazies/README.md"),
    );

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir }),
      /could not inflate|CRC mismatch/i,
    );
    await assert.rejects(fs.access(path.resolve(outputDir, UPDATE_MANIFEST_FILE_NAME)));
    await assert.rejects(fs.access(path.resolve(outputDir, "SHA256SUMS.txt")));
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("prepareReleaseAssets enforces an explicitly requested pair version", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-assets-version-test-"));
  try {
    const inputDir = path.resolve(tempRoot, "downloads");
    const outputDir = path.resolve(tempRoot, "release-assets");
    await writeReleasePair(inputDir);

    await assert.rejects(
      prepareReleaseAssets({ inputDir, outputDir, version: "0.2.0" }),
      /does not match requested version 0\.2\.0/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
