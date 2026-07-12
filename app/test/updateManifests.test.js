import test from "node:test";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  APP_ID,
  PAYLOAD_MANIFEST_FILE_NAME,
  UPDATE_MANIFEST_SCHEMA,
  UPDATE_NOTES_SUMMARY_MAX_CHARS,
  assertSafePortableRelativePath,
  buildUpdateManifest,
  copyPayloadManifestSidecar,
  generatePayloadManifest,
  getPayloadManifestSidecarPath,
  getUpdateNotesSummary,
  validatePayloadManifest,
} from "../scripts/updateManifests.mjs";
import { createReleaseZipFixture } from "../test-support/releaseZipFixture.js";

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

async function writeUpdateReleasePair(root, { version = "1.2.3" } = {}) {
  const releaseDir = path.resolve(root, "release-assets");
  const linuxPortable = path.resolve(root, "linux", "Video_For_Lazies");
  const winPortable = path.resolve(root, "win", "Video_For_Lazies");
  await fs.mkdir(releaseDir, { recursive: true });
  await writePortableFixture(linuxPortable, { platform: "linux" });
  await writePortableFixture(winPortable, { platform: "win32" });
  await generatePayloadManifest({ portableDir: linuxPortable, version, platform: "linux" });
  await generatePayloadManifest({ portableDir: winPortable, version, platform: "win32" });
  const linuxSidecar = await copyPayloadManifestSidecar({
    portableDir: linuxPortable,
    releaseDir,
    version,
    platform: "linux",
  });
  const winSidecar = await copyPayloadManifestSidecar({
    portableDir: winPortable,
    releaseDir,
    version,
    platform: "win32",
  });
  const linuxZip = path.resolve(releaseDir, `Video_For_Lazies-v${version}-linux-x64.zip`);
  const winZip = path.resolve(releaseDir, `Video_For_Lazies-v${version}-win-x64.zip`);
  await createReleaseZipFixture(linuxPortable, linuxZip);
  await createReleaseZipFixture(winPortable, winZip);
  return {
    releaseDir,
    linuxZip,
    winZip,
    linuxSidecar,
    winSidecar,
    linuxPortable,
    winPortable,
  };
}

function sha256(bytes) {
  return crypto.createHash("sha256").update(bytes).digest("hex");
}

function replaceZipNameBytes(zipBytes, fromName, toName) {
  const fromBytes = Buffer.from(fromName, "utf8");
  const toBytes = Buffer.from(toName, "utf8");
  assert.equal(toBytes.length, fromBytes.length, "ZIP name mutation must preserve byte length");
  const mutated = Buffer.from(zipBytes);
  let replacements = 0;
  let offset = 0;
  while (true) {
    const index = mutated.indexOf(fromBytes, offset);
    if (index < 0) break;
    toBytes.copy(mutated, index);
    replacements += 1;
    offset = index + toBytes.length;
  }
  assert.ok(replacements >= 2, `Expected local and central ZIP names for ${fromName}`);
  return mutated;
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
    const executableEntry = manifest.files.find((entry) => entry.path === "video_for_lazies");
    assert.ok(executableEntry);
    assert.equal(typeof executableEntry.mode, "number");
    if (process.platform !== "win32") {
      assert.equal(executableEntry.mode, 0o755);
    }

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

test("safe portable path validation mirrors updater-reserved path rules", () => {
  assert.equal(assertSafePortableRelativePath("ffmpeg-sidecar/ffmpeg"), "ffmpeg-sidecar/ffmpeg");
  assert.throws(() => assertSafePortableRelativePath("../outside"), /escapes/);
  assert.throws(() => assertSafePortableRelativePath("/tmp/file"), /relative/);
  assert.throws(() => assertSafePortableRelativePath("C:\\temp\\file"), /drive/);
  assert.throws(() => assertSafePortableRelativePath("folder\\file"), /backslashes/);
  assert.throws(() => assertSafePortableRelativePath("~backup/file"), /reserved character/);
  assert.throws(() => assertSafePortableRelativePath("folder:stream"), /reserved character/);
  assert.throws(() => assertSafePortableRelativePath(".vfl-updates/state.json"), /state directory/);
  assert.throws(() => assertSafePortableRelativePath(".VFL-UPDATES/state.json"), /state directory/);
});

test("update manifest is built from staged zips and payload sidecars", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-manifest-test-"));
  try {
    const { releaseDir, linuxZip, linuxSidecar } = await writeUpdateReleasePair(tempRoot);

    assert.match(getPayloadManifestSidecarPath({ releaseDir, version: "1.2.3", platform: "linux" }), /linux-x64\.payload-manifest\.json$/);

    const manifest = await buildUpdateManifest({
      releaseAssetDir: releaseDir,
      version: "1.2.3",
      publishedAt: "2026-05-03T00:00:00.000Z",
    });

    assert.equal(manifest.schema, UPDATE_MANIFEST_SCHEMA);
    assert.equal(manifest.version, "1.2.3");
    assert.equal(manifest.releaseTag, "v1.2.3");
    assert.equal(manifest.notes.summary, "See the GitHub release notes for changes.");
    assert.equal(manifest.artifacts["linux-x64"].payloadManifest.path, "Video_For_Lazies/VFL_PAYLOAD_MANIFEST.json");
    assert.equal(manifest.artifacts["windows-x64"].fileName, "Video_For_Lazies-v1.2.3-win-x64.zip");
    const linuxZipBytes = await fs.readFile(linuxZip);
    assert.equal(manifest.artifacts["linux-x64"].sha256, sha256(linuxZipBytes));
    assert.equal(manifest.artifacts["linux-x64"].sizeBytes, linuxZipBytes.length);
    assert.equal(
      manifest.artifacts["linux-x64"].payloadManifest.sha256,
      sha256(await fs.readFile(linuxSidecar)),
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("v1.10.0 update manifest carries its bounded plain-text release summary", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-summary-test-"));
  try {
    const { releaseDir } = await writeUpdateReleasePair(tempRoot, { version: "1.10.0" });
    const manifest = await buildUpdateManifest({
      releaseAssetDir: releaseDir,
      version: "1.10.0",
      publishedAt: "2026-07-12T00:00:00.000Z",
    });

    assert.equal(
      manifest.notes.summary,
      "Adds codec-aware exports, accessible crop and trim controls, in-memory queue tools and local recipes, Strict Fit, SRT subtitle burn-in, guarded Fast Trim, clearer phase progress, and journaled recovery for signed portable updates.",
    );
    assert.ok(manifest.notes.summary.length <= UPDATE_NOTES_SUMMARY_MAX_CHARS);
    assert.doesNotMatch(manifest.notes.summary, /[\u0000-\u001f\u007f]/);
    assert.equal(getUpdateNotesSummary("1.10.1"), "See the GitHub release notes for changes.");
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("v2.0.0 update manifest explains the trim simplification without rewriting v1.10.0", async () => {
  const summary = getUpdateNotesSummary("2.0.0");
  assert.equal(
    summary,
    "Simplifies trimming to one frame-accurate workflow by removing Fast Trim and widened-boundary consent, while retaining crop, Strict Fit, SRT burn-in, queue and recipes, accessible controls, progress, and signed update recovery.",
  );
  assert.ok(summary.length <= UPDATE_NOTES_SUMMARY_MAX_CHARS);
  assert.doesNotMatch(summary, /[\u0000-\u001f\u007f]/);
  assert.match(getUpdateNotesSummary("1.10.0"), /guarded Fast Trim/);
});

test("update manifest rejects a payload sidecar that differs from its embedded ZIP manifest", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-sidecar-pairing-test-"));
  try {
    const { releaseDir, linuxSidecar } = await writeUpdateReleasePair(tempRoot);
    await fs.appendFile(linuxSidecar, "\n");

    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /byte-for-byte.*embedded/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("update manifest rejects a raw backslash ZIP payload path", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-backslash-path-test-"));
  try {
    const { releaseDir, linuxZip } = await writeUpdateReleasePair(tempRoot);
    await fs.writeFile(
      linuxZip,
      replaceZipNameBytes(
        await fs.readFile(linuxZip),
        "Video_For_Lazies/README.md",
        "Video_For_Lazies\\README.md",
      ),
    );

    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /non-canonical ZIP path separator/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("update manifest rejects a non-normalized ZIP directory path", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-directory-path-test-"));
  try {
    const { releaseDir, linuxZip, linuxPortable } = await writeUpdateReleasePair(tempRoot);
    const fixtureDir = path.resolve(linuxPortable, "a____b");
    await fs.mkdir(fixtureDir, { recursive: true });
    await fs.writeFile(path.resolve(fixtureDir, "sentinel.txt"), "directory fixture");
    await createReleaseZipFixture(linuxPortable, linuxZip);
    await fs.writeFile(
      linuxZip,
      replaceZipNameBytes(
        await fs.readFile(linuxZip),
        "Video_For_Lazies/a____b/",
        "Video_For_Lazies/a/../b/",
      ),
    );

    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /non-normalized payload (?:path|directory)/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("update manifest rejects a CRC-valid ZIP with an unowned payload file", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-unowned-zip-test-"));
  try {
    const { releaseDir, linuxZip, linuxPortable } = await writeUpdateReleasePair(tempRoot);
    await fs.writeFile(path.resolve(linuxPortable, "unowned.bin"), "not listed in the signed payload manifest");
    await createReleaseZipFixture(linuxPortable, linuxZip);

    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /payload does not exactly match its manifest file list/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("update manifest rejects CRC-valid payload bytes that disagree with the embedded manifest hash", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-mutated-zip-test-"));
  try {
    const { releaseDir, winZip, winPortable } = await writeUpdateReleasePair(tempRoot);
    await fs.writeFile(path.resolve(winPortable, "README.md"), "mutated after manifest generation");
    await createReleaseZipFixture(winPortable, winZip);

    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /payload (size|hash) mismatch for README\.md/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});

test("update manifest rejects nested duplicate ZIP and payload-sidecar candidates per target", async (t) => {
  await t.test("duplicate ZIP", async () => {
    const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-duplicate-zip-test-"));
    try {
      const { releaseDir, linuxZip } = await writeUpdateReleasePair(tempRoot);
      const duplicateDir = path.resolve(releaseDir, "nested", "copy");
      await fs.mkdir(duplicateDir, { recursive: true });
      await fs.copyFile(linuxZip, path.resolve(duplicateDir, path.basename(linuxZip)));

      await assert.rejects(
        buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
        /duplicate release zip candidates.*linux-x64/i,
      );
    } finally {
      await fs.rm(tempRoot, { recursive: true, force: true });
    }
  });

  await t.test("duplicate payload sidecar", async () => {
    const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-duplicate-sidecar-test-"));
    try {
      const { releaseDir, winSidecar } = await writeUpdateReleasePair(tempRoot);
      const duplicateDir = path.resolve(releaseDir, "nested", "copy");
      await fs.mkdir(duplicateDir, { recursive: true });
      await fs.copyFile(winSidecar, path.resolve(duplicateDir, path.basename(winSidecar)));

      await assert.rejects(
        buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
        /duplicate payload manifest sidecar candidates.*windows-x64/i,
      );
    } finally {
      await fs.rm(tempRoot, { recursive: true, force: true });
    }
  });
});

test("update manifest requires one complete cross-platform pair at the requested version", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-update-complete-pair-test-"));
  try {
    const { releaseDir, winSidecar } = await writeUpdateReleasePair(tempRoot);
    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.4" }),
      /does not match requested version 1\.2\.4/i,
    );

    await fs.rm(winSidecar);
    await assert.rejects(
      buildUpdateManifest({ releaseAssetDir: releaseDir, version: "1.2.3" }),
      /missing payload manifest sidecar.*windows-x64/i,
    );
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
