import test from "node:test";
import assert from "node:assert/strict";
import {
  TRANSFORM_MEMORY_BLOCK_BYTES,
  TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES,
  TRANSFORM_MEMORY_WARN_BYTES,
  classifyColorSource,
  codecOutputDimensionBlockingReason,
  effectiveSampleAspectRatio,
  encodedOutputDimensions,
  estimateTransformMemory,
  fitMaxEdgeDisplayDimensions,
  fitMaxEdgeDimensions,
  hasNonSquarePixels,
  minimumVideoBitrateKbps,
  plannedDimensionBlockingReason,
  sizeTargetRetainsAudio,
  sourceRotationBlockingReason,
  squarePixelDimensions,
  timelineSpeedChanges,
  trimRequestIsActive,
  videoCodecDimensionLimit,
} from "../src/lib/mediaDepth.mjs";

test("trim activity mirrors request clamping at the source duration", () => {
  assert.equal(trimRequestIsActive({ startS: 0, endS: null, durationS: 10 }), false);
  assert.equal(trimRequestIsActive({ startS: 0, endS: 5, durationS: 10 }), true);
  assert.equal(trimRequestIsActive({ startS: 0, endS: 10, durationS: 10 }), false);
  assert.equal(trimRequestIsActive({ startS: 0, endS: 12, durationS: 10 }), false);
  assert.equal(trimRequestIsActive({ startS: 2, endS: 12, durationS: 10 }), true);
});

test("encoded output geometry evens definite encodes but preserves conditional remux dimensions", () => {
  assert.deepEqual(encodedOutputDimensions(3, 3, true), { width: 2, height: 2 });
  assert.deepEqual(encodedOutputDimensions(853, 481, true), { width: 852, height: 480 });
  assert.deepEqual(encodedOutputDimensions(853, 481, false), { width: 853, height: 481 });
});

test("codec-specific output bounds match the pinned FFmpeg encoder limits", () => {
  for (const [codec, limit] of [
    ["h264", 16_384],
    ["mpeg4", 8_190],
    ["vp8", 16_382],
    ["vp9", 32_768],
  ]) {
    assert.equal(videoCodecDimensionLimit(codec), limit);
    assert.equal(codecOutputDimensionBlockingReason(codec, limit, 2), null);
    assert.match(codecOutputDimensionBlockingReason(codec, limit + 2, 2), new RegExp(String(limit)));
  }
});

test("speed transform predicate matches the backend request threshold", () => {
  assert.equal(timelineSpeedChanges(1), false);
  assert.equal(timelineSpeedChanges(1 + 1e-10), false);
  assert.equal(timelineSpeedChanges(1.00000001), true);
  assert.equal(timelineSpeedChanges("1.0005"), true);
  assert.equal(timelineSpeedChanges("not-a-speed"), false);
});

const baseProbe = {
  durationS: 10,
  frameRate: 30,
  hasAudio: true,
  decodedVideoBytesPerPixel: 1.5,
  audioSampleRate: 48_000,
  audioChannels: 2,
  decodedAudioBytesPerSample: 4,
  bitDepth: 8,
  dynamicRange: "sdr",
  sampleAspectRatio: { numerator: 1, denominator: 1 },
};

test("color classification requires explicit handling only for supported high-depth sources", () => {
  assert.deepEqual(classifyColorSource(baseProbe), { kind: "standard", label: "Standard SDR" });
  assert.equal(classifyColorSource({ ...baseProbe, bitDepth: null }).kind, "unsupported");
  assert.deepEqual(
    classifyColorSource({ ...baseProbe, bitDepth: 10, colorPrimaries: "bt709", colorTransfer: "bt709", colorSpace: "bt709" }),
    { kind: "convertible", label: "10-bit SDR" },
  );
  assert.deepEqual(
    classifyColorSource({ ...baseProbe, bitDepth: 10, dynamicRange: "hdr10", colorPrimaries: "bt2020", colorTransfer: "smpte2084", colorSpace: "bt2020nc" }),
    { kind: "convertible", label: "HDR10" },
  );
  assert.equal(classifyColorSource({ ...baseProbe, bitDepth: 10, dynamicRange: "hlg" }).kind, "unsupported");
  assert.equal(classifyColorSource({ ...baseProbe, bitDepth: 10, dynamicRange: "dolbyVision" }).kind, "unsupported");
  assert.equal(classifyColorSource({ ...baseProbe, bitDepth: 10, dynamicRange: "unknown" }).kind, "unsupported");
  assert.equal(
    classifyColorSource({ ...baseProbe, bitDepth: 8, dynamicRange: "unknown", colorTransfer: "smpte2084" }).kind,
    "unsupported",
  );
});

test("SAR helpers preserve display shape through source and manual rotation", () => {
  const probe = { ...baseProbe, sampleAspectRatio: { numerator: 16, denominator: 15 }, rotationDeg: 0 };
  assert.equal(hasNonSquarePixels(probe), true);
  assert.equal(hasNonSquarePixels({ ...baseProbe, sampleAspectRatio: { numerator: 1_000_001, denominator: 1_000_000 } }), true);
  assert.equal(effectiveSampleAspectRatio(probe), 16 / 15);
  assert.deepEqual(squarePixelDimensions({ probe, width: 720, height: 576 }), { width: 768, height: 576 });
  assert.equal(effectiveSampleAspectRatio(probe, 90), 15 / 16);
  assert.deepEqual(squarePixelDimensions({ probe, width: 576, height: 720, manualRotateDeg: 90 }), { width: 540, height: 720 });
  assert.deepEqual(
    squarePixelDimensions({
      probe: { ...baseProbe, sampleAspectRatio: { numerator: 4, denominator: 3 }, rotationDeg: 90 },
      width: 180,
      height: 320,
    }),
    { width: 134, height: 320 },
  );
  assert.deepEqual(fitMaxEdgeDimensions(853, 481, 720), { width: 720, height: 406 });
  assert.deepEqual(fitMaxEdgeDimensions(853, 481, 900), { width: 852, height: 480 });
  assert.deepEqual(
    fitMaxEdgeDisplayDimensions({
      probe: { ...baseProbe, sampleAspectRatio: { numerator: 32, denominator: 27 } },
      width: 720,
      height: 480,
      maxEdge: 720,
    }),
    { width: 720, height: 404 },
  );
});

test("geometry helpers disclose unsupported source rotation and extreme derived dimensions", () => {
  assert.equal(sourceRotationBlockingReason(baseProbe), null);
  assert.match(
    sourceRotationBlockingReason({ ...baseProbe, unsupportedRotationDeg: 45 }),
    /45\.000° display rotation/,
  );
  assert.equal(plannedDimensionBlockingReason(7680, 4320), null);
  assert.match(plannedDimensionBlockingReason(500_000, 576), /32768 px per-dimension/);
});

test("size-target audio retention mirrors codec-aware backend floors and FPS bounds", () => {
  assert.equal(
    minimumVideoBitrateKbps({ codec: "mpeg4", width: 1920, height: 1080, sourceFrameRate: 30 }),
    882,
  );
  assert.equal(
    minimumVideoBitrateKbps({ codec: "mpeg4", width: 1920, height: 1080, sourceFrameRate: 240 }),
    3_526,
  );
  assert.equal(
    minimumVideoBitrateKbps({
      codec: "mpeg4",
      width: 1920,
      height: 1080,
      sourceFrameRate: null,
      frameRateCapFps: 24,
    }),
    706,
  );
  assert.equal(
    sizeTargetRetainsAudio({
      audioRequested: true,
      hasAudio: true,
      sizeLimitEnabled: true,
      totalKbps: 900,
      codec: "mpeg4",
      width: 1920,
      height: 1080,
      sourceFrameRate: 30,
    }),
    false,
  );
  assert.equal(
    sizeTargetRetainsAudio({
      audioRequested: true,
      hasAudio: true,
      sizeLimitEnabled: true,
      totalKbps: 900,
      codec: "h264",
      width: 1920,
      height: 1080,
      sourceFrameRate: 30,
    }),
    true,
  );
});

test("Reverse and Loop estimates classify safe, warning, blocked, and unknown inputs", () => {
  const safe = estimateTransformMemory({ probe: baseProbe, reverse: true, loopVideo: false, width: 640, height: 360 });
  assert.equal(safe.severity, "ok");
  assert.ok(safe.bytes > 0 && safe.bytes < TRANSFORM_MEMORY_WARN_BYTES);

  const warning = estimateTransformMemory({ probe: baseProbe, reverse: true, loopVideo: false, width: 1920, height: 1080 });
  assert.equal(warning.severity, "warning");
  assert.ok(warning.bytes >= TRANSFORM_MEMORY_WARN_BYTES && warning.bytes < TRANSFORM_MEMORY_BLOCK_BYTES);

  const blocked = estimateTransformMemory({
    probe: { ...baseProbe, durationS: 30, bitDepth: 10, decodedVideoBytesPerPixel: 3 },
    reverse: true,
    loopVideo: true,
    width: 1920,
    height: 1080,
  });
  assert.equal(blocked.severity, "blocked");
  assert.ok(blocked.bytes >= TRANSFORM_MEMORY_BLOCK_BYTES);

  const unknown = estimateTransformMemory({
    probe: { ...baseProbe, frameRate: null },
    reverse: true,
    loopVideo: false,
    width: 640,
    height: 360,
  });
  assert.equal(unknown.severity, "blocked");
  assert.equal(unknown.bytes, null);

  const inactive = estimateTransformMemory({
    probe: { ...baseProbe, frameRate: null },
    reverse: false,
    loopVideo: false,
    width: 640,
    height: 360,
  });
  assert.deepEqual(inactive, { bytes: 0, severity: "ok", reason: null, videoBytes: 0, audioBytes: 0 });
});

test("Loop estimate applies a frame-rate cap after speed changes timestamps", () => {
  const uncapped = estimateTransformMemory({
    probe: { ...baseProbe, frameRate: 20 },
    reverse: false,
    loopVideo: true,
    speed: 2,
    width: 640,
    height: 360,
  });
  const capped = estimateTransformMemory({
    probe: { ...baseProbe, frameRate: 20 },
    reverse: false,
    loopVideo: true,
    speed: 2,
    frameRateCapFps: 30,
    width: 640,
    height: 360,
  });

  assert.ok(capped.videoBytes < uncapped.videoBytes);
  assert.equal(capped.videoBytes / uncapped.videoBytes, 0.75);
});

test("transform estimate can use the post-conversion decoded pixel layout", () => {
  const sourceLayout = estimateTransformMemory({
    probe: { ...baseProbe, decodedVideoBytesPerPixel: 3 },
    reverse: true,
    loopVideo: false,
    width: 1920,
    height: 1080,
  });
  const standardSdrLayout = estimateTransformMemory({
    probe: { ...baseProbe, decodedVideoBytesPerPixel: 3 },
    reverse: true,
    loopVideo: false,
    width: 1920,
    height: 1080,
    decodedVideoBytesPerPixel: 1.5,
  });

  const ratio = standardSdrLayout.videoBytes / sourceLayout.videoBytes;
  assert.ok(ratio > 0.5 && ratio < 0.51, `fixed frame overhead should keep the ratio just above 0.5, saw ${ratio}`);
  assert.equal(TRANSFORM_VIDEO_FRAME_OVERHEAD_BYTES, 8 * 1024);
});

test("tiny high-frame-count video includes fixed retained-frame overhead", () => {
  const estimate = estimateTransformMemory({
    probe: {
      ...baseProbe,
      durationS: 170,
      frameRate: 1_000,
      hasAudio: false,
      decodedVideoBytesPerPixel: 1,
    },
    reverse: true,
    loopVideo: false,
    width: 16,
    height: 16,
    audioEnabled: false,
  });
  assert.equal(estimate.severity, "blocked");
  assert.ok(estimate.bytes >= TRANSFORM_MEMORY_BLOCK_BYTES);
});

test("audio normalization estimates the 48 kHz buffer before Reverse and Loop", () => {
  const lowRateProbe = {
    ...baseProbe,
    durationS: 1_800,
    audioSampleRate: 8_000,
    audioChannels: 2,
    decodedAudioBytesPerSample: 8,
  };
  const sourceRate = estimateTransformMemory({
    probe: lowRateProbe,
    reverse: true,
    loopVideo: false,
    width: 1,
    height: 1,
    videoEnabled: false,
  });
  const normalized = estimateTransformMemory({
    probe: lowRateProbe,
    reverse: true,
    loopVideo: false,
    width: 1,
    height: 1,
    videoEnabled: false,
    normalizeAudio: true,
  });

  assert.equal(sourceRate.severity, "ok");
  assert.equal(normalized.severity, "blocked");
  assert.ok(Math.abs(normalized.audioBytes / sourceRate.audioBytes - 6) < 0.001);
});

test("retained audio facts fail closed instead of assuming an ordinary layout", () => {
  for (const missing of ["audioSampleRate", "audioChannels", "decodedAudioBytesPerSample"]) {
    const probe = { ...baseProbe, [missing]: null };
    const estimate = estimateTransformMemory({
      probe,
      reverse: true,
      loopVideo: false,
      width: 1,
      height: 1,
      videoEnabled: false,
    });
    assert.equal(estimate.severity, "blocked", missing);
    assert.equal(estimate.bytes, null, missing);
    assert.match(estimate.reason, /known retained-audio/);
  }

  const normalized = estimateTransformMemory({
    probe: { ...baseProbe, audioSampleRate: null, decodedAudioBytesPerSample: null },
    reverse: true,
    loopVideo: false,
    width: 1,
    height: 1,
    videoEnabled: false,
    normalizeAudio: true,
  });
  assert.equal(normalized.severity, "ok");
});
