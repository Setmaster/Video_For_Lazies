import test from "node:test";
import assert from "node:assert/strict";

import { PINNED_BUNDLE_URLS, checkUrl } from "../scripts/check-ffmpeg-urls.mjs";

test("pinned bundle URL list covers binaries and all source archives", () => {
  assert.ok(PINNED_BUNDLE_URLS.length >= 5);
  assert.ok(PINNED_BUNDLE_URLS.every((url) => url.startsWith("https://")));
  assert.ok(PINNED_BUNDLE_URLS.some((url) => url.includes("win64-gpl-shared")));
  assert.ok(PINNED_BUNDLE_URLS.some((url) => url.includes("linux64-gpl-shared")));
  assert.ok(PINNED_BUNDLE_URLS.some((url) => url.includes("codeload.github.com/FFmpeg/FFmpeg")));
  assert.ok(PINNED_BUNDLE_URLS.some((url) => url.includes("code.videolan.org/videolan/x264")));
});

test("checkUrl reports ok for reachable URLs", async () => {
  const result = await checkUrl("https://example.com/x", {
    fetchImpl: async () => ({ ok: true, status: 200 }),
  });
  assert.deepEqual(result, { url: "https://example.com/x", ok: true, status: 200 });
});

test("checkUrl falls back to ranged GET when HEAD is rejected", async () => {
  const calls = [];
  const result = await checkUrl("https://example.com/x", {
    fetchImpl: async (_url, init) => {
      calls.push(init.method);
      if (init.method === "HEAD") return { ok: false, status: 403 };
      return { ok: false, status: 206 };
    },
  });
  assert.deepEqual(calls, ["HEAD", "GET"]);
  assert.equal(result.ok, true);
  assert.equal(result.status, 206);
});

test("checkUrl reports failures including thrown fetch errors", async () => {
  const notFound = await checkUrl("https://example.com/gone", {
    fetchImpl: async () => ({ ok: false, status: 404 }),
  });
  assert.equal(notFound.ok, false);
  assert.equal(notFound.status, 404);

  const refused = await checkUrl("https://example.com/refused", {
    fetchImpl: async () => {
      throw new Error("ECONNREFUSED");
    },
  });
  assert.equal(refused.ok, false);
  assert.match(refused.error, /ECONNREFUSED/);
});
