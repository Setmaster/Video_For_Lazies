import test from "node:test";
import assert from "node:assert/strict";

import { DEFAULT_OUTPUT_FORMAT, DEFAULT_SIZE_LIMIT_MB } from "../src/lib/defaults.mjs";
import { parsePersistedSettings, serializePersistedSettings } from "../src/lib/settings.mjs";

test("app defaults keep size targeting disabled until the user opts in", () => {
  assert.equal(DEFAULT_OUTPUT_FORMAT, "mp4");
  assert.equal(DEFAULT_SIZE_LIMIT_MB, "");
});

test("parsePersistedSettings ignores invalid JSON", () => {
  assert.deepEqual(parsePersistedSettings("{not-json"), {});
});

test("parsePersistedSettings only restores the supported default format", () => {
  assert.deepEqual(
    parsePersistedSettings(JSON.stringify({
      format: "webm",
      sizeLimitMb: "50",
      trimStart: "12.5",
      trimEnd: "48",
      cropEnabled: true,
      brightness: "0.2",
    })),
    { format: "webm" },
  );
});

test("parsePersistedSettings rejects unsupported formats", () => {
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ format: "avi" })), {});
});

test("serializePersistedSettings only writes the supported default format", () => {
  assert.equal(
    serializePersistedSettings({
      format: "mp4",
      trimStart: "5",
      sizeLimitMb: "25",
    }),
    JSON.stringify({ format: "mp4" }),
  );
});
