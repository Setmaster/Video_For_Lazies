import test from "node:test";
import assert from "node:assert/strict";

import { replaceExtension, suggestOutputPath } from "../src/lib/outputPath.mjs";

test("suggestOutputPath adds -2 when no suffix", () => {
  assert.equal(suggestOutputPath("myvideo.mp4", "mp4"), "myvideo-2.mp4");
  assert.equal(suggestOutputPath("myvideo.mp4", "webm"), "myvideo-2.webm");
});

test("suggestOutputPath increments existing -N suffix", () => {
  assert.equal(suggestOutputPath("coolvideo-2.webm", "webm"), "coolvideo-3.webm");
  assert.equal(suggestOutputPath("coolvideo-2.webm", "mp4"), "coolvideo-3.mp4");
});

test("suggestOutputPath avoids incrementing long numeric suffixes", () => {
  assert.equal(suggestOutputPath("movie-2024.mp4", "mp4"), "movie-2024-2.mp4");
});

test("suggestOutputPath preserves directory separators", () => {
  assert.equal(
    suggestOutputPath("C:\\\\Videos\\\\myvideo.mp4", "mp4"),
    "C:\\\\Videos\\\\myvideo-2.mp4",
  );
  assert.equal(
    suggestOutputPath("C:/Videos/myvideo.mp4", "mp4"),
    "C:/Videos/myvideo-2.mp4",
  );
});

test("replaceExtension swaps extension or appends if missing", () => {
  assert.equal(replaceExtension("C:\\\\Videos\\\\foo-2.mp4", "webm"), "C:\\\\Videos\\\\foo-2.webm");
  assert.equal(replaceExtension("foo", "mp4"), "foo.mp4");
});
