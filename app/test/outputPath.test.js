import test from "node:test";
import assert from "node:assert/strict";

import { ensureUniqueOutputPath, replaceExtension, suggestOutputPath } from "../src/lib/outputPath.mjs";

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

test("ensureUniqueOutputPath keeps unclaimed candidates", () => {
  assert.equal(ensureUniqueOutputPath("a-2.mp4", []), "a-2.mp4");
  assert.equal(ensureUniqueOutputPath("a-2.mp4", null), "a-2.mp4");
  assert.equal(ensureUniqueOutputPath("a-2.mp4", ["b-2.mp4"]), "a-2.mp4");
});

test("ensureUniqueOutputPath bumps past queue-claimed paths", () => {
  // Add current plan twice: the second snapshot must not reuse the path.
  assert.equal(ensureUniqueOutputPath("a-2.mp4", ["a-2.mp4"]), "a-3.mp4");
  // Inputs a.mp4 and a-2.mp4 both suggesting a-3 in one batch.
  assert.equal(ensureUniqueOutputPath("a-3.mp4", ["a-3.mp4", "a-4.mp4"]), "a-5.mp4");
  assert.equal(
    ensureUniqueOutputPath("C:/Videos/clip-2.mp4", ["C:/Videos/clip-2.mp4"]),
    "C:/Videos/clip-3.mp4",
  );
});

test("ensureUniqueOutputPath rejects equivalent Windows and UNC claims", () => {
  assert.equal(
    ensureUniqueOutputPath("C:/Videos/Clip-2.MP4", ["c:\\videos\\clip-2.mp4"]),
    "C:/Videos/Clip-3.MP4",
  );
  assert.equal(
    ensureUniqueOutputPath("//Server/Share/Clip-2.mp4", ["\\\\server\\share\\clip-2.MP4"]),
    "//Server/Share/Clip-3.mp4",
  );
});

test("ensureUniqueOutputPath preserves explicit POSIX case and double-slash identity", () => {
  assert.equal(
    ensureUniqueOutputPath("//Server/Share/Clip-2.mp4", ["//server/share/clip-2.mp4"], "posix"),
    "//Server/Share/Clip-2.mp4",
  );
});
