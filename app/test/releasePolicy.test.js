import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";

import { resolveReleasePolicy, validateReleaseTargets } from "../scripts/releasePolicy.mjs";

test("v2.6.0 builds Windows only and cannot become shared latest", async () => {
  const policy = await resolveReleasePolicy("v2.6.0");
  assert.deepEqual(policy.targets, ["windows-x64"]);
  assert.equal(policy.makeLatest, false);
  assert.deepEqual(policy.matrix, { include: [{ os: "windows-2022", target: "win-x64" }] });
});

test("other versions retain the complete shared release by default", async () => {
  const policy = await resolveReleasePolicy("2.5.0");
  assert.deepEqual(policy.targets, ["linux-x64", "windows-x64"]);
  assert.equal(policy.makeLatest, true);
  assert.equal(policy.matrix.include.length, 2);
});

test("invalid and repeated release targets fail closed", () => {
  for (const targets of [[], ["win-x64"], ["linux-x64", "linux-x64"], "windows-x64"]) {
    assert.throws(() => validateReleaseTargets(targets), /nonempty, unique list/);
  }
});

test("configuration cannot promote a platform-only release to shared latest", async () => {
  const tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-release-policy-test-"));
  try {
    const configPath = path.join(tempDir, "release-policy.json");
    await fs.writeFile(configPath, JSON.stringify({
      schemaVersion: 1, releases: { "2.6.0": { targets: ["windows-x64"], makeLatest: true } },
    }));
    await assert.rejects(resolveReleasePolicy("2.6.0", { configPath }), /must not replace the shared latest/);
  } finally {
    await fs.rm(tempDir, { recursive: true, force: true });
  }
});

test("future declared Windows, Linux and paired releases resolve without version-specific code", async () => {
  const tempDir = await fs.mkdtemp(path.join(os.tmpdir(), "vfl-release-declarations-test-"));
  try {
    const configPath = path.join(tempDir, "release-policy.json");
    await fs.writeFile(configPath, JSON.stringify({ schemaVersion: 1, releases: {
      "9.1.0": { targets: ["windows-x64"], makeLatest: false },
      "9.2.0": { targets: ["linux-x64"], makeLatest: false },
      "9.3.0": { targets: ["linux-x64", "windows-x64"], makeLatest: true },
      "9.4.0": { targets: ["linux-x64"], makeLatest: true },
    } }));
    assert.deepEqual((await resolveReleasePolicy("v9.1.0", { configPath })).matrix.include, [{ os: "windows-2022", target: "win-x64" }]);
    assert.deepEqual((await resolveReleasePolicy("9.2.0", { configPath })).matrix.include, [{ os: "ubuntu-24.04", target: "linux-x64" }]);
    assert.equal((await resolveReleasePolicy("9.3.0", { configPath })).matrix.include.length, 2);
    await assert.rejects(resolveReleasePolicy("9.4.0", { configPath }), /must not replace the shared latest/);
  } finally {
    await fs.rm(tempDir, { recursive: true, force: true });
  }
});
