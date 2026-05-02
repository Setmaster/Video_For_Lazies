import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import {
  buildChecksumLines,
  getPortableArchiveBaseName,
  getPortableChecksumPath,
  getPortableSevenZipPath,
  getPortableZipPath,
} from "../scripts/portableRelease.mjs";

test("portable release artifact names are stable for Windows x64", () => {
  assert.equal(getPortableArchiveBaseName({ platform: "win32", arch: "x64" }), "Video_For_Lazies-win-x64");
  assert.match(getPortableZipPath({ platform: "win32", arch: "x64" }), /Video_For_Lazies-win-x64\.zip$/);
  assert.match(getPortableSevenZipPath({ platform: "win32", arch: "x64" }), /Video_For_Lazies-win-x64\.7z$/);
  assert.match(getPortableChecksumPath(), /SHA256SUMS\.txt$/);
});

test("portable release helpers reject non-Windows archive targets", () => {
  assert.throws(() => getPortableArchiveBaseName({ platform: "linux", arch: "x64" }), /only supported on Windows/i);
});

test("buildChecksumLines emits sorted sha256 entries", async () => {
  const tempRoot = await fs.mkdtemp(path.resolve(os.tmpdir(), "vfl-release-checksum-test-"));
  try {
    const zPath = path.resolve(tempRoot, "b.zip");
    const sevenZipPath = path.resolve(tempRoot, "a.7z");
    await fs.writeFile(zPath, "zip-bytes");
    await fs.writeFile(sevenZipPath, "7z-bytes");

    const lines = await buildChecksumLines([zPath, sevenZipPath]);
    const entries = lines.trim().split("\n");

    assert.equal(entries.length, 2);
    assert.match(entries[0], /\s+a\.7z$/);
    assert.match(entries[1], /\s+b\.zip$/);
  } finally {
    await fs.rm(tempRoot, { recursive: true, force: true });
  }
});
