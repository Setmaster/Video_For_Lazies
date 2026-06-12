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
      advanced: {
        videoCodec: "vp9",
        audioBitrateKbps: 192,
        videoQuality: "higher",
        encodeSpeed: "faster",
        frameRateCapFps: 30,
        audioChannels: "mono",
      },
      sizeLimitMb: "50",
      trimStart: "12.5",
      trimEnd: "48",
      cropEnabled: true,
      brightness: "0.2",
    })),
    {
      format: "webm",
      advanced: {
        videoCodec: "vp9",
        audioBitrateKbps: 192,
        videoQuality: "higher",
        encodeSpeed: "faster",
        frameRateCapFps: 30,
        audioChannels: "mono",
      },
    },
  );
});

test("parsePersistedSettings rejects unsupported formats", () => {
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ format: "avi" })), {});
});

test("parsePersistedSettings rejects unsupported advanced settings", () => {
  assert.deepEqual(
    parsePersistedSettings(
      JSON.stringify({
        advanced: {
          videoCodec: "prores",
          audioBitrateKbps: 999,
          videoQuality: "raw-crf",
          encodeSpeed: "warp",
          frameRateCapFps: 120,
          audioChannels: "surround",
        },
      }),
    ),
    {},
  );
});

test("serializePersistedSettings only writes the supported default format", () => {
  assert.equal(
    serializePersistedSettings({
      format: "mp4",
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 256,
        videoQuality: "balanced",
        encodeSpeed: "smaller",
        frameRateCapFps: 60,
        audioChannels: "stereo",
      },
      trimStart: "5",
      sizeLimitMb: "25",
    }),
    JSON.stringify({
      format: "mp4",
      advanced: {
        videoCodec: "h264",
        audioBitrateKbps: 256,
        videoQuality: "balanced",
        encodeSpeed: "smaller",
        frameRateCapFps: 60,
        audioChannels: "stereo",
      },
    }),
  );
});

test("serializePersistedSettings omits auto advanced defaults", () => {
  assert.equal(
    serializePersistedSettings({
      format: "mp4",
      advanced: {
        videoCodec: "auto",
        audioBitrateKbps: null,
        videoQuality: "auto",
        encodeSpeed: "auto",
        frameRateCapFps: null,
        audioChannels: "auto",
      },
    }),
    JSON.stringify({ format: "mp4" }),
  );
});

test("stripMetadata persists only the opt-out and ignores garbage", () => {
  assert.equal(
    serializePersistedSettings({ format: "mp4", stripMetadata: false }),
    JSON.stringify({ format: "mp4", stripMetadata: false }),
  );
  assert.equal(
    serializePersistedSettings({ format: "mp4", stripMetadata: true }),
    JSON.stringify({ format: "mp4" }),
  );

  assert.deepEqual(parsePersistedSettings(JSON.stringify({ stripMetadata: false })), {
    stripMetadata: false,
  });
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ stripMetadata: true })), {});
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ stripMetadata: "no" })), {});
});

test("normalizeAudio persists only when enabled and ignores garbage", () => {
  assert.equal(
    serializePersistedSettings({ format: "mp4", normalizeAudio: true }),
    JSON.stringify({ format: "mp4", normalizeAudio: true }),
  );
  assert.equal(
    serializePersistedSettings({ format: "mp4", normalizeAudio: false }),
    JSON.stringify({ format: "mp4" }),
  );

  assert.deepEqual(parsePersistedSettings(JSON.stringify({ normalizeAudio: true })), {
    normalizeAudio: true,
  });
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ normalizeAudio: "yes" })), {});
  assert.deepEqual(parsePersistedSettings(JSON.stringify({ normalizeAudio: 1 })), {});
});
