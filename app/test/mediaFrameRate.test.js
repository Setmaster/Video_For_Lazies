import assert from "node:assert/strict";
import test from "node:test";

import { effectiveFrameRatePlan } from "../src/lib/mediaDepth.mjs";

test("frame-rate planning caps the post-speed frame rate", () => {
  assert.deepEqual(
    effectiveFrameRatePlan({ sourceFrameRate: 24, speed: 2, frameRateCapFps: 30 }),
    { postSpeedFps: 48, capFps: 30, capApplies: true, outputFps: 30 },
  );
});

test("frame-rate planning does not claim a cap below the post-speed threshold", () => {
  assert.deepEqual(
    effectiveFrameRatePlan({ sourceFrameRate: 60, speed: 0.5, frameRateCapFps: 30 }),
    { postSpeedFps: 30, capFps: 30, capApplies: false, outputFps: 30 },
  );
});

test("frame-rate planning fails conservatively when the source rate is unknown", () => {
  assert.deepEqual(
    effectiveFrameRatePlan({ sourceFrameRate: null, speed: 2, frameRateCapFps: 30 }),
    { postSpeedFps: null, capFps: 30, capApplies: true, outputFps: 30 },
  );
  assert.deepEqual(
    effectiveFrameRatePlan({ sourceFrameRate: null, speed: 2 }),
    { postSpeedFps: null, capFps: null, capApplies: false, outputFps: null },
  );
});

test("frame-rate planning exposes no video rate or cap for MP3 output", () => {
  assert.deepEqual(
    effectiveFrameRatePlan({ format: "mp3", sourceFrameRate: 60, speed: 2, frameRateCapFps: 30 }),
    { postSpeedFps: null, capFps: null, capApplies: false, outputFps: null },
  );
});
