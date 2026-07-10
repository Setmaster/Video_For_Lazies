import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";

import {
  renderSourceNotice,
  renderThirdPartyNotices,
} from "../scripts/generate-portable-docs.mjs";
import {
  FFMPEG_BUNDLE,
  linuxBuildScriptsArchiveName,
  linuxBundleAssetUrl,
  linuxSourceArchiveName,
  linuxX264SourceArchiveName,
  windowsBuildScriptsArchiveName,
  windowsBundleAssetUrl,
  windowsSourceArchiveName,
  windowsX264SourceArchiveName,
} from "../scripts/ffmpegBundle.mjs";

const repoRoot = path.resolve(import.meta.dirname, "..", "..");

test("portable source notice describes app and FFmpeg source artifacts without local paths", () => {
  const notice = renderSourceNotice({
    version: "0.1.0",
    commitSha: "abc1234def",
    releaseTag: "v0.1.0",
    tagAtHead: true,
  });

  assert.match(notice, /Repository: https:\/\/github\.com\/Setmaster\/Video_For_Lazies/);
  assert.match(notice, /Release tag: `v0\.1\.0`/);
  assert.match(notice, /Source commit: `abc1234def`/);
  assert.match(notice, /ffmpeg-sidecar\/source\/ffmpeg-ce3c09c101/);
  assert.match(notice, /btbn-ffmpeg-builds-7a83528e/);
  assert.match(notice, /x264-0480cb05/);
  assert.doesNotMatch(notice, /\/home\//);
  assert.doesNotMatch(notice, /C:\\Users\\/);
});

test("portable third-party notices include dependency inventories and bundled sidecar provenance", () => {
  const notice = renderThirdPartyNotices({
    version: "0.1.0",
    commitSha: "abc1234def",
    npmDependencies: [
      {
        name: "react",
        version: "19.2.4",
        license: "MIT",
        scope: "runtime",
        repository: "https://github.com/facebook/react",
      },
    ],
    cargoDependencies: [
      {
        name: "serde",
        version: "1.0.0",
        license: "MIT OR Apache-2.0",
        source: "crates.io",
        repository: "https://github.com/serde-rs/serde",
      },
    ],
  });

  assert.match(notice, /FFmpeg Sidecars/);
  assert.match(notice, /Windows x64/);
  assert.match(notice, /Linux x64/);
  assert.match(notice, /BtbN build recipe archive/);
  assert.match(notice, /x264 source archive/);
  assert.match(notice, /\| react \| 19\.2\.4 \| MIT \| runtime \|/);
  assert.match(notice, /\| serde \| 1\.0\.0 \| MIT OR Apache-2\.0 \| crates\.io \|/);
  assert.doesNotMatch(notice, /\/home\//);
  assert.doesNotMatch(notice, /C:\\Users\\/);
});

test("checked-in FFmpeg bundling doc matches pinned bundle config", () => {
  const doc = fs.readFileSync(path.resolve(repoRoot, "docs", "ffmpeg-bundling.md"), "utf8");
  const windowsBundle = FFMPEG_BUNDLE.windowsX64;
  const linuxBundle = FFMPEG_BUNDLE.linuxX64;

  for (const expected of [
    windowsBundle.releaseTag,
    windowsBundle.assetName,
    windowsBundle.assetSha256,
    windowsBundle.versionString,
    windowsBundle.sourceCommit,
    windowsBundle.sourceSha256,
    windowsBundle.buildScriptsCommit,
    windowsBundle.buildScriptsSha256,
    windowsBundle.x264Commit,
    windowsBundle.x264Sha256,
    windowsBundleAssetUrl,
    windowsSourceArchiveName,
    windowsBuildScriptsArchiveName,
    windowsX264SourceArchiveName,
    linuxBundle.releaseTag,
    linuxBundle.assetName,
    linuxBundle.assetSha256,
    linuxBundle.versionString,
    linuxBundle.sourceCommit,
    linuxBundle.sourceSha256,
    linuxBundle.buildScriptsCommit,
    linuxBundle.buildScriptsSha256,
    linuxBundle.x264Commit,
    linuxBundle.x264Sha256,
    linuxBundleAssetUrl,
    linuxSourceArchiveName,
    linuxBuildScriptsArchiveName,
    linuxX264SourceArchiveName,
  ]) {
    assert.match(doc, new RegExp(expected.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
});

test("public docs carry no stale FFmpeg pin identifiers", () => {
  // The v1.5.0 zips shipped with a provenance doc describing the previous pin
  // because only current-value presence was checked; reject any pin-shaped
  // string that is not part of the current bundle config.
  const currentValues = new Set();
  for (const bundle of [FFMPEG_BUNDLE.windowsX64, FFMPEG_BUNDLE.linuxX64]) {
    currentValues.add(bundle.releaseTag);
    currentValues.add(bundle.sourceCommit);
    currentValues.add(bundle.buildScriptsCommit);
    currentValues.add(bundle.x264Commit);
    currentValues.add(bundle.assetSha256);
    currentValues.add(bundle.sourceSha256);
    currentValues.add(bundle.buildScriptsSha256);
    currentValues.add(bundle.x264Sha256);
    currentValues.add(bundle.versionString);
  }
  for (const docPath of ["docs/ffmpeg-bundling.md", "docs/release.md"]) {
    const doc = fs.readFileSync(path.resolve(repoRoot, docPath), "utf8");

    for (const match of doc.matchAll(/autobuild-\d{4}-\d{2}-\d{2}-\d{2}-\d{2}/g)) {
      assert.ok(currentValues.has(match[0]), `${docPath} mentions stale release tag ${match[0]}`);
    }
    for (const match of doc.matchAll(/\b[0-9a-f]{40}\b/g)) {
      assert.ok(currentValues.has(match[0]), `${docPath} mentions stale pin hash ${match[0]}`);
    }
    for (const match of doc.matchAll(/\b[0-9a-f]{64}\b/g)) {
      assert.ok(currentValues.has(match[0]), `${docPath} mentions stale sha256 ${match[0]}`);
    }
    for (const match of doc.matchAll(/\bn\d+\.\d+(?:\.\d+)?-\d+-g[0-9a-f]+(?:-\d+)?\b/g)) {
      assert.ok(
        [...currentValues].some((value) => value.includes(match[0]) || match[0].includes(value)),
        `${docPath} mentions stale FFmpeg version string ${match[0]}`,
      );
    }
  }
});
