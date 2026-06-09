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
  assert.match(notice, /ffmpeg-sidecar\/source\/ffmpeg-e4c7fbf6c0/);
  assert.match(notice, /btbn-ffmpeg-builds-97ef373e/);
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
