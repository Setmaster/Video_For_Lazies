import test from "node:test";
import assert from "node:assert/strict";

import {
  renderSourceNotice,
  renderThirdPartyNotices,
} from "../scripts/generate-portable-docs.mjs";

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
  assert.match(notice, /ffmpeg-sidecar\/source\/ffmpeg-75d37c499/);
  assert.match(notice, /btbn-ffmpeg-builds-28ae7513/);
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

  assert.match(notice, /Windows FFmpeg Sidecar/);
  assert.match(notice, /BtbN build recipe archive/);
  assert.match(notice, /x264 source archive/);
  assert.match(notice, /\| react \| 19\.2\.4 \| MIT \| runtime \|/);
  assert.match(notice, /\| serde \| 1\.0\.0 \| MIT OR Apache-2\.0 \| crates\.io \|/);
  assert.doesNotMatch(notice, /\/home\//);
  assert.doesNotMatch(notice, /C:\\Users\\/);
});
