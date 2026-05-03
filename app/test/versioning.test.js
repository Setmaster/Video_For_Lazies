import test from "node:test";
import assert from "node:assert/strict";

import {
  assertSynchronizedVersion,
  normalizeVersionInput,
  parseCargoLockPackageVersion,
  readProjectVersions,
} from "../scripts/versioning.mjs";

test("project version metadata is synchronized", async () => {
  const { version, versions } = await assertSynchronizedVersion();

  assert.match(version, /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/);
  assert.ok(versions.length >= 5);
  assert.ok(versions.every((entry) => entry.version === version));
});

test("version helper accepts release tag syntax and rejects non-semver values", () => {
  assert.equal(normalizeVersionInput("v1.0.5"), "1.0.5");
  assert.equal(normalizeVersionInput("1.0.5-beta.1"), "1.0.5-beta.1");
  assert.throws(() => normalizeVersionInput("1.0"), /SemVer/);
  assert.throws(() => normalizeVersionInput("latest"), /SemVer/);
});

test("version metadata exposes the expected locations", async () => {
  const labels = (await readProjectVersions()).map((entry) => entry.label);

  assert.deepEqual(labels, [
    "app/package.json",
    "app/package-lock.json",
    "app/package-lock.json packages[\"\"]",
    "app/src-tauri/tauri.conf.json",
    "app/src-tauri/Cargo.toml",
    "app/src-tauri/Cargo.lock",
  ]);
});

test("Cargo.lock version parsing accepts CRLF line endings", () => {
  const raw = [
    "[[package]]",
    "name = \"video_for_lazies\"",
    "version = \"0.1.0\"",
    "dependencies = []",
    "",
  ].join("\r\n");

  assert.equal(parseCargoLockPackageVersion(raw, "video_for_lazies"), "0.1.0");
});
