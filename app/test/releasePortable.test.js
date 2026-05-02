import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  buildChecksumLines,
  getPortableArchiveBaseName,
  getPortableExecutableName,
  getPortableTargetLabel,
  getPortableChecksumPath,
  getPortableZipPath,
} from "../scripts/portableRelease.mjs";

test("portable release artifact names are stable and versioned for Windows x64", () => {
  const options = { platform: "win32", arch: "x64", version: "0.1.0" };
  assert.equal(getPortableTargetLabel(options), "win-x64");
  assert.equal(getPortableExecutableName(options), "Video_For_Lazies.exe");
  assert.equal(getPortableArchiveBaseName(options), "Video_For_Lazies-v0.1.0-win-x64");
  assert.match(getPortableZipPath(options), /Video_For_Lazies-v0\.1\.0-win-x64\.zip$/);
  assert.match(getPortableChecksumPath(), /SHA256SUMS\.txt$/);
});

test("portable release artifact names are stable and versioned for Linux x64", () => {
  const options = { platform: "linux", arch: "x64", version: "0.1.0" };
  assert.equal(getPortableTargetLabel(options), "linux-x64");
  assert.equal(getPortableExecutableName(options), "video_for_lazies");
  assert.equal(getPortableArchiveBaseName(options), "Video_For_Lazies-v0.1.0-linux-x64");
  assert.match(getPortableZipPath(options), /Video_For_Lazies-v0\.1\.0-linux-x64\.zip$/);
});

test("portable release helpers reject unsupported archive targets", () => {
  assert.throws(() => getPortableArchiveBaseName({ platform: "darwin", arch: "x64", version: "0.1.0" }), /Windows and Linux/i);
  assert.throws(() => getPortableExecutableName({ platform: "darwin" }), /Windows and Linux/i);
  assert.throws(() => getPortableArchiveBaseName({ platform: "linux", arch: "ia32", version: "0.1.0" }), /x64/i);
  assert.throws(() => getPortableArchiveBaseName({ platform: "linux", arch: "x64", version: "latest" }), /SemVer/i);
});

test("buildChecksumLines emits sorted sha256 entries", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-checksum-test-"));
  try {
    const firstZipPath = path.resolve(tempRoot, "b.zip");
    const secondZipPath = path.resolve(tempRoot, "a.zip");
    await fs.writeFile(firstZipPath, "zip-b-bytes");
    await fs.writeFile(secondZipPath, "zip-a-bytes");

    const lines = await buildChecksumLines([firstZipPath, secondZipPath]);
    const entries = lines.trim().split("\n");

    assert.equal(entries.length, 2);
    assert.match(entries[0], /\s+a\.zip$/);
    assert.match(entries[1], /\s+b\.zip$/);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
