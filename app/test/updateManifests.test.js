import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  APP_ID,
  PAYLOAD_MANIFEST_FILE_NAME,
  UPDATE_MANIFEST_SCHEMA,
  assertSafePortableRelativePath,
  buildUpdateManifest,
  copyPayloadManifestSidecar,
  generatePayloadManifest,
  getPayloadManifestSidecarPath,
  validatePayloadManifest,
} from "../scripts/updateManifests.mjs";

async function writePortableFixture(root, { platform = "linux" } = {}) {
  await fs.mkdir(path.resolve(root, "ffmpeg-sidecar"), { recursive: true });
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
  if (platform === "linux") {
    files.push("Video_For_Lazies.png", "Video_For_Lazies.desktop");
  }

  for (const relativePath of files) {
    const filePath = path.resolve(root, relativePath);
    await fs.mkdir(path.dirname(filePath), { recursive: true });
    await fs.writeFile(filePath, relativePath);
    if (platform === "linux" && (relativePath === "video_for_lazies" || relativePath === "vfl-update-helper" || relativePath.endsWith(".desktop"))) {
      await fs.chmod(filePath, 0o755);
    }
  }
}

test("payload manifests are generated and validated for a portable folder", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-payload-manifest-test-"));
  try {
    const portableDir = path.resolve(tempRoot, "Video_For_Lazies");
    await writePortableFixture(portableDir, { platform: "linux" });

    const { manifest, manifestPath } = await generatePayloadManifest({
      portableDir,
      version: "1.2.3",
      platform: "linux",
    });

    assert.equal(path.basename(manifestPath), PAYLOAD_MANIFEST_FILE_NAME);
    assert.equal(manifest.appId, APP_ID);
    assert.equal(manifest.version, "1.2.3");
    assert.equal(manifest.target, "linux-x64");
    assert.ok(manifest.files.some((entry) => entry.path === "video_for_lazies" && entry.mode === 0o755));

    const validated = await validatePayloadManifest({
      portableDir,
      version: "1.2.3",
      platform: "linux",
    });
    assert.equal(validated.files.length, manifest.files.length);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("payload manifest validation rejects extra unowned files", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-payload-extra-test-"));
  try {
    const portableDir = path.resolve(tempRoot, "Video_For_Lazies");
    await writePortableFixture(portableDir, { platform: "linux" });
    await generatePayloadManifest({ portableDir, version: "1.2.3", platform: "linux" });
    await fs.writeFile(path.resolve(portableDir, "unexpected.txt"), "user file");

    await assert.rejects(
      validatePayloadManifest({ portableDir, version: "1.2.3", platform: "linux" }),
      /does not exactly match/,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("safe portable path validation rejects traversal and absolute paths", () => {
  assert.equal(assertSafePortableRelativePath("ffmpeg-sidecar/ffmpeg"), "ffmpeg-sidecar/ffmpeg");
  assert.throws(() => assertSafePortableRelativePath("../outside"), /escapes/);
  assert.throws(() => assertSafePortableRelativePath("/tmp/file"), /relative/);
  assert.throws(() => assertSafePortableRelativePath("C:\\temp\\file"), /drive/);
});

test("update manifest is built from staged zips and payload sidecars", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-manifest-test-"));
  try {
    const releaseDir = path.resolve(tempRoot, "release-assets");
    const linuxPortable = path.resolve(tempRoot, "linux", "Video_For_Lazies");
    const winPortable = path.resolve(tempRoot, "win", "Video_For_Lazies");
    await fs.mkdir(releaseDir, { recursive: true });
    await writePortableFixture(linuxPortable, { platform: "linux" });
    await writePortableFixture(winPortable, { platform: "win32" });
    await generatePayloadManifest({ portableDir: linuxPortable, version: "1.2.3", platform: "linux" });
    await generatePayloadManifest({ portableDir: winPortable, version: "1.2.3", platform: "win32" });
    await copyPayloadManifestSidecar({ portableDir: linuxPortable, releaseDir, version: "1.2.3", platform: "linux" });
    await copyPayloadManifestSidecar({ portableDir: winPortable, releaseDir, version: "1.2.3", platform: "win32" });
    await fs.writeFile(path.resolve(releaseDir, "Video_For_Lazies-v1.2.3-linux-x64.zip"), "linux zip");
    await fs.writeFile(path.resolve(releaseDir, "Video_For_Lazies-v1.2.3-win-x64.zip"), "windows zip");

    assert.match(getPayloadManifestSidecarPath({ releaseDir, version: "1.2.3", platform: "linux" }), /linux-x64\.payload-manifest\.json$/);

    const manifest = await buildUpdateManifest({
      releaseAssetDir: releaseDir,
      version: "1.2.3",
      publishedAt: "2026-05-03T00:00:00.000Z",
    });

    assert.equal(manifest.schema, UPDATE_MANIFEST_SCHEMA);
    assert.equal(manifest.version, "1.2.3");
    assert.equal(manifest.releaseTag, "v1.2.3");
    assert.equal(manifest.artifacts["linux-x64"].payloadManifest.path, "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json");
    assert.equal(manifest.artifacts["windows-x64"].fileName, "Video_For_Lazies-v1.2.3-win-x64.zip");
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
